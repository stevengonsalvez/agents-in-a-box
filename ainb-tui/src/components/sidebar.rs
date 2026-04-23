// ABOUTME: Premium sidebar navigation component for AINB home screen
// Inspired by VS Code, Discord, and Slack sidebar patterns with enhanced selection styling

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Paragraph},
    Frame,
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

/// Sidebar navigation items - matches HomeTile options
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidebarItem {
    Agents,    // Agent selection
    Catalog,   // Browse catalog/marketplace
    Config,    // Settings & presets
    Sessions,  // Session manager
    Recovery,  // Recover orphaned sessions
    Logs,      // Log history viewer
    Stats,     // Analytics & usage
    Skills,    // Browse per-agent skills
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
            Self::Recovery => "🔄",
            Self::Logs => "📋",
            Self::Stats => "📊",
            Self::Skills => "🧠",
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
            Self::Recovery => "Recovery",
            Self::Logs => "Logs",
            Self::Stats => "Stats",
            Self::Skills => "Skills",
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
            Self::Recovery => "Resume Orphaned",
            Self::Logs => "View Log History",
            Self::Stats => "Usage & Analytics",
            Self::Skills => "Per-Agent Skills",
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
            Self::Recovery => "R",
            Self::Logs => "l",
            Self::Stats => "i",
            Self::Skills => "k",
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
            Self::Recovery,
            Self::Logs,
            Self::Stats,
            Self::Skills,
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
}

impl SidebarState {
    pub fn new() -> Self {
        Self {
            selected_index: 0,
            is_focused: true,
            show_labels: true,
            active_sessions_count: 0,
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
        // Outer block with subtle border
        let border_color = if state.is_focused {
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

        // Layout: title + items + flexible space
        // Using 2 lines per item to fit 11 items in typical terminal heights
        let layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(2),  // Title area
                Constraint::Length(1),  // Spacer
                Constraint::Length(2),  // Agents
                Constraint::Length(2),  // Catalog
                Constraint::Length(2),  // Config
                Constraint::Length(2),  // Sessions
                Constraint::Length(2),  // Recovery
                Constraint::Length(2),  // Logs
                Constraint::Length(2),  // Stats
                Constraint::Length(2),  // Skills
                Constraint::Length(2),  // Changelog
                Constraint::Length(2),  // Setup
                Constraint::Length(2),  // Help
                Constraint::Min(0),     // Flexible space
            ])
            .split(inner);

        // Render title
        self.render_title(frame, layout[0], state);

        let items = SidebarItem::all();

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

        let main_line = Paragraph::new(Line::from(main_spans))
            .style(Style::default().bg(bg_color));
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
            let desc_line = Paragraph::new(Line::from(desc_spans))
                .style(Style::default().bg(bg_color));
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
            24 // With labels + shortcuts
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
    fn test_select_specific_item() {
        let mut state = SidebarState::new();
        state.select(SidebarItem::Config);
        assert_eq!(state.selected_item(), SidebarItem::Config);
    }
}
