// ABOUTME: Which plugin owns which screen, and where input on it goes. Routing
// speaks the plugin runtime's key and mouse types, so every host gets the same
// policy; each host converts its own events first. The plugin screen renderer
// and the terminal event conversions live in `ainb-core::app::screens::builtin`.

use super::ids;
use crate::app::AppState;
use crate::app::keymap::{Chord, Key, KeyAction, KeyContext, Keymap, Mods, SubContext};
use ainb_plugin_protocol::params::{
    KEY_MOD_ALT, KEY_MOD_CTRL, KEY_MOD_SHIFT, KeyCode, KeyEvent, MouseEvent, Viewport,
};

/// Static screen → plugin routing table: the render tick, plugin action
/// delivery and key routing all read this one list.
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

/// `true` if the host reserves this key: it MUST NOT be forwarded to the
/// plugin and MUST fall through to the central dispatch.
///
/// The reservation list is deliberately small:
///
/// - `Ctrl+C` is host quit (ALWAYS reserved, never relaxed).
/// - `?` / `H` toggle host help, reserved only for a plugin that does NOT
///   render its own help ([`PLUGINS_WITH_OWN_HELP`]) and only while it is not
///   capturing text. A plugin that owns its help (hangar) keeps `?`/`H`
///   outright: the capture flag lags a render behind, so gating on it (8hx)
///   still let the first keystrokes into a freshly opened plugin text field
///   reach the host help toggle, which then swallowed every later key until
///   Esc.
///
/// `Esc` is plugin-owned: it pops one level inside the plugin, and at its root
/// the plugin publishes `ui.close_request`, which the host honours
/// (`AppState::tick_panel_close_requests`). When the plugin is missing or its
/// render is wedged, [`route_key_to_focused_plugin`] hands the screen's back
/// keys to the host instead, so the placeholder screen still closes.
///
/// `q`, `a`, `Tab`, `Enter` and the rest stay plugin-owned: the burndown plugin
/// binds them to period switches, panel focus and zoom.
#[must_use]
pub fn is_host_reserved_key(key: &KeyEvent, plugin_owns_help: bool, capturing: bool) -> bool {
    match key.code {
        KeyCode::Char { ch: 'c' } if key.mods & KEY_MOD_CTRL != 0 => true,
        KeyCode::Char { ch: '?' | 'H' } => !plugin_owns_help && !capturing,
        _ => false,
    }
}

/// Where input on a plugin-owned screen goes.
#[derive(Debug, PartialEq)]
pub enum PluginRoute {
    /// The plugin takes it: run this `Effect::ForwardToPlugin`.
    Forward(crate::app::Effect),
    /// The plugin screen takes it, but there is nothing to send (pointer-move
    /// spam, a click outside the plugin, a plugin the runtime does not have).
    /// Host dispatch must not run for it either.
    Consumed,
    /// Not the plugin's: the host's own dispatch runs.
    Host,
}

/// The keymap chord a wire key event stands for: the one key table (#1123).
///
/// A plugin screen's keys resolve through it against the same rows as every
/// other key, and a terminal host reaches the keymap through it too, by
/// converting its own key event to the wire shape first, so a new key is one
/// edit to the wire enum and one arm here.
///
/// `BackTab` becomes `Tab` with [`Mods::SHIFT`], its one canonical form. The
/// wire's super bit and the press, repeat or release kind are not part of a
/// chord.
#[must_use]
pub fn key_chord(key: &KeyEvent) -> Chord {
    let code = match key.code {
        KeyCode::Char { ch } => Key::Char(ch),
        KeyCode::Enter => Key::Enter,
        KeyCode::Tab | KeyCode::BackTab => Key::Tab,
        KeyCode::Esc => Key::Esc,
        KeyCode::Backspace => Key::Backspace,
        KeyCode::Delete => Key::Delete,
        KeyCode::Insert => Key::Insert,
        KeyCode::Up => Key::Up,
        KeyCode::Down => Key::Down,
        KeyCode::Left => Key::Left,
        KeyCode::Right => Key::Right,
        KeyCode::Home => Key::Home,
        KeyCode::End => Key::End,
        KeyCode::PageUp => Key::PageUp,
        KeyCode::PageDown => Key::PageDown,
        KeyCode::F { n } => Key::F(n),
    };
    let mut mods = Mods::NONE;
    if key.mods & KEY_MOD_CTRL != 0 {
        mods = mods | Mods::CTRL;
    }
    if key.mods & KEY_MOD_ALT != 0 {
        mods = mods | Mods::ALT;
    }
    if key.mods & KEY_MOD_SHIFT != 0 || matches!(key.code, KeyCode::BackTab) {
        mods = mods | Mods::SHIFT;
    }
    Chord::new(code, mods)
}

