// ABOUTME: Renderer-agnostic half of the `changelog` component: its
// state types and the logic that does not draw. The renderer lives in
// `ainb-core::components::changelog`, which re-exports this module.

use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Parser, Tag};

/// Embed the changelog at compile time
const CHANGELOG_CONTENT: &str = include_str!("../../../../CHANGELOG.md");

/// A line of rendered markdown content
#[derive(serde::Serialize, Debug, Clone)]
#[cfg_attr(feature = "typescript-bindings", derive(specta::Type))]
pub struct ChangelogLine {
    pub content: String,
    pub style: ChangelogStyle,
}

/// Styling categories for markdown content
#[derive(serde::Serialize, Debug, Clone, PartialEq)]
#[cfg_attr(feature = "typescript-bindings", derive(specta::Type))]
pub enum ChangelogStyle {
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
#[derive(serde::Serialize, Debug, Clone)]
#[cfg_attr(feature = "typescript-bindings", derive(specta::Type))]
pub struct ChangelogState {
    /// Parsed markdown lines
    pub lines: Vec<ChangelogLine>,
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
    fn parse_markdown(content: &str) -> Vec<ChangelogLine> {
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
                        lines.push(ChangelogLine {
                            content: current_text.clone(),
                            style: ChangelogStyle::Paragraph,
                        });
                        current_text.clear();
                    }

                    match tag {
                        Tag::Heading(level, _, _) => {
                            // Add blank line before headings (except first)
                            if !lines.is_empty() {
                                lines.push(ChangelogLine {
                                    content: String::new(),
                                    style: ChangelogStyle::Paragraph,
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
                                lines.push(ChangelogLine {
                                    content: format!("┌─ [{}] ", l.to_uppercase()),
                                    style: ChangelogStyle::CodeBlockHeader(l.clone()),
                                });
                            } else {
                                lines.push(ChangelogLine {
                                    content: "┌────────────────────".to_string(),
                                    style: ChangelogStyle::CodeBlock,
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
                                    HeadingLevel::H1 => ChangelogStyle::Heading1,
                                    HeadingLevel::H2 => ChangelogStyle::Heading2,
                                    _ => ChangelogStyle::Heading3,
                                };

                                // Add decorative prefix for version headers
                                let prefix = match level {
                                    HeadingLevel::H1 => "═══ ",
                                    HeadingLevel::H2 => "── ",
                                    _ => "• ",
                                };

                                lines.push(ChangelogLine {
                                    content: format!("{}{}", prefix, current_text.trim()),
                                    style,
                                });
                                current_text.clear();
                            }
                        }
                        Tag::Paragraph => {
                            if !current_text.is_empty() && !in_code_block {
                                lines.push(ChangelogLine {
                                    content: current_text.clone(),
                                    style: ChangelogStyle::Paragraph,
                                });
                                current_text.clear();
                            }
                        }
                        Tag::CodeBlock(_) => {
                            // Add any remaining code content
                            if !current_text.is_empty() {
                                for code_line in current_text.lines() {
                                    lines.push(ChangelogLine {
                                        content: format!("│ {}", code_line),
                                        style: ChangelogStyle::CodeBlock,
                                    });
                                }
                                current_text.clear();
                            }
                            lines.push(ChangelogLine {
                                content: "└────────────────────".to_string(),
                                style: ChangelogStyle::CodeBlock,
                            });
                            in_code_block = false;
                        }
                        Tag::List(_) => {
                            list_depth = list_depth.saturating_sub(1);
                        }
                        Tag::Item => {
                            if !current_text.is_empty() {
                                let indent = "  ".repeat(list_depth.saturating_sub(1));
                                lines.push(ChangelogLine {
                                    content: format!("{}• {}", indent, current_text.trim()),
                                    style: ChangelogStyle::ListItem,
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
            lines.push(ChangelogLine {
                content: current_text,
                style: ChangelogStyle::Paragraph,
            });
        }

        lines
    }
}
