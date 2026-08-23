//! Tripwire: the Daemons screen lists every managed daemon, never sits on a
//! `collecting…` placeholder, and lets you act on any row.
//!
//! Pins three user-visible regressions at once, by driving the real `ainb`
//! binary in a tmux pane:
//!
//! 1. The MCP pool, the Hangar daemon and the Headroom proxy have rows. They
//!    were real processes with no `DaemonKind`, visible only as ad-hoc lines in
//!    a separate System services panel.
//! 2. Nothing on the screen reads `collecting…`. The old panel's separate async
//!    fetch had an unbounded socket connect and a one-outstanding guard that,
//!    once wedged, rejected every later refresh — so the panel sat on that
//!    placeholder forever and `r` could not rescue it.
//! 3. `↑`/`↓` moves a row selection and `Enter` opens an action menu offering
//!    start, restart and stop. A stopped ATC previously had no way back up.
//!
//! The screen opens with `d` from the home menu (the sidebar's own hint).
//!
//! Skips gracefully if `tmux` isn't on `$PATH`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

fn ainb_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ainb"))
}

fn tmux_available() -> bool {
    Command::new("tmux")
        .arg("-V")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// An isolated `$HOME` with onboarding done and the hooks dialog dismissed, so
/// the first keystroke reaches the home screen rather than a modal.
fn seed_isolated_home(home: &Path) {
    let base = home.join(".agents-in-a-box");
    let cfg = base.join("config");
    fs::create_dir_all(&cfg).expect("create isolated config dir");

    let onboarding = format!(
        r#"completed = true
completed_at = "2026-05-27T00:00:00+00:00"
version = "{ver}"
skipped_dependencies = []
git_directories = []
"#,
        ver = env!("CARGO_PKG_VERSION"),
    );
    fs::write(cfg.join("onboarding.toml"), onboarding).expect("seed onboarding.toml");

    let install_record = r#"{"agents":[],"hook_script":"","claude_plugin_dir":null,"codex_hooks_json":null,"plugin_version":null,"prompt_dismissed":true}"#;
    fs::write(base.join("install.json"), install_record).expect("seed install.json");
}

fn capture_pane(session: &str) -> String {
    let out = Command::new("tmux")
        .args(["capture-pane", "-t", session, "-p"])
        .output()
        .expect("tmux capture-pane");
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn poll_capture<F>(session: &str, deadline: Instant, mut ok: F) -> Option<String>
where
    F: FnMut(&str) -> bool,
{
    while Instant::now() < deadline {
        let cap = capture_pane(session);
        if ok(&cap) {
            return Some(cap);
        }
        thread::sleep(Duration::from_millis(500));
    }
    None
}

fn send_key(session: &str, key: &str) {
    let status = Command::new("tmux")
        .args(["send-keys", "-t", session, key])
        .status()
        .expect("tmux send-keys");
    assert!(status.success(), "tmux send-keys {key:?} failed");
}

fn poll_capture_resending<F>(
    session: &str,
    key: &str,
    deadline: Instant,
    mut ok: F,
) -> Option<String>
where
    F: FnMut(&str) -> bool,
{
    while Instant::now() < deadline {
        send_key(session, key);
        thread::sleep(Duration::from_millis(500));
        let cap = capture_pane(session);
        if ok(&cap) {
            return Some(cap);
        }
    }
    None
}

/// Kill only this test's own session, by exact name.
fn kill_session(session: &str) {
    let _ = Command::new("tmux").args(["kill-session", "-t", session]).status();
}

#[test]
fn daemons_screen_lists_every_daemon_and_offers_per_row_actions() {
    if !tmux_available() {
        eprintln!("SKIP: tmux not available");
        return;
    }

    let home_tmp = tempfile::tempdir().expect("home tempdir");
    seed_isolated_home(home_tmp.path());

    let session = format!("tripwire-daemon-actions-{}", std::process::id());
    let ainb = ainb_bin();

    let status = Command::new("tmux")
        .args(["new-session", "-d", "-s", &session, "-x", "180", "-y", "50"])
        .status()
        .expect("tmux new-session");
    assert!(status.success(), "tmux new-session failed");

    let cmd = format!(
        "HOME={home} AINB_HOME={home}/.agents-in-a-box exec {bin} tui",
        home = home_tmp.path().display(),
        bin = ainb.display()
    );
    Command::new("tmux")
        .args(["send-keys", "-t", &session, &cmd, "Enter"])
        .status()
        .expect("send launch cmd");

    let home_deadline = Instant::now() + Duration::from_secs(90);
    if poll_capture(&session, home_deadline, |c| {
        c.contains("Stats") && c.contains("[i]")
    })
    .is_none()
    {
        let last = capture_pane(&session);
        kill_session(&session);
        panic!("HomeScreen never rendered; last capture:\n---\n{last}\n---");
    }

    // (1) Every daemon has a row — including the three that used to exist only
    // as lines in the System services panel.
    let post = poll_capture_resending(
        &session,
        "d",
        Instant::now() + Duration::from_secs(30),
        |c| c.contains("runtime health") && c.contains("mcp pool"),
    );
    let Some(post) = post else {
        let last = capture_pane(&session);
        kill_session(&session);
        panic!("Daemons screen never listed the mcp pool row; last capture:\n---\n{last}\n---");
    };
    for row in [
        "phone bridge",
        "notifyd",
        "approve broker",
        "ATC",
        "fleet daemon",
        "mcp pool",
        "hangar daemon",
        "headroom proxy",
    ] {
        assert!(post.contains(row), "daemon row missing: {row}\n{post}");
    }

    // (2) Nothing sits on a collecting placeholder. This is the whole of bug 2:
    // the panel that showed it is gone, and the probes behind the table are
    // bounded so a wedged socket degrades a row instead of the screen.
    assert!(
        !post.contains("collecting"),
        "the screen must never sit on a collecting placeholder:\n{post}"
    );
    // The footer has to advertise the actions, or they are undiscoverable.
    assert!(
        post.contains("Enter") && post.contains("select"),
        "footer must advertise selection + Enter:\n{post}"
    );

    // (3) Selection moves and Enter opens the action menu with all three verbs.
    send_key(&session, "Down");
    thread::sleep(Duration::from_millis(400));
    let menu = poll_capture_resending(
        &session,
        "Enter",
        Instant::now() + Duration::from_secs(20),
        |c| c.contains("restart") && c.contains("Enter run"),
    );
    let Some(menu) = menu else {
        let last = capture_pane(&session);
        kill_session(&session);
        panic!("Enter never opened the action menu; last capture:\n---\n{last}\n---");
    };
    for verb in ["start", "restart", "stop"] {
        assert!(menu.contains(verb), "action menu missing {verb}:\n{menu}");
    }

    // Esc closes the menu but must NOT leave the screen — the layered unwind.
    send_key(&session, "Escape");
    thread::sleep(Duration::from_millis(600));
    let after_esc = capture_pane(&session);
    assert!(
        after_esc.contains("runtime health"),
        "Esc must close the menu, not the screen:\n{after_esc}"
    );
    assert!(
        !after_esc.contains("Enter run"),
        "Esc must actually close the menu:\n{after_esc}"
    );

    kill_session(&session);
}
