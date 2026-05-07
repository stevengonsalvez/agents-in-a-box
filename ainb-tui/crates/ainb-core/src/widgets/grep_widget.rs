// ABOUTME: Widget for rendering Grep tool calls showing search results
// Displays search patterns and matching files with context

use super::{MessageWidget, WidgetOutput, ToolResult, helpers, result_parser};
use crate::agent_parsers::AgentEvent;
use crate::components::live_logs_stream::{LogEntry, LogEntryLevel};
use uuid::Uuid;

pub struct GrepWidget;

impl GrepWidget {
    pub fn new() -> Self {
        Self
    }
}

impl MessageWidget for GrepWidget {
    fn can_handle(&self, event: &AgentEvent) -> bool {
        matches!(event, AgentEvent::ToolCall { name, .. } if name == "Grep")
    }

    fn render(&self, event: AgentEvent, container_name: &str, session_id: Uuid) -> WidgetOutput {
        if let AgentEvent::ToolCall { id, name: _, input, description } = event {
            let mut entries = Vec::new();

            // Extract search parameters
            let pattern = input.get("pattern")
                .and_then(|v| v.as_str())
                .unwrap_or("*");

            // Build the main message
            let mut main_msg = String::new();
            let desc = description.as_ref().map(|s| s.as_str()).unwrap_or("");

            // Header line with tool name and description
            if !desc.is_empty() {
                main_msg.push_str(&format!("🔍 Grep: {}", desc));
            } else {
                main_msg.push_str("🔍 Grep");
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
            .with_metadata("tool_name", "Grep");

            entries.push(header_entry);

            // Add search pattern as separate entry
            let pattern_entry = LogEntry::new(
                LogEntryLevel::Info,
                container_name.to_string(),
                format!("  🎯 Pattern: \"{}\"", pattern),
            )
            .with_session(session_id)
            .with_metadata("event_type", "grep_pattern")
            .with_metadata("tool_id", &id);

            entries.push(pattern_entry);

            // Add search location if not current directory
            let path = input.get("path")
                .and_then(|v| v.as_str())
                .unwrap_or(".");

            if path != "." {
                entries.push(
                    LogEntry::new(
                        LogEntryLevel::Debug,
                        container_name.to_string(),
                        format!("  📁 Path: {}", path),
                    )
                    .with_session(session_id)
                    .with_metadata("event_type", "grep_path")
                );
            }

            // Add search options if present
            let mut options = Vec::new();

            if input.get("-i").and_then(|v| v.as_bool()).unwrap_or(false) {
                options.push("case-insensitive");
            }

            if input.get("multiline").and_then(|v| v.as_bool()).unwrap_or(false) {
                options.push("multiline");
            }

            if let Some(glob) = input.get("glob").and_then(|v| v.as_str()) {
                entries.push(
                    LogEntry::new(
                        LogEntryLevel::Debug,
                        container_name.to_string(),
                        format!("  🗂️  Files: {}", glob),
                    )
                    .with_session(session_id)
                    .with_metadata("event_type", "grep_glob")
                );
            }

            if let Some(file_type) = input.get("type").and_then(|v| v.as_str()) {
                entries.push(
                    LogEntry::new(
                        LogEntryLevel::Debug,
                        container_name.to_string(),
                        format!("  📝 Type: {}", file_type),
                    )
                    .with_session(session_id)
                    .with_metadata("event_type", "grep_type")
                );
            }

            if !options.is_empty() {
                entries.push(
                    LogEntry::new(
                        LogEntryLevel::Debug,
                        container_name.to_string(),
                        format!("  ⚙️ Options: {}", options.join(", ")),
                    )
                    .with_session(session_id)
                    .with_metadata("event_type", "grep_options")
                );
            }

            // Add output mode
            let output_mode = input.get("output_mode")
                .and_then(|v| v.as_str())
                .unwrap_or("files_with_matches");

            let mode_icon = match output_mode {
                "content" => "📄",
                "count" => "🔢",
                _ => "📋",
            };

            entries.push(
                LogEntry::new(
                    LogEntryLevel::Debug,
                    container_name.to_string(),
                    format!("  {} Mode: {}", mode_icon, output_mode),
                )
                .with_session(session_id)
                .with_metadata("event_type", "grep_mode")
            );

            WidgetOutput::MultiLine(entries)
        } else {
            WidgetOutput::Simple(
                helpers::create_log_entry(
                    LogEntryLevel::Error,
                    container_name,
                    "Invalid event for GrepWidget".to_string(),
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

            // Extract search parameters
            let pattern = input.get("pattern")
                .and_then(|v| v.as_str())
                .unwrap_or("*");

            // Build the main header message
            let mut main_msg = String::new();
            let desc = description.as_ref().map(|s| s.as_str()).unwrap_or("");

            // Header line with tool name and description
            if !desc.is_empty() {
                main_msg.push_str(&format!("🔍 Grep: {}", desc));
            } else {
                main_msg.push_str("🔍 Grep");
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
            .with_metadata("tool_name", "Grep");

            header_entries.push(header_entry);

            // Add search pattern as part of header
            let pattern_entry = LogEntry::new(
                LogEntryLevel::Info,
                container_name.to_string(),
                format!("Pattern: \"{}\"", pattern),
            )
            .with_session(session_id)
            .with_metadata("event_type", "grep_pattern")
            .with_metadata("tool_id", &id);

            header_entries.push(pattern_entry);

            // Process result if available
            if let Some(tool_result) = result {
                // Extract result content
                if let Some(content_str) = result_parser::format_tool_result(&tool_result.content) {
                    // Format grep results nicely
                    let formatted_results = format_grep_results(&content_str, &input);

                    for formatted_line in formatted_results {
                        let level = if tool_result.is_error {
                            LogEntryLevel::Error
                        } else {
                            LogEntryLevel::Info
                        };

                        content_entries.push(
                            LogEntry::new(
                                level,
                                container_name.to_string(),
                                formatted_line,
                            )
                            .with_session(session_id)
                            .with_metadata("grep_result", "true")
                        );
                    }
                } else if tool_result.is_error {
                    // Error with no content
                    content_entries.push(
                        LogEntry::new(
                            LogEntryLevel::Error,
                            container_name.to_string(),
                            "❌ Search failed with no output".to_string(),
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
                    "Invalid event for GrepWidget".to_string(),
                    session_id,
                    "error",
                )
            )
        }
    }

    fn name(&self) -> &'static str {
        "GrepWidget"
    }
}

/// Format grep results based on the output mode and content
fn format_grep_results(content: &str, input: &serde_json::Value) -> Vec<String> {
    let mut results = Vec::new();

    if content.trim().is_empty() {
        results.push("📭 No matches found".to_string());
        return results;
    }

    let output_mode = input.get("output_mode")
        .and_then(|v| v.as_str())
        .unwrap_or("files_with_matches");

    match output_mode {
        "content" => {
            // Format content with line numbers and context
            for line in content.lines() {
                if line.trim().is_empty() {
                    continue;
                }

                // Check if line has filename:line:content format
                if let Some(colon_pos) = line.find(':') {
                    if let Some(second_colon) = line[colon_pos + 1..].find(':') {
                        let second_colon_abs = colon_pos + 1 + second_colon;
                        let filename = &line[..colon_pos];
                        let line_num = &line[colon_pos + 1..second_colon_abs];
                        let content_part = &line[second_colon_abs + 1..];

                        results.push(format!("📄 {} (line {})", filename, line_num));
                        results.push(format!("   {}", content_part.trim()));
                    } else {
                        // Simple filename:content format
                        let filename = &line[..colon_pos];
                        let content_part = &line[colon_pos + 1..];
                        results.push(format!("📄 {}", filename));
                        results.push(format!("   {}", content_part.trim()));
                    }
                } else {
                    // No colon, treat as plain text
                    results.push(format!("   {}", line));
                }
            }
        },
        "count" => {
            // Format count results
            for line in content.lines() {
                if line.trim().is_empty() {
                    continue;
                }

                if let Some(colon_pos) = line.find(':') {
                    let filename = &line[..colon_pos];
                    let count = &line[colon_pos + 1..];
                    results.push(format!("📊 {} ({} matches)", filename, count.trim()));
                } else {
                    results.push(format!("📊 {}", line));
                }
            }
        },
        _ => {
            // files_with_matches or default
            for line in content.lines() {
                if !line.trim().is_empty() {
                    results.push(format!("📄 {}", line.trim()));
                }
            }
        }
    }

    results
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_grep_widget_can_handle() {
        let widget = GrepWidget::new();

        let grep_event = AgentEvent::ToolCall {
            id: "test".to_string(),
            name: "Grep".to_string(),
            input: json!({}),
            description: None,
        };
        assert!(widget.can_handle(&grep_event));

        let other_event = AgentEvent::ToolCall {
            id: "test".to_string(),
            name: "Read".to_string(),
            input: json!({}),
            description: None,
        };
        assert!(!widget.can_handle(&other_event));
    }

    #[test]
    fn test_grep_widget_render() {
        let widget = GrepWidget::new();
        let event = AgentEvent::ToolCall {
            id: "grep_123".to_string(),
            name: "Grep".to_string(),
            input: json!({
                "pattern": "TODO",
                "path": "src/",
                "glob": "*.rs",
                "-i": true,
                "output_mode": "content"
            }),
            description: Some("Find all TODOs".to_string()),
        };

        let output = widget.render(event, "test-container", Uuid::nil());

        match output {
            WidgetOutput::MultiLine(entries) => {
                assert!(!entries.is_empty());
                assert!(entries[0].message.contains("🔍 Grep: Find all TODOs"));
                assert!(entries.iter().any(|e| e.message.contains("Pattern: \"TODO\"")));
                assert!(entries.iter().any(|e| e.message.contains("*.rs")));
                assert!(entries.iter().any(|e| e.message.contains("case-insensitive")));
            }
            _ => panic!("Expected MultiLine output"),
        }
    }

    #[test]
    fn test_grep_widget_render_with_result() {
        let widget = GrepWidget::new();
        let event = AgentEvent::ToolCall {
            id: "grep_123".to_string(),
            name: "Grep".to_string(),
            input: json!({
                "pattern": "TODO",
                "output_mode": "content"
            }),
            description: Some("Find all TODOs".to_string()),
        };

        let tool_result = ToolResult {
            tool_use_id: "grep_123".to_string(),
            content: json!({"content": "src/main.rs:42:    // TODO: implement this feature\nsrc/lib.rs:15:    // TODO: add error handling"}),
            is_error: false,
        };

        let output = widget.render_with_result(event, Some(tool_result), "test-container", Uuid::nil());

        match output {
            WidgetOutput::Hierarchical { header, content, collapsed } => {
                assert!(!header.is_empty());
                assert!(!content.is_empty());
                assert!(!collapsed);
                assert!(header[0].message.contains("🔍 Grep: Find all TODOs"));
                assert!(header.iter().any(|e| e.message.contains("Pattern: \"TODO\"")));
                assert!(content.iter().any(|e| e.message.contains("src/main.rs")));
                assert!(content.iter().any(|e| e.message.contains("implement this feature")));
            }
            _ => panic!("Expected Hierarchical output"),
        }
    }

    #[test]
    fn test_grep_widget_render_with_empty_result() {
        let widget = GrepWidget::new();
        let event = AgentEvent::ToolCall {
            id: "grep_123".to_string(),
            name: "Grep".to_string(),
            input: json!({
                "pattern": "NONEXISTENT"
            }),
            description: None,
        };

        let tool_result = ToolResult {
            tool_use_id: "grep_123".to_string(),
            content: json!({"content": ""}),
            is_error: false,
        };

        let output = widget.render_with_result(event, Some(tool_result), "test-container", Uuid::nil());

        match output {
            WidgetOutput::Hierarchical { header, content, collapsed: _ } => {
                assert!(!header.is_empty());
                assert!(header[0].message.contains("🔍 Grep"));
                assert!(content.iter().any(|e| e.message.contains("📭 No matches found")));
            }
            _ => panic!("Expected Hierarchical output"),
        }
    }

    #[test]
    fn test_format_grep_results_files_with_matches() {
        let content = "src/main.rs\nsrc/lib.rs\ntests/test.rs";
        let input = json!({"output_mode": "files_with_matches"});

        let results = format_grep_results(content, &input);

        assert_eq!(results.len(), 3);
        assert!(results[0].contains("📄 src/main.rs"));
        assert!(results[1].contains("📄 src/lib.rs"));
        assert!(results[2].contains("📄 tests/test.rs"));
    }

    #[test]
    fn test_format_grep_results_content() {
        let content = "src/main.rs:42:    // TODO: implement this\nsrc/lib.rs:15:    // TODO: add error handling";
        let input = json!({"output_mode": "content"});

        let results = format_grep_results(content, &input);

        assert!(results.len() >= 4); // 2 files with 2 lines each
        assert!(results.iter().any(|r| r.contains("src/main.rs (line 42)")));
        assert!(results.iter().any(|r| r.contains("implement this")));
        assert!(results.iter().any(|r| r.contains("src/lib.rs (line 15)")));
    }

    #[test]
    fn test_format_grep_results_count() {
        let content = "src/main.rs:3\nsrc/lib.rs:1";
        let input = json!({"output_mode": "count"});

        let results = format_grep_results(content, &input);

        assert_eq!(results.len(), 2);
        assert!(results[0].contains("📊 src/main.rs (3 matches)"));
        assert!(results[1].contains("📊 src/lib.rs (1 matches)"));
    }
}
