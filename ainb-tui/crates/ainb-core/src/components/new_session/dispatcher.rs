// ABOUTME: New-session dispatch component (post-Phase-6).
//
// Phase 6 (new-session redesign) retired the 13-step wizard that used to live
// here. This file is now a thin three-arm dispatcher (`PickRepo` / `Configure`
// / `Creating`) that delegates rendering to the sibling component modules
// (finding #10 — previously `legacy.rs`; renamed so the filename matches the
// actual responsibility).

use ratatui::{
    prelude::*,
    style::{Color, Modifier, Style},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
};

use crate::app::{AppState, state::NewSessionStep};

pub struct NewSessionComponent;

impl NewSessionComponent {
    pub fn new() -> Self {
        Self
    }

    pub fn render(&mut self, frame: &mut Frame, area: Rect, state: &AppState) {
        if let Some(ref session_state) = state.new_session_state {
            // Create a centered popup
            let popup_area = self.centered_rect(80, 70, area);

            // Clear the background
            frame.render_widget(Clear, popup_area);

            match session_state.step {
                NewSessionStep::PickRepo => {
                    if let Some(ref pick) = session_state.pick_repo_state {
                        super::pick_repo::render(frame, pick, popup_area);
                    }
                }
                NewSessionStep::Configure => {
                    if let Some(ref cfg) = session_state.configure_state {
                        super::configure::render(frame, cfg, popup_area);
                    }
                }
                NewSessionStep::Creating => self.render_creating(frame, popup_area),
            }
        }
    }

    /// Render the "Creating session…" panel shown while the async session
    /// build runs. Retained from the legacy flow because Phase 6 keeps the
    /// `Creating` step as the bridge between Enter-on-Configure and the
    /// session showing up in the sessions list.
    fn render_creating(&self, frame: &mut Frame, area: Rect) {
        let cornflower_blue = Color::Rgb(100, 149, 237);
        let dark_bg = Color::Rgb(25, 25, 35);
        let gold = Color::Rgb(255, 215, 0);
        let soft_white = Color::Rgb(220, 220, 230);
        let muted_gray = Color::Rgb(120, 120, 140);
        let progress_cyan = Color::Rgb(100, 200, 230);

        let background = Block::default().style(Style::default().bg(dark_bg));
        frame.render_widget(background, area);

        let title_line = Line::from(vec![
            Span::styled(" ⚙️  ", Style::default().fg(progress_cyan)),
            Span::styled(
                "Creating Session",
                Style::default().fg(gold).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" ", Style::default()),
        ]);

        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(cornflower_blue))
            .title(title_line)
            .title_alignment(Alignment::Center)
            .style(Style::default().bg(dark_bg));
        frame.render_widget(block.clone(), area);

        let inner = block.inner(area);

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .margin(1)
            .constraints([
                Constraint::Length(2),
                Constraint::Min(0),
                Constraint::Length(2),
            ])
            .split(inner);

        let subtitle = Paragraph::new(Line::from(vec![Span::styled(
            "Setting up your development environment...",
            Style::default().fg(muted_gray),
        )]))
        .alignment(Alignment::Center);
        frame.render_widget(subtitle, chunks[0]);

        let progress_lines = vec![
            Line::from(""),
            Line::from(vec![
                Span::styled("  🔄 ", Style::default().fg(progress_cyan)),
                Span::styled("Creating Git worktree", Style::default().fg(soft_white)),
                Span::styled(" ...", Style::default().fg(progress_cyan)),
            ]),
            Line::from(""),
            Line::from(vec![
                Span::styled("  🐳 ", Style::default().fg(cornflower_blue)),
                Span::styled(
                    "Initializing Docker container",
                    Style::default().fg(soft_white),
                ),
                Span::styled(" ...", Style::default().fg(cornflower_blue)),
            ]),
            Line::from(""),
            Line::from(vec![
                Span::styled("  📦 ", Style::default().fg(gold)),
                Span::styled(
                    "Mounting volumes and configuring environment",
                    Style::default().fg(soft_white),
                ),
            ]),
            Line::from(""),
            Line::from(""),
            Line::from(vec![Span::styled(
                "       This may take a moment...",
                Style::default().fg(muted_gray).add_modifier(Modifier::ITALIC),
            )]),
        ];

        let progress = Paragraph::new(progress_lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(Color::Rgb(60, 60, 80)))
                .style(Style::default().bg(dark_bg)),
        );
        frame.render_widget(progress, chunks[1]);

        let footer = Paragraph::new(Line::from(vec![
            Span::styled("⏳ ", Style::default().fg(progress_cyan)),
            Span::styled("Please wait", Style::default().fg(muted_gray)),
            Span::styled("  │  ", Style::default().fg(Color::Rgb(60, 60, 80))),
            Span::styled(
                "Esc",
                Style::default().fg(gold).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" Cancel", Style::default().fg(muted_gray)),
        ]))
        .alignment(Alignment::Center);
        frame.render_widget(footer, chunks[2]);
    }

    fn centered_rect(&self, percent_x: u16, percent_y: u16, r: Rect) -> Rect {
        let popup_layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Percentage((100 - percent_y) / 2),
                Constraint::Percentage(percent_y),
                Constraint::Percentage((100 - percent_y) / 2),
            ])
            .split(r);

        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage((100 - percent_x) / 2),
                Constraint::Percentage(percent_x),
                Constraint::Percentage((100 - percent_x) / 2),
            ])
            .split(popup_layout[1])[1]
    }
}

impl Default for NewSessionComponent {
    fn default() -> Self {
        Self::new()
    }
}
