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

use crate::app::keymap::{HostAction, ScrollAction};
use crate::app::screens::ScreenId;
use crate::app::state::{
    AppState, AttachableRef, COLLAPSED_SESSIONS_SIDEBAR_WIDTH, DEFAULT_SESSIONS_SIDEBAR_WIDTH,
    MIN_SESSIONS_SIDEBAR_WIDTH, SESSIONS_ROW_DOUBLE_CLICK_WINDOW, SessionListRowTarget,
};
use crate::components::layout::LayoutComponent;

#[derive(Debug, Clone)]
pub struct SessionsPaneState {
    /// The sidebar width in columns set on this surface, by a drag or a
    /// legacy column count. `None` until then, so the saved fraction applies.
    width: Option<u16>,
    /// The saved preference: the sidebar's share of its row.
    saved_fraction: Option<f64>,
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
    /// Each tab label of the right pane's strip, as last painted. Empty while
    /// no strip is drawn, such as under the interactive embed.
    tab_strip: Vec<(crate::components::session_tabs::SessionTab, Rect)>,
}

impl Default for SessionsPaneState {
    fn default() -> Self {
        Self {
            width: None,
            saved_fraction: None,
            collapsed: false,
            resize_active: false,
            edge_hovered: false,
            last_sessions_rect: None,
            last_preview_rect: None,
            last_list_scroll_offset: 0,
            last_list_item_heights: Vec::new(),
            last_attachable_click: None,
            filter_toggle_area: None,
            tab_strip: Vec::new(),
        }
    }
}

impl SessionsPaneState {
    /// Restore the saved layout: a `fraction` of the row, else a `legacy`
    /// column count an older ainb saved (converted on launch), and the
    /// collapsed flag.
    pub fn restore(&mut self, fraction: Option<f64>, legacy: Option<u16>, collapsed: bool) {
        self.saved_fraction = fraction;
        if fraction.is_none() {
            self.width = legacy.map(|width| width.max(MIN_SESSIONS_SIDEBAR_WIDTH));
        }
        self.collapsed = collapsed;
    }

