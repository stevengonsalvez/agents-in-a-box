//! Tripwire: from the SkillManager view, pressing `Esc` returns to
//! the HomeScreen. The forward-only sibling
//! `tripwire_skill_manager_screen_opens.rs` would pass while the
//! return path silently broke — exactly the failure mode the
//! tmux-ui-tripwire skill's hard rule #6 codifies.
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
    let _ = Command::new("tmux")
        .args(["kill-session", "-t", session])
        .status();
}

#[test]
fn esc_on_skill_manager_returns_to_home_screen() {
    if !tmux_available() {
        eprintln!("SKIP: tmux not on PATH");
        return;
    }

    let home_tmp = tempfile::tempdir().expect("home tempdir");
    seed_isolated_home(home_tmp.path());

    let session = format!("tripwire-sm-esc-{}", std::process::id());
    let bin = ainb_bin();

    Command::new("tmux")
        .args(["new-session", "-d", "-s", &session, "-x", "200", "-y", "50"])
        .status()
        .expect("tmux new-session");

    let cmd = format!(
        "HOME={} exec {} 2>&1",
        home_tmp.path().display(),
        bin.display()
    );
    Command::new("tmux")
        .args(["send-keys", "-t", &session, &cmd, "Enter"])
        .status()
        .expect("tmux send-keys launch");

    // Wait for HomeScreen.
    if poll_capture(
        &session,
        Instant::now() + Duration::from_secs(120),
        |c| c.contains("Agents") && c.contains("Catalog") && c.contains("Welcome to AINB"),
    )
    .is_none()
    {
        let dump = capture_pane(&session);
        kill_session(&session);
        panic!("HomeScreen never rendered:\n{dump}");
    }

    // Settle window: predicate can match mid-frame. Give ratatui one
    // idle paint cycle before sending M so the keystroke isn't lost
    // to a binary still finishing its boot paint.
    thread::sleep(Duration::from_millis(200));

    // Navigate into SkillManager.
    send_key(&session, "M");
    if poll_capture(
        &session,
        Instant::now() + Duration::from_secs(90),
        |c| c.contains("Sources") && c.contains("Units") && c.contains("Detail"),
    )
    .is_none()
    {
        let dump = capture_pane(&session);
        kill_session(&session);
        panic!("SkillManager never rendered after M:\n{dump}");
    }

    // Settle before the return trip too — analogous race in reverse.
    thread::sleep(Duration::from_millis(200));

    // Return-trip: press Esc.
    send_key(&session, "Escape");

    // Positive + negative markers per skill hard-rule #2:
    //   positive  →  HomeScreen-only text ("Welcome to AINB" appears
    //               only on the v2 home-screen welcome panel)
    //   negative  →  the unique SkillManager Detail placeholder
    //               "(select a unit to see details)" must be gone.
    // The slow debug-mode build can spend ~1s mid-transition where
    // both views are partially painted; we wait for the new screen
    // to FULLY claim the bottom rows by also requiring the
    // SkillManager-only marker to disappear before sampling.
    let post = poll_capture(
        &session,
        Instant::now() + Duration::from_secs(90),
        |c| {
            c.contains("Agents")
                && c.contains("Catalog")
                && c.contains("Welcome to")
                && !c.contains("(select a unit to see details)")
        },
    );
    let post = match post {
        Some(p) => p,
        None => {
            let dump = capture_pane(&session);
            kill_session(&session);
            panic!(
                "Esc did not return to HomeScreen — SkillManager swallowed the key \
                 OR repaint left SkillManager chrome on screen.\n{dump}"
            );
        }
    };
    kill_session(&session);

    assert!(post.contains("Agents"), "missing HomeScreen sidebar: {post}");
    assert!(post.contains("Catalog"), "missing HomeScreen sidebar: {post}");
    assert!(
        post.contains("Welcome to"),
        "missing HomeScreen welcome panel: {post}"
    );
    // SkillManager-only Detail placeholder must be gone.
    assert!(
        !post.contains("(select a unit to see details)"),
        "SkillManager Detail panel still visible — Esc didn't fully leave the view:\n{post}"
    );
}
