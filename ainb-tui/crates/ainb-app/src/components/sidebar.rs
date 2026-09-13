// ABOUTME: Renderer-agnostic half of the `sidebar` component: its
// state types and the logic that does not draw. The renderer lives in
// `ainb-core::components::sidebar`, which re-exports this module.

use crate::geometry::Area;

pub const DEFAULT_SIDEBAR_WIDTH: u16 = 26;

pub const MIN_SIDEBAR_WIDTH: u16 = 16;

pub const SIDEBAR_CONTENT_RESERVE: u16 = 50;

/// Sidebar navigation items - matches HomeTile options
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidebarItem {
    Config,       // Settings & presets
    Sessions,     // Session manager
    Daemons,      // Unified runtime-health and repair screen
    Recovery,     // Recover orphaned sessions
    Mcp,          // Shared MCP pool overlay
    Logs,         // Log history viewer
    Stats,        // Analytics & usage
    Witr,         // Process causality (witr plugin)
    Abtop,        // top-for-agents — live agent monitor (abtop plugin)
    Skills,       // Browse per-agent skills
    SkillManager, // Skill / unit manager (spec §10.1)
    Hangar,       // Autopilot control plane (hangar-tui plugin)
    Memory,       // Knowledge-base browser (learnings plugin)
    Changelog,    // Version history
    Setup,        // Setup wizard & factory reset
    Help,         // Docs & guides
}

impl SidebarItem {
    /// Get the display icon for this item (emoji)
    pub fn icon(&self) -> &'static str {
        match self {
            Self::Config => "⚙️",
            Self::Sessions => "🚀",
            Self::Daemons => "⚙",
            Self::Recovery => "🔄",
            Self::Mcp => "🧬",
            Self::Logs => "📋",
            Self::Stats => "📊",
            Self::Witr => "🌳",
            Self::Abtop => "📡",
            Self::Skills => "🧠",
            Self::SkillManager => "🧰",
            Self::Hangar => "🛩️",
            Self::Memory => "📚",
            Self::Changelog => "📝",
            Self::Setup => "🛠️",
            Self::Help => "❓",
        }
    }

    /// Get the display label for this item
    pub fn label(&self) -> &'static str {
        match self {
            Self::Config => "Config",
            Self::Sessions => "Sessions",
            Self::Daemons => "Daemons",
            Self::Recovery => "Recovery",
            Self::Mcp => "MCP",
            Self::Logs => "Logs",
            Self::Stats => "Stats",
            Self::Witr => "Witr",
            Self::Abtop => "abtop",
            Self::Skills => "Skills Catalogue",
            Self::SkillManager => "Skills (manager)",
            Self::Hangar => "Hangar",
            Self::Memory => "Memory",
            Self::Changelog => "Changelog",
            Self::Setup => "Setup",
            Self::Help => "Help",
        }
    }

    /// Get the description for this item
    pub fn description(&self) -> &'static str {
        match self {
            Self::Config => "Settings & Presets",
            Self::Sessions => "Manage Active",
            Self::Daemons => "Runtime health & repair",
            Self::Recovery => "Resume Orphaned",
            Self::Mcp => "Shared Pool",
            Self::Logs => "View Log History",
            Self::Stats => "Usage & Analytics",
            Self::Witr => "Process Causality",
            Self::Abtop => "top-for-agents",
            Self::Skills => "Per-Agent Skills",
            Self::SkillManager => "Install / sync / doctor",
            Self::Hangar => "Autopilot Control Plane",
            Self::Memory => "Knowledge & Recall",
            Self::Changelog => "Version History",
            Self::Setup => "Setup & Reset",
            Self::Help => "Docs & Guides",
        }
    }

    /// Get the keyboard shortcut for this item
    pub fn shortcut(&self) -> &'static str {
        // All lowercase and distinct - no case pairs (order in `all()`).
        match self {
            Self::Sessions => "s",
            Self::Setup => "u",
            Self::Config => "o",
            Self::Skills => "c",
            Self::SkillManager => "z",
            Self::Memory => "m",
            Self::Stats => "i",
            Self::Daemons => "d",
            Self::Witr => "w",
            Self::Abtop => "t",
            Self::Mcp => "p",
            Self::Logs => "l",
            Self::Recovery => "r",
            Self::Hangar => "g",
            Self::Changelog => "v",
            Self::Help => "?",
        }
    }

    /// Get all items in order
    pub fn all() -> &'static [SidebarItem] {
        // UX order: primary work first, then skills/knowledge, insight,
        // Agent Deck, observability, maintenance; Help last. Keys in
        // `shortcut()`.
        &[
            Self::Sessions,
            Self::Setup,
            Self::Config,
            Self::Skills,
            Self::SkillManager,
            Self::Memory,
            Self::Stats,
            Self::Daemons,
            Self::Witr,
            Self::Abtop,
            Self::Mcp,
            Self::Logs,
            Self::Recovery,
            Self::Hangar,
            Self::Changelog,
            Self::Help,
        ]
    }
}

