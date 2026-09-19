// ABOUTME: Shared CLI utilities for session lookup and common operations
//
// Provides consistent session finding logic across all CLI commands.
// Uses prefix matching for both UUID and workspace name for user convenience.

use anyhow::{Result, anyhow};
use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;
use uuid::Uuid;

use crate::interactive::session_manager::{
    HeldLockMark, ModelSource, SessionMetadata, SessionStore, SessionStoreGuard,
};
use crate::models::SessionAgentType;
use ainb_hangar_client::DaemonClient;
use ainb_hangar_proto::protocol::CAP_WORKSPACE_SESSIONS;
use ainb_hangar_proto::sessions::{
    WorkspaceSessionDeleteParams, WorkspaceSessionEntry, WorkspaceSessionListParams,
    WorkspaceSessionUpsertParams,
};

/// Convert a proto [`WorkspaceSessionEntry`] into local [`SessionMetadata`].
///
/// # Errors
///
/// Returns why the entry cannot be used when its `session_id` is not a UUID.
/// The caller skips such a row; it never invents an id for it, because an
/// id minted on read would differ on every read and match no worktree.
pub fn entry_to_metadata(entry: &WorkspaceSessionEntry) -> Result<SessionMetadata, String> {
    let session_id = Uuid::parse_str(&entry.session_id).map_err(|e| {
        format!(
            "session {:?} has a non-UUID id: {e}",
            entry.tmux_session_name
        )
    })?;
    let created_at = DateTime::from_timestamp_millis(entry.created_at).unwrap_or_else(Utc::now);
    let agent_type = serde_json::from_value(serde_json::Value::String(entry.agent_type.clone()))
        .unwrap_or(SessionAgentType::Claude);
    let model_source =
        serde_json::from_value(serde_json::Value::String(entry.model_source.clone()))
            .unwrap_or(ModelSource::LegacyTyped);
    let codex_model = entry
        .codex_model
        .as_ref()
        .and_then(|cm| serde_json::from_value(serde_json::Value::String(cm.clone())).ok());

    Ok(SessionMetadata {
        session_id,
        tmux_session_name: entry.tmux_session_name.clone(),
        worktree_path: PathBuf::from(&entry.worktree_path),
        workspace_name: entry.workspace_name.clone(),
        created_at,
        agent_type,
        headroom_enabled: entry.headroom_enabled,
        rtk_enabled: entry.rtk_enabled,
        skip_permissions: entry.skip_permissions,
        model: entry.model.clone(),
        model_source,
        codex_model,
        codex_thread_id: entry.codex_thread_id.clone(),
    })
}

/// Convert local [`SessionMetadata`] into proto [`WorkspaceSessionEntry`].
#[must_use]
pub fn metadata_to_entry(meta: &SessionMetadata) -> WorkspaceSessionEntry {
    // These are fieldless enums with no serde renames, so the Debug name is
    // the persisted name `entry_to_metadata` parses back (pinned by
    // `enum_names_round_trip`). Formatting keeps this module off the
    // serialisation call-site list `tests/serialize_guard.rs` fences.
    let agent_type = format!("{:?}", meta.agent_type);
    let model_source = format!("{:?}", meta.model_source);
    let codex_model = meta.codex_model.map(|cm| format!("{cm:?}"));

    WorkspaceSessionEntry {
        session_id: meta.session_id.to_string(),
        tmux_session_name: meta.tmux_session_name.clone(),
        worktree_path: meta.worktree_path.to_string_lossy().to_string(),
        workspace_name: meta.workspace_name.clone(),
        created_at: meta.created_at.timestamp_millis(),
        agent_type,
        headroom_enabled: meta.headroom_enabled,
        rtk_enabled: meta.rtk_enabled,
        skip_permissions: meta.skip_permissions,
        model: meta.model.clone(),
        model_source,
        codex_model,
        codex_thread_id: meta.codex_thread_id.clone(),
    }
}

/// Where this process reads and writes sessions.
///
/// ```text
/// resolve ──▶ AINB_SESSION_SOURCE=file? ──yes──────────────────────▶ File
///               │ no
///               ▼
///             this build advertises hangar.workspace.sessions? ─no─▶ File
///               │ yes
///               ▼
///             dial + hello within SESSION_RPC_DEADLINE ──fails────▶ Degraded
///               │
///               ▼
///             the daemon advertises the capability? ──no─────────▶ File
///               │ yes
///               ▼
///             session_list says import_complete? ──no / fails────▶ Degraded
///               │ yes
///               ▼
///             Daemon
/// ```
///
/// Decided once per process by [`session_source`], so one command cannot
/// read from the table and write to the file. The one transition is
/// [`Degraded`](Self::Degraded) to [`Daemon`](Self::Daemon), made by
/// [`leave_degraded`] once a daemon is up and has reconciled; there is no way
/// back, and a daemon that fails after the switch is an error, not a
/// fallback.
///
/// On [`Daemon`](Self::Daemon) the table is authoritative for reads, and
/// every write changes the `sessions.json` row first and then the table,
/// under the file's lock, so a previous release reading the file still sees
/// current sessions. On [`File`](Self::File) and
/// [`Degraded`](Self::Degraded) the flocked `SessionStore` path is used, the
/// lock wait bounded by [`SESSION_RPC_DEADLINE`].
#[derive(Debug, Clone)]
pub enum SessionSource {
    /// The daemon's `sessions` table, behind RPC.
    Daemon(DaemonClient),
    /// `~/.agents-in-a-box/sessions.json`.
    File,
    /// This build speaks the sessions capability but no ready daemon was
    /// reachable: sessions are on the file, as on [`File`](Self::File), and
    /// a long-lived process keeps trying to reach the daemon
    /// ([`reresolve_while_degraded`]). Carries the client it will retry with,
    /// or `None` to read one from the environment when it retries.
    Degraded(Option<DaemonClient>),
}

