// ABOUTME: Configuration screen component with split-pane layout for AINB 2.0

use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, List, ListItem, Paragraph},
    Frame,
};

use crate::app::state::{AppState, ConfigPane};

// Color palette from TUI style guide
const CORNFLOWER_BLUE: Color = Color::Rgb(100, 149, 237);
const GOLD: Color = Color::Rgb(255, 215, 0);
const SELECTION_GREEN: Color = Color::Rgb(100, 200, 100);
const DARK_BG: Color = Color::Rgb(25, 25, 35);
const PANEL_BG: Color = Color::Rgb(30, 30, 40);
const LIST_HIGHLIGHT_BG: Color = Color::Rgb(40, 40, 60);
const SOFT_WHITE: Color = Color::Rgb(220, 220, 230);
const MUTED_GRAY: Color = Color::Rgb(120, 120, 140);

pub struct ConfigScreenComponent;

impl ConfigScreenComponent {
    pub fn new() -> Self {
        Self
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, state: &AppState) {
        tracing::debug!("Rendering ConfigScreen view");

        // Main container with dark background
        let container = Block::default().style(Style::default().bg(DARK_BG));
        frame.render_widget(container, area);

        // Main layout: Title, Content (split), Help bar
        let main_layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3), // Title bar
                Constraint::Min(0),    // Content area
                Constraint::Length(2), // Help bar
            ])
            .split(area);

        self.render_title(frame, main_layout[0], state);
        self.render_content(frame, main_layout[1], state);
        self.render_help_bar(frame, main_layout[2], state);
    }

    fn render_title(&self, frame: &mut Frame, area: Rect, state: &AppState) {
        let agent_name = state
            .agent_selection_state
            .current_model()
            .map(|m| m.name.clone())
            .unwrap_or_else(|| "No Agent".to_string());

        let title_block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(CORNFLOWER_BLUE))
            .style(Style::default().bg(PANEL_BG));

        let inner = title_block.inner(area);
        frame.render_widget(title_block, area);

        let title_text = Paragraph::new(Line::from(vec![
            Span::styled("  ", Style::default()),
            Span::styled(
                "Configuration",
                Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
            ),
            Span::styled("                                        ", Style::default()),
            Span::styled("[", Style::default().fg(MUTED_GRAY)),
            Span::styled(&agent_name, Style::default().fg(CORNFLOWER_BLUE)),
            Span::styled("]", Style::default().fg(MUTED_GRAY)),
        ]))
        .alignment(Alignment::Left)
        .style(Style::default().bg(PANEL_BG));

        frame.render_widget(title_text, inner);
    }

    fn render_content(&self, frame: &mut Frame, area: Rect, state: &AppState) {
        // Split into categories (left) and settings (right)
        let content_layout = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(30), // Categories panel
                Constraint::Percentage(70), // Settings panel
            ])
            .split(area);

        self.render_categories(frame, content_layout[0], state);
        self.render_settings(frame, content_layout[1], state);
    }

    fn render_categories(&self, frame: &mut Frame, area: Rect, state: &AppState) {
        let config_state = &state.config_screen_state;
        let is_focused = config_state.focused_pane == ConfigPane::Categories;

        // Use GOLD border when focused, CORNFLOWER_BLUE when not
        let border_color = if is_focused { GOLD } else { CORNFLOWER_BLUE };
        let title_style = if is_focused {
            Style::default().fg(GOLD).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(MUTED_GRAY)
        };

        let block = Block::default()
            .title(Span::styled(" Categories ", title_style))
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(border_color))
            .style(Style::default().bg(PANEL_BG));

        let items: Vec<ListItem> = config_state
            .categories
            .iter()
            .enumerate()
            .map(|(i, cat)| {
                let is_selected = i == config_state.selected_category;
                let icon = cat.icon();
                let label = cat.label();

                let style = if is_selected {
                    Style::default()
                        .fg(SELECTION_GREEN)
                        .bg(LIST_HIGHLIGHT_BG)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(SOFT_WHITE)
                };

                let indicator = if is_selected { " " } else { "  " };

                ListItem::new(Line::from(vec![
                    Span::styled(indicator, Style::default().fg(SELECTION_GREEN)),
                    Span::styled(icon, Style::default().fg(GOLD)),
                    Span::styled(" ", Style::default()),
                    Span::styled(label, style),
                ]))
            })
            .collect();

        let list = List::new(items)
            .block(block)
            .style(Style::default().bg(PANEL_BG));

        frame.render_widget(list, area);
    }

    fn render_settings(&self, frame: &mut Frame, area: Rect, state: &AppState) {
        let config_state = &state.config_screen_state;
        let current_category = &config_state.categories[config_state.selected_category];
        let is_focused = config_state.focused_pane == ConfigPane::Settings;

        // Use GOLD border when focused, CORNFLOWER_BLUE when not
        let border_color = if is_focused { GOLD } else { CORNFLOWER_BLUE };
        let title_style = if is_focused {
            Style::default().fg(GOLD).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(MUTED_GRAY)
        };

        let block = Block::default()
            .title(Span::styled(
                format!(" {} Settings ", current_category.label()),
                title_style,
            ))
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(border_color))
            .style(Style::default().bg(PANEL_BG));

        let inner = block.inner(area);
        frame.render_widget(block, area);

        // Get settings for current category
        if let Some(settings) = config_state.settings.get(current_category) {
            let settings_layout = Layout::default()
                .direction(Direction::Vertical)
                .constraints(
                    settings
                        .iter()
                        .map(|_| Constraint::Length(4))
                        .collect::<Vec<_>>(),
                )
                .split(inner);

            for (i, setting) in settings.iter().enumerate() {
                if i >= settings_layout.len() {
                    break;
                }
                self.render_setting(frame, settings_layout[i], setting, i == config_state.selected_setting, config_state.editing && i == config_state.selected_setting, &config_state.edit_buffer);
            }
        }
    }

    fn render_setting(
        &self,
        frame: &mut Frame,
        area: Rect,
        setting: &crate::app::state::ConfigSetting,
        is_selected: bool,
        is_editing: bool,
        edit_buffer: &str,
    ) {
        let layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1), // Label + value
                Constraint::Length(1), // Separator
                Constraint::Length(1), // Description
                Constraint::Length(1), // Padding
            ])
            .split(area);

        // Selection indicator
        let indicator = if is_selected { "" } else { " " };
        let indicator_style = if is_selected {
            Style::default().fg(SELECTION_GREEN)
        } else {
            Style::default().fg(MUTED_GRAY)
        };

        // Value display
        let value_display = if is_editing {
            format!("{}|", edit_buffer)
        } else {
            setting.value.display()
        };

        let value_style = if is_editing {
            Style::default()
                .fg(GOLD)
                .add_modifier(Modifier::UNDERLINED)
        } else {
            Style::default().fg(SOFT_WHITE)
        };

        // Label + Value line
        let label_line = Paragraph::new(Line::from(vec![
            Span::styled(format!("{} ", indicator), indicator_style),
            Span::styled(
                &setting.label,
                if is_selected {
                    Style::default()
                        .fg(SELECTION_GREEN)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(SOFT_WHITE)
                },
            ),
            Span::styled(": ", Style::default().fg(MUTED_GRAY)),
            Span::styled(value_display, value_style),
        ]));
        frame.render_widget(label_line, layout[0]);

        // Separator
        let separator = Paragraph::new(Line::from(vec![Span::styled(
            "  ────────────────────────────────────────────────",
            Style::default().fg(MUTED_GRAY),
        )]));
        frame.render_widget(separator, layout[1]);

        // Description
        let desc = Paragraph::new(Line::from(vec![
            Span::styled("  ", Style::default()),
            Span::styled("", Style::default().fg(CORNFLOWER_BLUE)),
            Span::styled(" ", Style::default()),
            Span::styled(&setting.description, Style::default().fg(MUTED_GRAY)),
        ]));
        frame.render_widget(desc, layout[2]);
    }

    fn render_help_bar(&self, frame: &mut Frame, area: Rect, state: &AppState) {
        let config_state = &state.config_screen_state;

        let help_items = if config_state.editing {
            vec![
                ("Enter", "save"),
                ("Esc", "cancel"),
            ]
        } else {
            vec![
                ("↑↓", "navigate"),
                ("←→/Tab", "switch pane"),
                ("Enter", "edit"),
                ("S", "save all"),
                ("Esc", "back"),
            ]
        };

        let mut spans = Vec::new();
        spans.push(Span::styled("  ", Style::default()));

        for (i, (key, desc)) in help_items.iter().enumerate() {
            if i > 0 {
                spans.push(Span::styled(" | ", Style::default().fg(MUTED_GRAY)));
            }
            spans.push(Span::styled(
                *key,
                Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
            ));
            spans.push(Span::styled(" ", Style::default()));
            spans.push(Span::styled(*desc, Style::default().fg(MUTED_GRAY)));
        }

        let help_bar = Paragraph::new(Line::from(spans)).style(Style::default().bg(DARK_BG));

        frame.render_widget(help_bar, area);
    }
}

impl Default for ConfigScreenComponent {
    fn default() -> Self {
        Self::new()
    }
}
