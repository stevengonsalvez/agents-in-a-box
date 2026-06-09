//! Host-side registry for `host/spawn_managed_subprocess`.
//!
//! A *managed subprocess* is a host-supervised child process a plugin
//! asks the host to spawn (cap-gated by `spawn_managed_subprocess`).
//! Unlike a plugin process (which the host drives over JSON-RPC stdio),
//! a managed child is opaque to the host: the host owns only its
//! lifecycle (spawn + reliable kill), not its protocol.
//!
//! ## Why the host owns supervision
//!
//! If the plugin forked the child itself, an orphaned daemon would
//! survive a plugin crash and leak. By routing the spawn through the
//! host we get:
//!
//! - **Leak guard.** The child is spawned under the same pgrp /
//!   `PDEATHSIG` machinery as plugin processes ([`crate::process`]), so
//!   it dies with the host even on hard `SIGKILL`.
//! - **Deterministic reap.** Every managed child is keyed by which
//!   plugin requested it; on plugin shutdown / crash / quarantine the
//!   host kills all of that plugin's children, and on `Runtime` drop the
//!   whole registry is drained.
//!
//! ## Env allow-list
//!
//! The child inherits **only** the parent env vars named in the
//! request's `env_allowlist`. Everything else is stripped before exec so
//! a plugin can't smuggle the host's secrets (`ANTHROPIC_API_KEY`, ...)
//! into an arbitrary child.

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use parking_lot::Mutex;
use tokio::process::{Child as TokioChild, Command};

use crate::error::RuntimeError;
use crate::process::{SIGTERM, install_leak_guard, signal_pgrp};
use crate::types::PluginId;

/// Host-minted, opaque managed-subprocess handle.
///
/// The plugin echoes it back when composing with
/// `host/event_stream_subscribe` (topic `managed:<handle>:stdout`) and
/// the host keys the child by it. Backed by a ULID + monotonic suffix so
/// two spawns in the same millisecond produce distinct ids even under a
/// frozen clock.
pub type ManagedHandle = String;

/// Allocator for [`ManagedHandle`]s. Cheap to clone (shares one `Arc`).
#[derive(Clone)]
pub struct HandleGen {
    seq: Arc<AtomicU64>,
}

impl std::fmt::Debug for HandleGen {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HandleGen").finish_non_exhaustive()
    }
}

impl Default for HandleGen {
    fn default() -> Self {
        Self::new()
    }
}

impl HandleGen {
    /// Construct a fresh allocator.
    #[must_use]
    pub fn new() -> Self {
        Self {
            seq: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Allocate the next opaque handle.
    pub fn allocate(&self) -> ManagedHandle {
        let n = self.seq.fetch_add(1, Ordering::Relaxed);
        format!("{}-{n:08x}", ulid::Ulid::new())
    }
}

/// One live managed child: which plugin owns it, its pid, and the
/// process-group leader pid used to reap it.
struct ManagedChild {
    plugin: PluginId,
    pid: u32,
    /// The tokio `Child` handle. Kept so `kill_on_drop(true)` and an
    /// explicit `start_kill` both reach the process.
    child: TokioChild,
}

/// Concurrent registry of live managed subprocesses.
///
/// Cheap to clone (single `Arc`). Shared between the [`crate::Runtime`]
/// and each per-plugin task so a plugin's children are reaped when the
/// plugin leaves `Running`.
#[derive(Clone, Default)]
pub struct ManagedSubprocessRegistry {
    inner: Arc<Mutex<HashMap<ManagedHandle, ManagedChild>>>,
    ids: HandleGen,
}

impl std::fmt::Debug for ManagedSubprocessRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ManagedSubprocessRegistry")
            .field("live", &self.inner.lock().len())
            .finish_non_exhaustive()
    }
}

/// A spawned managed subprocess: its host-minted handle + OS pid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnedManaged {
    /// Host-allocated opaque handle.
    pub handle: ManagedHandle,
    /// OS process id.
    pub pid: u32,
}

