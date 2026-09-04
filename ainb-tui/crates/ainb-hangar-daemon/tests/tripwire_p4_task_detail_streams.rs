//! P4.9 — task-detail tripwire: `Enter` on the selected row opens task detail.
//!
//! Opening the first issue row's task detail renders the issue DETAIL CARD from
//! the issue's row: the `📋` card title (a task-detail-only marker, POSITIVE)
//! and the assignee resolved to its roster name (`claude-agent`, never the raw
//! `agent:agent-1` ref: crisp B1, defect 8), paired with a NEGATIVE check that
//! the issue-list `Todo (3)` chrome is gone (we actually navigated).
//! The transcript itself is empty until task events stream (P5 event push), so
//! the assertions key on the card that renders from the snapshot today.
//! Forward (Enter → detail) is paired with a return (`1` → issue list).
//!
//! The markers used to be the sidebar's `Assignee` / `Project` LABELS. Crisp B4
//! §2.3 deleted that sidebar (it repeated the card's fields, printed the
//! workspace ULID under `Project:` and cost the transcript a quarter of the
//! width) and collapsed the labelled rows into one line of values. The three
//! things this test exists to prove — we navigated, the name resolved, we came
//! back — are unchanged; only the strings that witness them moved.
//!
//! Runs for real when tmux + binaries + staged plugin are present; SKIPs
//! gracefully otherwise (see `tripwire_p4_common.rs`).

use std::time::{Duration, Instant};

#[path = "tripwire_p4_common.rs"]
mod common;
use common::{TuiSession, can_run_tripwire, prepare_pipeline, skip};

/// The task-detail DETAIL CARD's title glyph — painted on that screen and no
/// other, so it witnesses both the arrival and (by its absence) the return.
const DETAIL_CARD_MARKER: &str = "📋";

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
            c.contains(DETAIL_CARD_MARKER) && c.contains("claude-agent")
        })
        .expect("task detail never rendered");

    // POSITIVE: the detail card painted, and the meta line carries the seeded
    // assignee resolved to its roster name. NEGATIVE: the issue-list status-group
    // header is gone, so we genuinely left the list.
    assert!(
        detail.contains(DETAIL_CARD_MARKER),
        "detail card missing:\n{detail}"
    );
    assert!(
        detail.contains("claude-agent"),
        "seeded assignee not resolved to its name:\n{detail}"
    );
    assert!(
        !detail.contains("agent:agent-1"),
        "raw actor ref leaked onto the meta line:\n{detail}"
    );
    assert!(
        !detail.contains("Todo (3)"),
        "still on the issue list:\n{detail}"
    );

    // Return navigation: back to the issue list. The `📋` card title is a
    // task-detail-only marker (no other screen paints it), so its absence proves
    // we returned — unlike the assignee VALUE, which legitimately shows in the
    // issue list's assignee column.
    // Re-send the nav key until the issue list re-renders: a lone keypress can
    // be dropped on a loaded CI runner.
    let back = sess
        .switch_tab_until(
            "1",
            Instant::now() + Duration::from_secs(10 * common::budget_scale()),
            |c| c.contains("Todo (3)"),
        )
        .expect("issue list never returned from task detail");
    assert!(
        !back.contains(DETAIL_CARD_MARKER),
        "the task detail card bled into the issue list:\n{back}"
    );
}
