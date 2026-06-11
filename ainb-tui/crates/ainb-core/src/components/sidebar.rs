// ABOUTME: Premium sidebar navigation component for AINB home screen
// Inspired by VS Code, Discord, and Slack sidebar patterns with enhanced selection styling

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Paragraph},
};

// Color palette from TUI style guide
const CORNFLOWER_BLUE: Color = Color::Rgb(100, 149, 237);
const GOLD: Color = Color::Rgb(255, 215, 0);
const SELECTION_GREEN: Color = Color::Rgb(100, 200, 100);
const DARK_BG: Color = Color::Rgb(25, 25, 35);
const LIST_HIGHLIGHT_BG: Color = Color::Rgb(40, 40, 60);
const SOFT_WHITE: Color = Color::Rgb(220, 220, 230);
const MUTED_GRAY: Color = Color::Rgb(120, 120, 140);
const SUBDUED_BORDER: Color = Color::Rgb(60, 60, 80);

// Premium selection colors
const ACCENT_CYAN: Color = Color::Rgb(80, 200, 220);
const SELECTION_BG: Color = Color::Rgb(45, 55, 75);
const HOVER_BG: Color = Color::Rgb(35, 40, 55);

pub const DEFAULT_SIDEBAR_WIDTH: u16 = 26;
pub const MIN_SIDEBAR_WIDTH: u16 = 16;
pub const SIDEBAR_CONTENT_RESERVE: u16 = 50;

/// Sidebar navigation items - matches HomeTile options
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidebarItem {
    Agents,    // Agent selection
    Catalog,   // Browse catalog/marketplace
    Config,    // Settings & presets
    Sessions,  // Session manager
    Inbox,     // ainb-hooks notification inbox
    Recovery,  // Recover orphaned sessions
    Logs,      // Log history viewer
    Stats,     // Analytics & usage
    Witr,      // Process causality (witr plugin)
    Abtop,     // top-for-agents — live agent monitor (abtop plugin)
    Skills,    // Browse per-agent skills
    Memory,    // Knowledge-base browser (learnings plugin)
    Changelog, // Version history
    Setup,     // Setup wizard & factory reset
    Help,      // Docs & guides
}

