// ABOUTME: Markdown parsing and result formatting for TUI display
// Converts markdown content and tool results into TUI-friendly formatted text

use pulldown_cmark::{Parser, Event, Tag, CodeBlockKind};
use crate::components::live_logs_stream::{LogEntry, LogEntryLevel};
use uuid::Uuid;
use super::syntax_highlighter;

/// Parse markdown content and convert to log entries for TUI display
pub fn parse_markdown_to_logs(
    content: &str,
    container_name: &str,
    session_id: Uuid,
    base_level: LogEntryLevel,
) -> Vec<LogEntry> {
    let mut entries = Vec::new();
    let parser = Parser::new(content);

    let mut current_text = String::new();
    let mut in_code_block = false;
    let mut code_block_lang = None;
    let mut code_block_content = String::new();
    let mut list_depth: usize = 0;

    for event in parser {
        match event {
            Event::Start(tag) => {
                // Flush any accumulated text
                if !current_text.is_empty() {
                    entries.push(create_text_entry(
                        &current_text,
                        container_name,
                        session_id,
                        base_level,
                    ));
                    current_text.clear();
                }

                match tag {
                    Tag::Heading(_level, _, _) => {
                        // Add spacing before headings
                        if !entries.is_empty() {
                            entries.push(create_text_entry(
                                "",
                                container_name,
                                session_id,
                                LogEntryLevel::Debug,
                            ));
                        }
                    }
                    Tag::CodeBlock(kind) => {
                        in_code_block = true;
                        code_block_lang = match kind {
                            CodeBlockKind::Fenced(lang) => Some(lang.to_string()),
                            _ => None,
                        };
                        code_block_content.clear();
                    }
                    Tag::List(_) => {
                        list_depth += 1;
                    }
                    _ => {}
                }
            }

            Event::End(tag) => {
                match tag {
                    Tag::Heading(level, _, _) => {
                        if !current_text.is_empty() {
                            let prefix = match level {
                                pulldown_cmark::HeadingLevel::H1 => "# ",
                                pulldown_cmark::HeadingLevel::H2 => "## ",
                                pulldown_cmark::HeadingLevel::H3 => "### ",
                                pulldown_cmark::HeadingLevel::H4 => "#### ",
                                pulldown_cmark::HeadingLevel::H5 => "##### ",
                                pulldown_cmark::HeadingLevel::H6 => "###### ",
                            };
                            entries.push(create_text_entry(
                                &format!("{}{}", prefix, current_text),
                                container_name,
                                session_id,
                                LogEntryLevel::Info,
                            ));
                            current_text.clear();
                        }
                    }
                    Tag::Paragraph => {
                        if !current_text.is_empty() {
                            entries.push(create_text_entry(
                                &current_text,
                                container_name,
                                session_id,
                                base_level,
                            ));
                            current_text.clear();
                        }
                    }
                    Tag::CodeBlock(_) => {
                        in_code_block = false;
                        if !code_block_content.is_empty() {
                            entries.extend(format_code_block(
                                &code_block_content,
                                code_block_lang.as_deref(),
                                container_name,
                                session_id,
                            ));
                        }
                        code_block_content.clear();
                        code_block_lang = None;
                    }
                    Tag::List(_) => {
                        list_depth = list_depth.saturating_sub(1);
                    }
                    Tag::Item => {
                        if !current_text.is_empty() {
                            let indent = "  ".repeat(list_depth.saturating_sub(1));
                            let formatted = format!("{}• {}", indent, current_text);
                            entries.push(create_text_entry(
                                &formatted,
                                container_name,
                                session_id,
                                base_level,
                            ));
                            current_text.clear();
                        }
                    }
                    Tag::Strong => {
                        // Add asterisks for bold in TUI
                        current_text = format!("*{}*", current_text);
                    }
                    Tag::Emphasis => {
                        // Add underscores for italic in TUI
                        current_text = format!("_{}_", current_text);
                    }
                    _ => {}
                }
            }

            Event::Text(text) => {
                if in_code_block {
                    code_block_content.push_str(&text);
                } else {
                    current_text.push_str(&text);
                }
            }

            Event::Code(code) => {
                current_text.push_str(&format!("`{}`", code));
            }

            Event::SoftBreak | Event::HardBreak => {
                if !in_code_block {
                    current_text.push(' ');
                } else {
                    code_block_content.push('\n');
                }
            }

            _ => {}
        }
    }

    // Flush any remaining text
    if !current_text.is_empty() {
        entries.push(create_text_entry(
            &current_text,
            container_name,
            session_id,
            base_level,
        ));
    }

    entries
}

