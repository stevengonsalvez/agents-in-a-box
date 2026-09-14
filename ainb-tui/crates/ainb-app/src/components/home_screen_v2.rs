// ABOUTME: Renderer-agnostic half of the `home_screen_v2` component: its
// state types and the logic that does not draw. The renderer lives in
// `ainb-core::components::home_screen_v2`, which re-exports this module.

use super::mascot::MascotAnimation;
use super::sidebar::{SidebarItem, SidebarState};
use super::welcome_panel::WelcomePanelState;
use std::time::{Duration, Instant};

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

    /// Select and focus sidebar item `item_index`, reporting whether this
    /// click and the last one on the same item make a double-click.
    pub fn click_sidebar_item(&mut self, item_index: usize, now: Instant) -> SidebarClickOutcome {
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

        SidebarClickOutcome {
            item: self.sidebar.selected_item(),
            double_click,
        }
    }
}

impl Default for HomeScreenV2State {
    fn default() -> Self {
        Self::new()
    }
}
