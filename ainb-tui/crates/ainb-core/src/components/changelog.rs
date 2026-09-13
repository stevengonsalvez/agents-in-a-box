// ABOUTME: Changelog viewer component - displays embedded CHANGELOG.md with markdown rendering
// Accessible via 'v' key from home screen or sidebar

pub use ainb_app::components::changelog::*;

use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Paragraph, Wrap},
};

// Color palette from TUI style guide
const CORNFLOWER_BLUE: Color = Color::Rgb(100, 149, 237);
const GOLD: Color = Color::Rgb(255, 215, 0);
const SELECTION_GREEN: Color = Color::Rgb(100, 200, 100);
const DARK_BG: Color = Color::Rgb(25, 25, 35);
const SOFT_WHITE: Color = Color::Rgb(220, 220, 230);
const MUTED_GRAY: Color = Color::Rgb(120, 120, 140);
const PROGRESS_CYAN: Color = Color::Rgb(100, 200, 230);

/// Changelog viewer component
pub struct ChangelogComponent;

impl ChangelogComponent {
    /// Render the changelog view
    pub fn render(frame: &mut Frame, area: Rect, state: &ChangelogState) {
        // Calculate visible lines
        let content_height = area.height.saturating_sub(2) as usize; // Account for borders
        let start_line = state.scroll_offset;
        let end_line = (start_line + content_height).min(state.lines.len());

        // Colors
        let heading1_color = PROGRESS_CYAN;
        let heading2_color = CORNFLOWER_BLUE;
        let heading3_color = Color::Rgb(150, 150, 220);
        let code_bg = Color::Rgb(35, 35, 45);
        let code_fg = SELECTION_GREEN;

        let visible_lines: Vec<Line> = state.lines[start_line..end_line]
            .iter()
            .map(|md_line| {
                let style = match &md_line.style {
                    MarkdownStyle::Heading1 => Style::default()
                        .fg(heading1_color)
                        .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
                    MarkdownStyle::Heading2 => {
                        Style::default().fg(heading2_color).add_modifier(Modifier::BOLD)
                    }
                    MarkdownStyle::Heading3 => {
                        Style::default().fg(heading3_color).add_modifier(Modifier::BOLD)
                    }
                    MarkdownStyle::Paragraph => Style::default().fg(SOFT_WHITE),
                    MarkdownStyle::CodeBlock => Style::default().fg(code_fg).bg(code_bg),
                    MarkdownStyle::CodeBlockHeader(_) => {
                        Style::default().fg(GOLD).add_modifier(Modifier::BOLD)
                    }
                    MarkdownStyle::ListItem => Style::default().fg(SOFT_WHITE),
                    MarkdownStyle::Bold => {
                        Style::default().fg(SOFT_WHITE).add_modifier(Modifier::BOLD)
                    }
                    MarkdownStyle::BlockQuote => {
                        Style::default().fg(MUTED_GRAY).add_modifier(Modifier::ITALIC)
                    }
                };

                Line::from(Span::styled(md_line.content.clone(), style))
            })
            .collect();

        // Scroll indicator
        let scroll_info = format!(
            " [{}/{}] ",
            state.scroll_offset + 1,
            state.total_lines.max(1)
        );

        let changelog_paragraph = Paragraph::new(visible_lines)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(CORNFLOWER_BLUE))
                    .style(Style::default().bg(DARK_BG))
                    .title(Line::from(vec![
                        Span::styled(" 📝 ", Style::default().fg(GOLD)),
                        Span::styled(
                            "Changelog",
                            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(scroll_info, Style::default().fg(MUTED_GRAY)),
                    ]))
                    .title_bottom(Line::from(vec![
                        Span::styled(
                            " ↑↓",
                            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(" scroll ", Style::default().fg(MUTED_GRAY)),
                        Span::styled("│", Style::default().fg(Color::Rgb(60, 60, 80))),
                        Span::styled(
                            " PgUp/Dn",
                            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(" page ", Style::default().fg(MUTED_GRAY)),
                        Span::styled("│", Style::default().fg(Color::Rgb(60, 60, 80))),
                        Span::styled(
                            " g/G",
                            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(" top/bottom ", Style::default().fg(MUTED_GRAY)),
                        Span::styled("│", Style::default().fg(Color::Rgb(60, 60, 80))),
                        Span::styled(
                            " Esc",
                            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(" back ", Style::default().fg(MUTED_GRAY)),
                    ])),
            )
            .wrap(Wrap { trim: false });

        frame.render_widget(changelog_paragraph, area);
    }
}
