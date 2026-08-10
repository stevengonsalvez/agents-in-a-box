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
//!
//! **Judging a file stale and reclaiming it must be ONE step.** Even reading a
//! genuinely dead PID is not enough to justify an unlink: two contenders can
//! sample the SAME stale pidfile, both conclude "crashed", and both remove it —
//! the first republishes, and the second's unlink deletes that brand-new live
//! holder's pidfile. Third route, same double-hold. [`BdLock::steal_atomically`]
//! therefore reclaims with `rename`, which is winner-take-all: only one caller
//! can move a given inode aside, and every loser simply retries.

use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use super::BdError;

/// How long to spin for the lock before giving up with [`BdError::LockTimeout`].
const ACQUIRE_TIMEOUT: Duration = Duration::from_secs(30);
/// How long [`BdLock::try_acquire_with`] spins before declaring the lock held.
///
/// Deliberately NOT zero. An ABSENT pidfile means "free, retry now" (module doc,
/// second double-hold route), and a publish can also lose a benign link race, so
/// a single attempt would report a free lock as held. This is short enough to
/// keep a single-instance guard fail-fast and long enough to ride out the churn.
pub const TRY_ACQUIRE_TIMEOUT: Duration = Duration::from_millis(500);
/// Polling interval while the lock is held by another process.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// Why an acquisition attempt did not take the lock.
#[derive(Debug)]
pub enum LockHeld {
    /// A holder the liveness predicate judged live owns the lock, named by pid.
    By(i32),
    /// The window elapsed with the path churning (repeated steals/publishes by
    /// other contenders) and no live holder to name.
    Contended,
    /// The pidfile's directory could not be created.
    Io(std::io::Error),
}

