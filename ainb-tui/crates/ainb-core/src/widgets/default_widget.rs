// ABOUTME: Default widget fallback for rendering any AgentEvent not handled by specialized widgets
// Provides basic formatting for all event types

use super::{MessageWidget, WidgetOutput, helpers};
use crate::agent_parsers::{AgentEvent, types::StructuredPayload};
use crate::components::live_logs_stream::{LogEntry, LogEntryLevel};
use uuid::Uuid;

pub struct DefaultWidget;

impl DefaultWidget {
    pub fn new() -> Self {
        Self
    }
}

impl MessageWidget for DefaultWidget {
    fn can_handle(&self, _event: &AgentEvent) -> bool {
        // Default widget can handle anything
        true
    }

    fn render(&self, event: AgentEvent, container_name: &str, session_id: Uuid) -> WidgetOutput {
        // This is essentially the existing agent_event_to_log_entry logic
        // but restructured for the widget pattern

        let log_entry = match event {
            AgentEvent::SessionInfo { model, tools, session_id: sid, mcp_servers } => {
                let mut info = format!("📊 Session: {} | {} tools available", model, tools.len());
                if let Some(servers) = mcp_servers {
                    info.push_str(&format!(
                        " | MCP: {}",
                        servers
                            .iter()
                            .map(|s| format!("{} ({})", s.name, s.status))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ));
                }
                LogEntry::new(LogEntryLevel::Info, container_name.to_string(), info)
                    .with_session(session_id)
                    .with_metadata("event_type", "session_info")
                    .with_metadata("session_id", &sid)
            }

            AgentEvent::Thinking { content } => {
                LogEntry::new(
                    LogEntryLevel::Debug,
                    container_name.to_string(),
                    format!("💭 {}", content),
                )
                .with_session(session_id)
                .with_metadata("event_type", "thinking")
            }

            AgentEvent::Message { content, id } => {
                LogEntry::new(
                    LogEntryLevel::Info,
                    container_name.to_string(),
                    format!("Claude: {}", content),
                )
                .with_session(session_id)
                .with_metadata("event_type", "message")
                .with_metadata("message_id", &id.unwrap_or_default())
            }

            AgentEvent::StreamingText { delta, message_id } => {
                LogEntry::new(LogEntryLevel::Info, container_name.to_string(), delta)
                    .with_session(session_id)
                    .with_metadata("event_type", "streaming")
                    .with_metadata("message_id", &message_id.unwrap_or_default())
            }

            AgentEvent::ToolCall { id, name, input, description } => {
                // Generic tool call formatting
                let mut msg = String::new();
                let desc = description.unwrap_or_default();

                if !desc.is_empty() {
                    msg.push_str(&format!("🔧 {}: {}", name, desc));
                } else {
                    msg.push_str(&format!("🔧 {}", name));
                }

                // Add common fields
                if let Some(cmd) = input.get("command").and_then(|v| v.as_str()) {
                    msg.push_str(&format!("\n💻 Command: {}", cmd));
                } else if let Some(query) = input.get("query").and_then(|v| v.as_str()) {
                    msg.push_str(&format!("\n🔎 Query: {}", query));
                } else if let Some(path) = input.get("file_path").and_then(|v| v.as_str()) {
                    msg.push_str(&format!("\n📄 File: {}", path));
                }

                LogEntry::new(LogEntryLevel::Info, container_name.to_string(), msg)
                    .with_session(session_id)
                    .with_metadata("event_type", "tool_call")
                    .with_metadata("tool_id", &id)
                    .with_metadata("tool_name", &name)
            }

            AgentEvent::ToolResult { tool_use_id, content, is_error } => {
                let (level, prefix) = if is_error {
                    (LogEntryLevel::Error, "❌")
                } else {
                    (LogEntryLevel::Info, "✅")
                };

                let display_content = helpers::truncate_text(&content, 500);

                LogEntry::new(
                    level,
                    container_name.to_string(),
                    format!("{} Result: {}", prefix, display_content),
                )
                .with_session(session_id)
                .with_metadata("event_type", "tool_result")
                .with_metadata("tool_use_id", &tool_use_id)
            }

            AgentEvent::Error { message, code } => {
                LogEntry::new(
                    LogEntryLevel::Error,
                    container_name.to_string(),
                    format!("❌ Error: {}", message),
                )
                .with_session(session_id)
                .with_metadata("event_type", "error")
                .with_metadata("error_code", &code.unwrap_or_default())
            }

            AgentEvent::Usage { .. } => {
                return WidgetOutput::MultiLine(vec![]);
            }

            AgentEvent::Custom { event_type, data } => {
                LogEntry::new(
                    LogEntryLevel::Info,
                    container_name.to_string(),
                    format!("📌 {}: {}", event_type, data),
                )
                .with_session(session_id)
                .with_metadata("event_type", "custom")
                .with_metadata("custom_type", &event_type)
            }

            AgentEvent::Structured(payload) => {
                // Handle structured payloads not caught by specialized widgets
                match payload {
                    StructuredPayload::GlobResults { paths, total } => {
                        let mut msg = format!("📂 Found {} files\n", total);

                        for path in paths.iter().take(15) {
                            msg.push_str(&format!("  • {}\n", path));
                        }

                        if paths.len() > 15 {
                            msg.push_str(&format!("  … +{} more", paths.len() - 15));
                        }

                        LogEntry::new(LogEntryLevel::Info, container_name.to_string(), msg)
                            .with_session(session_id)
                            .with_metadata("event_type", "structured")
                            .with_metadata("icon", "📂")
                    }

                    StructuredPayload::PrettyJson(json_str) => {
                        LogEntry::new(
                            LogEntryLevel::Info,
                            container_name.to_string(),
                            format!("📋 Data:\n{}", json_str),
                        )
                        .with_session(session_id)
                        .with_metadata("event_type", "structured")
                        .with_metadata("icon", "📋")
                    }

                    // TodoList should be handled by TodoWidget, but include as fallback
                    StructuredPayload::TodoList { .. } => {
                        LogEntry::new(
                            LogEntryLevel::Info,
                            container_name.to_string(),
                            "📝 Todo list (use TodoWidget for rich display)".to_string(),
                        )
                        .with_session(session_id)
                        .with_metadata("event_type", "structured")
                    }
                }
            }
        };

        WidgetOutput::Simple(log_entry)
    }

