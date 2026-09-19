//! One-time boot import of `~/.agents-in-a-box/sessions.json` into the
//! daemon-owned `sessions` table (spec P6d, #1166).
//!
//! ```text
//! boot ──▶ marker for this path? ──yes──▶ AlreadyCompleted (nothing read)
//!                │ no
//!                ▼
//!          file missing ──▶ marker, 0 rows (fresh home)
//!          over cap / unreadable / unparseable ──▶ Err, NO marker
//!          parsed ──▶ validate each record ──▶ rows + marker, one tx
//! ```
//!
//! The file is only ever read, never written, so a user who downgrades keeps
//! every session it held. The marker (migration 0102) is what makes the import
//! one-time: a row deleted after the import is not brought back by the next
//! boot. Until a marker exists, `workspace/session_list` answers
//! `import_complete: false` and the CLI keeps reading the file, so a failed
//! import can never make a populated file look empty.
//!
//! # Repeatable reconcile (P6e)
//!
//! While P6d was dark every surface wrote the file only, so the file holds
//! sessions the table lacks. [`reconcile_sessions`] closes that gap and keeps
//! closing it: it runs at boot after the import, on a client's
//! `workspace/session_reconcile`, and whenever the file's mtime moves
//! ([`ReconcileWatch`], every [`RECONCILE_INTERVAL`]).
//!
//! ```text
//! pass ──▶ flock sessions.json (bounded) ──▶ read + parse
//!            │ timeout / read / parse error ──▶ Err, marker unchanged
//!            ▼
//!          one tx: insert every file session whose id the table lacks,
//!          skip + count a tmux name bound to another id (table wins),
//!          write the <path>#reconcile marker ──▶ release the flock
//! ```
//!
//! The pass holds the flock from before its read until its commit, so no
//! flock-taking writer can add or remove a file row in between. It never
//! deletes and never overwrites a table row, so it cannot resurrect a session
//! the new stack removed: that stack removes the file row too. The table is
//! authoritative for a client only once the import AND a pass have finished
//! (`SessionsRepo::import_complete_for`).

use ainb_hangar_core::clock::{HangarClock, SystemClock};
use ainb_hangar_proto::sessions::WorkspaceSessionEntry;
use ainb_hangar_store::repo::sessions::{FileSession, ImportOutcome, SessionRow, SessionsRepo};
use anyhow::{Context, Result, bail};
use sqlx::SqlitePool;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

pub use ainb_hangar_store::repo::sessions::{ImportMarker, NameConflict, ReconcileOutcome};

/// How long a daemon writer waits for the `sessions.json` flock.
///
/// Every daemon wait on that flock is bounded, so a client holding it across
/// an RPC to this daemon can never deadlock the two.
pub const SESSIONS_FLOCK_BOUND: Duration = Duration::from_secs(2);

/// How long a pass may spend in its store write while it holds the flock.
///
/// The pool's acquire and `busy_timeout` waits are tens of seconds, and every
/// CLI writer blocks on the flock meanwhile. Cancelling at this bound is safe:
/// the write is one transaction, so a cancelled pass changes nothing and the
/// next one redoes it.
pub const RECONCILE_STORE_BOUND: Duration = Duration::from_secs(5);

/// How often [`ReconcileWatch`] checks whether the file changed.
pub const RECONCILE_INTERVAL: Duration = Duration::from_secs(30);

/// How soon [`ReconcileWatch`] retries after a failed pass, doubling on each
/// further failure up to [`RECONCILE_INTERVAL`].
pub const RECONCILE_RETRY_FIRST: Duration = Duration::from_secs(1);

/// The `sessions.json` this daemon imports, reconciles and serves.
///
/// Every daemon site (the boot import, the watcher, the session RPCs) goes
/// through here, so they cannot disagree on the file. It is resolved on each
/// call rather than cached: `AINB_HOME` is read per call by every client too,
/// and in-process test daemons change it between tests.
#[must_use]
pub fn daemon_sessions_path() -> PathBuf {
    ainb_fleet_core::session_registry::sessions_json_path()
}

/// How long a session read waits for this boot's first pass before it
/// answers not-ready. The same cap as the flock wait that pass may be stuck on.
pub const FIRST_PASS_WAIT: Duration = Duration::from_secs(2);

/// This process's first-pass gate: `false` until a pass has committed since
/// boot. Absent until [`arm_first_pass_gate`] runs, which only the daemon's
/// boot does; an in-process harness that never boots has no gate and its
/// reads are never held.
static FIRST_PASS: std::sync::OnceLock<tokio::sync::watch::Sender<bool>> =
    std::sync::OnceLock::new();