/// Whether `key` is one the `plugin.owned` rows bind to leaving the screen.
fn is_back_key(keymap: &Keymap, key: &KeyEvent) -> bool {
    matches!(
        keymap.resolve(
            &[KeyContext::Screen("plugin", SubContext::Named("owned"))],
            &key_chord(key)
        ),
        Some(KeyAction::App(crate::app::AppEvent::PanelBack))
    )
}

/// Route `key` on the focused screen: to the plugin that owns it, or back to
/// the host.
///
/// Decided from state and the keymap alone. The host's runtime is never
/// asked: what it knows about the plugin arrives in
/// `plugins_host.plugin_presence`, and a key that then cannot be delivered
/// comes back as a report.
#[must_use]
pub fn route_key_to_focused_plugin(
    state: &AppState,
    keymap: &Keymap,
    key: &KeyEvent,
) -> PluginRoute {
    let Some(plugin_name) = plugin_id_for_screen(&state.shell.current_screen) else {
        return PluginRoute::Host;
    };
    let capturing = focused_plugin_captures_text(state);
    if is_host_reserved_key(key, plugin_owns_help_keys(state), capturing) {
        // Host claims this key: the central dispatch resolves it to Quit or
        // ToggleHelp.
        return PluginRoute::Host;
    }
    // A screen the tick has not reported on yet (the runtime is not up, or
    // this frame came first) is treated as absent, never as the host's: the
    // host's rows on the screen beneath include destructive ones.
    let presence = state
        .plugins_host
        .plugin_presence
        .get(&state.shell.current_screen)
        .copied()
        .unwrap_or_default();
    // The back keys leave the screen. When the plugin is absent or its render
    // is wedged the key would not be acted on, so the central dispatch takes
    // it (PanelBack) rather than trapping the user with only Ctrl+C.
    let back = is_back_key(keymap, key);
    // Esc is a floor on a dead screen: a rowset rebound away from it must not
    // trap the operator on a plugin that will never pop.
    let dead = !presence.registered || presence.wedged;
    // A key newer than the plugin's ABI (Insert, for an ABI 2 plugin) would
    // not decode on its side, so it never reaches the plugin.
    // `RuntimeHandle::send_key` refuses the same keys, so a second sender
    // cannot slip one past this (#1171). A blocked back key or Esc gets the
    // same floor as a dead screen: a plugin declaring an ABI older than every
    // key's cannot trap the operator.
    let blocked = key.code.min_abi() > presence.abi;
    if (dead || blocked) && (back || matches!(key.code, KeyCode::Esc)) {
        return PluginRoute::Host;
    }
    // Every other key stays claimed on an absent plugin's screen, so the
    // session list's destructive bindings (`d` delete, `n` new session) cannot
    // fire from it.
    if !presence.registered {
        return PluginRoute::Consumed;
    }
    // Any other blocked key the screen still claims, as it claims every key
    // the wire has no shape for.
    if blocked {
        return PluginRoute::Consumed;
    }
    PluginRoute::Forward(crate::app::Effect::ForwardToPlugin {
        plugin: plugin_name.to_string(),
        screen: state.shell.current_screen.clone(),
        input: crate::app::PluginInput::Key(key.clone()),
        back,
    })
}

