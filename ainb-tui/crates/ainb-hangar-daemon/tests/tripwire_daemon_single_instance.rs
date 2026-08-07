//! Process-level tripwire for one-daemon-per-hangar-home.
//!
//! The incident this guards against: 69 `ainb hangar daemon run` processes on one
//! machine, 65 of them orphaned at `ppid==1`, twice exhausting a 32 GB Mac. The
//! cause was a check-then-act guard — a pidfile written at the END of boot and
//! read by a different process BEFORE spawning — so every duplicate booted, and
//! `rpc::bind` unlinked the incumbent's socket on the way past.
//!
//! Unit tests cannot prove this: the defect lives between processes. So these
//! spawn REAL daemon binaries against an isolated `HOME` and assert on the
//! outcomes an operator would see — one survivor, a losing daemon that exits 0,
//! and an incumbent whose socket is never stolen out from under it.
//!
//! Safety (mandatory, mirrors `scripts/soak-codex-orphans.sh`): every daemon here
//! is a child of this test, killed by its EXACT pid via `DaemonProcess`'s drop.
//! No `pkill`, no `killall`, no name matching. `HOME` is pinned and the home
//! overrides removed, so nothing touches the operator's `~/.agents-in-a-box`, and
//! `AINB_CODEX_MANAGED=0` keeps the codex manager (and the desktop Codex app)
//! entirely out of it.

mod tripwire_support;

use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use tripwire_support::DaemonProcess;

/// Keep the codex manager out of these tests: it is irrelevant to ownership and
/// its boot reaper signals processes outside this tempdir.
const CODEX_OFF: (&str, &str) = ("AINB_CODEX_MANAGED", "0");

/// Generous: the lock is taken before the store opens, so it lands early, but a
/// cold CI runner still has to exec the binary.
const LOCK_BUDGET: Duration = Duration::from_secs(30);
/// The socket appears only after migrations + token mint, so it needs more room.
const SOCKET_BUDGET: Duration = Duration::from_secs(60);

fn hangar_home(home: &Path) -> PathBuf {
    home.join(".agents-in-a-box")
}

fn lock_path(home: &Path) -> PathBuf {
    hangar_home(home).join("hangar").join("daemon.lock")
}

fn socket_path(home: &Path) -> PathBuf {
    hangar_home(home).join("hangar.sock")
}

fn read_lock_pid(home: &Path) -> Option<i32> {
    std::fs::read_to_string(lock_path(home)).ok()?.trim().parse().ok()
}

/// Poll until the ownership lock names someone, or fail with what was seen.
fn wait_for_lock(home: &Path) -> i32 {
    wait_until(LOCK_BUDGET, || read_lock_pid(home)).unwrap_or_else(|| {
        panic!(
            "no daemon claimed {} within {LOCK_BUDGET:?}",
            lock_path(home).display()
        )
    })
}

fn wait_for_socket(home: &Path) -> std::fs::Metadata {
    wait_until(SOCKET_BUDGET, || std::fs::metadata(socket_path(home)).ok()).unwrap_or_else(|| {
        panic!(
            "daemon never bound {} within {SOCKET_BUDGET:?}",
            socket_path(home).display()
        )
    })
}