impl SidebarItem {
    /// Get the display icon for this item (emoji)
    pub fn icon(&self) -> &'static str {
        match self {
            Self::Agents => "🤖",
            Self::Catalog => "📦",
            Self::Config => "⚙️",
            Self::Sessions => "🚀",
            Self::Inbox => "📥",
            Self::Recovery => "🔄",
            Self::Logs => "📋",
            Self::Stats => "📊",
            Self::Witr => "🌳",
            Self::Abtop => "📡",
            Self::Skills => "🧠",
            Self::Memory => "📚",
            Self::Changelog => "📝",
            Self::Setup => "🛠️",
            Self::Help => "❓",
        }
    }

    /// Get the display label for this item
    pub fn label(&self) -> &'static str {
        match self {
            Self::Agents => "Agents",
            Self::Catalog => "Catalog",
            Self::Config => "Config",
            Self::Sessions => "Sessions",
            Self::Inbox => "Inbox",
            Self::Recovery => "Recovery",
            Self::Logs => "Logs",
            Self::Stats => "Stats",
            Self::Witr => "Witr",
            Self::Abtop => "abtop",
            Self::Skills => "Skills",
            Self::Memory => "Memory",
            Self::Changelog => "Changelog",
            Self::Setup => "Setup",
            Self::Help => "Help",
        }
    }

    /// Get the description for this item
    pub fn description(&self) -> &'static str {
        match self {
            Self::Agents => "Select & Configure",
            Self::Catalog => "Browse & Bootstrap",
            Self::Config => "Settings & Presets",
            Self::Sessions => "Manage Active",
            Self::Inbox => "Hook Notifications",
            Self::Recovery => "Resume Orphaned",
            Self::Logs => "View Log History",
            Self::Stats => "Usage & Analytics",
            Self::Witr => "Process Causality",
            Self::Abtop => "top-for-agents",
            Self::Skills => "Per-Agent Skills",
            Self::Memory => "Knowledge & Recall",
            Self::Changelog => "Version History",
            Self::Setup => "Setup & Reset",
            Self::Help => "Docs & Guides",
        }
    }

    /// Get the keyboard shortcut for this item
    pub fn shortcut(&self) -> &'static str {
        match self {
            Self::Agents => "a",
            Self::Catalog => "c",
            Self::Config => "C",
            Self::Sessions => "s",
            Self::Inbox => "b",
            Self::Recovery => "R",
            Self::Logs => "l",
            Self::Stats => "i",
            Self::Witr => "w",
            Self::Abtop => "t",
            Self::Skills => "k",
            Self::Memory => "m",
            Self::Changelog => "v",
            Self::Setup => "S",
            Self::Help => "?",
        }
    }

    /// Get all items in order
    pub fn all() -> &'static [SidebarItem] {
        &[
            Self::Agents,
            Self::Catalog,
            Self::Config,
            Self::Sessions,
            Self::Inbox,
            Self::Recovery,
            Self::Logs,
            Self::Stats,
            Self::Witr,
            Self::Abtop,
            Self::Skills,
            Self::Memory,
            Self::Changelog,
            Self::Setup,
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

/// Premium sidebar component for rendering
pub struct SidebarComponent;

impl SidebarComponent {
    pub fn new() -> Self {
        Self
    }

    /// Render the sidebar with premium styling
    pub fn render(&self, frame: &mut Frame, area: Rect, state: &SidebarState) {
        self.render_with_edge_highlight(frame, area, state, false);
    }

    pub fn render_with_edge_highlight(
        &self,
        frame: &mut Frame,
        area: Rect,
        state: &SidebarState,
        edge_highlighted: bool,
    ) {
        // Outer block with subtle border
        let border_color = if edge_highlighted {
            GOLD
        } else if state.is_focused {
            CORNFLOWER_BLUE
        } else {
            SUBDUED_BORDER
        };

        let block = Block::default()
            .borders(Borders::RIGHT)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(border_color))
            .style(Style::default().bg(DARK_BG));

        let inner = block.inner(area);
        frame.render_widget(block, area);

        // Layout: title + spacer + one row per SidebarItem + flexible space.
        // Constraints are derived from SidebarItem::all() so adding a new
        // entry to the enum automatically reserves a layout slot (instead of
        // silently rendering into a zero-height row).
        let items = SidebarItem::all();
        let mut constraints: Vec<Constraint> = Vec::with_capacity(items.len() + 3);
        constraints.push(Constraint::Length(2)); // Title area
        constraints.push(Constraint::Length(1)); // Spacer
        constraints.extend(items.iter().map(|_| Constraint::Length(2)));
        constraints.push(Constraint::Min(0)); // Flexible space

        let layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints(constraints)
            .split(inner);

        // Render title
        self.render_title(frame, layout[0], state);

        // Render all items with premium styling
        for (idx, item) in items.iter().enumerate() {
            let is_selected = state.selected_index == idx;
            let badge = if *item == SidebarItem::Sessions && state.active_sessions_count > 0 {
                Some(state.active_sessions_count)
            } else {
                None
            };
            self.render_premium_item(frame, layout[idx + 2], item, is_selected, state, badge);
        }
    }

    pub fn item_index_at(area: Rect, y: u16) -> Option<usize> {
        let inner_y = area.y;
        let first_item_y = inner_y.saturating_add(3);
        if y < first_item_y {
            return None;
        }

        let relative = y - first_item_y;
        let item_index = (relative / 2) as usize;
        if relative % 2 <= 1 && item_index < SidebarItem::all().len() {
            Some(item_index)
        } else {
            None
        }
    }

    /// Render the sidebar title
    fn render_title(&self, frame: &mut Frame, area: Rect, state: &SidebarState) {
        let title_style = if state.is_focused {
            Style::default().fg(GOLD).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(MUTED_GRAY)
        };

        let title = Paragraph::new(Line::from(vec![
            Span::styled("  ", Style::default()),
            Span::styled("◆", Style::default().fg(ACCENT_CYAN)),
            Span::styled(" AINB", title_style),
        ]))
        .style(Style::default().bg(DARK_BG));

        frame.render_widget(title, area);
    }

    /// Render a single item with premium selection styling
    fn render_premium_item(
        &self,
        frame: &mut Frame,
        area: Rect,
        item: &SidebarItem,
        is_selected: bool,
        state: &SidebarState,
        badge: Option<usize>,
    ) {
        // Premium selection styling
        let (accent_bar, icon_style, label_style, shortcut_style, bg_color) =
            if is_selected && state.is_focused {
                // Selected + focused: full accent bar, bright colors
                (
                    "█",
                    Style::default().fg(GOLD),
                    Style::default().fg(SOFT_WHITE).add_modifier(Modifier::BOLD),
                    Style::default().fg(ACCENT_CYAN).add_modifier(Modifier::BOLD),
                    SELECTION_BG,
                )
            } else if is_selected {
                // Selected but not focused: dimmer accent
                (
                    "▐",
                    Style::default().fg(GOLD),
                    Style::default().fg(SOFT_WHITE),
                    Style::default().fg(MUTED_GRAY),
                    HOVER_BG,
                )
            } else {
                // Not selected: no accent bar
                (
                    " ",
                    Style::default().fg(MUTED_GRAY),
                    Style::default().fg(MUTED_GRAY),
                    Style::default().fg(SUBDUED_BORDER),
                    DARK_BG,
                )
            };

        // Split the item area for 2-line content (compact to fit 10 items)
        let item_layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1), // Main line (icon + label + shortcut)
                Constraint::Length(1), // Description line (when selected)
            ])
            .split(area);

        // Main line: accent bar + icon + label + shortcut
        let accent_style = if is_selected && state.is_focused {
            Style::default().fg(ACCENT_CYAN)
        } else if is_selected {
            Style::default().fg(CORNFLOWER_BLUE)
        } else {
            Style::default().fg(DARK_BG)
        };

        let mut main_spans = vec![
            Span::styled(accent_bar, accent_style),
            Span::styled(" ", Style::default()),
            Span::styled(item.icon(), icon_style),
        ];

        if state.show_labels {
            main_spans.push(Span::styled("  ", Style::default()));
            main_spans.push(Span::styled(item.label(), label_style));

            // Add badge if present
            if let Some(count) = badge {
                main_spans.push(Span::styled(" ", Style::default()));
                main_spans.push(Span::styled(
                    format!("●{}", count),
                    Style::default().fg(SELECTION_GREEN),
                ));
            }

            // Push shortcut to the right
            let label_len = item.label().len();
            let badge_len = badge.map(|c| format!(" ●{}", c).len()).unwrap_or(0);
            let used_width = 4 + label_len + badge_len; // accent + space + icon(2) + spaces + label + badge
            let available = area.width.saturating_sub(used_width as u16 + 6);

            if available > 0 {
                main_spans.push(Span::styled(
                    " ".repeat(available as usize),
                    Style::default(),
                ));
            }
            main_spans.push(Span::styled("[", Style::default().fg(SUBDUED_BORDER)));
            main_spans.push(Span::styled(item.shortcut(), shortcut_style));
            main_spans.push(Span::styled("]", Style::default().fg(SUBDUED_BORDER)));
        }

        let main_line = Paragraph::new(Line::from(main_spans)).style(Style::default().bg(bg_color));
        frame.render_widget(main_line, item_layout[0]);

        // Description line (only when selected and space available)
        if is_selected && state.show_labels && area.width > 15 {
            let desc_spans = vec![
                Span::styled(accent_bar, accent_style),
                Span::styled("     ", Style::default()), // Indent under icon
                Span::styled(
                    item.description(),
                    Style::default().fg(MUTED_GRAY).add_modifier(Modifier::ITALIC),
                ),
            ];
            let desc_line =
                Paragraph::new(Line::from(desc_spans)).style(Style::default().bg(bg_color));
            frame.render_widget(desc_line, item_layout[1]);
        } else {
            // Empty line with background
            let empty = Paragraph::new("").style(Style::default().bg(bg_color));
            frame.render_widget(empty, item_layout[1]);
        }
    }

    /// Get the recommended width for the sidebar
    pub fn recommended_width(state: &SidebarState) -> u16 {
        if state.show_labels {
            state.preferred_width // With labels + shortcuts
        } else {
            4 // Icons only
        }
    }
}

