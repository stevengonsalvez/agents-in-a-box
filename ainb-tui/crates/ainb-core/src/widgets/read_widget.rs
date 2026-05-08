// ABOUTME: Widget for rendering Read tool calls showing file content operations
// Displays file paths and content previews with line numbers

use super::{MessageWidget, WidgetOutput, ToolResult, helpers, result_parser};
use crate::agent_parsers::AgentEvent;
use crate::components::live_logs_stream::{LogEntry, LogEntryLevel};
use uuid::Uuid;

pub struct ReadWidget;

impl ReadWidget {
    pub fn new() -> Self {
        Self
    }

    /// Format file path with icon based on extension
    fn format_file_path(path: &str) -> String {
        let icon = if path.ends_with(".rs") {
            "🦀"
        } else if path.ends_with(".py") {
            "🐍"
        } else if path.ends_with(".js") || path.ends_with(".ts") || path.ends_with(".jsx") || path.ends_with(".tsx") {
            "📜"
        } else if path.ends_with(".json") {
            "📋"
        } else if path.ends_with(".md") {
            "📝"
        } else if path.ends_with(".toml") || path.ends_with(".yaml") || path.ends_with(".yml") {
            "⚙️"
        } else if path.ends_with(".sh") || path.ends_with(".bash") {
            "🖥️"
        } else {
            "📄"
        };

        format!("{} {}", icon, path)
    }

    /// Detect file type based on extension for syntax context
    fn detect_file_type(file_path: &str) -> Option<String> {
        if file_path.ends_with(".rs") {
            Some("rust".to_string())
        } else if file_path.ends_with(".py") {
            Some("python".to_string())
        } else if file_path.ends_with(".js") || file_path.ends_with(".jsx") {
            Some("javascript".to_string())
        } else if file_path.ends_with(".ts") || file_path.ends_with(".tsx") {
            Some("typescript".to_string())
        } else if file_path.ends_with(".json") {
            Some("json".to_string())
        } else if file_path.ends_with(".md") || file_path.ends_with(".markdown") {
            Some("markdown".to_string())
        } else if file_path.ends_with(".toml") {
            Some("toml".to_string())
        } else if file_path.ends_with(".yaml") || file_path.ends_with(".yml") {
            Some("yaml".to_string())
        } else if file_path.ends_with(".sh") || file_path.ends_with(".bash") {
            Some("bash".to_string())
        } else if file_path.ends_with(".html") {
            Some("html".to_string())
        } else if file_path.ends_with(".css") {
            Some("css".to_string())
        } else if file_path.ends_with(".sql") {
            Some("sql".to_string())
        } else {
            None
        }
    }
}

impl MessageWidget for ReadWidget {
    fn can_handle(&self, event: &AgentEvent) -> bool {
        matches!(event, AgentEvent::ToolCall { name, .. } if name == "Read")
    }

    fn render(&self, event: AgentEvent, container_name: &str, session_id: Uuid) -> WidgetOutput {
        if let AgentEvent::ToolCall { id, name: _, input, description } = event {
            let mut entries = Vec::new();

            // Extract file path
            let file_path = input.get("file_path")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");

            // Header with file path
            let header = format!("📖 Reading: {}", Self::format_file_path(file_path));
            entries.push(
                helpers::create_log_entry(
                    LogEntryLevel::Info,
                    container_name,
                    header,
                    session_id,
                    "tool_call",
                )
                .with_metadata("tool_id", &id)
                .with_metadata("tool_name", "Read")
                .with_metadata("file_path", file_path)
            );

            // Add line range if specified
            if let Some(offset) = input.get("offset").and_then(|v| v.as_u64()) {
                let limit = input.get("limit").and_then(|v| v.as_u64()).unwrap_or(2000);
                let range_msg = format!("  📏 Lines {}-{}", offset + 1, offset + limit);
                entries.push(
                    LogEntry::new(
                        LogEntryLevel::Debug,
                        container_name.to_string(),
                        range_msg,
                    )
                    .with_session(session_id)
                    .with_metadata("event_type", "read_range")
                );
            }

            // Add description if present
            if let Some(desc) = description {
                if !desc.is_empty() {
                    entries.push(
                        LogEntry::new(
                            LogEntryLevel::Debug,
                            container_name.to_string(),
                            format!("  💭 {}", desc),
                        )
                        .with_session(session_id)
                        .with_metadata("event_type", "read_description")
                    );
                }
            }

            WidgetOutput::MultiLine(entries)
        } else {
            WidgetOutput::Simple(
                helpers::create_log_entry(
                    LogEntryLevel::Error,
                    container_name,
                    "Invalid event for ReadWidget".to_string(),
                    session_id,
                    "error",
                )
            )
        }
    }

