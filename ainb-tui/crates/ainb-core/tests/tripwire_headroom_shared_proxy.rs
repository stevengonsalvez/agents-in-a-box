// ABOUTME: Tripwire for issue #988: quitting one of two TUIs must not stop the
// shared headroom proxy the other TUI still uses, and quitting the last TUI
// must stop it.
//
// Drives two REAL `ainb tui` processes in detached tmux sessions against one
// isolated home. The "proxy" is a `sleep` child of this test whose pid is
// written to `proxy.pid`, which is exactly what `headroom::stop()` reads and
// signals, so the assertion is on a real process receiving (or not receiving)
// SIGTERM. This is G6 checkpoint step 5, automated.

use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::thread;
use std::time::{Duration, Instant};

fn ainb_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ainb"))
}

fn tmux_available() -> bool {
    Command::new("tmux").arg("-V").output().is_ok_and(|o| o.status.success())
}

/// Skips the first-run wizard and the notifyd install modal, which would
/// otherwise sit over the home screen.
fn seed_isolated_home(home: &Path) {
    let cfg = home.join(".agents-in-a-box").join("config");
    std::fs::create_dir_all(&cfg).expect("create isolated config dir");
    let onboarding = format!(
        r#"completed = true
completed_at = "2026-05-11T00:00:00+00:00"
version = "{ver}"
skipped_dependencies = []
git_directories = []
"#,
        ver = env!("CARGO_PKG_VERSION"),
    );
    std::fs::write(cfg.join("onboarding.toml"), onboarding).expect("seed onboarding.toml");

    let install = concat!(
        r#"{"agents":[],"hook_script":"","claude_plugin_dir":null,"#,
        r#""codex_hooks_json":null,"copilot_hooks_json":null,"#,
        r#""antigravity_hooks_json":null,"plugin_version":null,"prompt_dismissed":true}"#,
    );
    for base in [home.to_path_buf(), home.join(".agents-in-a-box")] {
        std::fs::create_dir_all(&base).expect("create notifyd base");
        std::fs::write(base.join("install.json"), install).expect("seed install.json");
    }
}

/// Kills by EXACT session name only. Never kill-server, never a wildcard:
/// other agents' sessions live in this tmux server.
struct TmuxGuard(String);

impl Drop for TmuxGuard {
    fn drop(&mut self) {
        let _ = Command::new("tmux").args(["kill-session", "-t", &self.0]).output();
    }
}

/// Reaps the fake proxy if an assertion fails before the TUI does.
struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

impl ChildGuard {
    /// `kill(pid, 0)` still succeeds on a zombie, so ask `wait` instead.
    fn is_running(&mut self) -> bool {
        self.0.try_wait().expect("poll fake proxy").is_none()
    }
}

fn session_exists(session: &str) -> bool {
    Command::new("tmux")
        .args(["has-session", "-t", session])
        .output()
        .is_ok_and(|o| o.status.success())
}

fn capture_pane(session: &str) -> String {
    let out = Command::new("tmux")
        .args(["capture-pane", "-t", session, "-p"])
        .output()
        .expect("tmux capture-pane");
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn wait_until(deadline: Duration, mut ok: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + deadline;
    while Instant::now() < end {
        if ok() {
            return true;
        }
        thread::sleep(Duration::from_millis(250));
    }
    ok()
}

fn start_tui(home: &Path, session: &str) -> TmuxGuard {
    let _ = Command::new("tmux").args(["kill-session", "-t", session]).output();
    assert!(
        Command::new("tmux")
            .args(["new-session", "-d", "-s", session, "-x", "160", "-y", "45"])
            .status()
            .is_ok_and(|s| s.success()),
        "failed to create tmux session {session}"
    );
    let guard = TmuxGuard(session.to_string());
    // `exec`, so the tmux session ends exactly when the TUI process exits.
    let cmd = format!(
        "HOME={home} AINB_HOME={home} AINB_DISABLE_PLUGINS=1 exec {bin} tui",
        home = home.display(),
        bin = ainb_bin().display(),
    );
    Command::new("tmux")
        .args(["send-keys", "-t", session, &cmd, "Enter"])
        .status()
        .expect("launch tui");
    assert!(
        wait_until(Duration::from_mins(1), || capture_pane(session).contains("Agents in a Box")),
        "TUI in {session} never drew its home screen:\n{}",
        capture_pane(session)
    );
    guard
}

fn quit_tui(session: &str) {
    Command::new("tmux")
        .args(["send-keys", "-t", session, "C-c"])
        .status()
        .expect("send ctrl+c");
    assert!(
        wait_until(Duration::from_secs(30), || !session_exists(session)),
        "TUI in {session} did not exit on ctrl+c:\n{}",
        capture_pane(session)
    );
}

#[test]
fn quitting_one_tui_keeps_the_shared_proxy_and_the_last_quit_stops_it() {
    if !tmux_available() {
        eprintln!("SKIP: tmux unavailable");
        return;
    }

    let home = tempfile::tempdir().expect("tempdir");
    let home_path = home.path().canonicalize().expect("canonicalize home");
    seed_isolated_home(&home_path);

    let headroom_dir = home_path.join(".agents-in-a-box").join("headroom");
    std::fs::create_dir_all(&headroom_dir).expect("create headroom dir");
    let pid_path = headroom_dir.join("proxy.pid");
    let mut proxy = ChildGuard(
        Command::new("sleep")
            .arg("600")
            .spawn()
            .expect("spawn fake proxy"),
    );
    std::fs::write(&pid_path, proxy.0.id().to_string()).expect("write proxy.pid");

    let tag = std::process::id();
    let session_a = format!("tripwire-headroom-a-{tag}");
    let session_b = format!("tripwire-headroom-b-{tag}");
    let _guard_a = start_tui(&home_path, &session_a);
    let _guard_b = start_tui(&home_path, &session_b);

    quit_tui(&session_b);
    // Give a wrongly-sent SIGTERM time to land before asserting it did not.
    thread::sleep(Duration::from_secs(2));
    assert!(
        proxy.is_running(),
        "quitting TUI B stopped the proxy while TUI A still runs"
    );
    assert!(
        pid_path.exists(),
        "quitting TUI B removed proxy.pid while TUI A still runs"
    );

    quit_tui(&session_a);
    assert!(
        wait_until(Duration::from_secs(10), || !proxy.is_running()),
        "quitting the last TUI must stop the proxy"
    );
    assert!(!pid_path.exists(), "the last quit must remove proxy.pid");
}
