//! Tripwire (overlay-panels): the panel shortcuts are DISCOVERABLE.
//!
//! The feature is only useful if a user can find the keys. Two surfaces
//! advertise them, and both are asserted here against the real running
//! TUI (the unit test `menu_bar_keys_not_truncated_at_80_cols` covers
//! the 80-col layout math; this proves the tokens actually paint in a
//! live 200-col pane):
//!
//! 1. The session-list menu legend's panel line — `b inbox  i stats
//!    w witr  k skills` — so every panel is reachable from the session
//!    list, not just the home menu.
//! 2. The `?` help overlay's "Panels" section, which documents that
//!    closing a panel returns to its origin.
//!
//! Skips gracefully if `tmux` isn't on `$PATH`. Plugins are disabled —
//! the legend and help overlay are host chrome, independent of the
//! plugin runtime.

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
    let install_record = r#"{"agents":[],"hook_script":"","prompt_dismissed":true}"#;
    fs::write(
        home.join(".agents-in-a-box").join("install.json"),
        install_record,
    )
    .expect("seed install.json");
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
    Command::new("tmux")
        .args(["send-keys", "-t", session, key])
        .status()
        .expect("tmux send-keys");
}

fn kill_session(session: &str) {
    let _ = Command::new("tmux").args(["kill-session", "-t", session]).status();
}

fn launch_to_home(session: &str, home: &Path) {
    let status = Command::new("tmux")
        .args(["new-session", "-d", "-s", session, "-x", "200", "-y", "50"])
        .status()
        .expect("tmux new-session");
    assert!(status.success(), "tmux new-session failed");
    let cmd = format!(
        "HOME={} AINB_DISABLE_PLUGINS=1 exec {} tui",
        home.display(),
        ainb_bin().display()
    );
    Command::new("tmux")
        .args(["send-keys", "-t", session, &cmd, "Enter"])
        .status()
        .expect("send launch cmd");
    if poll_capture(session, Instant::now() + Duration::from_secs(90), |c| {
        c.contains("Stats") && c.contains("[i]")
    })
    .is_none()
    {
        let last = capture_pane(session);
        kill_session(session);
        panic!("HomeScreen never rendered; last capture:\n---\n{last}\n---");
    }
}

#[test]
fn session_list_legend_advertises_every_panel() {
    if !tmux_available() {
        eprintln!("SKIP: tmux not available");
        return;
    }
    let home_tmp = tempfile::tempdir().expect("home tempdir");
    seed_isolated_home(home_tmp.path());
    let session = format!("tripwire-legend-{}", std::process::id());
    launch_to_home(&session, home_tmp.path());

    send_key(&session, "s");
    // The panel line renders the four shortcuts together; wait for the
    // whole group so we don't race a half-painted legend.
    let cap = poll_capture(&session, Instant::now() + Duration::from_secs(40), |c| {
        c.contains("b inbox") && c.contains("k skills")
    });
    let final_cap = cap.unwrap_or_else(|| capture_pane(&session));
    kill_session(&session);

    // Each panel shortcut must be advertised — exact `<key> <label>`
    // pairs as the legend paints them (key span + description span are
    // adjacent in the capture). A missing token means a panel is
    // reachable but undiscoverable from the session list.
    for token in ["b inbox", "i stats", "w witr", "k skills"] {
        assert!(
            final_cap.contains(token),
            "session-list legend missing panel shortcut {token:?}:\n---\n{final_cap}\n---"
        );
    }
}

#[test]
fn help_overlay_documents_panels_section() {
    if !tmux_available() {
        eprintln!("SKIP: tmux not available");
        return;
    }
    let home_tmp = tempfile::tempdir().expect("home tempdir");
    seed_isolated_home(home_tmp.path());
    let session = format!("tripwire-help-{}", std::process::id());
    launch_to_home(&session, home_tmp.path());

    // `?` opens the global help overlay.
    send_key(&session, "?");
    let cap = poll_capture(&session, Instant::now() + Duration::from_secs(30), |c| {
        c.contains("Panels (closing returns here)")
    });
    let final_cap = cap.unwrap_or_else(|| capture_pane(&session));
    kill_session(&session);

    // The Panels section header plus each panel entry — the entries
    // double as the documented contract that closing a panel returns to
    // its origin (and that witr is quit, not Esc-closed).
    for token in [
        "Panels (closing returns here)",
        "Inbox (Esc closes)",
        "Stats / usage analytics (Esc closes)",
        "Witr process browser (quit witr to return)",
        "Skills browser (Esc closes)",
    ] {
        assert!(
            final_cap.contains(token),
            "help overlay missing Panels entry {token:?}:\n---\n{final_cap}\n---"
        );
    }
}
