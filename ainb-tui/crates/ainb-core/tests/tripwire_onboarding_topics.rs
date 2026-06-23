// ABOUTME: Tripwire — the first-run onboarding wizard's dependency step must
// render the new topic/dependency model (crate::setup catalog), not the old flat
// category list. Drives the real `ainb tui` in tmux with a fresh HOME (so the
// wizard appears), advances Welcome -> DependencyCheck, and asserts the new
// topic headers + tier tags are on screen.
//
// Follows the tmux-ui-tripwire rules: isolated HOME, poll_capture (no bare
// sleep), positive marker + negative placeholder, kill-session by exact name.

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
        sleep(Duration::from_millis(500));
    }
    None
}

#[test]
fn onboarding_dependency_step_renders_topics() {
    if !tmux_available() {
        eprintln!("SKIP: tmux not available");
        return;
    }

    let home = tempfile::tempdir().unwrap();
    let session = format!("tripwire-onb-{}", std::process::id());

    // Fresh HOME (no onboarding.toml) → the first-run wizard appears.
    Command::new("tmux")
        .args(["new-session", "-d", "-s", &session, "-x", "200", "-y", "55"])
        .status()
        .unwrap();
    let cmd = format!(
        "HOME={} AINB_DISABLE_PLUGINS=1 exec {} tui",
        home.path().display(),
        ainb_bin().display()
    );
    Command::new("tmux")
        .args(["send-keys", "-t", &session, &cmd, "Enter"])
        .status()
        .unwrap();

    // Wait for the Welcome step of the setup wizard.
    let pre = poll_capture(&session, Instant::now() + Duration::from_secs(45), |c| {
        c.contains("Setup Wizard") || c.contains("Welcome to")
    });
    if pre.is_none() {
        let last = capture(&session);
        Command::new("tmux").args(["kill-session", "-t", &session]).status().ok();
        panic!("wizard welcome never rendered:\n{last}");
    }

    // Advance Welcome -> DependencyCheck (auto-triggers the catalog detect).
    Command::new("tmux")
        .args(["send-keys", "-t", &session, "Enter"])
        .status()
        .unwrap();

    // The new topic-grouped model must render these headers — none of which
    // existed in the old flat category list (Core/Container/Session/Toolkit/
    // GitHub/AiCli/Configuration).
    let post = poll_capture(&session, Instant::now() + Duration::from_secs(40), |c| {
        c.contains("agents-in-a-box")
            && c.contains("Reflect")
            && c.contains("Plugin binaries")
            && c.contains("Statusline")
    })
    .unwrap_or_else(|| capture(&session));

    Command::new("tmux").args(["kill-session", "-t", &session]).status().ok();

    // Positive markers: the new topics are present.
    assert!(
        post.contains("agents-in-a-box"),
        "missing agents-in-a-box topic:\n{post}"
    );
    assert!(post.contains("Reflect"), "missing Reflect topic:\n{post}");
    assert!(
        post.contains("Plugin binaries"),
        "missing Plugin binaries topic:\n{post}"
    );
    assert!(
        post.contains("Statusline"),
        "missing Statusline topic:\n{post}"
    );
    // Tier tags are part of the new model — at least one must show.
    assert!(
        post.contains("[recommended]") || post.contains("[optional]"),
        "no tier tags rendered (topic/tier model not active):\n{post}"
    );
    // A fresh HOME leaves ainb-toolkit/statusline unsatisfied, so at least one
    // install hint (the → arrow) must render — proves missing deps surface their
    // provisioner, not just that everything happened to be installed.
    assert!(
        post.contains('\u{2192}'),
        "no install-hint arrow rendered:\n{post}"
    );
    // Negative: we are not stuck on the loading placeholder.
    assert!(
        !post.contains("Checking dependencies"),
        "stuck on dependency-check spinner:\n{post}"
    );
}
