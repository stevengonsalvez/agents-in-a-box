// ABOUTME: Built-in Screen impls — thin shims around existing component renderers; no logic moves in Phase 2a

// Screen-to-plugin routing lives in `ainb_app::app::screens::builtin`.
pub use ainb_app::app::screens::builtin::*;

use crate::app::ui_state::UiState;
use ratatui::{Frame, layout::Rect};

use super::{Screen, ids};
use crate::app::AppState;
use crate::components::{
    AttachedTerminalComponent, AuthProviderPopupComponent, AuthSetupComponent, ChangelogComponent,
    ConfigPopupComponent, ConfigScreenComponent, GitViewComponent, HomeScreenV2Component,
    LogHistoryViewerComponent, OnboardingComponent, SessionRecovery, SetupMenuComponent,
};

/// Centred sub-rect helper, mirroring `components::layout::centered_rect`.
/// Duplicated here so screen impls don't reach into `components::layout`'s
/// private helpers.
fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    use ratatui::layout::{Constraint, Direction, Layout};
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}

// ---------------------------------------------------------------------------
// Stateless screens (delegate to free functions or static methods)
// ---------------------------------------------------------------------------

/// Plugin-owned screen wrapper. Reads the WireBuffer that
/// `App::tick_plugin_renders` drained into
/// `state.plugins_host.pending_plugin_renders[screen_id]` and paints it cell-by-cell
/// onto the host's ratatui Frame.
///
/// Falls back to a single-line "loading" message if the plugin hasn't
/// painted yet (e.g. the very first frame after startup, before
/// `tick_plugin_renders` has run).
pub struct PluginScreen {
    screen_id: &'static str,
}

impl PluginScreen {
    #[must_use]
    pub fn new(screen_id: &'static str) -> Self {
        Self { screen_id }
    }
}

/// Convert a `crossterm::event::KeyEvent` into the portable wire
/// shape consumed by `plugin/handle_key`. Returns `None` for keys we
/// don't model on the wire (e.g. media keys, mouse events surfaced as
/// `KeyEvent::Modifier`-only no-ops on some terminals) so callers can
/// silently drop them rather than fabricate a wire shape.
#[must_use]
pub fn crossterm_to_protocol_key(
    key: &crossterm::event::KeyEvent,
) -> Option<ainb_plugin_runtime::KeyEvent> {
    use ainb_plugin_runtime::{
        KEY_MOD_ALT, KEY_MOD_CTRL, KEY_MOD_SHIFT, KEY_MOD_SUPER, KeyCode as ProtocolKey,
        KeyEvent as ProtocolEvent, KeyKind as ProtocolKind,
    };
    use crossterm::event::{KeyCode as CtKey, KeyEventKind as CtKind, KeyModifiers as CtMods};

    let code = match key.code {
        CtKey::Char(c) => ProtocolKey::Char { ch: c },
        CtKey::Enter => ProtocolKey::Enter,
        CtKey::Tab => ProtocolKey::Tab,
        CtKey::BackTab => ProtocolKey::BackTab,
        CtKey::Esc => ProtocolKey::Esc,
        CtKey::Backspace => ProtocolKey::Backspace,
        CtKey::Delete => ProtocolKey::Delete,
        CtKey::Up => ProtocolKey::Up,
        CtKey::Down => ProtocolKey::Down,
        CtKey::Left => ProtocolKey::Left,
        CtKey::Right => ProtocolKey::Right,
        CtKey::Home => ProtocolKey::Home,
        CtKey::End => ProtocolKey::End,
        CtKey::PageUp => ProtocolKey::PageUp,
        CtKey::PageDown => ProtocolKey::PageDown,
        CtKey::F(n) => ProtocolKey::F { n },
        _ => return None,
    };

    let mut mods: u8 = 0;
    if key.modifiers.contains(CtMods::SHIFT) {
        mods |= KEY_MOD_SHIFT;
    }
    if key.modifiers.contains(CtMods::CONTROL) {
        mods |= KEY_MOD_CTRL;
    }
    if key.modifiers.contains(CtMods::ALT) {
        mods |= KEY_MOD_ALT;
    }
    if key.modifiers.contains(CtMods::SUPER) {
        mods |= KEY_MOD_SUPER;
    }

    let kind = match key.kind {
        CtKind::Press => ProtocolKind::Press,
        CtKind::Repeat => ProtocolKind::Repeat,
        CtKind::Release => ProtocolKind::Release,
    };

    Some(ProtocolEvent { code, mods, kind })
}

/// `true` if the host reserves this key — it MUST NOT be forwarded to
/// the plugin and MUST fall through to the central dispatch.
///
/// The reservation list is deliberately small:
///
/// - `Ctrl+C` → host quit (ALWAYS reserved — never relaxed).
/// - `?` / `H` → host help toggle, reserved only for a plugin that does NOT
///   render its own help ([`PLUGINS_WITH_OWN_HELP`]) and only while it is not
///   capturing text. A plugin that owns its help (hangar) keeps `?`/`H`
///   outright: the capture flag lags a render behind, so gating on it (8hx)
///   still let the first keystrokes into a freshly opened plugin text field
///   reach the host help toggle, which then swallowed every later key until
///   Esc.
///
/// `Esc` is plugin-owned (it used to be host-reserved): it pops one
/// internal level (detail drawer, search overlay, zoom, filter chip)
/// and, once the plugin is already at its root view, the plugin
/// publishes `ui.close_request` on the snapshot bus to ask the host to
/// close the screen. The host's event loop polls that topic by version
/// (`AppState::tick_panel_close_requests`) and pops back to the screen
/// the panel was opened from. This replaces the old behaviour where
/// Esc on a zoomed plugin view discarded zoom state and jumped
/// straight home. `Backspace` remains a plugin-internal alias for the
/// same one-level pop. When the plugin is missing or its render is
/// wedged, [`route_key_to_focused_plugin`] returns `Host` for Esc and q,
/// so the central dispatch still closes the placeholder screen.
///
/// `q`, `a`, `Tab`, `Enter`, etc. remain plugin-owned — the burndown
/// plugin re-binds them to period switches, panel focus, and zoom
/// toggles. Letting the host swallow them would make the screen
/// uninteractive.
#[must_use]
pub fn is_host_reserved_key(
    key: &crossterm::event::KeyEvent,
    plugin_owns_help: bool,
    capturing: bool,
) -> bool {
    use crossterm::event::{KeyCode as CtKey, KeyModifiers as CtMods};
    match key.code {
        CtKey::Char('c') if key.modifiers.contains(CtMods::CONTROL) => true,
        CtKey::Char('?' | 'H') => !plugin_owns_help && !capturing,
        _ => false,
    }
}

