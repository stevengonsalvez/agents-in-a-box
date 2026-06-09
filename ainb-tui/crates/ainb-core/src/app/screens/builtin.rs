// ABOUTME: Built-in Screen impls — thin shims around existing component renderers; no logic moves in Phase 2a

use ratatui::{Frame, layout::Rect};

use super::{EventOutcome, Screen, ids};
use crate::app::AppState;
use crate::components::{
    AgentSelectionComponent, AttachedTerminalComponent, AuthProviderPopupComponent,
    AuthSetupComponent, ChangelogComponent, ConfigPopupComponent, ConfigScreenComponent,
    GitViewComponent, HomeScreenV2Component, LogHistoryViewerComponent, OnboardingComponent,
    SessionRecovery, SetupMenuComponent,
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
    (ids::ABTOP, "abtop"),
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

/// `true` if the host reserves this key — it MUST NOT be forwarded to
/// the plugin and MUST fall through to the central dispatch.
///
/// The reservation list is deliberately small:
///
/// - `Ctrl+C` → host quit (always).
/// - `?` / `H` → help toggle (already short-circuited above
///   `handle_key_event`'s plugin branch, but listed here for parity in
///   the `PluginScreen` trait impl path).
/// - `Esc` → pop screen / return to home. The plugin/handle_key wire
///   method is a one-way notification (no `consumed` reply), so once
///   the host forwards a keystroke it has no way to know whether the
///   plugin had any state to consume it. The user-visible failure is
///   Esc-from-burndown silently disappearing into the plugin even when
///   there's no filter chip or zoom to pop. Until the wire protocol is
///   extended with a `consumed: bool` result for `plugin/handle_key`,
///   Esc belongs to the host. Plugins re-bind internal pop semantics
///   to `Backspace` (see burndown's `KeyCode::Backspace` handler).
///
///   UX note: a side-effect of routing Esc straight to the host is
///   that Esc on a *zoomed* plugin view does NOT first un-zoom — it
///   navigates straight to home, discarding zoom state. Users who
///   want a one-level pop press `Backspace` (closes zoom, stays on
///   the screen). The burndown help bar advertises both keys.
///
/// `q`, `a`, `Tab`, `Enter`, etc. remain plugin-owned — the burndown
/// plugin re-binds them to period switches, panel focus, and zoom
/// toggles. Letting the host swallow them would make the screen
/// uninteractive.
#[must_use]
pub fn is_host_reserved_key(key: &crossterm::event::KeyEvent) -> bool {
    use crossterm::event::{KeyCode as CtKey, KeyModifiers as CtMods};
    match key.code {
        CtKey::Char('c') if key.modifiers.contains(CtMods::CONTROL) => true,
        CtKey::Char('?' | 'H') => true,
        CtKey::Esc => true,
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
    if is_host_reserved_key(key) {
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
    let _ = runtime.send_key(&pid, state.current_screen.clone(), protocol_key);
    EventOutcome::Handled
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
) -> ratatui::widgets::Paragraph<'static> {
    use ratatui::{
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
        // Genuine transient render lag.
        let line = Line::from(vec![
            Span::styled("  ⏳ ", Style::default().fg(Color::Yellow)),
            Span::styled(
                format!("Loading {}…", screen_id),
                Style::default().add_modifier(Modifier::BOLD),
            ),
        ]);
        return Paragraph::new(line);
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

        let Some(wire) = state.pending_plugin_renders.get(self.screen_id) else {
            let placeholder = build_placeholder_for_unloaded_plugin(self.screen_id, state);
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
            let target = buf.get_mut(area.x + coord.x, area.y + coord.y);
            target.set_symbol(&cell.symbol);
            target.set_fg(rgb_to_color(cell.fg));
            target.set_bg(rgb_to_color(cell.bg));
            target.set_style(
                ratatui::style::Style::default()
                    .add_modifier(modifier_bits_to_modifiers(cell.modifier)),
            );
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

pub struct AgentSelectionScreen {
    component: AgentSelectionComponent,
}

impl AgentSelectionScreen {
    #[must_use]
    pub fn new() -> Self {
        Self {
            component: AgentSelectionComponent::new(),
        }
    }
}

impl Default for AgentSelectionScreen {
    fn default() -> Self {
        Self::new()
    }
}

impl Screen for AgentSelectionScreen {
    fn id(&self) -> &str {
        ids::AGENT_SELECTION
    }
    fn render(&mut self, frame: &mut Frame, area: Rect, state: &mut AppState) {
        self.component.render(frame, area, state);
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
    registry.register(Box::new(AgentSelectionScreen::new()));
    registry.register(Box::new(ConfigScreen::new()));
    registry.register(Box::new(LogHistoryScreen::new()));
    registry.register(Box::new(ChangelogScreen::default()));
    registry.register(Box::new(PluginScreen::new(ids::ANALYTICS)));
    registry.register(Box::new(PluginScreen::new(ids::WITR)));
    registry.register(Box::new(PluginScreen::new(ids::ABTOP)));
    registry.register(Box::new(SkillsScreen::default()));
    registry.register(Box::new(GitViewScreen::default()));
    registry.register(Box::new(SessionRecoveryScreen::default()));
    registry.register(Box::new(OnboardingScreen::new()));
    registry.register(Box::new(SetupMenuScreen::new()));
    registry.register(Box::new(AuthSetupScreen::new()));
    registry.register(Box::new(AttachedTerminalScreen::new()));
    registry.register(Box::new(InboxScreen));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_builtins_populates_registry() {
        let mut r = ScreenRegistry::new();
        register_builtins(&mut r);
        // Every full-screen view gets a Screen impl.
        for id in [
            ids::HOME,
            ids::AGENT_SELECTION,
            ids::CONFIG,
            ids::LOG_HISTORY,
            ids::CHANGELOG,
            ids::ANALYTICS,
            ids::WITR,
            ids::ABTOP,
            ids::SKILLS,
            ids::GIT_VIEW,
            ids::SESSION_RECOVERY,
            ids::ONBOARDING,
            ids::SETUP_MENU,
            ids::AUTH_SETUP,
            ids::ATTACHED_TERMINAL,
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

        // Reserved.
        assert!(is_host_reserved_key(&mk(
            CtKey::Char('c'),
            KeyModifiers::CONTROL
        )));
        assert!(is_host_reserved_key(&mk(
            CtKey::Char('?'),
            KeyModifiers::NONE
        )));
        assert!(is_host_reserved_key(&mk(
            CtKey::Char('H'),
            KeyModifiers::NONE
        )));
        // Esc is reserved: it must always bubble to the host so the
        // screen pops back to home — see the doc on
        // `is_host_reserved_key`. Plugins use `Backspace` for pop-state
        // semantics instead.
        assert!(is_host_reserved_key(&mk(CtKey::Esc, KeyModifiers::NONE)));

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
                !is_host_reserved_key(&mk(k, KeyModifiers::NONE)),
                "host must not reserve {k:?} — plugin owns it"
            );
        }
    }

    #[test]
    fn plugin_id_for_screen_resolves_analytics() {
        assert_eq!(plugin_id_for_screen(ids::ANALYTICS), Some("burndown"));
        assert_eq!(plugin_id_for_screen(ids::WITR), Some("witr"));
        assert_eq!(plugin_id_for_screen(ids::ABTOP), Some("abtop"));
        // Non-plugin screens return None so the forwarder bails early.
        assert_eq!(plugin_id_for_screen(ids::HOME), None);
        assert_eq!(plugin_id_for_screen("nonsense"), None);
    }
}
