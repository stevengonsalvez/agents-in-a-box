//! Golden-table parity checks for the host keymap.

use ainb::app::{
    events::AppEvent,
    keymap::{Chord, KeyAction, KeyContext, Keymap, UiAction},
};

#[test]
fn defaults_are_unique_documented_and_parseable() {
    let keymap = Keymap::defaults();
    let mut keys = std::collections::HashSet::new();

    for binding in keymap.bindings() {
        assert!(keys.insert((binding.ctx.clone(), binding.chord.clone())));
        assert!(!binding.doc.trim().is_empty());
        assert_eq!(Chord::parse(binding.chord.as_str()).unwrap(), binding.chord);
    }

    let fixture_rows = include_str!("fixtures/keymap_rows.txt")
        .lines()
        .filter(|line| !line.trim().is_empty() && !line.starts_with('#'))
        .map(str::to_owned)
        .collect::<Vec<_>>();
    assert_eq!(
        fixture_rows.len(),
        491,
        "fixture must cover every host binding"
    );
    assert_eq!(
        keymap.bindings().count(),
        491,
        "default table must be complete"
    );
    let table_rows = keymap
        .bindings()
        .map(|binding| {
            format!(
                "{} | {} | {}",
                binding.ctx.name(),
                binding.chord.as_str(),
                binding.id
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(fixture_rows, table_rows, "fixture must equal default table");
}

#[test]
fn host_routes_respect_context_priority() {
    let keymap = Keymap::defaults();
    let chord = |value| Chord::parse(value).unwrap();

    assert!(matches!(
        keymap.resolve(
            &[KeyContext::ConfirmDialog, KeyContext::Global],
            &chord("esc"),
        ),
        Some(KeyAction::App(AppEvent::ConfirmationCancel))
    ));
    assert!(matches!(
        keymap.resolve(
            &[
                KeyContext::from_name("session_list.ask").unwrap(),
                KeyContext::screen("session_list"),
                KeyContext::Global,
            ],
            &chord("enter"),
        ),
        Some(KeyAction::App(AppEvent::SessionAskSend))
    ));
    assert!(matches!(
        keymap.resolve(
            &[KeyContext::screen("session_list"), KeyContext::Global],
            &chord("enter"),
        ),
        Some(KeyAction::Ui(UiAction::SessionActivateSelected))
    ));
    assert!(matches!(
        keymap.resolve(
            &[KeyContext::EmbedInteractive, KeyContext::Global],
            &chord("ctrl+q"),
        ),
        Some(KeyAction::App(AppEvent::DetachSession))
    ));
    assert!(matches!(
        keymap.resolve(
            &[KeyContext::TextInput, KeyContext::screen("session_list")],
            &chord("d"),
        ),
        Some(KeyAction::Text('d'))
    ));
    assert!(matches!(
        keymap.resolve(&[KeyContext::Global], &chord("q")),
        Some(KeyAction::App(AppEvent::GoToHomeScreen))
    ));
    assert!(keymap.resolve(&[KeyContext::Global], &chord("ctrl+z")).is_none());
}