impl Default for SidebarComponent {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sidebar_state_navigation() {
        let mut state = SidebarState::new();
        assert_eq!(state.selected_index, 0);

        state.move_down();
        assert_eq!(state.selected_index, 1);

        state.move_up();
        assert_eq!(state.selected_index, 0);

        // Should not go below 0
        state.move_up();
        assert_eq!(state.selected_index, 0);
    }

    #[test]
    fn test_sidebar_item_properties() {
        let item = SidebarItem::Agents;
        assert_eq!(item.label(), "Agents");
        assert_eq!(item.icon(), "🤖");
    }

    #[test]
    fn inbox_tile_registered_with_discoverable_shortcut() {
        // The ainb-hooks Inbox screen is only useful if the user can
        // find it. Lock in the tile shape + position so a future
        // refactor doesn't quietly drop it from the home screen.
        let all = SidebarItem::all();
        let inbox_pos = all
            .iter()
            .position(|i| *i == SidebarItem::Inbox)
            .expect("SidebarItem::Inbox missing from all()");
        assert!(inbox_pos > 0, "Inbox shouldn't be first sidebar item");
        assert_eq!(SidebarItem::Inbox.icon(), "📥");
        assert_eq!(SidebarItem::Inbox.label(), "Inbox");
        assert_eq!(SidebarItem::Inbox.shortcut(), "b");
        assert_eq!(SidebarItem::Inbox.description(), "Hook Notifications");
        // 'b' must not collide with any other tile shortcut. Picked
        // over 'I' (Shift+i) to avoid the case-pair confusion with
        // Stats ('i') — both opening different screens off the same
        // letter was a UX bug.
        let collisions =
            all.iter().filter(|i| **i != SidebarItem::Inbox && i.shortcut() == "b").count();
        assert_eq!(collisions, 0, "sidebar shortcut 'b' collides");
    }