/// Where input on a plugin-owned screen goes.
#[derive(Debug, PartialEq)]
pub enum PluginRoute {
    /// The plugin takes it: run this `Effect::ForwardToPlugin`.
    Forward(crate::app::Effect),
    /// The plugin screen takes it, but there is nothing to send (an unmodelled
    /// key, pointer-move spam, a click outside the plugin, a plugin the runtime
    /// does not have). Host dispatch must not run for it either.
    Consumed,
    /// Not the plugin's: the host's own dispatch runs.
    Host,
}

/// Route `key` on the focused screen: to the plugin that owns it, or back to
/// the host.
///
/// Decided from state alone. The host's runtime is never asked: what it knows
/// about the plugin arrives in `plugins_host.plugin_presence`, and a key that
/// then cannot be delivered comes back as a report.
#[must_use]
pub fn route_key_to_focused_plugin(
    state: &AppState,
    key: &crossterm::event::KeyEvent,
) -> PluginRoute {
    let Some(plugin_name) = plugin_id_for_screen(&state.shell.current_screen) else {
        return PluginRoute::Host;
    };
    let capturing = focused_plugin_captures_text(state);
    if is_host_reserved_key(key, plugin_owns_help_keys(state), capturing) {
        // Host claims this key: the central dispatch in `events.rs` resolves
        // it to Quit / ToggleHelp / etc.
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
    // Esc and q leave the screen. When the plugin is absent or its render is
    // wedged the key would not be acted on, so the central dispatch takes it
    // (PanelBack) rather than trapping the user with only Ctrl+C.
    let back = matches!(
        key.code,
        crossterm::event::KeyCode::Esc | crossterm::event::KeyCode::Char('q')
    );
    if back && (!presence.registered || presence.wedged) {
        return PluginRoute::Host;
    }
    // Every other key stays claimed on an absent plugin's screen, so the
    // session list's destructive bindings (`d` delete, `n` new session, …)
    // cannot fire from it.
    if !presence.registered {
        return PluginRoute::Consumed;
    }
    let Some(protocol_key) = crossterm_to_protocol_key(key) else {
        // Unmodelled key (e.g. media keys): dropped rather than forging a wire
        // shape.
        return PluginRoute::Consumed;
    };
    PluginRoute::Forward(crate::app::Effect::ForwardToPlugin {
        plugin: plugin_name.to_string(),
        screen: state.shell.current_screen.clone(),
        input: crate::app::PluginInput::Key(protocol_key),
        back,
    })
}

/// Convert a `crossterm::event::MouseEvent` into the portable wire shape
/// consumed by `plugin/handle_mouse`. Coordinates are left as the
/// absolute terminal column/row the caller received — translation into
/// the plugin's viewport space happens in
/// [`route_mouse_to_focused_plugin`], which knows the screen origin.
#[must_use]
pub fn crossterm_to_protocol_mouse(
    event: &crossterm::event::MouseEvent,
) -> ainb_plugin_runtime::MouseEvent {
    use ainb_plugin_runtime::{
        KEY_MOD_ALT, KEY_MOD_CTRL, KEY_MOD_SHIFT, KEY_MOD_SUPER, MouseButton as PButton,
        MouseEvent as PEvent, MouseKind as PKind,
    };
    use crossterm::event::{
        KeyModifiers as CtMods, MouseButton as CtButton, MouseEventKind as CtKind,
    };

    fn button(b: CtButton) -> PButton {
        match b {
            CtButton::Left => PButton::Left,
            CtButton::Right => PButton::Right,
            CtButton::Middle => PButton::Middle,
        }
    }

    let kind = match event.kind {
        CtKind::Down(b) => PKind::Down { button: button(b) },
        CtKind::Up(b) => PKind::Up { button: button(b) },
        CtKind::Drag(b) => PKind::Drag { button: button(b) },
        CtKind::Moved => PKind::Moved,
        CtKind::ScrollDown => PKind::ScrollDown,
        CtKind::ScrollUp => PKind::ScrollUp,
        CtKind::ScrollLeft => PKind::ScrollLeft,
        CtKind::ScrollRight => PKind::ScrollRight,
    };

    let mut mods: u8 = 0;
    if event.modifiers.contains(CtMods::SHIFT) {
        mods |= KEY_MOD_SHIFT;
    }
    if event.modifiers.contains(CtMods::CONTROL) {
        mods |= KEY_MOD_CTRL;
    }
    if event.modifiers.contains(CtMods::ALT) {
        mods |= KEY_MOD_ALT;
    }
    if event.modifiers.contains(CtMods::SUPER) {
        mods |= KEY_MOD_SUPER;
    }

    PEvent {
        kind,
        col: event.column,
        row: event.row,
        mods,
    }
}

/// Route a pointer `event` on the focused screen: to the plugin that owns it,
/// or back to the host.
///
/// Mirrors [`route_key_to_focused_plugin`]. A plugin-owned screen consumes
/// every pointer event, even one it drops (a high-frequency `Moved`, a click
/// outside the plugin's rect), so the host's own mouse handling never
/// double-acts on it. Coordinates are translated from absolute terminal space
/// into the plugin's viewport (origin subtracted), so the plugin hit-tests
/// against the same `(0, 0)`-based grid it painted.
#[must_use]
pub fn route_mouse_to_focused_plugin(
    state: &AppState,
    ui: &UiState,
    event: &crossterm::event::MouseEvent,
) -> PluginRoute {
    use ainb_plugin_runtime::MouseKind;

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

    let mut mouse = crossterm_to_protocol_mouse(event);

    // Drop pointer-move spam: forwarding every `Moved` would flood the
    // priority mouse channel for an event the map doesn't need (no hover
    // semantics in v1).
    if matches!(mouse.kind, MouseKind::Moved) {
        return PluginRoute::Consumed;
    }

    let origin = ui
        .plugin_render_origins
        .get(&state.shell.current_screen)
        .copied()
        .unwrap_or((0, 0));
    let area = ui
        .plugin_viewports
        .render_areas
        .get(&state.shell.current_screen)
        .copied()
        .unwrap_or((0, 0));
    let Some((col, row)) = click_to_viewport(mouse.col, mouse.row, origin, area) else {
        return PluginRoute::Consumed;
    };
    mouse.col = col;
    mouse.row = row;

    PluginRoute::Forward(crate::app::Effect::ForwardToPlugin {
        plugin: plugin_name.to_string(),
        screen: state.shell.current_screen.clone(),
        input: crate::app::PluginInput::Mouse(mouse),
        back: false,
    })
}

/// Translate an absolute terminal `(col, row)` into a plugin-viewport
/// coordinate, given the plugin screen's painted `origin` (top-left
/// `(x, y)`) and `area` size `(width, height)`.
///
/// Returns `None` — i.e. the click is outside the plugin and should be
/// dropped — when the point is left/above the origin (would underflow),
/// at/beyond the right/bottom edge (a `width`-wide rect owns cols
/// `0..width`), or the area is zero-sized (the screen has never painted,
/// so both origin and area default to `(0, 0)`). Kept as a pure free
/// function so the underflow guard + bounds math are unit-testable
/// without a live `AppState`/runtime.
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

/// Build the placeholder paragraph shown when a plugin screen renders
/// but no frame has arrived yet. Three cases:
///
/// 1. **Plugin not registered** — runtime came up but this plugin is
///    absent. Either disabled (`AINB_DISABLE_PLUGINS=1`,
///    `AINB_DISABLE_PLUGIN=<id>`, `AINB_ONLY_PLUGINS` omits it,
///    `config.toml [plugins]` excludes it) or never installed at all.
///    Show actionable text so the user knows it's a deliberate state,
///    not a hang.
/// 2. **Plugin registered, render failed** — the runtime tried and could
///    not produce a frame (most often: the subprocess cannot be spawned
///    because its binary was removed under a running TUI by an upgrade).
///    Show the error and how to recover. Without this the screen claimed
///    to be connecting forever while every keystroke was dropped.
/// 3. **Plugin registered, no frame yet** — runtime spawned the
///    subprocess but the first `plugin/render` hasn't completed. This
///    is the genuine "rendering…" case; lasts milliseconds in normal
///    operation.
fn build_placeholder_for_unloaded_plugin(
    screen_id: &str,
    state: &AppState,
    area: Rect,
) -> ratatui::widgets::Paragraph<'static> {
    use ratatui::{
        layout::Alignment,
        style::{Color, Modifier, Style},
        text::{Line, Span},
        widgets::{Block, BorderType, Borders, Paragraph},
    };

    let plugin_name = plugin_id_for_screen(screen_id);
    let plugin_registered = plugin_name.is_some()
        && state
            .plugins_host
            .plugin_presence
            .get(screen_id)
            .is_some_and(|presence| presence.registered);

    // A recorded render failure outranks the loading beat: the plugin is
    // registered, so case 3 would otherwise paint "connecting…" forever.
    let render_error = if plugin_registered {
        state.plugins_host.plugin_render_errors.get(screen_id)
    } else {
        None
    };

    if let Some(err) = render_error {
        const GOLD: Color = Color::Rgb(255, 215, 0);
        const CORNFLOWER_BLUE: Color = Color::Rgb(100, 149, 237);
        const SOFT_WHITE: Color = Color::Rgb(220, 220, 230);
        const MUTED_GRAY: Color = Color::Rgb(120, 120, 140);
        const ERROR_RED: Color = Color::Rgb(230, 110, 110);
        const DARK_BG: Color = Color::Rgb(25, 25, 35);

        let name = title_case_screen(screen_id);
        let plugin_label = plugin_name.unwrap_or(screen_id).to_string();
        let lines = vec![
            Line::from(""),
            Line::from(Span::styled(
                format!("The `{plugin_label}` plugin could not render this screen."),
                Style::default().fg(SOFT_WHITE).add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from(Span::styled(err.clone(), Style::default().fg(ERROR_RED))),
            Line::from(""),
            Line::from(Span::styled(
                "If ainb was upgraded while this session was open, the plugin binary",
                Style::default().fg(MUTED_GRAY),
            )),
            Line::from(Span::styled(
                "it was discovered from no longer exists. Quit and relaunch ainb.",
                Style::default().fg(MUTED_GRAY),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "Logs: ~/.agents-in-a-box/logs/agents-in-a-box-*.jsonl - search `plugin spawn failed`.",
                Style::default().fg(MUTED_GRAY),
            )),
        ];
        let block = Block::default()
            .title(Line::from(vec![
                Span::styled(" ⬡ ", Style::default().fg(GOLD)),
                Span::styled(
                    format!("{name} unavailable "),
                    Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
                ),
            ]))
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(CORNFLOWER_BLUE))
            .style(Style::default().bg(DARK_BG));
        return Paragraph::new(lines).alignment(Alignment::Center).block(block);
    }

    if plugin_registered {
        // Genuine transient render lag: the runtime has the plugin (freshly
        // spawned OR idle-reaped back to `Idle`) but its first `plugin/render`
        // frame hasn't landed. Paint a full-area, palette-styled "connecting"
        // panel — backdrop fill + rounded border + centred title — so entering
        // the screen reads as a deliberate loading beat, NEVER a blank void with
        // the outer layout dropped.
        const GOLD: Color = Color::Rgb(255, 215, 0);
        const CORNFLOWER_BLUE: Color = Color::Rgb(100, 149, 237);
        const PROGRESS_CYAN: Color = Color::Rgb(100, 200, 230);
        const MUTED_GRAY: Color = Color::Rgb(120, 120, 140);
        const DARK_BG: Color = Color::Rgb(25, 25, 35);

        let name = title_case_screen(screen_id);
        // Vertically centre the two-line message within the framed body.
        let pad = area.height.saturating_sub(4) / 2;
        let mut lines: Vec<Line> = (0..pad).map(|_| Line::from("")).collect();
        lines.push(Line::from(vec![
            Span::styled("⬡  ", Style::default().fg(GOLD)),
            Span::styled(
                format!("{name} — connecting…"),
                Style::default().fg(PROGRESS_CYAN).add_modifier(Modifier::BOLD),
            ),
        ]));
        // Generic wording: this placeholder serves EVERY plugin screen
        // (hangar, burndown, witr, …), not just the hangar workspace.
        lines.push(Line::from(Span::styled(
            "waiting for the plugin's first frame",
            Style::default().fg(MUTED_GRAY).add_modifier(Modifier::ITALIC),
        )));
        let block = Block::default()
            .title(Line::from(vec![
                Span::styled(" ⬡ ", Style::default().fg(GOLD)),
                Span::styled(
                    format!("{name} "),
                    Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
                ),
            ]))
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(CORNFLOWER_BLUE))
            .style(Style::default().bg(DARK_BG));
        return Paragraph::new(lines).alignment(Alignment::Center).block(block);
    }

    // Plugin not registered — explain why and how to fix.
    let title = plugin_name
        .map(|n| format!(" [ {} unavailable ] ", n))
        .unwrap_or_else(|| " [ plugin unavailable ] ".to_string());

    let lines = vec![
        Line::from(""),
        Line::from(vec![Span::styled(
            format!(
                "  This screen is owned by the `{}` plugin, which isn't loaded.",
                plugin_name.unwrap_or(screen_id)
            ),
            Style::default().add_modifier(Modifier::BOLD),
        )]),
        Line::from(""),
        Line::from("  Check whether plugins are disabled in this session:"),
        Line::from(""),
        Line::from(vec![
            Span::raw("    • "),
            Span::styled("AINB_DISABLE_PLUGINS=1", Style::default().fg(Color::Cyan)),
            Span::raw("     — all plugins off (kill switch)"),
        ]),
        Line::from(vec![
            Span::raw("    • "),
            Span::styled(
                format!("AINB_DISABLE_PLUGIN={}", plugin_name.unwrap_or("<id>")),
                Style::default().fg(Color::Cyan),
            ),
            Span::raw("   — this plugin denylisted by env"),
        ]),
        Line::from(vec![
            Span::raw("    • "),
            Span::styled("AINB_ONLY_PLUGINS=…", Style::default().fg(Color::Cyan)),
            Span::raw("        — env allowlist excludes it"),
        ]),
        Line::from(vec![
            Span::raw("    • "),
            Span::styled("config.toml [plugins]", Style::default().fg(Color::Cyan)),
            Span::raw("     — persistent allow/disable list"),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::raw("  To restore it: unset the env var(s) and/or edit "),
            Span::styled(
                "~/.agents-in-a-box/config/config.toml",
                Style::default().fg(Color::Cyan),
            ),
        ]),
        Line::from(vec![
            Span::raw("  Logs: "),
            Span::styled(
                "~/.agents-in-a-box/logs/agents-in-a-box-*.jsonl",
                Style::default().fg(Color::Cyan),
            ),
            Span::raw(" — search `applying plugin filter`."),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::raw("  See "),
            Span::styled("docs/plugins.md", Style::default().fg(Color::Cyan)),
            Span::raw(" → Configuration → Enable/disable plugins."),
        ]),
    ];

    let block = Block::default()
        .title(Span::styled(
            title,
            Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Yellow));

    Paragraph::new(lines).block(block)
}

