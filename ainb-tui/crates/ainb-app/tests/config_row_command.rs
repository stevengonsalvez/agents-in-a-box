#![allow(missing_docs)]

// ABOUTME: A settings form names the row it edits by key and sends the value
// as one pointer command, `config.set_row`, with the config section version
// it drew. The reducer resolves it against the row's own kind, marks only that
// row dirty, and persists through the same key-level write the terminal's
// popup uses, so a window never saves the whole config from the snapshot it
// loaded at startup (#1175, D3d). An edit it drops says why, as a notice.

#[path = "support/home.rs"]
mod home;

use ainb_app::app::NoRenderer;
use ainb_app::app::effect::Persist;
use ainb_app::app::pointer;
use ainb_app::app::screens::ids as screen_ids;
use ainb_app::config::renderer_edit::{MAX_TEXT_CHARS, SECRET_REASON};
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
    home::shared();
    let mut state = AppState::new();
    state.shell.current_screen = screen_ids::CONFIG.to_string();
    state
}

/// Send `config.set_row` for `key` as a form drawing the current frame would:
/// with the config section version it holds.
fn edit(state: &mut AppState, keymap: &Keymap, key: &str, edit: ConfigRowEdit) -> Vec<Effect> {
    let revision = state.config.version();
    dispatch(
        state,
        keymap,
        &mut NoRenderer,
        pointer::set_config_row(key, edit, revision),
    )
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

/// The last notice the reducer raised, which the window toasts.
fn last_notice(state: &AppState) -> String {
    state
        .shell
        .notifications
        .last()
        .map(|notice| notice.message.clone())
        .unwrap_or_default()
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
/// persist elsewhere than config.toml, and off the rows the renderer policy
/// refuses.
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
        // And off the rows a renderer may not edit (#1224): those are
        // `config_renderer_edits.rs`'s to prove refused.
        .filter(|row| ainb_app::config::renderer_edit::refusal(&row.key).is_none())
        .collect();
    keys.sort_by(|a, b| a.key.cmp(&b.key));
    keys.into_iter().find_map(|row| pick(row)).expect("a row of that kind")
}

#[test]
fn a_text_edit_marks_only_its_row_dirty_and_persists_only_its_key() {
    let keymap = Keymap::defaults();
    let mut state = on_config();
    let before = state.versions();

    let effects = edit(
        &mut state,
        &keymap,
        "workspace_defaults.branch_prefix",
        ConfigRowEdit::Text("g6a/".to_string()),
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

    let effects = edit(
        &mut state,
        &keymap,
        "workspace_defaults.scan_max_depth",
        ConfigRowEdit::Number(4),
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
    let effects = edit(&mut state, &keymap, &key, ConfigRowEdit::Bool(!was));
    assert_eq!(row_value(&state, &key).raw(), (!was).to_string());
    assert_eq!(persisted_keys(effects), vec![key]);
}

#[test]
fn a_choice_edit_names_an_option_index_and_one_past_the_options_is_refused_with_a_notice() {
    let keymap = Keymap::defaults();
    let mut state = on_config();
    let (key, options, selected) = find_row(&state, |row| match &row.value {
        ConfigValue::Choice(options, selected) if options.len() > 1 => {
            Some((row.key.clone(), options.clone(), *selected))
        }
        _ => None,
    });
    let other = (selected + 1) % options.len();

    let effects = edit(&mut state, &keymap, &key, ConfigRowEdit::Choice(other));
    assert_eq!(row_value(&state, &key).raw(), options[other]);
    assert_eq!(persisted_keys(effects), vec![key.clone()]);

    let effects = edit(
        &mut state,
        &keymap,
        &key,
        ConfigRowEdit::Choice(options.len()),
    );
    assert_eq!(row_value(&state, &key).raw(), options[other]);
    assert!(effects.is_empty());
    assert!(
        last_notice(&state).contains("not one of the row's"),
        "{}",
        last_notice(&state)
    );
}

/// The action `config.set_row` runs with `key` and a text `value`, as the
/// host judges it before the reducer sees it: a refusal here is what the
/// window toasts.
fn set_row_action(keymap: &Keymap, key: &str, value: &str) -> ainb_app::app::keymap::KeyAction {
    let row = keymap
        .command(&ainb_app::CommandId::new(pointer::ids::CONFIG_SET_ROW))
        .expect("config.set_row is a row");
    row.action
        .with_args(&serde_json::json!({ "key": key, "value": { "Text": value }, "revision": 0 }))
        .expect("the payload parses")
}

/// A secret row is not renderer-settable in this slice: the host refuses the
/// edit with the reason that says where a secret is set, and the reducer
/// changes nothing even when the payload reaches it.
#[test]
fn a_secret_row_is_refused_from_a_renderer_with_the_reason() {
    let keymap = Keymap::defaults();
    let mut state = on_config();
    let key = state
        .config
        .config_screen_state
        .settings
        .values()
        .flatten()
        .find_map(|row| matches!(row.value, ConfigValue::Secret(_)).then(|| row.key.clone()))
        .expect("a secret row");
    let before = state.versions();
    let was = row_value(&state, &key);

    assert_eq!(
        state.remote_command_refusal(&set_row_action(&keymap, &key, "keychain:tg")),
        Some(SECRET_REASON)
    );
    let effects = edit(
        &mut state,
        &keymap,
        &key,
        ConfigRowEdit::Text("keychain:tg".to_string()),
    );

    assert_eq!(row_value(&state, &key).raw(), was.raw());
    assert!(effects.is_empty());
    assert!(!bumped(&before, &state.versions()).contains(&SectionId::Config));
}

/// A row the policy refuses is refused by the host, whose refusal the window
/// toasts; an edit the policy allows but the row cannot take is dropped by
/// the reducer with a notice saying why.
#[test]
fn a_refused_row_is_the_hosts_to_say_and_a_misfit_edit_is_the_reducers() {
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

    for (key, why) in [
        (read_only.as_str(), "may not set it"),
        ("no.such.row", "does not edit it"),
    ] {
        let refusal = state
            .remote_command_refusal(&set_row_action(&keymap, key, "changed"))
            .unwrap_or_else(|| panic!("{key} is refused"));
        assert!(refusal.contains(why), "{key}: {refusal}");
        let before = state.versions();
        let effects = edit(
            &mut state,
            &keymap,
            key,
            ConfigRowEdit::Text("changed".to_string()),
        );
        assert!(effects.is_empty(), "{key}");
        assert!(
            !bumped(&before, &state.versions()).contains(&SectionId::Config),
            "{key}"
        );
    }

    let before = state.versions();
    let effects = edit(
        &mut state,
        &keymap,
        "workspace_defaults.branch_prefix",
        ConfigRowEdit::Bool(true),
    );
    assert!(effects.is_empty());
    assert!(state.config.config_screen_state.dirty.is_empty());
    assert!(!bumped(&before, &state.versions()).contains(&SectionId::Config));
    assert!(
        last_notice(&state).contains("does not fit"),
        "{}",
        last_notice(&state)
    );
}

/// A text edit is cleaned of control and format characters, and one over the
/// bound is refused rather than cut.
#[test]
fn a_text_edit_is_cleaned_and_bounded() {
    let keymap = Keymap::defaults();
    let mut state = on_config();

    let effects = edit(
        &mut state,
        &keymap,
        "workspace_defaults.branch_prefix",
        ConfigRowEdit::Text("g6a\u{202E}/\u{0007}".to_string()),
    );
    assert_eq!(
        row_value(&state, "workspace_defaults.branch_prefix").raw(),
        "g6a/"
    );
    assert_eq!(persisted_keys(effects).len(), 1);

    let effects = edit(
        &mut state,
        &keymap,
        "workspace_defaults.branch_prefix",
        ConfigRowEdit::Text("x".repeat(MAX_TEXT_CHARS + 1)),
    );
    assert!(effects.is_empty());
    assert_eq!(
        row_value(&state, "workspace_defaults.branch_prefix").raw(),
        "g6a/"
    );
    assert!(
        last_notice(&state).contains("longer than"),
        "{}",
        last_notice(&state)
    );
}

/// A form that drew an older frame edits rows it never saw: the edit is
/// refused, and the refusal is a notice the window toasts.
#[test]
fn a_stale_revision_is_refused_with_a_notice() {
    let keymap = Keymap::defaults();
    let mut state = on_config();
    let before = state.versions();
    let stale = state.config.version().wrapping_sub(1);

    let effects = dispatch(
        &mut state,
        &keymap,
        &mut NoRenderer,
        pointer::set_config_row(
            "workspace_defaults.branch_prefix",
            ConfigRowEdit::Text("late/".to_string()),
            stale,
        ),
    );
    assert_ne!(
        row_value(&state, "workspace_defaults.branch_prefix").raw(),
        "late/"
    );
    assert!(effects.is_empty());
    assert!(!bumped(&before, &state.versions()).contains(&SectionId::Config));
    assert!(
        last_notice(&state).contains("moved since"),
        "{}",
        last_notice(&state)
    );
}

#[test]
fn a_row_edit_runs_only_on_the_config_screen_and_never_from_a_bare_name() {
    let keymap = Keymap::defaults();
    let mut state = on_config();
    state.shell.current_screen = screen_ids::SESSION_LIST.to_string();
    let before = state.versions();

    let effects = edit(
        &mut state,
        &keymap,
        "workspace_defaults.branch_prefix",
        ConfigRowEdit::Text("elsewhere/".to_string()),
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
