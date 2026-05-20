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

#[path = "tripwire_tmux_lock.rs"]
mod tripwire_tmux_lock;

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

#[test]
fn tui_sessions_screen_renders_after_pressing_s() {
    if !tmux_available() {
        eprintln!("SKIP: tmux not available");
        return;
    }

    let _lock = tripwire_tmux_lock::TmuxSerialLock::acquire();

    let home_tmp = tempfile::tempdir().expect("home tempdir");
    seed_isolated_home(home_tmp.path());

    let session = format!("tripwire-sess-{}", std::process::id());
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
        let _ = Command::new("tmux").args(["kill-session", "-t", &session]).status();
        panic!("HomeScreen never rendered; last:\n{last}");
    };
    assert!(
        !pre_cap.contains("Session List") && !pre_cap.contains("session_list"),
        "pre-key capture already on sessions screen — state leaked\n{pre_cap}"
    );

    // Re-send `s` every ~5s up to 60s — under full L1 ci contention a
    // single send-key can be dropped before the host's event loop
    // drains it. Solo runs don't see this.
    let nav_deadline = Instant::now() + Duration::from_secs(60);
    let mut last_press = Instant::now();
    let _ = Command::new("tmux")
        .args(["send-keys", "-t", &session, "s"])
        .status()
        .expect("send s");
    let mut post: Option<String> = None;
    while Instant::now() < nav_deadline {
        let c = capture(&session);
        if is_on_sessions_screen(&c) {
            post = Some(c);
            break;
        }
        if last_press.elapsed() > Duration::from_secs(5) {
            let _ = Command::new("tmux").args(["send-keys", "-t", &session, "s"]).status();
            last_press = Instant::now();
        }
        thread::sleep(Duration::from_millis(400));
    }

    let final_cap = post.unwrap_or_else(|| capture(&session));
    let _ = Command::new("tmux").args(["kill-session", "-t", &session]).status();

    let on_sessions = is_on_sessions_screen(&final_cap);
    assert!(
        on_sessions,
        "sessions screen never rendered after 's'.\n{final_cap}"
    );
    // Regression cross-check: must not be analytics by accident.
    assert!(
        !final_cap.contains("Usage Analytics"),
        "navigation drifted to analytics instead of sessions.\n{final_cap}"
    );
}
