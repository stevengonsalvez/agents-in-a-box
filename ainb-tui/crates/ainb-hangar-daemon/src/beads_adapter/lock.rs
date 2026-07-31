//! Cross-process pidfile lock for serialising `bd` writes (P2.2).
//!
//! `bd` mutates a git-backed store; two concurrent writes against the same
//! `BEADS_DIR` can race on the git head. Cargo runs integration test binaries in
//! parallel processes, so an in-process `Mutex` is not enough — serialization
//! must span processes. [`BdLock`] uses a pidfile as the token: whoever
//! publishes the file holds the lock; everyone else spins until it disappears.
//! A crashed holder leaves a stale file, so a contender whose PID is no longer
//! alive (`kill(pid, 0)` → `ESRCH`) steals the lock by removing it. Mirrors the
//! project's `reference_cross_process_test_serialization` pattern.
//!
//! **Publication is atomic, and must stay that way.** The pidfile is written to
//! a private temp file first and only then linked into place with `hard_link`,
//! which is atomic and fails with `EEXIST` while the lock is held. The naive
//! `O_CREAT | O_EXCL` + `write!` sequence is *not* safe here: between the create
//! and the write the pidfile exists but is empty, and an empty pidfile is
//! indistinguishable from the one a crashed holder leaves behind. A contender
//! sampling that window takes the crash-recovery path, deletes a *live* holder's
//! pidfile and acquires alongside it — two holders, two concurrent `bd`
//! processes, the git-head race the lock exists to prevent.
//!
//! **An absent pidfile is not stale, it is free.** Atomic publication closed the
//! empty-file window but not the second route to the same double-hold: the
//! crash-recovery path decided "stale" from a *failed read*, then unlinked the
//! path. Absent reads that way too, and between that read and the unlink another
//! contender can publish — so the unlink deletes a brand-new live holder's
//! pidfile and the stealer acquires alongside it. Removal must therefore be
//! driven by what the file SAYS (a dead PID, or unusable content), never by the
//! read merely failing. [`PidfileRead`] keeps the two apart.