/// Arm the first-pass gate: session reads wait until a pass commits.
///
/// P6e, amended: the socket opens at once, the first read waits for the
/// first pass. The boot pass runs in the background, and the table may
/// still hold rows from a previous boot that this boot's file no longer
/// matches, so no read may be served from it before this process has
/// reconciled once.
pub fn arm_first_pass_gate() {
    FIRST_PASS.get_or_init(|| tokio::sync::watch::channel(false).0);
}

/// Whether session reads may be served from the table: the gate is unarmed,
/// or this process's first pass commits within `wait`.
pub async fn first_pass_done_within(wait: Duration) -> bool {
    let Some(gate) = FIRST_PASS.get() else {
        return true;
    };
    let mut done = gate.subscribe();
    let waited = tokio::time::timeout(wait, done.wait_for(|done| *done)).await;
    waited.is_ok_and(|seen| seen.is_ok())
}

/// How long a pass waits for another pass in this process to finish: the
/// most one pass can hold [`PASS`] for, its flock wait plus its store write.
const PASS_WAIT: Duration = SESSIONS_FLOCK_BOUND.saturating_add(RECONCILE_STORE_BOUND);

/// Serialises passes within this process. Two passes would otherwise take the
/// flock on two descriptors and the second would time out.
static PASS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Largest `sessions.json` the import reads. A real store is a few KB per
/// session; anything past this is not a session store.
pub const SESSIONS_JSON_MAX_BYTES: u64 = 8 * 1024 * 1024;

/// What one boot's import did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportReport {
    /// This boot imported and wrote the marker.
    Completed(ImportMarker),
    /// An earlier boot finished the import of this file; nothing was read.
    AlreadyCompleted,
}

/// Import `sessions_path` once, capped at [`SESSIONS_JSON_MAX_BYTES`].
///
/// # Errors
///
/// Returns an error, and writes no marker, when the file is over the cap,
/// unreadable or unparseable, or the store write fails. The caller logs it;
/// clients see `import_complete: false` and keep reading the file.
pub async fn import_sessions_if_needed(
    pool: &SqlitePool,
    sessions_path: &Path,
) -> Result<ImportReport> {
    import_sessions_from(pool, sessions_path, SESSIONS_JSON_MAX_BYTES).await
}

/// [`import_sessions_if_needed`] with an explicit size cap (the test seam).
///
/// # Errors
///
/// As [`import_sessions_if_needed`].
pub async fn import_sessions_from(
    pool: &SqlitePool,
    sessions_path: &Path,
    max_bytes: u64,
) -> Result<ImportReport> {
    let source = sessions_path.to_string_lossy().into_owned();
    if SessionsRepo::import_marker(pool, &source).await?.is_some() {
        return Ok(ImportReport::AlreadyCompleted);
    }

    let path = sessions_path.to_path_buf();
    let content = tokio::task::spawn_blocking(move || read_capped(&path, max_bytes))
        .await
        .context("sessions.json read task")??;

    let (sessions, rejected) = match content {
        None => (Vec::new(), 0),
        Some(content) => parse_records(&content)
            .with_context(|| format!("could not parse {}", sessions_path.display()))?,
    };
    let rows: Vec<SessionRow> = sessions.into_iter().map(|s| s.row).collect();

    let outcome =
        SessionsRepo::complete_import(pool, &source, &rows, rejected, SystemClock.now_ms()).await?;
    Ok(match outcome {
        ImportOutcome::Completed(marker) => ImportReport::Completed(marker),
        ImportOutcome::AlreadyCompleted => ImportReport::AlreadyCompleted,
    })
}

/// Run one reconcile pass of `sessions_path`, capped at
/// [`SESSIONS_JSON_MAX_BYTES`]. See the module doc for the rule.
///
/// # Errors
///
/// Returns an error, and leaves the marker as it was, when the flock is not
/// free within [`SESSIONS_FLOCK_BOUND`], the file is over the cap, unreadable
/// or unparseable, or the store write fails or outlasts
/// [`RECONCILE_STORE_BOUND`].
pub async fn reconcile_sessions(
    pool: &SqlitePool,
    sessions_path: &Path,
) -> Result<ReconcileOutcome> {
    reconcile_sessions_from(pool, sessions_path, SESSIONS_JSON_MAX_BYTES).await
}

