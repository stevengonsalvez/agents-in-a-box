// ABOUTME: Logs viewer component for displaying container logs and session information

#![allow(dead_code)]

use ratatui::{
    prelude::*,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, ListItem, Paragraph, Wrap},
};

use crate::app::AppState;

pub struct LogsViewerComponent;

impl LogsViewerComponent {
    pub fn new() -> Self {
        Self
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, state: &AppState) {
        if let Some(session) = state.selected_session() {
            self.render_session_info(frame, area, state, session);
        } else {
            self.render_empty_state(frame, area);
        }
    }

    fn render_session_info(
        &self,
        frame: &mut Frame,
        area: Rect,
        _state: &AppState,
        session: &crate::models::Session,
    ) {
        // Flat single-line session info with pipe separators and status color
        let status_text = match &session.status {
            crate::models::SessionStatus::Running => "Running",
            crate::models::SessionStatus::Stopped => "Stopped",
            crate::models::SessionStatus::Idle => "Idle",
            crate::models::SessionStatus::Error(err) => err,
        };

        let status_color = match &session.status {
            crate::models::SessionStatus::Running => Color::Green,
            crate::models::SessionStatus::Idle => Color::Yellow,
            crate::models::SessionStatus::Stopped => Color::Gray,
            crate::models::SessionStatus::Error(_) => Color::Red,
        };

        // Build spans with colored status
        let info_spans = vec![
            Span::styled(" ", Style::default()),
            Span::styled(&session.name, Style::default().fg(Color::White)),
            Span::styled(" │ ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("{} {}", session.status.indicator(), status_text),
                Style::default().fg(status_color),
            ),
            Span::styled(" │ ", Style::default().fg(Color::DarkGray)),
            Span::styled(" ", Style::default().fg(Color::Cyan)),
            Span::styled(&session.branch_name, Style::default().fg(Color::Cyan)),
        ];

        let info_line = Line::from(info_spans);

        let info_paragraph = Paragraph::new(info_line)
            .block(
                Block::default()
                    .title("Session Info")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Cyan)),
            );

        frame.render_widget(info_paragraph, area);
    }

    fn render_empty_state(&self, frame: &mut Frame, area: Rect) {
        let paragraph = Paragraph::new("Select a session to view details and logs")
            .block(
                Block::default()
                    .title("Session Details")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Gray)),
            )
            .style(Style::default().fg(Color::Gray))
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: true });

        frame.render_widget(paragraph, area);
    }

    fn get_session_logs(
        &self,
        state: &AppState,
        session: &crate::models::Session,
    ) -> Vec<ListItem> {
        // First check if we have real logs for this session
        if let Some(logs) = state.logs.get(&session.id) {
            if !logs.is_empty() {
                return logs.iter().map(|log| ListItem::new(log.clone())).collect();
            }
        }

        // Fallback to status-based mock logs
        self.get_mock_logs(session)
    }

    fn get_mock_logs(&self, session: &crate::models::Session) -> Vec<ListItem> {
        match session.status {
            crate::models::SessionStatus::Running => vec![
                ListItem::new("Starting Claude Code environment...")
                    .style(Style::default().fg(Color::Blue)),
                ListItem::new("Loading MCP servers...").style(Style::default().fg(Color::Blue)),
                ListItem::new("✓ Connected to container claude-abc123")
                    .style(Style::default().fg(Color::Green)),
                ListItem::new("✓ Workspace mounted: /workspace")
                    .style(Style::default().fg(Color::Green)),
                ListItem::new("✓ Git worktree ready").style(Style::default().fg(Color::Green)),
                ListItem::new("Ready! Attached to container.")
                    .style(Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
                ListItem::new("").style(Style::default()),
                ListItem::new("> claude help").style(Style::default().fg(Color::Yellow)),
                ListItem::new("Available commands:").style(Style::default()),
                ListItem::new("  help     Show this help message").style(Style::default()),
                ListItem::new("  list     List files in workspace").style(Style::default()),
                ListItem::new("  run      Execute command").style(Style::default()),
            ],
            crate::models::SessionStatus::Stopped => vec![
                ListItem::new("Container stopped").style(Style::default().fg(Color::Gray)),
                ListItem::new("Last active: 2 minutes ago").style(Style::default().fg(Color::Gray)),
            ],
            crate::models::SessionStatus::Idle => vec![
                ListItem::new("Tmux session active").style(Style::default().fg(Color::Yellow)),
                ListItem::new("Claude CLI stopped").style(Style::default().fg(Color::Yellow)),
                ListItem::new("Press 'r' to restart Claude").style(Style::default().fg(Color::Cyan)),
            ],
            crate::models::SessionStatus::Error(ref err) => vec![
                ListItem::new("Starting Claude Code environment...")
                    .style(Style::default().fg(Color::Blue)),
                ListItem::new("Loading MCP servers...").style(Style::default().fg(Color::Blue)),
                ListItem::new(format!("✗ Error: {}", err))
                    .style(Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)),
                ListItem::new("Container failed to start").style(Style::default().fg(Color::Red)),
            ],
        }
    }
}

impl Default for LogsViewerComponent {
    fn default() -> Self {
        Self::new()
    }
}
