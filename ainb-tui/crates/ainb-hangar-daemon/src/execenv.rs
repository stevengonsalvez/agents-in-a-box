//! Per-task execution-environment layout (P1.6).
//!
//! Every dispatched task gets an isolated on-disk sandbox so its working tree,
//! emitted artifacts, and provider logs never collide with another task's. The
//! layout mirrors Multica's (`build-plan.md:40`, `CLI_AND_DAEMON.md:185-193`):
//!
//! ```text
//! {home}/.ainb/hangar/workspaces/{ws_slug}/{shortID}/
//! ├── workdir/        # the agent's CWD (a git worktree for issue tasks)
//! ├── output/         # emitted artifacts (patches, diffs)
//! ├── logs/           # provider JSONL streams (claude.jsonl, …)
//! └── .gc_meta.json   # GC marker: who owns this dir + when it was seen
//! ```
//!
//! `shortID` is the first 8 characters of the task ULID (Multica's short form).
//! Because the ULID is injected via [`crate`]'s id-generation seam in tests, the
//! path is deterministic across runs.
//!
//! # GC marker
//!
//! `.gc_meta.json` lets the orphan scanner ([`cleanup`] with
//! [`CleanupKind::OrphanScan`]) distinguish a live task dir from a leaked one:
//! a dir with no marker whose mtime is older than 72h is a leak and is removed.
//! [`prepare_env`] is idempotent — a re-`prepare_env` (e.g. a resumed task)
//! reuses the dirs and only refreshes `.gc_meta.json.last_seen_at`, never
//! clobbering an existing `workdir/`.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use ainb_hangar_core::clock::HangarClock;
use ainb_hangar_store::repo::task::Task;
use serde::{Deserialize, Serialize};

/// Age (in milliseconds) past which an orphan dir (no `.gc_meta.json`) is
/// removed by the orphan scanner. Matches Multica's 72h GC grace window.
const ORPHAN_GRACE_MS: i64 = 72 * 3_600 * 1_000;

/// The materialised per-task directory set.
///
/// All paths are absolute and live under the task's `{shortID}` root; the root
/// itself is `workdir.parent()` (every field shares it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecEnv {
    /// The agent's working directory (a git worktree for issue tasks).
    pub workdir: PathBuf,
    /// Emitted-artifact directory (patches, diffs).
    pub output: PathBuf,
    /// Provider-log directory (`claude.jsonl`, …).
    pub logs: PathBuf,
    /// The `.gc_meta.json` marker file.
    pub gc_meta: PathBuf,
}

impl ExecEnv {
    /// The per-task `{shortID}` root directory (parent of every sibling dir).
    #[must_use]
    pub fn root(&self) -> &Path {
        // `workdir` is `<root>/workdir`, so its parent is the root. All four
        // fields are built from the same root, so this is always `Some`.
        self.workdir.parent().unwrap_or(&self.workdir)
    }
}

/// The `parent` block of [`GcMeta`]: what originated this task dir.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GcParent {
    /// Origin kind — `"issue"` for issue-driven tasks, `"chat"` otherwise.
    pub kind: String,
    /// Origin id (the issue id, or the task id for chat tasks).
    pub id: String,
}

/// Contents of `.gc_meta.json` (Multica shape, `CLI_AND_DAEMON.md:185-193`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GcMeta {
    /// The owning task id.
    pub task_id: String,
    /// The originating issue id, or `None` for chat tasks.
    pub issue_id: Option<String>,
    /// The owning workspace id.
    pub workspace_id: String,
    /// When the dir was first created (epoch milliseconds).
    pub created_at: i64,
    /// When the dir was last re-prepared (epoch milliseconds); `None` until the
    /// first idempotent re-`prepare_env`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_seen_at: Option<i64>,
    /// What originated this dir.
    pub parent: GcParent,
}

/// What [`cleanup`] should remove.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CleanupKind {
    /// Remove the entire per-task tree (`{shortID}/` and everything under it).
    Full,
    /// Remove only emitted artifacts (`output/`), keeping `workdir/` and `logs/`.
    ArtifactOnly,
    /// Remove the dir only if it is an orphan: no `.gc_meta.json` marker AND its
    /// mtime is older than 72h relative to `now_ms`. A live (marked) dir is left
    /// untouched.
    OrphanScan {
        /// "Now" in epoch milliseconds (injected so the scan is deterministic).
        now_ms: i64,
    },
}

/// The Multica short form of a task id: its first 8 characters.
///
/// Ids shorter than 8 chars are returned whole (defensive; real ULIDs are 26).
/// ULIDs are ASCII (Crockford base32), so byte-slicing is safe here.
#[must_use]
pub fn short_id(id: &str) -> &str {
    id.get(..8).unwrap_or(id)
}

