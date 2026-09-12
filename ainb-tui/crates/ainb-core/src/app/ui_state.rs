//! Renderer-local state for the ratatui host (plan Phase 3, the "scroll seal").
//!
//! Everything here is geometry, scroll position, hover and per-frame cache: it
//! describes what the terminal is currently painting, never what the product
//! knows. It exists so the draw path can take `&AppState` and mutate nothing
//! behind the reducer, which is what makes a version-on-mutation scheme over
//! `AppState` (Phase 2) mean anything: after this seal, a bumped section is a
//! real change, not a repaint.
//!
//! Nothing in here crosses to another surface. A desktop or web renderer keeps
//! its own equivalent; only `AppState` is shared.

use std::collections::HashMap;
use std::time::Instant;

use ratatui::layout::Rect;
use ratatui::widgets::ListState;

use crate::app::keymap::UiAction;
use crate::app::screens::ScreenId;
use crate::app::state::{
    AppState, AttachableRef, COLLAPSED_SESSIONS_SIDEBAR_WIDTH, DEFAULT_SESSIONS_SIDEBAR_WIDTH,
    MIN_SESSIONS_SIDEBAR_WIDTH, SESSIONS_PREVIEW_RESERVE, SESSIONS_ROW_DOUBLE_CLICK_WINDOW,
    SessionListRowTarget,
};
use crate::components::layout::LayoutComponent;

#[derive(Debug, Clone)]
pub struct SessionsPaneState {
    pub preferred_width: u16,
    pub collapsed: bool,
    resize_active: bool,
    edge_hovered: bool,
    last_sessions_rect: Option<Rect>,
    last_preview_rect: Option<Rect>,
    last_list_scroll_offset: usize,
    /// Physical terminal-line height for each logical `ListItem` from the
    /// latest render. Session rows have metadata on a second line, while
    /// headers and separators remain one line; mouse hit-testing needs this
    /// mapping rather than assuming one item equals one terminal row.
    last_list_item_heights: Vec<usize>,
    last_attachable_click: Option<(AttachableRef, Instant)>,
    filter_toggle_area: Option<Rect>,
}

impl Default for SessionsPaneState {
    fn default() -> Self {
        Self {
            preferred_width: DEFAULT_SESSIONS_SIDEBAR_WIDTH,
            collapsed: false,
            resize_active: false,
            edge_hovered: false,
            last_sessions_rect: None,
            last_preview_rect: None,
            last_list_scroll_offset: 0,
            last_list_item_heights: Vec::new(),
            last_attachable_click: None,
            filter_toggle_area: None,
        }
    }
}

impl SessionsPaneState {
    pub fn restore(&mut self, width: Option<u16>, collapsed: bool) {
        if let Some(width) = width {
            self.preferred_width = width.max(MIN_SESSIONS_SIDEBAR_WIDTH);
        }
        self.collapsed = collapsed;
    }

    pub fn set_layout(&mut self, sessions_rect: Rect, preview_rect: Rect) {
        self.last_sessions_rect = Some(sessions_rect);
        self.last_preview_rect = Some(preview_rect);
    }

    pub fn set_list_scroll_offset(&mut self, offset: usize) {
        self.last_list_scroll_offset = offset;
    }

    pub fn set_list_item_heights(&mut self, heights: Vec<usize>) {
        self.last_list_item_heights = heights;
    }

    pub fn set_filter_toggle_area(&mut self, area: Rect) {
        self.filter_toggle_area = Some(area);
    }

    pub fn is_on_filter_toggle(&self, x: u16, y: u16) -> bool {
        self.filter_toggle_area.is_some_and(|area| {
            x >= area.x
                && x < area.x.saturating_add(area.width)
                && y >= area.y
                && y < area.y.saturating_add(area.height)
        })
    }

    pub fn last_content_width(&self) -> Option<u16> {
        Some(self.last_sessions_rect?.width.saturating_add(self.last_preview_rect?.width))
    }

    pub fn effective_width(&self, terminal_width: u16) -> u16 {
        if self.collapsed {
            return COLLAPSED_SESSIONS_SIDEBAR_WIDTH.min(terminal_width);
        }

        Self::clamp_width(self.preferred_width, terminal_width)
    }

