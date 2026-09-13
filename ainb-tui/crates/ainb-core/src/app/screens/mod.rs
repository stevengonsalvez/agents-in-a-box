// ABOUTME: Screen trait for in-tree views and plugin screens. Screen ids and the
// event outcome live in `ainb_app::app::screens`.

use crate::app::ui_state::UiState;
use ratatui::{Frame, layout::Rect};

use crate::app::AppState;

pub mod builtin;

pub use ainb_app::app::screens::*;

/// In-tree screen contract.
///
/// Phase 2a only wires `render`. Event routing through the registry lands in
/// later phases (and for plugin-owned screens via `ainb-plugin-host`).
pub trait Screen: Send {
    fn id(&self) -> &str;

    /// Render this screen into `area`. The frame may be the full terminal
    /// area; the screen is free to clip or carve sub-regions as needed.
    fn render(&mut self, frame: &mut Frame, area: Rect, state: &AppState, ui: &mut UiState);

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
