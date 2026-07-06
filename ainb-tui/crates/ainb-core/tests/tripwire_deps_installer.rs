// ABOUTME: Tripwire — the onboarding dependency step's interactive installer
// (increments A+B). Drives the real `ainb tui` in tmux with a fresh HOME,
// advances Welcome -> DependencyCheck, and asserts the new keymap footer
// (i install / t tmux / up-down focus), the focused-row marker, that the cursor
// moves on Down, and that focusing a missing dep with a docs page shows the full
// docsite URL + the "press i to install" affordance in the detail band.
//
// Follows the tmux-ui-tripwire rules: isolated HOME, poll_capture (no bare
// sleep), positive markers + negative placeholder, kill-session by exact name.

use std::path::PathBuf;
use std::process::Command;
use std::thread::sleep;
use std::time::{Duration, Instant};

fn tmux_available() -> bool {
    Command::new("tmux")
        .arg("-V")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn ainb_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ainb"))
}

fn capture(session: &str) -> String {
    let out = Command::new("tmux").args(["capture-pane", "-t", session, "-p"]).output();
    out.map(|o| String::from_utf8_lossy(&o.stdout).to_string()).unwrap_or_default()
}

fn poll_capture(session: &str, deadline: Instant, pred: impl Fn(&str) -> bool) -> Option<String> {
    while Instant::now() < deadline {
        let c = capture(session);
        if pred(&c) {
            return Some(c);
        }
        sleep(Duration::from_millis(400));
    }
    None
}

fn send(session: &str, keys: &str) {
    Command::new("tmux").args(["send-keys", "-t", session, keys]).status().ok();
}

/// The first line carrying the focus marker, trimmed — identifies which dep is
/// focused so we can prove the cursor actually moved.
fn focused_line(screen: &str) -> Option<String> {
    screen.lines().find(|l| l.contains('\u{25B6}')).map(|l| l.trim().to_string())
}

#[test]
fn deps_installer_cursor_docs_and_install_affordance() {
    if !tmux_available() {
        eprintln!("SKIP: tmux not available");
        return;
    }

    let home = tempfile::tempdir().unwrap();
    let session = format!("tripwire-deps-install-{}", std::process::id());

    Command::new("tmux")
        .args(["new-session", "-d", "-s", &session, "-x", "200", "-y", "55"])
        .status()
        .unwrap();
    let cmd = format!(
        "HOME={} AINB_DISABLE_PLUGINS=1 exec {} tui",
        home.path().display(),
        ainb_bin().display()
    );
    send(&session, &cmd);
    send(&session, "Enter");

    // Welcome step.
    if poll_capture(&session, Instant::now() + Duration::from_secs(45), |c| {
        c.contains("Setup Wizard") || c.contains("Welcome to")
    })
    .is_none()
    {
        let last = capture(&session);
        Command::new("tmux").args(["kill-session", "-t", &session]).status().ok();
        panic!("wizard welcome never rendered:\n{last}");
    }

    // Advance Welcome -> Source -> Role -> UseCase -> DependencyCheck: the
    // wizard has three questionnaire steps between Welcome and the dependency
    // step, each accepting its default selection on Enter.
    for _ in 0..4 {
        send(&session, "Enter");
        sleep(Duration::from_millis(400));
    }
    let loaded = poll_capture(&session, Instant::now() + Duration::from_secs(40), |c| {
        c.contains("Plugin binaries") && !c.contains("Checking dependencies")
    })
    .unwrap_or_else(|| capture(&session));

    // The new keymap footer must advertise the install + cursor affordances.
    assert!(
        loaded.contains("i install"),
        "footer missing 'i install':\n{loaded}"
    );
    assert!(
        loaded.contains("t tmux"),
        "footer missing 't tmux':\n{loaded}"
    );
    assert!(
        loaded.contains("focus"),
        "footer missing the up/down focus hint:\n{loaded}"
    );
    // A dep is focused (the detail band's marker).
    let first_focus =
        focused_line(&loaded).unwrap_or_else(|| panic!("no focused-row marker:\n{loaded}"));

    // Down moves the cursor: the focused row must change.
    send(&session, "Down");
    let moved = poll_capture(&session, Instant::now() + Duration::from_secs(5), |c| {
        focused_line(c).map(|l| l != first_focus).unwrap_or(false)
    });
    assert!(
        moved.is_some(),
        "Down did not move the focused-dep cursor (still {first_focus:?})"
    );

    // Traverse down; a missing dep with a docs page must surface BOTH its full
    // docsite URL and the install affordance in the detail band. A fresh HOME
    // leaves plugin/token-opt/reflect deps unsatisfied, so this is reachable.
    let mut saw_docs = false;
    let mut saw_install = false;
    for _ in 0..30 {
        let c = capture(&session);
        if c.contains("stevengonsalvez.github.io/agents-in-a-box/") {
            saw_docs = true;
        }
        if c.contains("press i to install") {
            saw_install = true;
        }
        if saw_docs && saw_install {
            break;
        }
        send(&session, "Down");
        sleep(Duration::from_millis(250));
    }

    let last = capture(&session);
    Command::new("tmux").args(["kill-session", "-t", &session]).status().ok();

    assert!(
        saw_install,
        "never saw 'press i to install' while focusing missing deps:\n{last}"
    );
    assert!(
        saw_docs,
        "never saw a full docsite URL in the detail band while navigating:\n{last}"
    );
}
