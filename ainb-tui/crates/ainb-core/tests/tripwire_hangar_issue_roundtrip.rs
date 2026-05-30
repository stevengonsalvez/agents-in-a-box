//! tmux tripwire: real `ainb hangar issue create` -> `issue list` round-trip.
//!
//! Unlike the TUI tripwires (which drive `ainb tui` in an alternate screen),
//! this exercises the *CLI* surface in a plain shell pane: it creates an issue
//! then lists it, and polls `capture-pane` for the created title. This is the
//! user-visible proof that the `hangar` namespace is wired end-to-end through
//! the real binary, an isolated ephemeral `hangar.db`, and the store's migrate
//! path — not just that `cargo test` parses the clap tree.
//!
//! Follows the `tmux-ui-tripwire` skill rules: skip gracefully without tmux,
//! poll (never bare-sleep), and kill the session by EXACT name only.

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
        .is_ok_and(|o| o.status.success())
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
    // Specific session by EXACT name. Never kill-server, never wildcard.
    let _ = Command::new("tmux")
        .args(["kill-session", "-t", session])
        .status();
}

#[test]
fn hangar_issue_create_then_list_round_trip_in_tmux() {
    if !tmux_available() {
        eprintln!("SKIP: tmux unavailable");
        return;
    }

    let home = tempfile::tempdir().unwrap();
    let session = format!("tripwire-hangar-{}", std::process::id());

    // Detached shell pane with explicit geometry (capture output is geometry-
    // dependent).
    let status = Command::new("tmux")
        .args(["new-session", "-d", "-s", &session, "-x", "200", "-y", "50"])
        .status()
        .expect("tmux new-session");
    assert!(status.success(), "tmux new-session failed");

    // Isolate the hangar db under the tempdir so the real ~/.ainb is untouched.
    let title = "TmuxRoundtripIssue";
    let create = format!(
        "AINB_HANGAR_HOME={home} HOME={home} {bin} hangar issue create --title {title}",
        home = home.path().display(),
        bin = ainb_bin().display(),
        title = title,
    );
    Command::new("tmux")
        .args(["send-keys", "-t", &session, &create, "Enter"])
        .status()
        .expect("send create");

    // Wait for the create ack before listing.
    let created = poll_capture(&session, Instant::now() + Duration::from_secs(30), |c| {
        c.contains("created issue")
    });
    if created.is_none() {
        let pane = capture_pane(&session);
        kill_session(&session);
        panic!("issue create never acked in pane:\n{pane}");
    }

    let list = format!(
        "AINB_HANGAR_HOME={home} HOME={home} {bin} hangar issue list",
        home = home.path().display(),
        bin = ainb_bin().display(),
    );
    Command::new("tmux")
        .args(["send-keys", "-t", &session, &list, "Enter"])
        .status()
        .expect("send list");

    let post = poll_capture(&session, Instant::now() + Duration::from_secs(30), |c| {
        c.contains(title)
    })
    .unwrap_or_else(|| capture_pane(&session));

    kill_session(&session);

    assert!(
        post.contains(title),
        "created issue '{title}' never appeared in `hangar issue list` output:\n{post}"
    );
}
