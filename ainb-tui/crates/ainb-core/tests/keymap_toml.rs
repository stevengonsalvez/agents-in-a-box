//! TOML override behavior for the data-owned terminal keymap.

use ainb::app::keymap::{Chord, KeyAction, KeyContext, Keymap, UiAction};
use ainb::app::keymap_toml::KeymapOverrides;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[test]
fn chord_normalises_terminal_spellings() {
    assert_eq!(Chord::parse("ctrl+k").unwrap().as_str(), "ctrl+k");
    assert_eq!(Chord::parse("CTRL+K").unwrap().as_str(), "ctrl+k");
    assert_eq!(Chord::parse("shift+tab").unwrap().as_str(), "shift+tab");
    assert_eq!(Chord::parse("G").unwrap().as_str(), "G");
    assert_eq!(Chord::parse("g g").unwrap().as_str(), "g g");
    assert!(Chord::parse("cmd+k").is_err());
}

#[test]
fn shifted_printable_terminal_keys_use_the_printed_character() {
    let event = KeyEvent::new(KeyCode::Char(':'), KeyModifiers::SHIFT);
    assert_eq!(Chord::from_key_event(&event).as_str(), ":");

    let event = KeyEvent::new(KeyCode::Char('G'), KeyModifiers::SHIFT);
    assert_eq!(Chord::from_key_event(&event).as_str(), "G");
}

#[test]
fn toml_override_replaces_default_binding() {
    let overrides = KeymapOverrides::parse(
        r#"
[session_list]
attach = "o"
"#,
    )
    .unwrap();
    let keymap = Keymap::defaults().with_overrides(&overrides).unwrap();

    assert!(
        keymap
            .binding_for(&KeyContext::screen("session_list"), "attach")
            .is_some_and(|binding| binding.chord.as_str() == "o")
    );
    assert!(matches!(
        keymap.resolve(
            &[KeyContext::screen("session_list")],
            &Chord::parse("o").unwrap(),
        ),
        Some(KeyAction::Ui(UiAction::SessionActivateSelected))
    ));
    assert!(
        keymap
            .resolve(
                &[KeyContext::screen("session_list")],
                &Chord::parse("enter").unwrap(),
            )
            .is_none()
    );
}

#[test]
fn embed_ctrl_c_override_is_rejected() {
    let overrides = KeymapOverrides::parse(
        r#"
[embed_interactive]
passthrough = "ctrl+c"
"#,
    )
    .unwrap();

    assert!(Keymap::defaults().with_overrides(&overrides).is_err());
}

#[test]
fn unknown_event_is_ignored_without_disabling_other_overrides() {
    let overrides = KeymapOverrides::parse(
        r#"
[session_list]
does_not_exist = "o"
attach = "a"
"#,
    )
    .unwrap();
    let keymap = Keymap::defaults().with_overrides(&overrides).unwrap();

    assert_eq!(
        keymap
            .binding_for(&KeyContext::screen("session_list"), "attach")
            .unwrap()
            .chord
            .as_str(),
        "a"
    );
}

#[test]
fn malformed_toml_is_rejected_before_defaults_change() {
    assert!(KeymapOverrides::parse("[session_list\nattach = \"o\"").is_err());
}
