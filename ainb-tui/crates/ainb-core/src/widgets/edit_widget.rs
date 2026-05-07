// ABOUTME: Widget for rendering Edit tool calls as diff views
// Shows file changes with additions and deletions highlighted

use super::{MessageWidget, WidgetOutput, ToolResult, helpers, result_parser};
use crate::agent_parsers::AgentEvent;
use crate::components::live_logs_stream::{LogEntry, LogEntryLevel};
use uuid::Uuid;

pub struct EditWidget;

impl EditWidget {
    pub fn new() -> Self {
        Self
    }

    /// Create a diff view from old and new strings
    fn create_diff_view(old_str: &str, new_str: &str, file_path: &str) -> Vec<String> {
        let mut diff_lines = Vec::new();

        // Header
        diff_lines.push(format!("📝 Edit: {}", file_path));
        diff_lines.push("  ───────────────────────".to_string());

        // For simple edits, show before/after
        // In a real implementation, we'd use a proper diff algorithm

        // Show removed lines
        if !old_str.is_empty() {
            diff_lines.push("  ➖ Removed:".to_string());
            for line in old_str.lines().take(5) { // Limit preview
                diff_lines.push(format!("    - {}", line));
            }
            if old_str.lines().count() > 5 {
                diff_lines.push(format!("    ... ({} more lines)", old_str.lines().count() - 5));
            }
        }

        // Show added lines
        if !new_str.is_empty() {
            diff_lines.push("  ➕ Added:".to_string());
            for line in new_str.lines().take(5) { // Limit preview
                diff_lines.push(format!("    + {}", line));
            }
            if new_str.lines().count() > 5 {
                diff_lines.push(format!("    ... ({} more lines)", new_str.lines().count() - 5));
            }
        }

        diff_lines
    }
}

impl MessageWidget for EditWidget {
    fn can_handle(&self, event: &AgentEvent) -> bool {
        matches!(event, AgentEvent::ToolCall { name, .. } if name == "Edit" || name == "MultiEdit")
    }

