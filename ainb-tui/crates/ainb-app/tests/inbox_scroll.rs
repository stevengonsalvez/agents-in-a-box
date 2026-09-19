#![allow(missing_docs)]

// ABOUTME: The inbox screen's scroll is the reducer's and bounded by it: a
// key moves the first drawn row one step inside the rows the section holds,
// and a read that shrinks the list pulls the row back inside it.

use ainb_app::app::NoRenderer;
use ainb_app::app::screens::ids;
use ainb_app::{AppState, Chord, Intent, Keymap, dispatch};
use ainb_hangar_proto::events::InboxEntryRow;
use ainb_hangar_proto::snapshots::InboxListResult;

fn read(n: usize) -> InboxListResult {
    InboxListResult {
        entries: (0..n)
            .map(|i| InboxEntryRow {
                id: format!("01J0{i}"),
                kind: "issue".into(),
                event: "issue_created".into(),
                subject_id: format!("issue-{i}"),
                summary: format!("New issue: {i}"),
                recipient: "member:me".into(),
                created_at: i as i64,
                read_at: None,
            })
            .collect(),
        unread: n as i64,
    }
}

fn on_inbox(rows: usize) -> AppState {
    let mut state = AppState::new();
    state.shell.current_screen = ids::INBOX.to_string();
    state.apply_inbox_read(read(rows), 5);
    state
}

fn press(state: &mut AppState, keymap: &Keymap, chord: &str) {
    let _ = dispatch(
        state,
        keymap,
        &mut NoRenderer,
        Intent::Key(Chord::parse(chord).expect("chord")),
    );
}

#[test]
fn j_and_k_move_the_first_row_inside_the_rows_the_section_holds() {
    let keymap = Keymap::defaults();
    let mut state = on_inbox(3);
    press(&mut state, &keymap, "k");
    assert_eq!(state.host.inbox_scroll, 0, "the top is the top");
    for _ in 0..5 {
        press(&mut state, &keymap, "j");
    }
    assert_eq!(state.host.inbox_scroll, 2, "the last row bounds the scroll");
    press(&mut state, &keymap, "up");
    assert_eq!(state.host.inbox_scroll, 1);
    press(&mut state, &keymap, "down");
    assert_eq!(state.host.inbox_scroll, 2);
}

#[test]
fn a_read_that_shrinks_the_list_pulls_the_first_row_back_inside_it() {
    let keymap = Keymap::defaults();
    let mut state = on_inbox(10);
    for _ in 0..9 {
        press(&mut state, &keymap, "j");
    }
    assert_eq!(state.host.inbox_scroll, 9);
    state.apply_inbox_read(read(2), 6);
    assert_eq!(state.host.inbox_scroll, 1);
    state.apply_inbox_read(read(0), 7);
    assert_eq!(
        state.host.inbox_scroll, 0,
        "an empty list draws from the top"
    );
}

#[test]
fn a_scroll_moves_no_section_version() {
    let keymap = Keymap::defaults();
    let mut state = on_inbox(3);
    let before = state.inbox.version();
    press(&mut state, &keymap, "j");
    assert_eq!(state.inbox.version(), before, "the scroll is host-only");
}