/// Format a code block for TUI display
fn format_code_block(
    code: &str,
    language: Option<&str>,
    container_name: &str,
    session_id: Uuid,
) -> Vec<LogEntry> {
    let mut entries = Vec::new();

    // Add code block header with language badge if specified
    if let Some(lang) = language {
        let badge = syntax_highlighter::language_badge(lang);
        entries.push(
            LogEntry::new(
                LogEntryLevel::Debug,
                container_name.to_string(),
                format!("  {}", badge),
            )
            .with_session(session_id)
            .with_metadata("code_lang", lang)
        );
    }

    // Add top border
    entries.push(
        LogEntry::new(
            LogEntryLevel::Debug,
            container_name.to_string(),
            "  ┌────────────────────────────────────────".to_string(),
        )
        .with_session(session_id)
        .with_metadata("code_border", "top")
    );

    // Check if we should use highlighting (only for known languages)
    let use_highlighting = language.is_some() &&
        std::env::var("NO_COLOR").is_err() && // Respect NO_COLOR env var
        std::env::var("AGENTS_BOX_SYNTAX_HIGHLIGHT").unwrap_or_else(|_| "true".to_string()) == "true";

    // Format code lines with optional syntax highlighting
    let formatted_lines = if use_highlighting {
        syntax_highlighter::format_code_block(code, language, 1, true)
    } else {
        // Simple formatting without highlighting
        code.lines()
            .enumerate()
            .map(|(i, line)| format!("  │ {:>3} │ {}", i + 1, line))
            .collect()
    };

    // Add formatted code lines
    for line in formatted_lines {
        entries.push(
            LogEntry::new(
                LogEntryLevel::Debug,
                container_name.to_string(),
                format!("  {}", line),
            )
            .with_session(session_id)
            .with_metadata("code_line", "true")
        );
    }

    // Add bottom border
    entries.push(
        LogEntry::new(
            LogEntryLevel::Debug,
            container_name.to_string(),
            "  └────────────────────────────────────────".to_string(),
        )
        .with_session(session_id)
        .with_metadata("code_border", "bottom")
    );

    entries
}

/// Create a text log entry
fn create_text_entry(
    text: &str,
    container_name: &str,
    session_id: Uuid,
    level: LogEntryLevel,
) -> LogEntry {
    LogEntry::new(level, container_name.to_string(), text.to_string())
        .with_session(session_id)
        .with_metadata("markdown", "true")
}

/// Extract and format tool result content
pub fn format_tool_result(result: &serde_json::Value) -> Option<String> {
    // Try to extract content from various result formats
    if let Some(content) = result.get("content") {
        if let Some(text) = content.as_str() {
            return Some(text.to_string());
        } else if let Some(text_obj) = content.get("text") {
            if let Some(text) = text_obj.as_str() {
                return Some(text.to_string());
            }
        } else if let Some(array) = content.as_array() {
            let text_parts: Vec<String> = array
                .iter()
                .filter_map(|item| {
                    if let Some(s) = item.as_str() {
                        Some(s.to_string())
                    } else if let Some(text) = item.get("text").and_then(|t| t.as_str()) {
                        Some(text.to_string())
                    } else {
                        None
                    }
                })
                .collect();

            if !text_parts.is_empty() {
                return Some(text_parts.join("\n"));
            }
        }
    }

    // Fallback to raw JSON if we can't extract structured content
    if result.is_object() && !result.as_object().unwrap().is_empty() {
        Some(serde_json::to_string_pretty(result).unwrap_or_default())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_parse_simple_markdown() {
        let markdown = "# Hello World\n\nThis is a paragraph.";
        let entries = parse_markdown_to_logs(
            markdown,
            "test",
            Uuid::nil(),
            LogEntryLevel::Info,
        );

        assert!(!entries.is_empty());
        assert!(entries.iter().any(|e| e.message.contains("# Hello World")));
        assert!(entries.iter().any(|e| e.message.contains("This is a paragraph")));
    }

    #[test]
    fn test_parse_code_block() {
        let markdown = "```rust\nfn main() {\n    println!(\"Hello\");\n}\n```";
        let entries = parse_markdown_to_logs(
            markdown,
            "test",
            Uuid::nil(),
            LogEntryLevel::Info,
        );

        // Now uses language badges instead of ```rust
        assert!(entries.iter().any(|e| e.message.contains("[RUST]")));
        // Code is formatted with ANSI colors, line numbers and borders
        assert!(entries.iter().any(|e| e.message.contains("main")));
        assert!(entries.iter().any(|e| e.message.contains("println")));
        assert!(entries.iter().any(|e| e.message.contains("Hello")));
    }

    #[test]
    fn test_format_tool_result() {
        let result = json!({
            "content": "Test output"
        });

        let formatted = format_tool_result(&result);
        assert_eq!(formatted, Some("Test output".to_string()));

        let nested_result = json!({
            "content": {
                "text": "Nested text"
            }
        });

        let formatted = format_tool_result(&nested_result);
        assert_eq!(formatted, Some("Nested text".to_string()));
    }
}