/// [`reconcile_sessions`] with an explicit size cap (the test seam).
///
/// # Errors
///
/// As [`reconcile_sessions`].
pub async fn reconcile_sessions_from(
    pool: &SqlitePool,
    sessions_path: &Path,
    max_bytes: u64,
) -> Result<ReconcileOutcome> {
    reconcile_sessions_bounded(pool, sessions_path, max_bytes, RECONCILE_STORE_BOUND).await
}

/// [`reconcile_sessions_from`] with an explicit store-write bound (the test
/// seam for [`RECONCILE_STORE_BOUND`]).
///
/// # Errors
///
/// As [`reconcile_sessions`].
pub async fn reconcile_sessions_bounded(
    pool: &SqlitePool,
    sessions_path: &Path,
    max_bytes: u64,
    store_bound: Duration,
) -> Result<ReconcileOutcome> {
    reconcile_pass(pool, sessions_path, max_bytes, store_bound)
        .await
        .map(|(outcome, _)| outcome)
}

/// One pass, also returning the file's stamp as read under the flock.
async fn reconcile_pass(
    pool: &SqlitePool,
    sessions_path: &Path,
    max_bytes: u64,
    store_bound: Duration,
) -> Result<(ReconcileOutcome, Option<FileStamp>)> {
    let _pass = tokio::time::timeout(PASS_WAIT, PASS.lock())
        .await
        .map_err(|_| anyhow::anyhow!("another sessions reconcile pass is still running"))?;
    let dir = sessions_path.parent().context("sessions.json has no parent directory")?;
    let flock = acquire_sessions_flock(dir, SESSIONS_FLOCK_BOUND).await?;

    let path = sessions_path.to_path_buf();
    let (stamp, content) = tokio::task::spawn_blocking(move || {
        let stamp = FileStamp::of(&path);
        read_capped(&path, max_bytes).map(|content| (stamp, content))
    })
    .await
    .context("sessions.json read task")??;
    let (sessions, rejected) = match content {
        None => (Vec::new(), 0),
        Some(content) => parse_records(&content)
            .with_context(|| format!("could not parse {}", sessions_path.display()))?,
    };

    let source = sessions_path.to_string_lossy().into_owned();
    let write =
        SessionsRepo::complete_reconcile(pool, &source, &sessions, rejected, SystemClock.now_ms());
    let outcome = tokio::time::timeout(store_bound, write).await.map_err(|_| {
        anyhow::anyhow!(
            "sessions store write did not finish within {} ms",
            store_bound.as_millis()
        )
    })??;
    drop(flock);
    if let Some(gate) = FIRST_PASS.get() {
        gate.send_replace(true);
    }

    for conflict in &outcome.conflicts {
        tracing::warn!(
            session_id = %conflict.session_id,
            holder = %conflict.holder,
            tmux_session_name = %conflict.tmux_session_name.escape_debug(),
            "sessions.json session not reconciled: its tmux name belongs to another session in the table"
        );
    }
    Ok((outcome, stamp))
}

