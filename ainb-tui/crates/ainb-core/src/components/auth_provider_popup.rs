// ABOUTME: Auth provider selection popup for configuring Claude authentication method

use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
    Frame,
};

use crate::app::state::AppState;

// Color palette from TUI style guide
const CORNFLOWER_BLUE: Color = Color::Rgb(100, 149, 237);
const GOLD: Color = Color::Rgb(255, 215, 0);
const SELECTION_GREEN: Color = Color::Rgb(100, 200, 100);
const PANEL_BG: Color = Color::Rgb(30, 30, 40);
const LIST_HIGHLIGHT_BG: Color = Color::Rgb(40, 40, 60);
const SOFT_WHITE: Color = Color::Rgb(220, 220, 230);
const MUTED_GRAY: Color = Color::Rgb(120, 120, 140);
const COMING_SOON_GRAY: Color = Color::Rgb(80, 80, 100);

pub struct AuthProviderPopupComponent;

impl AuthProviderPopupComponent {
    pub fn new() -> Self {
        Self
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, state: &AppState) {
        // Calculate popup size (60% width, 70% height, centered)
        let popup_width = (area.width as f32 * 0.6) as u16;
        let popup_height = (area.height as f32 * 0.7) as u16;
        let popup_x = area.x + (area.width - popup_width) / 2;
        let popup_y = area.y + (area.height - popup_height) / 2;

        let popup_area = Rect::new(popup_x, popup_y, popup_width, popup_height);

        // Clear the background
        frame.render_widget(Clear, popup_area);

        // Main popup block
        let popup_block = Block::default()
            .title(Span::styled(
                " Select Auth Provider ",
                Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
            ))
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(CORNFLOWER_BLUE))
            .style(Style::default().bg(PANEL_BG));

        let inner = popup_block.inner(popup_area);
        frame.render_widget(popup_block, popup_area);

        // Layout: Provider list, API key input (if applicable), Help bar
        let layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(10),   // Provider list
                Constraint::Length(4), // API key input area
                Constraint::Length(2), // Help bar
            ])
            .split(inner);

        self.render_providers(frame, layout[0], state);
        self.render_api_key_input(frame, layout[1], state);
        self.render_help_bar(frame, layout[2], state);
    }

    fn render_providers(&self, frame: &mut Frame, area: Rect, state: &AppState) {
        let popup_state = &state.auth_provider_popup_state;

        let providers = &popup_state.providers;
        let selected = popup_state.selected_index;

        // Calculate lines needed (provider + description = 2 lines each, plus spacing)
        let mut lines = Vec::new();

        for (i, provider) in providers.iter().enumerate() {
            let is_selected = i == selected;
            let is_available = provider.available;

            // Selection indicator
            let indicator = if is_selected { "" } else { " " };

            // Provider name with icon
            let name_style = if !is_available {
                Style::default().fg(COMING_SOON_GRAY)
            } else if is_selected {
                Style::default()
                    .fg(SELECTION_GREEN)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(SOFT_WHITE)
            };

            let mut name_spans = vec![
                Span::styled(
                    format!("{} ", indicator),
                    if is_selected {
                        Style::default().fg(SELECTION_GREEN)
                    } else {
                        Style::default()
                    },
                ),
                Span::styled(&provider.icon, Style::default().fg(if is_available { GOLD } else { COMING_SOON_GRAY })),
                Span::styled(" ", Style::default()),
                Span::styled(&provider.name, name_style),
            ];

            // Add "Coming Soon" badge if not available
            if !is_available {
                name_spans.push(Span::styled(" ", Style::default()));
                name_spans.push(Span::styled(
                    "[Coming Soon]",
                    Style::default().fg(COMING_SOON_GRAY).add_modifier(Modifier::ITALIC),
                ));
            }

            // Add selection indicator for current auth method
            if provider.is_current {
                name_spans.push(Span::styled(" ", Style::default()));
                name_spans.push(Span::styled(
                    "(Current)",
                    Style::default().fg(CORNFLOWER_BLUE),
                ));
            }

            lines.push(Line::from(name_spans));

            // Description line (indented)
            let desc_style = if !is_available {
                Style::default().fg(COMING_SOON_GRAY)
            } else {
                Style::default().fg(MUTED_GRAY)
            };

            lines.push(Line::from(vec![
                Span::styled("    ", Style::default()),
                Span::styled(&provider.description, desc_style),
            ]));

            // Empty line for spacing
            lines.push(Line::from(""));
        }

        let paragraph = Paragraph::new(lines)
            .style(Style::default().bg(PANEL_BG));

        frame.render_widget(paragraph, area);
    }

    fn render_api_key_input(&self, frame: &mut Frame, area: Rect, state: &AppState) {
        let popup_state = &state.auth_provider_popup_state;

        // Only show input if API Key provider is selected
        let selected_provider = popup_state.providers.get(popup_state.selected_index);
        let is_api_key = selected_provider.map(|p| p.id == "api_key").unwrap_or(false);

        if !is_api_key || !popup_state.is_entering_key {
            // Show current API key status or hint
            let status_text = if is_api_key {
                let masked = crate::credentials::get_anthropic_api_key_masked();
                format!("API Key: {} (Press Enter to {})", masked, if masked == "Not configured" { "configure" } else { "update" })
            } else {
                String::new()
            };

            let status = Paragraph::new(Line::from(vec![
                Span::styled("  ", Style::default()),
                Span::styled(status_text, Style::default().fg(MUTED_GRAY)),
            ]))
            .style(Style::default().bg(PANEL_BG));

            frame.render_widget(status, area);
            return;
        }

        // API key input mode
        let input_block = Block::default()
            .title(Span::styled(
                " Enter API Key ",
                Style::default().fg(GOLD),
            ))
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(CORNFLOWER_BLUE))
            .style(Style::default().bg(LIST_HIGHLIGHT_BG));

        let inner = input_block.inner(area);
        frame.render_widget(input_block, area);

        // Mask the API key input (show only first 7 chars + dots)
        let display_key = if popup_state.api_key_input.len() > 7 {
            format!("{}{}|", &popup_state.api_key_input[..7], "•".repeat(popup_state.api_key_input.len() - 7))
        } else {
            format!("{}|", &popup_state.api_key_input)
        };

        let input_text = Paragraph::new(Line::from(vec![
            Span::styled(&display_key, Style::default().fg(SOFT_WHITE)),
        ]))
        .alignment(Alignment::Left)
        .style(Style::default().bg(LIST_HIGHLIGHT_BG));

        frame.render_widget(input_text, inner);
    }

    fn render_help_bar(&self, frame: &mut Frame, area: Rect, state: &AppState) {
        let popup_state = &state.auth_provider_popup_state;

        let help_items = if popup_state.is_entering_key {
            vec![
                ("Enter", "save"),
                ("Esc", "cancel"),
            ]
        } else {
            vec![
                ("", "navigate"),
                ("Enter", "select"),
                ("D", "delete key"),
                ("Esc", "close"),
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

        let help_bar = Paragraph::new(Line::from(spans)).style(Style::default().bg(PANEL_BG));

        frame.render_widget(help_bar, area);
    }
}

impl Default for AuthProviderPopupComponent {
    fn default() -> Self {
        Self::new()
    }
}