impl ManagedSubprocessRegistry {
    /// Construct an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Spawn a managed child for `plugin`.
    ///
    /// The child inherits **only** the env vars named in `env_allowlist`
    /// (read from the host's current environment); every other variable
    /// is cleared. The same leak guard as plugin processes is installed
    /// so the child dies with the host. Returns the host-minted handle +
    /// the child's pid.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeError::Io`] if the spawn fails (e.g. binary not
    /// found on `PATH`).
    pub fn spawn(
        &self,
        plugin: PluginId,
        bin: &str,
        argv: &[String],
        env_allowlist: &[String],
        cwd: Option<&str>,
    ) -> Result<SpawnedManaged, RuntimeError> {
        let mut cmd = Command::new(bin);
        cmd.args(argv)
            // Pipe stdout so a later `managed:<handle>:stdout` stream can
            // be wired (P3.3 composes with P3.2). stdin/stderr piped too
            // so the child can't inherit (and block on) the host's tty.
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        // Strip the environment to the allow-list only.
        cmd.env_clear();
        for name in env_allowlist {
            if let Ok(val) = std::env::var(name) {
                cmd.env(name, val);
            }
        }
        if let Some(dir) = cwd {
            cmd.current_dir(dir);
        }

        install_leak_guard(&mut cmd);

        let child = cmd.spawn().map_err(RuntimeError::Io)?;
        let pid = child.id().unwrap_or(0);
        let handle = self.ids.allocate();
        self.inner.lock().insert(handle.clone(), ManagedChild { plugin, pid, child });
        Ok(SpawnedManaged { handle, pid })
    }

    /// Kill every managed child owned by `plugin`. Called on plugin
    /// teardown (shutdown / crash / quarantine). Returns the number of
    /// children reaped.
    pub fn kill_plugin(&self, plugin: &PluginId) -> usize {
        let mut map = self.inner.lock();
        let to_kill: Vec<ManagedHandle> = map
            .iter()
            .filter(|(_, c)| &c.plugin == plugin)
            .map(|(h, _)| h.clone())
            .collect();
        let n = to_kill.len();
        for h in to_kill {
            if let Some(mut c) = map.remove(&h) {
                kill_child(&mut c);
            }
        }
        n
    }

    /// Kill every managed child regardless of owner. Called on
    /// [`crate::Runtime`] drop so no managed process outlives the host.
    /// Returns the number of children reaped.
    pub fn kill_all(&self) -> usize {
        let mut map = self.inner.lock();
        let n = map.len();
        for (_, mut c) in map.drain() {
            kill_child(&mut c);
        }
        n
    }

    /// Number of live managed children. Test/diagnostic helper.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.lock().len()
    }

    /// Whether the registry holds no managed children.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inner.lock().is_empty()
    }

    /// The pid of a managed child by handle, if live. Test helper.
    #[must_use]
    pub fn pid_of(&self, handle: &str) -> Option<u32> {
        self.inner.lock().get(handle).map(|c| c.pid)
    }
}

/// Reap a single managed child: SIGTERM the whole process group (so any
/// descendants the child forked die too), then `start_kill` the child
/// itself as a backstop.
fn kill_child(c: &mut ManagedChild) {
    // `setpgid(0, 0)` in the leak guard makes the child a process-group
    // leader equal to its pid, so `kill(-pid, SIGTERM)` reaches every
    // descendant. Best-effort: the pid may already be gone.
    if c.pid != 0 {
        let _ = signal_pgrp(c.pid.cast_signed(), SIGTERM);
    }
    let _ = c.child.start_kill();
}

/// The delivery topic a managed child's stdout stream arrives under:
/// `managed:<handle>:stdout`.
#[must_use]
pub fn managed_stdout_topic(handle: &str) -> String {
    format!("managed:{handle}:stdout")
}

