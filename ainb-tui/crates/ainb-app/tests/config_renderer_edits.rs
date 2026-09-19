#![allow(missing_docs)]

// ABOUTME: A renderer may set only the settings rows the policy allows
// (#1224). The whole config schema is walked here: every registry row is on
// exactly one of the two exact lists or is a secret, so a new row is refused
// until someone classifies it; every row whose leaf names something the host
// runs, binds or trusts is on the deny list; every secret row is refused
// before the lists. Each refused row is proved refused from a renderer twice:
// by name, as `config.set_row`, and by the key sequence, Enter on the row and
// Enter in its popup. The verdicts are committed as a fixture so the page's
// copy of the policy is diffed against the reducer's.

#[path = "support/home.rs"]
mod home;

use std::collections::BTreeSet;

use ainb_app::app::NoRenderer;
use ainb_app::app::keymap::KeyAction;
use ainb_app::app::pointer;
use ainb_app::app::screens::ids as screen_ids;
use ainb_app::config::registry::{self, RowKind};
use ainb_app::config::renderer_edit::{
    self, ALLOWED, DENIED, DENIED_REASON, NOT_DRAWN_REASON, SECRET_REASON,
};
use ainb_app::config::settings_model::{
    ConfigCategory, ConfigRowEdit, ConfigSetting, ConfigValue, SecretValue,
};
use ainb_app::{AppState, Chord, CommandId, Effect, Keymap, dispatch};

/// A key's last segment that names something the host runs, binds, mounts,
/// installs or trusts. Derived from the schema at test time: any registry
/// row ending in one of these must be denied, whatever list it was put on.
const EXECUTABLE_LEAVES: &[&str] = &[
    "command",
    "entrypoint",
    "args",
    "script",
    "install_command",
    "package",
    "url",
    "branch",
    "host",
    "listen",
    "insecure_bind",
    "path",
    "volumes",
    "mount_ssh",
    "mount_git_config",
    "system_packages",
    "npm_packages",
    "python_packages",
    "ports",
    "preferred_editor",
    "terminal",
    "transport",
    "permission_mode",
    "token",
    "bot_token",
    "app_token",
    "api_key",
    "user_id",
    "default_target",
    "channel_id",
    "file",
    "cache_db",
    "catalog_release",
    "base_image",
    "user",
];

/// The reducer on the Config screen with one synthetic row `key` of `value`
/// on the right pane, selected.
fn on_row(key: &str, value: ConfigValue) -> AppState {
    home::shared();
    let mut state = AppState::new();
    state.shell.current_screen = screen_ids::CONFIG.to_string();
    let screen = &mut state.config.config_screen_state;
    let rows = screen.settings.entry(ConfigCategory::General).or_default();
    rows.push(ConfigSetting {
        key: key.to_string(),
        label: key.to_string(),
        value,
        description: String::new(),
    });
    let index = rows.len() - 1;
    screen.visible_rows = vec![(ConfigCategory::General, index)];
    screen.selected_setting = 0;
    screen.focused_pane = ainb_app::app::state::ConfigPane::Settings;
    state
}

fn text_row(key: &str) -> AppState {
    on_row(key, ConfigValue::Text("before".to_string()))
}

/// Send `config.set_row` for `key` with the current frame's revision.
fn edit(state: &mut AppState, keymap: &Keymap, key: &str, edit: ConfigRowEdit) -> Vec<Effect> {
    let revision = state.config.version();
    dispatch(
        state,
        keymap,
        &mut NoRenderer,
        pointer::set_config_row(key, edit, revision),
    )
}

/// The action `config.set_row` runs with `key` and a text `value`, as the
/// host judges it: the row's action with the payload parsed in.
fn set_row_action(keymap: &Keymap, key: &str, value: &str) -> KeyAction {
    let row = keymap
        .command(&CommandId::new(pointer::ids::CONFIG_SET_ROW))
        .expect("config.set_row is a row");
    row.action
        .with_args(&serde_json::json!({ "key": key, "value": { "Text": value }, "revision": 0 }))
        .expect("the payload parses")
}

/// The action Enter runs now.
fn enter(keymap: &Keymap, state: &AppState) -> KeyAction {
    keymap
        .resolve_with_context(
            &ainb_app::app::keymap::active_contexts(state),
            &Chord::parse("enter").expect("chord"),
        )
        .map(|(_, action)| action)
        .expect("Enter is bound here")
}

/// `pattern` with each `*` made a concrete map key.
fn concrete(pattern: &str) -> String {
    pattern.replace('*', "sample")
}

