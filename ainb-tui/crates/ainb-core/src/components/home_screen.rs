// ABOUTME: Home screen component with tile-based navigation for AINB 2.0

use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Paragraph},
    Frame,
};

use crate::app::state::{AppState, HomeTile};

// Color palette from TUI style guide
const CORNFLOWER_BLUE: Color = Color::Rgb(100, 149, 237);
const GOLD: Color = Color::Rgb(255, 215, 0);
const SELECTION_GREEN: Color = Color::Rgb(100, 200, 100);
const DARK_BG: Color = Color::Rgb(25, 25, 35);
const PANEL_BG: Color = Color::Rgb(30, 30, 40);
const LIST_HIGHLIGHT_BG: Color = Color::Rgb(40, 40, 60);
const SOFT_WHITE: Color = Color::Rgb(220, 220, 230);
const MUTED_GRAY: Color = Color::Rgb(120, 120, 140);

pub struct HomeScreenComponent;

impl HomeScreenComponent {
    pub fn new() -> Self {
        Self
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, state: &AppState) {
        // Main container with dark background
        let container = Block::default()
            .style(Style::default().bg(DARK_BG));
        frame.render_widget(container, area);

        // Layout: Title bar, main content, footer
        let layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(4),  // Title area
                Constraint::Min(0),     // Tiles area
                Constraint::Length(3),  // Recent session info
                Constraint::Length(2),  // Help bar
            ])
            .split(area);

        self.render_title(frame, layout[0], state);
        self.render_tiles(frame, layout[1], state);
        self.render_recent_session(frame, layout[2], state);
        self.render_help_bar(frame, layout[3]);
    }

    fn render_title(&self, frame: &mut Frame, area: Rect, _state: &AppState) {
        let title = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(CORNFLOWER_BLUE))
            .style(Style::default().bg(PANEL_BG));

        let inner = title.inner(area);
        frame.render_widget(title, area);

        let title_text = Paragraph::new(Line::from(vec![
            Span::styled("  ", Style::default()),
            Span::styled("AINB", Style::default().fg(GOLD).add_modifier(Modifier::BOLD)),
            Span::styled(" - Agentic Coding Hub", Style::default().fg(SOFT_WHITE)),
            Span::styled("                    ", Style::default()),
            Span::styled("v2.0.0", Style::default().fg(MUTED_GRAY)),
        ]))
        .alignment(Alignment::Center)
        .style(Style::default().bg(PANEL_BG));

        frame.render_widget(title_text, inner);
    }

    fn render_tiles(&self, frame: &mut Frame, area: Rect, state: &AppState) {
        let home_state = &state.home_screen_state;

        // Create a 3-row grid layout for 7 tiles
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),  // Top padding
                Constraint::Length(6),  // Row 1
                Constraint::Length(1),  // Middle padding
                Constraint::Length(6),  // Row 2
                Constraint::Length(1),  // Middle padding
                Constraint::Length(6),  // Row 3
                Constraint::Min(0),     // Bottom padding
            ])
            .split(area);

        let row1_cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(5),  // Left padding
                Constraint::Percentage(28), // Tile 1
                Constraint::Percentage(2),  // Spacer
                Constraint::Percentage(28), // Tile 2
                Constraint::Percentage(2),  // Spacer
                Constraint::Percentage(28), // Tile 3
                Constraint::Percentage(7),  // Right padding
            ])
            .split(rows[1]);

        let row2_cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(5),  // Left padding
                Constraint::Percentage(28), // Tile 4
                Constraint::Percentage(2),  // Spacer
                Constraint::Percentage(28), // Tile 5 (Recovery)
                Constraint::Percentage(2),  // Spacer
                Constraint::Percentage(28), // Tile 6
                Constraint::Percentage(7),  // Right padding
            ])
            .split(rows[3]);

        let row3_cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(36), // Left padding
                Constraint::Percentage(28), // Tile 7 (Help - centered)
                Constraint::Percentage(36), // Right padding
            ])
            .split(rows[5]);

        // Render tiles: Agents, Catalog, Config (row 1)
        self.render_tile(frame, row1_cols[1], &HomeTile::Agents, home_state.selected_tile == 0);
        self.render_tile(frame, row1_cols[3], &HomeTile::Catalog, home_state.selected_tile == 1);
        self.render_tile(frame, row1_cols[5], &HomeTile::Config, home_state.selected_tile == 2);

        // Render tiles: Sessions, Recovery, Stats (row 2)
        self.render_tile(frame, row2_cols[1], &HomeTile::Sessions, home_state.selected_tile == 3);
        self.render_tile(frame, row2_cols[3], &HomeTile::Recovery, home_state.selected_tile == 4);
        self.render_tile(frame, row2_cols[5], &HomeTile::Stats, home_state.selected_tile == 5);

        // Render tiles: Help (row 3 - centered)
        self.render_tile(frame, row3_cols[1], &HomeTile::Help, home_state.selected_tile == 6);
    }

    fn render_tile(&self, frame: &mut Frame, area: Rect, tile: &HomeTile, is_selected: bool) {
        let border_color = if is_selected { SELECTION_GREEN } else { CORNFLOWER_BLUE };
        let bg_color = if is_selected { LIST_HIGHLIGHT_BG } else { PANEL_BG };

        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(border_color))
            .style(Style::default().bg(bg_color));

        let inner = block.inner(area);
        frame.render_widget(block, area);

        // Tile content layout
        let content_layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1), // Top padding
                Constraint::Length(1), // Icon line
                Constraint::Length(1), // Title line
                Constraint::Length(1), // Description line
                Constraint::Min(0),    // Bottom padding
            ])
            .split(inner);

        // Selection indicator
        let selection_indicator = if is_selected { " " } else { "  " };

        // Icon line
        let icon_text = Paragraph::new(Line::from(vec![
            Span::styled(selection_indicator, Style::default().fg(SELECTION_GREEN)),
            Span::styled(tile.icon(), Style::default().fg(GOLD)),
            Span::styled(" ", Style::default()),
            Span::styled(tile.label(), Style::default().fg(GOLD).add_modifier(Modifier::BOLD)),
        ]))
        .alignment(Alignment::Center);
        frame.render_widget(icon_text, content_layout[1]);

        // Description line
        let desc_text = Paragraph::new(Line::from(vec![
            Span::styled(tile.description(), Style::default().fg(MUTED_GRAY)),
        ]))
        .alignment(Alignment::Center);
        frame.render_widget(desc_text, content_layout[3]);
    }

    fn render_recent_session(&self, frame: &mut Frame, area: Rect, state: &AppState) {
        let block = Block::default()
            .borders(Borders::TOP)
            .border_style(Style::default().fg(CORNFLOWER_BLUE))
            .style(Style::default().bg(DARK_BG));

        let inner = block.inner(area);
        frame.render_widget(block, area);

        // Show recent session or placeholder
        let recent_text = if let Some(workspace) = state.workspaces.first() {
            if let Some(session) = workspace.sessions.first() {
                let status_icon = if session.status.is_running() { "" } else { "" };
                format!(
                    "  Recent: {}/{} {} {}",
                    workspace.name,
                    session.branch_name,
                    status_icon,
                    if session.status.is_running() { "(running)" } else { "(stopped)" }
                )
            } else {
                "  No recent sessions".to_string()
            }
        } else {
            "  No workspaces configured".to_string()
        };

        let recent = Paragraph::new(Line::from(vec![
            Span::styled(recent_text, Style::default().fg(SOFT_WHITE)),
        ]))
        .style(Style::default().bg(DARK_BG));

        frame.render_widget(recent, inner);
    }

    fn render_help_bar(&self, frame: &mut Frame, area: Rect) {
        let help_items = vec![
            ("Enter", "select"),
            ("", "navigate"),
            ("q", "quit"),
        ];

        let mut spans = Vec::new();
        spans.push(Span::styled("  ", Style::default()));

        for (i, (key, desc)) in help_items.iter().enumerate() {
            if i > 0 {
                spans.push(Span::styled(" | ", Style::default().fg(MUTED_GRAY)));
            }
            spans.push(Span::styled(*key, Style::default().fg(GOLD).add_modifier(Modifier::BOLD)));
            spans.push(Span::styled(" ", Style::default()));
            spans.push(Span::styled(*desc, Style::default().fg(MUTED_GRAY)));
        }

        let help_bar = Paragraph::new(Line::from(spans))
            .style(Style::default().bg(DARK_BG));

        frame.render_widget(help_bar, area);
    }
}

impl Default for HomeScreenComponent {
    fn default() -> Self {
        Self::new()
    }
}
