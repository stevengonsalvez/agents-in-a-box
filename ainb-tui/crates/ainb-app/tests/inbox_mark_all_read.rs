#![allow(missing_docs)]

// ABOUTME: One held key is one sweep. `inbox.mark_all_read` emits its effect
// once and then nothing until the host's report lands, and the report folds
// the daemon's count and the daemon's rows, never a local clock.

use ainb_app::app::NoRenderer;
use ainb_app::app::reports;
use ainb_app::app::screens::ids;
use ainb_app::fleet::inbox_write::MarkAllReadOutcome;
use ainb_app::{AppState, CommandId, Effect, Intent, Keymap, dispatch};
use ainb_hangar_proto::events::InboxEntryRow;
use ainb_hangar_proto::snapshots::InboxListResult;

fn on_inbox() -> AppState {
    let mut state = AppState::new();
    state.shell.current_screen = ids::INBOX.to_string();
    state.apply_inbox_read(
        InboxListResult {
            entries: vec![InboxEntryRow {
                id: "01J0ONE".into(),
                kind: "issue".into(),
                event: "issue_created".into(),
                subject_id: "issue-1".into(),
                summary: "New issue: one".into(),
                recipient: "member:me".into(),
                created_at: 1,
                read_at: None,
            }],
            unread: 1,
        },
        5,
    );
    state
}

fn mark(state: &mut AppState, keymap: &Keymap) -> Vec<Effect> {
    dispatch(
        state,
        keymap,
        &mut NoRenderer,
        Intent::Command(
            CommandId::new("inbox.mark_all_read"),
            serde_json::Value::Null,
        ),
    )
}

fn sweeps(effects: &[Effect]) -> usize {
    effects.iter().filter(|e| matches!(e, Effect::InboxMarkAllRead)).count()
}

#[test]
fn a_held_key_is_one_sweep_until_the_report_lands() {
    let keymap = Keymap::defaults();
    let mut state = on_inbox();
    assert_eq!(sweeps(&mark(&mut state, &keymap)), 1, "the first press sends the sweep");
    assert_eq!(sweeps(&mark(&mut state, &keymap)), 0, "a second press in flight sends nothing");
    assert_eq!(sweeps(&mark(&mut state, &keymap)), 0);

    let report = reports::inbox_mark_all_read_finished(&MarkAllReadOutcome {
        op_id: "0123456789abcdef0123456789abcdef".into(),
        ok: true,
        marked: 1,
        unread: 0,
        error: None,
        after: None,
    });
    let effects = dispatch(&mut state, &keymap, &mut NoRenderer, report);
    assert!(effects.is_empty(), "a report queues no effect");
    assert_eq!(state.inbox.get().unread, 0, "the daemon's count is folded");
    assert_eq!(sweeps(&mark(&mut state, &keymap)), 1, "after the report a press sends again");
}

#[test]
fn the_report_folds_the_daemons_rows_and_never_a_local_stamp() {
    let keymap = Keymap::defaults();
    let mut state = on_inbox();
    let _ = mark(&mut state, &keymap);
    let mut row = state.inbox.get().entries[0].clone();
    row.read_at = Some(777);
    let report = reports::inbox_mark_all_read_finished(&MarkAllReadOutcome {
        op_id: "0123456789abcdef0123456789abcdef".into(),
        ok: true,
        marked: 1,
        unread: 0,
        error: None,
        after: Some(InboxListResult {
            entries: vec![row],
            unread: 0,
        }),
    });
    let _ = dispatch(&mut state, &keymap, &mut NoRenderer, report);
    assert_eq!(state.inbox.get().entries[0].read_at, Some(777), "the stamp is the daemon's");
}

#[test]
fn a_failed_sweep_clears_the_guard_and_flips_nothing() {
    let keymap = Keymap::defaults();
    let mut state = on_inbox();
    let _ = mark(&mut state, &keymap);
    let report = reports::inbox_mark_all_read_finished(&MarkAllReadOutcome {
        op_id: "0123456789abcdef0123456789abcdef".into(),
        ok: false,
        marked: 0,
        unread: 0,
        error: Some("daemon io: gone".into()),
        after: None,
    });
    let _ = dispatch(&mut state, &keymap, &mut NoRenderer, report);
    assert_eq!(state.inbox.get().unread, 1, "nothing flipped on a failure");
    assert_eq!(sweeps(&mark(&mut state, &keymap)), 1, "the guard is released by the failed report");
}
