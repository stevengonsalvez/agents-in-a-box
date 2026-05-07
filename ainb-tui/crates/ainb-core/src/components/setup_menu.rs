// ABOUTME: Setup menu component for factory reset and re-running onboarding
// Provides access to setup wizard, dependency checks, and configuration

use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
    Frame,
};

// Color palette from TUI style guide
const CORNFLOWER_BLUE: Color = Color::Rgb(100, 149, 237);
const GOLD: Color = Color::Rgb(255, 215, 0);
const SELECTION_GREEN: Color = Color::Rgb(100, 200, 100);
const DARK_BG: Color = Color::Rgb(25, 25, 35);
const PANEL_BG: Color = Color::Rgb(30, 30, 40);
const SOFT_WHITE: Color = Color::Rgb(220, 220, 230);
const MUTED_GRAY: Color = Color::Rgb(120, 120, 140);
const DANGER_RED: Color = Color::Rgb(220, 80, 80);

/// Menu items in the setup menu
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetupMenuItem {
    RerunWizard,
    CheckDependencies,
    ConfigureGitPaths,
    AuthenticationSettings,
    EditorPreference,
    FactoryReset,
}

impl SetupMenuItem {
    /// Get all menu items in order
    pub fn all() -> &'static [SetupMenuItem] {
        &[
            Self::RerunWizard,
            Self::CheckDependencies,
            Self::ConfigureGitPaths,
            Self::AuthenticationSettings,
            Self::EditorPreference,
            Self::FactoryReset,
        ]
    }

    /// Get display label
    pub fn label(&self) -> &'static str {
        match self {
            Self::RerunWizard => "Re-run Setup Wizard",
            Self::CheckDependencies => "Check Dependencies",
            Self::ConfigureGitPaths => "Configure Git Paths",
            Self::AuthenticationSettings => "Authentication Settings",
            Self::EditorPreference => "Editor Preference",
            Self::FactoryReset => "Factory Reset AINB",
        }
    }

    /// Get description for this item
    pub fn description(&self) -> &'static str {
        match self {
            Self::RerunWizard => "Start the full setup wizard from scratch",
            Self::CheckDependencies => "Verify all required tools are installed",
            Self::ConfigureGitPaths => "Update your project directories",
            Self::AuthenticationSettings => "Configure Claude authentication",
            Self::EditorPreference => "Choose your preferred code editor",
            Self::FactoryReset => "Remove all configuration and start fresh",
        }
    }

    /// Get icon for this item
    pub fn icon(&self) -> &'static str {
        match self {
            Self::RerunWizard => "🔄",
            Self::CheckDependencies => "🔍",
            Self::ConfigureGitPaths => "📁",
            Self::AuthenticationSettings => "🔐",
            Self::EditorPreference => "📝",
            Self::FactoryReset => "⚠️",
        }
    }

    /// Is this a dangerous action?
    pub fn is_dangerous(&self) -> bool {
        matches!(self, Self::FactoryReset)
    }
}

/// State for the setup menu
#[derive(Debug)]
pub struct SetupMenuState {
    /// Currently selected item index
    pub selected_index: usize,
    /// Whether confirmation dialog is showing
    pub showing_confirmation: bool,
    /// Item pending confirmation
    pub pending_action: Option<SetupMenuItem>,
}

impl SetupMenuState {
    pub fn new() -> Self {
        Self {
            selected_index: 0,
            showing_confirmation: false,
            pending_action: None,
        }
    }

    /// Get currently selected item
    pub fn selected_item(&self) -> SetupMenuItem {
        SetupMenuItem::all()[self.selected_index]
    }

    /// Move selection up
    pub fn move_up(&mut self) {
        if self.selected_index > 0 {
            self.selected_index -= 1;
        }
    }

    /// Move selection down
    pub fn move_down(&mut self) {
        let max_index = SetupMenuItem::all().len() - 1;
        if self.selected_index < max_index {
            self.selected_index += 1;
        }
    }

    /// Request action (may show confirmation)
    pub fn request_action(&mut self) -> Option<SetupMenuItem> {
        let item = self.selected_item();
        if item.is_dangerous() {
            self.showing_confirmation = true;
            self.pending_action = Some(item);
            None
        } else {
            Some(item)
        }
    }

    /// Confirm pending action
    pub fn confirm_action(&mut self) -> Option<SetupMenuItem> {
        self.showing_confirmation = false;
        self.pending_action.take()
    }

    /// Cancel pending action
    pub fn cancel_action(&mut self) {
        self.showing_confirmation = false;
        self.pending_action = None;
    }
}