use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
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
            match self.try_publish_pidfile() {
                Ok(true) => {
                    return Ok(BdLockGuard {
                        path: self.path.clone(),
                    });
                }
                Ok(false) => {
                    // The deadline bounds EVERY path, including the immediate
                    // retry. `steal_if_stale` now reports "free, retry now" for
                    // an absent pidfile, which under contention is common rather
                    // than rare — leaving that branch to `continue` unchecked
                    // would let a contender spin without ever timing out.
                    if Instant::now() >= deadline {
                        return Err(BdError::LockTimeout(self.path.clone()));
                    }
                    if self.steal_if_stale() {
                        continue;
                    }
                    std::thread::sleep(POLL_INTERVAL);
                }
                Err(e) => return Err(BdError::Spawn(e)),
            }
        }
    }

    /// Publish a fully-written pidfile atomically; `Ok(false)` means held.
    ///
    /// Writes the PID into a private temp file in the same directory, then
    /// `hard_link`s it onto the lock path. The link is the atomic step and
    /// returns `EEXIST` while another holder is in place, so the pidfile is
    /// never observable in a created-but-empty state (see the module doc).
    fn try_publish_pidfile(&self) -> std::io::Result<bool> {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let name = self.path.file_name().and_then(std::ffi::OsStr::to_str).unwrap_or("bd");
        let tmp = self.path.with_file_name(format!(
            ".{name}.{}.{}.tmp",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        {
            let mut f = OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp)?;
            write!(f, "{}", std::process::id())?;
            f.sync_all()?;
        }
        let linked = std::fs::hard_link(&tmp, &self.path);
        let _ = std::fs::remove_file(&tmp);
        match linked {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// Remove the pidfile if it names a process that is no longer alive.
    ///
    /// Returns `true` when a stale file was removed (caller should retry the
    /// exclusive create immediately).
    fn steal_if_stale(&self) -> bool {
        let pid = match read_pid(&self.path) {
            Ok(pid) => pid,
            // The pidfile vanished between our failed publish and this read: the
            // previous holder released. There is nothing to steal, and removing
            // is actively WRONG — by the time the unlink lands a new holder may
            // have published, and we would delete a live holder's pidfile and
            // acquire alongside it. That is the very double-hold atomic
            // publication was introduced to kill, reachable through this branch
            // instead of through the create-then-write window. Just retry.
            Err(PidfileRead::Absent) => return true,
            // The file exists but says nothing usable. Publication is atomic, so
            // a live holder cannot produce this — it is a pre-fix leftover or a
            // truncated file. Genuine crash-recovery territory.
            Err(PidfileRead::Unreadable) => return std::fs::remove_file(&self.path).is_ok(),
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

/// Why a pidfile yielded no PID.
///
/// The two cases demand OPPOSITE handling, which is why they are not one
/// `Option`: an absent file must never be removed (a new holder may already own
/// that path), while an unreadable one is genuine crash debris.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PidfileRead {
    /// No file at the path — the holder released.
    Absent,
    /// A file is there but carries no usable PID.
    Unreadable,
}

/// Read the PID written into the pidfile.
fn read_pid(path: &Path) -> Result<i32, PidfileRead> {
    let mut f = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(PidfileRead::Absent),
        Err(_) => return Err(PidfileRead::Unreadable),
    };
    let mut buf = String::new();
    f.read_to_string(&mut buf).map_err(|_| PidfileRead::Unreadable)?;
    buf.trim().parse().map_err(|_| PidfileRead::Unreadable)
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::sync::mpsc;
    use std::sync::{Arc, Barrier};

    /// A PID that is guaranteed dead: spawn a trivial child and reap it.
    fn dead_pid() -> i32 {
        let mut child = std::process::Command::new("/usr/bin/true")
            .spawn()
            .or_else(|_| std::process::Command::new("/bin/true").spawn())
            .expect("spawn /bin/true");
        let pid = i32::try_from(child.id()).expect("pid fits i32");
        child.wait().expect("reap child");
        pid
    }

    /// The invariant the whole lock rests on: a visible pidfile always names its
    /// holder. Publication is a single atomic link, so there is no window in
    /// which the file exists but is empty.
    #[test]
    fn acquire_publishes_a_populated_pidfile() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let path = dir.path().join("bd.pid");
        let lock = BdLock::new(path.clone());
        let guard = lock.acquire().expect("acquire");
        assert_eq!(
            read_pid(&path),
            Ok(i32::try_from(std::process::id()).unwrap())
        );
        drop(guard);
        assert!(!path.exists(), "guard drop releases the lock");
        // No temp turds left behind by the publish step.
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .expect("read_dir")
            .map(|e| e.expect("entry").file_name())
            .collect();
        assert!(
            leftovers.is_empty(),
            "publish left files behind: {leftovers:?}"
        );
    }

    /// Regression for the create-then-write race: contenders starting at the same
    /// instant must never both hold the lock.
    ///
    /// Under the old exclusive-create-then-`write!` publish the loser's very next
    /// syscall sampled the pidfile while it was still empty, mistook a live holder
    /// for a crashed one, deleted its pidfile and acquired alongside it. Two knobs
    /// make that observable instead of luck: a [`Barrier`] so every round starts
    /// the contenders simultaneously (the window only exists at the instant of
    /// publication), and a short hold inside the critical section so an admitted
    /// second holder overlaps the first for long enough to be counted. Measured on
    /// the pre-fix publish this trips within a handful of rounds; the integration
    /// sibling `test_concurrent_invocations_serialize_per_beads_dir` failed 10/40
    /// runs from the same defect.
    #[test]
    fn contended_acquire_never_admits_two_holders() {
        const THREADS: usize = 4;
        const ROUNDS: usize = 60;
        const HOLD: Duration = Duration::from_millis(2);

        let dir = tempfile::tempdir().expect("tmpdir");
        let lock = BdLock::new(dir.path().join("bd.pid"));
        let holders = Arc::new(AtomicUsize::new(0));
        let overlaps = Arc::new(AtomicUsize::new(0));
        let entered_occupied = Arc::new(AtomicUsize::new(0));
        let joined_during_hold = Arc::new(AtomicUsize::new(0));
        let barrier = Arc::new(Barrier::new(THREADS));

        let handles: Vec<_> = (0..THREADS)
            .map(|_| {
                let lock = lock.clone();
                let holders = Arc::clone(&holders);
                let overlaps = Arc::clone(&overlaps);
                let entered_occupied = Arc::clone(&entered_occupied);
                let joined_during_hold = Arc::clone(&joined_during_hold);
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    for _ in 0..ROUNDS {
                        barrier.wait();
                        let guard = lock.acquire().expect("acquire");
                        let before = holders.fetch_add(1, Ordering::SeqCst);
                        std::thread::sleep(HOLD);
                        let during = holders.load(Ordering::SeqCst);
                        holders.fetch_sub(1, Ordering::SeqCst);
                        drop(guard);
                        if before != 0 || during != 1 {
                            overlaps.fetch_add(1, Ordering::SeqCst);
                            // Record WHICH invariant broke. `before != 0` means a
                            // holder was already counted when we acquired; `during
                            // != 1` means one joined while we held. They implicate
                            // different windows, and this test only fails on CI
                            // Linux, so a bare count is undiagnosable after the
                            // fact (see agents-in-a-box-hp8).
                            if before != 0 {
                                entered_occupied.fetch_add(1, Ordering::SeqCst);
                            }
                            if during != 1 {
                                joined_during_hold.fetch_add(1, Ordering::SeqCst);
                            }
                        }
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().expect("thread join");
        }
        assert_eq!(
            overlaps.load(Ordering::SeqCst),
            0,
            "the lock admitted concurrent holders \
             (entered-occupied={}, joined-during-hold={}, of {} acquisitions)",
            entered_occupied.load(Ordering::SeqCst),
            joined_during_hold.load(Ordering::SeqCst),
            THREADS * ROUNDS
        );
    }

    /// A pidfile that is simply GONE is not "stolen" — nothing is removed.
    ///
    /// Regression for the second double-hold window. `steal_if_stale` used to
    /// treat any unreadable pidfile, including an absent one, as crash debris
    /// and unlink the path. Between that failed read and the unlink another
    /// contender can publish, so the unlink deletes a LIVE holder's pidfile and
    /// the stealer acquires alongside it. Absent must mean "retry", never
    /// "remove".
    #[test]
    fn an_absent_pidfile_is_retried_not_removed() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let path = dir.path().join("bd.pid");
        let lock = BdLock::new(path.clone());

        assert_eq!(read_pid(&path), Err(PidfileRead::Absent));
        assert!(
            lock.steal_if_stale(),
            "an absent pidfile means the lock is free, so retry immediately"
        );

        // The decisive part: a holder that publishes between the failed read and
        // the steal decision must survive it.
        let guard = lock.acquire().expect("acquire");
        assert!(
            !lock.steal_if_stale(),
            "a live holder's pidfile must never be stolen"
        );
        assert!(path.exists(), "the live holder's pidfile was deleted");
        drop(guard);
    }

    /// A pidfile that exists but carries no usable PID is still crash debris.
    #[test]
    fn an_unreadable_pidfile_is_still_stolen() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let path = dir.path().join("bd.pid");
        std::fs::write(&path, "not-a-pid").expect("seed corrupt pidfile");

        assert_eq!(read_pid(&path), Err(PidfileRead::Unreadable));
        assert!(BdLock::new(path.clone()).steal_if_stale());
        assert!(!path.exists(), "corrupt pidfile should be removed");
    }

    /// Crash recovery still works: a pidfile naming a dead process is stolen
    /// without waiting out the acquire timeout.
    #[test]
    fn pidfile_of_a_dead_holder_is_stolen_promptly() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let path = dir.path().join("bd.pid");
        std::fs::write(&path, dead_pid().to_string()).expect("seed stale pidfile");

        let started = Instant::now();
        let guard = BdLock::new(path.clone()).acquire().expect("steal stale lock");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "steal was not prompt"
        );
        assert_eq!(
            read_pid(&path),
            Ok(i32::try_from(std::process::id()).unwrap())
        );
        drop(guard);
    }

    /// A live holder is never stolen from: the contender blocks until release.
    #[test]
    fn live_holder_blocks_a_contender_until_release() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let lock = BdLock::new(dir.path().join("bd.pid"));
        let held = lock.acquire().expect("first acquire");

        let (tx, rx) = mpsc::channel();
        let contender = std::thread::spawn(move || {
            let g = lock.acquire().expect("second acquire");
            tx.send(()).expect("signal acquired");
            drop(g);
        });

        assert!(
            rx.recv_timeout(Duration::from_millis(300)).is_err(),
            "contender acquired while the lock was held"
        );
        drop(held);
        rx.recv_timeout(Duration::from_secs(5))
            .expect("contender acquires after release");
        contender.join().expect("thread join");
    }
}
