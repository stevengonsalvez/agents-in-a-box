// ABOUTME: Widget for rendering Model Context Protocol (MCP) tool calls
// Handles MCP server interactions and displays their results

use crate::agent_parsers::AgentEvent;
use crate::components::live_logs_stream::LogEntryLevel;
use serde_json::Value;
use uuid::Uuid;

use super::{MessageWidget, WidgetOutput, ToolResult, helpers};

pub struct McpWidget;

impl McpWidget {
    pub fn new() -> Self {
        Self
    }

    /// Extract the MCP server and method from the tool name (e.g., "mcp__sequential-thinking__sequentialthinking")
    fn parse_mcp_tool_name(name: &str) -> (String, String, String) {
        let parts: Vec<&str> = name.split("__").collect();
        if parts.len() >= 3 {
            (parts[1].to_string(), parts[2].to_string(), name.to_string())
        } else {
            ("unknown".to_string(), "unknown".to_string(), name.to_string())
        }
    }

    /// Format MCP input parameters for display
    fn format_mcp_input(input: &Value) -> Vec<String> {
        let mut lines = Vec::new();

        if let Some(obj) = input.as_object() {
            for (key, value) in obj {
                let value_str = match value {
                    Value::String(s) => {
                        if s.len() > 100 {
                            format!("\"{}...\"", &s[..100])
                        } else {
                            format!("\"{}\"", s)
                        }
                    }
                    Value::Number(n) => n.to_string(),
                    Value::Bool(b) => b.to_string(),
                    Value::Null => "null".to_string(),
                    Value::Array(arr) => format!("[{} items]", arr.len()),
                    Value::Object(obj) => format!("{{{}
 fields}}", obj.len()),
                };
                lines.push(format!("      {}: {}", key, value_str));
            }
        }

        lines
    }
}

impl MessageWidget for McpWidget {
    fn can_handle(&self, event: &AgentEvent) -> bool {
        // Check if this is an MCP tool call (starts with "mcp__")
        matches!(event, AgentEvent::ToolCall { name, .. } if name.starts_with("mcp__"))
    }

    fn render(&self, event: AgentEvent, container_name: &str, session_id: Uuid) -> WidgetOutput {
        if let AgentEvent::ToolCall { id: _, name, input, description: _ } = event {
            let mut entries = Vec::new();

            let (server, method, _full_name) = Self::parse_mcp_tool_name(&name);

            // Header with MCP icon
            entries.push(helpers::create_log_entry(
                LogEntryLevel::Info,
                container_name,
                format!("🔌 MCP: {}::{}", server, method),
                session_id,
                "mcp",
            ));

            // Show input parameters if available
            entries.push(helpers::create_log_entry(
                LogEntryLevel::Debug,
                container_name,
                "   Parameters:".to_string(),
                session_id,
                "mcp",
            ));

            let formatted_lines = Self::format_mcp_input(&input);
            for line in formatted_lines {
                entries.push(helpers::create_log_entry(
                    LogEntryLevel::Debug,
                    container_name,
                    line,
                    session_id,
                    "mcp",
                ));
            }

            // Add separator for visual clarity
            entries.push(helpers::create_separator(container_name, session_id));

            WidgetOutput::MultiLine(entries)
        } else {
            // Should not happen if can_handle works correctly
            WidgetOutput::Simple(
                helpers::create_log_entry(
                    LogEntryLevel::Error,
                    container_name,
                    "Invalid event for McpWidget".to_string(),
                    session_id,
                    "error",
                )
            )
        }
    }