/// What to do about a pidfile we could not publish over.
///
/// Distinguishing [`Self::Now`] from [`Self::Backoff`] preserves the module's
/// third invariant: losing a `rename` steal is not evidence the lock is free, so
/// that path backs off instead of hot-spinning on a path someone else just won.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reclaim {
    /// The path is free right now — publish again immediately.
    Now,
    /// Someone else is in the way; wait a poll interval.
    Backoff,
    /// A live holder owns the lock.
    HeldBy(i32),
}

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
        self.acquire_within(ACQUIRE_TIMEOUT, pid_alive).map_err(|held| match held {
            LockHeld::Io(e) => BdError::Spawn(e),
            LockHeld::By(_) | LockHeld::Contended => BdError::LockTimeout(self.path.clone()),
        })
    }

    /// Take the lock or report who holds it, without a long spin.
    ///
    /// The fail-fast half of [`acquire`](Self::acquire), for a single-instance
    /// guard: a process that loses wants to exit now, not in 30 seconds.
    ///
    /// `holder_is_live` decides whether an existing holder must be respected.
    /// It MUST fail SAFE — return `true` whenever it cannot tell — because a
    /// false negative steals a LIVE holder's pidfile and admits two holders,
    /// the exact double-hold this module exists to prevent. `pid_alive` alone is
    /// the right predicate only when a recycled PID is impossible; a lock file
    /// that outlives a reboot needs an identity check layered on top.
    ///
    /// # Errors
    ///
    /// [`LockHeld::By`] when a live holder owns the lock, [`LockHeld::Contended`]
    /// when the window elapsed with the path churning, [`LockHeld::Io`] when the
    /// pidfile's directory cannot be created.
    pub fn try_acquire_with<F>(&self, holder_is_live: F) -> Result<BdLockGuard, LockHeld>
    where
        F: Fn(i32) -> bool,
    {
        self.acquire_within(TRY_ACQUIRE_TIMEOUT, holder_is_live)
    }

    /// Take the lock or report who holds it, spinning for `timeout` first.
    ///
    /// The escalation [`try_acquire_with`](Self::try_acquire_with) needs when it
    /// cannot name a holder: churn is not evidence that anyone won, so a caller
    /// that declined on it could leave nobody holding the lock at all.
    ///
    /// # Errors
    ///
    /// As [`try_acquire_with`](Self::try_acquire_with).
    pub fn acquire_within_with<F>(
        &self,
        timeout: Duration,
        holder_is_live: F,
    ) -> Result<BdLockGuard, LockHeld>
    where
        F: Fn(i32) -> bool,
    {
        self.acquire_within(timeout, holder_is_live)
    }

    /// Spin for the lock until `timeout` elapses, judging holders with
    /// `holder_is_live`. The one acquisition path; both public entry points are
    /// this function with a different deadline and predicate.
    fn acquire_within<F>(
        &self,
        timeout: Duration,
        holder_is_live: F,
    ) -> Result<BdLockGuard, LockHeld>
    where
        F: Fn(i32) -> bool,
    {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(LockHeld::Io)?;
        }
        let deadline = Instant::now() + timeout;
        // The last holder we judged live, so a timeout can name it instead of
        // reporting a bare "contended" a caller cannot act on.
        let mut last_holder = None;
        loop {
            match self.try_publish_pidfile() {
                Ok(true) => {
                    return Ok(BdLockGuard {
                        path: self.path.clone(),
                    });
                }
                Ok(false) => {
                    // The deadline bounds EVERY path, including the immediate
                    // retry. `reclaim` reports "free, retry now" for an absent
                    // pidfile, which under contention is common rather than
                    // rare — leaving that branch to `continue` unchecked would
                    // let a contender spin without ever timing out.
                    if Instant::now() >= deadline {
                        return Err(last_holder.map_or(LockHeld::Contended, LockHeld::By));
                    }
                    match self.reclaim(&holder_is_live) {
                        // Retry immediately: the path is free right now.
                        Reclaim::Now => {}
                        Reclaim::Backoff => std::thread::sleep(POLL_INTERVAL),
                        Reclaim::HeldBy(pid) => {
                            last_holder = Some(pid);
                            std::thread::sleep(POLL_INTERVAL);
                        }
                    }
                }
                Err(e) => return Err(LockHeld::Io(e)),
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

    /// Decide what to do about a pidfile we could not publish over, taking the
    /// stale one out of the way when `holder_is_live` says its holder is gone.
    fn reclaim<F>(&self, holder_is_live: F) -> Reclaim
    where
        F: Fn(i32) -> bool,
    {
        let pid = match read_pid(&self.path) {
            Ok(pid) => pid,
            // The pidfile vanished between our failed publish and this read: the
            // previous holder released. There is nothing to steal, and removing
            // is actively WRONG — by the time the unlink lands a new holder may
            // have published, and we would delete a live holder's pidfile and
            // acquire alongside it. That is the very double-hold atomic
            // publication was introduced to kill, reachable through this branch
            // instead of through the create-then-write window. Just retry.
            Err(PidfileRead::Absent) => return Reclaim::Now,
            // The file exists but says nothing usable. Publication is atomic, so
            // a live holder cannot produce this — it is a pre-fix leftover or a
            // truncated file. Genuine crash-recovery territory.
            Err(PidfileRead::Unreadable) => return self.reclaimed_or_backoff(),
        };
        if holder_is_live(pid) {
            return Reclaim::HeldBy(pid);
        }
        self.reclaimed_or_backoff()
    }

    /// Steal a pidfile we have judged stale, mapping the winner-take-all outcome
    /// onto the retry policy: we won the `rename` so the path is free now, or we
    /// lost it to another contender and must back off rather than hot-spin.
    fn reclaimed_or_backoff(&self) -> Reclaim {
        if self.steal_atomically() {
            Reclaim::Now
        } else {
            Reclaim::Backoff
        }
    }

    /// Take a pidfile we have judged stale OUT of the way, winner-take-all.
    ///
    /// A blind `remove_file` here is not safe, and this is the third route to
    /// the same double-hold the module doc describes. Two contenders can sample
    /// the SAME stale pidfile and both conclude it is dead: the first unlinks it
    /// and republishes, and the second's unlink then deletes that brand-new LIVE
    /// holder's pidfile, so both hold.
    ///
    /// `rename` collapses judge-and-remove into one atomic step. Only one caller
    /// can move a given inode aside; every loser gets `NotFound` and simply
    /// retries, by which point the winner's own pidfile is in place and is
    /// evaluated on its merits like any other holder.
    fn steal_atomically(&self) -> bool {
        static STEAL_SEQ: AtomicU64 = AtomicU64::new(0);
        let name = self.path.file_name().and_then(std::ffi::OsStr::to_str).unwrap_or("bd");
        let claimed = self.path.with_file_name(format!(
            ".{name}.steal.{}.{}",
            std::process::id(),
            STEAL_SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        if std::fs::rename(&self.path, &claimed).is_err() {
            // Lost the race, or it vanished on its own. Either way the path is
            // no longer ours to reclaim; retry the publish instead.
            return false;
        }
        let _ = std::fs::remove_file(&claimed);
        true
    }
}

/// RAII guard that removes the pidfile on drop, releasing the lock.
#[derive(Debug)]
pub struct BdLockGuard {
    path: PathBuf,
}

impl Drop for BdLockGuard {
    /// Release by compare-and-delete, never a blind unlink.
    ///
    /// A guard can outlive its own pidfile: a contender whose liveness predicate
    /// judged us dead reclaims the path and publishes its own. A blind unlink
    /// here would then delete THAT live holder's pidfile — the same double-hold
    /// the steal path is careful to avoid, reintroduced on the way out. Removing
    /// only while the file still names us keeps the invariant symmetric.
    ///
    /// This is a read-then-unlink, so a residual window remains: a successor
    /// could steal and republish between the read and the unlink, and we would
    /// delete its file. POSIX offers no compare-and-unlink to close it — the
    /// steal path's `rename` trick does not transfer, because renaming a file we
    /// intend to delete would displace whatever is at the path if it is no
    /// longer ours. Reaching it requires the predicate to first misjudge a LIVE
    /// holder as dead, which `single_instance::holder_is_live_daemon` now
    /// answers only on positive evidence; a lock the kernel releases (`flock` /
    /// `F_SETLK`) is the primitive that removes the window entirely.
    fn drop(&mut self) {
        if read_pid(&self.path)
            == i32::try_from(std::process::id()).map_err(|_| PidfileRead::Unreadable)
        {
            let _ = std::fs::remove_file(&self.path);
        }
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
///
/// The default holder predicate, and the base every stricter one builds on (see
/// [`crate::single_instance`], which layers a process-identity check over it so a
/// recycled PID cannot masquerade as the lock's holder).
#[must_use]
pub fn pid_alive(pid: i32) -> bool {
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
    /// Regression for the second double-hold window. `reclaim` used to
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
            lock.reclaim(pid_alive) == Reclaim::Now,
            "an absent pidfile means the lock is free, so retry immediately"
        );

        // The decisive part: a holder that publishes between the failed read and
        // the steal decision must survive it.
        let guard = lock.acquire().expect("acquire");
        assert!(
            lock.reclaim(pid_alive) == Reclaim::HeldBy(i32::try_from(std::process::id()).unwrap()),
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
        assert!(BdLock::new(path.clone()).reclaim(pid_alive) == Reclaim::Now);
        assert!(!path.exists(), "corrupt pidfile should be removed");
    }

    /// Only ONE of many contenders may steal the same stale pidfile.
    ///
    /// Third route to the module's double-hold: two contenders sample the same
    /// dead PID, both conclude "crashed", and both unlink. The first republishes
    /// and the second's unlink deletes that LIVE pidfile.
    ///
    /// Exercises `steal_atomically` DIRECTLY rather than through
    /// `reclaim`, because that also answers "retry now" for an absent
    /// pidfile ("free, retry") — which would conflate "I reclaimed it" with "it
    /// was already gone" and make the count mean two different things.
    #[test]
    fn only_one_contender_can_steal_the_same_stale_pidfile() {
        const CONTENDERS: usize = 8;

        let dir = tempfile::tempdir().expect("tmpdir");
        let path = dir.path().join("bd.pid");
        std::fs::write(&path, dead_pid().to_string()).expect("seed stale pidfile");

        let lock = BdLock::new(path.clone());
        let barrier = Arc::new(Barrier::new(CONTENDERS));
        let winners = Arc::new(AtomicUsize::new(0));

        let handles: Vec<_> = (0..CONTENDERS)
            .map(|_| {
                let lock = lock.clone();
                let barrier = Arc::clone(&barrier);
                let winners = Arc::clone(&winners);
                std::thread::spawn(move || {
                    barrier.wait();
                    if lock.steal_atomically() {
                        winners.fetch_add(1, Ordering::SeqCst);
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().expect("thread join");
        }

        assert_eq!(
            winners.load(Ordering::SeqCst),
            1,
            "exactly one contender may reclaim a stale pidfile; more than one \
             means two holders can publish over each other"
        );
        assert!(!path.exists(), "the stale pidfile is gone after the steal");
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .expect("read_dir")
            .map(|e| e.expect("entry").file_name())
            .collect();
        assert!(
            leftovers.is_empty(),
            "steal left files behind: {leftovers:?}"
        );
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

    /// A live process we own, killed by its EXACT pid on drop. Used as a lock
    /// holder that is neither this process nor a dead pid, so a predicate can
    /// tell it apart from the contending threads (which all share OUR pid).
    struct Impostor(std::process::Child);

    impl Impostor {
        fn spawn() -> Self {
            Self(
                std::process::Command::new("/bin/sleep")
                    .arg("30")
                    .spawn()
                    .expect("spawn /bin/sleep"),
            )
        }

        fn pid(&self) -> i32 {
            i32::try_from(self.0.id()).expect("pid fits i32")
        }
    }

    impl Drop for Impostor {
        fn drop(&mut self) {
            // Exact pid, never a name-based kill.
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    /// Fail-fast: a live holder is reported by pid, promptly, instead of the
    /// 30-second spin [`BdLock::acquire`] would take.
    #[test]
    fn try_acquire_reports_the_live_holder_promptly() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let path = dir.path().join("daemon.lock");
        let impostor = Impostor::spawn();
        std::fs::write(&path, impostor.pid().to_string()).expect("seed held lock");

        let started = Instant::now();
        let outcome = BdLock::new(path.clone()).try_acquire_with(pid_alive);

        match outcome {
            Err(LockHeld::By(pid)) => assert_eq!(pid, impostor.pid()),
            other => panic!("expected the live holder to be named, got {other:?}"),
        }
        assert!(
            started.elapsed() < ACQUIRE_TIMEOUT / 2,
            "try_acquire spun like the blocking acquire ({:?})",
            started.elapsed()
        );
        assert!(
            path.exists(),
            "a live holder's lock must survive the attempt"
        );
    }

    /// The predicate, not the pid, decides who is respected: a holder judged
    /// dead is stolen even though its process is alive. This is what lets a
    /// single-instance guard reclaim a lock left by a RECYCLED pid.
    #[test]
    fn a_holder_the_predicate_judges_dead_is_stolen() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let path = dir.path().join("daemon.lock");
        let impostor = Impostor::spawn();
        let squatter = impostor.pid();
        std::fs::write(&path, squatter.to_string()).expect("seed held lock");

        let lock = BdLock::new(path.clone());
        let guard = lock
            .try_acquire_with(move |pid| pid != squatter && pid_alive(pid))
            .expect("a holder the predicate rejects is stolen");
        assert_eq!(
            read_pid(&path),
            Ok(i32::try_from(std::process::id()).unwrap())
        );
        drop(guard);
    }

    /// An absent pidfile is FREE, and the fast path must not mistake it for held.
    ///
    /// The window is short but never zero for exactly this reason: `reclaim`
    /// answers "retry now" for an absent file, so a single-shot attempt that
    /// lost one benign link race would report a free lock as held and a daemon
    /// would decline to boot with nothing running.
    #[test]
    fn an_absent_pidfile_is_free_on_the_fast_path() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let lock = BdLock::new(dir.path().join("daemon.lock"));
        let guard = lock.try_acquire_with(pid_alive).expect("free lock is acquired");
        drop(guard);
    }

    /// The fail-fast path must lose none of the exclusion the blocking path has:
    /// contenders racing from a barrier never overlap inside the lock.
    ///
    /// Losers here fail fast rather than blocking, so the assertion is two-part —
    /// zero overlaps AND progress (somebody wins every round) — otherwise a lock
    /// that always declined would pass the overlap half trivially.
    #[test]
    fn contended_try_acquire_never_admits_two_holders() {
        const THREADS: usize = 4;
        const ROUNDS: usize = 30;
        const HOLD: Duration = Duration::from_millis(2);

        let dir = tempfile::tempdir().expect("tmpdir");
        let lock = BdLock::new(dir.path().join("daemon.lock"));
        let holders = Arc::new(AtomicUsize::new(0));
        let overlaps = Arc::new(AtomicUsize::new(0));
        let acquisitions = Arc::new(AtomicUsize::new(0));
        let barrier = Arc::new(Barrier::new(THREADS));

        let handles: Vec<_> = (0..THREADS)
            .map(|_| {
                let lock = lock.clone();
                let holders = Arc::clone(&holders);
                let overlaps = Arc::clone(&overlaps);
                let acquisitions = Arc::clone(&acquisitions);
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    for _ in 0..ROUNDS {
                        barrier.wait();
                        let Ok(guard) = lock.try_acquire_with(pid_alive) else {
                            continue;
                        };
                        acquisitions.fetch_add(1, Ordering::SeqCst);
                        let before = holders.fetch_add(1, Ordering::SeqCst);
                        std::thread::sleep(HOLD);
                        let during = holders.load(Ordering::SeqCst);
                        holders.fetch_sub(1, Ordering::SeqCst);
                        drop(guard);
                        if before != 0 || during != 1 {
                            overlaps.fetch_add(1, Ordering::SeqCst);
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
            "the fail-fast path admitted concurrent holders"
        );
        assert!(
            acquisitions.load(Ordering::SeqCst) >= ROUNDS,
            "expected at least one winner per round, got {} over {ROUNDS} rounds",
            acquisitions.load(Ordering::SeqCst)
        );
    }

    /// Releasing must not delete a pidfile that is no longer ours.
    ///
    /// The mirror of the steal-path invariant: if a contender reclaimed the path
    /// while we held a guard, our drop must leave ITS pidfile alone, or the lock
    /// admits two holders on the way out instead of on the way in.
    #[test]
    fn dropping_a_guard_leaves_a_successors_pidfile_alone() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let path = dir.path().join("daemon.lock");
        let guard = BdLock::new(path.clone()).acquire().expect("acquire");

        // A successor reclaimed the path and published its own pid.
        std::fs::write(&path, "424242").expect("successor publishes");
        drop(guard);

        assert!(path.exists(), "the successor's pidfile was deleted");
        assert_eq!(read_pid(&path), Ok(424_242));
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
