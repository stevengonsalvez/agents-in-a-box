//! One daemon per hangar home, enforced before anything else exists.
//!
//! Everything [`crate::boot`] builds — the SQLite store, the claim loop, the
//! sweepers, the control socket — assumes sole ownership of one hangar home. Two
//! daemons on one home is not a degraded mode: `rpc::bind` unlinks the incumbent's
//! socket (leaving it `accept()`ing an inode no client can reach) while both claim
//! loops and both sweepers race the same database.
//!
//! The guard that used to stand in for this was a pidfile written at the END of
//! boot and read by a different process BEFORE spawning — a check-then-act with a
//! window spanning a dozen subsystem initialisations, and a `std::fs::write` that
//! cannot fail, so a losing daemon never learned it had lost. This module replaces
//! it with [`crate::beads_adapter::lock::BdLock`], the crate's existing hardened
//! cross-process mutex, taken as the first statement of `boot`.
//!
//! # Why the holder check is not just `kill(pid, 0)`
//!
//! `<hangar_home>/hangar/daemon.lock` outlives reboots: a SIGKILLed (or
//! power-cut) daemon leaves the file naming its pid, and after the next boot that
//! pid very likely belongs to some unrelated process. A liveness-only check would
//! read that as "a daemon owns this home" forever and the daemon would never start
//! again. So the predicate also asks the process table whether the pid still looks
//! like a hangar daemon.
//!
//! That check FAILS SAFE: when `ps` cannot answer, the holder is respected. The
//! asymmetry is deliberate — declining to boot is loud and recoverable (the log
//! names the file to remove), whereas stealing a live daemon's lock silently
//! recreates the double-hold this module exists to prevent.

use std::path::{Path, PathBuf};

use crate::beads_adapter::lock::{BdLock, BdLockGuard, LockHeld, pid_alive};

/// Resolve the ownership lock for a hangar home:
/// `<hangar_home>/hangar/daemon.lock`.
///
/// Beside `daemon.pid` rather than replacing it: the pid file remains the
/// discovery/status artifact, while this file is the thing that actually decides
/// who runs.
#[must_use]
pub fn lock_path_in(dir: &Path) -> PathBuf {
    dir.join("hangar").join("daemon.lock")
}

/// The outcome of asking for a hangar home.
#[derive(Debug)]
pub enum Ownership {
    /// This process owns the home. The guard MUST be held for the daemon's whole
    /// lifetime — dropping it early republishes the home as free.
    Acquired(BdLockGuard),
    /// A live hangar daemon already owns the home, named by pid.
    HeldBy(i32),
    /// The lock churned for the whole window without ever being publishable and
    /// without a live holder to name. Another daemon is mid-acquisition; the
    /// correct response is still to decline.
    Contended,
}

/// Claim `dir` for this process, or report who owns it.
///
/// # Errors
///
/// The lock's parent directory could not be created. That is the same
/// `<home>/hangar` directory the store and pid file need, so it is fatal rather
/// than a reason to boot unguarded.
pub fn acquire(dir: &Path) -> std::io::Result<Ownership> {
    match acquire_with(dir, holder_is_live_daemon)? {
        // Sample twice before declining on `Contended`. That verdict means the
        // path churned for the whole window without ever naming a live holder,
        // which is NOT evidence that someone else won it — and if every
        // contender declined on one such sample, the home would end up with no
        // daemon at all. A second pass costs one fail-fast window and turns the
        // usual case (someone did win) into an honest `HeldBy`.
        Ownership::Contended => acquire_with(dir, holder_is_live_daemon),
        won_or_held => Ok(won_or_held),
    }
}

/// [`acquire`] with an injectable holder predicate, so the decision table is
/// testable without a real daemon in the process list.
fn acquire_with<F>(dir: &Path, holder_is_live: F) -> std::io::Result<Ownership>
where
    F: Fn(i32) -> bool,
{
    match BdLock::new(lock_path_in(dir)).try_acquire_with(holder_is_live) {
        Ok(guard) => Ok(Ownership::Acquired(guard)),
        Err(LockHeld::By(pid)) => Ok(Ownership::HeldBy(pid)),
        Err(LockHeld::Contended) => Ok(Ownership::Contended),
        Err(LockHeld::Io(e)) => Err(e),
    }
}

/// Is `pid` a live process that still looks like a hangar daemon?
///
/// Fails safe: a process table we cannot read yields `true` (respect the
/// holder). See the module doc for why that direction is the safe one.
fn holder_is_live_daemon(pid: i32) -> bool {
    if !pid_alive(pid) {
        return false;
    }
    ps_args(pid).is_none_or(|args| is_hangar_daemon_args(&args))
}

