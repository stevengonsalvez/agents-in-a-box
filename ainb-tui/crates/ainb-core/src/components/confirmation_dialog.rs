// ABOUTME: Confirmation dialog component for displaying yes/no prompts with keyboard navigation

use crate::app::state::AppState;
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};

pub struct ConfirmationDialogComponent;

impl ConfirmationDialogComponent {
    pub fn new() -> Self {
        Self
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, state: &AppState) {
        if let Some(dialog) = &state.confirmation_dialog {
            // Calculate dialog size (center it)
            // Increase height if warning is present
            let dialog_width = 60.min(area.width - 4);
            let dialog_height = if dialog.warning.is_some() { 11 } else { 8 };

            let dialog_area = Rect {
                x: (area.width - dialog_width) / 2,
                y: (area.height - dialog_height) / 2,
                width: dialog_width,
                height: dialog_height,
            };

            // Clear ONLY the dialog area, not the entire screen
            // This prevents ghost/duplicate UI elements from appearing
            frame.render_widget(Clear, dialog_area);

            // Render dialog background
            let block = Block::default()
                .title(dialog.title.clone())
                .borders(Borders::ALL)
                .style(Style::default().bg(Color::Black));

            frame.render_widget(block, dialog_area);

            // Create inner layout
            let inner_area = Rect {
                x: dialog_area.x + 1,
                y: dialog_area.y + 1,
                width: dialog_area.width - 2,
                height: dialog_area.height - 2,
            };

            // Build constraints based on whether warning is present
            let constraints = if dialog.warning.is_some() {
                vec![
                    Constraint::Length(3), // Warning
                    Constraint::Min(1),    // Message
                    Constraint::Length(2), // Buttons
                ]
            } else {
                vec![
                    Constraint::Min(1),    // Message
                    Constraint::Length(2), // Buttons
                ]
            };

            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints(constraints)
                .split(inner_area);

            // Determine which chunk indices to use based on warning presence
            let (message_chunk, button_chunk) = if let Some(warning_text) = &dialog.warning {
                // Render warning with yellow/orange highlight
                let warning = Paragraph::new(warning_text.as_str())
                    .style(
                        Style::default()
                            .fg(Color::Black)
                            .bg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    )
                    .alignment(Alignment::Center)
                    .wrap(Wrap { trim: true });

                frame.render_widget(warning, chunks[0]);
                (1, 2)
            } else {
                (0, 1)
            };

            // Render message
            let message = Paragraph::new(dialog.message.clone())
                .wrap(Wrap { trim: true })
                .style(Style::default().fg(Color::White));

            frame.render_widget(message, chunks[message_chunk]);

            // Render buttons. Tri-option mode (e.g. Stop / Delete / Cancel) takes
            // precedence; binary mode is used by all legacy callsites.
            if let Some(options) = dialog.options.as_ref() {
                let n = options.len().max(1);
                let pct = (100u16 / n as u16).max(1);
                let constraints: Vec<Constraint> =
                    (0..n).map(|_| Constraint::Percentage(pct)).collect();
                let button_chunks = Layout::default()
                    .direction(Direction::Horizontal)
                    .constraints(constraints)
                    .split(chunks[button_chunk]);

                for (i, opt) in options.iter().enumerate() {
                    let style = if i == dialog.selected_index {
                        Style::default().fg(Color::Black).bg(Color::White)
                    } else {
                        Style::default().fg(Color::White)
                    };
                    let label = format!("[ {} ]", opt.label);
                    let button = Paragraph::new(label).style(style).alignment(Alignment::Center);
                    if let Some(area) = button_chunks.get(i) {
                        frame.render_widget(button, *area);
                    }
                }
            } else {
                let button_chunks = Layout::default()
                    .direction(Direction::Horizontal)
                    .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
                    .split(chunks[button_chunk]);

                // Yes button
                let yes_style = if dialog.selected_option {
                    Style::default().fg(Color::Black).bg(Color::White)
                } else {
                    Style::default().fg(Color::White)
                };

                let yes_button =
                    Paragraph::new("Yes").style(yes_style).alignment(Alignment::Center);

                frame.render_widget(yes_button, button_chunks[0]);

                // No button
                let no_style = if !dialog.selected_option {
                    Style::default().fg(Color::Black).bg(Color::White)
                } else {
                    Style::default().fg(Color::White)
                };

                let no_button = Paragraph::new("No").style(no_style).alignment(Alignment::Center);

                frame.render_widget(no_button, button_chunks[1]);
            }
        }
    }
}