/// Take the `sessions.json` flock in `dir`, retrying for at most `bound`.
///
/// # Errors
///
/// Returns an error when the flock is still held after `bound`, or when
/// taking it fails for another reason.
pub async fn acquire_sessions_flock(dir: &Path, bound: Duration) -> Result<std::fs::File> {
    let deadline = tokio::time::Instant::now() + bound;
    loop {
        let attempt = dir.to_path_buf();
        let got = tokio::task::spawn_blocking(move || {
            ainb_fleet_core::session_registry::try_lock_sessions_store_at(&attempt)
        })
        .await
        .context("sessions.json lock task")??;
        if let Some(flock) = got {
            return Ok(flock);
        }
        if tokio::time::Instant::now() >= deadline {
            bail!(
                "sessions.json lock in {} still held after {} ms",
                dir.display(),
                bound.as_millis()
            );
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// Runs a reconcile pass when `sessions.json` has changed since the last
/// successful one, or when the last one failed.
#[derive(Debug)]
pub struct ReconcileWatch {
    path: std::path::PathBuf,
    max_bytes: u64,
    last: LastPass,
}

/// What [`ReconcileWatch`] knows about its previous pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LastPass {
    /// No pass has run yet, or the last one failed: the next tick runs one.
    Due,
    /// The last pass succeeded having read this stamp (`None`: no file).
    Read(Option<FileStamp>),
}

/// What says `sessions.json` changed: mtime, length and inode together.
///
/// mtime alone misses a rewrite that keeps it (`cp -p`, a restore, a
/// filesystem with one-second granularity); the atomic temp-and-rename every
/// writer does gives the file a new inode, and most edits change its length.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileStamp {
    mtime: Option<SystemTime>,
    len: u64,
    inode: u64,
}

impl FileStamp {
    /// The stamp of `path`, `None` when it cannot be read (a missing file).
    fn of(path: &Path) -> Option<Self> {
        let meta = std::fs::metadata(path).ok()?;
        #[cfg(unix)]
        let inode = std::os::unix::fs::MetadataExt::ino(&meta);
        #[cfg(not(unix))]
        let inode = 0;
        Some(Self {
            mtime: meta.modified().ok(),
            len: meta.len(),
            inode,
        })
    }
}

impl ReconcileWatch {
    /// Watch `sessions_path` with the default size cap.
    #[must_use]
    pub fn new(sessions_path: &Path) -> Self {
        Self::with_cap(sessions_path, SESSIONS_JSON_MAX_BYTES)
    }

    /// Watch `sessions_path` with an explicit size cap (the test seam).
    #[must_use]
    pub fn with_cap(sessions_path: &Path, max_bytes: u64) -> Self {
        Self {
            path: sessions_path.to_path_buf(),
            max_bytes,
            last: LastPass::Due,
        }
    }

    /// Run a pass if one is due. `None` means the file is unchanged since the
    /// last successful pass and nothing ran.
    pub async fn tick(&mut self, pool: &SqlitePool) -> Option<Result<ReconcileOutcome>> {
        let path = self.path.clone();
        let now = tokio::task::spawn_blocking(move || FileStamp::of(&path)).await.ok()?;
        if self.last == LastPass::Read(now) {
            return None;
        }
        Some(
            match reconcile_pass(pool, &self.path, self.max_bytes, RECONCILE_STORE_BOUND).await {
                Ok((outcome, stamp)) => {
                    self.last = LastPass::Read(stamp);
                    Ok(outcome)
                }
                Err(e) => {
                    self.last = LastPass::Due;
                    Err(e)
                }
            },
        )
    }

    /// Tick for the life of the process, logging each pass that ran.
    ///
    /// The first tick is immediate. After a success, or an unchanged file,
    /// the next is [`RECONCILE_INTERVAL`] later. After a failure the next
    /// comes sooner, [`RECONCILE_RETRY_FIRST`] doubling up to the interval:
    /// until the first pass commits, every session read waits on the gate,
    /// so a boot pass that lost a race for the flock is retried in seconds,
    /// not half a minute.
    ///
    /// A failure is warned about once; the same failure on later ticks (a
    /// file left malformed) is logged at debug, so it does not warn every
    /// 30 s forever. A different failure, or a success, resets that.
    pub async fn run(mut self, pool: SqlitePool) {
        let mut warned: Option<String> = None;
        let mut retry = RECONCILE_RETRY_FIRST;
        loop {
            let next = match self.tick(&pool).await {
                None => RECONCILE_INTERVAL,
                Some(Ok(outcome)) => {
                    warned = None;
                    retry = RECONCILE_RETRY_FIRST;
                    log_reconcile(&outcome);
                    RECONCILE_INTERVAL
                }
                Some(Err(e)) => {
                    let error = format!("{e:#}");
                    if warned.as_deref() == Some(error.as_str()) {
                        tracing::debug!(%error, "sessions.json reconcile still failing");
                    } else {
                        tracing::warn!(
                            %error,
                            "sessions.json reconcile failed; retrying with backoff, warned once"
                        );
                        warned = Some(error);
                    }
                    let wait = retry;
                    retry = retry.saturating_mul(2).min(RECONCILE_INTERVAL);
                    wait
                }
            };
            tokio::time::sleep(next).await;
        }
    }
}

/// Log a finished pass: quiet when it changed nothing.
pub fn log_reconcile(outcome: &ReconcileOutcome) {
    let m = &outcome.marker;
    if m.imported > 0 || m.skipped > 0 || m.rejected > 0 {
        tracing::info!(
            imported = m.imported,
            skipped = m.skipped,
            rejected = m.rejected,
            "sessions.json reconciled into the sessions table"
        );
    }
}

/// Read `path` if it exists and is at most `max_bytes`. `Ok(None)` means no
/// file (a fresh home).
fn read_capped(path: &Path, max_bytes: u64) -> Result<Option<String>> {
    let meta = match std::fs::metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("could not stat {}", path.display())),
    };
    if !meta.is_file() {
        bail!("{} is not a regular file", path.display());
    }
    if meta.len() > max_bytes {
        bail!(
            "{} is {} bytes, over the {max_bytes} byte import limit",
            path.display(),
            meta.len()
        );
    }
    std::fs::read_to_string(path)
        .map(Some)
        .with_context(|| format!("could not read {}", path.display()))
}

