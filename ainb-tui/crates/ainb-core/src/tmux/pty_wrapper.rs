// ABOUTME: PTY (Pseudo-Terminal) wrapper for tmux session interactions
//
// Owns the spawned child (a `tmux attach-session` client for the embed) and a
// cloned killer so the client is reliably terminated on:
//   - explicit `kill()` (focus release / session switch),
//   - `Drop` (normal teardown),
//   - process-global registry drain via `kill_all_embed_children()` (panic hook).
// Killing the ephemeral tmux *client* never kills the tmux session — tmux owns
// that — so session persistence across ainb restarts is preserved.

#![allow(dead_code)] // TODO(tmux-in-pane #P2/#P3): master()/resize()/is_running() wired by the embed render + focus phases.

use anyhow::Result;
use portable_pty::{Child, ChildKiller, CommandBuilder, ExitStatus, MasterPty, PtySize};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex, OnceLock};

/// Monotonic id so each wrapper can find + remove its own registry slot on Drop.
static NEXT_ID: AtomicU64 = AtomicU64::new(0);

/// Process-global registry of live embed-child killers. Keyed by wrapper id so a
/// normally-dropped wrapper removes exactly its own entry. Drained by the panic
/// hook (`kill_all_embed_children`) so a crash while an embed PTY is live cannot
/// leak the `tmux attach` client.
#[allow(clippy::type_complexity)]
static REGISTRY: OnceLock<StdMutex<Vec<(u64, Box<dyn ChildKiller + Send + Sync>)>>> =
    OnceLock::new();

fn registry() -> &'static StdMutex<Vec<(u64, Box<dyn ChildKiller + Send + Sync>)>> {
    REGISTRY.get_or_init(|| StdMutex::new(Vec::new()))
}

/// Kill every registered embed child. Called from the panic hook before the
/// terminal is restored, and safe to call at normal shutdown. Best-effort: a
/// kill error on an already-dead child is ignored.
pub fn kill_all_embed_children() {
    if let Ok(mut reg) = registry().lock() {
        for (_, killer) in reg.iter_mut() {
            let _ = killer.kill();
        }
        reg.clear();
    }
}

/// Number of currently-registered embed children (test/diagnostic aid).
pub fn registered_embed_child_count() -> usize {
    registry().lock().map(|r| r.len()).unwrap_or(0)
}

/// Wrapper around a PTY master + its child for tmux embed interactions.
pub struct PtyWrapper {
    master: Arc<StdMutex<Box<dyn MasterPty + Send>>>,
    child: Box<dyn Child + Send + Sync>,
    killer: Box<dyn ChildKiller + Send + Sync>,
    id: u64,
}

impl std::fmt::Debug for PtyWrapper {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PtyWrapper")
            .field("id", &self.id)
            .field("pid", &self.child.process_id())
            .finish()
    }
}

impl PtyWrapper {
    /// Start a new PTY running `cmd`, retaining the child so it can be killed.
    pub fn start(cmd: CommandBuilder) -> Result<Self> {
        let pty_system = portable_pty::native_pty_system();

        // Default size; the embed resizes to the pane rect once focused.
        let pair = pty_system.openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })?;

        let child = pair.slave.spawn_command(cmd)?;
        let killer = child.clone_killer();
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);

        // Register a killer for panic-hook cleanup. We register a *clone* so the
        // registry can kill the child even if this wrapper is leaked (forgotten
        // / unwound past) without running Drop.
        if let Ok(mut reg) = registry().lock() {
            reg.push((id, child.clone_killer()));
        }

        Ok(Self {
            master: Arc::new(StdMutex::new(pair.master)),
            child,
            killer,
            id,
        })
    }

    /// Kill the child (the ephemeral tmux client), NOT the tmux session.
    pub fn kill(&mut self) -> Result<()> {
        self.killer.kill()?;
        // Reap so we don't leak a zombie (SIGKILL'd children must be waited on).
        let _ = self.child.wait();
        self.deregister();
        Ok(())
    }

    /// True while the child is still running.
    pub fn is_running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// Reap the child if it has exited; returns its status when available.
    pub fn try_wait(&mut self) -> Result<Option<ExitStatus>> {
        Ok(self.child.try_wait()?)
    }

    /// OS process id of the child, if known.
    pub fn process_id(&self) -> Option<u32> {
        self.child.process_id()
    }

    /// Resize the PTY (cols × rows) — sends SIGWINCH to the child.
    pub fn resize(&self, cols: u16, rows: u16) -> Result<()> {
        let master = self.master.lock().map_err(|e| anyhow::anyhow!("Lock error: {}", e))?;
        master.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;
        Ok(())
    }

    /// Shared handle to the master PTY for read/write (embed reader + input).
    pub fn master(&self) -> Arc<StdMutex<Box<dyn MasterPty + Send>>> {
        Arc::clone(&self.master)
    }

    fn deregister(&self) {
        if let Ok(mut reg) = registry().lock() {
            reg.retain(|(id, _)| *id != self.id);
        }
    }
}

