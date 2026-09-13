// ABOUTME: Renderer-agnostic half of the `home_screen_v2` component: its
// state types and the logic that does not draw. The renderer lives in
// `ainb-core::components::home_screen_v2`, which re-exports this module.

use super::mascot::MascotAnimation;
use super::sidebar::{SidebarItem, SidebarState};
use super::welcome_panel::WelcomePanelState;
use std::time::{Duration, Instant};

const SIDEBAR_EDGE_HIT_SLOP: u16 = 1;

/// Window in which two sidebar clicks count as a double-click, from
/// `ui.double_click_ms`. A function rather than a const because the value is a
/// preference now, for the same reason a slow-hands accessibility setting exists.
pub fn sidebar_double_click_window() -> Duration {
    Duration::from_millis(crate::config::tunables::snapshot().ui.double_click_ms)
}

/// Focus area on the home screen
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HomeScreenFocus {
    Sidebar,
    ContentPanel,
}

/// State for the refreshed home screen
#[derive(Debug)]
pub struct HomeScreenV2State {
    /// Current focus (always sidebar for now)
    pub focus: HomeScreenFocus,
    /// Sidebar state
    pub sidebar: SidebarState,
    /// Welcome panel state
    pub welcome: WelcomePanelState,
    /// Mascot animation
    pub mascot: MascotAnimation,
    /// Last sidebar area rendered by HomeScreen V2.
    pub last_sidebar_rect: Option<crate::geometry::Area>,
    /// Whether the mouse is currently over the sidebar resize edge.
    pub sidebar_edge_hovered: bool,
    /// Whether a sidebar resize drag is active.
    pub sidebar_resize_active: bool,
    last_sidebar_click: Option<(usize, Instant)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SidebarClickOutcome {
    pub item: SidebarItem,
    pub double_click: bool,
}

impl HomeScreenV2State {
    pub fn new() -> Self {
        let mut state = Self {
            focus: HomeScreenFocus::Sidebar,
            sidebar: SidebarState::new(),
            welcome: WelcomePanelState::new(),
            mascot: MascotAnimation::new(),
            last_sidebar_rect: None,
            sidebar_edge_hovered: false,
            sidebar_resize_active: false,
            last_sidebar_click: None,
        };
        // Sidebar starts focused
        state.sidebar.is_focused = true;
        state.welcome.is_focused = false;
        state
    }

    /// Toggle focus between sidebar and content panel
    pub fn toggle_focus(&mut self) {
        match self.focus {
            HomeScreenFocus::Sidebar => {
                self.focus = HomeScreenFocus::ContentPanel;
                self.sidebar.is_focused = false;
                self.welcome.is_focused = true;
            }
            HomeScreenFocus::ContentPanel => {
                self.focus = HomeScreenFocus::Sidebar;
                self.sidebar.is_focused = true;
                self.welcome.is_focused = false;
            }
        }
    }

    /// Update mascot animation
    pub fn tick_mascot(&mut self) {
        self.mascot.tick();
    }

    /// Update session count badge
    pub fn set_active_sessions(&mut self, count: usize) {
        self.sidebar.active_sessions_count = count;
    }

    pub fn restore_sidebar_width(&mut self, width: Option<u16>) {
        if let Some(width) = width {
            self.sidebar.preferred_width = width.max(super::sidebar::MIN_SIDEBAR_WIDTH);
        }
    }

    pub fn rendered_sidebar_width(&self) -> Option<u16> {
        self.last_sidebar_rect.map(|rect| rect.width)
    }

    pub fn sidebar_edge_highlighted(&self) -> bool {
        self.sidebar_edge_hovered || self.sidebar_resize_active
    }

    pub fn update_sidebar_edge_hover(&mut self, x: u16, y: u16) {
        self.sidebar_edge_hovered = self.is_on_sidebar_edge(x, y);
    }

    pub fn is_on_sidebar_edge(&self, x: u16, y: u16) -> bool {
        let Some(rect) = self.last_sidebar_rect else {
            return false;
        };
        if y < rect.y || y >= rect.y.saturating_add(rect.height) || rect.width == 0 {
            return false;
        }

        let edge_x = rect.x.saturating_add(rect.width.saturating_sub(1));
        x.abs_diff(edge_x) <= SIDEBAR_EDGE_HIT_SLOP
    }

    pub fn begin_sidebar_resize(&mut self, x: u16, y: u16) -> bool {
        let on_edge = self.is_on_sidebar_edge(x, y);
        self.sidebar_resize_active = on_edge;
        self.sidebar_edge_hovered = on_edge;
        on_edge
    }

    pub fn drag_sidebar_resize(&mut self, x: u16, terminal_width: u16) -> bool {
        if !self.sidebar_resize_active {
            return false;
        }
        let Some(rect) = self.last_sidebar_rect else {
            return false;
        };

        let requested_width = x.saturating_sub(rect.x).saturating_add(1);
        self.sidebar.set_preferred_width(requested_width, terminal_width);
        true
    }

    pub fn finish_sidebar_resize(&mut self) -> bool {
        let was_active = self.sidebar_resize_active;
        self.sidebar_resize_active = false;
        was_active
    }

    pub fn click_sidebar_item_at(
        &mut self,
        x: u16,
        y: u16,
        now: Instant,
    ) -> Option<SidebarClickOutcome> {
        let rect = self.last_sidebar_rect?;
        if !rect.contains(x, y) || self.is_on_sidebar_edge(x, y) {
            return None;
        }

        let item_index = super::sidebar::item_index_at(rect, y, self.sidebar.selected_index)?;
        self.sidebar.select_index(item_index);
        self.focus = HomeScreenFocus::Sidebar;
        self.sidebar.is_focused = true;
        self.welcome.is_focused = false;

        let double_click = self
            .last_sidebar_click
            .map(|(last_index, last_at)| {
                last_index == item_index
                    && now.saturating_duration_since(last_at) <= sidebar_double_click_window()
            })
            .unwrap_or(false);
        self.last_sidebar_click = Some((item_index, now));

        Some(SidebarClickOutcome {
            item: self.sidebar.selected_item(),
            double_click,
        })
    }
}

impl Default for HomeScreenV2State {
    fn default() -> Self {
        Self::new()
    }
}
