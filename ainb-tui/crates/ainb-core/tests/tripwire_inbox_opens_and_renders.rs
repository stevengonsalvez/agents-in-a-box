//! Tripwire: `b` on the home screen opens the inbox, the screen draws from
//! the `inbox` section, and `Esc` returns to where it was opened from.
//!
//! Without a hangar daemon behind the socket the section is absent and the
//! screen says so, which is the honest state to pin here: the title is the
//! POSITIVE marker, the absent line proves the screen reads its section
//! rather than a placeholder, and the daemons title is the NEGATIVE check
//! (a screen that opened the wrong panel would carry it).
//!
//! Skips when `tmux` is not on `$PATH`. No plugins are needed: the inbox is
//! a built-in screen over a section.

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

/// A home with onboarding complete and the hooks prompt dismissed, so no
/// wizard or dialog overlays the home screen and swallows the `b`.
fn seed_home(home: &Path) {
    let cfg = home.join(".agents-in-a-box").join("config");
    fs::create_dir_all(&cfg).expect("create config dir");
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

struct Session(String);

impl Drop for Session {
    fn drop(&mut self) {
        let _ = Command::new("tmux").args(["kill-session", "-t", &self.0]).status();
    }
}

#[test]
fn b_opens_the_inbox_over_its_section_and_esc_returns_home() {
    if !tmux_available() {
        eprintln!("SKIP: tmux not available");
        return;
    }
    let home_tmp = tempfile::tempdir().expect("home tempdir");
    seed_home(home_tmp.path());

    let session = Session(format!("tripwire-inbox-{}", std::process::id()));
    let status = Command::new("tmux")
        .args([
            "new-session",
            "-d",
            "-s",
            &session.0,
            "-x",
            "160",
            "-y",
            "45",
        ])
        .status()
        .expect("tmux new-session");
    assert!(status.success(), "tmux new-session failed");

    // No hangar home is pointed at a live daemon: the inbox read fails and the
    // section is absent, which the screen must say.
    let cmd = format!(
        "HOME={home} AINB_HOME={home}/.agents-in-a-box AINB_DISABLE_PLUGINS=1 exec {bin} tui",
        home = home_tmp.path().display(),
        bin = ainb_bin().display(),
    );
    send_key(&session.0, &cmd);
    send_key(&session.0, "Enter");

    // The home screen's sidebar names its panels; the inbox is a key on it,
    // not a sidebar row yet, so readiness is the sidebar having drawn.
    let home_deadline = Instant::now() + Duration::from_secs(30);
    let home = poll_capture(&session.0, home_deadline, |cap| cap.contains("Daemons"))
        .unwrap_or_else(|| panic!("home never drew its sidebar:\n{}", capture_pane(&session.0)));
    assert!(
        !home.contains("📥"),
        "the inbox is not open before the key:\n{home}"
    );

    send_key(&session.0, "b");
    let opened = poll_capture(
        &session.0,
        Instant::now() + Duration::from_secs(15),
        |cap| cap.contains("Inbox") && cap.contains("mark all read"),
    )
    .unwrap_or_else(|| panic!("the inbox did not open:\n{}", capture_pane(&session.0)));
    assert!(
        !opened.contains("⚙ Daemons"),
        "the wrong panel opened:\n{opened}"
    );
    // The section, not a placeholder: with no daemon the reader's failure is
    // what the screen draws, in the section's own words.
    let absent = poll_capture(
        &session.0,
        Instant::now() + Duration::from_secs(15),
        |cap| cap.contains("inbox unavailable"),
    )
    .unwrap_or_else(|| {
        panic!(
            "the screen never drew the section's absent reason:\n{}",
            capture_pane(&session.0)
        )
    });
    assert!(
        absent.contains("0 unread"),
        "the unread count is drawn from the section:\n{absent}"
    );

    send_key(&session.0, "Escape");
    poll_capture(
        &session.0,
        Instant::now() + Duration::from_secs(15),
        |cap| !cap.contains("📥") && cap.contains("Daemons"),
    )
    .unwrap_or_else(|| {
        panic!(
            "Esc did not return to the origin screen:\n{}",
            capture_pane(&session.0)
        )
    });
}