    fn name(&self) -> &'static str {
        "DefaultWidget"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_default_widget_handles_everything() {
        let widget = DefaultWidget::new();

        // Should handle any event
        let message_event = AgentEvent::Message {
            content: "Hello".to_string(),
            id: None,
        };
        assert!(widget.can_handle(&message_event));

        let error_event = AgentEvent::Error {
            message: "Something went wrong".to_string(),
            code: None,
        };
        assert!(widget.can_handle(&error_event));
    }

    #[test]
    fn test_default_widget_render_message() {
        let widget = DefaultWidget::new();
        let event = AgentEvent::Message {
            content: "Hello, world!".to_string(),
            id: Some("msg_123".to_string()),
        };

        let output = widget.render(event, "test-container", Uuid::nil());

        match output {
            WidgetOutput::Simple(entry) => {
                assert_eq!(entry.message, "Claude: Hello, world!");
                assert_eq!(entry.level, LogEntryLevel::Info);
            }
            _ => panic!("Expected Simple output"),
        }
    }

    #[test]
    fn test_default_widget_render_error() {
        let widget = DefaultWidget::new();
        let event = AgentEvent::Error {
            message: "Failed to process".to_string(),
            code: Some("ERR_001".to_string()),
        };

        let output = widget.render(event, "test-container", Uuid::nil());

        match output {
            WidgetOutput::Simple(entry) => {
                assert!(entry.message.contains("❌ Error: Failed to process"));
                assert_eq!(entry.level, LogEntryLevel::Error);
            }
            _ => panic!("Expected Simple output"),
        }
    }
}
