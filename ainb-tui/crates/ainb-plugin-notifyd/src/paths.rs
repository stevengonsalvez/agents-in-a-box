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
    /// The PID file written by the daemon at startup.
    pub pid: PathBuf,
    /// The fallback JSONL file that the hook script writes to when
    /// the daemon is unreachable. The daemon ingests + truncates this
    /// file on startup.
    pub fallback: PathBuf,
    /// Lock file for the lazy-spawn race between concurrent first
    /// hook fires. The daemon never reads it; we expose it here so
    /// tests and the install verb can clean it up.
    pub spawn_lock: PathBuf,
    /// Log file written by the daemon when running in the background.
    pub log: PathBuf,
}

impl Paths {
    /// Resolve paths under the user's home directory
    /// (`~/.agents-in-a-box/`). Fails if the home directory cannot be
    /// determined.
    pub fn from_home() -> anyhow::Result<Self> {
        let home = dirs::home_dir()
            .ok_or_else(|| anyhow::anyhow!("could not resolve home directory"))?;
        Ok(Self::under(home.join(".agents-in-a-box")))
    }

    /// Resolve paths under an arbitrary base directory. Used by tests
    /// to keep everything inside a `tempfile::TempDir`.
    pub fn under(base: impl AsRef<Path>) -> Self {
        let base = base.as_ref().to_path_buf();
        Self {
            db: base.join("notifications.db"),
            socket: base.join("notify.sock"),
            pid: base.join("notify.pid"),
            fallback: base.join("notify.fallback.jsonl"),
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
        assert!(p.pid.starts_with(dir.path()));
        assert!(p.fallback.starts_with(dir.path()));
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
