// ABOUTME: Behavioural tests for the renderer contract's input side: an
// Intent handed to `dispatch` changes exactly the section it should, the
// command registry addresses every keymap row, and intents survive the wire.

use std::collections::HashSet;

use ainb_app::app::NoRenderer;
use ainb_app::{AppState, Btn, Chord, CommandId, Intent, Keymap, Pos, SectionId, dispatch};

/// The sections whose version moved between two snapshots.
fn bumped(before: &[u64], after: &[u64]) -> Vec<SectionId> {
    SectionId::ALL
        .into_iter()
        .filter(|id| before[id.index()] != after[id.index()])
        .collect()
}

#[test]
fn command_intent_bumps_only_the_section_it_changes() {
    let keymap = Keymap::defaults();
    let mut state = AppState::new();
    let before = state.versions();
    assert!(!state.shell.help_visible);

    dispatch(
        &mut state,
        &keymap,
        &mut NoRenderer,
        Intent::Command(CommandId::new("global.help"), serde_json::Value::Null),
    );

    assert!(
        state.shell.help_visible,
        "global.help opens the help overlay"
    );
    assert_eq!(bumped(&before, &state.versions()), vec![SectionId::Shell]);
}

#[test]
fn text_intent_bumps_only_the_section_holding_the_field() {
    let keymap = Keymap::defaults();
    let mut state = AppState::new();
    state
        .config
        .config_popup_state
        .open_text("Branch prefix", "", "branch_prefix", "");
    let before = state.versions();

    dispatch(
        &mut state,
        &keymap,
        &mut NoRenderer,
        Intent::Text("agents/".to_string()),
    );

    assert!(matches!(
        &state.config.config_popup_state.popup_type,
        ainb_app::components::config_popup::ConfigPopupType::TextInput { value, .. } if value == "agents/"
    ));
    assert_eq!(bumped(&before, &state.versions()), vec![SectionId::Config]);
}

#[test]
fn key_intent_resolves_through_the_keymap() {
    let keymap = Keymap::defaults();
    let mut state = AppState::new();

    dispatch(
        &mut state,
        &keymap,
        &mut NoRenderer,
        Intent::Key(Chord::parse("?").expect("valid chord")),
    );

    assert!(state.shell.help_visible);
}

#[test]
fn pointer_intent_without_a_renderer_changes_nothing() {
    let keymap = Keymap::defaults();
    let mut state = AppState::new();
    let before = state.versions();

    dispatch(
        &mut state,
        &keymap,
        &mut NoRenderer,
        Intent::Mouse(Pos { x: 3, y: 4 }, Btn::Left),
    );

    assert!(bumped(&before, &state.versions()).is_empty());
}

#[test]
fn unknown_command_changes_nothing() {
    let keymap = Keymap::defaults();
    let mut state = AppState::new();
    let before = state.versions();

    dispatch(
        &mut state,
        &keymap,
        &mut NoRenderer,
        Intent::Command(
            CommandId::new("global.no_such_row"),
            serde_json::Value::Null,
        ),
    );

    assert!(bumped(&before, &state.versions()).is_empty());
}

#[test]
fn every_keymap_row_is_a_registered_command() {
    let keymap = Keymap::defaults();
    let mut seen = HashSet::new();
    let mut shared = Vec::new();
    for (id, binding) in keymap.commands() {
        if !seen.insert(id.clone()) {
            shared.push(id.to_string());
        }
        // A command and a keymap.toml override address the same row.
        let found = keymap.command(&id).unwrap_or_else(|| panic!("`{id}` does not resolve"));
        let overridden = keymap.binding_for(&binding.ctx, binding.id).expect("row is addressable");
        assert_eq!(found.chord, overridden.chord, "`{id}` resolves elsewhere");
    }
    // The generated shortcut docs print row ids, so this pre-existing clash
    // (`r` resume and `e` restart) cannot be renamed here. New clashes fail.
    assert_eq!(shared, ["session_list.restart"]);
}

#[test]
fn intents_round_trip_through_json() {
    let intents = [
        Intent::Key(Chord::parse("ctrl+k").expect("valid chord")),
        Intent::Command(CommandId::new("global.help"), serde_json::json!({ "n": 1 })),
        Intent::Mouse(Pos { x: 10, y: 2 }, Btn::Right),
        Intent::Text("owner/repo".to_string()),
    ];
    for intent in intents {
        let wire = serde_json::to_string(&intent).expect("serialises");
        let back: Intent = serde_json::from_str(&wire).expect("deserialises");
        assert_eq!(back, intent, "{wire}");
    }
    assert_eq!(
        serde_json::to_string(&Intent::Key(Chord::parse("Ctrl+K").expect("valid chord")))
            .expect("serialises"),
        r#"{"Key":"ctrl+k"}"#
    );
    assert!(serde_json::from_str::<Intent>(r#"{"Key":"cmd+k"}"#).is_err());
}

#[test]
fn an_unbound_row_is_a_command_no_key_reaches_until_an_override_binds_it() {
    use ainb_app::app::events::AppEvent;
    use ainb_app::app::keymap::{Binding, KeyAction, KeyContext};
    use ainb_app::app::keymap_toml::KeymapOverrides;

    let mut rows = Keymap::defaults().bindings().cloned().collect::<Vec<_>>();
    rows.push(Binding {
        id: "help_from_palette",
        ctx: KeyContext::Global,
        chord: None,
        action: KeyAction::App(AppEvent::ToggleHelp),
        doc: "Toggle keyboard help from the palette",
    });
    let keymap = Keymap::new(rows).expect("an unbound row is valid");
    let id = CommandId::new("global.help_from_palette");
    assert!(
        keymap.commands().any(|(command, _)| command == id),
        "listed for a palette"
    );

    let mut state = AppState::new();
    let before = state.versions();
    dispatch(
        &mut state,
        &keymap,
        &mut NoRenderer,
        Intent::Command(id, serde_json::Value::Null),
    );
    assert!(state.shell.help_visible, "runs by name");
    assert_eq!(bumped(&before, &state.versions()), vec![SectionId::Shell]);

    let chord = Chord::parse("ctrl+g").expect("valid chord");
    assert!(
        keymap.resolve(&[KeyContext::Global], &chord).is_none(),
        "no key reaches it"
    );
    let overrides = KeymapOverrides::parse("[global]\nhelp_from_palette = \"ctrl+g\"\n")
        .expect("valid override");
    let bound = keymap.with_overrides(&overrides).expect("an override can bind it");
    assert!(matches!(
        bound.resolve(&[KeyContext::Global], &chord),
        Some(KeyAction::App(AppEvent::ToggleHelp))
    ));
}