/// The bound on every session RPC [`SessionSource`] makes.
///
/// It also bounds the wait for the `sessions.json` lock. On expiry the call
/// returns an error the caller surfaces; it never hangs a reducer or a CLI
/// command.
///
/// 3 s, not the 2 s the goal recommends: a daemon that has just restarted
/// holds a session read for up to its `FIRST_PASS_WAIT` (2 s) before it
/// answers not-ready, and that answer must arrive inside this deadline, or
/// the client reads a timeout where the daemon meant "retry". A test in
/// `ainb-core` pins this one above the daemon's.
pub const SESSION_RPC_DEADLINE: Duration = Duration::from_secs(3);

/// The bound on the reconcile a surface asks for when it leaves
/// [`SessionSource::Degraded`]: the daemon's pass may wait for the lock and
/// then write, each bounded on the daemon's side.
const LEAVE_DEGRADED_DEADLINE: Duration = Duration::from_secs(10);

/// The process's kill switch: `AINB_SESSION_SOURCE=file` makes
/// [`SessionSource::resolve`] answer [`File`](SessionSource::File) before
/// it dials anything. Any other value is ignored with one warning.
pub const SESSION_SOURCE_ENV: &str = "AINB_SESSION_SOURCE";

/// How many times [`SessionSource::mutate`] reads a not-ready daemon before
/// it gives up. Each wait is the daemon's own bounded first-pass wait.
const NOT_READY_ATTEMPTS: u32 = 3;

/// The pause between two of those attempts, so a daemon that answers
/// not-ready at once cannot turn them into a tight loop.
const NOT_READY_PAUSE: Duration = Duration::from_millis(100);

fn daemon_io_error(what: &str, error: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::other(format!("daemon session {what} failed: {error}"))
}

/// Run a session RPC under [`SESSION_RPC_DEADLINE`].
async fn within_deadline<T, E: std::fmt::Display>(
    what: &str,
    call: impl std::future::Future<Output = Result<T, E>>,
) -> std::io::Result<T> {
    tokio::time::timeout(SESSION_RPC_DEADLINE, call).await.map_or_else(
        |_| {
            Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!(
                    "daemon session {what} did not answer within {} ms",
                    SESSION_RPC_DEADLINE.as_millis()
                ),
            ))
        },
        |result| result.map_err(|e| daemon_io_error(what, e)),
    )
}

/// How long any write through the resolver waits for the `sessions.json`
/// lock, on every source.
///
/// Longer than the RPC deadline on purpose: the daemon's reconcile pass holds
/// this lock on every boot, whatever the capability says, for up to its flock
/// wait (`SESSIONS_FLOCK_BOUND`, 2 s) plus its store write
/// (`RECONCILE_STORE_BOUND`, 5 s). A writer that gave up sooner would fail,
/// and `ainb run` roll back, a live session only because a daemon happened to
/// be reconciling. A test in `ainb-core` pins this above the daemon's sum.
pub const SESSIONS_LOCK_WAIT: Duration = Duration::from_secs(10);

/// The bound on all of one `mutate`'s table writes together.
///
/// On [`SessionSource::Daemon`] each RPC is also under
/// [`SESSION_RPC_DEADLINE`]; this caps a run of slow ones, so a write of many
/// rows cannot hold the lock for many deadlines.
pub const MUTATE_WRITES_DEADLINE: Duration = SESSION_RPC_DEADLINE;

