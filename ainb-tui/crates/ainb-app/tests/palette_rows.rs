#![allow(missing_docs)]

// ABOUTME: The reducer owns the rows a palette may offer (#1161): what exists,
// what it says, whether it would run now, and whether it can be named at all.
// A surface narrows that by what IT may send, and by nothing else.

use ainb_app::app::palette;
use ainb_app::{AppState, Keymap};

#[test]
fn a_row_that_writes_outside_ainb_is_never_nameable() {
    let keymap = Keymap::defaults();
    let state = AppState::new();
    let rows = palette::rows(&state, &keymap);
    assert!(!rows.is_empty(), "the keymap has rows to offer");

    for row in &rows {
        assert!(
            !keymap.is_key_only(&row.id),
            "a key-only row reached the palette: {}",
            row.id.as_str()
        );
    }
    // And the rule is the keymap's own, so every key-only row is missing.
    let offered: Vec<&str> = rows.iter().map(|row| row.id.as_str()).collect();
    for (id, binding) in keymap.commands() {
        if binding.key_only() {
            assert!(
                !offered.contains(&id.as_str()),
                "the palette offers `{}`, which writes outside ainb",
                id.as_str()
            );
        }
    }
}

#[test]
fn a_row_with_no_payload_to_run_with_is_never_nameable() {
    let keymap = Keymap::defaults();
    let rows = palette::rows(&AppState::new(), &keymap);
    let offered: Vec<&str> = rows.iter().map(|row| row.id.as_str()).collect();

    for (id, binding) in keymap.commands() {
        if binding.action.with_args(&serde_json::Value::Null).is_none() {
            assert!(
                !offered.contains(&id.as_str()),
                "the palette offers `{}`, whose action refuses an empty payload",
                id.as_str()
            );
        }
    }
}

#[test]
fn active_tracks_the_contexts_the_state_is_in() {
    let keymap = Keymap::defaults();
    let state = AppState::new();
    let contexts = ainb_app::app::keymap::command_contexts(&state);
    let rows = palette::rows(&state, &keymap);

    assert!(
        rows.iter().any(|row| row.active),
        "some row is runnable in the state the app starts in"
    );
    assert!(
        rows.iter().any(|row| !row.active),
        "and some row belongs to a context the state is not in"
    );
    for row in &rows {
        assert_eq!(
            row.active,
            contexts.contains(&row.context),
            "`{}` says active {} for context {}",
            row.id.as_str(),
            row.active,
            row.context.name()
        );
    }
}

#[test]
fn a_row_carries_what_a_palette_draws() {
    let keymap = Keymap::defaults();
    let rows = palette::rows(&AppState::new(), &keymap);
    let row = rows.first().expect("a row");

    assert!(!row.doc.is_empty(), "every row says what it does");
    assert!(
        !row.context.name().is_empty(),
        "and which context it belongs to"
    );
    // The chord is optional: a row reachable only by name has none, and that is
    // the row a palette exists for.
    assert!(
        rows.iter().any(|row| row.chord.is_none()),
        "some row is reachable only by name"
    );
}
