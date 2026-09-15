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

    /// `{"screen": String, "host": HostId, "watching": bool, "width": u16,
    /// "height": u16}`; `width` and `height` default to 0, so a stop may leave
    /// them out.
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

/// `host` keeps `screen`'s plugin rendering at `width` by `height`, the
/// viewport it draws the screen at, while the terminal shows something else
/// (`watching`), or stops. Several hosts watching one screen get the largest
/// size any live request asked for, up to `ScreenWatch::MAX_VIEWPORT`.
///
/// Each host holds one request per screen: its stop ends only its own, and a
/// new size replaces its old one at once. A host that goes away without a stop
/// is released by [`crate::app::reports::host_disconnected`], or its request
/// lapses with `AppState::PLUGIN_SCREEN_WATCH_LEASE`.
#[must_use]
pub fn watch_screen(
    screen: &str,
    host: &crate::wire::frame::HostId,
    watching: bool,
    width: u16,
    height: u16,
) -> Intent {
    Intent::Command(
        CommandId::new(ids::WATCH_SCREEN),
        json!({
            "screen": screen,
            "host": host,
            "watching": watching,
            "width": width,
            "height": height,
        }),
    )
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WatchArgs {
    screen: String,
    host: crate::wire::frame::HostId,
    watching: bool,
    #[serde(default)]
    width: u16,
    #[serde(default)]
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
                    host: args.host,
                    watching: args.watching,
                    width: args.width,
                    height: args.height,
                }
            }),
        ),
        _ => None,
    }
}
