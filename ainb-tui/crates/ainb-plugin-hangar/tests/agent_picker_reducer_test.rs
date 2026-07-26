//! P4.5 RED — agent-picker modal reducer + actor-row render behaviour.
//!
//! The agent picker is a modal overlay opened for a specific issue (`a` on a
//! selected issue-list row). It shows a polymorphic actor list (humans + agents
//! flat, recent-use pinned), a type-narrowing filter, and emits an assign intent
//! on Enter. These tests pin the pure reducer contract and the actor-row visual
//! differentiation (violet ring on agents, neutral on humans).

use ainb_hangar_core::ids::IssueId;
use ainb_hangar_proto::events::{ActorRow, PresenceState};
use ainb_plugin_hangar::screen::agent_picker::{
    AgentPickerEvent, AgentPickerIntent, AgentPickerState, reduce_agent_picker,
};
use ainb_plugin_hangar::widgets::actor_row::{AGENT_VIOLET, HUMAN_NEUTRAL, actor_glyph_color};
use ainb_plugin_sdk::WireBuffer;

fn issue() -> IssueId {
    IssueId::from_str("i1").unwrap()
}

fn actor(
    reff: &str,
    name: &str,
    is_agent: bool,
    presence: PresenceState,
    recent: Option<u32>,
) -> ActorRow {
    ActorRow {
        actor_ref: reff.into(),
        display_name: name.into(),
        subtitle: if is_agent {
            "agent · gpt5".into()
        } else {
            "backend dev".into()
        },
        presence,
        workload: ainb_hangar_proto::events::Workload::Idle,
        is_agent,
        recent_rank: recent,
    ..ActorRow::default()
    }
}

fn sample() -> Vec<ActorRow> {
    vec![
        actor(
            "member:alice",
            "alice",
            false,
            PresenceState::Online,
            Some(0),
        ),
        actor("member:bob", "bob", false, PresenceState::Offline, None),
        actor(
            "agent:claude-agent",
            "claude-agent",
            true,
            PresenceState::Online,
            Some(1),
        ),
        actor(
            "agent:codex-agent",
            "codex-agent",
            true,
            PresenceState::Unstable,
            None,
        ),
    ]
}

fn state() -> AgentPickerState {
    AgentPickerState::new(issue(), sample())
}

/// Enter on the selected actor emits an assign intent carrying that actor and
/// the issue the picker was opened for.
#[test]
fn enter_emits_assign_intent_with_selected_actor() {
    let s = state();
    // First visible actor is selected by default.
    let out = reduce_agent_picker(&s, AgentPickerEvent::Key('\n'));
    match out.intent {
        Some(AgentPickerIntent::Assign {
            issue_id,
            actor_ref,
        }) => {
            assert_eq!(issue_id, issue());
            assert!(!actor_ref.is_empty());
        }
        other => panic!("expected assign intent, got {other:?}"),
    }
}

/// Esc closes the modal without emitting an intent.
#[test]
fn esc_closes_without_intent() {
    let s = state();
    let out = reduce_agent_picker(&s, AgentPickerEvent::Esc);
    assert!(out.state.is_closed());
    assert!(out.intent.is_none());
}

/// `/` enters filter mode and typed characters narrow the visible list.
#[test]
fn slash_filter_narrows_visible_list() {
    let mut s = state();
    let before = s.visible_actors().len();
    s = reduce_agent_picker(&s, AgentPickerEvent::Key('/')).state;
    assert!(s.is_filtering());
    for c in "claude".chars() {
        s = reduce_agent_picker(&s, AgentPickerEvent::Key(c)).state;
    }
    let after = s.visible_actors();
    assert!(after.len() < before, "filter should narrow the list");
    assert!(after.iter().all(|a| a.display_name.contains("claude")));
}

/// Agent rows render a violet ring on the glyph; human rows render neutral gray.
#[test]
fn agent_rows_render_violet_ring_human_rows_render_neutral() {
    let agent = actor(
        "agent:claude-agent",
        "claude-agent",
        true,
        PresenceState::Online,
        None,
    );
    let human = actor("member:alice", "alice", false, PresenceState::Online, None);
    assert_eq!(actor_glyph_color(&agent), AGENT_VIOLET);
    assert_eq!(actor_glyph_color(&human), HUMAN_NEUTRAL);
    // Sanity: distinct colours.
    assert_ne!(AGENT_VIOLET, HUMAN_NEUTRAL);
}

/// Presence `Unstable` reflects a degraded runtime, not mere queueing: the row
/// renders the amber `◐` dot only for an `Unstable` presence.
#[test]
fn presence_unstable_only_when_runtime_degraded_not_just_queueing() {
    use ainb_plugin_hangar::widgets::actor_row::presence_dot;
    assert_eq!(presence_dot(PresenceState::Online).0, '●');
    assert_eq!(presence_dot(PresenceState::Unstable).0, '◐');
    assert_eq!(presence_dot(PresenceState::Offline).0, '○');
    // The amber dot is reserved for Unstable; Online is not amber.
    assert_ne!(
        presence_dot(PresenceState::Unstable).1,
        presence_dot(PresenceState::Online).1
    );
}

/// Recently-used actors are pinned ahead of the alphabetical body.
#[test]
fn recent_actors_pinned_first() {
    let s = state();
    let visible = s.visible_actors();
    // alice (rank 0) and claude-agent (rank 1) are pinned ahead of the rest.
    assert_eq!(visible[0].display_name, "alice");
    assert_eq!(visible[1].display_name, "claude-agent");
}

/// j/k move the selection within the visible list.
#[test]
fn j_k_moves_selection() {
    let s = state();
    let down = reduce_agent_picker(&s, AgentPickerEvent::Key('j')).state;
    assert_eq!(down.selected_index(), 1);
    let up = reduce_agent_picker(&down, AgentPickerEvent::Key('k')).state;
    assert_eq!(up.selected_index(), 0);
}

/// The modal renders within an 80x24 area without panicking.
#[test]
fn modal_renders_at_floor_size() {
    let s = state();
    let mut buf = WireBuffer::new(80, 24);
    ainb_plugin_hangar::screen::agent_picker::render_agent_picker(&mut buf, 80, 24, &s);
    assert!(!buf.cells.is_empty());
}
