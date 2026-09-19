#![allow(missing_docs)]

// ABOUTME: A settings form names the row it edits by key and sends the value
// as one pointer command, `config.set_row`. The reducer resolves it against
// the row's own kind, marks only that row dirty, and persists through the same
// key-level write the terminal's popup uses, so a window never saves the whole
// config from the snapshot it loaded at startup (#1175, D3d).

use ainb_app::app::NoRenderer;
use ainb_app::app::effect::Persist;
use ainb_app::app::pointer;
use ainb_app::app::screens::ids as screen_ids;
use ainb_app::config::settings_model::{ConfigRowEdit, ConfigValue};
use ainb_app::{AppState, Effect, Keymap, SectionId, dispatch};

fn bumped(before: &[u64], after: &[u64]) -> Vec<SectionId> {
    SectionId::ALL
        .into_iter()
        .filter(|id| before[id.index()] != after[id.index()])
        .collect()
}

/// The reducer on the Config screen, under a scratch home.
fn on_config() -> AppState {
    let home = tempfile::tempdir().expect("scratch home");
    std::env::set_var("HOME", home.path());
    std::mem::forget(home);
    let mut state = AppState::new();
    state.shell.current_screen = screen_ids::CONFIG.to_string();
    state
}

fn row_value(state: &AppState, key: &str) -> ConfigValue {
    state
        .config
        .config_screen_state
        .settings
        .values()
        .flatten()
        .find(|row| row.key == key)
        .unwrap_or_else(|| panic!("row {key}"))
        .value
        .clone()
}