/// Route a pointer `event` on the focused screen: to the plugin that owns it,
/// or back to the host.
///
/// `origin` is where the host painted the plugin screen and `area` its size,
/// both in the host's own cells; `event` is in the same space. A plugin-owned
/// screen consumes every pointer event, even one it drops (a `Moved`, a click
/// outside the plugin's rect), so the host's own pointer handling never
/// double-acts on it. Coordinates are translated into the plugin's viewport, so
/// the plugin hit-tests against the same `(0, 0)`-based grid it painted.
#[must_use]
pub fn route_mouse_to_focused_plugin(
    state: &AppState,
    origin: (u16, u16),
    area: Viewport,
    event: &MouseEvent,
) -> PluginRoute {
    let Some(plugin_name) = plugin_id_for_screen(&state.shell.current_screen) else {
        return PluginRoute::Host;
    };
    // Absent or not yet reported: the plugin screen still owns the pointer,
    // so the host's click handling never runs on it.
    let registered = state
        .plugins_host
        .plugin_presence
        .get(&state.shell.current_screen)
        .is_some_and(|presence| presence.registered);
    if !registered {
        return PluginRoute::Consumed;
    }
    // Drop pointer-move spam: forwarding every `Moved` would flood the
    // priority mouse channel for an event with no hover semantics in v1.
    if matches!(event.kind, ainb_plugin_runtime::MouseKind::Moved) {
        return PluginRoute::Consumed;
    }
    let Some((col, row)) =
        click_to_viewport(event.col, event.row, origin, (area.width, area.height))
    else {
        return PluginRoute::Consumed;
    };
    let mut mouse = *event;
    mouse.col = col;
    mouse.row = row;
    PluginRoute::Forward(crate::app::Effect::ForwardToPlugin {
        plugin: plugin_name.to_string(),
        screen: state.shell.current_screen.clone(),
        input: crate::app::PluginInput::Mouse(mouse),
        back: false,
    })
}

