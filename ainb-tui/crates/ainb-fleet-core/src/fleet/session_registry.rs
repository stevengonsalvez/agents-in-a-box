// ABOUTME: The ainb session registry — the `sessions.json` store `ainb list`
// reads, plus the `AINB_PARENT_SESSION` wire constant, hosted here so the hangar
// daemon can make a daemon-spawned session fleet-visible without a crate cycle.
//
// WHY HERE (not ainb-core): `ainb-core` owns `SessionStore` / `SessionMetadata`
// and the `ainb run` / `ainb list` CLI, but `ainb-core` depends on
// `ainb-hangar-daemon`, so the daemon can NOT depend back on it to register a
// session it spawns. This fleet-core seam is the shared lower layer both use:
// the daemon registers a session HERE at spawn, and the CLI's `SessionStore::load`
// reads it back. The persisted shape is a strict SUBSET of `SessionMetadata` —
// the optional `agent_type` / `headroom_enabled` / `rtk_enabled` fields it omits
// are `#[serde(default)]` on the read side — so this is NOT a parallel store: it
// is the one `~/.agents-in-a-box/sessions.json` file, keyed by tmux session name.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use fs2::FileExt;
use serde::Serialize;
use uuid::Uuid;

/// Environment variable carrying the parent session id into a spawned child.
///
/// The single wire-contract definition of the linkage every producer/consumer
/// shares: `ainb run --parent` seeds it, the lifecycle hook
/// (`ainb fleet atc hook`) reads it as the primary fleet-membership signal, and
/// the hangar daemon stamps it onto every task it spawns so a daemon-owned
/// session is a legitimate fleet member (its `AskUserQuestion` / lifecycle events
/// reach the attention pipeline). `ainb-core` re-exports this as
/// `crate::fleet::plumbing::PARENT_ENV`, so its many existing references are
/// unchanged.
pub const PARENT_ENV: &str = "AINB_PARENT_SESSION";

/// One session to register, matching the persisted `SessionMetadata` shape.
///
/// Only the REQUIRED fields the CLI's `SessionStore` reads are modelled; the
/// `#[serde(default)]` fields `SessionMetadata` carries (`agent_type`,
/// `headroom_enabled`, `rtk_enabled`) are intentionally omitted and default on
/// read-back.
#[derive(Debug, Clone, Serialize)]
pub struct AinbSessionRecord {
    /// A fresh session identity (the daemon's task ids are ULIDs, not the UUIDs
    /// the CLI store keys metadata by, so a new v4 UUID is minted per registration).
    pub session_id: Uuid,
    /// The exact tmux session name — the store's map key and the handle
    /// `ainb list` / discover surface for attach + targeting.
    pub tmux_session_name: String,
    /// The session's working directory (the task's isolated workdir).
    pub worktree_path: PathBuf,
    /// The owning workspace's slug (shown by `ainb list`, carried into discover's
    /// `Session.workspace_name`).
    pub workspace_name: String,
    /// Registration time, so the store sorts newest-first like `ainb run` entries.
    pub created_at: DateTime<Utc>,
}

impl AinbSessionRecord {
    /// Build a record for `tmux_session_name` in `worktree_path` under
    /// `workspace_name`, minting a fresh session id + `created_at = now`.
    #[must_use]
    pub fn new(
        tmux_session_name: impl Into<String>,
        worktree_path: PathBuf,
        workspace_name: impl Into<String>,
    ) -> Self {
        Self {
            session_id: Uuid::new_v4(),
            tmux_session_name: tmux_session_name.into(),
            worktree_path,
            workspace_name: workspace_name.into(),
            created_at: Utc::now(),
        }
    }
}

/// The `sessions.json` path: `$AINB_HOME` (else the home dir, else `.`) +
/// `.agents-in-a-box/sessions.json`.
///
/// Byte-for-byte the resolution `SessionStore::storage_path` uses — including the
/// `.` fallback when no home directory resolves — so the daemon writes exactly the
/// file the CLI reads in every environment.
#[must_use]
pub fn sessions_json_path() -> PathBuf {
    let base = std::env::var_os("AINB_HOME").map_or_else(
        || dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")),
        PathBuf::from,
    );
    base.join(".agents-in-a-box").join("sessions.json")
}

/// Register `record` in the session store so `ainb list` (and thus fleet
/// discover / standup / broadcast) includes it.
///
/// # Errors
///
/// Returns an error if the home directory cannot be resolved or the store cannot
/// be locked / written. Callers on a spawn hot path treat this as best-effort —
/// a registry write fault must never fail an already-live task.
pub fn register_session(record: &AinbSessionRecord) -> Result<()> {
    register_session_at(&sessions_json_path(), record)
}