impl Default for SetupMenuState {
    fn default() -> Self {
        Self::new()
    }
}

/// Setup menu component
pub struct SetupMenuComponent;

impl SetupMenuComponent {
    pub fn new() -> Self {
        Self
    }

    /// Render the setup menu as a centered dialog
    pub fn render(&self, frame: &mut Frame, area: Rect, state: &SetupMenuState) {
        // Calculate centered dialog area
        let dialog_width = 50u16.min(area.width.saturating_sub(4));
        let dialog_height = 18u16.min(area.height.saturating_sub(4));

        let dialog_x = (area.width.saturating_sub(dialog_width)) / 2;
        let dialog_y = (area.height.saturating_sub(dialog_height)) / 2;

        let dialog_area = Rect::new(
            area.x + dialog_x,
            area.y + dialog_y,
            dialog_width,
            dialog_height,
        );

        // Clear the dialog area
        frame.render_widget(Clear, dialog_area);

        // Draw outer block
        let block = Block::default()
            .title(Line::from(vec![
                Span::styled(" 🛠️ ", Style::default().fg(GOLD)),
                Span::styled("Setup ", Style::default().fg(GOLD).add_modifier(Modifier::BOLD)),
            ]))
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(CORNFLOWER_BLUE))
            .style(Style::default().bg(PANEL_BG));

        let inner = block.inner(dialog_area);
        frame.render_widget(block, dialog_area);

