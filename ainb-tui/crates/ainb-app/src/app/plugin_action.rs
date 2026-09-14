// ABOUTME: Running a plugin's own action by id, for a click or palette command
// a renderer resolved against a plugin's `ui.state` view rather than its key
// map. One unbound keymap row, so the command registry lists it with the rest.

use serde::Deserialize;
use serde_json::{Value, json};

use crate::app::events::AppEvent;
use crate::app::intent::{Args, Intent};
use crate::app::keymap::CommandId;

/// Command ids of the plugin action rows.
pub mod ids {
    /// `{"plugin": String, "action_id": String, "payload": Value}`
    pub const PLUGIN_ACTION: &str = "plugin.owned.action";

    /// `{"screen": String, "watching": bool}`
    pub const WATCH_SCREEN: &str = "plugin.owned.watch_screen";

    /// Every plugin action command id.
    pub const ALL: &[&str] = &[PLUGIN_ACTION, WATCH_SCREEN];
}

/// Ask `plugin` to run its action `action_id` with `payload`.
#[must_use]
pub fn run(plugin: &str, action_id: &str, payload: Value) -> Intent {
    Intent::Command(
        CommandId::new(ids::PLUGIN_ACTION),
        json!({ "plugin": plugin, "action_id": action_id, "payload": payload }),
    )
}

/// Keep `screen`'s plugin rendering at `width` by `height`, the viewport the
/// watching host draws it at, while the terminal shows something else
/// (`watching`), or stop. Several hosts watching one screen get the largest
/// size any of them asked for.
#[must_use]
pub fn watch_screen(screen: &str, watching: bool, width: u16, height: u16) -> Intent {
    Intent::Command(
        CommandId::new(ids::WATCH_SCREEN),
        json!({ "screen": screen, "watching": watching, "width": width, "height": height }),
    )
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WatchArgs {
    screen: String,
    watching: bool,
    width: u16,
    height: u16,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ActionArgs {
    plugin: String,
    action_id: String,
    #[serde(default)]
    payload: Value,
}

/// The event a plugin action row runs with `args` as its payload, with the
/// same contract as [`crate::app::pointer::with_args`].
pub(crate) fn with_args(event: &AppEvent, args: &Args) -> Option<Option<AppEvent>> {
    match event {
        AppEvent::PluginAction { .. } => Some(
            serde_json::from_value::<ActionArgs>(args.clone())
                .ok()
                .filter(|args| !args.plugin.is_empty() && !args.action_id.is_empty())
                .map(|args| AppEvent::PluginAction {
                    plugin: args.plugin,
                    action_id: args.action_id,
                    payload: args.payload,
                }),
        ),
        AppEvent::WatchPluginScreen { .. } => Some(
            serde_json::from_value::<WatchArgs>(args.clone()).ok().map(|args| {
                AppEvent::WatchPluginScreen {
                    screen: args.screen,
                    watching: args.watching,
                    width: args.width,
                    height: args.height,
                }
            }),
        ),
        _ => None,
    }
}
