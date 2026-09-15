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
}
