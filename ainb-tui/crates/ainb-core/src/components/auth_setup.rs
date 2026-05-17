// ABOUTME: Auth setup component for first-time authentication configuration
// Allows users to choose between OAuth, API Key, or skip authentication

use crate::app::state::{AppState, AuthMethod, AuthSetupState};
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap},
};

pub struct AuthSetupComponent;

impl AuthSetupComponent {
    pub fn new() -> Self {
        Self
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, state: &AppState) {
        // Clear background
        frame.render_widget(Clear, area);

        // Get auth setup state
        let auth_state = match &state.auth_setup_state {
            Some(state) => state,
            None => return,
        };

        // Create centered box
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Blue))
            .title(" Claude-in-a-Box Setup ")
            .title_alignment(Alignment::Center);

        let inner = block.inner(area);
        frame.render_widget(block, area);

        // Split inner area into sections
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .margin(2)
            .constraints([
                Constraint::Length(3), // Title
                Constraint::Length(2), // Subtitle
                Constraint::Min(10),   // Options
                Constraint::Length(3), // Instructions
            ])
            .split(inner);

        // Title
        let title = Paragraph::new("Welcome to Claude-in-a-Box!")
            .style(Style::default().fg(Color::White).add_modifier(Modifier::BOLD))
            .alignment(Alignment::Center);
        frame.render_widget(title, chunks[0]);

        // Subtitle
        let subtitle = Paragraph::new("Choose your authentication method:")
            .style(Style::default().fg(Color::Gray))
            .alignment(Alignment::Center);
        frame.render_widget(subtitle, chunks[1]);

        // Render based on state
        if auth_state.is_processing {
            self.render_processing(frame, chunks[2]);
        } else if auth_state.selected_method == AuthMethod::ApiKey
            && !auth_state.api_key_input.is_empty()
        {
            self.render_api_key_input(frame, chunks[2], auth_state);
        } else {
            self.render_method_selection(frame, chunks[2], auth_state);
        }

        // Status/Error message if any
        if let Some(error_msg) = &auth_state.error_message {
            // Use different colors based on message type
            let color = if error_msg.contains("terminal opened")
                || error_msg.contains("Complete the login")
            {
                Color::Cyan // Informational message
            } else if error_msg.contains("Failed") || error_msg.contains("Could not") {
                Color::Red // Error message
            } else {
                Color::Yellow // Warning/instruction message
            };

            let message = Paragraph::new(error_msg.as_str())
                .style(Style::default().fg(color))
                .alignment(Alignment::Center)
                .wrap(Wrap { trim: true });
            frame.render_widget(message, chunks[3]);
        } else {
            // Instructions
            let instructions = match auth_state.selected_method {
                AuthMethod::OAuth if !auth_state.is_processing => {
                    "↑/↓: Navigate • Enter: Select • Esc: Cancel"
                }
                AuthMethod::ApiKey if auth_state.api_key_input.is_empty() => {
                    "↑/↓: Navigate • Enter: Select • Esc: Cancel"
                }
                AuthMethod::ApiKey => "Enter: Save API Key • Esc: Back",
                AuthMethod::Skip if !auth_state.is_processing => {
                    "↑/↓: Navigate • Enter: Select • Esc: Cancel"
                }
                _ => "",
            };

            let instructions_widget = Paragraph::new(instructions)
                .style(Style::default().fg(Color::DarkGray))
                .alignment(Alignment::Center);
            frame.render_widget(instructions_widget, chunks[3]);
        }
    }

    fn render_method_selection(&self, frame: &mut Frame, area: Rect, auth_state: &AuthSetupState) {
        let methods = vec![
            (
                "OAuth (Recommended)",
                "Authenticate via browser with your Claude account",
                AuthMethod::OAuth,
            ),
            ("API Key", "Use an Anthropic API key", AuthMethod::ApiKey),
            (
                "Skip for now",
                "Configure authentication later (per-container prompts)",
                AuthMethod::Skip,
            ),
        ];

        let items: Vec<ListItem> = methods
            .iter()
            .map(|(title, desc, method)| {
                let is_selected = auth_state.selected_method == *method;
                let style = if is_selected {
                    Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::White)
                };

                let prefix = if is_selected { "▶ " } else { "  " };
                let content = format!("{}{}\n  {}", prefix, title, desc);

                ListItem::new(content).style(style)
            })
            .collect();

        let list = List::new(items)
            .block(Block::default())
            .style(Style::default().fg(Color::White));

        frame.render_widget(list, area);
    }

    fn render_api_key_input(&self, frame: &mut Frame, area: Rect, auth_state: &AuthSetupState) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3), // Label
                Constraint::Length(3), // Input field
                Constraint::Min(1),    // Spacer
            ])
            .split(area);

        // Label
        let label = Paragraph::new("Enter your Anthropic API key:")
            .style(Style::default().fg(Color::White))
            .alignment(Alignment::Center);
        frame.render_widget(label, chunks[0]);

        // Input field with cursor
        let input_text = if auth_state.show_cursor {
            format!("{}│", auth_state.api_key_input)
        } else {
            auth_state.api_key_input.clone()
        };

        let input = Paragraph::new(input_text).style(Style::default().fg(Color::Yellow)).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Yellow)),
        );
        frame.render_widget(input, chunks[1]);
    }

    fn render_processing(&self, frame: &mut Frame, area: Rect) {
        let processing_msg = "🔄 Starting authentication container...\n\n\
                             Setting up OAuth authentication flow.\n\
                             Please wait...";

        let processing = Paragraph::new(processing_msg)
            .style(Style::default().fg(Color::Cyan))
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: true });

        frame.render_widget(processing, area);
    }
}
