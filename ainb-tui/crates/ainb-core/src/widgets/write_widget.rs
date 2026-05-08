// ABOUTME: Widget for rendering Write tool calls showing file creation/update operations
// Displays file paths and content previews

use super::{MessageWidget, WidgetOutput, ToolResult, helpers, result_parser};
use crate::agent_parsers::AgentEvent;
use crate::components::live_logs_stream::{LogEntry, LogEntryLevel};
use uuid::Uuid;

pub struct WriteWidget;

impl WriteWidget {
    pub fn new() -> Self {
        Self
    }

    /// Get file type icon
    fn get_file_icon(path: &str) -> &'static str {
        if path.ends_with(".rs") {
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
        } else {
            "📄"
        }
    }

    /// Create a preview of content
    fn create_preview(content: &str, max_lines: usize) -> Vec<String> {
        let lines: Vec<&str> = content.lines().take(max_lines).collect();
        let total_lines = content.lines().count();

        let mut preview = Vec::new();
        for (i, line) in lines.iter().enumerate() {
            if line.len() > 80 {
                preview.push(format!("    {} {}...", i + 1, &line[..80]));
            } else {
                preview.push(format!("    {} {}", i + 1, line));
            }
        }

        if total_lines > max_lines {
            preview.push(format!("    ... ({} more lines)", total_lines - max_lines));
        }

        preview
    }
}

impl MessageWidget for WriteWidget {
    fn can_handle(&self, event: &AgentEvent) -> bool {
        matches!(event, AgentEvent::ToolCall { name, .. } if name == "Write")
    }

    fn render(&self, event: AgentEvent, container_name: &str, session_id: Uuid) -> WidgetOutput {
        if let AgentEvent::ToolCall { id, name: _, input, description } = event {
            let mut entries = Vec::new();

            // Extract file path and content
            let file_path = input.get("file_path")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");

            let content = input.get("content")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            // Determine if this is creating a new file or updating
            let action = if file_path.contains("new") || description.as_ref().map_or(false, |d| d.contains("creat")) {
                "✨ Creating"
            } else {
                "✏️ Writing"
            };

            // Header with file path
            let header = format!("{}: {} {}", action, Self::get_file_icon(file_path), file_path);
            entries.push(
                helpers::create_log_entry(
                    LogEntryLevel::Info,
                    container_name,
                    header,
                    session_id,
                    "tool_call",
                )
                .with_metadata("tool_id", &id)
                .with_metadata("tool_name", "Write")
                .with_metadata("file_path", file_path)
            );

            // Add content stats
            let lines = content.lines().count();
            let bytes = content.len();
            let stats_msg = format!("  📊 {} lines, {} bytes", lines, bytes);
            entries.push(
                LogEntry::new(
                    LogEntryLevel::Debug,
                    container_name.to_string(),
                    stats_msg,
                )
                .with_session(session_id)
                .with_metadata("event_type", "write_stats")
            );

            // Add content preview (first few lines)
            if !content.is_empty() {
                entries.push(
                    LogEntry::new(
                        LogEntryLevel::Debug,
                        container_name.to_string(),
                        "  📄 Preview:".to_string(),
                    )
                    .with_session(session_id)
                    .with_metadata("event_type", "write_preview")
                );

                for preview_line in Self::create_preview(content, 3) {
                    entries.push(
                        LogEntry::new(
                            LogEntryLevel::Debug,
                            container_name.to_string(),
                            preview_line,
                        )
                        .with_session(session_id)
                        .with_metadata("event_type", "write_content")
                    );
                }
            }

            WidgetOutput::MultiLine(entries)
        } else {
            WidgetOutput::Simple(
                helpers::create_log_entry(
                    LogEntryLevel::Error,
                    container_name,
                    "Invalid event for WriteWidget".to_string(),
                    session_id,
                    "error",
                )
            )
        }
    }

