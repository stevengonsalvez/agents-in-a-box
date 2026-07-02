//! All filesystem paths owned by `ainb-notifyd`.
//!
//! Centralising path resolution here means tests can swap the base
//! directory (via [`Paths::under`]) without monkey-patching `$HOME`,
//! and there is exactly one place to look when the daemon's on-disk
//! layout has to change.

use std::path::{Path, PathBuf};

/// Resolved layout of every file the daemon reads or writes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    /// The base directory; everything below lives inside this.
    pub base: PathBuf,
    /// The SQLite database backing the notification store.
    pub db: PathBuf,
    /// The Unix domain socket used by the hook script to deliver
    /// envelopes.
    pub socket: PathBuf,
    /// The Unix domain socket the approve/deny broker listens on. Unlike
    /// [`Paths::socket`] (one-way fire-and-forget), this is a
    /// request/response socket: a waiting Claude `PermissionRequest` hook
    /// dials it and BLOCKS (`AWAIT`) until a human issues an approve/deny
    /// (`DECIDE`) from the fleet TUI or CLI, or the broker times out
    /// (fallback: deny). The `LIST` op enumerates pending approvals.
    pub approve_socket: PathBuf,
    /// The PID file written by the daemon at startup.
    pub pid: PathBuf,
    /// The fallback JSONL file that the hook script writes to when
    /// the daemon is unreachable. The daemon ingests + truncates this
    /// file on startup.
    pub fallback: PathBuf,
    /// The durable append-only event log the lifecycle hook appends one
    /// canonical line to per managed event. The daemon's ingest tailer
    /// folds it into the `events` table from a persisted byte offset
    /// (crash-safe catch-up); unlike `fallback`, this file is never
    /// truncated — the offset is the cursor.
    pub events_jsonl: PathBuf,
    /// Lock file for the lazy-spawn race between concurrent first
    /// hook fires. The daemon never reads it; we expose it here so
    /// tests and the install verb can clean it up.
    pub spawn_lock: PathBuf,
    /// Log file written by the daemon when running in the background.
    pub log: PathBuf,
}

impl Paths {
    /// Resolve paths under the ainb base directory: `$AINB_HOME` when set
    /// (the same override the fleet plumbing honours — the hook, the daemon,
    /// and the TUI/CLI deciders MUST all resolve the approve socket to the
    /// same base or a waiting hook parks on a socket nothing serves),
    /// otherwise `~/.agents-in-a-box/`. Fails if neither can be determined.
    pub fn from_home() -> anyhow::Result<Self> {
        if let Ok(h) = std::env::var("AINB_HOME") {
            if !h.is_empty() {
                return Ok(Self::under(PathBuf::from(h)));
            }
        }
        let home =
            dirs::home_dir().ok_or_else(|| anyhow::anyhow!("could not resolve home directory"))?;
        Ok(Self::under(home.join(".agents-in-a-box")))
    }

    /// Resolve paths under an arbitrary base directory. Used by tests
    /// to keep everything inside a `tempfile::TempDir`.
    pub fn under(base: impl AsRef<Path>) -> Self {
        let base = base.as_ref().to_path_buf();
        Self {
            db: base.join("notifications.db"),
            socket: base.join("notify.sock"),
            approve_socket: base.join("approve.sock"),
            pid: base.join("notify.pid"),
            fallback: base.join("notify.fallback.jsonl"),
            events_jsonl: base.join("events.jsonl"),
            spawn_lock: base.join("notify.spawn.lock"),
            log: base.join("notify.log"),
            base,
        }
    }

    /// Create the base directory and any missing parents.
    pub fn ensure_base(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.base)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn under_lays_out_every_file_inside_base() {
        let dir = TempDir::new().unwrap();
        let p = Paths::under(dir.path());
        assert_eq!(p.base, dir.path());
        assert!(p.db.starts_with(dir.path()));
        assert!(p.socket.starts_with(dir.path()));
        assert!(p.approve_socket.starts_with(dir.path()));
        assert!(p.approve_socket.ends_with("approve.sock"));
        assert!(p.pid.starts_with(dir.path()));
        assert!(p.fallback.starts_with(dir.path()));
        assert!(p.events_jsonl.starts_with(dir.path()));
        assert!(p.events_jsonl.ends_with("events.jsonl"));
        assert!(p.spawn_lock.starts_with(dir.path()));
        assert!(p.log.starts_with(dir.path()));
    }

    #[test]
    fn ensure_base_creates_missing_parents() {
        let dir = TempDir::new().unwrap();
        let nested = dir.path().join("a").join("b").join("c");
        let p = Paths::under(&nested);
        assert!(!nested.exists());
        p.ensure_base().unwrap();
        assert!(nested.exists());
    }
}