impl Drop for PtyWrapper {
    fn drop(&mut self) {
        // Best-effort: kill the client, reap it, and remove our registry slot.
        // Killing/reaping an already-dead child is a harmless no-op.
        let _ = self.killer.kill();
        let _ = self.child.wait();
        self.deregister();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use portable_pty::CommandBuilder;
    use std::time::Duration;

    /// The registry is process-global; cargo runs tests in this binary on
    /// multiple threads. Serialize the wrapper tests so one test's
    /// `kill_all_embed_children()` cannot kill another test's live child.
    static PTY_TEST_LOCK: StdMutex<()> = StdMutex::new(());

    /// Acquire the serialization lock, tolerating poisoning from a prior
    /// test's panic (we only guard ordering, not shared invariants).
    fn lock_serial() -> std::sync::MutexGuard<'static, ()> {
        PTY_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn sleeper() -> CommandBuilder {
        // portable-pty's CommandBuilder starts with an EMPTY environment, so a
        // bare program name won't resolve and the child exec-fails into a
        // zombie. Pass PATH through so `sleep` actually runs. (The real embed
        // spawns `tmux attach` — it must set PATH the same way.)
        let mut cmd = CommandBuilder::new("sleep");
        cmd.arg("60");
        if let Ok(path) = std::env::var("PATH") {
            cmd.env("PATH", path);
        }
        cmd
    }

    /// True if the process is terminated for our purposes: it no longer exists,
    /// OR it's a zombie (killed but not yet reaped — which happens in the
    /// `kill_all` path where the owning wrapper was forgotten so nobody waits
    /// on it; in production the panicking process exits and init reaps it).
    /// `kill -0` alone can't tell a zombie from a live process, so probe `ps`.
    fn terminated(pid: u32) -> bool {
        let out = std::process::Command::new("ps")
            .args(["-o", "stat=", "-p", &pid.to_string()])
            .output();
        match out {
            Ok(o) => {
                let st = String::from_utf8_lossy(&o.stdout);
                let st = st.trim();
                st.is_empty() || st.starts_with('Z')
            }
            Err(_) => true,
        }
    }

    fn wait_terminated(pid: u32) -> bool {
        for _ in 0..40 {
            if terminated(pid) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
    }

    #[test]
    fn start_runs_child_and_kill_terminates_it() {
        let _g = lock_serial();
        let mut pty = PtyWrapper::start(sleeper()).unwrap();
        let pid = pty.process_id().expect("child pid");
        assert!(pty.is_running(), "child should be running after start");
        assert!(!terminated(pid));
        pty.kill().unwrap();
        assert!(wait_terminated(pid), "child should be dead after kill()");
    }

    #[test]
    fn drop_terminates_child() {
        let _g = lock_serial();
        let pty = PtyWrapper::start(sleeper()).unwrap();
        let pid = pty.process_id().expect("child pid");
        assert!(!terminated(pid));
        drop(pty);
        assert!(wait_terminated(pid), "child should be dead after Drop");
    }

    #[test]
    fn kill_all_embed_children_drains_leaked_wrappers() {
        let _g = lock_serial();
        // Simulate the panic path: wrappers leaked (Drop never runs) but their
        // killers remain in the registry.
        let p1 = PtyWrapper::start(sleeper()).unwrap();
        let p2 = PtyWrapper::start(sleeper()).unwrap();
        let pid1 = p1.process_id().unwrap();
        let pid2 = p2.process_id().unwrap();
        std::mem::forget(p1);
        std::mem::forget(p2);
        assert!(!terminated(pid1) && !terminated(pid2));

        kill_all_embed_children();

        assert!(wait_terminated(pid1), "registry drain should kill leaked child 1");
        assert!(wait_terminated(pid2), "registry drain should kill leaked child 2");
        assert_eq!(registered_embed_child_count(), 0, "registry cleared after drain");
    }

    #[test]
    fn drop_removes_own_registry_slot() {
        let _g = lock_serial();
        kill_all_embed_children(); // clean slate
        let pty = PtyWrapper::start(sleeper()).unwrap();
        assert_eq!(registered_embed_child_count(), 1);
        drop(pty);
        assert_eq!(registered_embed_child_count(), 0, "Drop removes the registry slot");
    }

    #[test]
    fn resize_succeeds() {
        let _g = lock_serial();
        let pty = PtyWrapper::start(sleeper()).unwrap();
        assert!(pty.resize(100, 50).is_ok());
    }
}