/// [`register_session`] against an explicit store path (the test seam).
///
/// Upserts by tmux session name under an advisory lock, preserving every OTHER
/// entry verbatim: foreign entries are carried through as opaque JSON, so a
/// `SessionMetadata` field the daemon does not model is never dropped. The lock
/// serialises concurrent daemon dispatches so two interactive spawns cannot
/// lost-update each other.
///
/// # Errors
///
/// Returns an error if the parent dir cannot be created, the lock cannot be
/// acquired, or the atomic write fails.
pub fn register_session_at(path: &Path, record: &AinbSessionRecord) -> Result<()> {
    let dir = path.parent().context("sessions.json has no parent directory")?;
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;

    let _guard = lock_store(dir)?;

    // Read-merge-write under the lock: load the existing store as opaque JSON so
    // foreign entries survive, replace only our tmux-named key, write atomically.
    let mut store: serde_json::Value = std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| serde_json::json!({ "sessions": {} }));
    if !store.get("sessions").is_some_and(serde_json::Value::is_object) {
        store = serde_json::json!({ "sessions": {} });
    }
    let entry = serde_json::to_value(record).context("serializing session record")?;
    store["sessions"][&record.tmux_session_name] = entry;

    write_atomic(path, &store)
}

/// Acquire an exclusive advisory lock guarding the sessions.json
/// read-modify-write window (mirrors the `parents.json` lock pattern).
fn lock_store(dir: &Path) -> Result<std::fs::File> {
    let f = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join("sessions.json.lock"))
        .context("opening sessions.json lock file")?;
    f.lock_exclusive().context("acquiring sessions.json lock")?;
    Ok(f)
}

/// Serialize `value` and write it to `path` atomically (tmp + rename), so a crash
/// mid-write can never truncate the store and lose every tracked session — the
/// same durability `SessionStore::save` guarantees.
fn write_atomic(path: &Path, value: &serde_json::Value) -> Result<()> {
    let body = serde_json::to_string_pretty(value).context("serializing sessions.json")?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, body).with_context(|| format!("writing {}", tmp.display()))?;
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e).with_context(|| format!("renaming into {}", path.display()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn read(path: &Path) -> serde_json::Value {
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    #[test]
    fn register_writes_a_keyed_entry() {
        let home = TempDir::new().unwrap();
        let path = home.path().join("sessions.json");
        let rec = AinbSessionRecord::new("tmux_hangar-abc", PathBuf::from("/work/abc"), "proj");
        register_session_at(&path, &rec).unwrap();

        let store = read(&path);
        let entry = &store["sessions"]["tmux_hangar-abc"];
        assert_eq!(entry["tmux_session_name"], "tmux_hangar-abc");
        assert_eq!(entry["worktree_path"], "/work/abc");
        assert_eq!(entry["workspace_name"], "proj");
        assert_eq!(entry["session_id"], rec.session_id.to_string());
    }

    #[test]
    fn register_preserves_foreign_entries() {
        // An entry written by `ainb run` (with the optional fields the daemon
        // omits) must survive a daemon registration of a DIFFERENT session.
        let home = TempDir::new().unwrap();
        let path = home.path().join("sessions.json");
        std::fs::write(
            &path,
            r#"{"sessions":{"tmux_other":{"session_id":"11111111-1111-1111-1111-111111111111","tmux_session_name":"tmux_other","worktree_path":"/work/other","workspace_name":"other","created_at":"2026-01-01T00:00:00Z","agent_type":"Codex","headroom_enabled":true,"rtk_enabled":false}}}"#,
        )
        .unwrap();

        let rec = AinbSessionRecord::new("tmux_hangar-new", PathBuf::from("/work/new"), "proj");
        register_session_at(&path, &rec).unwrap();

        let store = read(&path);
        // Foreign entry (and its daemon-unmodelled fields) untouched.
        assert_eq!(store["sessions"]["tmux_other"]["agent_type"], "Codex");
        assert_eq!(store["sessions"]["tmux_other"]["headroom_enabled"], true);
        // Our entry landed alongside it.
        assert_eq!(store["sessions"]["tmux_hangar-new"]["workspace_name"], "proj");
    }

    #[test]
    fn register_upserts_by_tmux_name() {
        let home = TempDir::new().unwrap();
        let path = home.path().join("sessions.json");
        let a = AinbSessionRecord::new("tmux_hangar-x", PathBuf::from("/work/a"), "wsA");
        let b = AinbSessionRecord::new("tmux_hangar-x", PathBuf::from("/work/b"), "wsB");
        register_session_at(&path, &a).unwrap();
        register_session_at(&path, &b).unwrap();

        let store = read(&path);
        assert_eq!(store["sessions"].as_object().unwrap().len(), 1, "same key upserts");
        assert_eq!(store["sessions"]["tmux_hangar-x"]["workspace_name"], "wsB");
    }
}