/// Title-case a plugin screen id for display (`"hangar"` → `"Hangar"`).
/// ASCII-first-letter uppercase is enough for the current screen ids; a
/// multi-word id keeps its remaining characters verbatim.
fn title_case_screen(screen_id: &str) -> String {
    let mut chars = screen_id.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

impl Screen for PluginScreen {
    fn id(&self) -> &str {
        self.screen_id
    }
    fn render(&mut self, frame: &mut Frame, area: Rect, state: &AppState, ui: &mut UiState) {
        // Stash the allocated size so the next tick of
        // `App::tick_plugin_renders` can ask the plugin for a buffer that
        // actually fills this area. Without this the plugin renders into
        // its fallback (80×24) and everything outside that rect stays blank.
        ui.plugin_viewports
            .render_areas
            .insert(self.screen_id.to_string(), (area.width, area.height));
        // Stash the origin too, so the mouse forwarder can translate an
        // absolute terminal click into this plugin's viewport space.
        ui.plugin_render_origins.insert(self.screen_id.to_string(), (area.x, area.y));

        let Some(wire) = state.plugins_host.pending_plugin_renders.get(self.screen_id) else {
            let placeholder = build_placeholder_for_unloaded_plugin(self.screen_id, state, area);
            frame.render_widget(placeholder, area);
            return;
        };
        let buf = frame.buffer_mut();
        // ABI 2.0 cells are sparse `Vec<(Coord, Cell)>` — iterate the
        // painted set rather than indexing a dense grid. Anything
        // outside the area is silently clipped (matches the v1 paint
        // contract).
        for (coord, cell) in &wire.cells {
            if coord.x >= area.width || coord.y >= area.height {
                continue;
            }
            if let Some(target) = buf.cell_mut((area.x + coord.x, area.y + coord.y)) {
                target.set_symbol(&cell.symbol);
                target.set_fg(rgb_to_color(cell.fg));
                target.set_bg(rgb_to_color(cell.bg));
                target.set_style(
                    ratatui::style::Style::default()
                        .add_modifier(modifier_bits_to_modifiers(cell.modifier)),
                );
            }
        }
    }
}

fn rgb_to_color(c: Option<ainb_plugin_protocol::wire_buffer::Color>) -> ratatui::style::Color {
    use ratatui::style::Color;
    match c {
        None => Color::Reset,
        Some(rgb) => Color::Rgb(rgb.r, rgb.g, rgb.b),
    }
}

fn modifier_bits_to_modifiers(b: u16) -> ratatui::style::Modifier {
    use ratatui::style::Modifier;
    let mut m = Modifier::empty();
    if b & 1 != 0 {
        m |= Modifier::BOLD;
    }
    if b & 2 != 0 {
        m |= Modifier::DIM;
    }
    if b & 4 != 0 {
        m |= Modifier::ITALIC;
    }
    if b & 8 != 0 {
        m |= Modifier::UNDERLINED;
    }
    if b & 16 != 0 {
        m |= Modifier::REVERSED;
    }
    m
}

#[derive(Default)]
pub struct SkillsScreen;
impl Screen for SkillsScreen {
    fn id(&self) -> &str {
        ids::SKILLS
    }
    fn render(&mut self, frame: &mut Frame, area: Rect, state: &AppState, _ui: &mut UiState) {
        crate::components::skills::render(frame, area, &state.skills.skills_state);
    }
}

#[derive(Default)]
pub struct SkillManagerScreen;
impl Screen for SkillManagerScreen {
    fn id(&self) -> &str {
        ids::SKILL_MANAGER
    }
    fn render(&mut self, frame: &mut Frame, area: Rect, state: &AppState, ui: &mut UiState) {
        use crate::components::skill_manager_screen::DEFAULT_SOURCES_WIDTH;
        let saved = state.config.app_config.ui_preferences.skill_manager_sources_fraction;
        crate::components::skill_manager_screen::render_with_sources(
            frame,
            area,
            &state.skills.skill_manager_state,
            ui.skill_sources.preferred_width(saved, area.width, DEFAULT_SOURCES_WIDTH),
            ui.skill_sources.resize_active,
        );
    }
}

#[derive(Default)]
pub struct ChangelogScreen;
impl Screen for ChangelogScreen {
    fn id(&self) -> &str {
        ids::CHANGELOG
    }
    fn render(&mut self, frame: &mut Frame, area: Rect, _state: &AppState, ui: &mut UiState) {
        ChangelogComponent::render(frame, area, ui.changelog_scroll);
    }
}

#[derive(Default)]
pub struct GitViewScreen;
impl Screen for GitViewScreen {
    fn id(&self) -> &str {
        ids::GIT_VIEW
    }
    fn render(&mut self, frame: &mut Frame, area: Rect, state: &AppState, ui: &mut UiState) {
        if let Some(ref git_state) = state.git_view.git_view_state {
            GitViewComponent::render(frame, area, git_state, &mut ui.review_sidebar);
        }
    }
}

#[derive(Default)]
pub struct SessionRecoveryScreen;
impl Screen for SessionRecoveryScreen {
    fn id(&self) -> &str {
        ids::SESSION_RECOVERY
    }
    fn render(&mut self, frame: &mut Frame, area: Rect, state: &AppState, ui: &mut UiState) {
        SessionRecovery::render(frame, area, &state.recovery.session_recovery_state, ui);
    }
}

/// Daemons screen — runtime health and repair controls for every long-running ainb daemon
/// (phone bridge / notifyd / ATC / fleet daemon). Renders from
/// `fleet::daemons::collect` via the shared component; the collector is started
/// and polled by `LayoutComponent::tick_before_draw`, so painting is a pure read
/// of the last published snapshot. State lives on `AppState::daemons_state` so
/// that snapshot survives cross-screen navigation.
#[derive(Default)]
pub struct DaemonsScreen;

impl Screen for DaemonsScreen {
    fn id(&self) -> &str {
        ids::DAEMONS
    }
    fn render(&mut self, frame: &mut Frame, area: Rect, state: &AppState, _ui: &mut UiState) {
        crate::components::daemons::render(frame, area, &state.hangar.daemons_state);
    }
}

// ---------------------------------------------------------------------------
// Stateful screens — own their component instance
// ---------------------------------------------------------------------------

pub struct HomeScreen {
    component: HomeScreenV2Component,
}

impl HomeScreen {
    #[must_use]
    pub fn new() -> Self {
        Self {
            component: HomeScreenV2Component::new(),
        }
    }
}

impl Default for HomeScreen {
    fn default() -> Self {
        Self::new()
    }
}

impl Screen for HomeScreen {
    fn id(&self) -> &str {
        ids::HOME
    }
    fn render(&mut self, frame: &mut Frame, area: Rect, state: &AppState, ui: &mut UiState) {
        self.component.render_with_loading(
            frame,
            area,
            &state.shell.home_screen_v2_state,
            &state.sessions.workspaces,
            state.workspace_load.is_loading_workspaces,
            state.config.app_config.ui_preferences.home_sidebar_fraction,
            ui,
        );
    }
}

pub struct ConfigScreen {
    component: ConfigScreenComponent,
    auth_provider_popup: AuthProviderPopupComponent,
    config_popup: ConfigPopupComponent,
}

impl ConfigScreen {
    #[must_use]
    pub fn new() -> Self {
        Self {
            component: ConfigScreenComponent::new(),
            auth_provider_popup: AuthProviderPopupComponent::new(),
            config_popup: ConfigPopupComponent::new(),
        }
    }
}

impl Default for ConfigScreen {
    fn default() -> Self {
        Self::new()
    }
}

impl Screen for ConfigScreen {
    fn id(&self) -> &str {
        ids::CONFIG
    }
    fn render(&mut self, frame: &mut Frame, area: Rect, state: &AppState, _ui: &mut UiState) {
        self.component.render(frame, area, state);
        if state.onboarding.auth_provider_popup_state.show_popup {
            self.auth_provider_popup.render(frame, area, state);
        }
        if state.config.config_popup_state.show_popup {
            self.config_popup.render(frame, area, &state.config.config_popup_state);
        }
    }
}

pub struct LogHistoryScreen {
    component: LogHistoryViewerComponent,
}

impl LogHistoryScreen {
    #[must_use]
    pub fn new() -> Self {
        Self {
            component: LogHistoryViewerComponent::new(),
        }
    }
}

impl Default for LogHistoryScreen {
    fn default() -> Self {
        Self::new()
    }
}

impl Screen for LogHistoryScreen {
    fn id(&self) -> &str {
        ids::LOG_HISTORY
    }
    fn render(&mut self, frame: &mut Frame, area: Rect, state: &AppState, ui: &mut UiState) {
        self.component.render(frame, area, &state.log_streams.log_history_state, ui);
    }
}

pub struct OnboardingScreen {
    component: OnboardingComponent,
}

impl OnboardingScreen {
    #[must_use]
    pub fn new() -> Self {
        Self {
            component: OnboardingComponent,
        }
    }
}

impl Default for OnboardingScreen {
    fn default() -> Self {
        Self::new()
    }
}

impl Screen for OnboardingScreen {
    fn id(&self) -> &str {
        ids::ONBOARDING
    }
    fn render(&mut self, frame: &mut Frame, area: Rect, state: &AppState, _ui: &mut UiState) {
        if let Some(ref onboarding_state) = state.onboarding.onboarding_state {
            self.component.render(frame, area, onboarding_state);
        }
    }
}

/// SetupMenu uses HomeScreenV2 as a backdrop. Owns a fresh component instance
/// so it doesn't fight HomeScreen for the same component (each is stateless
/// across renders; per-frame state lives in `AppState`).
pub struct SetupMenuScreen {
    backdrop: HomeScreenV2Component,
    setup_menu: SetupMenuComponent,
}

impl SetupMenuScreen {
    #[must_use]
    pub fn new() -> Self {
        Self {
            backdrop: HomeScreenV2Component::new(),
            setup_menu: SetupMenuComponent::new(),
        }
    }
}

impl Default for SetupMenuScreen {
    fn default() -> Self {
        Self::new()
    }
}

impl Screen for SetupMenuScreen {
    fn id(&self) -> &str {
        ids::SETUP_MENU
    }
    fn render(&mut self, frame: &mut Frame, area: Rect, state: &AppState, ui: &mut UiState) {
        self.backdrop.render_with_loading(
            frame,
            area,
            &state.shell.home_screen_v2_state,
            &state.sessions.workspaces,
            state.workspace_load.is_loading_workspaces,
            state.config.app_config.ui_preferences.home_sidebar_fraction,
            ui,
        );
        self.setup_menu.render(frame, area, &state.onboarding.setup_menu_state);
    }
}

pub struct AuthSetupScreen {
    component: AuthSetupComponent,
}

impl AuthSetupScreen {
    #[must_use]
    pub fn new() -> Self {
        Self {
            component: AuthSetupComponent::new(),
        }
    }
}

impl Default for AuthSetupScreen {
    fn default() -> Self {
        Self::new()
    }
}

impl Screen for AuthSetupScreen {
    fn id(&self) -> &str {
        ids::AUTH_SETUP
    }
    fn render(&mut self, frame: &mut Frame, area: Rect, state: &AppState, _ui: &mut UiState) {
        // Auth setup historically renders into a centred 60x60 sub-rect of the
        // full frame; preserve that.
        let centered = centered_rect(60, 60, area);
        self.component.render(frame, centered, state);
    }
}

pub struct AttachedTerminalScreen {
    component: AttachedTerminalComponent,
}

impl AttachedTerminalScreen {
    #[must_use]
    pub fn new() -> Self {
        Self {
            component: AttachedTerminalComponent::new(),
        }
    }
}

impl Default for AttachedTerminalScreen {
    fn default() -> Self {
        Self::new()
    }
}

impl Screen for AttachedTerminalScreen {
    fn id(&self) -> &str {
        ids::ATTACHED_TERMINAL
    }
    fn render(&mut self, frame: &mut Frame, area: Rect, state: &AppState, _ui: &mut UiState) {
        self.component.render(frame, area, state);
    }
}

// ---------------------------------------------------------------------------
// Bulk registration of built-in screens.
// ---------------------------------------------------------------------------

use super::super::registry::ScreenRegistry;

/// Populate a registry with every built-in screen rendered as a "full-screen"
/// view. The split-pane fallback (SessionList, Logs, NewSession, ClaudeChat,
/// SearchWorkspace, NonGitNotification) is not part of the registry and stays
/// in `LayoutComponent::render`.
pub fn register_builtins(registry: &mut ScreenRegistry) {
    registry.register(Box::new(HomeScreen::new()));
    registry.register(Box::new(ConfigScreen::new()));
    registry.register(Box::new(LogHistoryScreen::new()));
    registry.register(Box::new(ChangelogScreen::default()));
    registry.register(Box::new(PluginScreen::new(ids::ANALYTICS)));
    registry.register(Box::new(PluginScreen::new(ids::WITR)));
    registry.register(Box::new(PluginScreen::new(ids::LEARNINGS)));
    registry.register(Box::new(PluginScreen::new(ids::ABTOP)));
    registry.register(Box::new(PluginScreen::new(ids::HANGAR)));
    registry.register(Box::new(SkillsScreen::default()));
    registry.register(Box::new(SkillManagerScreen::default()));
    registry.register(Box::new(GitViewScreen::default()));
    registry.register(Box::new(SessionRecoveryScreen::default()));
    registry.register(Box::new(OnboardingScreen::new()));
    registry.register(Box::new(SetupMenuScreen::new()));
    registry.register(Box::new(AuthSetupScreen::new()));
    registry.register(Box::new(AttachedTerminalScreen::new()));
    registry.register(Box::new(DaemonsScreen));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn click_to_viewport_translates_in_bounds_click() {
        // Plugin painted at origin (3, 1), size 20x10. A click at absolute
        // (10, 4) maps to viewport (7, 3).
        assert_eq!(click_to_viewport(10, 4, (3, 1), (20, 10)), Some((7, 3)));
        // Top-left corner of the plugin rect maps to (0, 0).
        assert_eq!(click_to_viewport(3, 1, (3, 1), (20, 10)), Some((0, 0)));
        // Bottom-right-most owned cell (origin + size - 1).
        assert_eq!(click_to_viewport(22, 10, (3, 1), (20, 10)), Some((19, 9)));
    }

    #[test]
    fn click_to_viewport_drops_clicks_left_or_above_origin() {
        // Left of origin (would underflow the u16 subtraction).
        assert_eq!(click_to_viewport(2, 4, (3, 1), (20, 10)), None);
        // Above origin.
        assert_eq!(click_to_viewport(10, 0, (3, 1), (20, 10)), None);
    }

    #[test]
    fn click_to_viewport_drops_clicks_at_or_beyond_far_edge() {
        // Column at the exclusive right edge (origin_x + width).
        assert_eq!(click_to_viewport(23, 4, (3, 1), (20, 10)), None);
        // Row at the exclusive bottom edge (origin_y + height).
        assert_eq!(click_to_viewport(10, 11, (3, 1), (20, 10)), None);
    }

    #[test]
    fn click_to_viewport_drops_when_area_never_painted() {
        // Zero-sized area (screen never rendered) → no destination.
        assert_eq!(click_to_viewport(0, 0, (0, 0), (0, 0)), None);
        assert_eq!(click_to_viewport(5, 5, (0, 0), (0, 0)), None);
    }

    #[test]
    fn register_builtins_populates_registry() {
        let mut r = ScreenRegistry::new();
        register_builtins(&mut r);
        // Every full-screen view gets a Screen impl.
        for id in [
            ids::HOME,
            ids::CONFIG,
            ids::LOG_HISTORY,
            ids::CHANGELOG,
            ids::ANALYTICS,
            ids::WITR,
            ids::ABTOP,
            ids::HANGAR,
            ids::SKILLS,
            ids::SKILL_MANAGER,
            ids::GIT_VIEW,
            ids::SESSION_RECOVERY,
            ids::ONBOARDING,
            ids::SETUP_MENU,
            ids::AUTH_SETUP,
            ids::ATTACHED_TERMINAL,
            ids::DAEMONS,
        ] {
            assert!(r.contains(id), "registry missing built-in screen {id}");
        }
        // Split-pane views deliberately stay out of the registry — layout
        // dispatch still falls through for those.
        assert!(!r.contains(ids::SESSION_LIST));
        assert!(!r.contains(ids::NEW_SESSION));
        assert!(!r.contains(ids::CLAUDE_CHAT));
    }

    #[test]
    fn crossterm_to_protocol_translates_char_and_mods() {
        use ainb_plugin_runtime::{
            KEY_MOD_CTRL, KEY_MOD_SHIFT, KeyCode as ProtocolKey, KeyKind as ProtocolKind,
        };
        use crossterm::event::{
            KeyCode as CtKey, KeyEvent as CtEvent, KeyEventKind, KeyEventState, KeyModifiers,
        };

        // Plain '1' → Char { ch: '1' }, no mods, Press.
        let ev = CtEvent {
            code: CtKey::Char('1'),
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::empty(),
        };
        let p = crossterm_to_protocol_key(&ev).expect("char key translates");
        assert_eq!(p.code, ProtocolKey::Char { ch: '1' });
        assert_eq!(p.mods, 0);
        assert_eq!(p.kind, ProtocolKind::Press);

        // Ctrl+Shift+'z' → Char { ch: 'z' } with both bits set.
        let ev = CtEvent {
            code: CtKey::Char('z'),
            modifiers: KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            kind: KeyEventKind::Press,
            state: KeyEventState::empty(),
        };
        let p = crossterm_to_protocol_key(&ev).expect("modified char translates");
        assert_eq!(p.code, ProtocolKey::Char { ch: 'z' });
        assert_eq!(p.mods, KEY_MOD_CTRL | KEY_MOD_SHIFT);
    }

    #[test]
    fn crossterm_to_protocol_translates_named_keys() {
        use ainb_plugin_runtime::KeyCode as ProtocolKey;
        use crossterm::event::{
            KeyCode as CtKey, KeyEvent as CtEvent, KeyEventKind, KeyEventState, KeyModifiers,
        };

        let cases = [
            (CtKey::Enter, ProtocolKey::Enter),
            (CtKey::Tab, ProtocolKey::Tab),
            (CtKey::BackTab, ProtocolKey::BackTab),
            (CtKey::Esc, ProtocolKey::Esc),
            (CtKey::Backspace, ProtocolKey::Backspace),
            (CtKey::Delete, ProtocolKey::Delete),
            (CtKey::Up, ProtocolKey::Up),
            (CtKey::Down, ProtocolKey::Down),
            (CtKey::Left, ProtocolKey::Left),
            (CtKey::Right, ProtocolKey::Right),
            (CtKey::Home, ProtocolKey::Home),
            (CtKey::End, ProtocolKey::End),
            (CtKey::PageUp, ProtocolKey::PageUp),
            (CtKey::PageDown, ProtocolKey::PageDown),
            (CtKey::F(7), ProtocolKey::F { n: 7 }),
        ];

        for (ct, expected) in cases {
            let ev = CtEvent {
                code: ct,
                modifiers: KeyModifiers::NONE,
                kind: KeyEventKind::Press,
                state: KeyEventState::empty(),
            };
            let p = crossterm_to_protocol_key(&ev)
                .unwrap_or_else(|| panic!("translation missing for {ct:?}"));
            assert_eq!(p.code, expected, "wrong protocol code for {ct:?}");
        }
    }

    #[test]
    fn host_reserves_ctrl_c_and_help_keys() {
        use crossterm::event::{
            KeyCode as CtKey, KeyEvent as CtEvent, KeyEventKind, KeyEventState, KeyModifiers,
        };

        let mk = |code, mods| CtEvent {
            code,
            modifiers: mods,
            kind: KeyEventKind::Press,
            state: KeyEventState::empty(),
        };

        // Reserved on any plugin screen: the emergency quit.
        assert!(is_host_reserved_key(
            &mk(CtKey::Char('c'), KeyModifiers::CONTROL),
            false,
            false
        ));
        // A plugin WITHOUT its own help keeps the host toggle while not capturing.
        assert!(is_host_reserved_key(
            &mk(CtKey::Char('?'), KeyModifiers::NONE),
            false,
            false
        ));
        assert!(is_host_reserved_key(
            &mk(CtKey::Char('H'), KeyModifiers::NONE),
            false,
            false
        ));
        assert!(!is_host_reserved_key(
            &mk(CtKey::Char('H'), KeyModifiers::NONE),
            false,
            true
        ));
        // A plugin WITH its own help owns `?`/`H` outright: reserving them off
        // the lagging per-frame capture flag stole the first keystrokes typed
        // into a plugin text field and then swallowed everything until Esc.
        assert!(!is_host_reserved_key(
            &mk(CtKey::Char('?'), KeyModifiers::NONE),
            true,
            false
        ));
        assert!(!is_host_reserved_key(
            &mk(CtKey::Char('H'), KeyModifiers::NONE),
            true,
            false
        ));
        // Esc is plugin-owned (overlay-panels redesign): the plugin pops
        // one internal level per press and publishes `ui.close_request`
        // at its root — see the doc on `is_host_reserved_key`.
        assert!(!is_host_reserved_key(
            &mk(CtKey::Esc, KeyModifiers::NONE),
            false,
            false
        ));

        // NOT reserved — these belong to the plugin on the analytics
        // screen (period switches, focus, filters, zoom).
        for k in [
            CtKey::Char('q'),
            CtKey::Char('a'),
            CtKey::Char('1'),
            CtKey::Char('2'),
            CtKey::Char('z'),
            CtKey::Enter,
            CtKey::Tab,
            CtKey::BackTab,
        ] {
            assert!(
                !is_host_reserved_key(&mk(k, KeyModifiers::NONE), false, false),
                "host must not reserve {k:?} — plugin owns it"
            );
        }

        assert!(
            is_host_reserved_key(&mk(CtKey::Char('c'), KeyModifiers::CONTROL), true, true),
            "Ctrl+C stays reserved even while the plugin captures text"
        );
    }

    fn key(
        code: crossterm::event::KeyCode,
        mods: crossterm::event::KeyModifiers,
    ) -> crossterm::event::KeyEvent {
        crossterm::event::KeyEvent {
            code,
            modifiers: mods,
            kind: crossterm::event::KeyEventKind::Press,
            state: crossterm::event::KeyEventState::empty(),
        }
    }

    fn on_plugin_screen(screen: &str, registered: bool, wedged: bool) -> AppState {
        let mut state = crate::app::state::AppState::default();
        state.shell.current_screen = screen.to_string();
        state.plugins_host.plugin_presence.insert(
            screen.to_string(),
            crate::app::sections::PluginPresence { registered, wedged },
        );
        state
    }

    /// Regression (PR #249 review HIGH-1): with the runtime up but the plugin
    /// NOT registered, the "[plugin unavailable]" placeholder, Esc/q go back to
    /// the central dispatch (PanelBack) so the placeholder stays escapable, and
    /// every other key stays claimed so session-list bindings (`d` delete, `n`
    /// new session, …) cannot fire from a dead plugin screen. A wedged render
    /// escapes the same way.
    #[test]
    fn esc_and_q_leave_an_absent_or_wedged_plugin_and_other_keys_stay_claimed() {
        use crossterm::event::{KeyCode as CtKey, KeyModifiers};

        for (registered, wedged) in [(false, false), (true, true)] {
            let state = on_plugin_screen(ids::ANALYTICS, registered, wedged);
            for code in [CtKey::Esc, CtKey::Char('q')] {
                assert_eq!(
                    route_key_to_focused_plugin(&state, &key(code, KeyModifiers::NONE)),
                    PluginRoute::Host,
                    "{code:?} leaves (registered {registered}, wedged {wedged})"
                );
            }
        }
        let absent = on_plugin_screen(ids::ANALYTICS, false, false);
        assert_eq!(
            route_key_to_focused_plugin(&absent, &key(CtKey::Char('d'), KeyModifiers::NONE)),
            PluginRoute::Consumed,
            "no destructive fallthrough from an absent plugin"
        );

        // Nothing reported for the screen yet (the runtime is not up, or the
        // first tick has not run): absent, so `d` is claimed and Esc still
        // leaves.
        let unreported = {
            let mut state = crate::app::state::AppState::default();
            state.shell.current_screen = ids::ANALYTICS.to_string();
            state
        };
        assert_eq!(
            route_key_to_focused_plugin(&unreported, &key(CtKey::Char('d'), KeyModifiers::NONE)),
            PluginRoute::Consumed,
            "an unreported plugin screen never hands `d` to the session list"
        );
        assert_eq!(
            route_key_to_focused_plugin(&unreported, &key(CtKey::Esc, KeyModifiers::NONE)),
            PluginRoute::Host,
            "and Esc still leaves it"
        );
    }

    /// A live plugin gets the key as an effect for the host, with `back` set
    /// on the keys that leave the screen, and nothing touches a runtime.
    #[test]
    fn a_key_for_a_live_plugin_is_an_effect_for_the_host() {
        use crossterm::event::{KeyCode as CtKey, KeyModifiers};

        let state = on_plugin_screen(ids::HANGAR, true, false);
        let PluginRoute::Forward(crate::app::Effect::ForwardToPlugin {
            plugin,
            screen,
            input,
            back,
        }) = route_key_to_focused_plugin(&state, &key(CtKey::Esc, KeyModifiers::NONE))
        else {
            panic!("Esc on a live plugin is forwarded");
        };
        assert_eq!(
            (plugin.as_str(), screen.as_str(), back),
            ("hangar-tui", ids::HANGAR, true)
        );
        assert!(matches!(input, crate::app::PluginInput::Key(_)));

        let PluginRoute::Forward(crate::app::Effect::ForwardToPlugin { back, .. }) =
            route_key_to_focused_plugin(&state, &key(CtKey::Char('j'), KeyModifiers::NONE))
        else {
            panic!("a plain key on a live plugin is forwarded");
        };
        assert!(!back);
    }

    /// On a plugin-owned screen the router FORWARDS `?`/`H` to the plugin
    /// whatever the per-frame `captures_text` stash says. That stash lags one
    /// render, so gating on it (8hx) still let the first keystrokes into a
    /// freshly opened plugin text field reach the host help toggle, which then
    /// swallowed every later key until Esc. `Ctrl+C` (host quit) stays
    /// reserved regardless.
    #[test]
    fn plugin_screen_forwards_help_keys_regardless_of_capture_flag() {
        use crossterm::event::{KeyCode as CtKey, KeyModifiers};

        let mut state = on_plugin_screen(ids::HANGAR, true, false);
        let forwarded = |state: &AppState, ch: char| {
            matches!(
                route_key_to_focused_plugin(state, &key(CtKey::Char(ch), KeyModifiers::NONE)),
                PluginRoute::Forward(_)
            )
        };

        assert!(
            !focused_plugin_captures_text(&state),
            "precondition: the capture stash is empty"
        );
        assert!(plugin_owns_help_keys(&state), "hangar renders its own help");
        assert!(
            forwarded(&state, 'H'),
            "H is forwarded before the capture frame lands"
        );
        assert!(
            forwarded(&state, '?'),
            "? is forwarded before the capture frame lands"
        );

        state.plugins_host.plugin_captures_text.insert(ids::HANGAR.to_string(), true);
        assert!(
            focused_plugin_captures_text(&state),
            "the stash drives focused_plugin_captures_text"
        );
        assert!(
            forwarded(&state, 'H'),
            "H reaches the plugin input while it captures text"
        );
        assert!(
            forwarded(&state, '?'),
            "? reaches the plugin input while it captures text"
        );
        assert_eq!(
            route_key_to_focused_plugin(&state, &key(CtKey::Char('c'), KeyModifiers::CONTROL)),
            PluginRoute::Host,
            "Ctrl+C (host quit) is never relaxed by text-capture"
        );
    }
}
