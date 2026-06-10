//! P4.1 — Screen routing + shared chrome (RED → GREEN reducer tests).
//!
//! These are the canonical reducer cases named in `docs/hangar/phases/P4.md`
//! under `hangar:P4.1`. The reducer is pure (no IO): it maps an [`AppState`]
//! plus an [`AppEvent`] to a new [`AppState`] and an optional [`Intent`].

use ainb_hangar_core::ids::{TaskId, WorkspaceId};
use ainb_plugin_hangar::screen::{reduce, AppEvent, AppState, Intent, Screen};

/// Build a default `AppState` rooted on the issue list for workspace `default`.
fn issue_list_state() -> AppState {
    AppState::new(WorkspaceId::from_str("default").unwrap())
}

/// Pressing `1` always routes to the issue-list tab.
#[test]
fn reduce_tab_key_1_switches_to_issue_list() {
    // Start somewhere other than the issue list.
    let mut s = issue_list_state();
    s.screen = Screen::SkillManager;

    let out = reduce(&s, AppEvent::Key('1'));

    assert_eq!(out.state.screen, Screen::IssueList);
    assert!(out.intent.is_none());
}

/// Pressing `2` (task detail) is a no-op unless a task is currently selected.
#[test]
fn reduce_tab_key_2_only_valid_when_task_selected() {
    // No task selected → `2` must NOT switch away from the issue list.
    let s = issue_list_state();
    let out = reduce(&s, AppEvent::Key('2'));
    assert_eq!(out.state.screen, Screen::IssueList);

    // With a task selected, `2` routes to its task-detail screen.
    let mut s2 = issue_list_state();
    let task = TaskId::from_str("task-1").unwrap();
    s2.selected_task = Some(task.clone());
    let out2 = reduce(&s2, AppEvent::Key('2'));
    assert_eq!(out2.state.screen, Screen::TaskDetail(task));
}

/// Esc from a modal overlay returns to the screen that opened it.
///
/// The modal is opened from a **non-default** screen (`SkillManager`) so the
/// restore target diverges from the reducer's `IssueList` fallback. A
/// regression that dropped `prior_screen` tracking would restore `IssueList`
/// and fail this assertion — pinning the "returns to the *prior* screen"
/// contract rather than coincidentally matching the default.
#[test]
fn reduce_esc_from_modal_returns_to_prior_screen() {
    let mut s = issue_list_state();
    // Park on a non-default screen, then open the agent-picker modal from it.
    s.screen = Screen::SkillManager;
    let issue = ainb_hangar_core::ids::IssueId::from_str("issue-1").unwrap();
    let opened = reduce(&s, AppEvent::OpenAgentPicker(issue.clone()));
    assert_eq!(opened.state.screen, Screen::AgentPicker(issue));
    assert_eq!(opened.state.prior_screen, Some(Screen::SkillManager));

    let closed = reduce(&opened.state, AppEvent::Esc);
    // Must restore SkillManager (the prior), NOT the IssueList fallback.
    assert_eq!(closed.state.screen, Screen::SkillManager);
    assert!(closed.intent.is_none());
}

/// `?` opens the help overlay from any screen, remembering the prior screen,
/// and Esc closes it back to that prior screen.
#[test]
fn reduce_question_mark_opens_help_overlay_and_esc_closes_it() {
    // Open help from a non-default screen so the restore target diverges from
    // the `IssueList` Esc-fallback.
    let mut s = issue_list_state();
    s.screen = Screen::Settings;

    let opened = reduce(&s, AppEvent::Key('?'));
    assert_eq!(opened.state.screen, Screen::Help);
    assert_eq!(opened.state.prior_screen, Some(Screen::Settings));
    assert!(opened.intent.is_none());

    let closed = reduce(&opened.state, AppEvent::Esc);
    assert_eq!(closed.state.screen, Screen::Settings);
    assert!(closed.intent.is_none());
}

/// `q` emits the quit intent (and leaves the screen unchanged).
#[test]
fn reduce_q_emits_quit_intent() {
    let s = issue_list_state();
    let out = reduce(&s, AppEvent::Key('q'));
    assert_eq!(out.intent, Some(Intent::Quit));
}
