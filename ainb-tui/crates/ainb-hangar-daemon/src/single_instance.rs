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
    acquire_with(dir, holder_is_live_daemon)
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
