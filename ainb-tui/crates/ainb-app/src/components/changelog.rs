// ABOUTME: Renderer-agnostic half of the `changelog` component: the bundled
// changelog parsed into static lines. The renderer lives in
// `ainb-core::components::changelog`, which re-exports this module.

use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Parser, Tag};

/// The bundled changelog, embedded at compile time.
///
/// A remote renderer (the desktop host, the web client) draws the changelog
/// from this same text, never from a frame: it is static content, not state
/// (#1052).
pub const CHANGELOG_MARKDOWN: &str = include_str!("../../../../CHANGELOG.md");

/// The changelog parsed into rendered lines, once per process.
///
/// Where the viewer is scrolled is renderer-local (`UiState` in the ratatui
/// host, under the scroll seal), so neither the lines nor the scroll position
/// is app state and no frame carries either (#1052).
#[must_use]
pub fn changelog_lines() -> &'static [ChangelogLine] {
    static LINES: std::sync::OnceLock<Vec<ChangelogLine>> = std::sync::OnceLock::new();
    LINES.get_or_init(|| parse_markdown(CHANGELOG_MARKDOWN))
}

/// A line of rendered markdown content
#[derive(Debug, Clone)]
pub struct ChangelogLine {
    pub content: String,
    pub style: ChangelogStyle,
}

/// Styling categories for markdown content
#[derive(Debug, Clone, PartialEq, Eq)]
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bundled_changelog_parses_once_into_static_lines() {
        let lines = changelog_lines();
        assert!(lines.len() > 100, "the bundled changelog parses");
        assert!(
            std::ptr::eq(lines, changelog_lines()),
            "parsed once per process"
        );
    }
}