/// Every registry row is classified: on exactly one list, or a secret, which
/// is refused before the lists. Every list entry names a real row. A row
/// added to the schema fails here until it is put on a list.
#[test]
fn every_registry_row_is_classified_exactly_once() {
    let rows: BTreeSet<&str> = registry::rows().map(|row| row.key).collect();
    let denied: BTreeSet<&str> = DENIED.iter().map(|(key, _)| *key).collect();
    let allowed: BTreeSet<&str> = ALLOWED.iter().copied().collect();
    let secrets: BTreeSet<&str> = registry::rows()
        .filter(|row| matches!(row.kind, RowKind::Secret))
        .map(|row| row.key)
        .collect();

    assert_eq!(denied.len(), DENIED.len(), "a row is denied twice");
    assert_eq!(allowed.len(), ALLOWED.len(), "a row is allowed twice");
    let both: Vec<_> = denied.intersection(&allowed).collect();
    assert!(both.is_empty(), "on both lists: {both:?}");
    let phantom: Vec<_> = denied.union(&allowed).filter(|key| !rows.contains(*key)).collect();
    assert!(
        phantom.is_empty(),
        "listed but not a registry row: {phantom:?}"
    );
    let allowed_secret: Vec<_> = allowed.intersection(&secrets).collect();
    assert!(
        allowed_secret.is_empty(),
        "a secret row is allowed: {allowed_secret:?}"
    );
    let unclassified: Vec<_> = rows
        .iter()
        .filter(|key| !denied.contains(*key) && !allowed.contains(*key) && !secrets.contains(*key))
        .collect();
    assert!(
        unclassified.is_empty(),
        "registry rows on neither list; classify them in renderer_edit.rs: {unclassified:?}"
    );
    for pattern in ALLOWED {
        assert!(
            !pattern.ends_with('.'),
            "a prefix on the allow list: {pattern}"
        );
    }
}

/// Every schema row whose leaf names something executable, bound, mounted,
/// installed or trusted is on the deny list, so a new `foo.command` row is
/// refused even if it is also added to the allow list by mistake.
#[test]
fn every_executable_valued_row_in_the_schema_is_denied() {
    let denied: BTreeSet<&str> = DENIED.iter().map(|(key, _)| *key).collect();
    let mut checked = 0;
    for row in registry::rows() {
        let leaf = row.key.rsplit('.').next().unwrap_or(row.key);
        if !EXECUTABLE_LEAVES.contains(&leaf) {
            continue;
        }
        checked += 1;
        let secret = matches!(row.kind, RowKind::Secret);
        assert!(
            denied.contains(row.key) || secret,
            "{} names something the host runs or trusts and is not denied",
            row.key
        );
        let expected = if secret { SECRET_REASON } else { DENIED_REASON };
        assert_eq!(
            renderer_edit::refusal(&concrete(row.key)),
            Some(expected),
            "{}",
            row.key
        );
    }
    assert!(
        checked >= 40,
        "the schema carries executable-valued rows: {checked}"
    );
}

#[test]
fn every_denied_row_is_refused_from_a_renderer_by_name_and_by_key_sequence() {
    let keymap = Keymap::defaults();
    for (pattern, why) in DENIED {
        // A secret row on the deny list is refused as a secret first; the
        // secret test below covers it.
        if registry::row(pattern).is_some_and(|row| matches!(row.kind, RowKind::Secret)) {
            continue;
        }
        let key = concrete(pattern);
        let mut state = text_row(&key);

        // By name: the command's action is judged before the reducer runs it,
        // and the reducer drops the payload even so, with a notice.
        let action = set_row_action(&keymap, &key, "evil");
        assert_eq!(
            state.remote_command_refusal(&action),
            Some(DENIED_REASON),
            "{key}: {why}"
        );
        let effects = edit(
            &mut state,
            &keymap,
            &key,
            ConfigRowEdit::Text("evil".to_string()),
        );
        assert!(
            effects.is_empty(),
            "{key}: the reducer persisted a denied row"
        );
        assert_eq!(
            state.config.config_screen_state.current_setting().map(|row| row.value.raw()),
            Some("before".to_string())
        );

        // By key sequence: Enter on the row is refused; and were the popup
        // open anyway, Enter in it is refused too.
        assert_eq!(
            state.remote_command_refusal(&enter(&keymap, &state)),
            Some(DENIED_REASON),
            "{key}: Enter opens its popup"
        );
        let _ = dispatch(
            &mut state,
            &keymap,
            &mut NoRenderer,
            ainb_app::Intent::Key(Chord::parse("enter").expect("chord")),
        );
        // An opaque or plugin row opens no popup for the terminal either; the
        // Enter that would have was refused above, which is the row's gate.
        if !state.config.config_popup_state.show_popup {
            continue;
        }
        assert_eq!(
            state.remote_command_refusal(&enter(&keymap, &state)),
            Some(DENIED_REASON),
            "{key}: Enter in the popup writes it"
        );
    }
}

