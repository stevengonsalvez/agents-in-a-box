// ABOUTME: Stable ScreenId constants for in-tree views and plugin screens, and
// the outcome a screen reports for an event. The Screen trait, which draws, lives
// in `ainb-core::app::screens`.

/// Stable string identifier for a screen.
///
/// Replaces the prior `View` enum (state.rs) with a string-keyed identifier so
/// plugin-registered screens (Phase 3+) can coexist with built-in screens
/// without enum churn.
pub type ScreenId = String;

/// Built-in screen identifiers. Kept as `&'static str` so they can be used in
/// equality comparisons against `ScreenId` (`String`) without allocation.
pub mod ids {
    pub const HOME: &str = "home";
    pub const CONFIG: &str = "config";
    pub const ANALYTICS: &str = "analytics";
    /// Process-causality screen, owned by the `witr` plugin. Generic
    /// plugin-screen registration — no witr domain logic in the host;
    /// the same plumbing the `analytics` screen uses for `burndown`.
    pub const WITR: &str = "witr";
    /// Knowledge-base browser screen, owned by the `learnings` plugin.
    /// Generic plugin-screen registration — no learnings domain logic in
    /// the host; the same plumbing `analytics`/`witr` use.
    pub const LEARNINGS: &str = "learnings";
    /// top-for-agents screen, owned by the `abtop` plugin. Generic
    /// plugin-screen registration like `witr`; the live menu hands the
    /// terminal to the external `abtop` binary full-screen, so this
    /// screen only paints the plugin's install-hint / ready empty-state.
    pub const ABTOP: &str = "abtop";
    pub const SESSION_LIST: &str = "session_list";
    pub const LOGS: &str = "logs";
    pub const LOG_HISTORY: &str = "log_history";
    pub const TERMINAL: &str = "terminal";
    pub const HELP: &str = "help";
    pub const NEW_SESSION: &str = "new_session";
    pub const SEARCH_WORKSPACE: &str = "search_workspace";
    pub const NON_GIT_NOTIFICATION: &str = "non_git_notification";
    pub const ATTACHED_TERMINAL: &str = "attached_terminal";
    pub const AUTH_SETUP: &str = "auth_setup";
    pub const CLAUDE_CHAT: &str = "claude_chat";
    pub const GIT_VIEW: &str = "git_view";
    pub const ONBOARDING: &str = "onboarding";
    pub const SETUP_MENU: &str = "setup_menu";
    pub const CHANGELOG: &str = "changelog";
    pub const SESSION_RECOVERY: &str = "session_recovery";
    pub const SKILLS: &str = "skills";
    pub const SKILL_MANAGER: &str = "skill_manager";
    /// Hangar managed-agents control plane — a plugin-owned screen rendered by
    /// the `hangar-tui` subprocess plugin (P4.10). Reached from home with `g`.
    pub const HANGAR: &str = "hangar";
    /// Daemons runtime-health and repair screen for phone bridge, notifyd,
    /// ATC, and fleet daemons. Reached from home with `d`.
    pub const DAEMONS: &str = "daemons";
}

/// Outcome of a screen-handled event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventOutcome {
    /// Screen handled the event; do not propagate.
    Handled,
    /// Screen did not handle; let core logic run.
    NotHandled,
}

pub mod builtin;

/// Where plugin screens were last drawn, which the plugin render tick needs to
/// ask each plugin for a frame at the right size. Written by the renderer that
/// draws the plugin screens; read and updated by `App::tick_plugin_renders`.
#[derive(Debug, Default, Clone)]
pub struct PluginViewports {
    /// The `(width, height)` each plugin screen's last frame was allocated.
    pub render_areas: std::collections::HashMap<ScreenId, (u16, u16)>,
    /// The `(width, height)` each plugin was last asked to render at, so a
    /// changed area forces a fresh render.
    pub last_render_viewport: std::collections::HashMap<ScreenId, (u16, u16)>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn screen_ids_are_unique() {
        let all = [
            ids::HOME,
            ids::CONFIG,
            ids::ANALYTICS,
            ids::SESSION_LIST,
            ids::LOGS,
            ids::LOG_HISTORY,
            ids::TERMINAL,
            ids::HELP,
            ids::NEW_SESSION,
            ids::SEARCH_WORKSPACE,
            ids::NON_GIT_NOTIFICATION,
            ids::ATTACHED_TERMINAL,
            ids::AUTH_SETUP,
            ids::CLAUDE_CHAT,
            ids::GIT_VIEW,
            ids::ONBOARDING,
            ids::SETUP_MENU,
            ids::CHANGELOG,
            ids::SESSION_RECOVERY,
            ids::SKILLS,
            ids::SKILL_MANAGER,
        ];
        let mut sorted = all.to_vec();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), all.len(), "screen ids must be unique");
    }
}