/// Take the `sessions.json` lock, waiting at most [`SESSIONS_LOCK_WAIT`].
async fn lock_within_deadline() -> std::io::Result<SessionStoreGuard> {
    let deadline = tokio::time::Instant::now() + SESSIONS_LOCK_WAIT;
    loop {
        if let Some(guard) = SessionStore::try_lock()? {
            return Ok(guard);
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!(
                    "sessions.json lock still held after {} ms",
                    SESSIONS_LOCK_WAIT.as_millis()
                ),
            ));
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Whether [`SESSION_SOURCE_ENV`] forces the file. Read before any dial.
fn kill_switch_says_file() -> bool {
    static IGNORED: std::sync::Once = std::sync::Once::new();
    match std::env::var(SESSION_SOURCE_ENV) {
        Ok(value) if value == "file" => true,
        Ok(value) if !value.is_empty() => {
            IGNORED.call_once(|| {
                say(&format!(
                    "Warning: ignoring {SESSION_SOURCE_ENV}={value:?}; only \"file\" is read."
                ));
            });
            false
        }
        _ => false,
    }
}

/// Whether this build speaks the sessions capability: its own catalogue, or,
/// in a `test-support` build, the harness switch
/// ([`advertise_workspace_sessions_for_tests`], or
/// `AINB_TEST_WORKSPACE_SESSIONS=1` for a real `ainb` binary).
fn build_advertises() -> bool {
    if ainb_hangar_proto::protocol::advertises(CAP_WORKSPACE_SESSIONS) {
        return true;
    }
    #[cfg(any(test, feature = "test-support"))]
    if TEST_ADVERTISES.load(std::sync::atomic::Ordering::SeqCst)
        || std::env::var("AINB_TEST_WORKSPACE_SESSIONS").is_ok_and(|v| v == "1")
    {
        return true;
    }
    false
}

#[cfg(any(test, feature = "test-support"))]
static TEST_ADVERTISES: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Make [`SessionSource::resolve`] treat the sessions capability as built in.
///
/// For tests before the flip does it for real; test builds only. The daemon
/// side is `advertise_workspace_sessions_for_tests`.
#[cfg(any(test, feature = "test-support"))]
pub fn advertise_workspace_sessions_for_tests(on: bool) {
    TEST_ADVERTISES.store(on, std::sync::atomic::Ordering::SeqCst);
}

/// Set once a long-lived surface (the TUI, the desktop host) owns the
/// terminal or has no terminal at all: from then on the resolver's notices go
/// to the log, never to raw stderr, which would draw over the TUI's
/// alternate screen. A CLI command never sets it and keeps its stderr lines.
static LONG_LIVED_SURFACE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Mark this process as a long-lived surface (see [`LONG_LIVED_SURFACE`]).
/// The TUI and desktop host call it before their first session read.
pub fn mark_long_lived_surface() {
    LONG_LIVED_SURFACE.store(true, std::sync::atomic::Ordering::SeqCst);
}

/// Whether the resolver's notices go to the log rather than stderr.
#[must_use]
pub fn notices_go_to_the_log() -> bool {
    LONG_LIVED_SURFACE.load(std::sync::atomic::Ordering::SeqCst)
}

/// One notice from the resolver: a stderr line for a CLI command, a log line
/// for a long-lived surface.
fn say(line: &str) {
    if notices_go_to_the_log() {
        tracing::warn!("{line}");
    } else {
        eprintln!("{line}");
    }
}

/// Say once per process that sessions are on the file for now.
fn degraded_notice() {
    static SAID: std::sync::Once = std::sync::Once::new();
    SAID.call_once(|| {
        say("Notice: sessions are on the local sessions.json until the hangar daemon is up.");
    });
}

impl SessionSource {
    /// Decide against the daemon named by the environment. See the type's
    /// diagram.
    pub async fn resolve() -> Self {
        if kill_switch_says_file() || !build_advertises() {
            return Self::File;
        }
        if let Ok(client) = DaemonClient::from_env() {
            Self::resolve_with(client).await
        } else {
            degraded_notice();
            Self::Degraded(None)
        }
    }

    /// Decide against the daemon at `socket` with `token` (the test seam).
    ///
    /// Unlike [`resolve`](Self::resolve) this asks the daemon even when the
    /// build does not advertise the capability, so tests can drive the
    /// daemon path. The kill switch still wins, before any dial.
    pub async fn resolve_at(socket: PathBuf, token: String) -> Self {
        if kill_switch_says_file() {
            return Self::File;
        }
        Self::resolve_with(DaemonClient::with_parts(socket, token)).await
    }

    async fn resolve_with(client: DaemonClient) -> Self {
        let Ok(hello) = within_deadline("hello", client.hello()).await else {
            degraded_notice();
            return Self::Degraded(Some(client));
        };
        if !hello.advertises(CAP_WORKSPACE_SESSIONS) {
            // A daemon from before the sessions table: it will never serve
            // them, so this is the file for good, not a degraded wait.
            return Self::File;
        }
        let probe = WorkspaceSessionListParams {
            workspace_name: None,
            limit: Some(1),
        };
        match within_deadline("list", client.workspace_session_list(probe)).await {
            Ok(res) if res.import_complete => Self::Daemon(client),
            _ => {
                degraded_notice();
                Self::Degraded(Some(client))
            }
        }
    }

    /// Whether sessions are on the file only because the daemon is not up:
    /// the state a surface shows a notice for.
    #[must_use]
    pub const fn is_degraded(&self) -> bool {
        matches!(self, Self::Degraded(_))
    }

    /// Read the whole store from this source.
    ///
    /// # Errors
    ///
    /// When the current thread holds the `sessions.json` lock (nesting would
    /// hang). On [`Daemon`](Self::Daemon), when the RPC fails or passes
    /// [`SESSION_RPC_DEADLINE`], or when the daemon answers not-ready
    /// (`import_complete: false`). The file is not read instead: the process
    /// already decided where its sessions live.
    ///
    /// Not-ready is not monotonic: a daemon that restarts answers it until
    /// its first reconcile pass commits, with no rows. Read as a store, that
    /// is zero sessions, and `mutate` would diff against an empty view.
    pub async fn load(&self) -> std::io::Result<SessionStore> {
        SessionStore::ensure_lock_not_held()?;
        let client = match self {
            Self::File | Self::Degraded(_) => return Ok(SessionStore::load()),
            Self::Daemon(client) => client,
        };
        Self::load_daemon(client).await?.ok_or_else(|| {
            daemon_io_error(
                "list",
                "the daemon's sessions table is not ready (its reconcile pass has not committed)",
            )
        })
    }

    /// Read the whole store from the daemon. `None` means the daemon answered
    /// not-ready: its first reconcile pass since boot has not committed.
    async fn load_daemon(client: &DaemonClient) -> std::io::Result<Option<SessionStore>> {
        let res = within_deadline(
            "list",
            client.workspace_session_list(WorkspaceSessionListParams::default()),
        )
        .await?;
        if !res.import_complete {
            return Ok(None);
        }
        if res.truncated {
            say(&format!(
                "Warning: the daemon returned only the newest {} sessions.",
                res.sessions.len()
            ));
        }
        let mut store = SessionStore::default();
        for entry in &res.sessions {
            match entry_to_metadata(entry) {
                Ok(meta) => {
                    store.sessions.insert(meta.tmux_session_name.clone(), meta);
                }
                Err(why) => say(&format!("Warning: skipping {why}")),
            }
        }
        Ok(Some(store))
    }

    /// Apply `f` to the store and persist the difference.
    ///
    /// On [`File`](Self::File) and [`Degraded`](Self::Degraded) the store is
    /// loaded under the lock, `f` runs, and the file is saved only if
    /// something changed.
    ///
    /// On [`Daemon`](Self::Daemon), under the `sessions.json` lock: the table
    /// is read, `f` runs, and what changed is written row by row, the file
    /// first (upsert or remove by tmux key, never a whole-file replace from
    /// the table) and then the table (one delete per session id that
    /// disappeared, one upsert per session that is new or different). If a
    /// table write fails, the file is put back as it was before the lock is
    /// released and the error is returned. So after every successful write
    /// the file and the table agree on the sessions it touched, and a
    /// previous release reading the file sees them.
    ///
    /// A daemon that is not ready (it restarted, and its first reconcile pass
    /// has not committed) needs that same lock for the pass. So on a not-ready
    /// read the lock is released, the daemon is asked again without it (the
    /// daemon holds that request until its pass commits, bounded), and the
    /// lock is retaken for a fresh read, at most [`NOT_READY_ATTEMPTS`] times.
    /// No write is ever computed from a not-ready read.
    ///
    /// # Errors
    ///
    /// When the current thread already holds the lock; when the lock is not
    /// free within [`SESSION_RPC_DEADLINE`]; the first failed or timed-out
    /// RPC, so a caller such as `ainb run` can roll back; or a
    /// still-reconciling error once the attempts are spent.
    pub async fn mutate<F>(&self, f: F) -> std::io::Result<()>
    where
        F: FnOnce(&mut SessionStore),
    {
        SessionStore::ensure_lock_not_held()?;
        let client = match self {
            Self::File | Self::Degraded(_) => {
                let _guard = lock_within_deadline().await?;
                let (mut store, _) = load_file_for_write()?;
                let before = snapshot(&store);
                {
                    let _mark = HeldLockMark::enter();
                    f(&mut store);
                }
                // Save only on change, as v2's orphan cleanup did: a no-op
                // never rewrites the file another writer may be reading.
                return if snapshot(&store) == before {
                    Ok(())
                } else {
                    store.save()
                };
            }
            Self::Daemon(client) => client,
        };

        let mut attempts = 0;
        let (_guard, mut store) = loop {
            let guard = lock_within_deadline().await?;
            if let Some(store) = Self::load_daemon(client).await? {
                break (guard, store);
            }
            drop(guard);
            attempts += 1;
            if attempts == 1 {
                say("Notice: the hangar daemon is reconciling sessions; waiting.");
            }
            if attempts >= NOT_READY_ATTEMPTS {
                return Err(daemon_io_error(
                    "list",
                    "the daemon is still reconciling sessions.json; nothing was written, try again",
                ));
            }
            tokio::time::sleep(NOT_READY_PAUSE).await;
            // Asked without the lock, so the daemon's pass can take it.
            let _ = Self::load_daemon(client).await?;
        };
        let before = entries_by_id(&store);
        {
            let _mark = HeldLockMark::enter();
            f(&mut store);
        }
        let after = entries_by_id(&store);
        let removed: Vec<Uuid> =
            before.keys().filter(|id| !after.contains_key(*id)).copied().collect();
        // Sorted by tmux name, so a multi-row write happens in the same
        // order every time, whatever the map's order.
        let mut written: Vec<&WorkspaceSessionEntry> = after
            .iter()
            .filter(|(id, entry)| before.get(*id) != Some(*entry))
            .map(|(_, entry)| entry)
            .collect();
        written.sort_by(|a, b| a.tmux_session_name.cmp(&b.tmux_session_name));
        if removed.is_empty() && written.is_empty() {
            return Ok(());
        }

        let file_before = write_file_rows(&removed, &written)?;
        let table = tokio::time::timeout(
            MUTATE_WRITES_DEADLINE,
            write_table_rows(client, &removed, &written),
        )
        .await
        .unwrap_or_else(|_| {
            Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!(
                    "daemon session writes did not finish within {} ms",
                    MUTATE_WRITES_DEADLINE.as_millis()
                ),
            ))
        });
        if let Err(e) = table {
            if let Err(revert) = restore_file(file_before.as_deref()) {
                return Err(std::io::Error::other(format!(
                    "{e}; and sessions.json could not be put back: {revert}"
                )));
            }
            return Err(e);
        }
        Ok(())
    }
}

