//! Cross-process locks for config and adjacent runtime state files.

use std::cell::RefCell;
use std::collections::HashSet;
use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

use fs2::FileExt;

thread_local! {
    /// `flock` is not re-entrant for the same file description. Catch a same-
    /// thread nesting bug before it turns a settings save into a deadlock.
    static HELD_PATHS: RefCell<HashSet<PathBuf>> = RefCell::new(HashSet::new());
}

/// Cross-process lock for read-modify-write of a file.
///
/// Holds a blocking exclusive flock on `<file>.lock`, released on drop.
pub(crate) struct ConfigLock {
    _file: File,
    path: PathBuf,
}

impl ConfigLock {
    /// Whether this guard protects `path`. Only debug assertions use this: the
    /// filesystem lock remains the source of truth across processes.
    pub(crate) fn guards(&self, path: &Path) -> bool {
        self.path == path
    }
}

impl Drop for ConfigLock {
    fn drop(&mut self) {
        HELD_PATHS.with(|held| {
            held.borrow_mut().remove(&self.path);
        });
    }
}

/// Take a blocking exclusive lock on `<path>.lock`.
///
/// Callers create `path`'s parent directory first. The lock filename appends
/// `.lock` rather than replacing the extension, so every writer of
/// `config.toml` uses the byte-identical `config.toml.lock` path.
pub(crate) fn lock_for(path: &Path) -> io::Result<ConfigLock> {
    let path = path.to_path_buf();
    HELD_PATHS.with(|held| {
        debug_assert!(
            !held.borrow().contains(&path),
            "attempted to acquire a non-reentrant config lock for {}",
            path.display()
        );
    });

    let lock_path = PathBuf::from(format!("{}.lock", path.display()));
    let file = OpenOptions::new().create(true).write(true).open(lock_path)?;
    file.lock_exclusive()?;
    HELD_PATHS.with(|held| {
        held.borrow_mut().insert(path.clone());
    });
    Ok(ConfigLock { _file: file, path })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn second_holder_waits_for_first_holder_to_drop() {
        let dir = tempfile::tempdir().expect("temporary config dir");
        let path = dir.path().join("config.toml");
        let first = lock_for(&path).expect("first config lock");
        let (entered_tx, entered_rx) = mpsc::channel();
        let (acquired_tx, acquired_rx) = mpsc::channel();
        let wait_path = path.clone();
        let worker = std::thread::spawn(move || {
            entered_tx.send(()).expect("announce second holder");
            acquired_tx
                .send(lock_for(&wait_path).expect("second config lock"))
                .expect("return second holder");
        });

        entered_rx.recv_timeout(Duration::from_secs(1)).expect("second holder started");
        assert!(
            acquired_rx.recv_timeout(Duration::from_millis(100)).is_err(),
            "second holder must wait while the first holder remains live"
        );
        drop(first);
        let _second = acquired_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("second holder acquires after first drops");
        worker.join().expect("second holder thread");
    }
}