    fn render_with_result(&self, event: AgentEvent, result: Option<ToolResult>, container_name: &str, session_id: Uuid) -> WidgetOutput {
        if let AgentEvent::ToolCall { id, name: _, input, description } = event {
            let mut header_entries = Vec::new();
            let mut content_entries = Vec::new();

            // Extract file path
            let file_path = input.get("file_path")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");

            // Header with file path
            let header = format!("📖 Reading: {}", Self::format_file_path(file_path));
            let header_entry = helpers::create_log_entry(
                LogEntryLevel::Info,
                container_name,
                header,
                session_id,
                "tool_call",
            )
            .with_metadata("tool_id", &id)
            .with_metadata("tool_name", "Read")
            .with_metadata("file_path", file_path);

            header_entries.push(header_entry);

            // Add line range if specified
            if let Some(offset) = input.get("offset").and_then(|v| v.as_u64()) {
                let limit = input.get("limit").and_then(|v| v.as_u64()).unwrap_or(2000);
                let range_msg = format!("📏 Lines {}-{}", offset + 1, offset + limit);
                let range_entry = LogEntry::new(
                    LogEntryLevel::Debug,
                    container_name.to_string(),
                    range_msg,
                )
                .with_session(session_id)
                .with_metadata("event_type", "read_range")
                .with_metadata("tool_id", &id);

                header_entries.push(range_entry);
            }

            // Add description if present
            if let Some(desc) = description {
                if !desc.is_empty() {
                    let desc_entry = LogEntry::new(
                        LogEntryLevel::Debug,
                        container_name.to_string(),
                        format!("💭 {}", desc),
                    )
                    .with_session(session_id)
                    .with_metadata("event_type", "read_description")
                    .with_metadata("tool_id", &id);

                    header_entries.push(desc_entry);
                }
            }

            // Process result if available
            if let Some(tool_result) = result {
                if let Some(content_str) = result_parser::format_tool_result(&tool_result.content) {
                    // Determine if file content should be treated as markdown based on extension
                    let is_markdown_file = file_path.ends_with(".md") || file_path.ends_with(".markdown");

                    // Check if content looks like structured markdown or if it's a markdown file
                    let should_parse_as_markdown = is_markdown_file ||
                        (content_str.contains('#') &&
                         (content_str.contains("```") || content_str.contains("*") || content_str.contains("-")));

                    if should_parse_as_markdown {
                        // Parse as markdown
                        let parsed_entries = result_parser::parse_markdown_to_logs(
                            &content_str,
                            container_name,
                            session_id,
                            if tool_result.is_error { LogEntryLevel::Error } else { LogEntryLevel::Info },
                        );
                        content_entries.extend(parsed_entries);
                    } else {
                        // Display as plain text with syntax awareness
                        let level = if tool_result.is_error {
                            LogEntryLevel::Error
                        } else {
                            LogEntryLevel::Info
                        };

                        // Detect file type for context
                        let file_type = Self::detect_file_type(file_path);

                        for (line_num, line) in content_str.lines().enumerate() {
                            // Add line numbers if content appears to be from a file read
                            let formatted_line = if content_str.lines().count() > 1 && line.trim().len() > 0 {
                                // Check if line already has line numbers (from Read tool output)
                                if line.chars().take(10).any(|c| c == '\t') {
                                    // Line already has line numbers, use as-is
                                    line.to_string()
                                } else {
                                    // Add simple line indicator
                                    format!("{}:{}", line_num + 1, line)
                                }
                            } else {
                                line.to_string()
                            };

                            let mut entry = LogEntry::new(
                                level,
                                container_name.to_string(),
                                formatted_line,
                            )
                            .with_session(session_id)
                            .with_metadata("file_content", "true");

                            if let Some(ft) = &file_type {
                                entry = entry.with_metadata("file_type", ft);
                            }

                            content_entries.push(entry);
                        }
                    }
                } else if tool_result.is_error {
                    // Error with no content
                    content_entries.push(
                        LogEntry::new(
                            LogEntryLevel::Error,
                            container_name.to_string(),
                            "❌ Failed to read file".to_string(),
                        )
                        .with_session(session_id)
                    );
                }

                // Return hierarchical output
                WidgetOutput::Hierarchical {
                    header: header_entries,
                    content: content_entries,
                    collapsed: false,
                }
            } else {
                // No result yet, just return the header
                WidgetOutput::MultiLine(header_entries)
            }
        } else {
            // Should not happen if can_handle works correctly
            WidgetOutput::Simple(
                helpers::create_log_entry(
                    LogEntryLevel::Error,
                    container_name,
                    "Invalid event for ReadWidget".to_string(),
                    session_id,
                    "error",
                )
            )
        }
    }

