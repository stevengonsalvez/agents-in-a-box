//! P4.9 — task-detail tripwire: `Enter` on the selected row opens task detail.
//!
//! Opening the first issue row's task detail renders the progressive-disclosure
//! sidebar from the issue's row: the `Assignee` field bound to the seeded agent
//! (`agent:agent-1`) and the `Status` field (POSITIVE), paired with a NEGATIVE
//! check that the issue-list `Todo (3)` chrome is gone (we actually navigated).
//! The transcript itself is empty until task events stream (P5 event push), so
//! the assertions key on the sidebar that renders from the snapshot today.
//! Forward (Enter → detail) is paired with a return (`1` → issue list).
//!
//! Runs for real when tmux + binaries + staged plugin are present; SKIPs
//! gracefully otherwise (see `tripwire_p4_common.rs`).

use std::time::{Duration, Instant};

#[path = "tripwire_p4_common.rs"]
mod common;
use common::{TuiSession, can_run_tripwire, prepare_pipeline, skip};

#[test]
fn task_detail_opens_for_selected_issue() {
    if !can_run_tripwire() {
        skip("task_detail");
        return;
    }
    let pipe = prepare_pipeline();
    let bin = common::ainb_bin().expect("gated by can_run_tripwire");
    let (sess, _landing) = TuiSession::launch_to_hangar(&bin, pipe.home());

    // Open the selected (first) row's task detail. Enter commits the row open.
    sess.send_enter();
    let detail = sess
        .poll_capture(Instant::now() + Duration::from_secs(15), |c| {
            c.contains("Assignee") && c.contains("agent:agent-1")
        })
        .expect("task detail never rendered");

    // POSITIVE: sidebar fields bound to the seeded issue/agent. NEGATIVE: the
    // issue-list status-group header is gone, so we genuinely left the list.
    assert!(
        detail.contains("Assignee"),
        "sidebar Assignee missing:\n{detail}"
    );
    assert!(
        detail.contains("agent:agent-1"),
        "seeded assignee missing:\n{detail}"
    );
    assert!(
        detail.contains("Project"),
        "sidebar Project missing:\n{detail}"
    );
    assert!(
        !detail.contains("Todo (3)"),
        "still on the issue list:\n{detail}"
    );

    // Return navigation: back to the issue list. The sidebar `Project` label is a
    // task-detail-only marker (the issue list has no such field), so its absence
    // proves we returned rather than the assignee VALUE (which legitimately shows
    // in the issue list's assignee column).
    sess.send_key("1");
    let back = sess
        .poll_capture(Instant::now() + Duration::from_secs(10), |c| {
            c.contains("Todo (3)")
        })
        .expect("issue list never returned from task detail");
    assert!(
        !back.contains("Project"),
        "task sidebar bled into the issue list:\n{back}"
    );
}