/// Apply the row changes to `sessions.json` under the caller's lock: remove
/// every row of a removed session id, upsert every written row by its tmux
/// key (dropping the row an id held under an old tmux name). Rows the change
/// does not touch are left as they are. Goes through `SessionStore`'s own
/// load and save, the same typed path every file writer uses. Returns the
/// file's bytes before the change (`None`: no file) for [`restore_file`].
fn write_file_rows(
    removed: &[Uuid],
    written: &[&WorkspaceSessionEntry],
) -> std::io::Result<Option<Vec<u8>>> {
    let (mut file, before) = load_file_for_write()?;
    for id in removed {
        file.remove_by_session_id(*id);
    }
    for entry in written {
        let meta = entry_to_metadata(entry).map_err(std::io::Error::other)?;
        file.remove_by_session_id(meta.session_id);
        file.upsert(meta);
    }
    file.save()?;
    Ok(before)
}

/// Load `sessions.json` for a write, under the caller's lock, with the bytes
/// it was read from (`None`: no file).
///
/// `SessionStore::load` answers an empty store for a file it cannot parse,
/// which is right for a read and wrong for a write: saving that store would
/// cut a corrupt file down to the rows the write touched. So a file that
/// does not parse is refused here, untouched, before anything is written, on
/// every source.
fn load_file_for_write() -> std::io::Result<(SessionStore, Option<Vec<u8>>)> {
    let path = SessionStore::storage_path();
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok((SessionStore::default(), None));
        }
        Err(e) => return Err(e),
    };
    match serde_json::from_slice::<SessionStore>(&bytes) {
        Ok(store) => Ok((store, Some(bytes))),
        Err(e) => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("sessions.json does not parse ({e}); refusing to rewrite it"),
        )),
    }
}

