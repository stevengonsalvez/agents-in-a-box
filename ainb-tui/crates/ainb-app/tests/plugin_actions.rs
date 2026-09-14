// ABOUTME: A renderer runs a plugin's own action through `dispatch` and reads
// what it changed from the plugin's `ui.state` view, kept per plugin in the
// plugins-host section.

use ainb_app::app::NoRenderer;
use ainb_app::app::plugin_action::{self, ids};
use ainb_app::app::state::NotificationType;
use ainb_app::{AppState, CommandId, Intent, Keymap, SectionId, dispatch};
use ainb_plugin_runtime::types::PluginId;

fn bumped(before: &[u64], after: &[u64]) -> Vec<SectionId> {
    SectionId::ALL
        .into_iter()
        .filter(|id| before[id.index()] != after[id.index()])
        .collect()
}

fn isolated_home() {
    static HOME: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
    HOME.get_or_init(|| {
        let home = tempfile::tempdir().expect("scratch home");
        std::env::set_var("HOME", home.path());
        home
    });
}

#[test]
fn the_plugin_action_is_an_unbound_row_that_refuses_to_run_bare() {
    isolated_home();
    let keymap = Keymap::defaults();
    let row = keymap.command(&CommandId::new(ids::PLUGIN_ACTION)).expect("the row resolves");
    assert!(row.chord.is_none());

    let mut state = AppState::new();
    let before = state.versions();
    let effects = dispatch(
        &mut state,
        &keymap,
        &mut NoRenderer,
        Intent::Command(CommandId::new(ids::PLUGIN_ACTION), serde_json::Value::Null),
    );
    assert!(effects.is_empty());
    assert!(bumped(&before, &state.versions()).is_empty());
}

#[test]
fn an_action_for_a_plugin_that_is_not_running_says_so() {
    isolated_home();
    let mut state = AppState::new();
    assert!(state.plugins_host.plugin_runtime.is_none());

    let effects = dispatch(
        &mut state,
        &Keymap::defaults(),
        &mut NoRenderer,
        plugin_action::run(
            "hangar-tui",
            "board.open_card",
            serde_json::json!({ "id": "card-7" }),
        ),
    );

    assert!(
        effects.is_empty(),
        "the action goes to the plugin, not the host"
    );
    let errors: Vec<_> = state
        .shell
        .notifications
        .iter()
        .filter(|n| n.notification_type == NotificationType::Error)
        .map(|n| n.message.clone())
        .collect();
    assert!(
        errors.len() == 1
            && errors[0].contains("board.open_card")
            && errors[0].contains("hangar-tui"),
        "{errors:?}"
    );
}

#[test]
fn an_action_for_a_plugin_no_screen_owns_is_refused() {
    isolated_home();
    let mut state = AppState::new();
    let _ = dispatch(
        &mut state,
        &Keymap::defaults(),
        &mut NoRenderer,
        plugin_action::run("not-a-plugin", "anything", serde_json::Value::Null),
    );
    assert_eq!(state.shell.notifications.len(), 1);
}

fn publish(view: &str, version: u64, publisher: &str) -> Option<(bytes::Bytes, u64, PluginId)> {
    Some((
        bytes::Bytes::from(view.to_string()),
        version,
        PluginId::new(publisher),
    ))
}

#[test]
fn a_newer_ui_state_is_kept_per_plugin_and_bumps_only_the_plugins_host_section() {
    isolated_home();
    let mut state = AppState::new();

    let before = state.versions();
    state.record_plugin_ui_state(publish(r#"{"screen":"kanban"}"#, 3, "hangar-tui"));
    assert_eq!(
        bumped(&before, &state.versions()),
        vec![SectionId::PluginsHost]
    );
    let kept = &state.plugins_host.plugin_ui_states["hangar-tui"];
    assert_eq!(kept.version, 3);
    assert_eq!(kept.view["screen"], "kanban");

    // The same publish read on the next tick changes nothing.
    let before = state.versions();
    state.record_plugin_ui_state(publish(r#"{"screen":"kanban"}"#, 3, "hangar-tui"));
    state.record_plugin_ui_state(None);
    assert!(bumped(&before, &state.versions()).is_empty());

    state.record_plugin_ui_state(publish(r#"{"screen":"issues"}"#, 4, "hangar-tui"));
    assert_eq!(
        state.plugins_host.plugin_ui_states["hangar-tui"].view["screen"],
        "issues"
    );
}

#[test]
fn a_host_publish_or_a_non_json_view_is_not_kept() {
    isolated_home();
    let mut state = AppState::new();
    let before = state.versions();

    state.record_plugin_ui_state(publish(r#"{"screen":"kanban"}"#, 1, "host"));
    state.record_plugin_ui_state(publish("not json", 2, "hangar-tui"));

    assert!(state.plugins_host.plugin_ui_states.is_empty());
    assert!(bumped(&before, &state.versions()).is_empty());
}
