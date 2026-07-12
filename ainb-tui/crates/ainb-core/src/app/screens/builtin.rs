// ABOUTME: Built-in Screen impls — thin shims around existing component renderers; no logic moves in Phase 2a

use ratatui::{Frame, layout::Rect};

use super::{EventOutcome, Screen, ids};
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
/// `state.pending_plugin_renders[screen_id]` and paints it cell-by-cell
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
    plugin_id_for_screen(&state.current_screen).is_some()
        && state.plugin_captures_text.get(&state.current_screen).copied().unwrap_or(false)
}

/// `true` if the host reserves this key — it MUST NOT be forwarded to
/// the plugin and MUST fall through to the central dispatch.
///
/// The reservation list is deliberately small:
///
/// - `Ctrl+C` → host quit (ALWAYS reserved — never relaxed).
/// - `?` / `H` → help toggle — reserved UNLESS the focused plugin surface is
///   capturing text (`capturing == true`), in which case they are typed content
///   and must reach the plugin's input rather than toggle help. This is the
///   general fix for the class of bug where typing `H`/`?` into a plugin overlay
///   (e.g. the boards card-title input) was swallowed by the host help toggle
///   (8hx); it applies to every plugin screen, not just boards.
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
/// same one-level pop. When the plugin is missing or its runtime is
/// down, the forwarder returns `NotHandled` and Esc falls through to
/// the central dispatch, which still closes the placeholder screen.
///
/// `q`, `a`, `Tab`, `Enter`, etc. remain plugin-owned — the burndown
/// plugin re-binds them to period switches, panel focus, and zoom
/// toggles. Letting the host swallow them would make the screen
/// uninteractive.
#[must_use]
pub fn is_host_reserved_key(key: &crossterm::event::KeyEvent, capturing: bool) -> bool {
    use crossterm::event::{KeyCode as CtKey, KeyModifiers as CtMods};
    match key.code {
        CtKey::Char('c') if key.modifiers.contains(CtMods::CONTROL) => true,
        // `?`/`H` are reserved (help toggle) only while the plugin is NOT
        // capturing text; while it is, they're typed into the plugin's input.
        CtKey::Char('?' | 'H') => !capturing,
        _ => false,
    }
}

/// Try to forward `key` to the plugin owning `current_screen`. Returns
/// `Handled` if the host claimed it (plugin forwarder ran or the host
/// reservation list bailed us out), `NotHandled` if the caller's
/// upstream key dispatch should run instead (no plugin owns this
/// screen, or no plugin runtime is initialised yet).
pub fn forward_key_to_focused_plugin(
    state: &mut AppState,
    key: &crossterm::event::KeyEvent,
) -> EventOutcome {
    let Some(plugin_name) = plugin_id_for_screen(&state.current_screen) else {
        return EventOutcome::NotHandled;
    };
    // While the focused plugin surface captures text, `?`/`H` are typed content
    // and must be forwarded, not reserved for the host help toggle (8hx).
    let capturing = focused_plugin_captures_text(state);
    if is_host_reserved_key(key, capturing) {
        // Host claims this key — let the central dispatch in
        // `events.rs` resolve it to Quit / ToggleHelp / etc.
        return EventOutcome::NotHandled;
    }
    let Some(runtime) = state.plugin_runtime.as_ref() else {
        return EventOutcome::NotHandled;
    };
    let Some(protocol_key) = crossterm_to_protocol_key(key) else {
        // Unmodelled key (e.g. media keys) — silently drop. Better
        // than forging a wire shape.
        return EventOutcome::Handled;
    };
    let pid = ainb_plugin_runtime::PluginId::from(plugin_name);
    let delivered = runtime.send_key(&pid, state.current_screen.clone(), protocol_key);
    if !delivered
        && matches!(
            key.code,
            crossterm::event::KeyCode::Esc | crossterm::event::KeyCode::Char('q')
        )
    {
        // Plugin unregistered or its task is gone — the screen is
        // showing the "plugin unavailable" placeholder and the key
        // just got dropped on the floor. Esc/q must stay escapable
        // there (the central dispatch resolves them to PanelBack), or
        // the user is trapped with only Ctrl+C. Other keys stay
        // claimed so the session-list fallthrough's destructive
        // bindings (`d` delete, `n` new session, …) can't fire from a
        // dead plugin screen.
        return EventOutcome::NotHandled;
    }
    EventOutcome::Handled
}