    fn render_with_result(&self, event: AgentEvent, result: Option<ToolResult>, container_name: &str, session_id: Uuid) -> WidgetOutput {
        if let AgentEvent::ToolCall { id, name: _, input, description: _ } = event {
            let mut header_entries = Vec::new();
            let mut content_entries = Vec::new();

            // Extract file path and content
            let file_path = input.get("file_path")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");

            let content = input.get("content")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            // Determine if this is creating a new file or updating
            let action_icon = if std::path::Path::new(file_path).exists() {
                "✏️"
            } else {
                "✨"
            };

            // Create header message
            let header_msg = format!("{} Write: {} {}", action_icon, Self::get_file_icon(file_path), file_path);
            let header_entry = helpers::create_log_entry(
                LogEntryLevel::Info,
                container_name,
                header_msg,
                session_id,
                "tool_call",
            )
            .with_metadata("tool_id", &id)
            .with_metadata("tool_name", "Write")
            .with_metadata("file_path", file_path);

            header_entries.push(header_entry);

            // Add content stats as part of header
            let lines = content.lines().count();
            let bytes = content.len();
            let stats_entry = LogEntry::new(
                LogEntryLevel::Info,
                container_name.to_string(),
                format!("📊 {} lines, {} bytes", lines, bytes),
            )
            .with_session(session_id)
            .with_metadata("event_type", "write_stats")
            .with_metadata("tool_id", &id);

            header_entries.push(stats_entry);

            // Process result if available
            if let Some(tool_result) = result {
                if tool_result.is_error {
                    // Show error
                    if let Some(content_str) = result_parser::format_tool_result(&tool_result.content) {
                        let error_lines: Vec<&str> = content_str.lines().collect();
                        for line in error_lines {
                            content_entries.push(
                                LogEntry::new(
                                    LogEntryLevel::Error,
                                    container_name.to_string(),
                                    format!("❌ {}", line),
                                )
                                .with_session(session_id)
                                .with_metadata("write_error", "true")
                            );
                        }
                    } else {
                        content_entries.push(
                            LogEntry::new(
                                LogEntryLevel::Error,
                                container_name.to_string(),
                                "❌ Write operation failed".to_string(),
                            )
                            .with_session(session_id)
                            .with_metadata("write_error", "true")
                        );
                    }
                } else {
                    // Show success
                    content_entries.push(
                        LogEntry::new(
                            LogEntryLevel::Info,
                            container_name.to_string(),
                            "✅ File written successfully".to_string(),
                        )
                        .with_session(session_id)
                        .with_metadata("write_success", "true")
                    );

                    // Show result details if available
                    if let Some(content_str) = result_parser::format_tool_result(&tool_result.content) {
                        if !content_str.is_empty() && content_str.trim() != "null" {
                            for line in content_str.lines() {
                                if !line.is_empty() {
                                    content_entries.push(
                                        LogEntry::new(
                                            LogEntryLevel::Debug,
                                            container_name.to_string(),
                                            format!("  {}", line),
                                        )
                                        .with_session(session_id)
                                        .with_metadata("write_result", "true")
                                    );
                                }
                            }
                        }
                    }
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
            WidgetOutput::Simple(
                helpers::create_log_entry(
                    LogEntryLevel::Error,
                    container_name,
                    "Invalid event for WriteWidget".to_string(),
                    session_id,
                    "error",
                )
            )
        }
    }

    fn name(&self) -> &'static str {
        "WriteWidget"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_write_widget_can_handle() {
        let widget = WriteWidget::new();

        let write_event = AgentEvent::ToolCall {
            id: "test".to_string(),
            name: "Write".to_string(),
            input: json!({}),
            description: None,
        };
        assert!(widget.can_handle(&write_event));

        let other_event = AgentEvent::ToolCall {
            id: "test".to_string(),
            name: "Read".to_string(),
            input: json!({}),
            description: None,
        };
        assert!(!widget.can_handle(&other_event));
    }

    #[test]
    fn test_write_widget_render() {
        let widget = WriteWidget::new();
        let event = AgentEvent::ToolCall {
            id: "write_123".to_string(),
            name: "Write".to_string(),
            input: json!({
                "file_path": "test.py",
                "content": "def hello():\n    print('Hello, World!')\n"
            }),
            description: Some("Create greeting function".to_string()),
        };

        let output = widget.render(event, "test-container", Uuid::nil());

        match output {
            WidgetOutput::MultiLine(entries) => {
                assert!(!entries.is_empty());
                assert!(entries[0].message.contains("🐍")); // Python file icon
                assert!(entries[0].message.contains("test.py"));
                // Should show stats
                assert!(entries.iter().any(|e| e.message.contains("2 lines")));
            }
            _ => panic!("Expected MultiLine output"),
        }
    }

    #[test]
    fn test_write_widget_render_with_result_success() {
        let widget = WriteWidget::new();
        let event = AgentEvent::ToolCall {
            id: "write_123".to_string(),
            name: "Write".to_string(),
            input: json!({
                "file_path": "test.rs",
                "content": "fn main() {\n    println!(\"Hello, World!\");\n}"
            }),
            description: Some("Create main function".to_string()),
        };

        let result = Some(ToolResult {
            tool_use_id: "write_123".to_string(),
            content: json!(null),
            is_error: false,
        });

        let output = widget.render_with_result(event, result, "test-container", Uuid::nil());

        match output {
            WidgetOutput::Hierarchical { header, content, collapsed } => {
                assert!(!header.is_empty());
                assert!(header[0].message.contains("🦀")); // Rust file icon
                assert!(header[0].message.contains("test.rs"));
                assert!(header.iter().any(|e| e.message.contains("3 lines"))); // fn main has 3 lines

                assert!(!content.is_empty());
                assert!(content.iter().any(|e| e.message.contains("✅ File written successfully")));
                assert!(!collapsed);
            }
            _ => panic!("Expected Hierarchical output"),
        }
    }

    #[test]
    fn test_write_widget_render_with_result_error() {
        let widget = WriteWidget::new();
        let event = AgentEvent::ToolCall {
            id: "write_456".to_string(),
            name: "Write".to_string(),
            input: json!({
                "file_path": "/root/test.txt",
                "content": "test content"
            }),
            description: None,
        };

        let result = Some(ToolResult {
            tool_use_id: "write_456".to_string(),
            content: json!({"content": "Permission denied"}),
            is_error: true,
        });

        let output = widget.render_with_result(event, result, "test-container", Uuid::nil());

        match output {
            WidgetOutput::Hierarchical { header, content, collapsed: _ } => {
                assert!(!header.is_empty());
                assert!(header[0].message.contains("📄")); // Generic file icon
                assert!(header[0].message.contains("/root/test.txt"));

                assert!(!content.is_empty());
                assert!(content.iter().any(|e| e.message.contains("❌") && e.message.contains("Permission denied")));
            }
            _ => panic!("Expected Hierarchical output"),
        }
    }
}