/// Parse the store into valid sessions plus a count of rejected records.
fn parse_records(content: &str) -> Result<(Vec<FileSession>, i64)> {
    let value: serde_json::Value = serde_json::from_str(content).context("parse sessions.json")?;
    let Some(sessions) = value.get("sessions").and_then(serde_json::Value::as_object) else {
        bail!("parse sessions.json: no `sessions` object");
    };

    let mut rows = Vec::with_capacity(sessions.len());
    let mut rejected = 0_i64;
    for (tmux_key, record) in sessions {
        match record_to_entry(tmux_key, record).and_then(|(entry, id_minted)| {
            entry.validate()?;
            Ok((entry, id_minted))
        }) {
            Ok((entry, id_minted)) => rows.push(FileSession {
                row: entry_to_row(entry),
                id_minted,
            }),
            Err(why) => {
                rejected += 1;
                tracing::warn!(
                    tmux_key = %tmux_key.escape_debug(),
                    %why,
                    "sessions.json record not imported; it stays in the file"
                );
            }
        }
    }
    Ok((rows, rejected))
}

/// Map one file record onto the wire entry.
///
/// A record with no `session_id` gets a fresh UUID, minted here and stored by
/// whichever write inserts it; the flag in the answer says so, because the
/// next read mints a different one. A record whose `session_id` is present
/// keeps it verbatim; if it is not a UUID, validation rejects the record
/// rather than inventing a replacement.
fn record_to_entry(
    tmux_key: &str,
    record: &serde_json::Value,
) -> Result<(WorkspaceSessionEntry, bool), String> {
    let text = |key: &str| record.get(key).and_then(serde_json::Value::as_str);
    let flag = |key: &str| record.get(key).and_then(serde_json::Value::as_bool);

    let (session_id, id_minted) = match record.get("session_id") {
        None | Some(serde_json::Value::Null) => (uuid::Uuid::new_v4().to_string(), true),
        Some(serde_json::Value::String(id)) => (id.clone(), false),
        Some(_) => return Err("session_id is not a string".to_string()),
    };
    let created_at = match record.get("created_at") {
        Some(serde_json::Value::Number(n)) => {
            n.as_i64().ok_or_else(|| "created_at is not an integer".to_string())?
        }
        Some(serde_json::Value::String(s)) => chrono::DateTime::parse_from_rfc3339(s)
            .map(|dt| dt.timestamp_millis())
            .map_err(|e| format!("created_at: {e}"))?,
        _ => return Err("created_at is missing".to_string()),
    };

    let entry = WorkspaceSessionEntry {
        session_id,
        tmux_session_name: text("tmux_session_name").unwrap_or(tmux_key).to_string(),
        worktree_path: text("worktree_path").unwrap_or_default().to_string(),
        workspace_name: text("workspace_name").unwrap_or("default").to_string(),
        created_at,
        agent_type: text("agent_type").unwrap_or("Claude").to_string(),
        headroom_enabled: flag("headroom_enabled").unwrap_or(false),
        rtk_enabled: flag("rtk_enabled").unwrap_or(false),
        skip_permissions: flag("skip_permissions"),
        model: text("model").map(str::to_string),
        model_source: text("model_source").unwrap_or("LegacyTyped").to_string(),
        codex_model: text("codex_model").map(str::to_string),
        codex_thread_id: text("codex_thread_id").map(str::to_string),
    };
    Ok((entry, id_minted))
}

fn entry_to_row(entry: WorkspaceSessionEntry) -> SessionRow {
    SessionRow {
        session_id: entry.session_id,
        tmux_session_name: entry.tmux_session_name,
        worktree_path: entry.worktree_path,
        workspace_name: entry.workspace_name,
        created_at: entry.created_at,
        agent_type: entry.agent_type,
        headroom_enabled: entry.headroom_enabled,
        rtk_enabled: entry.rtk_enabled,
        skip_permissions: entry.skip_permissions,
        model: entry.model,
        model_source: entry.model_source,
        codex_model: entry.codex_model,
        codex_thread_id: entry.codex_thread_id,
    }
}