/// Sidebar state
#[derive(Debug)]
pub struct SidebarState {
    /// Currently selected item index
    pub selected_index: usize,
    /// Whether the sidebar is focused
    pub is_focused: bool,
    /// Whether to show labels (false = icon-only mode)
    pub show_labels: bool,
    /// Active sessions count (for badge display)
    pub active_sessions_count: usize,
    /// Preferred sidebar width, clamped against the current terminal width at render time.
    pub preferred_width: u16,
}

impl SidebarState {
    pub fn new() -> Self {
        Self {
            selected_index: 0,
            is_focused: true,
            show_labels: true,
            active_sessions_count: 0,
            preferred_width: DEFAULT_SIDEBAR_WIDTH,
        }
    }

    /// Get the currently selected item
    pub fn selected_item(&self) -> SidebarItem {
        SidebarItem::all()[self.selected_index]
    }

    /// Move selection up
    pub fn move_up(&mut self) {
        if self.selected_index > 0 {
            self.selected_index -= 1;
        }
    }

    /// Move selection down
    pub fn move_down(&mut self) {
        let max_index = SidebarItem::all().len() - 1;
        if self.selected_index < max_index {
            self.selected_index += 1;
        }
    }

    /// Set selection to a specific item
    pub fn select(&mut self, item: SidebarItem) {
        if let Some(index) = SidebarItem::all().iter().position(|&i| i == item) {
            self.selected_index = index;
        }
    }

    pub fn select_index(&mut self, index: usize) {
        if index < SidebarItem::all().len() {
            self.selected_index = index;
        }
    }

    pub fn set_preferred_width(&mut self, width: u16, terminal_width: u16) {
        self.preferred_width = Self::clamp_width(width, terminal_width);
    }

    pub fn effective_width(&self, terminal_width: u16) -> u16 {
        Self::clamp_width(self.preferred_width, terminal_width)
    }

    pub fn clamp_width(width: u16, terminal_width: u16) -> u16 {
        if terminal_width == 0 {
            return 0;
        }

        let max_with_content = terminal_width.saturating_sub(SIDEBAR_CONTENT_RESERVE);
        let max_with_divider = terminal_width.saturating_sub(1).max(1);
        let effective_max = max_with_content.max(MIN_SIDEBAR_WIDTH).min(max_with_divider);
        let effective_min = MIN_SIDEBAR_WIDTH.min(effective_max);

        width.clamp(effective_min, effective_max)
    }
}

impl Default for SidebarState {
    fn default() -> Self {
        Self::new()
    }
}

/// Map a click row to a sidebar item. Row heights are variable (the
/// selected item is 2 rows, the rest 1), so the selected index is needed
/// to walk the rows correctly. `area` is where the renderer last drew the
/// sidebar; the row layout here mirrors its title, spacer and item rows.
pub fn item_index_at(area: Area, y: u16, selected_index: usize) -> Option<usize> {
    let first_item_y = area.y.saturating_add(3); // title(2) + spacer(1)
    if y < first_item_y {
        return None;
    }

    let mut row = first_item_y;
    for idx in 0..SidebarItem::all().len() {
        let height = if idx == selected_index { 2 } else { 1 };
        if y >= row && y < row.saturating_add(height) {
            return Some(idx);
        }
        row = row.saturating_add(height);
    }
    None
}
