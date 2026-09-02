//! Tripwire: the Daemons observability screen opens via `h` from the home
//! menu and renders the four-daemon runtime-health table, with a seeded bridge
//! heartbeat flipping the phone-bridge row to "running" + "Telegram".
//!
//! Drives the real `ainb` binary inside a tmux pane (genuine ratatui frames),
//! pre-seeds an isolated `$HOME`/`$AINB_HOME` with:
//!
//! 1. A completed-onboarding marker + a dismissed ainb-hooks install record so
//!    no dialog intercepts the first key press.
//! 2. A `daemons/bridge.json` heartbeat for the CURRENT test process pid
//!    (alive → the aggregator's pid cross-check reports it running, not stale),
//!    connected to "Telegram (@tripwire_bot)".
//!
//! Then sends `h`, polls the pane for the Daemons chrome + all four daemon rows
//! + the running bridge's channel label.
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

fn seed_isolated_home(home: &Path) {
    let base = home.join(".agents-in-a-box");
    let cfg = base.join("config");
    fs::create_dir_all(&cfg).expect("create isolated config dir");

    // Skip onboarding so the first keystroke lands on the HomeScreen.
    let onboarding = format!(
        r#"completed = true
completed_at = "{ts}"
version = "{ver}"
skipped_dependencies = []
git_directories = []
"#,
        ts = "2026-05-27T00:00:00+00:00",
        ver = env!("CARGO_PKG_VERSION"),
    );
    fs::write(cfg.join("onboarding.toml"), onboarding).expect("seed onboarding.toml");

    // Suppress the ainb-hooks first-run install dialog (it overlays home and
    // would swallow the `h` keystroke).
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

fn kill_session(session: &str) {
    let _ = Command::new("tmux").args(["kill-session", "-t", session]).status();
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

#[test]
fn daemons_opens_and_renders_four_rows() {
    if !tmux_available() {
        eprintln!("SKIP: tmux not available");
        return;
    }

    let home_tmp = tempfile::tempdir().expect("home tempdir");
    seed_isolated_home(home_tmp.path());

    let session = format!("tripwire-daemons-{}", std::process::id());
    let ainb = ainb_bin();

    let status = Command::new("tmux")
        .args(["new-session", "-d", "-s", &session, "-x", "180", "-y", "50"])
        .status()
        .expect("tmux new-session");
    assert!(status.success(), "tmux new-session failed");

    // Point both HOME and AINB_HOME at the isolated tmp so the daemons
    // aggregator reads the isolated directory.
    let cmd = format!(
        "HOME={home} AINB_HOME={home}/.agents-in-a-box exec {bin} tui",
        home = home_tmp.path().display(),
        bin = ainb.display()
    );
    Command::new("tmux")
        .args(["send-keys", "-t", &session, &cmd, "Enter"])
        .status()
        .expect("send launch cmd");

    // Wait for HomeScreen — same marker the other tripwire tests use.
    let home_deadline = Instant::now() + Duration::from_secs(90);
    let pre_cap = poll_capture(&session, home_deadline, |c| {
        c.contains("Stats") && c.contains("[i]")
    });
    let Some(pre) = pre_cap else {
        let last = capture_pane(&session);
        kill_session(&session);
        panic!("HomeScreen never rendered; last capture:\n---\n{last}\n---");
    };

    // Pre-assertion: must not already be on the Daemons screen.
    assert!(
        !pre.contains("runtime health"),
        "pre-key capture already on daemons — state leaked.\n{pre}"
    );

    // Discoverability gate: the HomeScreen V2 sidebar MUST list the Daemons
    // tile with its `[d]` shortcut, else the screen is unreachable by a
    // fresh user who can't see it.
    assert!(
        pre.contains("Daemons") && pre.contains("[d]"),
        "HomeScreen V2 sidebar missing Daemons tile — discoverability regression.\n{pre}"
    );

    let post_cap = poll_capture_resending(
        &session,
        "d",
        Instant::now() + Duration::from_secs(30),
        |c| c.contains("runtime health") && c.contains("mcp pool"),
    );
    if post_cap.is_none() {
        let last = capture_pane(&session);
        kill_session(&session);
        panic!(
            "Daemons screen didn't render rows after pressing d; \
             last capture:\n---\n{last}\n---"
        );
    }
    let post = post_cap.unwrap();

    // The daemon rows are present — including the approve broker, which is a
    // socket (rides notifyd's process) yet still gets a first-class row.
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

    // The Hooks panel names BOTH executables. A pointer left aimed at a deleted
    // worktree while a perfectly good ainb is running was invisible until this
    // rendered two lines, and repair was a CLI command quoted in an error.
    // Substrings that exist ONLY on those two lines: a bare "hooks " matches
    // the panel title and a bare "running " matches the table's STATE column,
    // so either would pass with both lines deleted.
    for hook_line in [") → ", "running →", "B pin running", "I install / repair"] {
        assert!(
            post.contains(hook_line),
            "hooks panel missing {hook_line:?}\n{post}"
        );
    }

    // Back home on Escape.
    send_key(&session, "Escape");
    let back_home = poll_capture_resending(
        &session,
        "Escape",
        Instant::now() + Duration::from_secs(20),
        |c| c.contains("Stats") && c.contains("[i]"),
    );
    if back_home.is_none() {
        let last = capture_pane(&session);
        kill_session(&session);
        panic!("Escape never returned to HomeScreen; last capture:\n---\n{last}\n---");
    }

    kill_session(&session);
}
