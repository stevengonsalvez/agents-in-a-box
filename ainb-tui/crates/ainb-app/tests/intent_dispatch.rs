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

    let _ = dispatch(
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

    let _ = dispatch(
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

    let _ = dispatch(
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

    let _ = dispatch(
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

    let _ = dispatch(
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
    use ainb_app::app::keymap::{Binding, KeyContext};
    use ainb_app::app::keymap_toml::KeymapOverrides;

    let defaults = Keymap::defaults();
    // The same action `global.help` runs, on a row with no key.
    let toggle_help = defaults
        .command(&CommandId::new("global.help"))
        .expect("global.help row")
        .action
        .clone();
    let mut rows = defaults.bindings().cloned().collect::<Vec<_>>();
    rows.push(Binding {
        id: "help_from_palette",
        ctx: KeyContext::Global,
        chord: None,
        action: toggle_help.clone(),
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
    let _ = dispatch(
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
    assert_eq!(
        format!("{:?}", bound.resolve(&[KeyContext::Global], &chord)),
        format!("{:?}", Some(toggle_help)),
        "the override binds the row's own action"
    );
}

#[test]
fn command_args_replace_the_payload_of_a_row_that_carries_one() {
    let keymap = Keymap::defaults();
    let mut state = AppState::new();
    state.shell.current_screen = ainb_app::app::screens::ids::SESSION_LIST.to_string();
    let attach_one = || CommandId::new("session_list.attach_one");

    // The row attaches position 1; the argument asks for position 7, which an
    // empty list does not have, and the notice names the argument.
    let _ = dispatch(
        &mut state,
        &keymap,
        &mut NoRenderer,
        Intent::Command(attach_one(), serde_json::json!(7)),
    );
    let latest = state.shell.notifications.last().map(|note| note.message.clone());
    assert_eq!(latest.as_deref(), Some("No session at position 7"));
}

#[test]
fn command_args_that_do_not_fit_the_row_change_nothing() {
    let keymap = Keymap::defaults();
    let rejected = [
        // A row with no payload takes only Null.
        ("global.help", serde_json::json!({ "n": 1 })),
        // Wrong payload type.
        ("session_list.attach_one", serde_json::json!("seven")),
        // Positions count from 1.
        ("session_list.attach_one", serde_json::json!(0)),
    ];
    for (id, args) in rejected {
        let mut state = AppState::new();
        state.shell.current_screen = ainb_app::app::screens::ids::SESSION_LIST.to_string();
        let before = state.versions();
        let effects = dispatch(
            &mut state,
            &keymap,
            &mut NoRenderer,
            Intent::Command(CommandId::new(id), args.clone()),
        );
        assert!(effects.is_empty(), "{id} {args}");
        assert!(bumped(&before, &state.versions()).is_empty(), "{id} {args}");
    }
}

// ---------------------------------------------------------------------------
// The remote command gate judges state-dependent rows by state (#1080)
// ---------------------------------------------------------------------------

fn command(id: &str) -> Intent {
    Intent::Command(CommandId::new(id), serde_json::Value::Null)
}

/// A dialog whose highlighted option is `action`, then Cancel.
fn dialog_over(
    action: ainb_app::app::state::ConfirmAction,
) -> ainb_app::app::state::ConfirmationDialog {
    use ainb_app::app::state::{ConfirmAction, ConfirmationDialog, DialogOption};
    ConfirmationDialog {
        title: "Confirm".to_string(),
        message: String::new(),
        confirm_action: action.clone(),
        selected_option: false,
        warning: None,
        options: Some(vec![
            DialogOption {
                label: "Go".to_string(),
                action,
            },
            DialogOption {
                label: "Cancel".to_string(),
                action: ConfirmAction::Cancel,
            },
        ]),
        selected_index: 0,
    }
}

#[test]
fn a_remote_confirm_over_the_hook_install_is_refused() {
    let keymap = Keymap::defaults();
    let mut state = AppState::new();
    state.shell.confirmation_dialog = Some(dialog_over(
        ainb_app::app::state::ConfirmAction::InstallNotifyHooks,
    ));

    let effects = dispatch(
        &mut state,
        &keymap,
        &mut NoRenderer,
        command("confirm_dialog.confirm"),
    );

    assert!(effects.is_empty(), "{effects:?}");
    assert!(
        state.shell.confirmation_dialog.is_some(),
        "the refused confirm left the dialog open"
    );
}

#[test]
fn a_remote_confirm_over_a_session_delete_runs() {
    let keymap = Keymap::defaults();
    let mut state = AppState::new();
    state.shell.confirmation_dialog = Some(dialog_over(
        ainb_app::app::state::ConfirmAction::DeleteSession(uuid::Uuid::nil()),
    ));

    let _ = dispatch(
        &mut state,
        &keymap,
        &mut NoRenderer,
        command("confirm_dialog.confirm"),
    );

    assert!(
        state.shell.confirmation_dialog.is_none(),
        "the confirm ran and closed the dialog"
    );
}

/// The onboarding wizard on its welcome step, telemetry skipped or typed in.
fn onboarding(otel_skip: bool) -> AppState {
    use ainb_app::components::onboarding::{OnboardingState, OnboardingStep};
    let mut state = AppState::new();
    state.shell.current_screen = "onboarding".to_string();
    state.onboarding.onboarding_state = Some(OnboardingState {
        current_step: OnboardingStep::Welcome,
        otel_skip,
        otel_otlp_endpoint: "https://otlp.example.test".to_string(),
        otel_instance_id: "123".to_string(),
        otel_api_token: "token".to_string(),
        ..OnboardingState::default()
    });
    state
}

fn onboarding_step(state: &AppState) -> ainb_app::components::onboarding::OnboardingStep {
    state
        .onboarding
        .onboarding_state
        .as_ref()
        .expect("the wizard is open")
        .current_step
}

#[test]
fn a_remote_onboarding_next_with_telemetry_set_up_is_refused() {
    use ainb_app::components::onboarding::OnboardingStep;
    let keymap = Keymap::defaults();
    let mut state = onboarding(false);

    let _ = dispatch(
        &mut state,
        &keymap,
        &mut NoRenderer,
        command("onboarding.welcome.next"),
    );

    assert_eq!(onboarding_step(&state), OnboardingStep::Welcome);
}

#[test]
fn a_remote_onboarding_next_with_telemetry_skipped_runs() {
    use ainb_app::components::onboarding::OnboardingStep;
    let keymap = Keymap::defaults();
    let mut state = onboarding(true);

    let _ = dispatch(
        &mut state,
        &keymap,
        &mut NoRenderer,
        command("onboarding.welcome.next"),
    );

    assert_ne!(onboarding_step(&state), OnboardingStep::Welcome);
}