/// The keys the one config persist among `effects` names, whether the key is
/// in `AppConfig`'s shape or one config.toml holds outside it.
fn persisted_keys(effects: Vec<Effect>) -> Vec<String> {
    let persists: Vec<Vec<String>> = effects
        .into_iter()
        .filter_map(|effect| match effect {
            Effect::Persist(Persist::AppConfig { keys, .. }) => Some(keys),
            Effect::Persist(Persist::ConfigExternalKeys(edits)) => {
                Some(edits.into_iter().map(|(key, _)| key).collect())
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        persists.len(),
        1,
        "one config persist per edit: {persists:?}"
    );
    persists.into_iter().next().unwrap()
}

/// A row of the kind `pick` names, off the daemon and plugin rows, which
/// persist elsewhere than config.toml.
fn find_row<T>(
    state: &AppState,
    pick: impl Fn(&ainb_app::config::settings_model::ConfigSetting) -> Option<T>,
) -> T {
    let mut keys: Vec<_> = state
        .config
        .config_screen_state
        .settings
        .values()
        .flatten()
        .filter(|row| !row.key.starts_with("hangar_daemon.") && !row.key.starts_with("plugin"))
        .collect();
    keys.sort_by(|a, b| a.key.cmp(&b.key));
    keys.into_iter().find_map(|row| pick(row)).expect("a row of that kind")
}

#[test]
fn a_text_edit_marks_only_its_row_dirty_and_persists_only_its_key() {
    let keymap = Keymap::defaults();
    let mut state = on_config();
    let before = state.versions();

    let effects = dispatch(
        &mut state,
        &keymap,
        &mut NoRenderer,
        pointer::set_config_row(
            "workspace_defaults.branch_prefix",
            ConfigRowEdit::Text("g6a/".to_string()),
        ),
    );

    assert_eq!(
        row_value(&state, "workspace_defaults.branch_prefix").raw(),
        "g6a/"
    );
    assert_eq!(
        state.config.app_config.workspace_defaults.branch_prefix,
        "g6a/"
    );
    assert_eq!(
        persisted_keys(effects),
        vec!["workspace_defaults.branch_prefix".to_string()],
        "the write names the edited key, never the whole config"
    );
    // The edit is saved, so the row is clean again and the section moved.
    assert!(state.config.config_screen_state.dirty.is_empty());
    assert!(bumped(&before, &state.versions()).contains(&SectionId::Config));
}

#[test]
fn a_bool_and_a_number_edit_take_their_row_kinds() {
    let keymap = Keymap::defaults();
    let mut state = on_config();

    let effects = dispatch(
        &mut state,
        &keymap,
        &mut NoRenderer,
        pointer::set_config_row(
            "workspace_defaults.scan_max_depth",
            ConfigRowEdit::Number(4),
        ),
    );
    assert_eq!(
        row_value(&state, "workspace_defaults.scan_max_depth").raw(),
        "4"
    );
    assert_eq!(
        persisted_keys(effects),
        vec!["workspace_defaults.scan_max_depth".to_string()]
    );

    let (key, was) = find_row(&state, |row| match row.value {
        ConfigValue::Bool(b) => Some((row.key.clone(), b)),
        _ => None,
    });
    let effects = dispatch(
        &mut state,
        &keymap,
        &mut NoRenderer,
        pointer::set_config_row(&key, ConfigRowEdit::Bool(!was)),
    );
    assert_eq!(row_value(&state, &key).raw(), (!was).to_string());
    assert_eq!(persisted_keys(effects), vec![key]);
}

#[test]
fn a_choice_edit_names_an_option_index_and_one_past_the_options_changes_nothing() {
    let keymap = Keymap::defaults();
    let mut state = on_config();
    let (key, options, selected) = find_row(&state, |row| match &row.value {
        ConfigValue::Choice(options, selected) if options.len() > 1 => {
            Some((row.key.clone(), options.clone(), *selected))
        }
        _ => None,
    });
    let other = (selected + 1) % options.len();

    let effects = dispatch(
        &mut state,
        &keymap,
        &mut NoRenderer,
        pointer::set_config_row(&key, ConfigRowEdit::Choice(other)),
    );
    assert_eq!(row_value(&state, &key).raw(), options[other]);
    assert_eq!(persisted_keys(effects), vec![key.clone()]);

    let before = state.versions();
    let effects = dispatch(
        &mut state,
        &keymap,
        &mut NoRenderer,
        pointer::set_config_row(&key, ConfigRowEdit::Choice(options.len())),
    );
    assert_eq!(row_value(&state, &key).raw(), options[other]);
    assert!(effects.is_empty());
    assert!(bumped(&before, &state.versions()).is_empty());
}

#[test]
fn a_secret_row_takes_a_reference_and_a_literal_never_reaches_the_row() {
    let keymap = Keymap::defaults();
    let mut state = on_config();
    let key = find_row(&state, |row| {
        matches!(row.value, ConfigValue::Secret(_)).then(|| row.key.clone())
    });

    let effects = dispatch(
        &mut state,
        &keymap,
        &mut NoRenderer,
        pointer::set_config_row(&key, ConfigRowEdit::Secret("$MY_TOKEN".to_string())),
    );
    match row_value(&state, &key) {
        ConfigValue::Secret(secret) => assert_eq!(secret.reference, "$MY_TOKEN"),
        other => panic!("secret row became {other:?}"),
    }
    assert_eq!(persisted_keys(effects), vec![key.clone()]);

    // A plain text edit does not fit a secret row: it is not a reference.
    let before = state.versions();
    let effects = dispatch(
        &mut state,
        &keymap,
        &mut NoRenderer,
        pointer::set_config_row(&key, ConfigRowEdit::Text("sk-literal".to_string())),
    );
    match row_value(&state, &key) {
        ConfigValue::Secret(secret) => assert_eq!(secret.reference, "$MY_TOKEN"),
        other => panic!("secret row became {other:?}"),
    }
    assert!(effects.is_empty());
    assert!(bumped(&before, &state.versions()).is_empty());
}

#[test]
fn an_edit_of_the_wrong_kind_a_read_only_row_or_an_unknown_key_changes_nothing() {
    let keymap = Keymap::defaults();
    let mut state = on_config();
    let read_only = state
        .config
        .config_screen_state
        .settings
        .values()
        .flatten()
        .find_map(|row| row.key.starts_with("usage.").then(|| row.key.clone()))
        .expect("a read-only usage row");
    let before = state.versions();

    let mut effects = Vec::new();
    for intent in [
        pointer::set_config_row(
            "workspace_defaults.branch_prefix",
            ConfigRowEdit::Bool(true),
        ),
        pointer::set_config_row(&read_only, ConfigRowEdit::Text("changed".to_string())),
        pointer::set_config_row("no.such.row", ConfigRowEdit::Text("x".to_string())),
    ] {
        effects.extend(dispatch(&mut state, &keymap, &mut NoRenderer, intent));
    }

    assert!(state.config.config_screen_state.dirty.is_empty());
    assert!(effects.is_empty());
    assert!(bumped(&before, &state.versions()).is_empty());
}

#[test]
fn a_row_edit_runs_only_on_the_config_screen_and_never_from_a_bare_name() {
    let keymap = Keymap::defaults();
    let mut state = on_config();
    state.shell.current_screen = screen_ids::SESSION_LIST.to_string();
    let before = state.versions();

    let effects = dispatch(
        &mut state,
        &keymap,
        &mut NoRenderer,
        pointer::set_config_row(
            "workspace_defaults.branch_prefix",
            ConfigRowEdit::Text("elsewhere/".to_string()),
        ),
    );
    assert_ne!(
        state.config.app_config.workspace_defaults.branch_prefix,
        "elsewhere/"
    );
    assert!(effects.is_empty());
    assert!(bumped(&before, &state.versions()).is_empty());

    // Back on the screen, the row named with no payload (as a palette would
    // send it) changes nothing either.
    state.shell.current_screen = screen_ids::CONFIG.to_string();
    let before = state.versions();
    let effects = dispatch(
        &mut state,
        &keymap,
        &mut NoRenderer,
        ainb_app::Intent::Command(
            ainb_app::CommandId::new(pointer::ids::CONFIG_SET_ROW),
            serde_json::Value::Null,
        ),
    );
    assert!(effects.is_empty());
    assert!(bumped(&before, &state.versions()).is_empty());
}