/// Create (or reuse) the per-task directory layout under `home`, writing the
/// `.gc_meta.json` marker.
///
/// The layout root is
/// `{home}/.ainb/hangar/workspaces/{ws_slug}/{short_id(task.id)}/`. The call is
/// **idempotent**: a second `prepare_env` on the same task reuses every dir
/// (never wiping an existing `workdir/`) and only refreshes the marker's
/// `last_seen_at` to `clock.now_ms()`, preserving the original `created_at`.
///
/// # Errors
///
/// Returns an [`io::Error`] if a directory cannot be created or the marker
/// cannot be (de)serialised / written.
pub fn prepare_env(
    task: &Task,
    ws_slug: &str,
    home: &Path,
    clock: &dyn HangarClock,
) -> io::Result<ExecEnv> {
    let root = home
        .join(".ainb")
        .join("hangar")
        .join("workspaces")
        .join(ws_slug)
        .join(short_id(&task.id));

    let env = ExecEnv {
        workdir: root.join("workdir"),
        output: root.join("output"),
        logs: root.join("logs"),
        gc_meta: root.join(".gc_meta.json"),
    };

    for dir in [&env.workdir, &env.output, &env.logs] {
        fs::create_dir_all(dir)?;
    }

    write_marker(task, clock, &env.gc_meta)?;

    Ok(env)
}

/// Write or refresh the `.gc_meta.json` marker.
///
/// On first write the marker captures `created_at = clock.now_ms()`. On a
/// re-prepare (marker already present) `created_at` is preserved and only
/// `last_seen_at` is bumped, so the GC grace window is anchored to the original
/// creation, not the latest touch.
fn write_marker(task: &Task, clock: &dyn HangarClock, path: &Path) -> io::Result<()> {
    let now = clock.now_ms();
    let meta = match fs::read_to_string(path) {
        Ok(raw) => {
            let mut existing: GcMeta = serde_json::from_str(&raw)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            existing.last_seen_at = Some(now);
            existing
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            let (kind, id) = task.issue_id.as_ref().map_or_else(
                || ("chat".to_string(), task.id.clone()),
                |issue| ("issue".to_string(), issue.clone()),
            );
            GcMeta {
                task_id: task.id.clone(),
                issue_id: task.issue_id.clone(),
                workspace_id: task.workspace_id.clone(),
                created_at: now,
                last_seen_at: None,
                parent: GcParent { kind, id },
            }
        }
        Err(e) => return Err(e),
    };

    let json = serde_json::to_string_pretty(&meta).map_err(io::Error::other)?;
    fs::write(path, json)
}

/// Remove parts of a task's env per [`CleanupKind`].
///
/// - [`CleanupKind::Full`] removes the whole `{shortID}/` tree.
/// - [`CleanupKind::ArtifactOnly`] removes `output/`, keeping `workdir/` (incl.
///   its `.git/`) and `logs/`.
/// - [`CleanupKind::OrphanScan`] removes the tree only when it is an orphan:
///   no `.gc_meta.json` marker AND root mtime older than 72h. A live dir is a
///   no-op.
///
/// All variants are tolerant of an already-absent target (idempotent cleanup).
///
/// # Errors
///
/// Returns an [`io::Error`] if a present target cannot be removed or its mtime
/// cannot be read.
pub fn cleanup(env: &ExecEnv, kind: CleanupKind) -> io::Result<()> {
    match kind {
        CleanupKind::Full => remove_tree(env.root()),
        CleanupKind::ArtifactOnly => remove_tree(&env.output),
        CleanupKind::OrphanScan { now_ms } => {
            if env.gc_meta.exists() {
                // Still marked → live, leave it.
                return Ok(());
            }
            if is_older_than(env.root(), now_ms, ORPHAN_GRACE_MS)? {
                remove_tree(env.root())?;
            }
            Ok(())
        }
    }
}

/// `rm -rf` a directory, treating "already gone" as success.
fn remove_tree(dir: &Path) -> io::Result<()> {
    match fs::remove_dir_all(dir) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Whether `dir`'s mtime is older than `grace_ms` before `now_ms`.
///
/// A dir that no longer exists is treated as "not old" (nothing to remove).
fn is_older_than(dir: &Path, now_ms: i64, grace_ms: i64) -> io::Result<bool> {
    let meta = match fs::metadata(dir) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e),
    };
    let mtime_ms = system_time_to_ms(meta.modified()?);
    Ok(now_ms - mtime_ms > grace_ms)
}

/// Convert a [`SystemTime`] to epoch milliseconds, saturating before the epoch
/// to 0 (no mtime in Hangar predates 1970).
#[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
fn system_time_to_ms(t: SystemTime) -> i64 {
    t.duration_since(SystemTime::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}
