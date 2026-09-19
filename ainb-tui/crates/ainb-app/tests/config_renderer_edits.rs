#![allow(missing_docs)]

// ABOUTME: A renderer may not set a settings row whose value the host runs
// (#1224). Every such row is listed here with where its value is spawned,
// and each is proved refused from a renderer twice: by name, as
// `config.set_row`, and by the key sequence, Enter on the row and Enter in
// its popup. The allow list of rows the page draws is committed as a fixture
// so the page's copy of the policy is diffed against the reducer's.

use ainb_app::app::NoRenderer;
use ainb_app::app::keymap::KeyAction;
use ainb_app::app::pointer;
use ainb_app::app::screens::ids as screen_ids;
use ainb_app::config::renderer_edit::{self, DENIED, DENIED_REASON, NOT_DRAWN_REASON};
use ainb_app::config::settings_model::{ConfigCategory, ConfigRowEdit, ConfigSetting, ConfigValue};
use ainb_app::{AppState, Chord, CommandId, Keymap, dispatch};

/// Every config row whose value reaches a program the host runs, with the
/// spawn it reaches. The policy's deny list must name each; a row added here
/// without a deny entry fails, and a deny entry with no row here fails.
const SPAWN_ROWS: &[(&str, &str)] = &[
    (
        "ui_preferences.preferred_editor",
        "ainb-desktop/src/executor.rs open_editor, Effect::OpenEditor",
    ),
    (
        "acp.adapters.*.command",
        "the ACP adapter command the daemon spawns",
    ),
    (
        "container_templates.*.config.command",
        "docker/session_container.rs container command",
    ),
    (
        "container_templates.*.config.entrypoint",
        "docker/session_container.rs container entrypoint",
    ),
    (
        "container_templates.*.config.environment.*",
        "docker/container_manager.rs container env",
    ),
    (
        "container_templates.*.config.image_source.path",
        "docker image build context",
    ),
    (
        "container_templates.*.config.image_source.build_args.*",
        "docker image build args",
    ),
    (
        "container_templates.*.config.volumes",
        "docker/container_manager.rs host mounts",
    ),
    (
        "container_templates.*.config.mount_ssh",
        "config/container.rs mounts ~/.ssh",
    ),
    (
        "container_templates.*.config.mount_git_config",
        "config/container.rs mounts ~/.gitconfig",
    ),
    (
        "container_templates.*.config.system_packages",
        "image build package install",
    ),
    (
        "container_templates.*.config.npm_packages",
        "image build package install",
    ),
    (
        "container_templates.*.config.python_packages",
        "image build package install",
    ),
    (
        "mcp_servers.*.definition.command",
        "config/mcp.rs and the MCP pool spawn",
    ),
    (
        "mcp_servers.*.definition.args",
        "config/mcp.rs MCP server argv",
    ),
    (
        "mcp_servers.*.definition.env.*",
        "config/mcp.rs MCP server env",
    ),
    (
        "mcp_servers.*.definition.config",
        "config/mcp.rs imported server blob",
    ),
    (
        "mcp_servers.*.installation.install_command",
        "config/mcp.rs install shell command",
    ),
    (
        "mcp_servers.*.installation.script",
        "config/mcp.rs install script",
    ),
    (
        "mcp_servers.*.installation.package",
        "config/mcp.rs package manager install",
    ),
    (
        "mcp_servers.*.installation.url",
        "config/mcp.rs fetched installer",
    ),
    (
        "mcp_servers.*.installation.branch",
        "config/mcp.rs fetched installer",
    ),
    (
        "docker.host",
        "docker/container_manager.rs connection target",
    ),
    (
        "fleet.terminal",
        "ainb-core/src/cli/fleet/open_terminal.rs terminal app",
    ),
    (
        "hangar_daemon.card_agent.default",
        "ainb-hangar-daemon rpc card agent spawn",
    ),
    ("plugins.enabled", "plugin binaries the host loads"),
    ("plugins.disabled", "plugin binaries the host loads"),
    ("plugins.*", "a plugin's own configuration"),
    ("presets.file", "config/presets.rs file read"),
    (
        "usage_client.cache_db",
        "usage_cache/store.rs database open",
    ),
    ("web.listen", "config/tunables.rs web bind address"),
    ("web.insecure_bind", "config/tunables.rs web bind scope"),
    ("skills.catalog_release", "skill catalog fetch"),
];

/// The reducer on the Config screen with one synthetic text row `key` on
/// the right pane, selected.
fn on_row(key: &str) -> AppState {
    let home = tempfile::tempdir().expect("scratch home");
    std::env::set_var("HOME", home.path());
    std::mem::forget(home);
    let mut state = AppState::new();
    state.shell.current_screen = screen_ids::CONFIG.to_string();
    let screen = &mut state.config.config_screen_state;
    let rows = screen.settings.entry(ConfigCategory::General).or_default();
    rows.push(ConfigSetting {
        key: key.to_string(),
        label: key.to_string(),
        value: ConfigValue::Text("before".to_string()),
        description: String::new(),
    });
    let index = rows.len() - 1;
    screen.visible_rows = vec![(ConfigCategory::General, index)];
    screen.selected_setting = 0;
    screen.focused_pane = ainb_app::app::state::ConfigPane::Settings;
    state
}

