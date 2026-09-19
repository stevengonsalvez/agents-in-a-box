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
/// Carries the required fields plus what the daemon knows about the launch
/// (`agent_type`, `skip_permissions`, `model`, `model_source`). The
/// `#[serde(default)]` fields it cannot know (`headroom_enabled`,
/// `rtk_enabled`, the Codex thread) are omitted and default on read-back; the
/// daemon enables neither.
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
    /// The `SessionAgentType` variant name of the provider running in the
    /// pane (`Claude`, `Codex`, `Copilot`, `Antigravity`). Written so a
    /// daemon-spawned Codex session never reads back as the Claude default.
    pub agent_type: String,
    /// Whether the provider was launched with its permission prompts
    /// bypassed. `None` means unknown, which the reader treats as legacy.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skip_permissions: Option<bool>,
    /// The model the provider was launched with, verbatim; `None` means the
    /// provider default.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// `Raw` when `model` is a provider model id passed through unchanged,
    /// `LegacyTyped` otherwise (the `ModelSource` variant names).
    pub model_source: String,
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
            agent_type: "Claude".to_string(),
            skip_permissions: None,
            model: None,
            model_source: "LegacyTyped".to_string(),
        }
    }

    /// Record what was actually launched: the provider, whether its
    /// permission prompts are bypassed, and the model override. A `Some`
    /// model is a raw provider id, so `model_source` becomes `Raw`.
    #[must_use]
    pub fn with_launch(
        mut self,
        agent_type: impl Into<String>,
        skip_permissions: Option<bool>,
        model: Option<String>,
    ) -> Self {
        self.agent_type = agent_type.into();
        self.skip_permissions = skip_permissions;
        self.model_source = if model.is_some() {
            "Raw"
        } else {
            "LegacyTyped"
        }
        .to_string();
        self.model = model;
        self
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

    let guard = lock_store(dir)?;
    register_session_locked(path, record, &guard)
}

/// [`register_session_at`] for a caller that already holds the `sessions.json`
/// flock (from [`lock_sessions_store_at`] or [`try_lock_sessions_store_at`]).
///
/// P6e: the daemon's registration writes the file row and then the `sessions`
/// table under ONE flock, so a surface's read-modify-write cannot interleave
/// between the two. `_lock` is that held guard; it is taken by reference so the
/// caller keeps the lock until its second write is done. Taking the flock again
/// here would block forever: `flock` on a second descriptor conflicts even
/// within one process.
///
/// # Errors
///
/// Returns an error if the atomic write fails.
pub fn register_session_locked(
    path: &Path,
    record: &AinbSessionRecord,
    _lock: &std::fs::File,
) -> Result<()> {
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
    let f = open_lock_file(dir)?;
    f.lock_exclusive().context("acquiring sessions.json lock")?;
    Ok(f)
}

/// Open (creating if needed) the `sessions.json.lock` file in `dir`.
fn open_lock_file(dir: &Path) -> Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join("sessions.json.lock"))
        .context("opening sessions.json lock file")
}

/// Acquire the exclusive advisory lock guarding `sessions.json` in `dir`,
/// creating `dir` (and the lock file) if needed.
///
/// This is the SAME lock [`register_session_at`] takes internally, exposed so
/// the other writer of `~/.agents-in-a-box/sessions.json` — `ainb-core`'s
/// `SessionStore` load-mutate-save lifecycle sites — can serialise against the
/// daemon's registrations and against each other. Hold the returned handle
/// across the whole read → mutate → write window: the lock is released when the
/// handle drops, so a concurrent writer cannot lost-update the file.
///
/// The returned `File` is the guard; it carries no other meaning (the lock file
/// is never read or written, only `flock`ed). This is a blocking, cross-process
/// advisory lock — it excludes other processes' `ainb run` / `ainb kill` /
/// recovery mutations, not just threads.
///
/// # Errors
///
/// Returns an error if `dir` cannot be created or the lock cannot be acquired.
pub fn lock_sessions_store_at(dir: &Path) -> Result<std::fs::File> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    lock_store(dir)
}

/// Try once to take the `sessions.json` flock in `dir`, without blocking.
///
/// `Ok(None)` means another descriptor holds it. This is the primitive a
/// bounded wait is built on: a caller that must not hang (the daemon's run
/// loop, an RPC deadline) retries it on its own schedule instead of calling
/// the blocking [`lock_sessions_store_at`].
///
/// # Errors
///
/// Returns an error if `dir` cannot be created, the lock file cannot be
/// opened, or `flock` fails for a reason other than contention.
pub fn try_lock_sessions_store_at(dir: &Path) -> Result<Option<std::fs::File>> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let f = open_lock_file(dir)?;
    match f.try_lock_exclusive() {
        Ok(()) => Ok(Some(f)),
        Err(e) if e.kind() == fs2::lock_contended_error().kind() => Ok(None),
        Err(e) => Err(e).context("acquiring sessions.json lock"),
    }
}