/// Translate an absolute `(col, row)` into a plugin-viewport coordinate, given
/// the plugin screen's painted `origin` (top-left `(x, y)`) and `area` size
/// `(width, height)`.
///
/// Returns `None`, so the click is dropped, when the point is left of or above
/// the origin (would underflow), at or beyond the right or bottom edge (a
/// `width`-wide rect owns cols `0..width`), or the area is zero-sized (the
/// screen has never painted, so origin and area both default to `(0, 0)`).
#[must_use]
fn click_to_viewport(
    col: u16,
    row: u16,
    (origin_x, origin_y): (u16, u16),
    (width, height): (u16, u16),
) -> Option<(u16, u16)> {
    if col < origin_x || row < origin_y {
        return None;
    }
    let vcol = col - origin_x;
    let vrow = row - origin_y;
    if width == 0 || height == 0 || vcol >= width || vrow >= height {
        return None;
    }
    Some((vcol, vrow))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugin_id_for_screen_resolves_analytics() {
        assert_eq!(plugin_id_for_screen(ids::ANALYTICS), Some("burndown"));
        assert_eq!(plugin_id_for_screen(ids::WITR), Some("witr"));
        assert_eq!(plugin_id_for_screen(ids::LEARNINGS), Some("learnings"));
        assert_eq!(plugin_id_for_screen(ids::ABTOP), Some("abtop"));
        assert_eq!(plugin_id_for_screen(ids::HANGAR), Some("hangar-tui"));
        // Non-plugin screens return None so the forwarder bails early.
        assert_eq!(plugin_id_for_screen(ids::HOME), None);
        assert_eq!(plugin_id_for_screen("nonsense"), None);
    }

    fn key(code: KeyCode, mods: u8) -> KeyEvent {
        KeyEvent {
            code,
            mods,
            kind: ainb_plugin_runtime::KeyKind::Press,
        }
    }

    fn ch(ch: char) -> KeyEvent {
        key(KeyCode::Char { ch }, 0)
    }

    fn on_plugin_screen(screen: &str, registered: bool, wedged: bool) -> AppState {
        let mut state = AppState::default();
        state.shell.current_screen = screen.to_string();
        state.plugins_host.plugin_presence.insert(
            screen.to_string(),
            crate::app::sections::PluginPresence {
                registered,
                wedged,
                abi: if registered {
                    ainb_plugin_protocol::manifest::ABI_VERSION
                } else {
                    0
                },
            },
        );
        state
    }

    #[test]
    fn click_to_viewport_translates_in_bounds_click() {
        // Plugin painted at origin (3, 1), size 20x10. A click at absolute
        // (10, 4) maps to viewport (7, 3).
        assert_eq!(click_to_viewport(10, 4, (3, 1), (20, 10)), Some((7, 3)));
        assert_eq!(click_to_viewport(3, 1, (3, 1), (20, 10)), Some((0, 0)));
        assert_eq!(click_to_viewport(22, 10, (3, 1), (20, 10)), Some((19, 9)));
    }

    #[test]
    fn click_to_viewport_drops_clicks_outside_the_plugin() {
        assert_eq!(click_to_viewport(2, 4, (3, 1), (20, 10)), None);
        assert_eq!(click_to_viewport(10, 0, (3, 1), (20, 10)), None);
        assert_eq!(click_to_viewport(23, 4, (3, 1), (20, 10)), None);
        assert_eq!(click_to_viewport(10, 11, (3, 1), (20, 10)), None);
        assert_eq!(click_to_viewport(0, 0, (0, 0), (0, 0)), None);
    }

    #[test]
    fn host_reserves_ctrl_c_and_help_keys() {
        assert!(is_host_reserved_key(
            &key(KeyCode::Char { ch: 'c' }, KEY_MOD_CTRL),
            false,
            false
        ));
        assert!(is_host_reserved_key(&ch('?'), false, false));
        assert!(is_host_reserved_key(&ch('H'), false, false));
        assert!(!is_host_reserved_key(&ch('H'), false, true));
        // A plugin with its own help owns `?`/`H` outright.
        assert!(!is_host_reserved_key(&ch('?'), true, false));
        assert!(!is_host_reserved_key(&ch('H'), true, false));
        // Esc and the burndown's own keys belong to the plugin.
        for code in [
            KeyCode::Esc,
            KeyCode::Char { ch: 'q' },
            KeyCode::Char { ch: 'a' },
            KeyCode::Char { ch: '1' },
            KeyCode::Enter,
            KeyCode::Tab,
            KeyCode::BackTab,
        ] {
            assert!(
                !is_host_reserved_key(&key(code.clone(), 0), false, false),
                "{code:?}"
            );
        }
        assert!(
            is_host_reserved_key(&key(KeyCode::Char { ch: 'c' }, KEY_MOD_CTRL), true, true),
            "Ctrl+C stays reserved even while the plugin captures text"
        );
    }

    /// With the plugin absent or wedged, the back keys go to the central
    /// dispatch (PanelBack) so the placeholder stays escapable, and every
    /// other key stays claimed so session-list bindings cannot fire from a
    /// dead plugin screen (PR #249 review HIGH-1).
    #[test]
    fn back_keys_leave_an_absent_or_wedged_plugin_and_other_keys_stay_claimed() {
        let keymap = Keymap::defaults();
        for (registered, wedged) in [(false, false), (true, true)] {
            let state = on_plugin_screen(ids::ANALYTICS, registered, wedged);
            for back in [key(KeyCode::Esc, 0), ch('q')] {
                assert_eq!(
                    route_key_to_focused_plugin(&state, &keymap, &back),
                    PluginRoute::Host,
                    "{back:?} leaves (registered {registered}, wedged {wedged})"
                );
            }
        }
        let absent = on_plugin_screen(ids::ANALYTICS, false, false);
        assert_eq!(
            route_key_to_focused_plugin(&absent, &keymap, &ch('d')),
            PluginRoute::Consumed,
            "no destructive fallthrough from an absent plugin"
        );

        // Nothing reported for the screen yet: absent, so `d` is claimed and
        // Esc still leaves.
        let mut unreported = AppState::default();
        unreported.shell.current_screen = ids::ANALYTICS.to_string();
        assert_eq!(
            route_key_to_focused_plugin(&unreported, &keymap, &ch('d')),
            PluginRoute::Consumed
        );
        assert_eq!(
            route_key_to_focused_plugin(&unreported, &keymap, &key(KeyCode::Esc, 0)),
            PluginRoute::Host
        );
    }

    /// The back keys are the `plugin.owned` rows, not a second list: rebinding
    /// the row moves which key leaves a dead plugin screen.
    #[test]
    fn the_back_keys_follow_the_plugin_owned_rows() {
        let overrides =
            crate::app::keymap_toml::KeymapOverrides::parse("[\"plugin.owned\"]\nback_q = \"x\"\n")
                .expect("valid override");
        let keymap = Keymap::defaults().with_overrides(&overrides).expect("rebinds");
        let wedged = on_plugin_screen(ids::ANALYTICS, true, true);

        assert_eq!(
            route_key_to_focused_plugin(&wedged, &keymap, &ch('x')),
            PluginRoute::Host
        );
        assert!(
            matches!(
                route_key_to_focused_plugin(&wedged, &keymap, &ch('q')),
                PluginRoute::Forward(_)
            ),
            "q is a plain key once the row no longer binds it"
        );
    }

    /// With the back row rebound away from Esc, Esc still leaves a wedged or
    /// absent screen through the host, while on a live plugin it is a plain key.
    #[test]
    fn esc_still_leaves_a_dead_plugin_screen_when_the_back_row_is_rebound() {
        let overrides =
            crate::app::keymap_toml::KeymapOverrides::parse("[\"plugin.owned\"]\nback = \"x\"\n")
                .expect("valid override");
        let keymap = Keymap::defaults().with_overrides(&overrides).expect("rebinds");
        let esc = key(KeyCode::Esc, 0);

        for (registered, wedged) in [(true, true), (false, false)] {
            let mut state = on_plugin_screen(ids::ANALYTICS, registered, wedged);
            assert_eq!(
                route_key_to_focused_plugin(&state, &keymap, &esc),
                PluginRoute::Host,
                "registered {registered}, wedged {wedged}"
            );
            let event = crate::app::events::EventHandler::handle_key_event_with_keymap(
                Chord::from(Key::Esc),
                &mut state,
                &keymap,
                &mut crate::app::NoRenderer,
            );
            assert!(
                matches!(
                    event,
                    Some(crate::app::AppEvent::PanelBack | crate::app::AppEvent::GoToHomeScreen)
                ),
                "the host's own rows take Esc off the screen: {event:?}"
            );
        }

        let live = on_plugin_screen(ids::ANALYTICS, true, false);
        assert!(matches!(
            route_key_to_focused_plugin(&live, &keymap, &esc),
            PluginRoute::Forward(crate::app::Effect::ForwardToPlugin { back: false, .. })
        ));
    }

    /// A live plugin gets the key as an effect for the host, with `back` set
    /// on the keys that leave the screen.
    #[test]
    fn a_key_for_a_live_plugin_is_an_effect_for_the_host() {
        let keymap = Keymap::defaults();
        let state = on_plugin_screen(ids::HANGAR, true, false);
        let esc = key(KeyCode::Esc, 0);
        assert_eq!(
            route_key_to_focused_plugin(&state, &keymap, &esc),
            PluginRoute::Forward(crate::app::Effect::ForwardToPlugin {
                plugin: "hangar-tui".to_string(),
                screen: ids::HANGAR.to_string(),
                input: crate::app::PluginInput::Key(esc),
                back: true,
            })
        );
        let PluginRoute::Forward(crate::app::Effect::ForwardToPlugin { back, .. }) =
            route_key_to_focused_plugin(&state, &keymap, &ch('j'))
        else {
            panic!("a plain key on a live plugin is forwarded");
        };
        assert!(!back);
    }

    /// #1171: a plugin whose ABI predates every key cannot trap the operator.
    /// Esc and the back keys go to the host, as on a dead screen; every other
    /// key is claimed and never sent.
    #[test]
    fn a_plugin_below_every_key_abi_leaves_esc_to_the_host() {
        let keymap = Keymap::defaults();
        let mut old = on_plugin_screen(ids::HANGAR, true, false);
        old.plugins_host.plugin_presence.get_mut(ids::HANGAR).unwrap().abi = 1;
        assert_eq!(
            route_key_to_focused_plugin(&old, &keymap, &key(KeyCode::Esc, 0)),
            PluginRoute::Host,
            "Esc leaves the screen"
        );
        assert_eq!(
            route_key_to_focused_plugin(&old, &keymap, &ch('j')),
            PluginRoute::Consumed,
            "any other key is claimed, not sent"
        );
    }

    /// #1171: a plugin screen claims Insert without sending it while its
    /// plugin's ABI predates Insert, forwards it once the ABI carries it, and
    /// off a plugin screen Insert is the host's.
    #[test]
    fn insert_reaches_a_plugin_only_at_the_abi_that_carries_it() {
        let keymap = Keymap::defaults();
        let insert = key(KeyCode::Insert, 0);
        let mut live = on_plugin_screen(ids::HANGAR, true, false);
        assert_eq!(
            route_key_to_focused_plugin(&live, &keymap, &insert),
            PluginRoute::Consumed,
            "an ABI 2 plugin is not sent Insert"
        );
        live.plugins_host.plugin_presence.get_mut(ids::HANGAR).unwrap().abi =
            KeyCode::Insert.min_abi();
        assert!(matches!(
            route_key_to_focused_plugin(&live, &keymap, &insert),
            PluginRoute::Forward(crate::app::Effect::ForwardToPlugin { back: false, .. })
        ));
        let home = on_plugin_screen(ids::HOME, true, false);
        assert_eq!(
            route_key_to_focused_plugin(&home, &keymap, &insert),
            PluginRoute::Host
        );
    }

    /// #1123: the one key table spells every wire key the way the keymap does.
    #[test]
    fn key_chord_spells_every_wire_key() {
        let spelled = |code: KeyCode, mods: u8| key_chord(&key(code, mods)).as_str().to_string();
        assert_eq!(spelled(KeyCode::Insert, 0), "insert");
        assert_eq!(spelled(KeyCode::Delete, 0), "delete");
        assert_eq!(
            spelled(KeyCode::BackTab, 0),
            "shift+tab",
            "BackTab is shift+tab"
        );
        assert_eq!(spelled(KeyCode::BackTab, KEY_MOD_SHIFT), "shift+tab");
        assert_eq!(spelled(KeyCode::Char { ch: 'k' }, KEY_MOD_CTRL), "ctrl+k");
        assert_eq!(spelled(KeyCode::Char { ch: 'x' }, KEY_MOD_ALT), "alt+x");
        assert_eq!(spelled(KeyCode::Char { ch: ' ' }, 0), "space");
        assert_eq!(spelled(KeyCode::Char { ch: '+' }, 0), "plus");
        assert_eq!(
            spelled(
                KeyCode::Char { ch: 'a' },
                ainb_plugin_protocol::params::KEY_MOD_SUPER
            ),
            "a",
            "the super bit is not part of a chord"
        );
        assert_eq!(key_chord(&key(KeyCode::F { n: 5 }, 0)).code(), Key::F(5));
    }

    /// On a plugin that renders its own help the router forwards `?`/`H`
    /// whatever the lagging `captures_text` stash says; Ctrl+C stays the
    /// host's.
    #[test]
    fn plugin_screen_forwards_help_keys_regardless_of_capture_flag() {
        let keymap = Keymap::defaults();
        let mut state = on_plugin_screen(ids::HANGAR, true, false);
        let forwarded = |state: &AppState, c: char| {
            matches!(
                route_key_to_focused_plugin(state, &keymap, &ch(c)),
                PluginRoute::Forward(_)
            )
        };
        assert!(!focused_plugin_captures_text(&state));
        assert!(plugin_owns_help_keys(&state));
        assert!(forwarded(&state, 'H') && forwarded(&state, '?'));

        state.plugins_host.plugin_captures_text.insert(ids::HANGAR.to_string(), true);
        assert!(focused_plugin_captures_text(&state));
        assert!(forwarded(&state, 'H') && forwarded(&state, '?'));
        assert_eq!(
            route_key_to_focused_plugin(
                &state,
                &keymap,
                &key(KeyCode::Char { ch: 'c' }, KEY_MOD_CTRL)
            ),
            PluginRoute::Host
        );
    }

    /// A click inside a live plugin reaches it in viewport cells; a move, a
    /// click outside, and any pointer on an absent plugin are consumed.
    #[test]
    fn a_click_reaches_a_live_plugin_in_its_own_cells() {
        use ainb_plugin_runtime::{MouseButton, MouseKind};
        let down = |col, row| MouseEvent {
            kind: MouseKind::Down {
                button: MouseButton::Left,
            },
            col,
            row,
            mods: 0,
        };
        let state = on_plugin_screen(ids::HANGAR, true, false);
        assert_eq!(
            route_mouse_to_focused_plugin(&state, (3, 1), Viewport::new(20, 10), &down(10, 4)),
            PluginRoute::Forward(crate::app::Effect::ForwardToPlugin {
                plugin: "hangar-tui".to_string(),
                screen: ids::HANGAR.to_string(),
                input: crate::app::PluginInput::Mouse(down(7, 3)),
                back: false,
            })
        );
        assert_eq!(
            route_mouse_to_focused_plugin(&state, (3, 1), Viewport::new(20, 10), &down(1, 1)),
            PluginRoute::Consumed
        );
        let moved = MouseEvent {
            kind: MouseKind::Moved,
            ..down(10, 4)
        };
        assert_eq!(
            route_mouse_to_focused_plugin(&state, (3, 1), Viewport::new(20, 10), &moved),
            PluginRoute::Consumed
        );
        let absent = on_plugin_screen(ids::HANGAR, false, false);
        assert_eq!(
            route_mouse_to_focused_plugin(&absent, (3, 1), Viewport::new(20, 10), &down(10, 4)),
            PluginRoute::Consumed
        );
        let mut home = AppState::default();
        home.shell.current_screen = ids::HOME.to_string();
        assert_eq!(
            route_mouse_to_focused_plugin(&home, (3, 1), Viewport::new(20, 10), &down(10, 4)),
            PluginRoute::Host
        );
    }
}
