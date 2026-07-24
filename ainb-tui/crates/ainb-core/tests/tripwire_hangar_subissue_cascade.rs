//! tmux tripwire: sub-issue (`--parent`) create + child-done -> parent cascade.
//!
//! Drives the real `ainb hangar issue` CLI in a live shell pane (like
//! `tripwire_hangar_issue_roundtrip`) against an isolated ephemeral `hangar.db`:
//!
//!   1. create a PARENT issue,
//!   2. create a CHILD `--parent <parent-id>` (the 0046 sub-issue link),
//!   3. `issue update <child> --state done` — a non-terminal -> terminal
//!      transition that closes the (single, unstaged) stage barrier.
//!
//! The completing update prints `posted sub-issue roll-up on parent <id> (1/1)`,
//! the user-visible proof that the child linked to the parent AND that finishing
//! it cascaded a roll-up comment onto the parent through the real binary + store
//! service — not just that `cargo test` parses the clap tree. This is the
//! daemon-free half of the gap-3 acceptance (the TUI `D` key drives the SAME
//! `cascade_child_done` service via the daemon's `issue_update`).
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
    Command::new("tmux").arg("-V").output().is_ok_and(|o| o.status.success())
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
    let _ = Command::new("tmux").args(["kill-session", "-t", session]).status();
}

#[test]
fn hangar_subissue_child_done_cascades_rollup_in_tmux() {
    if !tmux_available() {
        eprintln!("SKIP: tmux unavailable");
        return;
    }

    let home = tempfile::tempdir().unwrap();
    let session = format!("tripwire-subissue-{}", std::process::id());

    let status = Command::new("tmux")
        .args(["new-session", "-d", "-s", &session, "-x", "200", "-y", "50"])
        .status()
        .expect("tmux new-session");
    assert!(status.success(), "tmux new-session failed");

    // One compound shell command: create parent, capture its id; create the child
    // linked with `--parent`, capture its id; complete the child. The parent id is
    // unknown at author time, so the assertions key off the stable cascade line
    // (`posted sub-issue roll-up on parent … (1/1)`) plus a SENTINEL echo that
    // proves both ids were captured (a create that failed to print `created issue`
    // leaves the var empty and the sentinel shows it).
    let bin = ainb_bin().display().to_string();
    let home_s = home.path().display().to_string();
    let script = format!(
        "export AINB_HANGAR_HOME={home_s}; export HOME={home_s}; \
         P=$({bin} hangar issue create --title ParentEpicXYZ | sed -n 's/^created issue //p'); \
         C=$({bin} hangar issue create --title ChildOneXYZ --parent \"$P\" | sed -n 's/^created issue //p'); \
         {bin} hangar issue update \"$C\" --state done; \
         echo \"SENTINEL parent=$P child=$C\"",
    );
    Command::new("tmux")
        .args(["send-keys", "-t", &session, &script, "Enter"])
        .status()
        .expect("send cascade script");

    let pane = poll_capture(&session, Instant::now() + Duration::from_secs(60), |c| {
        c.contains("posted sub-issue roll-up on parent") && c.contains("SENTINEL parent=")
    })
    .unwrap_or_else(|| capture_pane(&session));

    kill_session(&session);

    assert!(
        pane.contains("posted sub-issue roll-up on parent"),
        "completing the sub-issue did not cascade a roll-up onto the parent:\n{pane}"
    );
    // The single unstaged child is the whole set, so the barrier closes at 1/1.
    assert!(
        pane.contains("(1/1)"),
        "cascade roll-up did not report the 1/1 completion count:\n{pane}"
    );
    // The completing update committed the terminal state edit before cascading.
    assert!(
        pane.contains("updated issue"),
        "child issue was never updated to done:\n{pane}"
    );
    // The script ran to completion (the sentinel echo landed after the update),
    // so the cascade line above is the real terminal output, not a mid-run stall.
    // The cascade itself only fires when the child's `parent_issue_id` resolves to
    // the parent, so its presence already proves the `--parent` link took.
    assert!(
        pane.contains("SENTINEL parent="),
        "cascade script did not run to completion:\n{pane}"
    );
}
