//! P9.2 — the `o` (open-PR) keybinding on the task-detail screen.
//!
//! `o` raises a [`NavIntent::OpenPrUrl`] carrying the captured PR URL **only**
//! when the bound task has a `pr_url`; with no PR, `o` raises no intent (no
//! silent open of nothing). These pin the routing-layer gating; the actual
//! browser launch (the `Opener` side effect) is exercised by the
//! `tripwire_pr_badge` daemon tripwire.

use ainb_hangar_core::ids::WorkspaceId;
use ainb_hangar_core::ids::{IssueId, TaskId};
use ainb_hangar_proto::events::IssueRow;
use ainb_plugin_hangar::screen::{AppState, NavIntent, Screen, ScreenStates, route_key};
use ainb_plugin_protocol::params::{KeyCode, KeyEvent, KeyKind};

const fn key(ch: char) -> KeyEvent {
    KeyEvent {
        code: KeyCode::Char { ch },
        mods: 0,
        kind: KeyKind::Press,
    }
}

fn issue(pr_url: Option<&str>) -> IssueRow {
    IssueRow {
        subscriber_count: 0,
        subscribed: false,
        reactions: Vec::new(),
        properties: Vec::new(),
        metadata: Vec::new(),
        last_dispatch_reason: None,
        last_dispatch_detail: None,
        last_dispatch_at: None,
        origin_type: None,
        origin_id: None,
        id: IssueId::from_str("issue-1").unwrap(),
        display_id: None,
        workspace_id: "default".into(),
        title: "Refactor API".into(),
        description: None,
        state: "done".into(),
        assignee: Some("agent:claude-agent".into()),
        creator: "member:alice".into(),
        created_at: 0,
        priority: 0,
        due_date: None,
        labels: Vec::new(),
        pr_url: pr_url.map(String::from),
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

/// Build the routing state on the task-detail screen with a task open for
/// `issue`.
fn on_task_detail(pr_url: Option<&str>) -> (AppState, ScreenStates) {
    let task = TaskId::from_str("task-1").unwrap();
    let mut app = AppState::new(WorkspaceId::from_str("default").unwrap());
    app.screen = Screen::TaskDetail(task.clone());
    let mut states = ScreenStates::default();
    states.open_task_detail(task, issue(pr_url), None);
    (app, states)
}

/// `o` on a task WITH a PR raises `OpenPrUrl` carrying the URL.
#[test]
fn o_opens_pr_url_when_present() {
    let (app, mut states) = on_task_detail(Some("https://example.com/pr/1"));
    let nav = route_key(&app, &mut states, &key('o'));
    assert_eq!(
        nav,
        Some(NavIntent::OpenPrUrl("https://example.com/pr/1".into()))
    );
    // The task-detail state is preserved (not dropped on the floor).
    assert!(states.task_detail.is_some());
}

/// `o` on a task WITHOUT a PR raises no intent — no silent open of nothing.
#[test]
fn o_is_a_noop_when_no_pr_url() {
    let (app, mut states) = on_task_detail(None);
    let nav = route_key(&app, &mut states, &key('o'));
    assert_eq!(nav, None, "no PR → `o` must raise no intent");
    assert!(states.task_detail.is_some());
}

/// A non-`o` key (e.g. `j` scroll) raises no nav intent and folds into the
/// reducer, leaving the task-detail state present.
#[test]
fn other_keys_fold_into_the_reducer() {
    let (app, mut states) = on_task_detail(Some("https://example.com/pr/1"));
    let nav = route_key(&app, &mut states, &key('j'));
    assert_eq!(nav, None);
    assert!(states.task_detail.is_some());
}
