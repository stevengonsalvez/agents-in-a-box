//! P4.3 — Issue list screen (RED → GREEN reducer tests).
//!
//! These are the canonical reducer cases named in `docs/hangar/phases/P4.md`
//! under `hangar:P4.3`. The issue-list reducer is pure (no IO): it maps an
//! [`IssueListState`] plus an [`IssueListEvent`] to a new state and an optional
//! [`IssueListIntent`]. Rows are grouped by lifecycle status into the
//! Todo / In Progress / Done columns; filter chips narrow which rows show; host
//! [`HangarEvent`]s fold into the cached row set.

use ainb_hangar_core::ids::{AgentId, IssueId, TaskId};
use ainb_hangar_proto::events::{HangarEvent, IssueRow};
use ainb_plugin_hangar::screen::issue_list::{
    reduce_issue_list, FilterChip, IssueColumn, IssueListEvent, IssueListIntent, IssueListMode,
    IssueListState,
};

/// A wire `IssueRow` for tests. `state` drives column grouping (`open` → Todo,
/// `in_progress` → In Progress, `done` → Done); `assignee` drives the
/// member/agent filter chips.
fn row(id: &str, state: &str, assignee: Option<&str>) -> IssueRow {
    IssueRow {
        id: IssueId::from_str(id).unwrap(),
        workspace_id: "ws".to_string(),
        title: format!("Issue {id}"),
        description: None,
        state: state.to_string(),
        assignee: assignee.map(ToString::to_string),
        creator: "member:alice".to_string(),
        created_at: 0,
    }
}

/// A three-row fixture: two Todo, one In Progress, mixed assignees.
fn seeded_state() -> IssueListState {
    IssueListState::with_rows(vec![
        row("i1", "open", Some("member:alice")),
        row("i2", "open", Some("agent:claude")),
        row("i3", "in_progress", Some("agent:claude")),
    ])
}

/// `j` advances the row selection by one (vim-style down).
#[test]
fn j_key_moves_selection_down() {
    let s = seeded_state();
    assert_eq!(s.selected_index(), 0);

    let out = reduce_issue_list(&s, IssueListEvent::Key('j'));

    assert_eq!(out.state.selected_index(), 1);
    assert!(out.intent.is_none());
}

/// `enter` on the selected row emits an open-task-detail intent carrying that
/// row's issue id, so the router can route to the task-detail screen.
#[test]
fn enter_emits_open_task_detail_intent() {
    let s = seeded_state();

    let out = reduce_issue_list(&s, IssueListEvent::Key('\n'));

    assert_eq!(
        out.intent,
        Some(IssueListIntent::OpenTaskDetail(IssueId::from_str("i1").unwrap()))
    );
}

/// The `Agents` filter chip hides rows assigned to a human member, leaving only
/// agent-assigned rows visible.
#[test]
fn filter_chip_agents_hides_human_assigned_rows() {
    let mut s = seeded_state();
    s = reduce_issue_list(&s, IssueListEvent::SetFilter(FilterChip::Agents)).state;

    let visible: Vec<&str> = s.visible_rows().map(|r| r.id.as_str()).collect();

    // i1 is member-assigned → hidden; i2 + i3 are agent-assigned → shown.
    assert!(!visible.contains(&"i1"), "member row leaked: {visible:?}");
    assert!(visible.contains(&"i2"), "agent row missing: {visible:?}");
    assert!(visible.contains(&"i3"), "agent row missing: {visible:?}");
}

/// `/` switches the screen into filter-input mode (the user can then type a
/// query); it does not emit an intent.
#[test]
fn slash_enters_filter_input_mode() {
    let s = seeded_state();
    assert_eq!(s.mode(), IssueListMode::Normal);

    let out = reduce_issue_list(&s, IssueListEvent::Key('/'));

    assert_eq!(out.state.mode(), IssueListMode::FilterInput);
    assert!(out.intent.is_none());
}

/// `c` emits the create-issue intent (the router opens the create flow).
#[test]
fn c_emits_create_issue_intent() {
    let s = seeded_state();

    let out = reduce_issue_list(&s, IssueListEvent::Key('c'));

    assert_eq!(out.intent, Some(IssueListIntent::CreateIssue));
}

/// An `IssueCreated` host event lands the new row in the Todo column.
#[test]
fn event_issue_created_appears_in_todo_column() {
    let s = IssueListState::default();
    let new_row = row("i9", "open", None);

    let out = reduce_issue_list(&s, IssueListEvent::Event(HangarEvent::IssueCreated(new_row)));

    let todo: Vec<&str> = out
        .state
        .rows_in_column(IssueColumn::Todo)
        .map(|r| r.id.as_str())
        .collect();
    assert_eq!(todo, vec!["i9"]);
}

/// A `TaskStarted` host event promotes its issue out of Todo into In Progress.
#[test]
fn event_task_started_promotes_issue_to_in_progress() {
    // Seed one Todo issue, plus a queued task that maps a task id to that issue.
    let mut s = IssueListState::with_rows(vec![row("i1", "open", Some("agent:claude"))]);
    s = reduce_issue_list(
        &s,
        IssueListEvent::Event(HangarEvent::TaskQueued {
            task_id: TaskId::from_str("t1").unwrap(),
            issue_id: IssueId::from_str("i1").unwrap(),
            agent_id: AgentId::from_str("claude").unwrap(),
        }),
    )
    .state;

    let out = reduce_issue_list(
        &s,
        IssueListEvent::Event(HangarEvent::TaskStarted {
            task_id: TaskId::from_str("t1").unwrap(),
            started_at: chrono::Utc::now(),
        }),
    );

    // i1 left Todo …
    assert_eq!(out.state.rows_in_column(IssueColumn::Todo).count(), 0);
    // … and now sits in In Progress.
    let in_prog: Vec<&str> = out
        .state
        .rows_in_column(IssueColumn::InProgress)
        .map(|r| r.id.as_str())
        .collect();
    assert_eq!(in_prog, vec!["i1"]);
}
