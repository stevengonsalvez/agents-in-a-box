// ABOUTME: Widget for rendering Task tool calls showing agent/sub-agent spawning
// Displays task descriptions and agent types

use super::{MessageWidget, WidgetOutput, ToolResult, helpers, result_parser};
use crate::agent_parsers::AgentEvent;
use crate::components::live_logs_stream::{LogEntry, LogEntryLevel};
use uuid::Uuid;

pub struct TaskWidget;

impl TaskWidget {
    pub fn new() -> Self {
        Self
    }

    /// Get icon for agent type
    fn get_agent_icon(agent_type: &str) -> &'static str {
        match agent_type.to_lowercase().as_str() {
            "general-purpose" => "🤖",
            "web-search-researcher" => "🌐",
            "devops-automator" => "🔧",
            "security-agent" => "🔒",
            "agentmaker" => "🏗️",
            "studio-coach" => "🎯",
            "joker" => "🃏",
            "playwright-test-validator" => "🎭",
            "performance-optimizer" => "⚡",
            "documentation-specialist" => "📚",
            "code-reviewer" => "👁️",
            "test-writer-fixer" => "🧪",
            "frontend-developer" => "🎨",
            "backend-developer" => "⚙️",
            _ => "🤖",
        }
    }
}

impl MessageWidget for TaskWidget {
    fn can_handle(&self, event: &AgentEvent) -> bool {
        matches!(event, AgentEvent::ToolCall { name, .. } if name == "Task")
    }

