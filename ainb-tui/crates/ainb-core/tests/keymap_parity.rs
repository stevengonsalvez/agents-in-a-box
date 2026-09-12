//! Golden-table parity checks for the host keymap.

use ainb::app::{
    events::AppEvent,
    keymap::{Chord, KeyAction, KeyContext, Keymap, UiAction},
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde::Deserialize;

const DEFAULT_KEYMAP_GOLDEN: &str = include_str!("fixtures/keymap_rows.txt");

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct KeymapGolden {
    bindings: Vec<GoldenBinding>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GoldenBinding {
    context: String,
    chord: String,
    action: String,
}

fn golden_default_bindings() -> KeymapGolden {
    serde_json::from_str(DEFAULT_KEYMAP_GOLDEN)
        .expect("keymap golden fixture must be valid structured JSON")
}

fn golden_context(name: &str) -> Option<KeyContext> {
    KeyContext::from_name(name).or_else(|| match name {
        // These valid default screen contexts are intentionally unavailable to
        // keymap overrides, so `KeyContext::from_name` does not parse them.
        "attached_terminal" => Some(KeyContext::screen("attached_terminal")),
        "changelog" => Some(KeyContext::screen("changelog")),
        "claude_chat" => Some(KeyContext::screen("claude_chat")),
        "non_git_notification" => Some(KeyContext::screen("non_git_notification")),
        _ => None,
    })
}

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
fn default_rows_resolve_to_their_independent_golden_actions() {
    let golden = golden_default_bindings();
    assert_eq!(
        golden.bindings.len(),
        491,
        "golden fixture must cover every host binding"
    );

    let keymap = Keymap::defaults();
    let mut keys = std::collections::HashSet::new();

    for (index, binding) in golden.bindings.iter().enumerate() {
        let context = golden_context(&binding.context).unwrap_or_else(|| {
            panic!(
                "golden fixture row {} has unknown context {:?}",
                index + 1,
                binding.context
            )
        });
        let chord = Chord::parse(&binding.chord).unwrap_or_else(|error| {
            panic!(
                "golden fixture row {} has invalid chord {:?}: {error}",
                index + 1,
                binding.chord
            )
        });
        assert!(
            keys.insert((context.clone(), chord.clone())),
            "golden fixture row {} duplicates [{}] {}",
            index + 1,
            binding.context,
            binding.chord,
        );
        let resolved = keymap.resolve(&[context], &chord).map(|action| format!("{action:?}"));
        assert_eq!(
            resolved.as_deref(),
            Some(binding.action.as_str()),
            "golden fixture row {} [{}] {} resolved wrong action",
            index + 1,
            binding.context,
            binding.chord,
        );
    }

    assert_eq!(
        keymap.bindings().count(),
        golden.bindings.len(),
        "default table must contain no rows absent from golden fixture"
    );
}

#[test]
fn shifted_letter_rows_resolve_with_or_without_shift_modifier_bit() {
    let keymap = Keymap::defaults();

    for (index, binding) in
        golden_default_bindings().bindings.iter().enumerate().filter(|(_, binding)| {
            binding.chord.as_str().chars().count() == 1
                && binding.chord.as_str().chars().all(|character| character.is_ascii_uppercase())
        })
    {
        let context = golden_context(&binding.context).unwrap_or_else(|| {
            panic!(
                "golden fixture row {} has unknown context {:?}",
                index + 1,
                binding.context
            )
        });
        let character = binding.chord.chars().next().unwrap();
        for modifiers in [KeyModifiers::NONE, KeyModifiers::SHIFT] {
            let chord = Chord::from_key_event(&KeyEvent::new(KeyCode::Char(character), modifiers));
            assert_eq!(chord.as_str(), binding.chord);

            let resolved =
                keymap.resolve(&[context.clone()], &chord).map(|action| format!("{action:?}"));
            assert_eq!(
                resolved.as_deref(),
                Some(binding.action.as_str()),
                "golden fixture row {} [{}] {} must preserve its complete action",
                index + 1,
                binding.context,
                binding.chord,
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