    fn render_with_result(
        &self,
        event: AgentEvent,
        result: Option<ToolResult>,
        container_name: &str,
        session_id: Uuid,
    ) -> WidgetOutput {
        if let AgentEvent::ToolCall { id: _, name, input, description: _ } = event {
            let mut entries = Vec::new();

            let (server, method, _full_name) = Self::parse_mcp_tool_name(&name);

            // Header with MCP icon
            entries.push(helpers::create_log_entry(
                LogEntryLevel::Info,
                container_name,
                format!("🔌 MCP: {}::{}", server, method),
                session_id,
                "mcp",
            ));

            // Show input parameters if available
            entries.push(helpers::create_log_entry(
                LogEntryLevel::Debug,
                container_name,
                "   Parameters:".to_string(),
                session_id,
                "mcp",
            ));

            let formatted_lines = Self::format_mcp_input(&input);
            for line in formatted_lines {
                entries.push(helpers::create_log_entry(
                    LogEntryLevel::Debug,
                    container_name,
                    line,
                    session_id,
                    "mcp",
                ));
            }

            // Add separator for visual clarity
            entries.push(helpers::create_separator(container_name, session_id));

            // Then render the result if available
            if let Some(tool_result) = result {
                if tool_result.is_error {
                entries.push(helpers::create_log_entry(
                    LogEntryLevel::Error,
                    container_name,
                    "   ❌ MCP call failed".to_string(),
                    session_id,
                    "mcp_result",
                ));

                // Show error message
                if let Some(error_msg) = tool_result.content.as_str() {
                    entries.push(helpers::create_log_entry(
                        LogEntryLevel::Error,
                        container_name,
                        format!("   Error: {}", error_msg),
                        session_id,
                        "mcp_result",
                    ));
                }
            } else {
                entries.push(helpers::create_log_entry(
                    LogEntryLevel::Info,
                    container_name,
                    "   ✅ MCP call successful".to_string(),
                    session_id,
                    "mcp_result",
                ));

                // Show result based on content type
                match &tool_result.content {
                    Value::String(s) => {
                        // For short strings, show inline
                        if s.len() <= 200 && !s.contains('\n') {
                            entries.push(helpers::create_log_entry(
                                LogEntryLevel::Debug,
                                container_name,
                                format!("   Result: {}", s),
                                session_id,
                                "mcp_result",
                            ));
                        } else {
                            // For longer strings, show preview
                            entries.push(helpers::create_log_entry(
                                LogEntryLevel::Debug,
                                container_name,
                                "   Result:".to_string(),
                                session_id,
                                "mcp_result",
                            ));
                            for line in s.lines().take(10) {
                                entries.push(helpers::create_log_entry(
                                    LogEntryLevel::Debug,
                                    container_name,
                                    format!("      {}", line),
                                    session_id,
                                    "mcp_result",
                                ));
                            }
                            if s.lines().count() > 10 {
                                entries.push(helpers::create_log_entry(
                                    LogEntryLevel::Debug,
                                    container_name,
                                    "      ... (truncated)".to_string(),
                                    session_id,
                                    "mcp_result",
                                ));
                            }
                        }
                    }
                    Value::Object(obj) => {
                        entries.push(helpers::create_log_entry(
                            LogEntryLevel::Debug,
                            container_name,
                            format!("   Result: {} fields", obj.len()),
                            session_id,
                            "mcp_result",
                        ));
                        // Show first few fields
                        for (key, value) in obj.iter().take(5) {
                            let value_preview = match value {
                                Value::String(s) if s.len() > 50 => format!("\"{}...\"", &s[..50]),
                                Value::String(s) => format!("\"{}\"", s),
                                Value::Number(n) => n.to_string(),
                                Value::Bool(b) => b.to_string(),
                                Value::Null => "null".to_string(),
                                Value::Array(arr) => format!("[{} items]", arr.len()),
                                Value::Object(obj) => format!("{{{} fields}}", obj.len()),
                            };
                            entries.push(helpers::create_log_entry(
                                LogEntryLevel::Debug,
                                container_name,
                                format!("      {}: {}", key, value_preview),
                                session_id,
                                "mcp_result",
                            ));
                        }
                        if obj.len() > 5 {
                            entries.push(helpers::create_log_entry(
                                LogEntryLevel::Debug,
                                container_name,
                                format!("      ... and {} more fields", obj.len() - 5),
                                session_id,
                                "mcp_result",
                            ));
                        }
                    }
                    Value::Array(arr) => {
                        entries.push(helpers::create_log_entry(
                            LogEntryLevel::Debug,
                            container_name,
                            format!("   Result: {} items", arr.len()),
                            session_id,
                            "mcp_result",
                        ));
                    }
                    _ => {
                        entries.push(helpers::create_log_entry(
                            LogEntryLevel::Debug,
                            container_name,
                            format!("   Result: {:?}", tool_result.content),
                            session_id,
                            "mcp_result",
                        ));
                    }
                }
            }
            }

            WidgetOutput::MultiLine(entries)
        } else {
            // Should not happen if can_handle works correctly
            WidgetOutput::Simple(
                helpers::create_log_entry(
                    LogEntryLevel::Error,
                    container_name,
                    "Invalid event for McpWidget".to_string(),
                    session_id,
                    "error",
                )
            )
        }
    }

    fn name(&self) -> &'static str {
        "McpWidget"
    }
}

impl Default for McpWidget {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_mcp_widget_can_handle() {
        let widget = McpWidget::new();

        // Test MCP tool call
        let mcp_event = AgentEvent::ToolCall {
            id: "test".to_string(),
            name: "mcp__sequential-thinking__sequentialthinking".to_string(),
            input: json!({}),
            description: None,
        };
        assert!(widget.can_handle(&mcp_event), "Should handle MCP tool calls");

        // Test non-MCP tool call
        let other_event = AgentEvent::ToolCall {
            id: "test".to_string(),
            name: "Bash".to_string(),
            input: json!({}),
            description: None,
        };
        assert!(!widget.can_handle(&other_event), "Should not handle non-MCP tool calls");
    }

    #[test]
    fn test_mcp_widget_render() {
        let widget = McpWidget::new();
        let event = AgentEvent::ToolCall {
            id: "mcp_123".to_string(),
            name: "mcp__sequential-thinking__sequentialthinking".to_string(),
            input: json!({
                "prompt": "Test prompt",
                "context": "Test context"
            }),
            description: Some("Sequential thinking".to_string()),
        };

        let output = widget.render(event, "test-container", Uuid::nil());

        match output {
            WidgetOutput::MultiLine(entries) => {
                assert!(!entries.is_empty());
                // Check that the header contains the MCP icon and parsed server/method
                assert!(entries[0].message.contains("🔌 MCP: sequential-thinking::sequentialthinking"));
            }
            _ => panic!("Expected MultiLine output"),
        }
    }
}