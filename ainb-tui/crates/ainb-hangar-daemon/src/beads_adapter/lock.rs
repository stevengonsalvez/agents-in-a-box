//! Cross-process `O_EXCL` pidfile lock for serialising `bd` writes (P2.2).
//!
//! `bd` mutates a git-backed store; two concurrent writes against the same
//! `BEADS_DIR` can race on the git head. Cargo runs integration test binaries in
//! parallel processes, so an in-process `Mutex` is not enough — serialization
//! must span processes. [`BdLock`] uses an exclusive-create pidfile
//! (`O_CREAT | O_EXCL`) as the token: whoever creates the file holds the lock;
//! everyone else spins until it disappears. A crashed holder leaves a stale
//! file, so a contender whose PID is no longer alive (`kill(pid, 0)` →
//! `ESRCH`) steals the lock by removing it. Mirrors the project's
//! `reference_cross_process_test_serialization` pattern.

use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::BdError;

/// How long to spin for the lock before giving up with [`BdError::LockTimeout`].
const ACQUIRE_TIMEOUT: Duration = Duration::from_secs(30);
/// Polling interval while the lock is held by another process.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// A reusable handle that mints one [`BdLockGuard`] per [`acquire`](Self::acquire).
///
/// Cheap to clone-by-construction: it only stores the pidfile path. The actual
/// exclusive file is created on `acquire` and removed when the guard drops.
#[derive(Debug, Clone)]
pub struct BdLock {
    path: PathBuf,
}

impl BdLock {
    /// Bind a lock to the pidfile `path` (created lazily on first acquire).
    #[must_use]
    pub const fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// Block until the exclusive pidfile is held, returning an RAII guard.
    ///
    /// Steals the lock from a holder whose PID is no longer alive. Times out
    /// after [`ACQUIRE_TIMEOUT`].
    ///
    /// # Errors
    ///
    /// [`BdError::LockTimeout`] if the lock stays held past the deadline,
    /// [`BdError::Spawn`] if the pidfile's parent directory cannot be created.
    pub fn acquire(&self) -> Result<BdLockGuard, BdError> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(BdError::Spawn)?;
        }
        let deadline = Instant::now() + ACQUIRE_TIMEOUT;
        loop {
            match OpenOptions::new().write(true).create_new(true).mode(0o600).open(&self.path) {
                Ok(mut f) => {
                    let _ = write!(f, "{}", std::process::id());
                    return Ok(BdLockGuard {
                        path: self.path.clone(),
                    });
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    if self.steal_if_stale() {
                        continue;
                    }
                    if Instant::now() >= deadline {
                        return Err(BdError::LockTimeout(self.path.clone()));
                    }
                    std::thread::sleep(POLL_INTERVAL);
                }
                Err(e) => return Err(BdError::Spawn(e)),
            }
        }
    }

    /// Remove the pidfile if it names a process that is no longer alive.
    ///
    /// Returns `true` when a stale file was removed (caller should retry the
    /// exclusive create immediately).
    fn steal_if_stale(&self) -> bool {
        let Some(pid) = read_pid(&self.path) else {
            // Unreadable/empty pidfile from a half-written holder: treat as stale.
            return std::fs::remove_file(&self.path).is_ok();
        };
        if pid_alive(pid) {
            return false;
        }
        std::fs::remove_file(&self.path).is_ok()
    }
}

/// RAII guard that removes the pidfile on drop, releasing the lock.
#[derive(Debug)]
pub struct BdLockGuard {
    path: PathBuf,
}

impl Drop for BdLockGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Read the PID written into the pidfile, or `None` if absent/unparseable.
fn read_pid(path: &Path) -> Option<i32> {
    let mut f = std::fs::File::open(path).ok()?;
    let mut buf = String::new();
    f.read_to_string(&mut buf).ok()?;
    buf.trim().parse().ok()
}

/// Is `pid` backed by a live process? Uses `kill(pid, 0)`: success or `EPERM`
/// (exists, not ours) → alive; `ESRCH` → dead.
fn pid_alive(pid: i32) -> bool {
    use nix::errno::Errno;
    use nix::sys::signal::kill;
    use nix::unistd::Pid;
    // `Ok` => exists and signalable; `EPERM` => exists but owned by another user.
    matches!(kill(Pid::from_raw(pid), None), Ok(()) | Err(Errno::EPERM))
}