    pub fn clamp_width(width: u16, terminal_width: u16) -> u16 {
        if terminal_width <= COLLAPSED_SESSIONS_SIDEBAR_WIDTH {
            return terminal_width;
        }

        let max_width = terminal_width.saturating_sub(SESSIONS_PREVIEW_RESERVE);
        if max_width < MIN_SESSIONS_SIDEBAR_WIDTH {
            return terminal_width.saturating_sub(1).max(1);
        }

        width.clamp(MIN_SESSIONS_SIDEBAR_WIDTH, max_width)
    }

    pub fn expanded_width(&self, terminal_width: u16) -> u16 {
        Self::clamp_width(self.preferred_width, terminal_width)
    }

    pub fn edge_highlighted(&self) -> bool {
        self.edge_hovered || self.resize_active
    }

    pub fn is_on_edge(&self, x: u16, y: u16) -> bool {
        if self.collapsed {
            return false;
        }

        let Some(rect) = self.last_sessions_rect else {
            return false;
        };
        if y < rect.y || y >= rect.y.saturating_add(rect.height) || rect.width == 0 {
            return false;
        }

        let edge_x = rect.x.saturating_add(rect.width.saturating_sub(1));
        x.abs_diff(edge_x) <= 1
    }

    pub fn is_on_toggle(&self, x: u16, y: u16) -> bool {
        let Some(rect) = self.last_sessions_rect else {
            return false;
        };
        if rect.width == 0 {
            return false;
        }

        let on_x = x >= rect.x && x < rect.x.saturating_add(rect.width);
        if !on_x {
            return false;
        }

        if self.collapsed {
            // Expanded pane puts `[-]` in the block title on the top border.
            // Collapsed rail renders `[+]` as first content row inside the block.
            return y == rect.y || y == rect.y.saturating_add(1);
        }

        y == rect.y
    }

    pub fn contains_sessions_point(&self, x: u16, y: u16) -> bool {
        let Some(rect) = self.last_sessions_rect else {
            return false;
        };
        x >= rect.x
            && x < rect.x.saturating_add(rect.width)
            && y >= rect.y
            && y < rect.y.saturating_add(rect.height)
    }

    pub fn contains_preview_point(&self, x: u16, y: u16) -> bool {
        let Some(rect) = self.last_preview_rect else {
            return false;
        };
        x >= rect.x
            && x < rect.x.saturating_add(rect.width)
            && y >= rect.y
            && y < rect.y.saturating_add(rect.height)
    }

    pub fn row_index_at(&self, x: u16, y: u16) -> Option<usize> {
        if self.collapsed {
            return None;
        }
        let rect = self.last_sessions_rect?;
        if x < rect.x
            || x >= rect.x.saturating_add(rect.width)
            || y <= rect.y
            || y >= rect.y.saturating_add(rect.height.saturating_sub(1))
        {
            return None;
        }

        let mut item_index = self.last_list_scroll_offset;
        let mut line_in_view = usize::from(y - rect.y - 1);
        while let Some(&height) = self.last_list_item_heights.get(item_index) {
            let height = height.max(1);
            if line_in_view < height {
                return Some(item_index);
            }
            line_in_view = line_in_view.saturating_sub(height);
            item_index += 1;
        }
        None
    }

    pub fn record_row_click(&mut self, target: SessionListRowTarget, now: Instant) -> bool {
        let SessionListRowTarget::Attachable(target) = target else {
            self.last_attachable_click = None;
            return false;
        };

        let double_click = self
            .last_attachable_click
            .map(|(last_target, last_at)| {
                last_target == target
                    && now.saturating_duration_since(last_at) <= SESSIONS_ROW_DOUBLE_CLICK_WINDOW
            })
            .unwrap_or(false);
        self.last_attachable_click = Some((target, now));
        double_click
    }

    pub fn begin_resize(&mut self, x: u16, y: u16) -> bool {
        if self.is_on_edge(x, y) {
            self.resize_active = true;
            self.edge_hovered = true;
            true
        } else {
            false
        }
    }

    pub fn drag_resize(&mut self, x: u16, terminal_width: u16) {
        if !self.resize_active || self.collapsed {
            return;
        }

        let Some(rect) = self.last_sessions_rect else {
            return;
        };
        let requested = x.saturating_sub(rect.x).saturating_add(1);
        self.preferred_width = Self::clamp_width(requested, terminal_width);
    }

