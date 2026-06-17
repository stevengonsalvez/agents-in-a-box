// ABOUTME: Screen trait and stable ScreenId constants for in-tree views and (future) plugin screens

use ratatui::{Frame, layout::Rect};

use crate::app::AppState;

pub mod builtin;

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
    pub const AGENT_SELECTION: &str = "agent_selection";
    pub const CONFIG: &str = "config";
    pub const CATALOG: &str = "catalog";
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
    pub const INBOX: &str = "inbox";
    /// Hangar managed-agents control plane — a plugin-owned screen rendered by
    /// the `hangar-tui` subprocess plugin (P4.10). Reached from home with `g`.
    pub const HANGAR: &str = "hangar";
}

/// Outcome of a screen-handled event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventOutcome {
    /// Screen handled the event; do not propagate.
    Handled,
    /// Screen did not handle; let core logic run.
    NotHandled,
}

/// In-tree screen contract.
///
/// Phase 2a only wires `render`. Event routing through the registry lands in
/// later phases (and for plugin-owned screens via `ainb-plugin-host`).
pub trait Screen: Send {
    fn id(&self) -> &str;

    /// Render this screen into `area`. The frame may be the full terminal
    /// area; the screen is free to clip or carve sub-regions as needed.
    fn render(&mut self, frame: &mut Frame, area: Rect, state: &mut AppState);

    /// Stub for future event routing. Default: `NotHandled`.
    fn handle_event(&mut self, _state: &mut AppState) -> EventOutcome {
        EventOutcome::NotHandled
    }

    /// Hook for a screen to consume a single raw key event before the
    /// global key handler runs. Default: `NotHandled` (screen abstains —
    /// let the central dispatch in `app::events` do its thing).
    ///
    /// Plugin-owned screens (see `PluginScreen`) override this to
    /// translate the crossterm event into the portable wire shape and
    /// forward it down `plugin/handle_key`.
    fn handle_key(
        &mut self,
        _state: &mut AppState,
        _key: &crossterm::event::KeyEvent,
    ) -> EventOutcome {
        EventOutcome::NotHandled
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn screen_ids_are_unique() {
        let all = [
            ids::HOME,
            ids::AGENT_SELECTION,
            ids::CONFIG,
            ids::CATALOG,
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
            ids::INBOX,
        ];
        let mut sorted = all.to_vec();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), all.len(), "screen ids must be unique");
    }
}
