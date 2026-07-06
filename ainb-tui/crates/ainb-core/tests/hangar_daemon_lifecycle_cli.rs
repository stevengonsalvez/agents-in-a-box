//! e38.20 user-visible proof: the `ainb hangar daemon {start,stop,restart}`
//! lifecycle CLI really brings the control-plane daemon up and down.
//!
//! Drives the real `ainb` binary against an isolated `$AINB_HANGAR_HOME`:
//! `daemon start` spawns the `ainb-hangar-daemon` binary as a background child
//! and writes its EXACT pid to `<home>/hangar/daemon.pid`; `daemon status`
//! reports running with the socket bound; `daemon stop` kills that EXACT pid
//! (read back from the PID file) and removes the file; `status` then reports
//! stopped. The daemon is only ever killed by the pid the CLI recorded — never
//! by name — and a bounded poll waits for liveness transitions.
//!
//! The daemon binary lives beside the `ainb` binary in the same `target/<profile>`
//! dir, so it is resolved as a sibling of `CARGO_BIN_EXE_ainb` and handed to the
//! CLI via `AINB_HANGAR_DAEMON_BIN`. If that binary is missing (a partial build)
//! the test SKIPs rather than fails — the weak macOS CI runner must never be
//! blocked on a binary it did not build.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

/// Path to the `ainb` binary under test.
fn ainb_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ainb"))
}

/// Resolve the `ainb-hangar-daemon` binary that sits beside `ainb` in the same
/// target dir. Returns `None` (⇒ SKIP) if it was not built.
fn daemon_bin() -> Option<PathBuf> {
    let dir = ainb_bin().parent()?.to_path_buf();
    let candidate = dir.join("ainb-hangar-daemon");
    candidate.exists().then_some(candidate)
}

/// Run `ainb hangar daemon <args>` against an isolated home, pointing the CLI at
/// the sibling daemon binary. Returns (success, combined stdout+stderr).
fn run(home: &Path, daemon: &Path, args: &[&str]) -> (bool, String) {
    let out = Command::new(ainb_bin())
        .args(args)
        .env("AINB_HANGAR_HOME", home)
        .env("HOME", home)
        .env("AINB_HANGAR_DAEMON_BIN", daemon)
        // The spawned daemon claims for this runtime + self-registers; disable
        // the claim loop so the child stays a quiet idle process for the test.
        .env("HANGAR_DAEMON_RUNTIME_ID", "rt-lifecycle")
        .env("HANGAR_DAEMON_DISABLE_CLAIM", "1")
        .output()
        .expect("spawn ainb");
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    (out.status.success(), format!("{stdout}{stderr}"))
}

/// Read the daemon PID file under `<home>/hangar/daemon.pid`, if present.
fn read_pid(home: &Path) -> Option<u32> {
    let path = home.join("hangar").join("daemon.pid");
    let text = std::fs::read_to_string(path).ok()?;
    text.trim().parse().ok()
}

/// Is `pid` still a live process? `kill(pid, 0)` succeeds iff it exists.
fn pid_alive(pid: u32) -> bool {
    use nix::sys::signal::kill;
    use nix::unistd::Pid;
    matches!(kill(Pid::from_raw(pid as i32), None), Ok(()))
}

/// Poll `cond` until true or the deadline elapses. Returns whether it became true.
fn wait_until(timeout: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    cond()
}

#[test]
fn daemon_start_status_stop_round_trip() {
    let Some(daemon) = daemon_bin() else {
        eprintln!("SKIP: ainb-hangar-daemon binary not built beside ainb");
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();

    // Status before start: stopped.
    let (ok, out) = run(home, &daemon, &["hangar", "daemon", "status"]);
    assert!(ok, "status (stopped) should exit 0; out={out}");
    assert!(
        out.contains("stopped"),
        "status before start must report stopped:\n{out}"
    );

    // Start: spawns the daemon as a background child + writes its pid.
    let (ok, out) = run(home, &daemon, &["hangar", "daemon", "start"]);
    assert!(ok, "daemon start should exit 0; out={out}");

    // The pid file landed and points at a live process.
    let pid = read_pid(home).expect("daemon start wrote a pid file");
    assert!(
        wait_until(Duration::from_secs(10), || pid_alive(pid)),
        "the started daemon pid {pid} must be alive"
    );

    // The socket binds shortly after boot; status reports running.
    let socket = home.join("hangar.sock");
    assert!(
        wait_until(Duration::from_secs(10), || socket.exists()),
        "the daemon must bind its control socket"
    );
    let (ok, out) = run(home, &daemon, &["hangar", "daemon", "status"]);
    assert!(ok, "status (running) should exit 0; out={out}");
    assert!(
        out.contains("running"),
        "status after start must report running:\n{out}"
    );

    // Stop: kills the EXACT recorded pid and removes the file.
    let (ok, out) = run(home, &daemon, &["hangar", "daemon", "stop"]);
    assert!(ok, "daemon stop should exit 0; out={out}");

    // Bounded poll: the process is gone and the pid file is removed.
    assert!(
        wait_until(Duration::from_secs(10), || !pid_alive(pid)),
        "the stopped daemon pid {pid} must die"
    );
    assert!(
        wait_until(Duration::from_secs(5), || read_pid(home).is_none()),
        "stop must remove the pid file"
    );

    // Status after stop: stopped again.
    let (ok, out) = run(home, &daemon, &["hangar", "daemon", "status"]);
    assert!(ok, "status (stopped) should exit 0; out={out}");
    assert!(
        out.contains("stopped"),
        "status after stop must report stopped:\n{out}"
    );

    // Defence-in-depth: never leave an orphaned child even if an assert above
    // changes. The pid is the one the CLI recorded — we only ever signal it.
    if pid_alive(pid) {
        use nix::sys::signal::{Signal, kill};
        use nix::unistd::Pid;
        let _ = kill(Pid::from_raw(pid as i32), Signal::SIGKILL);
    }
}