    /// The expanded width in columns before clamping, on a `row`-wide
    /// screen: this surface's, else the saved fraction, else the default.
    #[must_use]
    pub fn preferred_width(&self, row: u16) -> u16 {
        self.width
            .or_else(|| {
                self.saved_fraction.map(|fraction| (fraction * f64::from(row)).round() as u16)
            })
            .unwrap_or(DEFAULT_SESSIONS_SIDEBAR_WIDTH)
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

    /// Publish where the tab strip's labels were painted this frame; an empty
    /// list when no strip was drawn.
    pub fn set_tab_strip(
        &mut self,
        hits: Vec<(crate::components::session_tabs::SessionTab, Rect)>,
    ) {
        self.tab_strip = hits;
    }

    /// The tab whose strip label covers (`x`, `y`), if any.
    #[must_use]
    pub fn tab_at(&self, x: u16, y: u16) -> Option<crate::components::session_tabs::SessionTab> {
        crate::components::session_tabs::tab_at(&self.tab_strip, x, y)
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

        self.expanded_width(terminal_width)
    }

    pub fn expanded_width(&self, terminal_width: u16) -> u16 {
        crate::app::state::clamp_sessions_sidebar_width(
            self.preferred_width(terminal_width),
            terminal_width,
        )
    }

    /// The intent that saves this layout: the width the user asked for as a
    /// fraction of a `row`-wide screen, unclamped, so a narrow surface cannot
    /// shrink the preference every surface draws from. The clamp is applied
    /// only when drawing. `None` for a row with no width yet.
    #[must_use]
    pub fn save_layout(&self, row: u16) -> Option<crate::app::Intent> {
        if row == 0 {
            return None;
        }
        let fraction = match (self.width, self.saved_fraction) {
            (Some(width), _) => f64::from(width) / f64::from(row),
            (None, Some(fraction)) => fraction,
            (None, None) => f64::from(DEFAULT_SESSIONS_SIDEBAR_WIDTH) / f64::from(row),
        };
        Some(crate::app::pointer::save_sessions_pane_layout(
            fraction,
            self.collapsed,
        ))
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
        self.width = Some(requested.min(terminal_width));
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
/// A panel whose width the user drags or steps: the home sidebar, the Skill
/// Manager's Sources panel.
///
/// Renderer-local because a width only means something against one surface:
/// a step taken on a narrow terminal must not shrink the panel another
/// surface draws from the same state. The saved preference is a fraction of
/// the screen, so every surface starts from the same proportion.
#[derive(Debug, Default, Clone)]
pub struct ResizablePane {
    /// The width in columns the user set on this surface, before clamping.
    /// `None` until they resize here, so the saved preference applies.
    width: Option<u16>,
    /// A divider drag is in flight: the edge draws bright and drags move it.
    pub resize_active: bool,
    /// The pointer is over the resize edge.
    pub edge_hovered: bool,
}

impl ResizablePane {
    /// The width in columns before clamping, on a `columns`-wide screen: this
    /// surface's, else the `saved` fraction of the screen, else `default`.
    #[must_use]
    pub fn preferred_width(&self, saved: Option<f64>, columns: u16, default: u16) -> u16 {
        self.width
            .or_else(|| saved.map(|fraction| (fraction * f64::from(columns)).round() as u16))
            .unwrap_or(default)
    }

    pub const fn set_width(&mut self, width: u16) {
        self.width = Some(width);
    }

    /// Whether the edge draws highlighted.
    #[must_use]
    pub const fn edge_highlighted(&self) -> bool {
        self.edge_hovered || self.resize_active
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
    /// The Skill Manager's Sources panel width and divider drag on this
    /// surface.
    pub skill_sources: ResizablePane,
    /// The home sidebar's width, divider drag and edge hover on this surface.
    pub home_sidebar: ResizablePane,
    /// Interior of the live-session embed pane, as last painted. The embed
    /// client is sized from it and mouse input inside it is forwarded to the
    /// PTY, so a stale value sends clicks to the wrong cells.
    pub embed_pane_area: Option<Rect>,
    /// The menu bar's row, as last painted, for click routing.
    pub menu_bar_area: Option<Rect>,
    /// Per-plugin-screen allocated size from the last render, and the viewport
    /// each plugin was last asked to render at. The plugin render tick reads
    /// and updates both.
    pub plugin_viewports: crate::app::screens::PluginViewports,
    /// Per-plugin-screen origin `(x, y)`, so absolute mouse coordinates can be
    /// translated into the plugin's own space.
    pub plugin_render_origins: HashMap<ScreenId, (u16, u16)>,
    /// Size `(rows, cols)` the embed should be resized to, measured off the
    /// pane interior the layout just carved. Only the layout knows that rect,
    /// but the resize is a mutation, so the draw records the want here and the
    /// run loop applies it once the frame is out. Cleared before every draw so
    /// a size from a frame that no longer paints the embed cannot be replayed.
    pub embed_desired_size: Option<(u16, u16)>,

    /// The size the embed was last resized to.
    ///
    /// Renderer-local so the resize can be skipped when the layout recomputes
    /// the same size, which it does on almost every frame. Without it
    /// `publish_after_draw` takes `&mut` on the tmux section every single
    /// frame and that section then reads as "changed" forever.
    pub last_embed_size: Option<(u16, u16)>,
    /// Sidebar rect the HomeScreen last painted, and the welcome panel's
    /// `(content_height, visible_height)`. Both are measurements of what was
    /// drawn, so only a draw can know them; [`crate::components::layout::publish_after_draw`]
    /// hands them back to the components that hit-test and clamp against them.
    pub home_sidebar_rect: Option<Rect>,
    /// Welcome panel `(content_height, visible_height)` from the last paint.
    pub welcome_viewport: (u16, u16),
    /// Where the code review sidebar tree was last drawn, for its click
    /// hit-test.
    pub review_sidebar: crate::components::code_review::render::ReviewSidebarLayout,
    /// Log-history log-entry pane rect from the last paint, for text selection.
    pub log_entries_area: Option<Rect>,
    /// Scroll offset of the Session Recovery list. The SELECTION is core's
    /// (`SessionRecoveryState::selected_index`); only the viewport offset the
    /// `List` widget maintains while painting lives here.
    pub session_recovery_list: ListState,
    /// Scroll offset of the Log History session list, mirroring core's
    /// selection for the same reason as [`Self::session_recovery_list`].
    pub log_history_list: ListState,
    /// First changelog line drawn. The lines are static content
    /// (`changelog_lines`), so this offset is all the viewer keeps, and it is
    /// this renderer's alone: another renderer scrolls its own copy (#1052).
    pub changelog_scroll: usize,
    /// Layout work resolved from the keymap this iteration, drained by the run
    /// loop into [`Self::apply_host`]. The reducer never sees it: scrolling a
    /// pane or collapsing the sidebar is renderer-local by definition.
    queued: Vec<HostAction>,
    /// Set when a `UiAction` changed something the user can see, so the run
    /// loop repaints without waiting for the animation floor.
    pub needs_redraw: bool,
}

impl UiState {
    /// Restore the persisted sidebar preferences into the renderer's copy.
    pub fn restore(&mut self, config: &crate::config::AppConfig) {
        self.sessions_pane.restore(
            config.ui_preferences.sessions_sidebar_fraction,
            config.ui_preferences.sessions_sidebar_width,
            config.ui_preferences.sessions_sidebar_collapsed.unwrap_or(false),
        );
    }

    /// Record layout work the keymap resolved. Queued rather than applied on
    /// the spot because the key path does not hold the layout.
    pub fn queue(&mut self, action: HostAction) {
        self.queued.push(action);
    }

    /// Take everything queued since the last drain.
    pub fn take_queued(&mut self) -> Vec<HostAction> {
        std::mem::take(&mut self.queued)
    }

    /// Apply queued layout work on a surface `columns` wide. Returns the
    /// intent that persists it, when the change is a preference the user
    /// keeps across launches.
    pub fn apply_host(
        &mut self,
        action: HostAction,
        layout: &mut LayoutComponent,
        state: &AppState,
        columns: u16,
    ) -> Option<crate::app::Intent> {
        match action {
            HostAction::Scroll(scroll) => {
                self.apply(scroll, layout, state);
                None
            }
            HostAction::ToggleSessionsSidebar => {
                self.sessions_pane.toggle_collapsed();
                self.needs_redraw = true;
                let row = self.sessions_pane.last_content_width().unwrap_or(columns);
                self.sessions_pane.save_layout(row)
            }
            HostAction::GrowSkillSources | HostAction::ShrinkSkillSources => {
                use crate::components::skill_manager_screen::{
                    DEFAULT_SOURCES_WIDTH, step_sources_width,
                };
                let saved = state.config.app_config.ui_preferences.skill_manager_sources_fraction;
                let width = step_sources_width(
                    self.skill_sources.preferred_width(saved, columns, DEFAULT_SOURCES_WIDTH),
                    action == HostAction::GrowSkillSources,
                    columns,
                );
                self.skill_sources.set_width(width);
                self.needs_redraw = true;
                Some(crate::app::pointer::save_skill_sources_width(
                    width, columns,
                ))
            }
        }
    }

    /// Apply one renderer-local scroll intent to the host layout.
    ///
    /// This is the whole reason the nine scroll `AppEvent` variants are gone:
    /// none of them ever reached the reducer with anything to say — they were
    /// routed straight back out to the `LayoutComponent`, which is what this
    /// does, without a round trip through core state.
    pub fn apply(&mut self, action: ScrollAction, layout: &mut LayoutComponent, state: &AppState) {
        let total_logs = || state.log_streams.live_logs.values().map(Vec::len).sum::<usize>();
        match action {
            ScrollAction::ScrollLogsUp => layout.live_logs_mut().scroll_up(),
            ScrollAction::ScrollLogsDown => layout.live_logs_mut().scroll_down(total_logs()),
            ScrollAction::ScrollLogsToTop => layout.live_logs_mut().scroll_to_top(),
            ScrollAction::ScrollLogsToBottom => {
                layout.live_logs_mut().scroll_to_bottom(total_logs())
            }
            ScrollAction::ToggleAutoScroll => layout.live_logs_mut().toggle_auto_scroll(),
            // Shift+arrow from the session list both ENTERS scroll mode and
            // moves, so one keypress on a live pane shows scrollback rather
            // than arming a mode the next keypress uses.
            ScrollAction::ScrollPreviewUp | ScrollAction::ScrollPreviewDown => {
                let preview = layout.tmux_preview_mut();
                if !preview.is_scroll_mode() {
                    preview.enter_scroll_mode();
                }
                if action == ScrollAction::ScrollPreviewUp {
                    preview.scroll_up();
                } else {
                    preview.scroll_down();
                }
            }
            // Inside scroll mode the same keys move without re-entering.
            ScrollAction::PreviewScrollUp => layout.tmux_preview_mut().scroll_up(),
            ScrollAction::PreviewScrollDown => layout.tmux_preview_mut().scroll_down(),
            ScrollAction::PreviewPageUp => layout.tmux_preview_mut().scroll_page_up(),
            ScrollAction::PreviewPageDown => layout.tmux_preview_mut().scroll_page_down(),
            ScrollAction::PreviewExitScroll => layout.tmux_preview_mut().exit_scroll_mode(),
            ScrollAction::ChangelogUp
            | ScrollAction::ChangelogDown
            | ScrollAction::ChangelogPageUp
            | ScrollAction::ChangelogPageDown
            | ScrollAction::ChangelogToTop
            | ScrollAction::ChangelogToBottom => {
                self.changelog_scroll = changelog_scroll(action, self.changelog_scroll);
            }
        }
        self.needs_redraw = true;
    }
}

/// The changelog offset after `action`. A page is the 30 lines the viewer
/// always paged by; the last page stops at the final line, and the renderer
/// clamps whatever height it actually has.
fn changelog_scroll(action: ScrollAction, offset: usize) -> usize {
    const PAGE: usize = 30;
    let last = crate::components::changelog::changelog_lines().len().saturating_sub(PAGE);
    match action {
        ScrollAction::ChangelogUp => offset.saturating_sub(1),
        ScrollAction::ChangelogDown => (offset + 1).min(last),
        ScrollAction::ChangelogPageUp => offset.saturating_sub(PAGE),
        ScrollAction::ChangelogPageDown => (offset + PAGE).min(last),
        ScrollAction::ChangelogToTop => 0,
        ScrollAction::ChangelogToBottom => last,
        _ => offset,
    }
}

impl crate::app::state::SessionsPaneHitTest for SessionsPaneState {
    fn row_index_at(&self, x: u16, y: u16) -> Option<usize> {
        Self::row_index_at(self, x, y)
    }

    fn contains_preview_point(&self, x: u16, y: u16) -> bool {
        Self::contains_preview_point(self, x, y)
    }

    fn contains_sessions_point(&self, x: u16, y: u16) -> bool {
        Self::contains_sessions_point(self, x, y)
    }

    fn is_collapsed(&self) -> bool {
        self.collapsed
    }
}

/// The terminal host's side of intent dispatch: layout work is queued for the
/// run loop to apply, and pointer presses are hit-tested
/// against the panes this renderer last drew.
impl crate::app::RendererHost for UiState {
    fn queue(&mut self, action: HostAction) {
        Self::queue(self, action);
    }

    fn pointer(
        &mut self,
        state: &crate::app::AppState,
        pos: crate::app::Pos,
        btn: crate::app::Btn,
    ) -> Option<crate::app::Intent> {
        crate::app::mouse::press(state, self, pos, btn)
    }
}
