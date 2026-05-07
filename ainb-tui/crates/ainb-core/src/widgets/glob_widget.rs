// ABOUTME: Widget for rendering Glob tool calls showing file pattern matching
// Displays file patterns and matched paths

use super::{MessageWidget, WidgetOutput, ToolResult, helpers, result_parser};
use crate::agent_parsers::AgentEvent;
use crate::components::live_logs_stream::{LogEntry, LogEntryLevel};
use uuid::Uuid;

pub struct GlobWidget;

impl GlobWidget {
    pub fn new() -> Self {
        Self
    }
}

impl MessageWidget for GlobWidget {
    fn can_handle(&self, event: &AgentEvent) -> bool {
        matches!(event, AgentEvent::ToolCall { name, .. } if name == "Glob") ||
        matches!(event, AgentEvent::Structured(crate::agent_parsers::types::StructuredPayload::GlobResults { .. }))
    }

    fn render(&self, event: AgentEvent, container_name: &str, session_id: Uuid) -> WidgetOutput {
        if let AgentEvent::ToolCall { id, name: _, input, description } = event {
            let mut entries = Vec::new();

            // Build the main message
            let mut main_msg = String::new();
            let desc = description.as_ref().map(|s| s.as_str()).unwrap_or("");

            // Extract pattern for display
            let pattern = input.get("pattern")
                .and_then(|v| v.as_str())
                .unwrap_or("*");

            // Header line with tool name and description
            if !desc.is_empty() {
                main_msg.push_str(&format!("🔎 Glob: {}", desc));
            } else {
                main_msg.push_str(&format!("🔎 Glob: {}", pattern));
            }

            // Create the header entry
            let header_entry = helpers::create_log_entry(
                LogEntryLevel::Info,
                container_name,
                main_msg,
                session_id,
                "tool_call",
            )
            .with_metadata("tool_id", &id)
            .with_metadata("tool_name", "Glob")
            .with_metadata("pattern", pattern);

            entries.push(header_entry);

            // Add pattern line as separate entry if not already in main message
            if !desc.is_empty() {
                let pattern_entry = LogEntry::new(
                    LogEntryLevel::Info,
                    container_name.to_string(),
                    format!("  🗂️  Pattern: {}", pattern),
                )
                .with_session(session_id)
                .with_metadata("event_type", "glob_pattern")
                .with_metadata("tool_id", &id);

                entries.push(pattern_entry);
            }

            // Add search path if specified
            if let Some(path) = input.get("path").and_then(|v| v.as_str()) {
                let path_entry = LogEntry::new(
                    LogEntryLevel::Info,
                    container_name.to_string(),
                    format!("  📁 Path: {}", path),
                )
                .with_session(session_id)
                .with_metadata("event_type", "glob_path")
                .with_metadata("tool_id", &id);

                entries.push(path_entry);
            }

            WidgetOutput::MultiLine(entries)
        } else {
            // Should not happen if can_handle works correctly
            WidgetOutput::Simple(
                helpers::create_log_entry(
                    LogEntryLevel::Error,
                    container_name,
                    "Invalid event for GlobWidget".to_string(),
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

            // Build the main header message
            let mut main_msg = String::new();
            let desc = description.as_ref().map(|s| s.as_str()).unwrap_or("");

            // Extract pattern for display
            let pattern = input.get("pattern")
                .and_then(|v| v.as_str())
                .unwrap_or("*");

            // Header line with tool name and description
            if !desc.is_empty() {
                main_msg.push_str(&format!("🔎 Glob: {}", desc));
            } else {
                main_msg.push_str(&format!("🔎 Glob: {}", pattern));
            }

            // Create the header entry
            let header_entry = helpers::create_log_entry(
                LogEntryLevel::Info,
                container_name,
                main_msg,
                session_id,
                "tool_call",
            )
            .with_metadata("tool_id", &id)
            .with_metadata("tool_name", "Glob")
            .with_metadata("pattern", pattern);

            header_entries.push(header_entry);

            // Add pattern line as part of header if not already in main message
            if !desc.is_empty() {
                let pattern_entry = LogEntry::new(
                    LogEntryLevel::Info,
                    container_name.to_string(),
                    format!("Pattern: {}", pattern),
                )
                .with_session(session_id)
                .with_metadata("event_type", "glob_pattern")
                .with_metadata("tool_id", &id);

                header_entries.push(pattern_entry);
            }

            // Add search path if specified
            if let Some(path) = input.get("path").and_then(|v| v.as_str()) {
                let path_entry = LogEntry::new(
                    LogEntryLevel::Info,
                    container_name.to_string(),
                    format!("Path: {}", path),
                )
                .with_session(session_id)
                .with_metadata("event_type", "glob_path")
                .with_metadata("tool_id", &id);

                header_entries.push(path_entry);
            }

            // Process result if available
            if let Some(tool_result) = result {
                // Extract result content
                if let Some(content_str) = result_parser::format_tool_result(&tool_result.content) {
                    // Parse the glob results - check if it's a list of files
                    let lines: Vec<&str> = content_str.lines().collect();

                    if lines.is_empty() {
                        content_entries.push(
                            LogEntry::new(
                                LogEntryLevel::Info,
                                container_name.to_string(),
                                "No files found matching pattern".to_string(),
                            )
                            .with_session(session_id)
                            .with_metadata("glob_result", "empty")
                        );
                    } else {
                        // Add count summary
                        let count_msg = if lines.len() == 1 {
                            "Found 1 file:".to_string()
                        } else {
                            format!("Found {} files:", lines.len())
                        };

                        content_entries.push(
                            LogEntry::new(
                                LogEntryLevel::Info,
                                container_name.to_string(),
                                count_msg,
                            )
                            .with_session(session_id)
                            .with_metadata("glob_count", &lines.len().to_string())
                        );

                        // Show files (limit to 20 for display)
                        for (idx, file_path) in lines.iter().take(20).enumerate() {
                            let level = if tool_result.is_error {
                                LogEntryLevel::Error
                            } else {
                                LogEntryLevel::Debug
                            };

                            content_entries.push(
                                LogEntry::new(
                                    level,
                                    container_name.to_string(),
                                    format!("  • {}", file_path.trim()),
                                )
                                .with_session(session_id)
                                .with_metadata("glob_file", "true")
                                .with_metadata("file_index", &idx.to_string())
                            );
                        }

                        // Show truncation message if there are more files
                        if lines.len() > 20 {
                            content_entries.push(
                                LogEntry::new(
                                    LogEntryLevel::Debug,
                                    container_name.to_string(),
                                    format!("  … and {} more files", lines.len() - 20),
                                )
                                .with_session(session_id)
                                .with_metadata("glob_truncated", "true")
                            );
                        }
                    }
                } else if tool_result.is_error {
                    // Error with no content
                    content_entries.push(
                        LogEntry::new(
                            LogEntryLevel::Error,
                            container_name.to_string(),
                            "❌ Glob search failed with no output".to_string(),
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
                    "Invalid event for GlobWidget".to_string(),
                    session_id,
                    "error",
                )
            )
        }
    }

    fn name(&self) -> &'static str {
        "GlobWidget"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_glob_widget_can_handle() {
        let widget = GlobWidget::new();

        let glob_event = AgentEvent::ToolCall {
            id: "test".to_string(),
            name: "Glob".to_string(),
            input: json!({}),
            description: None,
        };
        assert!(widget.can_handle(&glob_event));

        let other_event = AgentEvent::ToolCall {
            id: "test".to_string(),
            name: "Read".to_string(),
            input: json!({}),
            description: None,
        };
        assert!(!widget.can_handle(&other_event));
    }

    #[test]
    fn test_glob_widget_render() {
        let widget = GlobWidget::new();
        let event = AgentEvent::ToolCall {
            id: "glob_123".to_string(),
            name: "Glob".to_string(),
            input: json!({
                "pattern": "**/*.rs",
                "path": "src/"
            }),
            description: Some("Find all Rust files".to_string()),
        };

        let output = widget.render(event, "test-container", Uuid::nil());

        match output {
            WidgetOutput::MultiLine(entries) => {
                assert!(!entries.is_empty());
                assert!(entries[0].message.contains("🔎 Glob: Find all Rust files"));
                // Should have pattern and path entries
                assert!(entries.iter().any(|e| e.message.contains("**/*.rs")));
                assert!(entries.iter().any(|e| e.message.contains("src/")));
            }
            _ => panic!("Expected MultiLine output"),
        }
    }
}