/// The full argv of `pid` as one line, or `None` when `ps` cannot answer.
fn ps_args(pid: i32) -> Option<String> {
    let output = std::process::Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "args="])
        .stdin(std::process::Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let args = String::from_utf8(output.stdout).ok()?;
    let args = args.trim();
    (!args.is_empty()).then(|| args.to_string())
}

/// How often the watchdog re-checks that this daemon still owns its home.
///
/// Overridable with `AINB_HANGAR_OWNERSHIP_WATCH_MS` so a tripwire can drive
/// several ticks inside a test budget, mirroring `HANGAR_DAEMON_POLL_MS`.
fn watchdog_interval() -> std::time::Duration {
    std::env::var("AINB_HANGAR_OWNERSHIP_WATCH_MS")
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .filter(|ms| *ms > 0)
        .map_or(
            std::time::Duration::from_secs(30),
            std::time::Duration::from_millis,
        )
}

/// What one watchdog sample saw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Held {
    /// The lock still names us. Nothing to do.
    Ours,
    /// The lock is absent or unreadable. NOT proof we lost it — a steal may be
    /// mid-flight, and a daemon that exited on a transient read failure would be
    /// a worse bug than the one this watches for.
    Unknown,
    /// Another pid holds the lock and is a live daemon: it owns the home now.
    Lost(i32),
    /// Another pid is recorded but is not a live daemon — debris, not an owner.
    Stale(i32),
}

/// Classify the lock file's current contents against our own pid.
///
/// Pure but for the `holder_is_live` probe, so every branch is testable.
fn sample<F>(lock: &Path, mine: i32, holder_is_live: F) -> Held
where
    F: Fn(i32) -> bool,
{
    let Some(holder) = std::fs::read_to_string(lock)
        .ok()
        .and_then(|text| text.trim().parse::<i32>().ok())
    else {
        return Held::Unknown;
    };
    if holder == mine {
        Held::Ours
    } else if holder_is_live(holder) {
        Held::Lost(holder)
    } else {
        Held::Stale(holder)
    }
}

/// Watch that this daemon still owns `dir`, resolving with the new owner's pid
/// if it ever stops owning it.
///
/// The layer that does not depend on any exit path running. A daemon can only
/// lose a lock it holds by something outside its control — an operator deleting
/// the file, a predicate misfire, a home restored from a backup — and two
/// daemons on one home is the failure this whole module exists to prevent, so
/// the right response is to stand down rather than race.
///
/// Scoped to THIS home by construction: it reads one file and signals no one.
/// That is the difference between this and an argv-matching reaper, which cannot
/// tell which home a process serves and would kill daemons belonging to others.
pub async fn watch_ownership(dir: &Path) -> i32 {
    let lock = lock_path_in(dir);
    let mine = i32::try_from(std::process::id()).unwrap_or(-1);
    let interval = watchdog_interval();
    loop {
        tokio::time::sleep(interval).await;
        match sample(&lock, mine, holder_is_live_daemon) {
            Held::Ours => {}
            Held::Lost(pid) => return pid,
            // Neither is proof of loss. If the home really is free, the next
            // daemon to take it turns this into `Lost` on a later tick — one
            // tick of overlap, versus an exit on a transient read.
            Held::Unknown => tracing::warn!(
                lock = %lock.display(),
                "hangar ownership lock is missing; continuing to serve"
            ),
            Held::Stale(pid) => tracing::warn!(
                lock = %lock.display(),
                holder = pid,
                "hangar ownership lock names a stale pid; continuing to serve"
            ),
        }
    }
}