/// The action `config.set_row` runs with `key` and a text `value`, as the
/// host judges it: the row's action with the payload parsed in.
fn set_row_action(keymap: &Keymap, key: &str, value: &str) -> KeyAction {
    let row = keymap
        .command(&CommandId::new(pointer::ids::CONFIG_SET_ROW))
        .expect("config.set_row is a row");
    row.action
        .with_args(&serde_json::json!({ "key": key, "value": { "Text": value } }))
        .expect("the payload parses")
}

/// `pattern` with each `*` made a concrete map key.
fn concrete(pattern: &str) -> String {
    pattern.replace('*', "sample")
}

#[test]
fn the_deny_list_is_exactly_the_rows_whose_value_reaches_a_spawn() {
    let mut listed: Vec<&str> = SPAWN_ROWS.iter().map(|(key, _)| *key).collect();
    let mut denied: Vec<&str> = DENIED.iter().map(|(key, _)| *key).collect();
    listed.sort_unstable();
    denied.sort_unstable();
    assert_eq!(
        listed, denied,
        "SPAWN_ROWS and renderer_edit::DENIED name the same rows"
    );
    for (key, spawn) in SPAWN_ROWS {
        assert_eq!(
            renderer_edit::refusal(&concrete(key)),
            Some(DENIED_REASON),
            "{key} reaches {spawn} and must be refused"
        );
    }
}

#[test]
fn every_spawn_row_is_refused_from_a_renderer_by_name_and_by_key_sequence() {
    let keymap = Keymap::defaults();
    for (pattern, spawn) in SPAWN_ROWS {
        let key = concrete(pattern);
        let mut state = on_row(&key);

        // By name: the command's action is judged before the reducer runs it,
        // and the reducer drops the payload even so.
        let action = set_row_action(&keymap, &key, "evil");
        assert_eq!(
            state.remote_command_refusal(&action),
            Some(DENIED_REASON),
            "{key}: {spawn}"
        );
        let effects = dispatch(
            &mut state,
            &keymap,
            &mut NoRenderer,
            pointer::set_config_row(&key, ConfigRowEdit::Text("evil".to_string())),
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
        let enter = keymap
            .resolve_with_context(
                &ainb_app::app::keymap::active_contexts(&state),
                &Chord::parse("enter").expect("chord"),
            )
            .map(|(_, action)| action)
            .expect("Enter is bound on the config screen");
        assert_eq!(
            state.remote_command_refusal(&enter),
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
        let confirm = keymap
            .resolve_with_context(
                &ainb_app::app::keymap::active_contexts(&state),
                &Chord::parse("enter").expect("chord"),
            )
            .map(|(_, action)| action)
            .expect("Enter is bound in the popup");
        assert_eq!(
            state.remote_command_refusal(&confirm),
            Some(DENIED_REASON),
            "{key}: Enter in the popup writes it"
        );
    }
}

#[test]
fn a_row_the_page_draws_is_not_refused_and_a_row_it_does_not_draw_is() {
    let keymap = Keymap::defaults();
    let mut state = on_row("workspace_defaults.branch_prefix");
    let enter = keymap
        .resolve_with_context(
            &ainb_app::app::keymap::active_contexts(&state),
            &Chord::parse("enter").expect("chord"),
        )
        .map(|(_, action)| action)
        .expect("Enter");
    assert_eq!(state.remote_command_refusal(&enter), None);
    let action = set_row_action(&keymap, "workspace_defaults.branch_prefix", "g6a/");
    assert_eq!(state.remote_command_refusal(&action), None);

    let mut state = on_row("usage.plan.id");
    let enter = keymap
        .resolve_with_context(
            &ainb_app::app::keymap::active_contexts(&state),
            &Chord::parse("enter").expect("chord"),
        )
        .map(|(_, action)| action)
        .expect("Enter");
    assert_eq!(state.remote_command_refusal(&enter), Some(NOT_DRAWN_REASON));
    let effects = dispatch(
        &mut state,
        &keymap,
        &mut NoRenderer,
        pointer::set_config_row("usage.plan.id", ConfigRowEdit::Text("pro".to_string())),
    );
    assert!(effects.is_empty());
}

/// A scrubbed value sent back would write the marker over the real one.
#[test]
fn the_redaction_marker_is_never_written() {
    let keymap = Keymap::defaults();
    let mut state = on_row("workspace_defaults.branch_prefix");
    let effects = dispatch(
        &mut state,
        &keymap,
        &mut NoRenderer,
        pointer::set_config_row(
            "workspace_defaults.branch_prefix",
            ConfigRowEdit::Text(ainb_app::fleet::bridge::redact::REDACTED.to_string()),
        ),
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
    assert!(verdicts.lines().any(|line| line.starts_with("allow ")));
    assert!(verdicts.lines().any(|line| line.starts_with("deny ")));
}
