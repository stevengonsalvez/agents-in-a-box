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
//! again. So the predicate also asks the process table what that pid is running.
//!
//! That check FAILS SAFE in every direction: a `ps` that cannot answer, an argv
//! shape it does not recognise, and a holder running our own executable all
//! RESPECT the holder. Only positive evidence of a different program justifies a
//! steal. The asymmetry is deliberate — declining to boot is loud and
//! recoverable (the log names the file to remove), whereas stealing a live
//! daemon's lock silently recreates the double-hold this module exists to
//! prevent.
//!
//! The kernel offers a primitive with none of this bookkeeping: an `flock` /
//! `F_SETLK` held for the process lifetime is released by the kernel on exit,
//! crash and reboot alike, which removes the recycled-pid problem at the root.
//! This module reuses [`BdLock`] instead because that carries the crate's
//! existing double-hold race tests; the lock-file primitive is the better
//! long-term shape and the identity layer is what it costs to keep the pidfile.

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
        // `Contended` means the path churned for a whole window without ever
        // naming a live holder. That is NOT evidence anyone won it, so declining
        // on it is the one way this guard could leave a home with NO daemon:
        // symmetric contenders all sampling churn, all standing down.
        //
        // Escalate instead of declining. A blocking attempt either takes the
        // lock (nobody had it — exactly the case a fail-fast sample cannot tell
        // apart from a busy one) or resolves into an honest `HeldBy` once a
        // winner publishes. Only if THAT still cannot name anyone do we decline,
        // by which point a real owner has had seconds to appear.
        Ownership::Contended => Ok(escalate(dir)),
        won_or_held => Ok(won_or_held),
    }
}

/// How long the `Contended` escalation blocks before giving up.
const CONTENDED_ESCALATION: std::time::Duration = std::time::Duration::from_secs(5);

/// One blocking attempt after fail-fast could not name a holder.
fn escalate(dir: &Path) -> Ownership {
    match BdLock::new(lock_path_in(dir))
        .acquire_within_with(CONTENDED_ESCALATION, holder_is_live_daemon)
    {
        Ok(guard) => Ownership::Acquired(guard),
        Err(LockHeld::By(pid)) => Ownership::HeldBy(pid),
        // Still nobody nameable. Decline: something is churning the path that we
        // cannot identify, and booting alongside it is the worse failure.
        Err(LockHeld::Contended | LockHeld::Io(_)) => Ownership::Contended,
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

/// Is `pid` a live process this daemon must respect as the home's owner?
///
/// Three ways to answer yes, and only ONE way to answer no. That asymmetry is
/// the contract [`BdLock::try_acquire_with`] demands: a false negative steals a
/// LIVE holder's lock and puts two daemons on one home, so "not a daemon" is
/// only ever concluded from positive evidence that the holder is a different
/// program.
///
/// 1. `ps` cannot answer → respect (the original fail-safe).
/// 2. The argv is a recognised daemon shape → respect.
/// 3. The argv is unrecognised but is the SAME COMMAND LINE we were invoked
///    with → respect. This covers every daemon whose argv
///    [`is_hangar_daemon_args`] cannot know about: an `AINB_HANGAR_DAEMON_BIN`
///    override pointing at a wrapper or a dev binary, an install path
///    containing a space (`ps` renders argv whitespace-separated, so
///    `/Volumes/My Passport/bin/ainb-hangar-daemon` has a first token of
///    `/Volumes/My`), and an in-process `boot()` inside a cargo test binary,
///    whose argv is `target/debug/deps/<hash>`.
///
/// Without (3) a second daemon judged such an incumbent a stranger, stole its
/// lock, and `rpc::bind` then unlinked its socket — the exact double-daemon
/// incident this module exists to prevent, reintroduced by the guard meant to
/// stop it.
#[must_use]
pub fn holder_is_live_daemon(pid: i32) -> bool {
    if !pid_alive(pid) {
        return false;
    }
    let Some(args) = process_argv(pid) else {
        return true;
    };
    is_hangar_daemon_args(&args) || shares_our_command_line(&args)
}

/// Is this argv the SAME INVOCATION as ours — the whole command line, not just
/// the executable?
///
/// Comparing executables alone was a hole: on a Homebrew layout there is no
/// sidecar binary, so the daemon self-execs as `ainb hangar daemon run` and its
/// `current_exe` is plain `ainb` — the same executable the TUI runs. A recycled
/// pid landing on the user's TUI would then be respected as this home's owner,
/// and the daemon would decline to boot for as long as that TUI lived: zero
/// daemons, silently, which is the failure this module exists to prevent turned
/// inside out.
///
/// The full command line separates them (`…/ainb` vs `…/ainb hangar daemon
/// run`) while still covering everything [`is_hangar_daemon_args`] cannot know
/// about — an `AINB_HANGAR_DAEMON_BIN` wrapper, an install path containing a
/// space, an in-process `boot()` in a test binary — because two processes of
/// that kind share one command line.
///
/// An unresolvable argv answers `true` (respect the holder), keeping every
/// unknown on the fail-safe side.
fn shares_our_command_line(args: &str) -> bool {
    our_command_line().is_none_or(|ours| args == ours)
}

/// This process's own argv, rendered the way `ps -o args=` renders it.
fn our_command_line() -> Option<String> {
    let exe = std::env::current_exe().ok()?.to_string_lossy().into_owned();
    let rest: Vec<String> = std::env::args().skip(1).collect();
    let line = if rest.is_empty() {
        exe
    } else {
        format!("{exe} {}", rest.join(" "))
    };
    (!line.trim().is_empty()).then_some(line)
}

/// How long to wait for `ps` before treating the process table as unreadable.
///
/// `holder_is_live_daemon` runs as part of the FIRST statement of `boot`, so an
/// unbounded wait here would hang startup on a wedged `ps` (a stalled disk, a
/// frozen cgroup) — defeating the fail-fast the whole guard is built around. A
/// timeout degrades to the `None` branch, which respects the holder.
const PS_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

/// The full argv of `pid` as one line, or `None` when `ps` cannot answer in
/// time.
///
/// Exported so the CLI can apply the same identity rule to the same lock file —
/// the two halves disagreeing about who owns a home is how a recycled pid ends
/// up being reported as a running daemon, and `SIGTERM`ed by `stop`.
#[must_use]
pub fn process_argv(pid: i32) -> Option<String> {
    let mut child = std::process::Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "args="])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;

    let deadline = std::time::Instant::now() + PS_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if std::time::Instant::now() >= deadline => {
                // Reap our own child by its exact handle so a wedged `ps` is not
                // left behind, then answer "unreadable".
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(20)),
            Err(_) => return None,
        }
    };
    if !status.success() {
        return None;
    }
    let mut stdout = child.stdout.take()?;
    let mut args = String::new();
    std::io::Read::read_to_string(&mut stdout, &mut args).ok()?;
    let args = args.trim();
    (!args.is_empty()).then(|| args.to_string())
}

