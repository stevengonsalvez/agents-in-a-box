//! P9.2 — PR badge + open-in-browser tripwire (TRIPWIRE 2).
//!
//! Seeds the P4 fixture, marks its `task-1` (on `issue-1`) `done` with a
//! `result.pr_url`, and launches `ainb tui` with `$HANGAR_OPENER_PROBE_FILE`
//! pointing at a tempfile (flipping the plugin's PR opener to a recording opener
//! that writes the URL instead of launching a browser). It then:
//!
//! 1. opens `issue-1`'s task detail (`Enter` on the first row);
//! 2. `tmux capture-pane` asserts the gold badge row `PR https://example.com/pr/1`
//!    is on screen (POSITIVE), paired with the NEGATIVE that the issue-list
//!    `Todo (3)` chrome is gone (we genuinely navigated);
//! 3. `send-keys o` (single char, NO Enter — HARD RULE 3);
//! 4. asserts the recording-opener probe file contains the badged PR URL,
//!    proving the `o` action fired without a real browser popping up.
//!
//! tmux-ui-tripwire HARD RULES: exact-name `kill-session`, `poll_capture` with a
//! deadline (no bare sleep), single-char nav keys, POSITIVE+NEGATIVE assertions,
//! SKIP-not-fail when the environment can't support the test.

use std::time::{Duration, Instant};

#[path = "tripwire_p4_common.rs"]
mod common;
use common::{TuiSession, can_run_tripwire, prepare_pipeline, seed_completed_task_with_pr, skip};

/// The PR URL seeded onto the completed task — both the on-screen badge marker
/// and the expected probe-file contents.
const PR_URL: &str = "https://example.com/pr/1";

#[test]
fn pr_badge_renders_and_o_opens_the_url() {
    if !can_run_tripwire() {
        skip("pr_badge");
        return;
    }
    let pipe = prepare_pipeline();

    // Mark the fixture's task-1 done with a captured PR URL so issue-1's wire row
    // (and the opened task detail) badges the PR.
    seed_completed_task_with_pr(pipe.home(), PR_URL);

    // The recording-opener probe file: the plugin's `o` action writes the opened
    // URL here instead of launching a browser. It lives outside the isolated
    // $HOME (in this process's tempdir) so the daemon's $HOME isolation doesn't
    // hide it.
    let probe_dir = tempfile::tempdir().expect("probe tempdir");
    let probe = probe_dir.path().join("opener-probe.txt");

    let bin = common::ainb_bin().expect("gated by can_run_tripwire");
    let sess = TuiSession::spawn_with_env(
        &bin,
        pipe.home(),
        &[("HANGAR_OPENER_PROBE_FILE", &probe.to_string_lossy())],
    );
    sess.open_hangar_and_wait_ready()
        .unwrap_or_else(|| panic!("hangar issue list never rendered:\n{}", sess.capture()));

    // 1. Open the selected (first) row's task detail.
    sess.send_enter();
    let detail = sess
        .poll_capture(Instant::now() + Duration::from_secs(15), |c| {
            c.contains(&format!("PR {PR_URL}"))
        })
        .unwrap_or_else(|| panic!("PR badge never rendered:\n{}", sess.capture()));

    // POSITIVE: the gold badge row carries the captured PR URL. NEGATIVE: the
    // issue-list status-group header is gone, so we genuinely left the list.
    assert!(
        detail.contains(&format!("PR {PR_URL}")),
        "PR badge missing:\n{detail}"
    );
    assert!(
        !detail.contains("Todo (3)"),
        "still on the issue list:\n{detail}"
    );

    // 2. Press `o` (single char, NO Enter) to open the PR. The recording opener
    //    writes the URL to the probe file.
    sess.send_key("o");

    // 3. Poll the probe file until it carries the opened URL (the plugin's `o`
    //    handler fires asynchronously after the key reaches the subprocess).
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut written = String::new();
    while Instant::now() < deadline {
        if let Ok(s) = std::fs::read_to_string(&probe) {
            written = s;
            if written.contains(PR_URL) {
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    assert!(
        written.contains(PR_URL),
        "opener probe file did not record the PR URL (got {written:?}); pane:\n{}",
        sess.capture()
    );
}
