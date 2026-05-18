//! Tripwire: pressing `M` on the HomeScreen opens the SkillManager
//! screen with the spec §10.1 layout markers visible.
//!
//! Catches: HomeScreen `M` keybind missing, View::SkillManager
//! render branch dropped from layout dispatch, state mutation
//! handler removed. The in-process TestBackend renders show the
//! layout works in isolation; this tripwire proves the LIVE binary
//! actually reaches the screen on `M` press.
//!
//! Skips when `tmux` isn't on PATH.

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

/// Seed an isolated `$HOME` so the first-run onboarding wizard
/// doesn't intercept our keystrokes. Mirrors the canonical pattern
/// from `tmux-ui-tripwire` on `feat/plugin`.
fn seed_isolated_home(home: &Path) {
    let cfg = home.join(".agents-in-a-box").join("config");
    fs::create_dir_all(&cfg).expect("create isolated config dir");
    let onboarding = format!(
        r#"completed = true
completed_at = "2026-05-11T00:00:00+00:00"
version = "{ver}"
skipped_dependencies = []
git_directories = []
"#,
        ver = env!("CARGO_PKG_VERSION"),
    );
    fs::write(cfg.join("onboarding.toml"), onboarding).expect("seed onboarding.toml");
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
        thread::sleep(Duration::from_millis(400));
    }
    None
}

fn send_key(session: &str, key: &str) {
    Command::new("tmux")
        .args(["send-keys", "-t", session, key])
        .status()
        .expect("tmux send-keys");
}

fn kill_session(session: &str) {
    let _ = Command::new("tmux").args(["kill-session", "-t", session]).status();
}

#[test]
fn pressing_M_on_home_opens_skill_manager_screen() {
    if !tmux_available() {
        eprintln!("SKIP: tmux not on PATH");
        return;
    }

    let home_tmp = tempfile::tempdir().expect("home tempdir");
    seed_isolated_home(home_tmp.path());

    // Use process-id-suffixed exact session name so concurrent test
    // runs don't collide. NEVER wildcard-kill — only exact name.
    let session = format!("tripwire-sm-open-{}", std::process::id());
    let bin = ainb_bin();

    // Spawn a sized tmux session, then launch ainb inside it.
    let status = Command::new("tmux")
        .args(["new-session", "-d", "-s", &session, "-x", "200", "-y", "50"])
        .status()
        .expect("tmux new-session");
    assert!(status.success(), "tmux new-session failed");

    let cmd = format!(
        "HOME={} exec {} 2>&1",
        home_tmp.path().display(),
        bin.display()
    );
    Command::new("tmux")
        .args(["send-keys", "-t", &session, &cmd, "Enter"])
        .status()
        .expect("tmux send-keys launch");

    // Wait for HomeScreen to render FULLY. The previous bare
    // (Agents && Catalog) pair could match mid-paint, before the
    // welcome panel had painted, leading to a race where M was
    // sent while the binary was still booting and got lost.
    // Require the welcome-panel literal too so the predicate only
    // fires when both the sidebar AND the right-hand panel are
    // alive — the binary is then ready to consume keystrokes.
    let home_render = poll_capture(&session, Instant::now() + Duration::from_secs(120), |c| {
        c.contains("Agents") && c.contains("Catalog") && c.contains("Welcome to AINB")
    });
    let home_render = match home_render {
        Some(r) => r,
        None => {
            let dump = capture_pane(&session);
            kill_session(&session);
            panic!("HomeScreen never rendered. last capture:\n{dump}");
        }
    };

    // Small settle window before keystroke. ratatui paints at
    // 4-30 Hz; 200ms guarantees at least 1 idle frame between
    // initial-paint completion and the keystroke arriving.
    thread::sleep(Duration::from_millis(200));

    // Negative pre-press: we should NOT already be on the
    // SkillManager screen. If "Sources" already appears here the
    // test is testing the wrong starting state.
    assert!(
        !home_render.contains("Sources") || !home_render.contains("Units"),
        "test invariant broken: SkillManager markers visible on HomeScreen capture:\n{home_render}"
    );

    // Press M (uppercase — see app/events.rs:2012). NEVER append Enter
    // to a single-char nav key, per tripwire-skill hard rule #3.
    send_key(&session, "M");

    // Positive AND negative markers per hard rule #2 — substring-OR
    // on lone chrome strings ("Sources", "ainb") would silently pass
    // if the wrong screen rendered.
    let post = poll_capture(&session, Instant::now() + Duration::from_secs(90), |c| {
        c.contains("Sources") && c.contains("Units") && c.contains("Detail") && c.contains("[i]")
    });
    let post = match post {
        Some(p) => p,
        None => {
            let dump = capture_pane(&session);
            kill_session(&session);
            panic!("SkillManager screen never rendered after pressing M. last capture:\n{dump}");
        }
    };
    kill_session(&session);

    // Confirm placeholder data (empty state) — the wired-up screen
    // ships with SkillsScreenData::default() until the live-data
    // binding follow-up lands. "(no sources configured)" is the
    // placeholder we shipped in component code.
    assert!(post.contains("Sources"), "missing Sources panel: {post}");
    assert!(post.contains("Units"), "missing Units panel: {post}");
    assert!(post.contains("Detail"), "missing Detail pane: {post}");
    assert!(
        post.contains("[i]") && post.contains("[s]"),
        "missing help-bar hotkeys: {post}"
    );
}