    fn render(&self, event: AgentEvent, container_name: &str, session_id: Uuid) -> WidgetOutput {
        if let AgentEvent::ToolCall { id, name, input, description } = event {
            let mut entries = Vec::new();

            // Handle single Edit
            if name == "Edit" {
                if let (Some(file_path), Some(old_str), Some(new_str)) = (
                    input.get("file_path").and_then(|v| v.as_str()),
                    input.get("old_string").and_then(|v| v.as_str()),
                    input.get("new_string").and_then(|v| v.as_str()),
                ) {
                    // Create diff view
                    let diff_lines = Self::create_diff_view(old_str, new_str, file_path);

                    // Create entries for each diff line
                    for (i, line) in diff_lines.iter().enumerate() {
                        let level = if i == 0 {
                            LogEntryLevel::Info
                        } else {
                            LogEntryLevel::Debug
                        };

                        let entry = LogEntry::new(
                            level,
                            container_name.to_string(),
                            line.clone(),
                        )
                        .with_session(session_id)
                        .with_metadata("event_type", "edit_diff")
                        .with_metadata("tool_id", &id)
                        .with_metadata("tool_name", &name);

                        entries.push(entry);
                    }

                    // Add replace_all indicator if present
                    if let Some(replace_all) = input.get("replace_all").and_then(|v| v.as_bool()) {
                        if replace_all {
                            let replace_entry = LogEntry::new(
                                LogEntryLevel::Info,
                                container_name.to_string(),
                                "  🔄 Replace all occurrences".to_string(),
                            )
                            .with_session(session_id)
                            .with_metadata("event_type", "edit_replace_all")
                            .with_metadata("tool_id", &id);

                            entries.push(replace_entry);
                        }
                    }
                }
            }
            // Handle MultiEdit
            else if name == "MultiEdit" {
                if let (Some(file_path), Some(edits)) = (
                    input.get("file_path").and_then(|v| v.as_str()),
                    input.get("edits").and_then(|v| v.as_array()),
                ) {
                    // Header
                    let header = format!("📝 MultiEdit: {} ({} changes)", file_path, edits.len());
                    entries.push(
                        helpers::create_log_entry(
                            LogEntryLevel::Info,
                            container_name,
                            header,
                            session_id,
                            "multi_edit",
                        )
                        .with_metadata("tool_id", &id)
                        .with_metadata("tool_name", &name)
                    );

                    // Show summary of edits
                    for (idx, edit) in edits.iter().enumerate().take(3) {
                        if let (Some(old_str), Some(new_str)) = (
                            edit.get("old_string").and_then(|v| v.as_str()),
                            edit.get("new_string").and_then(|v| v.as_str()),
                        ) {
                            let old_preview = truncate_for_preview(old_str, 30);
                            let new_preview = truncate_for_preview(new_str, 30);

                            let edit_summary = format!(
                                "  Edit #{}: \"{}\" → \"{}\"",
                                idx + 1,
                                old_preview,
                                new_preview
                            );

                            entries.push(
                                LogEntry::new(
                                    LogEntryLevel::Debug,
                                    container_name.to_string(),
                                    edit_summary,
                                )
                                .with_session(session_id)
                                .with_metadata("event_type", "edit_summary")
                                .with_metadata("tool_id", &id)
                            );
                        }
                    }

                    if edits.len() > 3 {
                        let more_msg = format!("  ... and {} more edits", edits.len() - 3);
                        entries.push(
                            LogEntry::new(
                                LogEntryLevel::Debug,
                                container_name.to_string(),
                                more_msg,
                            )
                            .with_session(session_id)
                            .with_metadata("event_type", "edit_more")
                            .with_metadata("tool_id", &id)
                        );
                    }
                }
            }

            // Add description if present
            if let Some(desc) = description {
                if !desc.is_empty() {
                    entries.insert(0,
                        helpers::create_log_entry(
                            LogEntryLevel::Info,
                            container_name,
                            format!("🔧 {}: {}", name, desc),
                            session_id,
                            "tool_call",
                        )
                        .with_metadata("tool_id", &id)
                        .with_metadata("tool_name", &name)
                    );
                }
            }

            if entries.is_empty() {
                // Fallback if we couldn't parse the edit
                entries.push(
                    helpers::create_log_entry(
                        LogEntryLevel::Info,
                        container_name,
                        format!("🔧 {}", name),
                        session_id,
                        "tool_call",
                    )
                    .with_metadata("tool_id", &id)
                    .with_metadata("tool_name", &name)
                );
            }

            WidgetOutput::MultiLine(entries)
        } else {
            WidgetOutput::Simple(
                helpers::create_log_entry(
                    LogEntryLevel::Error,
                    container_name,
                    "Invalid event for EditWidget".to_string(),
                    session_id,
                    "error",
                )
            )
        }
    }