/// Put `sessions.json` back to `before` (`None`: remove it), under the
/// caller's lock.
fn restore_file(before: Option<&[u8]>) -> std::io::Result<()> {
    let path = SessionStore::storage_path();
    before.map_or_else(
        || match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        },
        |bytes| crate::config::write_atomic(&path, &String::from_utf8_lossy(bytes)),
    )
}

/// Apply the row changes to the daemon's table, each RPC under
/// [`SESSION_RPC_DEADLINE`].
async fn write_table_rows(
    client: &DaemonClient,
    removed: &[Uuid],
    written: &[&WorkspaceSessionEntry],
) -> std::io::Result<()> {
    for id in removed {
        within_deadline(
            "delete",
            client.workspace_session_delete(WorkspaceSessionDeleteParams {
                session_id: Some(id.to_string()),
                tmux_session_name: None,
            }),
        )
        .await?;
    }
    for entry in written {
        within_deadline(
            "write",
            client.workspace_session_upsert(WorkspaceSessionUpsertParams {
                session: (*entry).clone(),
            }),
        )
        .await?;
    }
    Ok(())
}

/// Every entry keyed by its map key, for change detection.
fn snapshot(store: &SessionStore) -> std::collections::BTreeMap<String, WorkspaceSessionEntry> {
    store.sessions.iter().map(|(k, m)| (k.clone(), metadata_to_entry(m))).collect()
}

fn entries_by_id(store: &SessionStore) -> HashMap<Uuid, WorkspaceSessionEntry> {
    store.sessions.values().map(|m| (m.session_id, metadata_to_entry(m))).collect()
}

static SESSION_SOURCE: tokio::sync::OnceCell<std::sync::RwLock<SessionSource>> =
    tokio::sync::OnceCell::const_new();

/// This process's [`SessionSource`], resolved on first use and then fixed,
/// except for the one move out of [`Degraded`](SessionSource::Degraded)
/// that [`leave_degraded`] makes.
pub async fn session_source() -> SessionSource {
    let cell = SESSION_SOURCE
        .get_or_init(|| async { std::sync::RwLock::new(SessionSource::resolve().await) })
        .await;
    cell.read().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
}

