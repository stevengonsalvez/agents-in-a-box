//! P4.9 — task-detail tripwire: `enter` on row 1 opens the streaming transcript.
//!
//! Asserts the running task's agent (`claude-agent`), elapsed `9m`, and a
//! transcript glyph `▌` render (POSITIVE), paired with a NEGATIVE check that the
//! issue-list chrome is gone (we actually navigated). Forward (enter → detail) is
//! paired with a return (`Esc`/`1` → issue list).
//!
//! SKIPs until the P5 render pipeline is standable — see `tripwire_p4_common.rs`.

use std::time::{Duration, Instant};

#[path = "tripwire_p4_common.rs"]
mod common;
use common::{can_run_tripwire, seed_isolated_home, skip, TuiSession};

#[test]
fn task_detail_streams_transcript() {
    if !can_run_tripwire() {
        skip("task_detail");
        return;
    }
    let home = seed_isolated_home();
    let bin = common::ainb_bin().expect("gated by can_run_tripwire");
    let sess = TuiSession::spawn(&bin, home.path());
    sess.wait_ready().expect("issue list never rendered");

    // Open the first row's task detail (Enter commits the row open).
    sess.send_enter();
    let detail = sess
        .poll_capture(Instant::now() + Duration::from_secs(15), |c| {
            c.contains("claude-agent") && c.contains('▌')
        })
        .expect("task detail never rendered");

    // POSITIVE: agent label + elapsed + transcript glyph. NEGATIVE: not the list.
    assert!(detail.contains("claude-agent"), "agent label missing:\n{detail}");
    assert!(detail.contains("9m"), "elapsed time missing:\n{detail}");
    assert!(detail.contains('▌'), "transcript glyph missing:\n{detail}");
    assert!(!detail.contains("Todo (3)"), "still on the issue list:\n{detail}");

    // Return navigation: back to the issue list.
    sess.send_key("1");
    let back = sess
        .poll_capture(Instant::now() + Duration::from_secs(10), |c| c.contains("Refactor API"))
        .expect("issue list never returned from task detail");
    assert!(!back.contains('▌'), "transcript bled into the issue list:\n{back}");
}