    fn render_with_result(&self, event: AgentEvent, result: Option<ToolResult>, container_name: &str, session_id: Uuid) -> WidgetOutput {
        if let AgentEvent::ToolCall { id, name, input, description } = event {
            let mut header_entries = Vec::new();
            let mut content_entries = Vec::new();

            // Build the main header message
            let mut main_msg = String::new();
            let desc = description.as_ref().map(|s| s.as_str()).unwrap_or("");

            // Header line with tool name and description
            if !desc.is_empty() {
                main_msg.push_str(&format!("📝 {}: {}", name, desc));
            } else {
                main_msg.push_str(&format!("📝 {}", name));
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
            .with_metadata("tool_name", &name);

            header_entries.push(header_entry);

            // Handle single Edit
            if name == "Edit" {
                if let (Some(file_path), Some(old_str), Some(new_str)) = (
                    input.get("file_path").and_then(|v| v.as_str()),
                    input.get("old_string").and_then(|v| v.as_str()),
                    input.get("new_string").and_then(|v| v.as_str()),
                ) {
                    // Show file path in header
                    let file_entry = LogEntry::new(
                        LogEntryLevel::Info,
                        container_name.to_string(),
                        format!("📁 {}", file_path),
                    )
                    .with_session(session_id)
                    .with_metadata("event_type", "edit_file")
                    .with_metadata("tool_id", &id);

                    header_entries.push(file_entry);

                    // Create diff view for content
                    let diff_lines = Self::create_diff_view(old_str, new_str, file_path);

                    // Add diff lines to content
                    for line in diff_lines.iter().skip(2) { // Skip header and separator
                        let entry = LogEntry::new(
                            LogEntryLevel::Info,
                            container_name.to_string(),
                            line.clone(),
                        )
                        .with_session(session_id)
                        .with_metadata("event_type", "edit_diff")
                        .with_metadata("tool_id", &id);

                        content_entries.push(entry);
                    }

                    // Add replace_all indicator if present
                    if let Some(replace_all) = input.get("replace_all").and_then(|v| v.as_bool()) {
                        if replace_all {
                            let replace_entry = LogEntry::new(
                                LogEntryLevel::Info,
                                container_name.to_string(),
                                "🔄 Replace all occurrences".to_string(),
                            )
                            .with_session(session_id)
                            .with_metadata("event_type", "edit_replace_all")
                            .with_metadata("tool_id", &id);

                            content_entries.push(replace_entry);
                        }
                    }
                }
            }
            // Handle MultiEdit
            else if name == "MultiEdit" {
                if let (Some(file_path), Some(edits)) = (
                    input.get("file_path").and_then(|v| v.as_str()),
                    input.get("edits").and_then(|v| v.as_array()),
                ) {
                    // Show file path and edit count in header
                    let edit_header = format!("📁 {} ({} changes)", file_path, edits.len());
                    let file_entry = LogEntry::new(
                        LogEntryLevel::Info,
                        container_name.to_string(),
                        edit_header,
                    )
                    .with_session(session_id)
                    .with_metadata("event_type", "multi_edit_file")
                    .with_metadata("tool_id", &id);

                    header_entries.push(file_entry);

                    // Show summary of edits in content
                    for (idx, edit) in edits.iter().enumerate() {
                        if let (Some(old_str), Some(new_str)) = (
                            edit.get("old_string").and_then(|v| v.as_str()),
                            edit.get("new_string").and_then(|v| v.as_str()),
                        ) {
                            let old_preview = truncate_for_preview(old_str, 40);
                            let new_preview = truncate_for_preview(new_str, 40);

                            let edit_summary = format!(
                                "Edit #{}: \"{}\" → \"{}\"",
                                idx + 1,
                                old_preview,
                                new_preview
                            );

                            content_entries.push(
                                LogEntry::new(
                                    LogEntryLevel::Info,
                                    container_name.to_string(),
                                    edit_summary,
                                )
                                .with_session(session_id)
                                .with_metadata("event_type", "edit_summary")
                                .with_metadata("tool_id", &id)
                            );
                        }
                    }
                }
            }

            // Process result if available
            if let Some(tool_result) = result {
                // Add a separator before result
                if !content_entries.is_empty() {
                    content_entries.push(
                        LogEntry::new(
                            LogEntryLevel::Debug,
                            container_name.to_string(),
                            "".to_string(),
                        )
                        .with_session(session_id)
                    );
                }

                // Extract result content
                if let Some(content_str) = result_parser::format_tool_result(&tool_result.content) {
                    // Check if the content looks like markdown
                    let is_markdown = content_str.contains('#') ||
                                     content_str.contains('*') ||
                                     content_str.contains('`') ||
                                     content_str.contains('\n');

                    if is_markdown {
                        // Parse as markdown
                        let parsed_entries = result_parser::parse_markdown_to_logs(
                            &content_str,
                            container_name,
                            session_id,
                            if tool_result.is_error { LogEntryLevel::Error } else { LogEntryLevel::Info },
                        );
                        content_entries.extend(parsed_entries);
                    } else {
                        // Simple text output - show status
                        let status_msg = if tool_result.is_error {
                            format!("❌ Edit failed: {}", content_str)
                        } else {
                            "✅ Edit completed successfully".to_string()
                        };

                        let level = if tool_result.is_error {
                            LogEntryLevel::Error
                        } else {
                            LogEntryLevel::Info
                        };

                        content_entries.push(
                            LogEntry::new(
                                level,
                                container_name.to_string(),
                                status_msg,
                            )
                            .with_session(session_id)
                            .with_metadata("edit_result", "true")
                        );
                    }
                } else if tool_result.is_error {
                    // Error with no content
                    content_entries.push(
                        LogEntry::new(
                            LogEntryLevel::Error,
                            container_name.to_string(),
                            "❌ Edit failed with no output".to_string(),
                        )
                        .with_session(session_id)
                    );
                } else {
                    // Success with no specific content
                    content_entries.push(
                        LogEntry::new(
                            LogEntryLevel::Info,
                            container_name.to_string(),
                            "✅ Edit completed successfully".to_string(),
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
                // No result yet, just return the header with preview content
                if content_entries.is_empty() {
                    WidgetOutput::MultiLine(header_entries)
                } else {
                    WidgetOutput::Hierarchical {
                        header: header_entries,
                        content: content_entries,
                        collapsed: true,
                    }
                }
            }
        } else {
            // Should not happen if can_handle works correctly
            WidgetOutput::Simple(
                helpers::create_log_entry(
                    LogEntryLevel::Error,
                    container_name,
                    "Invalid event for EditWidget".to_string(),
                    session_id,
                    "error",
                )
            )
        }
    }

    fn name(&self) -> &'static str {
        "EditWidget"
    }
}

/// Truncate text for preview, handling newlines
fn truncate_for_preview(text: &str, max_len: usize) -> String {
    let one_line = text.lines().next().unwrap_or("");
    if one_line.len() > max_len {
        format!("{}...", &one_line[..max_len])
    } else if text.contains('\n') {
        format!("{}...", one_line)
    } else {
        one_line.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_edit_widget_can_handle() {
        let widget = EditWidget::new();

        let edit_event = AgentEvent::ToolCall {
            id: "test".to_string(),
            name: "Edit".to_string(),
            input: json!({}),
            description: None,
        };
        assert!(widget.can_handle(&edit_event));

        let multi_edit_event = AgentEvent::ToolCall {
            id: "test".to_string(),
            name: "MultiEdit".to_string(),
            input: json!({}),
            description: None,
        };
        assert!(widget.can_handle(&multi_edit_event));

        let other_event = AgentEvent::ToolCall {
            id: "test".to_string(),
            name: "Bash".to_string(),
            input: json!({}),
            description: None,
        };
        assert!(!widget.can_handle(&other_event));
    }

    #[test]
    fn test_edit_widget_render_single() {
        let widget = EditWidget::new();
        let event = AgentEvent::ToolCall {
            id: "edit_123".to_string(),
            name: "Edit".to_string(),
            input: json!({
                "file_path": "src/main.rs",
                "old_string": "fn old_function() {}",
                "new_string": "fn new_function() {}"
            }),
            description: Some("Rename function".to_string()),
        };

        let output = widget.render(event, "test-container", Uuid::nil());

        match output {
            WidgetOutput::MultiLine(entries) => {
                assert!(!entries.is_empty());
                // Should have description, header, and diff lines
                assert!(entries.iter().any(|e| e.message.contains("Rename function")));
                assert!(entries.iter().any(|e| e.message.contains("src/main.rs")));
            }
            _ => panic!("Expected MultiLine output"),
        }
    }

    #[test]
    fn test_edit_widget_render_multi() {
        let widget = EditWidget::new();
        let event = AgentEvent::ToolCall {
            id: "edit_456".to_string(),
            name: "MultiEdit".to_string(),
            input: json!({
                "file_path": "src/lib.rs",
                "edits": [
                    {"old_string": "old1", "new_string": "new1"},
                    {"old_string": "old2", "new_string": "new2"},
                ]
            }),
            description: None,
        };

        let output = widget.render(event, "test-container", Uuid::nil());

        match output {
            WidgetOutput::MultiLine(entries) => {
                assert!(!entries.is_empty());
                assert!(entries[0].message.contains("MultiEdit"));
                assert!(entries[0].message.contains("2 changes"));
            }
            _ => panic!("Expected MultiLine output"),
        }
    }
}