    pub fn finish_resize(&mut self) -> bool {
        let was_active = self.resize_active;
        self.resize_active = false;
        was_active
    }

    pub fn update_hover(&mut self, x: u16, y: u16) {
        self.edge_hovered = self.is_on_edge(x, y);
    }

    pub fn toggle_collapsed(&mut self) {
        self.collapsed = !self.collapsed;
        self.resize_active = false;
        self.edge_hovered = false;
    }
}
/// Renderer-local state for the ratatui host.
///
/// Owned by the run loop next to the `LayoutComponent`, passed to every
/// `Screen::render` as `&mut`, and to the mouse hit test as `&`.
#[derive(Debug, Default)]
pub struct UiState {
    /// Sidebar geometry, hover, resize drag, and the row heights the mouse hit
    /// test resolves a click through.
    pub sessions_pane: SessionsPaneState,
    /// Interior of the live-session embed pane, as last painted. The embed
    /// client is sized from it and mouse input inside it is forwarded to the
    /// PTY, so a stale value sends clicks to the wrong cells.
    pub embed_pane_area: Option<Rect>,
    /// The menu bar's row, as last painted, for click routing.
    pub menu_bar_area: Option<Rect>,
    /// Per-plugin-screen viewport size `(width, height)` from the last render.
    pub plugin_render_areas: HashMap<ScreenId, (u16, u16)>,
    /// Per-plugin-screen origin `(x, y)`, so absolute mouse coordinates can be
    /// translated into the plugin's own space.
    pub plugin_render_origins: HashMap<ScreenId, (u16, u16)>,
    /// The viewport each plugin screen was last rendered AT, so a resize can be
    /// detected and a re-render kicked.
    pub plugin_last_render_viewport: HashMap<ScreenId, (u16, u16)>,
    /// TTL cache behind the status bar's statusline probe: `(value, read_at)`.
    /// Holding `W`, or any rapid keystroke, would otherwise hit the filesystem
    /// once per frame.
    statusline_status_cache: Option<(
        Option<crate::cli::statusline_install::StatuslineStatus>,
        Instant,
    )>,
    /// Size `(rows, cols)` the embed should be resized to, measured off the
    /// pane interior the layout just carved. Only the layout knows that rect,
    /// but the resize is a mutation, so the draw records the want here and the
    /// run loop applies it once the frame is out. Cleared before every draw so
    /// a size from a frame that no longer paints the embed cannot be replayed.
    pub embed_desired_size: Option<(u16, u16)>,
    /// Sidebar rect the HomeScreen last painted, and the welcome panel's
    /// `(content_height, visible_height)`. Both are measurements of what was
    /// drawn, so only a draw can know them; [`crate::components::layout::publish_after_draw`]
    /// hands them back to the components that hit-test and clamp against them.
    pub home_sidebar_rect: Option<Rect>,
    /// Welcome panel `(content_height, visible_height)` from the last paint.
    pub welcome_viewport: (u16, u16),
    /// Log-history log-entry pane rect from the last paint, for text selection.
    pub log_entries_area: Option<Rect>,
    /// Scroll offset of the Session Recovery list. The SELECTION is core's
    /// (`SessionRecoveryState::selected_index`); only the viewport offset the
    /// `List` widget maintains while painting lives here.
    pub session_recovery_list: ListState,
    /// Scroll offset of the Log History session list, mirroring core's
    /// selection for the same reason as [`Self::session_recovery_list`].
    pub log_history_list: ListState,
    /// Scroll intents resolved from the keymap this iteration, drained by the
    /// run loop into [`Self::apply`]. The reducer never sees them: scrolling a
    /// pane is renderer-local by definition.
    queued: Vec<UiAction>,
    /// Set when a `UiAction` changed something the user can see, so the run
    /// loop repaints without waiting for the animation floor.
    pub needs_redraw: bool,
}

impl UiState {
    /// Restore the persisted sidebar preferences into the renderer's copy.
    pub fn restore(&mut self, config: &crate::config::AppConfig) {
        self.sessions_pane.restore(
            config.ui_preferences.sessions_sidebar_width,
            config.ui_preferences.sessions_sidebar_collapsed.unwrap_or(false),
        );
    }

