// ABOUTME: Widget for rendering thinking blocks showing Claude's reasoning process
// Displays thinking content with special formatting

use super::{MessageWidget, WidgetOutput, helpers};
use crate::agent_parsers::AgentEvent;
use crate::components::live_logs_stream::{LogEntry, LogEntryLevel};
use uuid::Uuid;

pub struct ThinkingWidget;

impl ThinkingWidget {
    pub fn new() -> Self {
        Self
    }
}

impl MessageWidget for ThinkingWidget {
    fn can_handle(&self, event: &AgentEvent) -> bool {
        matches!(event, AgentEvent::Thinking { .. })
    }

    fn render(&self, event: AgentEvent, container_name: &str, session_id: Uuid) -> WidgetOutput {
        if let AgentEvent::Thinking { content } = event {
            let mut entries = Vec::new();

            // Add thinking header
            entries.push(
                LogEntry::new(
                    LogEntryLevel::Debug,
                    container_name.to_string(),
                    "💭 Thinking...".to_string(),
                )
                .with_session(session_id)
                .with_metadata("event_type", "thinking")
            );

            // Split content into lines and display with indentation
            for line in content.lines().take(10) { // Limit to 10 lines to avoid spam
                let truncated = if line.len() > 100 {
                    format!("{}...", &line[..100])
                } else {
                    line.to_string()
                };

                entries.push(
                    LogEntry::new(
                        LogEntryLevel::Debug,
                        container_name.to_string(),
                        format!("  {}", truncated),
                    )
                    .with_session(session_id)
                    .with_metadata("event_type", "thinking_content")
                );
            }

            if content.lines().count() > 10 {
                entries.push(
                    LogEntry::new(
                        LogEntryLevel::Debug,
                        container_name.to_string(),
                        format!("  ... ({} more lines)", content.lines().count() - 10),
                    )
                    .with_session(session_id)
                    .with_metadata("event_type", "thinking_more")
                );
            }

            WidgetOutput::MultiLine(entries)
        } else {
            WidgetOutput::Simple(
                helpers::create_log_entry(
                    LogEntryLevel::Error,
                    container_name,
                    "Invalid event for ThinkingWidget".to_string(),
                    session_id,
                    "error",
                )
            )
        }
    }

    fn name(&self) -> &'static str {
        "ThinkingWidget"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_thinking_widget_can_handle() {
        let widget = ThinkingWidget::new();

        let thinking_event = AgentEvent::Thinking {
            content: "Analyzing the problem...".to_string(),
        };
        assert!(widget.can_handle(&thinking_event));

        let other_event = AgentEvent::Message {
            content: "Hello".to_string(),
            id: None,
        };
        assert!(!widget.can_handle(&other_event));
    }

    #[test]
    fn test_thinking_widget_render() {
        let widget = ThinkingWidget::new();
        let event = AgentEvent::Thinking {
            content: "I need to analyze this problem.\nFirst, let me understand the requirements.\nThen I'll implement a solution.".to_string(),
        };

        let output = widget.render(event, "test-container", Uuid::nil());

        match output {
            WidgetOutput::MultiLine(entries) => {
                assert!(!entries.is_empty());
                assert!(entries[0].message.contains("💭 Thinking"));
                assert!(entries[1].message.contains("I need to analyze"));
                assert!(entries.len() >= 3); // Header + at least 2 lines of content
            }
            _ => panic!("Expected MultiLine output"),
        }
    }
}
