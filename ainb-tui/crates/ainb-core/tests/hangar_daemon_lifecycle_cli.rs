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

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Once;
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

/// Reap only orphaned `ainb hangar daemon run` children from an interrupted
/// prior invocation of this exact test binary. Selection requires both the
/// exact command and ppid=1; every signal addresses one PID only.
fn reap_orphaned_test_daemons() {
    static REAPED: Once = Once::new();
    REAPED.call_once(|| {
        let Ok(output) = Command::new("ps")
            .args(["-Ao", "pid=,ppid=,command="])
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
        else {
            return;
        };
        if !output.status.success() {
            return;
        }
        let expected = format!("{} hangar daemon run", ainb_bin().display());
        let Ok(table) = String::from_utf8(output.stdout) else {
            return;
        };
        for pid in table.lines().filter_map(|line| {
            let mut fields = line.split_whitespace();
            let pid = fields.next()?.parse::<u32>().ok()?;
            let ppid = fields.next()?.parse::<u32>().ok()?;
            let command = fields.collect::<Vec<_>>().join(" ");
            (ppid == 1 && command == expected).then_some(pid)
        }) {
            let _ = Command::new("kill").arg(pid.to_string()).status();
        }
    });
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
        .env("AINB_CODEX_MANAGED", "0")
        .env("HANGAR_TEST_PARENT_PID", std::process::id().to_string())
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

/// Kill this exact PID if an assertion aborts the watchdog proof early.
struct ExactPidCleanup(u32);

impl Drop for ExactPidCleanup {
    fn drop(&mut self) {
        if pid_alive(self.0) {
            use nix::sys::signal::{Signal, kill};
            use nix::unistd::Pid;
            let _ = kill(Pid::from_raw(self.0 as i32), Signal::SIGKILL);
        }
    }
}

fn kill_and_wait(child: &mut Child) {
    child.kill().expect("SIGKILL exact test parent");
    child.wait().expect("reap exact test parent");
}

#[test]
fn daemon_start_status_stop_round_trip() {
    reap_orphaned_test_daemons();
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

#[test]
fn daemon_exits_when_its_test_parent_is_sigkilled() {
    reap_orphaned_test_daemons();
    let home = tempfile::tempdir().expect("isolated hangar home");
    let mut parent = Command::new("sh")
        .args([
            "-c",
            "HANGAR_TEST_PARENT_PID=$$ AINB_CODEX_MANAGED=0 HANGAR_DAEMON_DISABLE_CLAIM=1 \\
             AINB_HANGAR_HOME=\"$2\" HOME=\"$2\" \"$1\" hangar daemon run >/dev/null 2>&1 &\n             printf '%s\\n' \"$!\"\n             wait \"$!\"",
            "sh",
        ])
        .arg(ainb_bin())
        .arg(home.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn test parent");
    let stdout = parent.stdout.take().expect("test parent stdout");
    let mut line = String::new();
    BufReader::new(stdout).read_line(&mut line).expect("read daemon pid");
    let daemon_pid = line.trim().parse::<u32>().expect("daemon pid");
    let _cleanup = ExactPidCleanup(daemon_pid);

    assert!(
        wait_until(Duration::from_secs(10), || home
            .path()
            .join("hangar.sock")
            .exists()),
        "daemon must reach its run loop before the parent dies"
    );
    kill_and_wait(&mut parent);
    assert!(
        wait_until(Duration::from_secs(5), || !pid_alive(daemon_pid)),
        "daemon {daemon_pid} survived its SIGKILLed test parent"
    );
}
