// ABOUTME: Widget for rendering TodoWrite tool calls with rich formatting
// Displays todo lists with status icons and summary statistics

use super::{MessageWidget, WidgetOutput, helpers};
use crate::agent_parsers::{AgentEvent, types::StructuredPayload};
use crate::components::live_logs_stream::{LogEntry, LogEntryLevel};
use serde_json::Value;
use uuid::Uuid;

pub struct TodoWidget;

impl TodoWidget {
    pub fn new() -> Self {
        Self
    }

    /// Get the icon for a todo status
    fn get_status_icon(status: &str) -> &'static str {
        match status {
            "done" | "completed" => "☑",
            "in_progress" | "active" => "⏳",
            _ => "◻︎",
        }
    }

    /// Count todos by status
    fn count_todos(todos: &[Value]) -> (u32, u32, u32) {
        let mut pending = 0u32;
        let mut in_progress = 0u32;
        let mut done = 0u32;

        for todo in todos {
            let status = todo.get("status")
                .and_then(|x| x.as_str())
                .unwrap_or("pending");

            match status {
                "completed" | "done" => done += 1,
                "in_progress" | "active" => in_progress += 1,
                _ => pending += 1,
            }
        }

        (pending, in_progress, done)
    }
}

impl MessageWidget for TodoWidget {
    fn can_handle(&self, event: &AgentEvent) -> bool {
        matches!(event,
            AgentEvent::ToolCall { name, .. } if name == "TodoWrite"
        ) || matches!(event,
            AgentEvent::Structured(StructuredPayload::TodoList { .. })
        )
    }

    fn render(&self, event: AgentEvent, container_name: &str, session_id: Uuid) -> WidgetOutput {
        let mut entries = Vec::new();

        match event {
            // Handle TodoWrite tool call
            AgentEvent::ToolCall { id, name: _, input, description: _ } => {
                // Extract todos and create summary
                if let Some(todos_val) = input.get("todos").and_then(|t| t.as_array()) {
                    let (pending, in_progress, done) = Self::count_todos(todos_val);

                    // Header with summary
                    let header_with_summary = format!(
                        "📝 Todos\n  Σ {} tasks • {} pending • {} ⏳ • {} ☑",
                        todos_val.len(),
                        pending,
                        in_progress,
                        done
                    );

                    entries.push(
                        LogEntry::new(
                            LogEntryLevel::Info,
                            container_name.to_string(),
                            header_with_summary,
                        )
                        .with_session(session_id)
                        .with_metadata("event_type", "tool_call")
                        .with_metadata("tool_id", &id)
                        .with_metadata("tool_name", "TodoWrite")
                    );

                    // Show all todos neatly formatted
                    for (idx, todo) in todos_val.iter().enumerate() {
                        if let Some(content) = todo.get("content")
                            .or_else(|| todo.get("text"))
                            .or_else(|| todo.get("task"))
                            .and_then(|v| v.as_str())
                        {
                            let status = todo.get("status")
                                .and_then(|x| x.as_str())
                                .unwrap_or("pending");

                            let icon = Self::get_status_icon(status);
                            let todo_line = format!("    {} {}", icon, content);

                            let todo_entry = LogEntry::new(
                                LogEntryLevel::Info,
                                container_name.to_string(),
                                todo_line,
                            )
                            .with_session(session_id)
                            .with_metadata("event_type", "todo_item")
                            .with_metadata("todo_index", &idx.to_string())
                            .with_metadata("todo_status", status);

                            entries.push(todo_entry);
                        }
                    }

                    // Add spacing after todo list
                    entries.push(helpers::create_separator(container_name, session_id));
                }
            }

            // Handle structured todo list
            AgentEvent::Structured(StructuredPayload::TodoList {
                title,
                items,
                pending,
                in_progress,
                done,
            }) => {
                // Title
                let title_text = title.unwrap_or_else(|| "📝 Todos".to_string());
                let title_entry = LogEntry::new(
                    LogEntryLevel::Info,
                    container_name.to_string(),
                    title_text,
                )
                .with_session(session_id)
                .with_metadata("event_type", "structured")
                .with_metadata("icon", "📝");

                entries.push(title_entry);

                // Show each todo item
                for (idx, item) in items.iter().enumerate() {
                    let icon = Self::get_status_icon(&item.status);
                    let item_line = format!("  {} {}", icon, item.text);

                    let item_entry = LogEntry::new(
                        LogEntryLevel::Info,
                        container_name.to_string(),
                        item_line,
                    )
                    .with_session(session_id)
                    .with_metadata("event_type", "todo_item")
                    .with_metadata("todo_index", &idx.to_string())
                    .with_metadata("todo_status", &item.status);

                    entries.push(item_entry);
                }

                // Summary line
                let summary = format!(
                    "  Σ {} • {} pending • {} ⏳ • {} ☑",
                    items.len(),
                    pending,
                    in_progress,
                    done
                );

                let summary_entry = LogEntry::new(
                    LogEntryLevel::Info,
                    container_name.to_string(),
                    summary,
                )
                .with_session(session_id)
                .with_metadata("event_type", "todo_summary");

                entries.push(summary_entry);
            }

            _ => {
                // Should not happen if can_handle works correctly
                entries.push(
                    helpers::create_log_entry(
                        LogEntryLevel::Error,
                        container_name,
                        "Invalid event for TodoWidget".to_string(),
                        session_id,
                        "error",
                    )
                );
            }
        }

        WidgetOutput::MultiLine(entries)
    }

