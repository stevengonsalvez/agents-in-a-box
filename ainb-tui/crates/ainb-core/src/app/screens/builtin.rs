// ABOUTME: Built-in Screen impls — thin shims around existing component renderers; no logic moves in Phase 2a

use ratatui::{Frame, layout::Rect};

use super::{Screen, ids};
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

#[derive(Default)]
pub struct AnalyticsScreen;
impl Screen for AnalyticsScreen {
    fn id(&self) -> &str {
        ids::ANALYTICS
    }
    fn render(&mut self, frame: &mut Frame, area: Rect, state: &mut AppState) {
        crate::components::usage::render(frame, area, &state.usage_state);
    }
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

// ---------------------------------------------------------------------------
// Stateful screens — own their component instance
// ---------------------------------------------------------------------------

pub struct HomeScreen {
    component: HomeScreenV2Component,
}

impl HomeScreen {
    #[must_use]
    pub fn new() -> Self {
        Self { component: HomeScreenV2Component::new() }
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
        Self { component: AgentSelectionComponent::new() }
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
        Self { component: LogHistoryViewerComponent::new() }
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
        Self { component: OnboardingComponent }
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
        Self { component: AuthSetupComponent::new() }
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
        Self { component: AttachedTerminalComponent::new() }
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
    registry.register(Box::new(AnalyticsScreen::default()));
    registry.register(Box::new(SkillsScreen::default()));
    registry.register(Box::new(GitViewScreen::default()));
    registry.register(Box::new(SessionRecoveryScreen::default()));
    registry.register(Box::new(OnboardingScreen::new()));
    registry.register(Box::new(SetupMenuScreen::new()));
    registry.register(Box::new(AuthSetupScreen::new()));
    registry.register(Box::new(AttachedTerminalScreen::new()));
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
}