    #[test]
    fn memory_tile_registered_with_discoverable_shortcut() {
        // The learnings/Memory panel was reachable by the `m` key but had no
        // sidebar tile, so it couldn't be discovered from the home menu like
        // every other overlay panel (Inbox/Stats/Witr/Skills/Abtop). Lock the
        // tile shape + position + a non-colliding shortcut so it can't be
        // dropped again.
        let all = SidebarItem::all();
        let memory_pos = all
            .iter()
            .position(|i| *i == SidebarItem::Memory)
            .expect("SidebarItem::Memory missing from all()");
        assert!(memory_pos > 0, "Memory shouldn't be first sidebar item");
        assert_eq!(SidebarItem::Memory.icon(), "📚");
        assert_eq!(SidebarItem::Memory.label(), "Memory");
        assert_eq!(SidebarItem::Memory.shortcut(), "m");
        assert_eq!(SidebarItem::Memory.description(), "Knowledge & Recall");
        // 'm' must not collide with any other tile shortcut.
        let collisions =
            all.iter().filter(|i| **i != SidebarItem::Memory && i.shortcut() == "m").count();
        assert_eq!(collisions, 0, "sidebar shortcut 'm' collides");
    }

    #[test]
    fn test_select_specific_item() {
        let mut state = SidebarState::new();
        state.select(SidebarItem::Config);
        assert_eq!(state.selected_item(), SidebarItem::Config);
    }

    #[test]
    fn clamps_sidebar_width_to_default_bounds() {
        assert_eq!(SidebarState::clamp_width(8, 120), MIN_SIDEBAR_WIDTH);
        assert_eq!(SidebarState::clamp_width(90, 120), 70);
        assert_eq!(SidebarState::clamp_width(24, 120), 24);
    }

    #[test]
    fn locks_to_best_possible_width_on_tiny_terminals() {
        assert_eq!(SidebarState::clamp_width(26, 60), MIN_SIDEBAR_WIDTH);
        assert_eq!(SidebarState::clamp_width(26, 10), 9);
    }

    #[test]
    fn maps_sidebar_item_rows_from_render_layout() {
        let area = Rect::new(0, 5, 26, 30);
        assert_eq!(SidebarComponent::item_index_at(area, 7), None);
        assert_eq!(SidebarComponent::item_index_at(area, 8), Some(0));
        assert_eq!(SidebarComponent::item_index_at(area, 9), Some(0));
        assert_eq!(SidebarComponent::item_index_at(area, 10), Some(1));
    }
}
