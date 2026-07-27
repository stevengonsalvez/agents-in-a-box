//! Activity-timeline modal reducer + render (multica parity #13).
//!
//! Pins the keyboard contract and the render text a user actually reads:
//!
//! * `y` on a selected issue-list row raises `OpenActivityTimeline(row.id)`;
//!   `y` with NO selection is a no-op (never a modal opened on nothing).
//! * `j`/`k` move the cursor, clamped at both ends.
//! * `r` raises the refresh intent carrying the issue id.
//! * Applying an `IssueTimelineResult` populates the entries in order.
//! * The render shows the EXACT change text (`open → in_progress`), the comment
//!   body, the `▶` cursor and the `j/k scroll  r refresh  esc back` bar.

use ainb_hangar_core::ids::IssueId;
use ainb_hangar_proto::snapshots::TimelineEntryRow;
use ainb_plugin_hangar::screen::activity::{
    ActivityEvent, ActivityIntent, ActivityState, reduce_activity, render_activity,
};
use ainb_plugin_hangar::screen::issue_list::{
    IssueListEvent, IssueListIntent, IssueListState, reduce_issue_list,
};
use ainb_plugin_sdk::WireBuffer;

fn issue_id(s: &str) -> IssueId {
    IssueId::from_str(s).expect("valid id")
}

fn activity(id: &str, at: i64, action: &str, details: serde_json::Value) -> TimelineEntryRow {
    TimelineEntryRow {
        kind: "activity".into(),
        id: id.into(),
        actor_type: Some("member".into()),
        actor_id: Some("u1".into()),
        created_at: at,
        action: Some(action.into()),
        details: Some(details),
        body: None,
    }
}

fn comment(id: &str, at: i64, body: &str) -> TimelineEntryRow {
    TimelineEntryRow {
        kind: "comment".into(),
        id: id.into(),
        actor_type: Some("agent".into()),
        actor_id: Some("claude".into()),
        created_at: at,
        action: None,
        details: None,
        body: Some(body.into()),
    }
}

/// The seeded narrative used by both the reducer and render tests.
fn seeded() -> ActivityState {
    let mut state = ActivityState::loading(&issue_id("i1"), "Wire the timeline");
    state.apply_entries(vec![
        activity("a1", 36_840_000, "created", serde_json::json!({})),
        activity(
            "a2",
            36_900_000,
            "status_changed",
            serde_json::json!({"from": "open", "to": "in_progress"}),
        ),
        comment("c1", 36_960_000, "picked this up, starting now"),
    ]);
    state
}

// ---------------------------------------------------------------------------
// The `y` affordance on the issue list
// ---------------------------------------------------------------------------

/// One issue row with only the fields this test cares about set.
fn row(id: &str) -> ainb_hangar_proto::events::IssueRow {
    ainb_hangar_proto::events::IssueRow {
        subscriber_count: 0,
        subscribed: false,
        reactions: Vec::new(),
        last_dispatch_reason: None,
        last_dispatch_detail: None,
        last_dispatch_at: None,
        origin_type: None,
        origin_id: None,
        id: issue_id(id),
        display_id: None,
        workspace_id: "ws".to_string(),
        title: "Wire the timeline".to_string(),
        description: None,
        state: "open".to_string(),
        assignee: None,
        creator: "member:u1".to_string(),
        created_at: 0,
        priority: 0,
        due_date: None,
        labels: Vec::new(),
        pr_url: None,
        branch: None,
        repo_ref: None,
        agent: None,
        source_branch: None,
        target_branch: None,
        external_ref: None,
        run_count: 0,
        last_run_status: None,
        last_run_at: None,
        parent_id: None,
        child_total: 0,
        child_done: 0,
        acceptance_criteria: Vec::new(),
        acceptance: Vec::new(),
        context_refs: Vec::new(),
        dependencies: Vec::new(),
    }
}

#[test]
fn y_on_a_selected_row_opens_the_activity_timeline() {
    let state = IssueListState::with_rows(vec![row("i1")]);
    let out = reduce_issue_list(&state, IssueListEvent::Key('y'));
    assert_eq!(
        out.intent,
        Some(IssueListIntent::OpenActivityTimeline(issue_id("i1"))),
        "`y` on a selected row must open that row's timeline"
    );
}

#[test]
fn y_with_no_selection_is_a_no_op() {
    let state = IssueListState::default();
    let out = reduce_issue_list(&state, IssueListEvent::Key('y'));
    assert_eq!(
        out.intent, None,
        "`y` on an empty list must not open a modal on nothing"
    );
}

