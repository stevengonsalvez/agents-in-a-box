// ABOUTME: Changelog viewer component - displays embedded CHANGELOG.md with markdown rendering
// Accessible via 'v' key from home screen or sidebar

use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Parser, Tag};
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

/// Embed the changelog at compile time
const CHANGELOG_CONTENT: &str = include_str!("../../../../CHANGELOG.md");

/// A line of rendered markdown content
#[derive(Debug, Clone)]
pub struct MarkdownLine {
    pub content: String,
    pub style: MarkdownStyle,
}

/// Styling categories for markdown content
#[derive(Debug, Clone, PartialEq)]
pub enum MarkdownStyle {
    Heading1,
    Heading2,
    Heading3,
    Paragraph,
    CodeBlock,
    CodeBlockHeader(String),
    ListItem,
    Bold,
    BlockQuote,
}

/// State for the changelog viewer
#[derive(Debug, Clone)]
pub struct ChangelogState {
    /// Parsed markdown lines
    pub lines: Vec<MarkdownLine>,
    /// Current scroll offset
    pub scroll_offset: usize,
    /// Total number of lines
    pub total_lines: usize,
}

impl Default for ChangelogState {
    fn default() -> Self {
        Self::new()
    }
}

impl ChangelogState {
    /// Create a new changelog state with embedded content
    pub fn new() -> Self {
        let lines = Self::parse_markdown(CHANGELOG_CONTENT);
        let total_lines = lines.len();

        Self {
            lines,
            scroll_offset: 0,
            total_lines,
        }
    }

    /// Scroll up by one line
    pub fn scroll_up(&mut self) {
        self.scroll_offset = self.scroll_offset.saturating_sub(1);
    }

    /// Scroll down by one line
    pub fn scroll_down(&mut self, visible_height: usize) {
        let max_scroll = self.total_lines.saturating_sub(visible_height);
        if self.scroll_offset < max_scroll {
            self.scroll_offset += 1;
        }
    }

    /// Scroll up by a page
    pub fn page_up(&mut self, visible_height: usize) {
        self.scroll_offset = self.scroll_offset.saturating_sub(visible_height);
    }

    /// Scroll down by a page
    pub fn page_down(&mut self, visible_height: usize) {
        let max_scroll = self.total_lines.saturating_sub(visible_height);
        self.scroll_offset = (self.scroll_offset + visible_height).min(max_scroll);
    }

    /// Jump to top
    pub fn scroll_to_top(&mut self) {
        self.scroll_offset = 0;
    }

    /// Jump to bottom
    pub fn scroll_to_bottom(&mut self, visible_height: usize) {
        self.scroll_offset = self.total_lines.saturating_sub(visible_height);
    }

    /// Parse markdown content into styled lines
    fn parse_markdown(content: &str) -> Vec<MarkdownLine> {
        let mut lines = Vec::new();
        let parser = Parser::new(content);

        let mut current_text = String::new();
        let mut in_code_block = false;
        let mut current_heading_level: Option<HeadingLevel> = None;
        let mut list_depth: usize = 0;

        for event in parser {
            match event {
                Event::Start(tag) => {
                    // Flush accumulated text
                    if !current_text.is_empty() && !in_code_block {
                        lines.push(MarkdownLine {
                            content: current_text.clone(),
                            style: MarkdownStyle::Paragraph,
                        });
                        current_text.clear();
                    }

                    match tag {
                        Tag::Heading(level, _, _) => {
                            // Add blank line before headings (except first)
                            if !lines.is_empty() {
                                lines.push(MarkdownLine {
                                    content: String::new(),
                                    style: MarkdownStyle::Paragraph,
                                });
                            }
                            current_heading_level = Some(level);
                            current_text.clear();
                        }
                        Tag::CodeBlock(kind) => {
                            in_code_block = true;
                            let lang = match kind {
                                CodeBlockKind::Fenced(lang) => {
                                    let lang_str = lang.to_string();
                                    if !lang_str.is_empty() {
                                        Some(lang_str)
                                    } else {
                                        None
                                    }
                                }
                                _ => None,
                            };
                            if let Some(ref l) = lang {
                                lines.push(MarkdownLine {
                                    content: format!("┌─ [{}] ", l.to_uppercase()),
                                    style: MarkdownStyle::CodeBlockHeader(l.clone()),
                                });
                            } else {
                                lines.push(MarkdownLine {
                                    content: "┌────────────────────".to_string(),
                                    style: MarkdownStyle::CodeBlock,
                                });
                            }
                        }
                        Tag::List(_) => {
                            list_depth += 1;
                        }
                        Tag::BlockQuote => {}
                        _ => {}
                    }
                }

                Event::End(tag) => {
                    match tag {
                        Tag::Heading(..) => {
                            if let Some(level) = current_heading_level.take() {
                                let style = match level {
                                    HeadingLevel::H1 => MarkdownStyle::Heading1,
                                    HeadingLevel::H2 => MarkdownStyle::Heading2,
                                    _ => MarkdownStyle::Heading3,
                                };

                                // Add decorative prefix for version headers
                                let prefix = match level {
                                    HeadingLevel::H1 => "═══ ",
                                    HeadingLevel::H2 => "── ",
                                    _ => "• ",
                                };

                                lines.push(MarkdownLine {
                                    content: format!("{}{}", prefix, current_text.trim()),
                                    style,
                                });
                                current_text.clear();
                            }
                        }
                        Tag::Paragraph => {
                            if !current_text.is_empty() && !in_code_block {
                                lines.push(MarkdownLine {
                                    content: current_text.clone(),
                                    style: MarkdownStyle::Paragraph,
                                });
                                current_text.clear();
                            }
                        }
                        Tag::CodeBlock(_) => {
                            // Add any remaining code content
                            if !current_text.is_empty() {
                                for code_line in current_text.lines() {
                                    lines.push(MarkdownLine {
                                        content: format!("│ {}", code_line),
                                        style: MarkdownStyle::CodeBlock,
                                    });
                                }
                                current_text.clear();
                            }
                            lines.push(MarkdownLine {
                                content: "└────────────────────".to_string(),
                                style: MarkdownStyle::CodeBlock,
                            });
                            in_code_block = false;
                        }
                        Tag::List(_) => {
                            list_depth = list_depth.saturating_sub(1);
                        }
                        Tag::Item => {
                            if !current_text.is_empty() {
                                let indent = "  ".repeat(list_depth.saturating_sub(1));
                                lines.push(MarkdownLine {
                                    content: format!("{}• {}", indent, current_text.trim()),
                                    style: MarkdownStyle::ListItem,
                                });
                                current_text.clear();
                            }
                        }
                        _ => {}
                    }
                }

                Event::Text(text) => {
                    current_text.push_str(&text);
                }

                Event::Code(code) => {
                    current_text.push('`');
                    current_text.push_str(&code);
                    current_text.push('`');
                }

                Event::SoftBreak | Event::HardBreak => {
                    if in_code_block {
                        current_text.push('\n');
                    }
                }

                _ => {}
            }
        }

        // Flush any remaining text
        if !current_text.is_empty() {
            lines.push(MarkdownLine {
                content: current_text,
                style: MarkdownStyle::Paragraph,
            });
        }

        lines
    }
}

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
