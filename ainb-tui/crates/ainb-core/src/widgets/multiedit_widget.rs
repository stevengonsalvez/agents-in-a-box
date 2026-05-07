// ABOUTME: Widget for rendering multiple file edit operations
// Displays multiple edits to a single file in a clean, organized format

use crate::agent_parsers::AgentEvent;
use crate::components::live_logs_stream::LogEntryLevel;
use uuid::Uuid;

use super::{MessageWidget, WidgetOutput, ToolResult, helpers};

pub struct MultiEditWidget;

impl MultiEditWidget {
    pub fn new() -> Self {
        Self
    }
}

impl MessageWidget for MultiEditWidget {
    fn can_handle(&self, event: &AgentEvent) -> bool {
        // Check if this is a multiedit tool use
        matches!(event,
            AgentEvent::ToolCall { name, .. } if name.to_lowercase() == "multiedit"
        )
    }

    fn render(&self, event: AgentEvent, container_name: &str, session_id: Uuid) -> WidgetOutput {
        let mut entries = Vec::new();

        match event {
            AgentEvent::ToolCall { id: _, name: _, input, description: _ } => {
                // Extract file path and edits from the input
                let file_path = input.get("file_path")
                    .and_then(|v| v.as_str())
                    .unwrap_or("<unknown file>");

                let edits = input.get("edits")
                    .and_then(|v| v.as_array());

                // Header
                entries.push(helpers::create_log_entry(
                    LogEntryLevel::Info,
                    container_name,
                    format!("📝 MultiEdit: {}", file_path),
                    session_id,
                    "multiedit",
                ));

                // Display each edit
                if let Some(edits_array) = edits {
                    entries.push(helpers::create_log_entry(
                        LogEntryLevel::Debug,
                        container_name,
                        format!("   Applying {} edits:", edits_array.len()),
                        session_id,
                        "multiedit",
                    ));

                    for (idx, edit) in edits_array.iter().enumerate() {
                        let old_string = edit.get("old_string")
                            .and_then(|v| v.as_str())
                            .unwrap_or("<unknown>");
                        let new_string = edit.get("new_string")
                            .and_then(|v| v.as_str())
                            .unwrap_or("<unknown>");
                        let replace_all = edit.get("replace_all")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);

                        // Edit number
                        entries.push(helpers::create_log_entry(
                            LogEntryLevel::Debug,
                            container_name,
                            format!("   Edit {}{}:", idx + 1, if replace_all { " (replace all)" } else { "" }),
                            session_id,
                            "multiedit",
                        ));

                        // Show old string (truncated if too long)
                        let old_display = if old_string.len() > 100 {
                            format!("{}...", &old_string[..100])
                        } else {
                            old_string.to_string()
                        };
                        entries.push(helpers::create_log_entry(
                            LogEntryLevel::Debug,
                            container_name,
                            format!("      - {}", old_display),
                            session_id,
                            "multiedit",
                        ));

                        // Show new string (truncated if too long)
                        let new_display = if new_string.len() > 100 {
                            format!("{}...", &new_string[..100])
                        } else {
                            new_string.to_string()
                        };
                        entries.push(helpers::create_log_entry(
                            LogEntryLevel::Debug,
                            container_name,
                            format!("      + {}", new_display),
                            session_id,
                            "multiedit",
                        ));
                    }
                } else {
                    entries.push(helpers::create_log_entry(
                        LogEntryLevel::Debug,
                        container_name,
                        "   No edits found".to_string(),
                        session_id,
                        "multiedit",
                    ));
                }
            }
            _ => {
                // Should not happen if can_handle works correctly
                entries.push(helpers::create_log_entry(
                    LogEntryLevel::Info,
                    container_name,
                    "📝 MultiEdit".to_string(),
                    session_id,
                    "multiedit",
                ));
            }
        }

        // Add separator for visual clarity
        entries.push(helpers::create_separator(container_name, session_id));

        WidgetOutput::MultiLine(entries)
    }

    fn render_with_result(
        &self,
        event: AgentEvent,
        result: Option<ToolResult>,
        container_name: &str,
        session_id: Uuid,
    ) -> WidgetOutput {
        let mut entries = Vec::new();

        // First render the tool call
        let tool_output = self.render(event.clone(), container_name, session_id);
        match tool_output {
            WidgetOutput::MultiLine(tool_entries) => entries.extend(tool_entries),
            WidgetOutput::Simple(entry) => entries.push(entry),
            _ => {}
        }

        // Then render the result if available
        if let Some(tool_result) = result {
            if tool_result.is_error {
                entries.push(helpers::create_log_entry(
                    LogEntryLevel::Error,
                    container_name,
                    "   ❌ MultiEdit failed".to_string(),
                    session_id,
                    "multiedit_result",
                ));

                // Show error message if available
                if let Some(error_msg) = tool_result.content.as_str() {
                    entries.push(helpers::create_log_entry(
                        LogEntryLevel::Error,
                        container_name,
                        format!("   Error: {}", error_msg),
                        session_id,
                        "multiedit_result",
                    ));
                }
            } else {
                entries.push(helpers::create_log_entry(
                    LogEntryLevel::Info,
                    container_name,
                    "   ✅ MultiEdit completed successfully".to_string(),
                    session_id,
                    "multiedit_result",
                ));

                // Show result preview if available
                if let Some(content_str) = tool_result.content.as_str() {
                    // Check if this looks like a file preview
                    if content_str.contains("has been updated with multiple edits") {
                        // Just show success message, already displayed above
                    } else if content_str.lines().count() <= 20 {
                        // Show short results inline
                        for line in content_str.lines().take(10) {
                            entries.push(helpers::create_log_entry(
                                LogEntryLevel::Debug,
                                container_name,
                                format!("      {}", line),
                                session_id,
                                "multiedit_result",
                            ));
                        }
                        if content_str.lines().count() > 10 {
                            entries.push(helpers::create_log_entry(
                                LogEntryLevel::Debug,
                                container_name,
                                "      ... (truncated)".to_string(),
                                session_id,
                                "multiedit_result",
                            ));
                        }
                    }
                }
            }
        }

        WidgetOutput::MultiLine(entries)
    }

    fn name(&self) -> &'static str {
        "MultiEditWidget"
    }
}

impl Default for MultiEditWidget {
    fn default() -> Self {
        Self::new()
    }
}