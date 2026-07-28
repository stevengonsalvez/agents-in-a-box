//! P7.5 RED — autopilot-manager reducer behaviour.
//!
//! The autopilot manager (hotkey `4`) is a table + run-history pane. These tests
//! pin the pure reducer contract: list navigation (and the run-history load it
//! raises), the fire-now intent (`r`), the enabled toggle (`d`, false→true), the
//! add/edit intents, and the event-fold that refreshes a row in place.

use ainb_hangar_proto::events::{AutopilotRow, HangarEvent};
use ainb_plugin_hangar::screen::autopilots::{
    AutopilotsEvent, AutopilotsIntent, AutopilotsState, reduce_autopilots,
};

fn autopilot(id: &str, name: &str, enabled: bool) -> AutopilotRow {
    AutopilotRow {
        id: id.into(),
        workspace_id: "default".into(),
        agent_id: "agent-1".into(),
        name: name.into(),
        cron_expr: "0 9 * * *".into(),
        next_tick_at: Some(1_700_000_300_000),
        enabled,
        last_run_status: Some("completed".into()),
        last_run_at: Some(1_699_000_000_000),
        api_trigger_enabled: false,
        ..Default::default()
    }
}

fn state() -> AutopilotsState {
    AutopilotsState::new(vec![
        autopilot("ap-1", "daily-triage", true),
        autopilot("ap-2", "nightly-clean", true),
        autopilot("ap-3", "weekly-report", false),
    ])
}

/// j/k move the selection and raise a load-runs intent for the new row.
#[test]
fn j_k_navigates_and_loads_runs() {
    let s = state();
    assert_eq!(s.selected_index(), 0);
    let down = reduce_autopilots(&s, AutopilotsEvent::Key('j'));
    assert_eq!(down.state.selected_index(), 1);
    assert_eq!(
        down.intent,
        Some(AutopilotsIntent::LoadRuns("ap-2".into())),
        "moving the selection must pull the new row's run history"
    );
    let up = reduce_autopilots(&down.state, AutopilotsEvent::Key('k'));
    assert_eq!(up.state.selected_index(), 0);
    assert_eq!(up.intent, Some(AutopilotsIntent::LoadRuns("ap-1".into())));
}

/// `r` fires the selected autopilot now (`hangar/autopilot_fire_now`).
#[test]
fn key_r_emits_fire_now_intent() {
    let s = state();
    let out = reduce_autopilots(&s, AutopilotsEvent::Key('r'));
    assert_eq!(out.intent, Some(AutopilotsIntent::FireNow("ap-1".into())));
}

/// `d` toggles the selected autopilot's enabled flag: an enabled row → `false`,
/// then a disabled row → `true`.
#[test]
fn key_d_toggles_enabled() {
    // ap-1 is enabled → `d` requests disable (false).
    let s = state();
    let out = reduce_autopilots(&s, AutopilotsEvent::Key('d'));
    assert_eq!(
        out.intent,
        Some(AutopilotsIntent::SetEnabled {
            autopilot_id: "ap-1".into(),
            enabled: false,
        })
    );

    // Move to ap-3 (disabled) → `d` requests enable (true).
    let down = reduce_autopilots(&s, AutopilotsEvent::Key('j')).state;
    let down = reduce_autopilots(&down, AutopilotsEvent::Key('j')).state;
    assert_eq!(down.selected_autopilot().unwrap().id, "ap-3");
    let out = reduce_autopilots(&down, AutopilotsEvent::Key('d'));
    assert_eq!(
        out.intent,
        Some(AutopilotsIntent::SetEnabled {
            autopilot_id: "ap-3".into(),
            enabled: true,
        })
    );
}

/// `a` opens the create flow; `e` edits the selected autopilot.
#[test]
fn key_a_and_e_emit_create_intents() {
    let s = state();
    assert_eq!(
        reduce_autopilots(&s, AutopilotsEvent::Key('a')).intent,
        Some(AutopilotsIntent::Add)
    );
    assert_eq!(
        reduce_autopilots(&s, AutopilotsEvent::Key('e')).intent,
        Some(AutopilotsIntent::Edit("ap-1".into()))
    );
}

/// An `AutopilotUpdated` event refreshes the matching row in place (e.g. an
/// enable toggle reflected from the daemon).
#[test]
fn event_autopilot_updated_refreshes_row() {
    let s = state();
    let mut updated = autopilot("ap-1", "daily-triage", false);
    updated.last_run_status = Some("failed".into());
    let out = reduce_autopilots(
        &s,
        AutopilotsEvent::Event(HangarEvent::AutopilotUpdated(updated)),
    );
    let row = out.state.autopilots().iter().find(|a| a.id == "ap-1").unwrap();
    assert!(
        !row.enabled,
        "the updated row's enabled flag must be folded in"
    );
    assert_eq!(row.last_run_status.as_deref(), Some("failed"));
}

/// An empty list is inert: navigation + actions are no-ops with no intent.
#[test]
fn empty_list_keys_are_noops() {
    let s = AutopilotsState::new(Vec::new());
    for key in ['j', 'k', 'r', 'd', 'e'] {
        let out = reduce_autopilots(&s, AutopilotsEvent::Key(key));
        assert_eq!(out.state, s, "key {key} must not change empty state");
        assert!(
            out.intent.is_none(),
            "key {key} must raise no intent on empty list"
        );
    }
    // `a` (add) is always available.
    assert_eq!(
        reduce_autopilots(&s, AutopilotsEvent::Key('a')).intent,
        Some(AutopilotsIntent::Add)
    );
}