// ---------------------------------------------------------------------------
// The modal's own reducer
// ---------------------------------------------------------------------------

#[test]
fn j_and_k_move_the_cursor_and_clamp() {
    let state = seeded();
    assert_eq!(state.selected_index(), 0);

    let down = reduce_activity(&state, ActivityEvent::Key('j')).state;
    assert_eq!(down.selected_index(), 1);
    let down = reduce_activity(&down, ActivityEvent::Key('j')).state;
    assert_eq!(down.selected_index(), 2);
    // Clamped at the last entry.
    let down = reduce_activity(&down, ActivityEvent::Key('j')).state;
    assert_eq!(down.selected_index(), 2);

    let up = reduce_activity(&down, ActivityEvent::Key('k')).state;
    assert_eq!(up.selected_index(), 1);
    let up = reduce_activity(&up, ActivityEvent::Key('k')).state;
    let up = reduce_activity(&up, ActivityEvent::Key('k')).state;
    assert_eq!(up.selected_index(), 0, "clamped at the first entry");
}

#[test]
fn r_raises_the_refresh_intent_for_the_open_issue() {
    let out = reduce_activity(&seeded(), ActivityEvent::Key('r'));
    assert_eq!(
        out.intent,
        Some(ActivityIntent::Refresh {
            issue_id: "i1".to_string()
        })
    );
}

#[test]
fn applying_a_result_populates_entries_in_order_and_clears_loading() {
    let mut state = ActivityState::loading(&issue_id("i1"), "Wire the timeline");
    assert!(state.is_loading());
    assert!(state.entries().is_empty());

    state.apply_entries(vec![
        activity("a1", 1, "created", serde_json::json!({})),
        activity(
            "a2",
            2,
            "status_changed",
            serde_json::json!({"from": "open", "to": "done"}),
        ),
    ]);
    assert!(!state.is_loading());
    let ids: Vec<&str> = state.entries().iter().map(|e| e.id.as_str()).collect();
    assert_eq!(ids, ["a1", "a2"]);
}

// ---------------------------------------------------------------------------
// Render
// ---------------------------------------------------------------------------

/// Flatten the buffer into a `\n`-joined glyph map, each line `trim_end`-ed.
fn glyph_map(buf: &WireBuffer, cols: u16) -> String {
    let mut grid = vec![vec![' '; cols as usize]; buf.height as usize];
    for (coord, cell) in &buf.cells {
        if coord.y < buf.height && coord.x < cols {
            if let Some(ch) = cell.symbol.chars().next() {
                grid[coord.y as usize][coord.x as usize] = ch;
            }
        }
    }
    grid.into_iter()
        .map(|r| r.into_iter().collect::<String>().trim_end().to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn render_shows_the_exact_change_text_the_cursor_and_the_help_bar() {
    let state = seeded();
    let mut buf = WireBuffer::new(100, 20);
    render_activity(&mut buf, 100, 20, &state);
    let full = glyph_map(&buf, 100);

    assert!(
        full.contains("Activity · Wire the timeline"),
        "the modal title names the card:\n{full}"
    );
    // The EXACT change text, not a substring-OR — the whole point of the item.
    assert!(
        full.contains("open → in_progress"),
        "the state move must render as its from → to text:\n{full}"
    );
    assert!(
        full.contains("moved"),
        "the status_changed action renders as its human label:\n{full}"
    );
    assert!(
        full.contains("picked this up, starting now"),
        "the merged comment body must render:\n{full}"
    );
    assert!(
        full.contains("claude"),
        "an agent entry is attributed to the agent:\n{full}"
    );
    assert!(
        full.contains("you"),
        "a member entry renders as `you`:\n{full}"
    );
    assert!(
        full.contains('▶'),
        "the selection cursor must render:\n{full}"
    );
    assert!(
        full.contains("j/k scroll   r refresh   esc back"),
        "the bottom help bar must advertise the exits:\n{full}"
    );
}

/// The tolerant-read contract at the render layer: an action token this binary
/// does not know renders RAW rather than vanishing.
#[test]
fn an_unknown_action_token_renders_raw() {
    let mut state = ActivityState::loading(&issue_id("i1"), "Card");
    state.apply_entries(vec![activity(
        "a1",
        1,
        "teleported_from_2027",
        serde_json::json!({}),
    )]);
    let mut buf = WireBuffer::new(100, 20);
    render_activity(&mut buf, 100, 20, &state);
    let full = glyph_map(&buf, 100);
    assert!(
        full.contains("teleported"),
        "an unknown token must render raw, not be dropped:\n{full}"
    );
}