/// A secret row is refused from a renderer by name, by Enter on it, and
/// through the keychain and API key prompts, with the reason that says where
/// a secret is set.
#[test]
fn every_secret_row_and_the_secret_write_paths_are_refused_from_a_renderer() {
    let keymap = Keymap::defaults();
    let secrets: Vec<&str> = registry::rows()
        .filter(|row| matches!(row.kind, RowKind::Secret))
        .map(|row| row.key)
        .collect();
    assert!(!secrets.is_empty());
    for key in secrets {
        let mut state = on_row(
            key,
            ConfigValue::Secret(SecretValue {
                reference: "$WAS".to_string(),
                resolved: false,
            }),
        );
        assert_eq!(
            state.remote_command_refusal(&set_row_action(&keymap, key, "keychain:x")),
            Some(SECRET_REASON),
            "{key}"
        );
        assert_eq!(
            state.remote_command_refusal(&enter(&keymap, &state)),
            Some(SECRET_REASON),
            "{key}: Enter on the row"
        );
        let effects = edit(
            &mut state,
            &keymap,
            key,
            ConfigRowEdit::Text("keychain:x".to_string()),
        );
        assert!(effects.is_empty(), "{key}");
        assert_eq!(
            state.config.config_screen_state.current_setting().map(|row| row.value.raw()),
            Some("$WAS".to_string()),
            "{key}"
        );
        // The keychain prompt on the row, ctrl+k.
        let keychain = keymap
            .resolve_with_context(
                &ainb_app::app::keymap::active_contexts(&state),
                &Chord::parse("ctrl+k").expect("chord"),
            )
            .map(|(_, action)| action)
            .expect("ctrl+k is bound on the config screen");
        assert_eq!(
            state.remote_command_refusal(&keychain),
            Some(SECRET_REASON),
            "{key}"
        );
    }
    // The API key prompt's rows, wherever they are bound.
    let state = text_row("workspace_defaults.branch_prefix");
    for id in ["config.api_key.save"] {
        let row = keymap.command(&CommandId::new(id)).expect(id);
        assert_eq!(
            state.remote_command_refusal(&row.action),
            Some(SECRET_REASON),
            "{id}"
        );
    }
}

#[test]
fn an_allowed_row_is_not_refused_and_an_unclassified_row_is() {
    let keymap = Keymap::defaults();
    let mut state = text_row("workspace_defaults.branch_prefix");
    assert_eq!(state.remote_command_refusal(&enter(&keymap, &state)), None);
    let action = set_row_action(&keymap, "workspace_defaults.branch_prefix", "g6a/");
    assert_eq!(state.remote_command_refusal(&action), None);
    let effects = edit(
        &mut state,
        &keymap,
        "workspace_defaults.branch_prefix",
        ConfigRowEdit::Text("g6a/".to_string()),
    );
    assert!(!effects.is_empty(), "an allowed row persists");

    let mut state = text_row("new.row.nobody.classified");
    assert_eq!(
        state.remote_command_refusal(&enter(&keymap, &state)),
        Some(NOT_DRAWN_REASON)
    );
    let effects = edit(
        &mut state,
        &keymap,
        "new.row.nobody.classified",
        ConfigRowEdit::Text("x".to_string()),
    );
    assert!(effects.is_empty());
}

/// A scrubbed value sent back would write the marker over the real one.
#[test]
fn the_redaction_marker_is_never_written() {
    let keymap = Keymap::defaults();
    let mut state = text_row("workspace_defaults.branch_prefix");
    let effects = edit(
        &mut state,
        &keymap,
        "workspace_defaults.branch_prefix",
        ConfigRowEdit::Text(ainb_app::fleet::bridge::redact::REDACTED.to_string()),
    );
    assert!(effects.is_empty());
    assert_eq!(
        state.config.config_screen_state.current_setting().map(|row| row.value.raw()),
        Some("before".to_string())
    );
}

/// The verdict for every registry row, committed so the settings page's copy
/// of the policy (`ainb-desktop/ui/src/settings.ts`) is diffed against this
/// one. `UPDATE_RENDERER_EDITABLE_ROWS=1` rewrites it.
#[test]
fn the_verdicts_match_the_committed_fixture() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/renderer_editable_rows.txt");
    let verdicts = renderer_edit::verdicts();
    if std::env::var_os("UPDATE_RENDERER_EDITABLE_ROWS").is_some() {
        std::fs::write(&path, &verdicts).expect("write fixture");
        return;
    }
    let committed = std::fs::read_to_string(&path).unwrap_or_else(|error| {
        panic!(
            "no fixture at {}: {error}; write it with UPDATE_RENDERER_EDITABLE_ROWS=1",
            path.display()
        )
    });
    assert_eq!(
        committed, verdicts,
        "renderer_editable_rows.txt is stale: regenerate it and update settings.ts"
    );
    for verdict in ["allow ", "deny ", "secret "] {
        assert!(
            verdicts.lines().any(|line| line.starts_with(verdict)),
            "{verdict}"
        );
    }
}