/// How often the watchdog re-checks that this daemon still owns its home.
///
/// Overridable with `AINB_HANGAR_OWNERSHIP_WATCH_MS` so a tripwire can drive
/// several ticks inside a test budget, mirroring `HANGAR_DAEMON_POLL_MS`.
fn watchdog_interval() -> std::time::Duration {
    /// The tick is the WIDTH of the window in which two daemons can be live on
    /// one home: the newcomer unlinks the incumbent's socket the moment it
    /// binds, while the incumbent keeps claiming and sweeping until its next
    /// sample. 5s bounds that, at the cost of one file read per tick.
    const DEFAULT: std::time::Duration = std::time::Duration::from_secs(5);

    std::env::var("AINB_HANGAR_OWNERSHIP_WATCH_MS")
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .filter(|ms| *ms > 0)
        .map_or(DEFAULT, std::time::Duration::from_millis)
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

    /// A lock left by a `SIGKILL`ed daemon whose pid has since been recycled onto
    /// an unrelated live process must be reclaimable, or the daemon can never
    /// boot again after a reboot.
    #[test]
    fn a_lock_naming_a_live_non_daemon_is_reclaimed() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let path = lock_path_in(dir.path());
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");

        // A genuinely foreign live process — the shape a recycled pid takes
        // after a reboot. Our OWN pid would not do: this test binary is the
        // executable a daemon would be running, so it is respected by design
        // (see `a_holder_running_our_own_binary_is_respected_whatever_its_argv`).
        // Killed by its exact pid on drop.
        let mut stranger = std::process::Command::new("/bin/sleep")
            .arg("30")
            .spawn()
            .expect("spawn /bin/sleep");
        std::fs::write(&path, stranger.id().to_string()).expect("seed lock");

        let outcome = acquire(dir.path()).expect("acquire");
        let _ = stranger.kill();
        let _ = stranger.wait();

        match outcome {
            Ownership::Acquired(_) => {}
            other => panic!("expected a recycled-pid lock to be reclaimed, got {other:?}"),
        }
    }

    /// The fail-safe that stops the guard from recreating the incident.
    ///
    /// A daemon whose argv `is_hangar_daemon_args` cannot recognise — a wrapper
    /// via `AINB_HANGAR_DAEMON_BIN`, an install path with a space, an
    /// in-process `boot()` in a test binary — must still be respected, or a
    /// second daemon steals its lock and both end up serving one home.
    #[test]
    fn a_holder_sharing_our_command_line_is_respected_whatever_its_argv() {
        let ours = our_command_line().expect("our own command line");

        assert!(shares_our_command_line(&ours), "our exact invocation");
        assert!(
            !is_hangar_daemon_args(&ours),
            "this test binary's argv is exactly the unrecognised shape under test"
        );
        assert!(
            holder_is_live_daemon(i32::try_from(std::process::id()).unwrap()),
            "our own live process must never be judged a stranger"
        );
    }

    /// The hole this replaced an executable-only check to close.
    ///
    /// On a Homebrew layout the daemon self-execs as `ainb hangar daemon run`,
    /// so its executable IS the TUI's executable. Crediting a bare `ainb` as
    /// this home's daemon would let a recycled pid landing on the user's TUI
    /// wedge the home with zero daemons for as long as that TUI lives.
    #[test]
    fn our_executable_running_a_different_command_line_is_not_us() {
        let exe = std::env::current_exe().expect("current_exe");
        let exe = exe.to_string_lossy().into_owned();

        // Same binary, different invocation — e.g. the TUI next to a self-exec'd
        // daemon. Not us, and not a recognised daemon shape either.
        assert!(!shares_our_command_line(&exe) || std::env::args().len() == 1);
        assert!(!shares_our_command_line(&format!("{exe} tui")));
        assert!(!is_hangar_daemon_args(&exe));
    }

    /// The other half: a genuinely different program is not protected, or a
    /// recycled pid would wedge the home forever.
    #[test]
    fn a_different_executable_is_not_ours() {
        assert!(!shares_our_command_line("/bin/sleep 30"));
        assert!(!shares_our_command_line("/usr/bin/vim"));
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
