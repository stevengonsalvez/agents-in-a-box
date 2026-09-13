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

use std::cell::Cell;
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
fn shell_quote_path_preserves_apostrophes() {
    assert_eq!(
        shell_quote_path(Path::new("/tmp/ainb's tui")),
        "'/tmp/ainb'\"'\"'s tui'"
    );
}

/// Quote a UTF-8 path as one POSIX shell word for tmux's shell pane.
fn shell_quote_path(path: &Path) -> String {
    let path = path.to_str().expect("test launch path must be valid UTF-8");
    format!("'{}'", path.replace('\'', r#"'"'"'"#))
}

/// Return the staged plugin root only when the real Hangar subprocess exists.
/// The acceptance test requires this artifact instead of exercising a mock.
fn hangar_plugin_root() -> Option<PathBuf> {
    let bin = ainb_bin();
    let mut dir = bin.parent()?;
    for _ in 0..6 {
        let candidate = dir.join("dist").join("plugins");
        let hangar = candidate.join("hangar-tui");
        if hangar.join("hangar-tui").is_file() && hangar.join("manifest.toml").is_file() {
            return Some(candidate);
        }
        dir = dir.parent()?;
    }
    None
}

fn tmux_available() -> bool {
    Command::new("tmux")
        .arg("-V")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

/// Skip TUI onboarding and modal prompts, then pre-ack Hangar's first-run
/// warning. The daemon binds its authenticated Unix endpoint inside this same
/// isolated home, so every process in the acceptance run is test-owned.
fn seed_tui_home(home: &Path) {
    let base = home.join(".agents-in-a-box");
    let config = base.join("config");
    std::fs::create_dir_all(&config).expect("create isolated TUI config");
    let onboarding = format!(
        r#"completed = true
completed_at = "2026-09-11T00:00:00+00:00"
version = "{version}"
skipped_dependencies = []
git_directories = []
"#,
        version = env!("CARGO_PKG_VERSION"),
    );
    std::fs::write(config.join("onboarding.toml"), onboarding).expect("seed onboarding");
    std::fs::write(
        base.join("install.json"),
        r#"{"agents":[],"hook_script":"","claude_plugin_dir":null,"codex_hooks_json":null,"plugin_version":null,"prompt_dismissed":true}"#,
    )
    .expect("dismiss hooks prompt");

    let hangar_state = home.join("hangar").join("state.toml");
    std::fs::create_dir_all(hangar_state.parent().expect("hangar state parent"))
        .expect("create isolated Hangar state dir");
    std::fs::write(hangar_state, "warnings_ack = [\"first_run\"]\n")
        .expect("ack Hangar first-run warning");
}

fn capture_pane(session: &str) -> String {
    let output = Command::new("tmux")
        .args(["capture-pane", "-t", session, "-p"])
        .output()
        .expect("capture owned tmux pane");
    String::from_utf8_lossy(&output.stdout).to_string()
}

fn poll_capture<F>(session: &str, timeout: Duration, mut ready: F) -> Option<String>
where
    F: FnMut(&str) -> bool,
{
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        let capture = capture_pane(session);
        if ready(&capture) {
            return Some(capture);
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    None
}

/// Press the Hangar launch key and confirm the TUI acted on it.
///
/// `g` lazy-spawns the staged hangar-tui subprocess, which authenticates as
/// `surface.kind=tui` over the production daemon socket. It used to be sent
/// once and hoped for, with a comment warning not to re-send because a plugin
/// screen may own `g` itself.
///
/// That warning is real but narrower than it looks: the hazard needs a plugin
/// screen to be UP, and while the HomeScreen chrome is still on the pane no
/// plugin screen is. So this re-sends only while the app is demonstrably still
/// on the HomeScreen, which is precisely the state in which the previous key
/// cannot have been consumed by anything that would mind a second one.
///
/// This is delivery-until-observed, not a retry loop over a flaky assertion.
/// A `send-keys` that tmux accepted can still be dropped by an application that
/// has not finished taking over the terminal, and the symptom is exactly what
/// #953 reports: the HomeScreen renders, the key reports success, and the
/// Hangar screen never appears, with the captured frame showing an ordinary
/// HomeScreen and the `[g]` item sitting unactivated in the sidebar.
fn press_hangar_launch_key(session: &str) {
    // `Stats` plus `[i]` is the HomeScreen chrome the caller just waited for,
    // and the same pair the post-launch assertion requires to be GONE.
    let on_home = |capture: &str| capture.contains("Stats") && capture.contains("[i]");
    for _ in 0..3 {
        send_key(session, "g");
        if poll_capture(session, Duration::from_secs(5), |capture| !on_home(capture)).is_some() {
            return;
        }
    }
    // Out of attempts. The caller's own poll produces the diagnostic frame.
}

fn send_key(session: &str, key: &str) {
    let status = Command::new("tmux")
        .args(["send-keys", "-t", session, key])
        .status()
        .expect("send key to owned tmux session");
    assert!(status.success(), "tmux send-keys {key:?} failed");
}

/// Exact-name tmux cleanup remains armed through assertion panics. This test
/// never touches the tmux server or any session it did not create.
struct OwnedTmuxSession {
    name: String,
    shut_down: Cell<bool>,
}

impl OwnedTmuxSession {
    fn name(&self) -> &str {
        &self.name
    }

    fn shutdown(&self) {
        if self.shut_down.replace(true) {
            return;
        }
        let _ = Command::new("tmux")
            .args(["kill-session", "-t", &self.name])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
}

impl Drop for OwnedTmuxSession {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn launch_tui(home: &Path, plugin_root: &Path, daemon: &Path) -> OwnedTmuxSession {
    let session = format!("tripwire-hangar-tui-presence-{}", std::process::id());
    let ainb_home = home.join(".agents-in-a-box");
    dismiss_notify_install_prompt(home);
    let mut new_session = Command::new("tmux");
    new_session.args(["new-session", "-d", "-s", &session, "-x", "180", "-y", "50"]);
    for (key, value) in [
        ("HOME", home),
        ("AINB_HOME", ainb_home.as_path()),
        ("AINB_HANGAR_HOME", home),
        ("AINB_PLUGIN_ROOT", plugin_root),
        ("AINB_HANGAR_DAEMON_BIN", daemon),
    ] {
        new_session.arg("-e").arg(format!("{key}={}", value.display()));
    }
    for (key, value) in [
        // Tmux windows inherit their server environment. Empty values prevent
        // a server-owned kill or deny filter from changing this acceptance run.
        ("AINB_DISABLE_PLUGINS", ""),
        ("AINB_DISABLE_PLUGIN", ""),
        ("AINB_ONLY_PLUGINS", "hangar-tui"),
    ] {
        new_session.arg("-e").arg(format!("{key}={value}"));
    }
    // The environment reaches tmux directly. Its noninteractive shell receives
    // one quoted executable path, avoiding user shell startup prompts.
    let command = format!("exec {} tui", shell_quote_path(&ainb_bin()));
    let status = new_session.arg(&command).status().expect("launch TUI in owned tmux session");
    assert!(status.success(), "tmux new-session failed");

    OwnedTmuxSession {
        name: session,
        shut_down: Cell::new(false),
    }
}

/// Record the ainb-hooks prompt as already dismissed, before the TUI starts.
///
/// `maybe_prompt_notify_install` fires on startup whenever `install.json`
/// records no agents and no dismissal, which is exactly what a fresh fixture
/// `HOME` looks like. The modal then owns the keyboard, so the `g` that should
/// open the Hangar screen is swallowed and the test fails with "Hangar screen
/// chrome never rendered after its launch key" while the captured frame shows
/// the install prompt (#953).
///
/// Whether the modal wins that race depends on how fast the runner reaches the
/// first render, which is why it failed intermittently and only on CI. Seeding
/// the dismissal removes the race rather than out-waiting it: this test is
/// about daemon connection presence and has no opinion about hook installation.
///
/// `Paths::from_home` reads `AINB_HANGAR_HOME` first, which `launch_tui` points
/// at `home`, so the record belongs at `home/install.json`.
fn dismiss_notify_install_prompt(home: &Path) {
    std::fs::create_dir_all(home).expect("fixture home exists");
    std::fs::write(
        home.join("install.json"),
        serde_json::json!({
            "agents": [],
            "hook_script": "",
            "claude_plugin_dir": null,
            "codex_hooks_json": null,
            "prompt_dismissed": true,
        })
        .to_string(),
    )
    .expect("seed the dismissed notify-install prompt");
}

fn connections_json(home: &Path, daemon: &Path) -> serde_json::Value {
    let (ok, output) = run(
        home,
        daemon,
        &["--format", "json", "hangar", "connections", "list"],
    );
    assert!(ok, "connections list should succeed: {output}");
    serde_json::from_str(&output)
        .unwrap_or_else(|error| panic!("connections list must be JSON: {error}; output:\n{output}"))
}

fn tui_pid(connections: &serde_json::Value) -> Option<u32> {
    connections["connections"]
        .as_array()?
        .iter()
        .find(|row| row["surface"]["kind"].as_str() == Some("tui"))?["surface"]["pid"]
        .as_u64()
        .and_then(|pid| u32::try_from(pid).ok())
        .filter(|pid| *pid > 0)
}

fn wait_for_tui_pid(home: &Path, daemon: &Path, timeout: Duration) -> (u32, serde_json::Value) {
    let deadline = Instant::now() + timeout;
    let mut last = None;
    while Instant::now() < deadline {
        let connections = connections_json(home, daemon);
        if let Some(pid) = tui_pid(&connections).filter(|pid| pid_alive(*pid)) {
            return (pid, connections);
        }
        last = Some(connections);
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!(
        "real TUI plugin never stayed in connections list; last listing: {}",
        last.unwrap_or(serde_json::Value::Null)
    );
}

fn tui_stays_listed(home: &Path, daemon: &Path, pid: u32, duration: Duration) -> bool {
    let deadline = Instant::now() + duration;
    let mut observed = false;
    while Instant::now() < deadline {
        let listing = connections_json(home, daemon);
        if tui_pid(&listing) != Some(pid) || !pid_alive(pid) {
            return false;
        }
        observed = true;
        std::thread::sleep(Duration::from_millis(100));
    }
    observed
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
    let binary = std::fs::read_to_string(home.join("hangar").join("daemon.binary"))
        .expect("daemon start recorded its binary path");
    assert_eq!(binary.trim(), daemon.display().to_string());

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

/// S-B acceptance: a real Hangar plugin inside the real TUI registers its
/// authenticated TUI presence with the daemon, keeps that row live, then drops
/// it when this test shuts down its exact tmux session. Raw daemon-row tests
/// and in-process plugin mocks cannot prove this host, subprocess, socket, and
/// CLI-list path together.
#[test]
fn real_tui_presence_stays_listed_then_disappears_on_shutdown() {
    // This test needs three artifacts the general `Test` job does not build:
    // tmux, the staged `dist/plugins/hangar-tui`, and the sibling daemon
    // binary. It is the `hangar-e2e` job that provides them, and it is there
    // that this assertion actually runs. Hard-failing without them made the
    // `Test` job red on an absent build artifact rather than on behaviour,
    // which is the same "my environment lacks X" failure every other tripwire
    // skips on. Skipping is only safe because the coverage is not lost: a
    // `hangar-e2e` run with the plugin missing fails at its own staging step.
    if !tmux_available() {
        eprintln!("SKIP: tmux is not on PATH");
        return;
    }
    let Some(plugin_root) = hangar_plugin_root() else {
        eprintln!("SKIP: dist/plugins/hangar-tui is not staged (run scripts/build-plugins.sh)");
        return;
    };
    let Some(daemon) = daemon_bin() else {
        eprintln!("SKIP: sibling ainb-hangar-daemon binary is not built");
        return;
    };

    let home = tempfile::tempdir().expect("isolated Hangar home");
    seed_tui_home(home.path());

    // Start the production daemon through the production CLI. Its endpoint is
    // the test-owned Unix socket at `<home>/hangar.sock`; explicit HOME values
    // ensure no real user daemon, token, or socket can join this assertion.
    let (ok, output) = run(home.path(), &daemon, &["hangar", "daemon", "start"]);
    assert!(ok, "start real daemon should succeed: {output}");
    let daemon_pid = read_pid(home.path()).expect("daemon start wrote owned pid");
    let _daemon_cleanup = ExactPidCleanup(daemon_pid);
    assert!(
        wait_until(Duration::from_secs(10), || home
            .path()
            .join("hangar.sock")
            .exists()),
        "real daemon never bound the test-owned socket"
    );

    let tui = launch_tui(home.path(), &plugin_root, &daemon);
    let home_capture = poll_capture(tui.name(), Duration::from_secs(45), |capture| {
        capture.contains("Stats") && capture.contains("[i]")
    })
    .unwrap_or_else(|| {
        panic!(
            "TUI HomeScreen never rendered in owned session; last capture:\n{}",
            capture_pane(tui.name())
        )
    });
    assert!(
        !home_capture.contains("Control Center"),
        "Hangar content appeared before its launch key:\n{home_capture}"
    );
    // If the install prompt is up, it owns the keyboard and the `g` below is
    // swallowed. `dismiss_notify_install_prompt` is what keeps it down, and
    // failing HERE names the cause instead of surfacing 30 s later as "Hangar
    // screen chrome never rendered" with no hint why (#953).
    assert!(
        !home_capture.contains("Get notified when a session needs you?")
            && !home_capture.contains("Update notification hooks?"),
        "the ainb-hooks install prompt is holding focus and will swallow the \
         launch key; the seeded dismissal did not take:\n{home_capture}"
    );
    // Keyed on the TUI surface, never on a count of CLI rows (#953).
    //
    // Every `connections_json` call is itself a CLI connection, and the daemon
    // deregisters one when its socket closes, which it does asynchronously. So
    // the number of `cli` rows visible at any instant is a property of this
    // harness racing its own previous probe, not of the daemon: two were seen
    // 0.3 ms apart on CI. The invariant the test is actually about is that the
    // Hangar TUI has not connected before its launch key, which is the same
    // thing `tui_pid` keys on everywhere below.
    let empty_listing = connections_json(home.path(), &daemon);
    let non_observer_connections: Vec<_> = empty_listing["connections"]
        .as_array()
        .expect("connections list must contain an array")
        .iter()
        .filter(|connection| connection["surface"]["kind"].as_str() != Some("cli"))
        .collect();
    assert!(
        non_observer_connections.is_empty(),
        "registry must have zero non-CLI connections before the Hangar launch key: {empty_listing}"
    );
    assert!(
        tui_pid(&empty_listing).is_none(),
        "no TUI may be registered before the Hangar launch key: {empty_listing}"
    );

    press_hangar_launch_key(tui.name());
    let hangar_capture = poll_capture(tui.name(), Duration::from_secs(30), |capture| {
        capture.contains("[1]Issues") && capture.contains("[B]Boards")
    })
    .unwrap_or_else(|| {
        panic!(
            "Hangar screen chrome never rendered after its launch key; last capture:\n{}",
            capture_pane(tui.name())
        )
    });
    assert!(
        !hangar_capture.contains("Stats"),
        "HomeScreen remained visible after Hangar launch key:\n{hangar_capture}"
    );
    let (plugin_pid, first_listing) =
        wait_for_tui_pid(home.path(), &daemon, Duration::from_secs(30));
    assert!(
        tui_pid(&first_listing) == Some(plugin_pid),
        "connections list must expose the live TUI row with its process pid: {first_listing}"
    );

    // Poll through a real hold period so this cannot pass on a transient
    // registration event. The plugin PID remains alive while its parent TUI
    // session remains connected.
    let stable = tui_stays_listed(home.path(), &daemon, plugin_pid, Duration::from_millis(750));
    assert!(
        stable,
        "TUI pid {plugin_pid} did not remain listed while its tmux session lived"
    );

    // Exact session only. Drop keeps this same cleanup armed if a later
    // assertion changes, and no broad tmux/process kill is ever used.
    tui.shutdown();
    assert!(
        wait_until(Duration::from_secs(15), || {
            let listing = connections_json(home.path(), &daemon);
            tui_pid(&listing).is_none()
        }),
        "TUI row for pid {plugin_pid} remained after exact tmux shutdown"
    );
    assert!(
        wait_until(Duration::from_secs(5), || !pid_alive(plugin_pid)),
        "TUI plugin pid {plugin_pid} survived its owned tmux shutdown"
    );

    let (ok, output) = run(home.path(), &daemon, &["hangar", "daemon", "stop"]);
    assert!(ok, "stop real daemon should succeed: {output}");
    assert!(
        wait_until(Duration::from_secs(10), || !pid_alive(daemon_pid)),
        "owned daemon pid {daemon_pid} survived test cleanup"
    );
}

/// Issue #784's guard binds a daemon to the process that launched it when the
/// hangar home is ephemeral. That is only ever correct for a launcher which
/// outlives the daemon (the TUI). `ainb hangar daemon start` exits about a
/// second after the spawn, and its whole contract is to leave a daemon behind:
/// arming there would stand the new daemon down and still report success.
///
/// Deliberately does NOT set `HANGAR_TEST_PARENT_PID`, unlike `run`: with a
/// parent already declared the guard declines anyway, which would hide the
/// regression this test exists to catch.
#[test]
fn daemon_start_under_a_temp_home_outlives_the_cli_invocation() {
    reap_orphaned_test_daemons();
    let Some(daemon) = daemon_bin() else {
        eprintln!("SKIP: ainb-hangar-daemon binary not built beside ainb");
        return;
    };
    // Under `$TMPDIR`, so the home IS ephemeral by the guard's definition.
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();

    let out = Command::new(ainb_bin())
        .args(["hangar", "daemon", "start"])
        .env("AINB_HANGAR_HOME", home)
        .env("HOME", home)
        .env("AINB_HANGAR_DAEMON_BIN", &daemon)
        .env("HANGAR_DAEMON_RUNTIME_ID", "rt-lifecycle-outlives")
        .env("HANGAR_DAEMON_DISABLE_CLAIM", "1")
        .env("AINB_CODEX_MANAGED", "0")
        .env_remove("HANGAR_TEST_PARENT_PID")
        .env_remove("AINB_HANGAR_PARENT_PID")
        .output()
        .expect("spawn ainb");
    assert!(
        out.status.success(),
        "daemon start should exit 0; out={}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    // Cleanup BEFORE the unwrap: this daemon is deliberately spawned with no
    // parent binding, and `reap_orphaned_test_daemons` cannot find it (it runs
    // the sibling daemon binary, not `<ainb> hangar daemon run`), so a panic
    // between here and the stop below would leak it permanently.
    let pid = read_pid(home);
    let _cleanup = pid.map(ExactPidCleanup);
    let pid = pid.expect("daemon start wrote a pid file");
    assert!(
        wait_until(Duration::from_secs(10), || home
            .join("hangar.sock")
            .exists()),
        "the started daemon must reach its run loop"
    );

    // `ainb` has exited by now (`output()` waited for it). A daemon wrongly
    // bound to it SIGINTs itself on the watchdog's next tick, ~1s later.
    std::thread::sleep(Duration::from_secs(3));
    assert!(
        pid_alive(pid),
        "daemon {pid} must outlive the `ainb hangar daemon start` that spawned it"
    );

    let (ok, out) = run(home, &daemon, &["hangar", "daemon", "stop"]);
    assert!(ok, "daemon stop should exit 0; out={out}");
    assert!(
        wait_until(Duration::from_secs(10), || !pid_alive(pid)),
        "the stopped daemon pid {pid} must die"
    );
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
/// Read the ownership lock — the record that decides who owns the home.
fn read_lock_pid(home: &Path) -> Option<u32> {
    std::fs::read_to_string(home.join("hangar").join("daemon.lock"))
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// A second `start` is a no-op that reports the incumbent, and above all leaves
/// no second daemon behind — the failure mode that reached 69 on one machine.
#[test]
fn a_second_start_reports_the_incumbent_and_spawns_nothing() {
    let Some(daemon) = daemon_bin() else {
        eprintln!("SKIP: ainb-hangar-daemon binary not built beside ainb");
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();

    let (ok, out) = run(home, &daemon, &["hangar", "daemon", "start"]);
    assert!(ok, "daemon start should exit 0; out={out}");
    let pid = read_pid(home).expect("daemon start wrote a pid file");
    assert!(
        wait_until(Duration::from_secs(10), || home
            .join("hangar.sock")
            .exists()),
        "the daemon must bind its control socket"
    );

    let (ok, out) = run(home, &daemon, &["hangar", "daemon", "start"]);
    assert!(ok, "a second start should exit 0; out={out}");
    assert!(
        out.contains("already running"),
        "a second start must report the incumbent:\n{out}"
    );
    assert_eq!(
        read_pid(home),
        Some(pid),
        "a second start must not re-point the pid file"
    );
    assert_eq!(
        read_lock_pid(home),
        Some(pid),
        "the incumbent must still own the home"
    );

    let (ok, out) = run(home, &daemon, &["hangar", "daemon", "stop"]);
    assert!(ok, "cleanup stop should exit 0; out={out}");
    if pid_alive(pid) {
        use nix::sys::signal::{Signal, kill};
        use nix::unistd::Pid;
        let _ = kill(Pid::from_raw(pid as i32), Signal::SIGKILL);
    }
}

/// `restart` must leave exactly one daemon: a new one, and the old one gone.
///
/// The regression this guards is self-inflicted and subtle. Shutdown is now
/// graceful, so it takes time; a restart that hands off while the outgoing
/// daemon still holds the home gets "already running" from `start`, returns —
/// and when that daemon finishes dying the home is left with NOTHING.
#[test]
fn restart_leaves_exactly_one_daemon_running() {
    let Some(daemon) = daemon_bin() else {
        eprintln!("SKIP: ainb-hangar-daemon binary not built beside ainb");
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();

    let (ok, out) = run(home, &daemon, &["hangar", "daemon", "start"]);
    assert!(ok, "daemon start should exit 0; out={out}");
    let first = read_pid(home).expect("daemon start wrote a pid file");
    assert!(
        wait_until(Duration::from_secs(10), || home
            .join("hangar.sock")
            .exists()),
        "the daemon must bind its control socket"
    );

    let (ok, out) = run(home, &daemon, &["hangar", "daemon", "restart"]);
    assert!(ok, "daemon restart should exit 0; out={out}");

    let second = read_pid(home).expect("restart recorded a pid");
    assert_ne!(second, first, "restart should replace the daemon");
    assert!(
        !pid_alive(first),
        "the old daemon must be gone after restart"
    );
    assert!(
        wait_until(Duration::from_secs(10), || pid_alive(second)),
        "restart must leave a live daemon, not an empty home"
    );
    assert_eq!(
        read_lock_pid(home),
        Some(second),
        "the lock must name the surviving daemon"
    );

    let (ok, out) = run(home, &daemon, &["hangar", "daemon", "stop"]);
    assert!(ok, "cleanup stop should exit 0; out={out}");
    if pid_alive(second) {
        use nix::sys::signal::{Signal, kill};
        use nix::unistd::Pid;
        let _ = kill(Pid::from_raw(second as i32), Signal::SIGKILL);
    }
}