/// Convert a `crossterm::event::MouseEvent` into the portable wire shape
/// consumed by `plugin/handle_mouse`. Coordinates are left as the
/// absolute terminal column/row the caller received — translation into
/// the plugin's viewport space happens in
/// [`forward_mouse_to_focused_plugin`], which knows the screen origin.
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

/// Try to forward `event` to the plugin owning `current_screen`.
///
/// Mirrors [`forward_key_to_focused_plugin`] for the pointer. Returns
/// `Handled` whenever a plugin owns the focused screen — even if the
/// event is dropped (outside the plugin's rect, or a high-frequency
/// `Moved`) — so the host's own mouse handling never double-acts on a
/// plugin screen. Returns `NotHandled` when no plugin owns the screen or
/// the runtime isn't up yet, letting the caller's host-side mouse
/// dispatch run.
///
/// Coordinates are translated from absolute terminal space into the
/// plugin's viewport (origin subtracted) before forwarding, so the
/// plugin hit-tests against the same `(0, 0)`-based grid it painted.
pub fn forward_mouse_to_focused_plugin(
    state: &mut AppState,
    event: &crossterm::event::MouseEvent,
) -> EventOutcome {
    use ainb_plugin_runtime::MouseKind;

    let Some(plugin_name) = plugin_id_for_screen(&state.current_screen) else {
        return EventOutcome::NotHandled;
    };
    let Some(runtime) = state.plugin_runtime.as_ref() else {
        return EventOutcome::NotHandled;
    };

    let mut mouse = crossterm_to_protocol_mouse(event);

    // Drop pointer-move spam: forwarding every `Moved` would flood the
    // priority mouse channel for an event the map doesn't need (no hover
    // semantics in v1). Still report Handled so host move-handling stays
    // off the plugin screen.
    if matches!(mouse.kind, MouseKind::Moved) {
        return EventOutcome::Handled;
    }

    // Translate absolute terminal coords → plugin-viewport coords. Drop
    // (still Handled) when the point falls outside the plugin's painted
    // rect rather than forwarding a click the plugin would mis-hit-test.
    let origin = state
        .plugin_render_origins
        .get(&state.current_screen)
        .copied()
        .unwrap_or((0, 0));
    let area = state.plugin_render_areas.get(&state.current_screen).copied().unwrap_or((0, 0));
    let Some((col, row)) = click_to_viewport(mouse.col, mouse.row, origin, area) else {
        return EventOutcome::Handled;
    };
    mouse.col = col;
    mouse.row = row;

    let pid = ainb_plugin_runtime::PluginId::from(plugin_name);
    let _ = runtime.send_mouse(&pid, state.current_screen.clone(), mouse);
    EventOutcome::Handled
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
/// but no frame has arrived yet. Two cases:
///
/// 1. **Plugin not registered** — runtime came up but this plugin is
///    absent. Either disabled (`AINB_DISABLE_PLUGINS=1`,
///    `AINB_DISABLE_PLUGIN=<id>`, `AINB_ONLY_PLUGINS` omits it,
///    `config.toml [plugins]` excludes it) or never installed at all.
///    Show actionable text so the user knows it's a deliberate state,
///    not a hang.
/// 2. **Plugin registered, no frame yet** — runtime spawned the
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
    let plugin_registered = match (plugin_name, state.plugin_runtime.as_ref()) {
        (Some(name), Some(rt)) => {
            let pid = ainb_plugin_runtime::PluginId::from(name);
            rt.lifecycle_state(&pid).is_some()
        }
        _ => false,
    };

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
    fn render(&mut self, frame: &mut Frame, area: Rect, state: &mut AppState) {
        // Stash the allocated size so the next tick of
        // `App::tick_plugin_renders` can ask the plugin for a buffer that
        // actually fills this area. Without this the plugin renders into
        // its fallback (80×24) and everything outside that rect stays blank.
        state
            .plugin_render_areas
            .insert(self.screen_id.to_string(), (area.width, area.height));
        // Stash the origin too, so the mouse forwarder can translate an
        // absolute terminal click into this plugin's viewport space.
        state.plugin_render_origins.insert(self.screen_id.to_string(), (area.x, area.y));

        let Some(wire) = state.pending_plugin_renders.get(self.screen_id) else {
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

    fn handle_key(
        &mut self,
        state: &mut AppState,
        key: &crossterm::event::KeyEvent,
    ) -> EventOutcome {
        forward_key_to_focused_plugin(state, key)
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
    fn render(&mut self, frame: &mut Frame, area: Rect, state: &mut AppState) {
        crate::components::skills::render(frame, area, &state.skills_state);
    }
}

#[derive(Default)]
pub struct SkillManagerScreen;
impl Screen for SkillManagerScreen {
    fn id(&self) -> &str {
        ids::SKILL_MANAGER
    }
    fn render(&mut self, frame: &mut Frame, area: Rect, state: &mut AppState) {
        crate::components::skill_manager_screen::render(frame, area, &state.skill_manager_state);
    }
}

#[derive(Default)]
pub struct ChangelogScreen;
impl Screen for ChangelogScreen {
    fn id(&self) -> &str {
        ids::CHANGELOG
    }
    fn render(&mut self, frame: &mut Frame, area: Rect, state: &mut AppState) {
        ChangelogComponent::render(frame, area, &state.changelog_state);
    }
}

#[derive(Default)]
pub struct GitViewScreen;
impl Screen for GitViewScreen {
    fn id(&self) -> &str {
        ids::GIT_VIEW
    }
    fn render(&mut self, frame: &mut Frame, area: Rect, state: &mut AppState) {
        if let Some(ref git_state) = state.git_view_state {
            GitViewComponent::render(frame, area, git_state);
        }
    }
}

#[derive(Default)]
pub struct SessionRecoveryScreen;
impl Screen for SessionRecoveryScreen {
    fn id(&self) -> &str {
        ids::SESSION_RECOVERY
    }
    fn render(&mut self, frame: &mut Frame, area: Rect, state: &mut AppState) {
        SessionRecovery::render(frame, area, &mut state.session_recovery_state);
    }
}

/// Inbox screen — surfaces ainb-hooks notifications from
/// `~/.agents-in-a-box/notifications.db`. The screen pulls its
/// per-session state from `AppState::inbox_state` so selection +
/// filters survive cross-screen navigation.
#[derive(Default)]
pub struct InboxScreen;

impl Screen for InboxScreen {
    fn id(&self) -> &str {
        ids::INBOX
    }
    fn render(&mut self, frame: &mut Frame, area: Rect, state: &mut AppState) {
        crate::components::inbox::render(frame, area, &mut state.inbox_state);
    }
}

/// Daemons screen — read-only runtime health of every long-running ainb daemon
/// (phone bridge / notifyd / ATC / fleet daemon). Renders from
/// `fleet::daemons::collect` via the shared component, refreshing live on the
/// render tick. State lives on `AppState::daemons_state` so the cached snapshot
/// survives cross-screen navigation.
#[derive(Default)]
pub struct DaemonsScreen;

impl Screen for DaemonsScreen {
    fn id(&self) -> &str {
        ids::DAEMONS
    }
    fn render(&mut self, frame: &mut Frame, area: Rect, state: &mut AppState) {
        crate::components::daemons::render(frame, area, &mut state.daemons_state);
    }
}

/// Fleet control panel — the interactive "who-needs-you" looking-glass. Reads
/// the event-sourced `current_state` (ASK/ERR/WAIT/IDLE per session) via the
/// shared component, refreshing on the render tick, and acts (answer
/// interviews / broadcast) through the existing fleet send path. State lives on
/// `AppState::fleet_panel_state` so selection survives cross-screen navigation.
#[derive(Default)]
pub struct FleetPanelScreen;

impl Screen for FleetPanelScreen {
    fn id(&self) -> &str {
        ids::FLEET_PANEL
    }
    fn render(&mut self, frame: &mut Frame, area: Rect, state: &mut AppState) {
        crate::components::fleet_panel::render(frame, area, &mut state.fleet_panel_state);
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
    fn render(&mut self, frame: &mut Frame, area: Rect, state: &mut AppState) {
        self.component.render_with_loading(
            frame,
            area,
            &mut state.home_screen_v2_state,
            &state.workspaces,
            state.is_loading_workspaces,
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
    fn render(&mut self, frame: &mut Frame, area: Rect, state: &mut AppState) {
        self.component.render(frame, area, state);
        if state.auth_provider_popup_state.show_popup {
            self.auth_provider_popup.render(frame, area, state);
        }
        if state.config_popup_state.show_popup {
            self.config_popup.render(frame, area, &state.config_popup_state);
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
    fn render(&mut self, frame: &mut Frame, area: Rect, state: &mut AppState) {
        self.component.render(frame, area, &mut state.log_history_state);
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
    fn render(&mut self, frame: &mut Frame, area: Rect, state: &mut AppState) {
        if let Some(ref onboarding_state) = state.onboarding_state {
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
    fn render(&mut self, frame: &mut Frame, area: Rect, state: &mut AppState) {
        self.backdrop.render_with_loading(
            frame,
            area,
            &mut state.home_screen_v2_state,
            &state.workspaces,
            state.is_loading_workspaces,
        );
        self.setup_menu.render(frame, area, &state.setup_menu_state);
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
    fn render(&mut self, frame: &mut Frame, area: Rect, state: &mut AppState) {
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
    fn render(&mut self, frame: &mut Frame, area: Rect, state: &mut AppState) {
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
    registry.register(Box::new(InboxScreen));
    registry.register(Box::new(DaemonsScreen));
    registry.register(Box::new(FleetPanelScreen));
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
            ids::INBOX,
            ids::DAEMONS,
            ids::FLEET_PANEL,
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

        // Reserved (plugin NOT capturing text).
        assert!(is_host_reserved_key(
            &mk(CtKey::Char('c'), KeyModifiers::CONTROL),
            false
        ));
        assert!(is_host_reserved_key(
            &mk(CtKey::Char('?'), KeyModifiers::NONE),
            false
        ));
        assert!(is_host_reserved_key(
            &mk(CtKey::Char('H'), KeyModifiers::NONE),
            false
        ));
        // Esc is plugin-owned (overlay-panels redesign): the plugin pops
        // one internal level per press and publishes `ui.close_request`
        // at its root — see the doc on `is_host_reserved_key`.
        assert!(!is_host_reserved_key(
            &mk(CtKey::Esc, KeyModifiers::NONE),
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
                !is_host_reserved_key(&mk(k, KeyModifiers::NONE), false),
                "host must not reserve {k:?} — plugin owns it"
            );
        }

        // 8hx: while the focused plugin surface captures text, `?`/`H` are typed
        // content and must NOT be reserved — they get forwarded to the plugin's
        // input. `Ctrl+C` (host quit) stays reserved even while capturing.
        assert!(
            !is_host_reserved_key(&mk(CtKey::Char('H'), KeyModifiers::NONE), true),
            "H must reach the plugin input while it captures text"
        );
        assert!(
            !is_host_reserved_key(&mk(CtKey::Char('?'), KeyModifiers::NONE), true),
            "? must reach the plugin input while it captures text"
        );
        assert!(
            is_host_reserved_key(&mk(CtKey::Char('c'), KeyModifiers::CONTROL), true),
            "Ctrl+C stays reserved even while the plugin captures text"
        );
    }

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

    /// Regression (PR #249 review HIGH-1): with the runtime up but the
    /// plugin NOT registered — exactly the "[plugin unavailable]"
    /// placeholder state — `send_key` drops the keystroke. Esc/q must
    /// fall through to the central dispatch (→ PanelBack) so the
    /// placeholder stays escapable; every other key stays claimed so
    /// session-list bindings (`d` delete, `n` new session, …) can't
    /// fire from a dead plugin screen.
    #[test]
    fn undelivered_esc_and_q_fall_through_on_placeholder_screen() {
        use crossterm::event::{
            KeyCode as CtKey, KeyEvent as CtEvent, KeyEventKind, KeyEventState, KeyModifiers,
        };

        // Real runtime, zero plugins registered → lifecycle_state(burndown)
        // is None and send_key returns false.
        let (runtime, handle) =
            ainb_plugin_runtime::Runtime::new().expect("runtime constructs without plugins");
        let mut state = crate::app::state::AppState::default();
        state.plugin_runtime = Some(handle);
        state.current_screen = ids::ANALYTICS.to_string();

        let mk = |code| CtEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::empty(),
        };

        assert!(
            matches!(
                forward_key_to_focused_plugin(&mut state, &mk(CtKey::Esc)),
                EventOutcome::NotHandled
            ),
            "undelivered Esc must fall through so PanelBack can run"
        );
        assert!(
            matches!(
                forward_key_to_focused_plugin(&mut state, &mk(CtKey::Char('q'))),
                EventOutcome::NotHandled
            ),
            "undelivered q must fall through so PanelBack can run"
        );
        assert!(
            matches!(
                forward_key_to_focused_plugin(&mut state, &mk(CtKey::Char('d'))),
                EventOutcome::Handled
            ),
            "undelivered non-nav keys stay claimed — no destructive fallthrough"
        );

        runtime.shutdown();
    }

    /// 8hx: while the focused plugin surface declares text-capture (stashed in
    /// `plugin_captures_text`), the forwarder must FORWARD `?`/`H` to the plugin
    /// (claiming the key) instead of treating them as host-reserved help keys
    /// that fall through to the global help toggle. `Ctrl+C` (host quit) stays
    /// reserved regardless. This is the second of the two host interception
    /// points the fix closes — the first is `is_text_input_context` gating the
    /// global shortcut handlers.
    #[test]
    fn capturing_plugin_forwards_help_keys_instead_of_reserving_them() {
        use crossterm::event::{
            KeyCode as CtKey, KeyEvent as CtEvent, KeyEventKind, KeyEventState, KeyModifiers,
        };

        // Runtime up, plugin not registered — send_key returns false but the
        // forwarder still CLAIMS a non-nav key (returns Handled), so this
        // isolates the reservation decision from live delivery.
        let (runtime, handle) =
            ainb_plugin_runtime::Runtime::new().expect("runtime constructs without plugins");
        let mut state = crate::app::state::AppState::default();
        state.plugin_runtime = Some(handle);
        state.current_screen = ids::HANGAR.to_string();

        let mk = |code, mods| CtEvent {
            code,
            modifiers: mods,
            kind: KeyEventKind::Press,
            state: KeyEventState::empty(),
        };
        // Forward a bare (unmodified) char key to the focused plugin.
        let fwd = |state: &mut AppState, ch: char| {
            forward_key_to_focused_plugin(state, &mk(CtKey::Char(ch), KeyModifiers::NONE))
        };

        // Not capturing → `?`/`H` are host-reserved (fall through to help toggle).
        assert!(
            matches!(fwd(&mut state, 'H'), EventOutcome::NotHandled),
            "without capture, H is reserved for the host help toggle"
        );
        assert!(
            matches!(fwd(&mut state, '?'), EventOutcome::NotHandled),
            "without capture, ? is reserved for the host help toggle"
        );

        // Declare text-capture (as the plugin's `captures_text` frame would).
        state.plugin_captures_text.insert(ids::HANGAR.to_string(), true);
        assert!(
            focused_plugin_captures_text(&state),
            "the stash drives focused_plugin_captures_text"
        );
        assert!(
            matches!(fwd(&mut state, 'H'), EventOutcome::Handled),
            "H must be forwarded to the plugin input while it captures text (8hx)"
        );
        assert!(
            matches!(fwd(&mut state, '?'), EventOutcome::Handled),
            "? must be forwarded to the plugin input while it captures text (8hx)"
        );
        // Ctrl+C stays host-reserved even while the plugin captures text.
        assert!(
            matches!(
                forward_key_to_focused_plugin(
                    &mut state,
                    &mk(CtKey::Char('c'), KeyModifiers::CONTROL)
                ),
                EventOutcome::NotHandled
            ),
            "Ctrl+C (host quit) is never relaxed by text-capture"
        );

        runtime.shutdown();
    }
}
