// ABOUTME: Attached terminal component for full-screen container interaction

use ratatui::{
    layout::{Constraint, Direction, Layout},
    prelude::*,
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, Paragraph},
};

use crate::app::AppState;

pub struct AttachedTerminalComponent;

impl AttachedTerminalComponent {
    pub fn new() -> Self {
        Self
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, state: &AppState) {
        if let Some(session_id) = state.attached_session_id {
            self.render_attached_terminal(frame, area, state, session_id);
        } else {
            self.render_error_state(frame, area);
        }
    }

    fn render_attached_terminal(
        &self,
        frame: &mut Frame,
        area: Rect,
        state: &AppState,
        session_id: uuid::Uuid,
    ) {
        // Get session info
        let session =
            state.workspaces.iter().flat_map(|w| &w.sessions).find(|s| s.id == session_id);

        let (title, recent_logs) = if let Some(session) = session {
            (
                format!(
                    "Attached to: {} ({})",
                    session.name,
                    session_id.to_string()[..8].to_string()
                ),
                session.recent_logs.clone(),
            )
        } else {
            (
                format!(
                    "Attached to session: {}",
                    session_id.to_string()[..8].to_string()
                ),
                None,
            )
        };

        // Split the area for info and logs
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(12), Constraint::Min(0)])
            .split(area);

        // Display session information in top section
        let info_content = vec![
            "🔗 Session Container".to_string(),
            "".to_string(),
            "🚀 Claude CLI is auto-started and running in background!".to_string(),
            "".to_string(),
            "Actions:".to_string(),
            "  • Press [a] to attach to interactive shell".to_string(),
            "  • Press [k] to kill container".to_string(),
            "  • Press [Esc] to return to session list".to_string(),
            "".to_string(),
            "💡 In shell: Run 'claude-start' to attach to Claude immediately".to_string(),
        ];

        let info_text = info_content.join("\n");

        // Render info section
        let info_paragraph = Paragraph::new(info_text)
            .block(
                Block::default()
                    .title(title.clone())
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Green)),
            )
            .style(Style::default().fg(Color::White))
            .wrap(ratatui::widgets::Wrap { trim: true });

        frame.render_widget(info_paragraph, chunks[0]);

        // Render logs section
        let logs_content = if let Some(logs) = recent_logs {
            if logs.trim().is_empty() {
                "Claude CLI is starting up...\nLogs will appear here once Claude begins processing."
                    .to_string()
            } else {
                logs
            }
        } else {
            "Claude CLI is running but no logs fetched yet.\nLogs will appear here automatically."
                .to_string()
        };

        let logs_paragraph = Paragraph::new(logs_content)
            .block(
                Block::default()
                    .title("📄 Claude Output (Live)")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Blue)),
            )
            .style(Style::default().fg(Color::Gray))
            .wrap(ratatui::widgets::Wrap { trim: false });

        frame.render_widget(logs_paragraph, chunks[1]);

        // Add status bar at the bottom
        let status_area = Rect {
            x: area.x,
            y: area.y + area.height - 3,
            width: area.width,
            height: 3,
        };

        let status_text =
            "[a] Attach to Shell  |  [k] Kill Container  |  [Esc] Return to Session List";
        let status_paragraph = Paragraph::new(status_text)
            .block(
                Block::default()
                    .borders(Borders::TOP)
                    .border_style(Style::default().fg(Color::Yellow)),
            )
            .style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD))
            .alignment(Alignment::Center);

        frame.render_widget(status_paragraph, status_area);
    }

    fn render_error_state(&self, frame: &mut Frame, area: Rect) {
        let error_text = "Error: No attached session found";

        let paragraph = Paragraph::new(error_text)
            .block(
                Block::default()
                    .title("Terminal Error")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Red)),
            )
            .style(Style::default().fg(Color::Red))
            .alignment(Alignment::Center);

        frame.render_widget(paragraph, area);
    }
}

impl Default for AttachedTerminalComponent {
    fn default() -> Self {
        Self::new()
    }
}