    fn name(&self) -> &'static str {
        "ReadWidget"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_read_widget_can_handle() {
        let widget = ReadWidget::new();

        let read_event = AgentEvent::ToolCall {
            id: "test".to_string(),
            name: "Read".to_string(),
            input: json!({}),
            description: None,
        };
        assert!(widget.can_handle(&read_event));

        let other_event = AgentEvent::ToolCall {
            id: "test".to_string(),
            name: "Write".to_string(),
            input: json!({}),
            description: None,
        };
        assert!(!widget.can_handle(&other_event));
    }

    #[test]
    fn test_read_widget_render() {
        let widget = ReadWidget::new();
        let event = AgentEvent::ToolCall {
            id: "read_123".to_string(),
            name: "Read".to_string(),
            input: json!({
                "file_path": "src/main.rs",
                "offset": 10,
                "limit": 50
            }),
            description: Some("Check main function".to_string()),
        };

        let output = widget.render(event, "test-container", Uuid::nil());

        match output {
            WidgetOutput::MultiLine(entries) => {
                assert!(!entries.is_empty());
                assert!(entries[0].message.contains("📖 Reading"));
                assert!(entries[0].message.contains("🦀")); // Rust file icon
                assert!(entries[0].message.contains("src/main.rs"));
            }
            _ => panic!("Expected MultiLine output"),
        }
    }

    #[test]
    fn test_read_widget_render_with_result() {
        let widget = ReadWidget::new();
        let event = AgentEvent::ToolCall {
            id: "read_123".to_string(),
            name: "Read".to_string(),
            input: json!({
                "file_path": "src/lib.rs"
            }),
            description: Some("Read library file".to_string()),
        };

        let result = ToolResult {
            tool_use_id: "read_123".to_string(),
            content: json!({
                "content": "1\tpub fn hello() {\n2\t    println!(\"Hello, world!\");\n3\t}"
            }),
            is_error: false,
        };

        let output = widget.render_with_result(event, Some(result), "test-container", Uuid::nil());

        match output {
            WidgetOutput::Hierarchical { header, content, .. } => {
                assert!(!header.is_empty());
                assert!(header[0].message.contains("📖 Reading"));
                assert!(header[0].message.contains("🦀")); // Rust file icon
                assert!(header[0].message.contains("src/lib.rs"));

                assert!(!content.is_empty());
                assert!(content.iter().any(|e| e.message.contains("hello()")));
            }
            _ => panic!("Expected Hierarchical output"),
        }
    }

    #[test]
    fn test_read_widget_render_with_markdown_result() {
        let widget = ReadWidget::new();
        let event = AgentEvent::ToolCall {
            id: "read_md".to_string(),
            name: "Read".to_string(),
            input: json!({
                "file_path": "README.md"
            }),
            description: None,
        };

        let result = ToolResult {
            tool_use_id: "read_md".to_string(),
            content: json!({
                "content": "# My Project\n\nThis is a **bold** statement.\n\n```rust\nfn main() {}\n```"
            }),
            is_error: false,
        };

        let output = widget.render_with_result(event, Some(result), "test-container", Uuid::nil());

        match output {
            WidgetOutput::Hierarchical { header, content, .. } => {
                assert!(!header.is_empty());
                assert!(header[0].message.contains("📖 Reading"));
                assert!(header[0].message.contains("📝")); // Markdown file icon

                assert!(!content.is_empty());
                // Should be parsed as markdown
                assert!(content.iter().any(|e| e.message.contains("# My Project")));
            }
            _ => panic!("Expected Hierarchical output"),
        }
    }

    #[test]
    fn test_detect_file_type() {
        assert_eq!(ReadWidget::detect_file_type("file.rs"), Some("rust".to_string()));
        assert_eq!(ReadWidget::detect_file_type("file.py"), Some("python".to_string()));
        assert_eq!(ReadWidget::detect_file_type("file.js"), Some("javascript".to_string()));
        assert_eq!(ReadWidget::detect_file_type("file.ts"), Some("typescript".to_string()));
        assert_eq!(ReadWidget::detect_file_type("file.json"), Some("json".to_string()));
        assert_eq!(ReadWidget::detect_file_type("file.md"), Some("markdown".to_string()));
        assert_eq!(ReadWidget::detect_file_type("file.txt"), None);
    }
}