        // Split inner area
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),  // Top padding
                Constraint::Min(8),     // Menu items
                Constraint::Length(2),  // Help bar
            ])
            .split(inner);

        // Render menu items
        self.render_menu_items(frame, chunks[1], state);

        // Render help bar
        self.render_help_bar(frame, chunks[2], state);

        // Render confirmation dialog if showing
        if state.showing_confirmation {
            self.render_confirmation_dialog(frame, area, state);
        }
    }

    /// Render menu items
    fn render_menu_items(&self, frame: &mut Frame, area: Rect, state: &SetupMenuState) {
        let items = SetupMenuItem::all();

        // Calculate item height (2 lines per item: label + description)
        let item_height = 2u16;
        let total_height = items.len() as u16 * item_height;

        // Create constraints for each item plus separator before Factory Reset
        let mut constraints: Vec<Constraint> = Vec::new();
        for (i, _) in items.iter().enumerate() {
            if i == items.len() - 1 {
                // Add separator before Factory Reset
                constraints.push(Constraint::Length(1)); // Separator
            }
            constraints.push(Constraint::Length(item_height));
        }
        constraints.push(Constraint::Min(0)); // Remaining space

        let layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints(constraints)
            .split(area);

        let mut layout_idx = 0;
        for (i, item) in items.iter().enumerate() {
            // Add separator before Factory Reset
            if i == items.len() - 1 {
                let separator = Paragraph::new("─".repeat(area.width.saturating_sub(2) as usize))
                    .style(Style::default().fg(MUTED_GRAY))
                    .alignment(Alignment::Center);
                frame.render_widget(separator, layout[layout_idx]);
                layout_idx += 1;
            }

            let is_selected = state.selected_index == i;
            self.render_menu_item(frame, layout[layout_idx], item, is_selected);
            layout_idx += 1;
        }
    }

    /// Render a single menu item
    fn render_menu_item(&self, frame: &mut Frame, area: Rect, item: &SetupMenuItem, is_selected: bool) {
        let (indicator, label_style, desc_style, bg_color) = if is_selected {
            (
                "▶ ",
                Style::default().fg(if item.is_dangerous() { DANGER_RED } else { SOFT_WHITE }).add_modifier(Modifier::BOLD),
                Style::default().fg(MUTED_GRAY),
                Color::Rgb(40, 45, 60),
            )
        } else {
            (
                "  ",
                Style::default().fg(if item.is_dangerous() { DANGER_RED } else { MUTED_GRAY }),
                Style::default().fg(Color::Rgb(80, 80, 100)),
                PANEL_BG,
            )
        };

        // Split area for label and description
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Length(1),
            ])
            .split(area);

        // Label line
        let label_line = Line::from(vec![
            Span::styled(indicator, Style::default().fg(SELECTION_GREEN)),
            Span::styled(item.icon(), label_style),
            Span::styled(" ", Style::default()),
            Span::styled(item.label(), label_style),
        ]);

        let label_para = Paragraph::new(label_line)
            .style(Style::default().bg(bg_color));
        frame.render_widget(label_para, chunks[0]);

        // Description line (indented)
        let desc_line = Line::from(vec![
            Span::styled("     ", Style::default()),  // Indent
            Span::styled(item.description(), desc_style),
        ]);

        let desc_para = Paragraph::new(desc_line)
            .style(Style::default().bg(bg_color));
        frame.render_widget(desc_para, chunks[1]);
    }

    /// Render help bar
    fn render_help_bar(&self, frame: &mut Frame, area: Rect, _state: &SetupMenuState) {
        let help_line = Line::from(vec![
            Span::styled("↑↓", Style::default().fg(GOLD)),
            Span::styled(" Navigate  ", Style::default().fg(MUTED_GRAY)),
            Span::styled("Enter", Style::default().fg(GOLD)),
            Span::styled(" Select  ", Style::default().fg(MUTED_GRAY)),
            Span::styled("Esc", Style::default().fg(GOLD)),
            Span::styled(" Back", Style::default().fg(MUTED_GRAY)),
        ]);

        let help = Paragraph::new(help_line)
            .alignment(Alignment::Center)
            .style(Style::default().bg(DARK_BG));
        frame.render_widget(help, area);
    }

    /// Render confirmation dialog for dangerous actions
    fn render_confirmation_dialog(&self, frame: &mut Frame, area: Rect, state: &SetupMenuState) {
        let dialog_width = 40u16.min(area.width.saturating_sub(8));
        let dialog_height = 8u16;

        let dialog_x = (area.width.saturating_sub(dialog_width)) / 2;
        let dialog_y = (area.height.saturating_sub(dialog_height)) / 2;

        let dialog_area = Rect::new(
            area.x + dialog_x,
            area.y + dialog_y,
            dialog_width,
            dialog_height,
        );

        // Clear and draw dialog
        frame.render_widget(Clear, dialog_area);

        let block = Block::default()
            .title(Line::from(vec![
                Span::styled(" ⚠️ ", Style::default().fg(DANGER_RED)),
                Span::styled("Confirm ", Style::default().fg(DANGER_RED).add_modifier(Modifier::BOLD)),
            ]))
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(DANGER_RED))
            .style(Style::default().bg(PANEL_BG));

        let inner = block.inner(dialog_area);
        frame.render_widget(block, dialog_area);

        // Content
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(2),  // Message
                Constraint::Length(1),  // Spacer
                Constraint::Length(1),  // Buttons
            ])
            .split(inner);

        let message = if let Some(item) = &state.pending_action {
            match item {
                SetupMenuItem::FactoryReset => {
                    "This will delete all AINB\nconfiguration. Continue?"
                }
                _ => "Are you sure?"
            }
        } else {
            "Are you sure?"
        };

        let msg_para = Paragraph::new(message)
            .alignment(Alignment::Center)
            .style(Style::default().fg(SOFT_WHITE));
        frame.render_widget(msg_para, chunks[0]);

        let buttons = Line::from(vec![
            Span::styled("[Y]", Style::default().fg(DANGER_RED).add_modifier(Modifier::BOLD)),
            Span::styled("es  ", Style::default().fg(MUTED_GRAY)),
            Span::styled("[N]", Style::default().fg(SELECTION_GREEN).add_modifier(Modifier::BOLD)),
            Span::styled("o", Style::default().fg(MUTED_GRAY)),
        ]);

        let btn_para = Paragraph::new(buttons)
            .alignment(Alignment::Center);
        frame.render_widget(btn_para, chunks[2]);
    }
}

impl Default for SetupMenuComponent {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_menu_navigation() {
        let mut state = SetupMenuState::new();
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
    fn test_dangerous_action_confirmation() {
        let mut state = SetupMenuState::new();

        // Move to Factory Reset (last item)
        for _ in 0..5 {
            state.move_down();
        }
        assert_eq!(state.selected_item(), SetupMenuItem::FactoryReset);

        // Request action should show confirmation
        let result = state.request_action();
        assert!(result.is_none());
        assert!(state.showing_confirmation);
        assert_eq!(state.pending_action, Some(SetupMenuItem::FactoryReset));

        // Cancel should clear
        state.cancel_action();
        assert!(!state.showing_confirmation);
        assert!(state.pending_action.is_none());
    }

    #[test]
    fn test_safe_action_no_confirmation() {
        let mut state = SetupMenuState::new();

        // First item (Re-run Wizard) is safe
        let result = state.request_action();
        assert_eq!(result, Some(SetupMenuItem::RerunWizard));
        assert!(!state.showing_confirmation);
    }
}