    fn name(&self) -> &'static str {
        "TodoWidget"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use crate::agent_parsers::types::TodoItem;

    #[test]
    fn test_todo_widget_can_handle() {
        let widget = TodoWidget::new();

        // Should handle TodoWrite tool calls
        let todo_event = AgentEvent::ToolCall {
            id: "test".to_string(),
            name: "TodoWrite".to_string(),
            input: json!({}),
            description: None,
        };
        assert!(widget.can_handle(&todo_event));

        // Should handle structured todo lists
        let structured_event = AgentEvent::Structured(StructuredPayload::TodoList {
            title: None,
            items: vec![],
            pending: 0,
            in_progress: 0,
            done: 0,
        });
        assert!(widget.can_handle(&structured_event));

        // Should not handle other tools
        let other_event = AgentEvent::ToolCall {
            id: "test".to_string(),
            name: "Bash".to_string(),
            input: json!({}),
            description: None,
        };
        assert!(!widget.can_handle(&other_event));
    }

    #[test]
    fn test_todo_widget_render_tool_call() {
        let widget = TodoWidget::new();
        let event = AgentEvent::ToolCall {
            id: "todo_123".to_string(),
            name: "TodoWrite".to_string(),
            input: json!({
                "todos": [
                    {"content": "Write tests", "status": "completed"},
                    {"content": "Fix bugs", "status": "in_progress"},
                    {"content": "Deploy", "status": "pending"},
                ]
            }),
            description: None,
        };

        let output = widget.render(event, "test-container", Uuid::nil());

        match output {
            WidgetOutput::MultiLine(entries) => {
                assert!(!entries.is_empty());
                assert!(entries[0].message.contains("📝 Todos"));
                assert!(entries[0].message.contains("Σ 3 tasks"));
                assert!(entries[0].message.contains("1 pending"));
                assert!(entries[0].message.contains("1 ⏳"));
                assert!(entries[0].message.contains("1 ☑"));
                // Check that todos are shown
                assert!(entries[1].message.contains("☑ Write tests"));
                assert!(entries[2].message.contains("⏳ Fix bugs"));
                assert!(entries[3].message.contains("◻︎ Deploy"));
            }
            _ => panic!("Expected MultiLine output"),
        }
    }

    #[test]
    fn test_todo_widget_render_structured() {
        let widget = TodoWidget::new();
        let event = AgentEvent::Structured(StructuredPayload::TodoList {
            title: Some("My Tasks".to_string()),
            items: vec![
                TodoItem { text: "Task 1".to_string(), status: "pending".to_string() },
                TodoItem { text: "Task 2".to_string(), status: "in_progress".to_string() },
                TodoItem { text: "Task 3".to_string(), status: "done".to_string() },
            ],
            pending: 1,
            in_progress: 1,
            done: 1,
        });

        let output = widget.render(event, "test-container", Uuid::nil());

        match output {
            WidgetOutput::MultiLine(entries) => {
                assert!(!entries.is_empty());
                assert_eq!(entries[0].message, "My Tasks");
                assert!(entries[1].message.contains("◻︎ Task 1"));
                assert!(entries[2].message.contains("⏳ Task 2"));
                assert!(entries[3].message.contains("☑ Task 3"));
            }
            _ => panic!("Expected MultiLine output"),
        }
    }

    #[test]
    fn test_status_icons() {
        assert_eq!(TodoWidget::get_status_icon("done"), "☑");
        assert_eq!(TodoWidget::get_status_icon("completed"), "☑");
        assert_eq!(TodoWidget::get_status_icon("in_progress"), "⏳");
        assert_eq!(TodoWidget::get_status_icon("active"), "⏳");
        assert_eq!(TodoWidget::get_status_icon("pending"), "◻︎");
        assert_eq!(TodoWidget::get_status_icon("unknown"), "◻︎");
    }
}