/// Try once to move this process from `Degraded` to `Daemon`.
///
/// Reach the daemon, check it speaks the capability, ask it to reconcile (so
/// the sessions written to the file while degraded are in the table) and
/// wait for that pass, then check the table is ready. Returns whether the
/// process is on the daemon now.
///
/// The move is made once; nothing moves the process back.
pub async fn leave_degraded() -> bool {
    let cell = SESSION_SOURCE
        .get_or_init(|| async { std::sync::RwLock::new(SessionSource::resolve().await) })
        .await;
    let current = cell.read().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
    let SessionSource::Degraded(client) = current else {
        return matches!(current, SessionSource::Daemon(_));
    };
    let Some(client) = client.or_else(|| DaemonClient::from_env().ok()) else {
        return false;
    };
    let Some(next) = SessionSource::leave_degraded_with(client).await else {
        return false;
    };
    let mut slot = cell.write().unwrap_or_else(std::sync::PoisonError::into_inner);
    if slot.is_degraded() {
        *slot = next;
    }
    true
}

impl SessionSource {
    /// The checks [`leave_degraded`] makes, against `client`. `None` means
    /// stay degraded for now.
    pub async fn leave_degraded_with(client: DaemonClient) -> Option<Self> {
        let hello = within_deadline("hello", client.hello()).await.ok()?;
        if !hello.advertises(CAP_WORKSPACE_SESSIONS) {
            return None;
        }
        tokio::time::timeout(
            LEAVE_DEGRADED_DEADLINE,
            client.workspace_session_reconcile(),
        )
        .await
        .ok()?
        .ok()?;
        let probe = WorkspaceSessionListParams {
            workspace_name: None,
            limit: Some(1),
        };
        let res = within_deadline("list", client.workspace_session_list(probe)).await.ok()?;
        res.import_complete.then_some(Self::Daemon(client))
    }
}

/// The delays between re-resolve attempts of a degraded long-lived process:
/// 1 s, 4 s, 16 s, then every 16 s (the P6a reconnect cadence).
pub fn reresolve_delays() -> impl Iterator<Item = Duration> {
    [1, 4, 16]
        .into_iter()
        .map(Duration::from_secs)
        .chain(std::iter::repeat(Duration::from_secs(16)))
}

/// Retry [`leave_degraded`] on [`reresolve_delays`] while this process is
/// degraded.
///
/// For a long-lived process (TUI, desktop, `ainb web`); returns once it is
/// on the daemon or was never degraded. A CLI command does not call this;
/// it ends.
pub async fn reresolve_while_degraded() {
    if !session_source().await.is_degraded() {
        return;
    }
    for delay in reresolve_delays() {
        tokio::time::sleep(delay).await;
        if leave_degraded().await {
            return;
        }
    }
}

/// A change in where this process's sessions live that a long-lived surface
/// shows the operator (the TUI and, through the mirrored notifications, the
/// desktop).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionSourceNotice {
    /// Sessions are on the local file until the daemon is up.
    Degraded,
    /// The daemon is up and has reconciled: sessions are back on it.
    Recovered,
}

impl SessionSourceNotice {
    /// The line a surface shows.
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            Self::Degraded => {
                "Sessions are on the local sessions.json until the hangar daemon is up."
            }
            Self::Recovered => "The hangar daemon is up: sessions are back on it.",
        }
    }
}

/// What [`session_source_notice`] last reported: 0 nothing yet, 1 degraded,
/// 2 recovered.
static NOTICE_STATE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// The notice this process's surface owes, if any, each at most once: the
/// first time it is seen degraded, and the move back to the daemon after
/// that. A process that was never degraded owes none.
pub async fn session_source_notice() -> Option<SessionSourceNotice> {
    use std::sync::atomic::Ordering;
    let degraded = session_source().await.is_degraded();
    match (NOTICE_STATE.load(Ordering::SeqCst), degraded) {
        (0, true) => {
            NOTICE_STATE.store(1, Ordering::SeqCst);
            Some(SessionSourceNotice::Degraded)
        }
        (1, false) => {
            NOTICE_STATE.store(2, Ordering::SeqCst);
            Some(SessionSourceNotice::Recovered)
        }
        _ => None,
    }
}

/// Start [`reresolve_while_degraded`] once per process, if it is degraded.
///
/// Spawned on the current tokio runtime, for a long-lived surface's load
/// path; a no-op outside a runtime and on every call after the first.
pub async fn watch_degraded_session_source() {
    static STARTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if !session_source().await.is_degraded()
        || STARTED.swap(true, std::sync::atomic::Ordering::SeqCst)
    {
        return;
    }
    if let Ok(runtime) = tokio::runtime::Handle::try_current() {
        runtime.spawn(reresolve_while_degraded());
    }
}

/// Drive `fut` to completion from sync code, wherever it is called from.
///
/// On a multi-thread runtime the current worker blocks in place. On a
/// current-thread runtime the only thread that can drive IO and timers is the
/// one calling us, so the future runs on a scoped thread with a fresh
/// current-thread runtime of its own: reusing the caller's handle there would
/// wait on a driver that is blocked waiting on us.
fn run_async<F: std::future::Future<Output = T> + Send, T: Send>(fut: F) -> T {
    let fresh = |fut: F| {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("create tokio runtime")
            .block_on(fut)
    };
    match tokio::runtime::Handle::try_current() {
        Ok(handle) if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
            tokio::task::block_in_place(|| handle.block_on(fut))
        }
        Ok(_) => std::thread::scope(|s| s.spawn(|| fresh(fut)).join().expect("thread join")),
        Err(_) => fresh(fut),
    }
}