/// Does this argv line belong to a hangar daemon?
///
/// Two shapes launch one (`cli::hangar::resolve_daemon_launch`): the sidecar
/// binary `ainb-hangar-daemon`, and `ainb` self-exec'ing as
/// `ainb hangar daemon run` when the sidecar is absent or stale.
///
/// Matching is exact-token, not substring: `ainb hangar daemon status` and
/// `ainb hangar daemon stop` are ordinary CLI invocations that must never be
/// mistaken for a running daemon, and a bare `ainb` (the TUI) must not either.
#[must_use]
pub fn is_hangar_daemon_args(args: &str) -> bool {
    let tokens: Vec<&str> = args.split_whitespace().collect();
    let sidecar = tokens
        .first()
        .map(Path::new)
        .and_then(Path::file_name)
        .is_some_and(|name| name == "ainb-hangar-daemon");
    sidecar || tokens.windows(3).any(|w| w == ["hangar", "daemon", "run"])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sidecar_and_self_exec_argv_are_recognised() {
        for args in [
            "/opt/homebrew/bin/ainb-hangar-daemon",
            "ainb-hangar-daemon",
            "target/debug/ainb-hangar-daemon --once",
            "/Users/x/.cargo/bin/ainb hangar daemon run",
            "ainb hangar daemon run",
        ] {
            assert!(is_hangar_daemon_args(args), "should match: {args}");
        }
    }

    /// The false positives that matter: a mismatch here either wedges every
    /// future boot (an ordinary CLI process mistaken for the owner) or, on the
    /// `stop --all` side, signals something that is not a daemon at all.
    #[test]
    fn cli_verbs_the_tui_and_strangers_are_not_daemons() {
        for args in [
            "/opt/homebrew/bin/ainb",
            "ainb",
            "ainb hangar daemon status",
            "ainb hangar daemon stop --all",
            "ainb hangar daemon restart",
            "ainb run --agent claude",
            "/bin/sleep 30",
            "grep ainb-hangar-daemon",
            "",
        ] {
            assert!(!is_hangar_daemon_args(args), "should not match: {args}");
        }
    }

    #[test]
    fn the_lock_lives_beside_the_pid_file() {
        let dir = Path::new("/tmp/hangar-home");
        assert_eq!(
            lock_path_in(dir),
            Path::new("/tmp/hangar-home/hangar/daemon.lock")
        );
    }

    #[test]
    fn a_free_home_is_acquired() {
        let dir = tempfile::tempdir().expect("tmpdir");
        match acquire(dir.path()).expect("acquire") {
            Ownership::Acquired(_) => {}
            other => panic!("expected a free home to be acquired, got {other:?}"),
        }
    }

    /// The decisive case: a second daemon must be told who owns the home rather
    /// than booting alongside it.
    #[test]
    fn a_home_owned_by_a_live_daemon_is_declined() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let held = acquire(dir.path()).expect("first acquire");
        assert!(matches!(held, Ownership::Acquired(_)));

        // Our own pid holds it, and the predicate is told we are a daemon.
        match acquire_with(dir.path(), |_| true).expect("second acquire") {
            Ownership::HeldBy(pid) => {
                assert_eq!(pid, i32::try_from(std::process::id()).unwrap());
            }
            other => panic!("expected the home to be declined, got {other:?}"),
        }
        drop(held);
    }

    /// A lock left by a SIGKILLed daemon whose pid has since been recycled onto
    /// an unrelated live process must be reclaimable, or the daemon can never
    /// boot again after a reboot.
    #[test]
    fn a_lock_naming_a_live_non_daemon_is_reclaimed() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let path = lock_path_in(dir.path());
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        // A live pid that is emphatically not a daemon: our own test process.
        std::fs::write(&path, std::process::id().to_string()).expect("seed lock");

        match acquire(dir.path()).expect("acquire") {
            Ownership::Acquired(_) => {}
            other => panic!("expected a recycled-pid lock to be reclaimed, got {other:?}"),
        }
    }

    /// The watchdog's whole decision table. The dangerous cell is `Unknown`:
    /// reading an absent or unreadable file as "we lost it" would let a
    /// transient failure shut down a healthy daemon.
    #[test]
    fn the_watchdog_stands_down_only_for_a_live_successor() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let lock = dir.path().join("daemon.lock");
        let mine = 4242;

        assert_eq!(sample(&lock, mine, |_| true), Held::Unknown, "absent");

        std::fs::write(&lock, "not-a-pid").expect("write");
        assert_eq!(sample(&lock, mine, |_| true), Held::Unknown, "unreadable");

        std::fs::write(&lock, mine.to_string()).expect("write");
        assert_eq!(sample(&lock, mine, |_| true), Held::Ours);

        std::fs::write(&lock, "9001").expect("write");
        assert_eq!(sample(&lock, mine, |_| true), Held::Lost(9001));
        assert_eq!(sample(&lock, mine, |_| false), Held::Stale(9001));
    }

    /// The fail-safe direction: an unreadable process table must NOT be read as
    /// "the holder is gone".
    #[test]
    fn an_unanswerable_process_table_respects_the_holder() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let held = acquire(dir.path()).expect("first acquire");
        // `holder_is_live_daemon` with `ps_args` returning None reduces to
        // `pid_alive`, which is what this predicate models.
        match acquire_with(dir.path(), pid_alive).expect("second acquire") {
            Ownership::HeldBy(_) => {}
            other => panic!("expected the holder to be respected, got {other:?}"),
        }
        drop(held);
    }
}
