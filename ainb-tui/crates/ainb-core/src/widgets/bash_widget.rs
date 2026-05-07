// ABOUTME: Widget for rendering Bash/shell command executions with rich formatting
// Displays commands with syntax highlighting and structured output

use super::{MessageWidget, WidgetOutput, ToolResult, helpers, result_parser};
use crate::agent_parsers::AgentEvent;
use crate::components::live_logs_stream::{LogEntry, LogEntryLevel};
use uuid::Uuid;

pub struct BashWidget;

impl BashWidget {
    pub fn new() -> Self {
        Self
    }
}

impl MessageWidget for BashWidget {
    fn can_handle(&self, event: &AgentEvent) -> bool {
        matches!(event, AgentEvent::ToolCall { name, .. } if name == "Bash")
    }

    fn render(&self, event: AgentEvent, container_name: &str, session_id: Uuid) -> WidgetOutput {
        if let AgentEvent::ToolCall { id, name: _, input, description } = event {
            let mut entries = Vec::new();

            // Build the main message
            let mut main_msg = String::new();
            let desc = description.as_ref().map(|s| s.as_str()).unwrap_or("");

            // Header line with tool name and description
            if !desc.is_empty() {
                main_msg.push_str(&format!("Bash: {}", desc));
            } else {
                main_msg.push_str("Bash");
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
            .with_metadata("tool_name", "Bash");

            entries.push(header_entry);

            // Extract and format the command
            if let Some(cmd) = input.get("command").and_then(|v| v.as_str()) {
                let formatted_cmd = format_bash_command(cmd);

                // Add command line as a separate entry
                let cmd_entry = LogEntry::new(
                    LogEntryLevel::Info,
                    container_name.to_string(),
                    formatted_cmd,
                )
                .with_session(session_id)
                .with_metadata("event_type", "bash_command")
                .with_metadata("tool_id", &id);

                entries.push(cmd_entry);
            }

            // Add description if present and different from command
            if let Some(desc_val) = input.get("description").and_then(|v| v.as_str()) {
                if !desc_val.is_empty() && description.is_none() {
                    let desc_entry = LogEntry::new(
                        LogEntryLevel::Info,
                        container_name.to_string(),
                        format!("  📝 {}", desc_val),
                    )
                    .with_session(session_id)
                    .with_metadata("event_type", "bash_description")
                    .with_metadata("tool_id", &id);

                    entries.push(desc_entry);
                }
            }

            // Add timeout if specified
            if let Some(timeout) = input.get("timeout").and_then(|v| v.as_u64()) {
                if timeout != 120000 { // Don't show default timeout
                    let timeout_entry = LogEntry::new(
                        LogEntryLevel::Debug,
                        container_name.to_string(),
                        format!("  ⏱️  Timeout: {}ms", timeout),
                    )
                    .with_session(session_id)
                    .with_metadata("event_type", "bash_timeout")
                    .with_metadata("tool_id", &id);

                    entries.push(timeout_entry);
                }
            }

            WidgetOutput::MultiLine(entries)
        } else {
            // Should not happen if can_handle works correctly
            WidgetOutput::Simple(
                helpers::create_log_entry(
                    LogEntryLevel::Error,
                    container_name,
                    "Invalid event for BashWidget".to_string(),
                    session_id,
                    "error",
                )
            )
        }
    }

    fn render_with_result(&self, event: AgentEvent, result: Option<ToolResult>, container_name: &str, session_id: Uuid) -> WidgetOutput {
        if let AgentEvent::ToolCall { id, name: _, input, description } = event {
            let mut entries = Vec::new();

            // Build the main header message
            let mut main_msg = String::new();
            let desc = description.as_ref().map(|s| s.as_str()).unwrap_or("");

            // Header line with tool name and description
            if !desc.is_empty() {
                main_msg.push_str(&format!("Bash: {}", desc));
            } else {
                main_msg.push_str("Bash");
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
            .with_metadata("tool_name", "Bash");

            entries.push(header_entry);

            // Extract and format the command
            if let Some(cmd) = input.get("command").and_then(|v| v.as_str()) {
                let formatted_cmd = format_bash_command(cmd);

                // Add command line
                let cmd_entry = LogEntry::new(
                    LogEntryLevel::Info,
                    container_name.to_string(),
                    formatted_cmd,
                )
                .with_session(session_id)
                .with_metadata("event_type", "bash_command")
                .with_metadata("tool_id", &id);

                entries.push(cmd_entry);
            }

            // Process result if available
            if let Some(tool_result) = result {
                // Extract result content
                if let Some(content_str) = result_parser::format_tool_result(&tool_result.content) {
                    let level = if tool_result.is_error {
                        LogEntryLevel::Error
                    } else {
                        LogEntryLevel::Info
                    };

                    // Process each line with simple indentation
                    for line in content_str.lines() {
                        entries.push(
                            LogEntry::new(
                                level,
                                container_name.to_string(),
                                format!("  {}", line),
                            )
                            .with_session(session_id)
                            .with_metadata("bash_output", "true")
                        );
                    }
                } else if tool_result.is_error {
                    // Error with no content
                    entries.push(
                        LogEntry::new(
                            LogEntryLevel::Error,
                            container_name.to_string(),
                            "  Command failed with no output".to_string(),
                        )
                        .with_session(session_id)
                    );
                }
            }

            // Return simple MultiLine output
            WidgetOutput::MultiLine(entries)
        } else {
            // Should not happen if can_handle works correctly
            WidgetOutput::Simple(
                helpers::create_log_entry(
                    LogEntryLevel::Error,
                    container_name,
                    "Invalid event for BashWidget".to_string(),
                    session_id,
                    "error",
                )
            )
        }
    }

    fn name(&self) -> &'static str {
        "BashWidget"
    }
}

/// Format a bash command with simple indentation
fn format_bash_command(cmd: &str) -> String {
    format!("  {}", cmd)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_bash_widget_can_handle() {
        let widget = BashWidget::new();

        let bash_event = AgentEvent::ToolCall {
            id: "test".to_string(),
            name: "Bash".to_string(),
            input: json!({}),
            description: None,
        };
        assert!(widget.can_handle(&bash_event));

        let other_event = AgentEvent::ToolCall {
            id: "test".to_string(),
            name: "Edit".to_string(),
            input: json!({}),
            description: None,
        };
        assert!(!widget.can_handle(&other_event));
    }

    #[test]
    fn test_bash_widget_render() {
        let widget = BashWidget::new();
        let event = AgentEvent::ToolCall {
            id: "cmd_123".to_string(),
            name: "Bash".to_string(),
            input: json!({
                "command": "cargo test --quiet",
                "description": "Run tests quietly"
            }),
            description: Some("Running tests".to_string()),
        };

        let output = widget.render(event, "test-container", Uuid::nil());

        match output {
            WidgetOutput::MultiLine(entries) => {
                assert!(!entries.is_empty());
                assert!(entries[0].message.contains("Bash: Running tests"));
                if entries.len() > 1 {
                    // The command is formatted with simple indentation
                    assert!(entries[1].message.contains("  cargo test --quiet"));
                }
            }
            _ => panic!("Expected MultiLine output"),
        }
    }
}