/// Load the session store from this process's [`session_source`].
///
/// # Errors
///
/// When the current thread holds the `sessions.json` lock, or the daemon was
/// chosen and its RPC fails or times out.
pub async fn load_session_store_async() -> std::io::Result<SessionStore> {
    SessionStore::ensure_lock_not_held()?;
    session_source().await.load().await
}

/// [`load_session_store_async`] from sync code.
///
/// # Errors
///
/// As [`load_session_store_async`].
pub fn load_session_store() -> std::io::Result<SessionStore> {
    SessionStore::ensure_lock_not_held()?;
    run_async(load_session_store_async())
}

/// Mutate the session store through this process's [`session_source`].
///
/// # Errors
///
/// The lock, file or RPC failure; never swallowed.
pub async fn mutate_session_store_async<F>(f: F) -> std::io::Result<()>
where
    F: FnOnce(&mut SessionStore),
{
    SessionStore::ensure_lock_not_held()?;
    session_source().await.mutate(f).await
}

/// [`mutate_session_store_async`] from sync code.
///
/// # Errors
///
/// As [`mutate_session_store_async`].
pub fn mutate_session_store<F>(f: F) -> std::io::Result<()>
where
    F: FnOnce(&mut SessionStore) + Send,
{
    SessionStore::ensure_lock_not_held()?;
    run_async(mutate_session_store_async(f))
}

/// Find a session by ID (full or partial UUID) or workspace name prefix
///
/// Matching priority:
/// 1. Exact UUID match
/// 2. UUID prefix match (e.g., "abc" matches "abc12345-...")
/// 3. Workspace name prefix match (case-insensitive)
///
/// Returns an error if no match is found or if multiple sessions match.
pub fn find_session(id_or_name: &str) -> Result<SessionMetadata> {
    let store = load_session_store()?;
    find_session_in_store(id_or_name, &store)
}

