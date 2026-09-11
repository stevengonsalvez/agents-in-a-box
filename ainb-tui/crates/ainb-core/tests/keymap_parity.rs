//! Golden-table parity checks for the host keymap.

use ainb::app::{
    events::AppEvent,
    keymap::{Chord, KeyAction, KeyContext, Keymap, UiAction},
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[test]
fn defaults_are_unique_documented_and_parseable() {
    let keymap = Keymap::defaults();
    let mut keys = std::collections::HashSet::new();

    for binding in keymap.bindings() {
        assert!(keys.insert((binding.ctx.clone(), binding.chord.clone())));
        assert!(!binding.doc.trim().is_empty());
        assert_eq!(Chord::parse(binding.chord.as_str()).unwrap(), binding.chord);
    }

    assert_eq!(
        keymap.bindings().count(),
        491,
        "default table must be complete"
    );
}

#[test]
fn default_rows_resolve_to_their_declared_actions() {
    let keymap = Keymap::defaults();

    for binding in keymap.bindings() {
        let resolved = keymap
            .resolve(&[binding.ctx.clone()], &binding.chord)
            .expect("default binding must resolve in its own context");
        assert_eq!(
            format!("{resolved:?}"),
            format!("{:?}", binding.action),
            "{} [{}] must preserve its complete action",
            binding.id,
            binding.ctx.name(),
        );
    }
}

#[test]
fn shifted_letter_rows_resolve_with_or_without_shift_modifier_bit() {
    let keymap = Keymap::defaults();

    for binding in keymap.bindings().filter(|binding| {
        binding.chord.as_str().chars().count() == 1
            && binding.chord.as_str().chars().all(|character| character.is_ascii_uppercase())
    }) {
        let character = binding.chord.as_str().chars().next().unwrap();
        for modifiers in [KeyModifiers::NONE, KeyModifiers::SHIFT] {
            let chord = Chord::from_key_event(&KeyEvent::new(KeyCode::Char(character), modifiers));
            assert_eq!(chord, binding.chord);

            let resolved = keymap
                .resolve(&[binding.ctx.clone()], &chord)
                .expect("shifted default binding must resolve in its own context");
            assert_eq!(
                format!("{resolved:?}"),
                format!("{:?}", binding.action),
                "{} [{}] must preserve its complete action",
                binding.id,
                binding.ctx.name(),
            );
        }
    }
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