    fn render(&self, event: AgentEvent, container_name: &str, session_id: Uuid) -> WidgetOutput {
        if let AgentEvent::ToolCall { id, name: _, input, description: _ } = event {
            let mut entries = Vec::new();

            // Extract task parameters
            let task_desc = input.get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("Task");

            let agent_type = input.get("subagent_type")
                .and_then(|v| v.as_str())
                .unwrap_or("general-purpose");

            let prompt = input.get("prompt")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            // Header with agent type
            let icon = Self::get_agent_icon(agent_type);
            let header = format!("🚀 Spawning Agent: {} {}", icon, agent_type);
            entries.push(
                helpers::create_log_entry(
                    LogEntryLevel::Info,
                    container_name,
                    header,
                    session_id,
                    "tool_call",
                )
                .with_metadata("tool_id", &id)
                .with_metadata("tool_name", "Task")
                .with_metadata("agent_type", agent_type)
            );

            // Task description
            entries.push(
                LogEntry::new(
                    LogEntryLevel::Info,
                    container_name.to_string(),
                    format!("  📋 Task: {}", task_desc),
                )
                .with_session(session_id)
                .with_metadata("event_type", "task_description")
            );

            // Show prompt preview (first few lines)
            if !prompt.is_empty() {
                entries.push(
                    LogEntry::new(
                        LogEntryLevel::Debug,
                        container_name.to_string(),
                        "  💭 Instructions:".to_string(),
                    )
                    .with_session(session_id)
                    .with_metadata("event_type", "task_prompt")
                );

                // Show first 3 lines of prompt
                for (i, line) in prompt.lines().take(3).enumerate() {
                    let truncated = if line.len() > 80 {
                        format!("{}...", &line[..80])
                    } else {
                        line.to_string()
                    };
                    entries.push(
                        LogEntry::new(
                            LogEntryLevel::Debug,
                            container_name.to_string(),
                            format!("     {}", truncated),
                        )
                        .with_session(session_id)
                        .with_metadata("event_type", "task_prompt_line")
                        .with_metadata("line_num", &i.to_string())
                    );
                }

                if prompt.lines().count() > 3 {
                    entries.push(
                        LogEntry::new(
                            LogEntryLevel::Debug,
                            container_name.to_string(),
                            format!("     ... ({} more lines)", prompt.lines().count() - 3),
                        )
                        .with_session(session_id)
                        .with_metadata("event_type", "task_prompt_more")
                    );
                }
            }

            // Add status indicator
            entries.push(
                LogEntry::new(
                    LogEntryLevel::Info,
                    container_name.to_string(),
                    "  ⏳ Agent working...".to_string(),
                )
                .with_session(session_id)
                .with_metadata("event_type", "task_status")
            );

            WidgetOutput::MultiLine(entries)
        } else {
            WidgetOutput::Simple(
                helpers::create_log_entry(
                    LogEntryLevel::Error,
                    container_name,
                    "Invalid event for TaskWidget".to_string(),
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

            // Extract task parameters
            let task_desc = input.get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("Task");

            let agent_type = input.get("subagent_type")
                .and_then(|v| v.as_str())
                .unwrap_or("general-purpose");

            // Header with agent type
            let icon = Self::get_agent_icon(agent_type);
            let header = format!("🚀 Spawning Agent: {} {}", icon, agent_type);
            header_entries.push(
                helpers::create_log_entry(
                    LogEntryLevel::Info,
                    container_name,
                    header,
                    session_id,
                    "tool_call",
                )
                .with_metadata("tool_id", &id)
                .with_metadata("tool_name", "Task")
                .with_metadata("agent_type", agent_type)
            );

            // Task description as part of header
            let task_entry = LogEntry::new(
                LogEntryLevel::Info,
                container_name.to_string(),
                format!("📋 Task: {}", task_desc),
            )
            .with_session(session_id)
            .with_metadata("event_type", "task_description")
            .with_metadata("tool_id", &id);

            header_entries.push(task_entry);

            // Process result if available
            if let Some(tool_result) = result {
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
                        // Simple text output
                        let level = if tool_result.is_error {
                            LogEntryLevel::Error
                        } else {
                            LogEntryLevel::Info
                        };

                        for line in content_str.lines() {
                            content_entries.push(
                                LogEntry::new(
                                    level,
                                    container_name.to_string(),
                                    line.to_string(),
                                )
                                .with_session(session_id)
                                .with_metadata("agent_output", "true")
                                .with_metadata("tool_id", &id)
                            );
                        }
                    }
                } else if tool_result.is_error {
                    // Error with no content
                    content_entries.push(
                        LogEntry::new(
                            LogEntryLevel::Error,
                            container_name.to_string(),
                            "❌ Agent failed with no output".to_string(),
                        )
                        .with_session(session_id)
                        .with_metadata("tool_id", &id)
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
            WidgetOutput::Simple(
                helpers::create_log_entry(
                    LogEntryLevel::Error,
                    container_name,
                    "Invalid event for TaskWidget".to_string(),
                    session_id,
                    "error",
                )
            )
        }
    }

    fn name(&self) -> &'static str {
        "TaskWidget"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_task_widget_can_handle() {
        let widget = TaskWidget::new();

        let task_event = AgentEvent::ToolCall {
            id: "test".to_string(),
            name: "Task".to_string(),
            input: json!({}),
            description: None,
        };
        assert!(widget.can_handle(&task_event));

        let other_event = AgentEvent::ToolCall {
            id: "test".to_string(),
            name: "Bash".to_string(),
            input: json!({}),
            description: None,
        };
        assert!(!widget.can_handle(&other_event));
    }

    #[test]
    fn test_task_widget_render() {
        let widget = TaskWidget::new();
        let event = AgentEvent::ToolCall {
            id: "task_123".to_string(),
            name: "Task".to_string(),
            input: json!({
                "description": "Research codebase",
                "subagent_type": "web-search-researcher",
                "prompt": "Find information about React hooks\nLook for best practices\nCheck official docs"
            }),
            description: None,
        };

        let output = widget.render(event, "test-container", Uuid::nil());

        match output {
            WidgetOutput::MultiLine(entries) => {
                assert!(!entries.is_empty());
                assert!(entries[0].message.contains("🚀 Spawning Agent"));
                assert!(entries[0].message.contains("🌐")); // Web search icon
                assert!(entries[0].message.contains("web-search-researcher"));
                assert!(entries.iter().any(|e| e.message.contains("Research codebase")));
                assert!(entries.iter().any(|e| e.message.contains("⏳ Agent working")));
            }
            _ => panic!("Expected MultiLine output"),
        }
    }

    #[test]
    fn test_task_widget_render_with_result() {
        let widget = TaskWidget::new();
        let event = AgentEvent::ToolCall {
            id: "task_123".to_string(),
            name: "Task".to_string(),
            input: json!({
                "description": "Research React hooks",
                "subagent_type": "web-search-researcher"
            }),
            description: None,
        };

        let result = ToolResult {
            tool_use_id: "task_123".to_string(),
            content: json!({"content": "# Research Results\n\n* Found 5 articles about React hooks\n* Best practices documented\n* **Important**: Use useState for state management"}),
            is_error: false,
        };

        let output = widget.render_with_result(event, Some(result), "test-container", Uuid::nil());

        match output {
            WidgetOutput::Hierarchical { header, content, collapsed } => {
                // Check header
                assert!(!header.is_empty());
                assert!(header[0].message.contains("🚀 Spawning Agent"));
                assert!(header[0].message.contains("🌐")); // Web search icon
                assert!(header.iter().any(|e| e.message.contains("📋 Task: Research React hooks")));

                // Check content (should be parsed as markdown)
                assert!(!content.is_empty());
                assert!(!collapsed);

                // Should contain markdown-parsed content
                let content_text: String = content.iter().map(|e| &e.message).cloned().collect::<Vec<String>>().join("\n");
                assert!(content_text.contains("Research Results"));
                assert!(content_text.contains("Found 5 articles"));
            }
            _ => panic!("Expected Hierarchical output"),
        }
    }

    #[test]
    fn test_task_widget_render_with_error_result() {
        let widget = TaskWidget::new();
        let event = AgentEvent::ToolCall {
            id: "task_456".to_string(),
            name: "Task".to_string(),
            input: json!({
                "description": "Failed task",
                "subagent_type": "general-purpose"
            }),
            description: None,
        };

        let result = ToolResult {
            tool_use_id: "task_456".to_string(),
            content: json!(null),
            is_error: true,
        };

        let output = widget.render_with_result(event, Some(result), "test-container", Uuid::nil());

        match output {
            WidgetOutput::Hierarchical { header, content, collapsed: _ } => {
                assert!(!header.is_empty());
                assert!(header[0].message.contains("🚀 Spawning Agent"));
                assert!(header[0].message.contains("🤖")); // General purpose icon

                // Should have error content
                assert!(!content.is_empty());
                assert!(content.iter().any(|e| e.message.contains("❌ Agent failed with no output")));
            }
            _ => panic!("Expected Hierarchical output"),
        }
    }
}