    /// Read the statusline status with a TTL-bounded cache.
    ///
    /// The probe reads `~/.claude/settings.json`; the status bar asks for it
    /// once a frame and the `W` shortcut once a keystroke, so without the TTL
    /// both would pay a filesystem read every time. `state` is taken (and
    /// ignored) so the call site reads as a projection of app state rather than
    /// a free-floating global.
    pub fn statusline_status(
        &mut self,
        _state: &AppState,
    ) -> Option<crate::cli::statusline_install::StatuslineStatus> {
        AppState::statusline_status_cached_inner(
            &mut self.statusline_status_cache,
            std::time::Duration::from_secs(crate::app::state::STATUSLINE_STATUS_CACHE_TTL_SECS),
            Instant::now(),
            crate::cli::statusline_install::detect_statusline_status,
        )
    }

    /// Drop the cached statusline status so the next reader re-detects. Called
    /// after the install event lands so the CTA flips on the very next frame
    /// instead of waiting out the TTL.
    pub fn invalidate_statusline_status(&mut self) {
        self.statusline_status_cache = None;
    }

    /// Record a scroll intent the keymap resolved. Queued rather than applied
    /// on the spot because the key path does not hold the layout.
    pub fn queue(&mut self, action: UiAction) {
        self.queued.push(action);
    }

    /// Take everything queued since the last drain.
    pub fn take_queued(&mut self) -> Vec<UiAction> {
        std::mem::take(&mut self.queued)
    }

    /// Apply one renderer-local scroll intent to the host layout.
    ///
    /// This is the whole reason the nine scroll `AppEvent` variants are gone:
    /// none of them ever reached the reducer with anything to say — they were
    /// routed straight back out to the `LayoutComponent`, which is what this
    /// does, without a round trip through core state.
    pub fn apply(&mut self, action: UiAction, layout: &mut LayoutComponent, state: &AppState) {
        let total_logs = || state.live_logs.values().map(Vec::len).sum::<usize>();
        match action {
            UiAction::ScrollLogsUp => layout.live_logs_mut().scroll_up(),
            UiAction::ScrollLogsDown => layout.live_logs_mut().scroll_down(total_logs()),
            UiAction::ScrollLogsToTop => layout.live_logs_mut().scroll_to_top(),
            UiAction::ScrollLogsToBottom => layout.live_logs_mut().scroll_to_bottom(total_logs()),
            UiAction::ToggleAutoScroll => layout.live_logs_mut().toggle_auto_scroll(),
            // Shift+arrow from the session list both ENTERS scroll mode and
            // moves, so one keypress on a live pane shows scrollback rather
            // than arming a mode the next keypress uses.
            UiAction::ScrollPreviewUp | UiAction::ScrollPreviewDown => {
                let preview = layout.tmux_preview_mut();
                if !preview.is_scroll_mode() {
                    preview.enter_scroll_mode();
                }
                if action == UiAction::ScrollPreviewUp {
                    preview.scroll_up();
                } else {
                    preview.scroll_down();
                }
            }
            // Inside scroll mode the same keys move without re-entering.
            UiAction::PreviewScrollUp => layout.tmux_preview_mut().scroll_up(),
            UiAction::PreviewScrollDown => layout.tmux_preview_mut().scroll_down(),
            UiAction::PreviewPageUp => layout.tmux_preview_mut().scroll_page_up(),
            UiAction::PreviewPageDown => layout.tmux_preview_mut().scroll_page_down(),
            UiAction::PreviewExitScroll => layout.tmux_preview_mut().exit_scroll_mode(),
            // Every other `UiAction` is an app intent the reducer owns; it is
            // translated to an `AppEvent` in `keymap_ui_event` and never
            // reaches here.
            _ => return,
        }
        self.needs_redraw = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::statusline_install::StatuslineStatus;

    #[test]
    fn invalidate_statusline_status_forces_refresh() {
        let mut ui = UiState::default();
        ui.statusline_status_cache = Some((Some(StatuslineStatus::NotConfigured), Instant::now()));

        ui.invalidate_statusline_status();
        assert!(
            ui.statusline_status_cache.is_none(),
            "invalidation must drop the cached entry"
        );
    }
}
