//! e38.8 — agent-picker Enter routing: the picker's Assign intent must be wired
//! through to a deferred `hangar/issue_update`, not discarded.
//!
//! The review BLOCKER was that `route_agent_picker` dropped the reducer's
//! [`AgentPickerIntent::Assign`]: pressing Enter closed the modal but never
//! reassigned the issue. These tests pin the routing-layer contract — Enter
//! must (1) stash a pending [`IssueAssignAction::Assign`] carrying the issue +
//! picked actor, and (2) dismiss the modal (return `CloseModal`, clear the
//! picker cache) — while Esc still closes with no pending action (assign is
//! opt-in on Enter).

use ainb_hangar_core::ids::{IssueId, WorkspaceId};
use ainb_hangar_proto::events::{ActorRow, PresenceState};
use ainb_plugin_hangar::screen::{AppState, IssueAssignAction, NavIntent, ScreenStates, route_key};
use ainb_plugin_sdk::{KeyCode, KeyEvent};

fn issue() -> IssueId {
    IssueId::from_str("issue-1").unwrap()
}

fn actor() -> ActorRow {
    ActorRow {
        actor_ref: "agent:claude-agent".into(),
        display_name: "claude-agent".into(),
        subtitle: "agent · gpt5".into(),
        presence: PresenceState::Online,
        workload: ainb_hangar_proto::events::Workload::Idle,
        is_agent: true,
        recent_rank: Some(0),
    ..ActorRow::default()
    }
}

/// Build an app parked on the agent-picker modal for `issue-1`, with the picker
/// seeded over a single actor (so the default selection assigns it).
fn picker_app() -> (AppState, ScreenStates) {
    let mut app = AppState::new(WorkspaceId::from_str("default").unwrap());
    app.screen = ainb_plugin_hangar::screen::Screen::AgentPicker(issue());
    app.prior_screen = Some(ainb_plugin_hangar::screen::Screen::IssueList);

    let mut states = ScreenStates::default();
    states.set_actors(vec![actor()]);
    states.open_picker(issue());
    (app, states)
}

const fn key(code: KeyCode) -> KeyEvent {
    KeyEvent {
        code,
        mods: 0,
        kind: ainb_plugin_sdk::KeyKind::Press,
    }
}

/// Enter on a picked actor stashes a pending assign action AND closes the
/// modal.
#[test]
fn enter_stashes_pending_assign_and_closes_modal() {
    let (app, mut states) = picker_app();

    let nav = route_key(&app, &mut states, &key(KeyCode::Enter));

    // The modal must dismiss back to its prior screen.
    assert_eq!(nav, Some(NavIntent::CloseModal));
    assert!(
        states.agent_picker.is_none(),
        "Enter must dismiss the picker modal"
    );

    // And the picked actor must be queued for the `hangar/issue_update` RPC.
    match states.take_pending_assign_action() {
        Some(IssueAssignAction::Assign {
            issue_id,
            actor_ref,
        }) => {
            assert_eq!(issue_id, "issue-1");
            assert_eq!(actor_ref, "agent:claude-agent");
        }
        other => panic!("expected a pending assign action, got {other:?}"),
    }
}

/// Esc closes the modal with NO pending assign (assign is Enter-only).
#[test]
fn esc_closes_without_pending_assign() {
    let (app, mut states) = picker_app();

    let nav = route_key(&app, &mut states, &key(KeyCode::Esc));

    assert_eq!(nav, Some(NavIntent::CloseModal));
    assert!(states.agent_picker.is_none());
    assert!(
        states.take_pending_assign_action().is_none(),
        "Esc must not queue an assign"
    );
}