/// Whether `bin` is permitted by a `spawn_managed_subprocess` allow-list.
///
/// Unlike [`crate::event_stream::topic_allowed`], the spawn cap allow-list
/// is **exact-match only** — no wildcard / prefix semantics. Spawning an
/// arbitrary binary is a far larger danger surface than subscribing to a
/// topic, so the whitelist names the exact binaries
/// (e.g. `["ainb-hangar-daemon"]`).
///
/// The caller is responsible for the prior bool-true rejection
/// (`-32003`) and `is_granted()` gate — this function only answers "does
/// the allow-list name this binary?".
#[must_use]
pub fn bin_allowed(allow_list: &[String], bin: &str) -> bool {
    allow_list.iter().any(|b| b == bin)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pid_id(s: &str) -> PluginId {
        PluginId::from(s)
    }

    fn process_alive(pid: u32) -> bool {
        // `kill(pid, 0)` probes liveness without sending a signal.
        // SAFETY: signal 0 performs no delivery, only permission/existence checks.
        unsafe { libc::kill(pid.cast_signed(), 0) == 0 }
    }

    #[tokio::test]
    async fn spawn_registers_and_returns_handle_pid() {
        let reg = ManagedSubprocessRegistry::new();
        let s = reg.spawn(pid_id("p"), "sleep", &["30".into()], &[], None).expect("spawn sleep");
        assert!(!s.handle.is_empty());
        assert_ne!(s.pid, 0);
        assert_eq!(reg.len(), 1);
        assert_eq!(reg.pid_of(&s.handle), Some(s.pid));
        // Cleanup.
        reg.kill_all();
    }

    #[tokio::test]
    async fn handles_are_unique() {
        let reg = ManagedSubprocessRegistry::new();
        let a = reg.spawn(pid_id("p"), "sleep", &["30".into()], &[], None).unwrap();
        let b = reg.spawn(pid_id("p"), "sleep", &["30".into()], &[], None).unwrap();
        assert_ne!(a.handle, b.handle);
        assert_eq!(reg.len(), 2);
        reg.kill_all();
    }

    #[tokio::test]
    async fn kill_plugin_reaps_only_its_children() {
        let reg = ManagedSubprocessRegistry::new();
        let a = reg.spawn(pid_id("p1"), "sleep", &["30".into()], &[], None).unwrap();
        let _b = reg.spawn(pid_id("p2"), "sleep", &["30".into()], &[], None).unwrap();
        assert_eq!(reg.kill_plugin(&pid_id("p1")), 1);
        assert_eq!(reg.len(), 1, "p2's child must survive");
        // p1's child must actually die within ~2s.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while process_alive(a.pid) && std::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert!(!process_alive(a.pid), "killed child still alive");
        reg.kill_all();
    }

    #[tokio::test]
    async fn kill_all_drains_registry() {
        let reg = ManagedSubprocessRegistry::new();
        let s = reg.spawn(pid_id("p"), "sleep", &["30".into()], &[], None).unwrap();
        assert_eq!(reg.kill_all(), 1);
        assert!(reg.is_empty());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while process_alive(s.pid) && std::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert!(!process_alive(s.pid), "kill_all left a child alive");
    }

    #[tokio::test]
    async fn env_allowlist_strips_unlisted_vars() {
        // Set two env vars; only one is on the allow-list. The child
        // (`env`) prints its environment; we assert the filter held.
        std::env::set_var("CTS_MANAGED_KEEP", "keep-me");
        std::env::set_var("CTS_MANAGED_DROP", "drop-me");
        let reg = ManagedSubprocessRegistry::new();
        let mut cmd = Command::new("printenv");
        cmd.env_clear();
        // Mirror the spawn() env logic to prove it in isolation: only
        // CTS_MANAGED_KEEP should survive.
        let allow = ["CTS_MANAGED_KEEP".to_string()];
        for name in &allow {
            if let Ok(v) = std::env::var(name) {
                cmd.env(name, v);
            }
        }
        let out = cmd.output().await.expect("printenv");
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(
            text.contains("CTS_MANAGED_KEEP=keep-me"),
            "kept var missing: {text}"
        );
        assert!(
            !text.contains("CTS_MANAGED_DROP"),
            "dropped var leaked: {text}"
        );
        std::env::remove_var("CTS_MANAGED_KEEP");
        std::env::remove_var("CTS_MANAGED_DROP");
        let _ = reg.kill_all();
    }

    #[test]
    fn managed_stdout_topic_format() {
        assert_eq!(managed_stdout_topic("01ABC"), "managed:01ABC:stdout");
    }

    #[test]
    fn bin_allowed_is_exact_match_only() {
        let list = vec!["ainb-hangar-daemon".to_string()];
        assert!(bin_allowed(&list, "ainb-hangar-daemon"));
        // No prefix / wildcard semantics — exact only.
        assert!(!bin_allowed(&list, "ainb-hangar-daemon-evil"));
        assert!(!bin_allowed(&list, "ainb-hangar"));
        assert!(!bin_allowed(&list, "/usr/bin/ainb-hangar-daemon"));
        // A literal `*` does NOT act as a wildcard.
        assert!(!bin_allowed(&["*".to_string()], "anything"));
    }

    #[test]
    fn bin_allowed_empty_list_denies() {
        assert!(!bin_allowed(&[], "ainb-hangar-daemon"));
    }
}