/// Find a session within a given store (testable version)
///
/// This function accepts a store reference for easier unit testing.
pub fn find_session_in_store(id_or_name: &str, store: &SessionStore) -> Result<SessionMetadata> {
    if store.sessions.is_empty() {
        return Err(anyhow!(
            "No sessions found. Run 'ainb run' to create a session."
        ));
    }

    // First, try exact UUID match
    if let Ok(uuid) = Uuid::parse_str(id_or_name) {
        for session in store.sessions.values() {
            if session.session_id == uuid {
                return Ok(session.clone());
            }
        }
    }

    // Try UUID prefix match (case-insensitive)
    let id_lower = id_or_name.to_lowercase();
    let uuid_matches: Vec<&SessionMetadata> = store
        .sessions
        .values()
        .filter(|s| s.session_id.to_string().to_lowercase().starts_with(&id_lower))
        .collect();

    match uuid_matches.len() {
        1 => return Ok(uuid_matches[0].clone()),
        n if n > 1 => {
            let ids: Vec<String> = uuid_matches
                .iter()
                .map(|s| {
                    format!(
                        "  {} ({})",
                        &s.session_id.to_string()[..8],
                        s.display_workspace_name()
                    )
                })
                .collect();
            return Err(anyhow!(
                "Ambiguous session ID prefix '{id_or_name}'. Matches:\n{}",
                ids.join("\n")
            ));
        }
        _ => {}
    }

    // Try workspace name prefix match (case-insensitive).
    //
    // Matches the DISPLAYED name (re-derived from the worktree path, what
    // `ainb list` and the TUI print) OR the name persisted at creation time.
    // Displayed alone would strand legacy records whose path has moved;
    // persisted alone would mean a name the user just read off `ainb list`
    // does not resolve, which is precisely the drift this pair of surfaces is
    // supposed to have stopped having.
    let name_matches: Vec<&SessionMetadata> = store
        .sessions
        .values()
        .filter(|s| {
            s.display_workspace_name().to_lowercase().starts_with(&id_lower)
                || s.workspace_name.to_lowercase().starts_with(&id_lower)
        })
        .collect();

    match name_matches.len() {
        1 => return Ok(name_matches[0].clone()),
        n if n > 1 => {
            let names: Vec<String> = name_matches
                .iter()
                .map(|s| {
                    format!(
                        "  {} ({})",
                        s.display_workspace_name(),
                        &s.session_id.to_string()[..8]
                    )
                })
                .collect();
            return Err(anyhow!(
                "Ambiguous session name prefix '{id_or_name}'. Matches:\n{}",
                names.join("\n")
            ));
        }
        _ => {}
    }

    // No match found - provide helpful error message
    let available: Vec<String> = store
        .sessions
        .values()
        .map(|s| {
            format!(
                "  {} ({})",
                &s.session_id.to_string()[..8],
                s.display_workspace_name()
            )
        })
        .collect();

    if available.is_empty() {
        Err(anyhow!(
            "No sessions found. Run 'ainb run' to create a session."
        ))
    } else {
        Err(anyhow!(
            "No session found matching '{id_or_name}'. Available sessions:\n{}",
            available.join("\n")
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::session::SessionAgentType;
    use chrono::Utc;
    use std::path::PathBuf;

    fn create_test_store() -> SessionStore {
        let mut store = SessionStore::default();

        let session1 = SessionMetadata {
            session_id: Uuid::parse_str("12345678-1234-1234-1234-123456789abc").unwrap(),
            tmux_session_name: "tmux_project-a".to_string(),
            worktree_path: PathBuf::from("/tmp/project-a"),
            workspace_name: "project-alpha".to_string(),
            created_at: Utc::now(),
            agent_type: SessionAgentType::default(),
            headroom_enabled: false,
            rtk_enabled: false,
            skip_permissions: None,
            model: None,
            model_source: Default::default(),
            codex_model: None,
            codex_thread_id: None,
        };

        let session2 = SessionMetadata {
            session_id: Uuid::parse_str("abcdef12-abcd-abcd-abcd-abcdef123456").unwrap(),
            tmux_session_name: "tmux_project-b".to_string(),
            worktree_path: PathBuf::from("/tmp/project-b"),
            workspace_name: "project-beta".to_string(),
            created_at: Utc::now(),
            agent_type: SessionAgentType::default(),
            headroom_enabled: false,
            rtk_enabled: false,
            skip_permissions: None,
            model: None,
            model_source: Default::default(),
            codex_model: None,
            codex_thread_id: None,
        };

        store.sessions.insert(session1.tmux_session_name.clone(), session1);
        store.sessions.insert(session2.tmux_session_name.clone(), session2);

        store
    }

    #[test]
    fn test_find_by_exact_uuid() {
        let store = create_test_store();
        let result = find_session_in_store("12345678-1234-1234-1234-123456789abc", &store);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().workspace_name, "project-alpha");
    }

    #[test]
    fn test_find_by_uuid_prefix() {
        let store = create_test_store();
        let result = find_session_in_store("12345678", &store);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().workspace_name, "project-alpha");
    }

    #[test]
    fn test_find_by_workspace_prefix() {
        let store = create_test_store();
        let result = find_session_in_store("project-a", &store);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().workspace_name, "project-alpha");
    }

    #[test]
    fn test_find_by_workspace_prefix_case_insensitive() {
        let store = create_test_store();
        let result = find_session_in_store("PROJECT-B", &store);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().workspace_name, "project-beta");
    }

    #[test]
    fn test_ambiguous_prefix() {
        let store = create_test_store();
        let result = find_session_in_store("project-", &store);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Ambiguous"));
    }

    #[test]
    fn test_not_found() {
        let store = create_test_store();
        let result = find_session_in_store("nonexistent", &store);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("No session found"));
    }

    #[test]
    fn a_non_uuid_entry_is_refused_not_given_a_fresh_id() {
        let mut entry = metadata_to_entry(&create_test_store().sessions["tmux_project-a"]);
        entry.session_id = "01J8Z3K6Q2N4T5V7W9X0Y1Z2A3".to_string();
        assert!(entry_to_metadata(&entry).is_err());
    }

    #[test]
    fn entry_round_trip_keeps_the_id() {
        let meta = create_test_store().sessions["tmux_project-a"].clone();
        let back = entry_to_metadata(&metadata_to_entry(&meta)).unwrap();
        assert_eq!(back.session_id, meta.session_id);
    }

    #[test]
    fn enum_names_round_trip() {
        use crate::models::session::CodexModel;
        fn back<T: serde::de::DeserializeOwned>(name: String) -> T {
            serde_json::from_value(serde_json::Value::String(name)).unwrap()
        }
        for v in [
            SessionAgentType::Claude,
            SessionAgentType::Shell,
            SessionAgentType::Ssh,
            SessionAgentType::Codex,
            SessionAgentType::Gemini,
            SessionAgentType::Copilot,
            SessionAgentType::Antigravity,
            SessionAgentType::Kiro,
        ] {
            assert_eq!(back::<SessionAgentType>(format!("{v:?}")), v);
        }
        for v in [ModelSource::LegacyTyped, ModelSource::Raw] {
            assert_eq!(back::<ModelSource>(format!("{v:?}")), v);
        }
        for v in [
            CodexModel::SystemDefault,
            CodexModel::Gpt55,
            CodexModel::Gpt56Terra,
            CodexModel::Gpt56Luna,
            CodexModel::Gpt53Codex,
        ] {
            assert_eq!(back::<CodexModel>(format!("{v:?}")), v);
        }
    }

    /// Called from inside a current-thread runtime, `run_async` must finish a
    /// future that needs the timer driver. Reusing the caller's handle
    /// deadlocked here, so the test fails on a timeout rather than hanging.
    #[test]
    fn run_async_completes_inside_a_current_thread_runtime() {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
            let got = rt.block_on(async {
                run_async(async {
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                    7
                })
            });
            let _ = tx.send(got);
        });
        let got = rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("run_async deadlocked inside a current-thread runtime");
        assert_eq!(got, 7);
    }

    #[test]
    fn test_empty_store() {
        let store = SessionStore::default();
        let result = find_session_in_store("anything", &store);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("No sessions found"));
    }
}
