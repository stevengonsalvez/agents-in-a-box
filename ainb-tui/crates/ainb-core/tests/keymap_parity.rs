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

/// Rewrite `fixtures/keymap_rows.txt` from the live table.
///
/// The golden is the contract, so it is never rewritten by a test run: set
/// `UPDATE_KEYMAP_GOLDEN=1` deliberately, read the diff, and commit it. Without
/// the variable this is a no-op, which is what keeps the fixture a check rather
/// than an echo of whatever the table currently says.
#[test]
fn update_golden_when_asked() {
    if std::env::var_os("UPDATE_KEYMAP_GOLDEN").is_none() {
        return;
    }

    let keymap = Keymap::defaults();
    let rows: Vec<serde_json::Value> = keymap
        .bindings()
        .map(|binding| {
            serde_json::json!({
                "context": binding.ctx.name(),
                "chord": binding.chord.as_ref().expect("built-in rows are bound").as_str(),
                "action": format!("{:?}", binding.action),
            })
        })
        .collect();
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("keymap_rows.txt");
    // One row per line, in the table's own order, so the diff of a table change
    // is the rows that changed and nothing else. `to_string_pretty` would
    // reflow every row onto five lines and bury a four-row addition in a
    // 3,000-line diff.
    let mut body = String::from("{\n  \"bindings\": [\n");
    for (index, row) in rows.iter().enumerate() {
        let comma = if index + 1 == rows.len() { "" } else { "," };
        body.push_str(&format!(
            "    {}{comma}\n",
            serde_json::to_string(row).expect("row")
        ));
    }
    body.push_str("  ]\n}\n");
    std::fs::write(&path, body).expect("write golden");
    eprintln!("rewrote {} with {} rows", path.display(), rows.len());
}

#[test]
fn defaults_are_unique_documented_and_parseable() {
    let keymap = Keymap::defaults();
    let mut keys = std::collections::HashSet::new();

    for binding in keymap.bindings() {
        // Unbound rows are for commands a palette adds; every built-in has a key.
        let chord = binding.chord.as_ref().expect("built-in rows are bound");
        assert!(keys.insert((binding.ctx.clone(), chord.clone())));
        assert!(!binding.doc.trim().is_empty());
        assert_eq!(&Chord::parse(chord.as_str()).unwrap(), chord);
    }

    assert_eq!(
        keymap.bindings().count(),
        525,
        "default table must be complete"
    );
}

#[test]
fn default_rows_resolve_to_their_independent_golden_actions() {
    let golden = golden_default_bindings();
    assert_eq!(
        golden.bindings.len(),
        525,
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
            let chord = ainb::app::terminal_keys::chord_from_key_event(&KeyEvent::new(
                KeyCode::Char(character),
                modifiers,
            ))
            .expect("mapped key");
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
