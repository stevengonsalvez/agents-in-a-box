// ABOUTME: Which plugin owns which screen, and what that means for key routing.
// The plugin screen renderer and the key/mouse forwarders, which speak the
// terminal's event types, live in `ainb-core::app::screens::builtin`.

use super::ids;
use crate::app::AppState;

/// Static screen → plugin routing table. The `state.rs` render tick
/// already maps the same way; both call sites read this so the
/// authoritative list lives in one place.
///
/// Keep this list in sync with `tick_plugin_renders` in `app/state.rs`.
pub const PLUGIN_SCREENS: &[(&str, &str)] = &[
    (ids::ANALYTICS, "burndown"),
    (ids::WITR, "witr"),
    (ids::LEARNINGS, "learnings"),
    (ids::ABTOP, "abtop"),
    (ids::HANGAR, "hangar-tui"),
];

/// Resolve the plugin id that owns `screen_id`, if any.
#[must_use]
pub fn plugin_id_for_screen(screen_id: &str) -> Option<&'static str> {
    PLUGIN_SCREENS.iter().find_map(|(s, p)| (*s == screen_id).then_some(*p))
}

/// `true` when the plugin owning `current_screen` reported (on its last frame)
/// that its focused surface is capturing free text — a title/filter/compose/
/// search/API-key input where every printable key is typed content.
///
/// Read on the host key-dispatch path so `?`/`H`/`W` reach the plugin's input
/// verbatim instead of toggling help / wiring the statusline (8hx). The flag is
/// refreshed every tick by `AppState::tick_plugin_renders` from the plugin's
/// `RenderResult.captures_text`. Non-plugin screens (and screens whose plugin
/// has never painted) read `false`.
#[must_use]
pub fn focused_plugin_captures_text(state: &AppState) -> bool {
    plugin_id_for_screen(&state.shell.current_screen).is_some()
        && state
            .plugins_host
            .plugin_captures_text
            .get(&state.shell.current_screen)
            .copied()
            .unwrap_or(false)
}

/// Plugins that render their own `?` help overlay. On their screens the host
/// never claims `?`/`H` (or the `W` statusline global), whatever the per-frame
/// `captures_text` flag says. Every other plugin keeps the host help toggle.
pub const PLUGINS_WITH_OWN_HELP: &[&str] = &["hangar-tui"];

/// `true` when the focused screen belongs to a plugin in
/// [`PLUGINS_WITH_OWN_HELP`]. The host's printable-key globals (`?`/`H` help,
/// `W` statusline) are suppressed there; see [`is_host_reserved_key`] for why
/// the per-frame `captures_text` flag alone is not a safe gate.
#[must_use]
pub fn plugin_owns_help_keys(state: &AppState) -> bool {
    plugin_id_for_screen(&state.shell.current_screen)
        .is_some_and(|id| PLUGINS_WITH_OWN_HELP.contains(&id))
}
