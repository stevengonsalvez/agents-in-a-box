//! Tripwire 7f.5: Sessions screen renders when user presses `s`.
//!
//! Sister to 7f.3 — protects the non-plugin path. The host owns the
//! sessions screen directly (no plugin involvement), so this gate
//! ensures the eager-spawn + plugin runtime work didn't regress
//! the rest of the TUI. Same wizard-skip + tmux dance as 7f.3.
//!
//! Pressing `s` on the HomeScreen fires `AppEvent::GoToSessionList`
//! per `crates/ainb-core/src/app/events.rs` HomeScreen handler.

use std::fs;
use std::path::Path;
use std::path::PathBuf;
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
    fs::create_dir_all(&cfg).unwrap();
    let onboarding = format!(
        r#"completed = true
completed_at = "2026-05-11T00:00:00+00:00"
version = "{}"
skipped_dependencies = []
git_directories = []
"#,
        env!("CARGO_PKG_VERSION")
    );
    fs::write(cfg.join("onboarding.toml"), onboarding).unwrap();
    let install_record = r#"{"agents":[],"hook_script":"","claude_plugin_dir":null,"codex_hooks_json":null,"plugin_version":null,"prompt_dismissed":true}"#;
    fs::write(base.join("install.json"), install_record).unwrap();
}

/// Markers unique to the Session List screen (post-`s` navigation).
/// Verified against an actual capture: the screen renders a session
/// list panel plus a "Session Details" sub-panel, with toolbar
/// footers that are not present on the HomeScreen.
fn is_on_sessions_screen(c: &str) -> bool {
    c.contains("Session Details")
        || c.contains("Select a session to view details")
        || c.contains("attach") && c.contains("restart") && c.contains("cleanup")
}

fn capture(session: &str) -> String {
    let out = Command::new("tmux")
        .args(["capture-pane", "-t", session, "-p"])
        .output()
        .expect("capture");
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn send_sgr_mouse(session: &str, code: u16, x: u16, y: u16, press_or_drag: bool) {
    let suffix = if press_or_drag { 'M' } else { 'm' };
    let sgr = format!("[<{};{};{}{}", code, x + 1, y + 1, suffix);
    let status = Command::new("tmux")
        .args(["send-keys", "-t", session, "Escape", &sgr])
        .status()
        .expect("send mouse");
    assert!(status.success(), "tmux send mouse failed");
}

fn config_text(home: &Path) -> String {
    fs::read_to_string(home.join(".agents-in-a-box/config/config.toml")).unwrap_or_default()
}

struct TmuxSessionGuard {
    name: String,
}

impl Drop for TmuxSessionGuard {
    fn drop(&mut self) {
        let _ = Command::new("tmux").args(["kill-session", "-t", &self.name]).status();
    }
}

fn poll<F>(session: &str, deadline: Instant, mut ok: F) -> Option<String>
where
    F: FnMut(&str) -> bool,
{
    while Instant::now() < deadline {
        let c = capture(session);
        if ok(&c) {
            return Some(c);
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

fn poll_resending<F>(session: &str, key: &str, deadline: Instant, mut ok: F) -> Option<String>
where
    F: FnMut(&str) -> bool,
{
    while Instant::now() < deadline {
        send_key(session, key);
        thread::sleep(Duration::from_millis(500));
        let c = capture(session);
        if ok(&c) {
            return Some(c);
        }
    }
    None
}

#[test]
fn tui_sessions_screen_renders_after_pressing_s() {
    if !tmux_available() {
        eprintln!("SKIP: tmux not available");
        return;
    }

    let home_tmp = tempfile::tempdir().expect("home tempdir");
    seed_isolated_home(home_tmp.path());

    let session = format!("tripwire-sess-{}", std::process::id());
    let _guard = TmuxSessionGuard {
        name: session.clone(),
    };
    let ainb = ainb_bin();

    let status = Command::new("tmux")
        .args(["new-session", "-d", "-s", &session, "-x", "180", "-y", "50"])
        .status()
        .expect("new-session");
    assert!(status.success());

    // No plugins needed — sessions screen is host-owned. Pass
    // `AINB_DISABLE_PLUGINS=1` so the test doesn't depend on the
    // plugin staging layout and finishes faster.
    let cmd = format!(
        "HOME={} AINB_DISABLE_PLUGINS=1 exec {} tui",
        home_tmp.path().display(),
        ainb.display()
    );
    Command::new("tmux")
        .args(["send-keys", "-t", &session, &cmd, "Enter"])
        .status()
        .expect("launch");

    let home_deadline = Instant::now() + Duration::from_secs(45);
    let pre = poll(&session, home_deadline, |c| {
        c.contains("Sessions") && c.contains("[s]")
    });
    let Some(pre_cap) = pre else {
        let last = capture(&session);
        panic!("HomeScreen never rendered; last:\n{last}");
    };
    assert!(
        !pre_cap.contains("Session List") && !pre_cap.contains("session_list"),
        "pre-key capture already on sessions screen — state leaked\n{pre_cap}"
    );

    // Wait for sessions screen markers. Per the layout in
    // `components/session_list.rs`, the screen renders a panel with
    // either a session list or an empty-state hint — accept either
    // because a fresh tempdir HOME has no sessions yet.
    let nav_deadline = Instant::now() + Duration::from_secs(20);
    let post = poll_resending(&session, "s", nav_deadline, |c| is_on_sessions_screen(c));

    let final_cap = post.unwrap_or_else(|| capture(&session));

    let on_sessions = is_on_sessions_screen(&final_cap);
    assert!(
        on_sessions,
        "sessions screen never rendered after 's'.\n{final_cap}"
    );

    send_sgr_mouse(&session, 0, 2, 3, true);
    send_sgr_mouse(&session, 0, 2, 3, false);
    let collapsed = poll(&session, Instant::now() + Duration::from_secs(10), |c| {
        c.contains("[+]")
            && config_text(home_tmp.path()).contains("sessions_sidebar_collapsed = true")
    });
    assert!(
        collapsed.is_some(),
        "sessions sidebar did not collapse from mouse control; config:\n{}\npane:\n{}",
        config_text(home_tmp.path()),
        capture(&session)
    );

    send_sgr_mouse(&session, 0, 2, 4, true);
    send_sgr_mouse(&session, 0, 2, 4, false);
    let expanded = poll(&session, Instant::now() + Duration::from_secs(10), |c| {
        !c.contains("[+]")
            && is_on_sessions_screen(c)
            && config_text(home_tmp.path()).contains("sessions_sidebar_collapsed = false")
    });
    assert!(
        expanded.is_some(),
        "sessions sidebar did not expand from visible [+] click; config:\n{}\npane:\n{}",
        config_text(home_tmp.path()),
        capture(&session)
    );

    // Regression cross-check: must not be analytics by accident.
    assert!(
        !final_cap.contains("Usage Analytics"),
        "navigation drifted to analytics instead of sessions.\n{final_cap}"
    );
}
