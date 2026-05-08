// ABOUTME: Agent selection screen for choosing AI provider and model

use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Paragraph},
    Frame,
};

use crate::app::state::{AgentProvider, AppState, ProviderStatus};

// Color palette from TUI style guide
const CORNFLOWER_BLUE: Color = Color::Rgb(100, 149, 237);
const GOLD: Color = Color::Rgb(255, 215, 0);
const SELECTION_GREEN: Color = Color::Rgb(100, 200, 100);
const WARNING_ORANGE: Color = Color::Rgb(255, 165, 0);
const DARK_BG: Color = Color::Rgb(25, 25, 35);
const PANEL_BG: Color = Color::Rgb(30, 30, 40);
#[allow(dead_code)]
const LIST_HIGHLIGHT_BG: Color = Color::Rgb(40, 40, 60);
const SOFT_WHITE: Color = Color::Rgb(220, 220, 230);
const MUTED_GRAY: Color = Color::Rgb(120, 120, 140);

pub struct AgentSelectionComponent;

impl AgentSelectionComponent {
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
                Constraint::Length(3),  // Title area
                Constraint::Min(0),     // Provider list
                Constraint::Length(2),  // Help bar
            ])
            .split(area);

        self.render_title(frame, layout[0]);
        self.render_providers(frame, layout[1], state);
        self.render_help_bar(frame, layout[2]);
    }

    fn render_title(&self, frame: &mut Frame, area: Rect) {
        let title = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(CORNFLOWER_BLUE))
            .style(Style::default().bg(PANEL_BG));

        let inner = title.inner(area);
        frame.render_widget(title, area);

        let title_text = Paragraph::new(Line::from(vec![
            Span::styled("  Choose Your Agent", Style::default().fg(GOLD).add_modifier(Modifier::BOLD)),
        ]))
        .alignment(Alignment::Left)
        .style(Style::default().bg(PANEL_BG));

        frame.render_widget(title_text, inner);
    }

    fn render_providers(&self, frame: &mut Frame, area: Rect, state: &AppState) {
        let agent_state = &state.agent_selection_state;

        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(CORNFLOWER_BLUE))
            .style(Style::default().bg(PANEL_BG));

        let inner = block.inner(area);
        frame.render_widget(block, area);

        // Calculate how many lines we need for each provider
        let mut lines: Vec<Line> = Vec::new();
        lines.push(Line::from("")); // Top padding

        for (idx, provider) in agent_state.providers.iter().enumerate() {
            let is_selected = idx == agent_state.selected_provider;
            let is_expanded = agent_state.expanded_provider == Some(idx);

            lines.push(self.render_provider_line(provider, is_selected, is_expanded));

            // If expanded, show models
            if is_expanded {
                for (model_idx, model) in provider.models.iter().enumerate() {
                    let is_model_selected = is_selected && model_idx == agent_state.selected_model;
                    lines.push(self.render_model_line(model, is_model_selected, provider.status == ProviderStatus::ComingSoon));
                }
            }

            lines.push(Line::from("")); // Spacing between providers
        }

        let content = Paragraph::new(lines)
            .style(Style::default().bg(PANEL_BG));

        frame.render_widget(content, inner);
    }

    fn render_provider_line(&self, provider: &AgentProvider, is_selected: bool, is_expanded: bool) -> Line<'static> {
        let selection_indicator = if is_selected { "" } else { "  " };
        let expand_indicator = if is_expanded { "" } else { "" };

        let status_badge = match provider.status {
            ProviderStatus::Available => "",
            ProviderStatus::ComingSoon => " [Coming Soon]",
            ProviderStatus::Disabled => " [Disabled]",
        };

        let status_style = match provider.status {
            ProviderStatus::Available => Style::default().fg(SELECTION_GREEN),
            ProviderStatus::ComingSoon => Style::default().fg(WARNING_ORANGE),
            ProviderStatus::Disabled => Style::default().fg(MUTED_GRAY),
        };

        let name_style = if is_selected {
            Style::default().fg(GOLD).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(SOFT_WHITE)
        };

        Line::from(vec![
            Span::styled("  ", Style::default()),
            Span::styled(selection_indicator.to_string(), Style::default().fg(SELECTION_GREEN)),
            Span::styled(" ", Style::default()),
            Span::styled(expand_indicator.to_string(), Style::default().fg(MUTED_GRAY)),
            Span::styled(" ", Style::default()),
            Span::styled(provider.name.clone(), name_style),
            Span::styled(format!("                    {}", provider.vendor), Style::default().fg(MUTED_GRAY)),
            Span::styled(status_badge.to_string(), status_style),
        ])
    }

    fn render_model_line(&self, model: &crate::app::state::AgentModel, is_selected: bool, is_disabled: bool) -> Line<'static> {
        let selection_indicator = if is_selected { "" } else { "   " };

        let name_style = if is_disabled {
            Style::default().fg(MUTED_GRAY)
        } else if is_selected {
            Style::default().fg(SELECTION_GREEN).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(SOFT_WHITE)
        };

        let desc_style = if is_disabled {
            Style::default().fg(MUTED_GRAY)
        } else {
            Style::default().fg(MUTED_GRAY)
        };

        let recommended = if model.is_recommended && !is_disabled {
            " (Recommended)"
        } else {
            ""
        };

        Line::from(vec![
            Span::styled("      ", Style::default()),
            Span::styled(selection_indicator.to_string(), Style::default().fg(SELECTION_GREEN)),
            Span::styled(" ", Style::default()),
            Span::styled(model.name.clone(), name_style),
            Span::styled(format!("      {}", model.description), desc_style),
            Span::styled(recommended.to_string(), Style::default().fg(SELECTION_GREEN)),
        ])
    }

    fn render_help_bar(&self, frame: &mut Frame, area: Rect) {
        let help_items = vec![
            ("Enter", "select"),
            ("↑↓", "navigate"),
            ("Tab", "expand/collapse"),
            ("Esc", "back"),
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

impl Default for AgentSelectionComponent {
    fn default() -> Self {
        Self::new()
    }
}