/// [`lock_sessions_store_at`] against the default [`sessions_json_path`]
/// directory (`$AINB_HOME`/`~/.agents-in-a-box`).
///
/// # Errors
///
/// Returns an error if the parent directory cannot be resolved/created or the
/// lock cannot be acquired.
pub fn lock_sessions_store() -> Result<std::fs::File> {
    let path = sessions_json_path();
    let dir = path.parent().context("sessions.json has no parent directory")?;
    lock_sessions_store_at(dir)
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
    fn register_writes_what_was_launched() {
        let home = TempDir::new().unwrap();
        let path = home.path().join("sessions.json");
        let rec = AinbSessionRecord::new("tmux_hangar-cx", PathBuf::from("/work/cx"), "proj")
            .with_launch("Codex", Some(true), Some("gpt-5-codex".to_string()));
        register_session_at(&path, &rec).unwrap();

        let entry = &read(&path)["sessions"]["tmux_hangar-cx"];
        assert_eq!(entry["agent_type"], "Codex");
        assert_eq!(entry["skip_permissions"], true);
        assert_eq!(entry["model"], "gpt-5-codex");
        assert_eq!(entry["model_source"], "Raw");
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
        assert_eq!(
            store["sessions"]["tmux_hangar-new"]["workspace_name"],
            "proj"
        );
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
        assert_eq!(
            store["sessions"].as_object().unwrap().len(),
            1,
            "same key upserts"
        );
        assert_eq!(store["sessions"]["tmux_hangar-x"]["workspace_name"], "wsB");
    }

    /// pu4: the advisory lock must serialise concurrent read-modify-write so
    /// two writers racing on the same `sessions.json` never lose an update.
    /// Each thread does a full load-merge-write of a DISTINCT key; without the
    /// lock the classic interleaving (both read the same base, both write back
    /// their own single entry) drops one. With the lock every write sees the
    /// prior writer's entry, so all N survive. This is the exact primitive the
    /// `ainb-core` `SessionStore::mutate` sites now hold across their RMW.
    #[test]
    fn concurrent_registrations_do_not_lose_updates() {
        use std::sync::{Arc, Barrier};

        let home = TempDir::new().unwrap();
        let path = Arc::new(home.path().join("sessions.json"));

        // Seed a pre-existing entry so we also prove foreign preservation under
        // contention (it must survive every racing writer).
        let seed = AinbSessionRecord::new("tmux_seed", PathBuf::from("/work/seed"), "seedws");
        register_session_at(&path, &seed).unwrap();

        const WRITERS: usize = 16;
        let barrier = Arc::new(Barrier::new(WRITERS));
        let handles: Vec<_> = (0..WRITERS)
            .map(|i| {
                let path = Arc::clone(&path);
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    // Release all writers at once to maximise the RMW overlap.
                    barrier.wait();
                    let rec = AinbSessionRecord::new(
                        format!("tmux_w{i}"),
                        PathBuf::from(format!("/work/{i}")),
                        format!("ws{i}"),
                    );
                    register_session_at(&path, &rec).unwrap();
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }

        let store = read(&path);
        let sessions = store["sessions"].as_object().unwrap();
        // Seed + every one of the WRITERS entries present: no lost update.
        assert_eq!(
            sessions.len(),
            WRITERS + 1,
            "all concurrent writers plus the seed must survive"
        );
        assert_eq!(sessions["tmux_seed"]["workspace_name"], "seedws");
        for i in 0..WRITERS {
            assert_eq!(
                sessions[&format!("tmux_w{i}")]["workspace_name"],
                format!("ws{i}"),
                "writer {i}'s entry was lost to a racing write"
            );
        }
    }

    /// The try-lock reports contention instead of blocking, and succeeds once
    /// the holder drops its guard.
    #[test]
    fn try_lock_reports_a_held_flock() {
        let home = TempDir::new().unwrap();
        let held = lock_sessions_store_at(home.path()).unwrap();
        assert!(try_lock_sessions_store_at(home.path()).unwrap().is_none());
        drop(held);
        assert!(try_lock_sessions_store_at(home.path()).unwrap().is_some());
    }

    /// The locked variant writes under the caller's flock, byte for byte what
    /// the self-locking form writes.
    #[test]
    fn register_under_a_held_lock_matches_the_self_locking_write() {
        let a = TempDir::new().unwrap();
        let b = TempDir::new().unwrap();
        let rec = AinbSessionRecord::new("tmux_held", PathBuf::from("/work/held"), "proj");

        register_session_at(&a.path().join("sessions.json"), &rec).unwrap();
        let guard = try_lock_sessions_store_at(b.path()).unwrap().expect("free lock");
        register_session_locked(&b.path().join("sessions.json"), &rec, &guard).unwrap();

        assert_eq!(
            std::fs::read(a.path().join("sessions.json")).unwrap(),
            std::fs::read(b.path().join("sessions.json")).unwrap()
        );
    }
}