fn wait_until<T, F: Fn() -> Option<T>>(budget: Duration, probe: F) -> Option<T> {
    let deadline = Instant::now() + budget;
    loop {
        if let Some(v) = probe() {
            return Some(v);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn pid_alive(pid: i32) -> bool {
    use nix::errno::Errno;
    use nix::sys::signal::kill;
    use nix::unistd::Pid;
    matches!(kill(Pid::from_raw(pid), None), Ok(()) | Err(Errno::EPERM))
}

/// Run the daemon binary to completion under `home`, returning (exit ok, elapsed).
///
/// Used for the daemon that is EXPECTED to decline: it must return by itself, so
/// there is nothing to kill.
fn run_daemon_to_completion(home: &Path, args: &[&str]) -> (std::process::ExitStatus, Duration) {
    let started = Instant::now();
    let status = Command::new(assert_cmd::cargo::cargo_bin("ainb-hangar-daemon"))
        .args(args)
        .env("HOME", home)
        .env_remove("AINB_HOME")
        .env_remove("AINB_HANGAR_HOME")
        .env(CODEX_OFF.0, CODEX_OFF.1)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .expect("run second daemon");
    (status, started.elapsed())
}

/// The headline guarantee: a second daemon on a live home does not boot.
///
/// Asserts the three things that actually went wrong in the incident, not just
/// "it exited": the loser exits 0 (so a supervisor does not restart-loop it), it
/// exits PROMPTLY (a fail-fast guard, not a 30s spin), the incumbent keeps the
/// lock, and — the corruption the pile caused — the incumbent's socket inode is
/// untouched, proving `rpc::bind` never unlinked it.
#[test]
fn a_second_daemon_declines_a_live_home_and_exits_zero() {
    let home = tempfile::tempdir().expect("tempdir home");
    let incumbent = DaemonProcess::spawn(home.path(), &[CODEX_OFF]);
    let holder = wait_for_lock(home.path());
    assert_eq!(
        holder,
        i32::try_from(incumbent.pid()).expect("pid fits i32"),
        "the first daemon should own the home"
    );
    let socket_before = wait_for_socket(home.path());

    let (status, elapsed) = run_daemon_to_completion(home.path(), &[]);

    assert!(
        status.success(),
        "a losing daemon must exit 0 so supervisors do not restart-loop it, got {status:?}"
    );
    assert!(
        elapsed < Duration::from_secs(20),
        "the guard should fail fast, took {elapsed:?}"
    );
    assert_eq!(
        read_lock_pid(home.path()),
        Some(holder),
        "the loser must not disturb the incumbent's lock"
    );
    assert!(pid_alive(holder), "the incumbent must still be running");

    let socket_after = std::fs::metadata(socket_path(home.path())).expect("socket still present");
    assert_eq!(
        socket_before.ino(),
        socket_after.ino(),
        "the loser rebound the socket, orphaning the incumbent's listener"
    );
}

/// `--once` is a full boot too: it opens the store and binds the socket. Against
/// a live home that used to unlink the running daemon's socket, so it must
/// decline exactly like a normal duplicate.
#[test]
fn once_mode_declines_a_home_another_daemon_owns() {
    let home = tempfile::tempdir().expect("tempdir home");
    let incumbent = DaemonProcess::spawn(home.path(), &[CODEX_OFF]);
    let holder = wait_for_lock(home.path());
    let socket_before = wait_for_socket(home.path());

    let (status, _) = run_daemon_to_completion(home.path(), &["--once"]);

    assert!(status.success(), "--once should exit 0, got {status:?}");
    assert_eq!(read_lock_pid(home.path()), Some(holder));
    assert!(pid_alive(
        i32::try_from(incumbent.pid()).expect("pid fits i32")
    ));
    let socket_after = std::fs::metadata(socket_path(home.path())).expect("socket still present");
    assert_eq!(
        socket_before.ino(),
        socket_after.ino(),
        "--once stole the live daemon's socket"
    );
}

/// `hangar daemon stop` sends SIGTERM. Until this fix the daemon had no handler
/// for it, so it died on the OS default disposition: no teardown ran and the
/// ownership lock was never released. Now it must exit AND release, so the very
/// next `start` — as `daemon restart` does — takes the home cleanly.
#[test]
fn sigterm_stops_the_daemon_and_releases_the_home() {
    let home = tempfile::tempdir().expect("tempdir home");
    let mut outgoing = DaemonProcess::spawn(home.path(), &[CODEX_OFF]);
    let holder = wait_for_lock(home.path());
    // Wait for a fully booted daemon: signalling mid-boot would prove nothing
    // about the run loop's handler.
    wait_for_socket(home.path());

    let signalled = Command::new("kill")
        .args(["-TERM", &holder.to_string()])
        .status()
        .expect("send SIGTERM");
    assert!(signalled.success(), "kill -TERM {holder} failed");

    // `wait_for_exit`, not `pid_alive`: this test is the daemon's parent, so an
    // exited-but-unreaped daemon is a zombie and answers `kill(pid, 0)` like a
    // live process.
    assert!(
        outgoing.wait_for_exit(Duration::from_secs(20)),
        "the daemon must exit on SIGTERM"
    );
    assert!(
        wait_until(Duration::from_secs(10), || (!lock_path(home.path())
            .exists())
        .then_some(()))
        .is_some(),
        "a graceful stop must release the ownership lock, or the next start would \
         have to steal it"
    );
    drop(outgoing);

    // The successor takes the home with no stealing involved.
    let successor = DaemonProcess::spawn(home.path(), &[CODEX_OFF]);
    assert_eq!(
        wait_for_lock(home.path()),
        i32::try_from(successor.pid()).expect("pid fits i32")
    );
}

/// A SIGKILLed daemon runs no destructor, so it leaves the lock file behind
/// naming a dead pid. That must not lock the home out forever — the next boot
/// steals it (atomically, `rename`, winner-take-all) and takes over.
#[test]
fn a_stale_lock_left_by_a_killed_daemon_is_reclaimed() {
    let home = tempfile::tempdir().expect("tempdir home");
    let dead_pid = {
        let doomed = DaemonProcess::spawn(home.path(), &[CODEX_OFF]);
        let pid = wait_for_lock(home.path());
        drop(doomed); // kills by EXACT pid, then reaps
        pid
    };
    assert!(
        lock_path(home.path()).exists(),
        "a hard-killed daemon leaves its lock behind — that is the case under test"
    );
    assert_eq!(read_lock_pid(home.path()), Some(dead_pid));

    let successor = DaemonProcess::spawn(home.path(), &[CODEX_OFF]);
    let holder = wait_until(LOCK_BUDGET, || {
        read_lock_pid(home.path()).filter(|pid| *pid != dead_pid)
    })
    .expect("the successor should reclaim the stale lock");

    assert_eq!(
        holder,
        i32::try_from(successor.pid()).expect("pid fits i32"),
        "the successor must own the home after stealing the stale lock"
    );
}
