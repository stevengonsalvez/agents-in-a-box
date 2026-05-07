// ABOUTME: Claude JSON stream parser - parses Claude's --output-format stream-json output
// Converts Claude-specific JSON events into unified AgentEvent types for display

use super::types::{
    AgentEvent, AgentOutputParser, McpServerInfo, ParserState, StructuredPayload, TodoItem,
    ToolCallInfo,
};
use serde_json::Value;
use tracing::{debug, warn};

/// Parser for Claude's stream-json output format
pub struct ClaudeJsonParser {
    state: ParserState,
}

impl ClaudeJsonParser {
    pub fn new() -> Self {
        Self {
            state: ParserState::default(),
        }
    }

    fn parse_json_event(&mut self, json_str: &str) -> Result<Vec<AgentEvent>, String> {
        let value: Value =
            serde_json::from_str(json_str).map_err(|e| format!("Failed to parse JSON: {}", e))?;

        let mut events = Vec::new();

        // Extract type field
        let event_type = value.get("type").and_then(|v| v.as_str()).unwrap_or("unknown");

        match event_type {
            "system" => {
                if let Some(subtype) = value.get("subtype").and_then(|v| v.as_str()) {
                    if subtype == "init" {
                        events.push(self.parse_system_init(&value)?);
                    }
                }
            }

            "assistant" => {
                events.extend(self.parse_assistant_message(&value)?);
            }

            "user" => {
                events.extend(self.parse_user_message(&value)?);
            }

            _ => {
                debug!("Unknown event type: {} - {}", event_type, json_str);
            }
        }

        Ok(events)
    }

    fn parse_system_init(&mut self, value: &Value) -> Result<AgentEvent, String> {
        let model = value.get("model").and_then(|v| v.as_str()).unwrap_or("unknown").to_string();

        let session_id = value.get("session_id").and_then(|v| v.as_str()).unwrap_or("").to_string();

        let tools = value
            .get("tools")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
            .unwrap_or_default();

        let mcp_servers = value.get("mcp_servers").and_then(|v| v.as_array()).map(|arr| {
            arr.iter()
                .filter_map(|server| {
                    let name = server.get("name")?.as_str()?;
                    let status = server.get("status")?.as_str()?;
                    Some(McpServerInfo {
                        name: name.to_string(),
                        status: status.to_string(),
                    })
                })
                .collect::<Vec<_>>()
        });

        Ok(AgentEvent::SessionInfo {
            model,
            tools,
            session_id,
            mcp_servers,
        })
    }

    fn parse_assistant_message(&mut self, value: &Value) -> Result<Vec<AgentEvent>, String> {
        let mut events = Vec::new();

        // Support both message.content and direct content formats
        let (message_source, content_array, message_id) =
            if let Some(message) = value.get("message") {
                // Format: {"type": "assistant", "message": {"content": [...]}}
                let message_id = message.get("id").and_then(|v| v.as_str()).map(String::from);
                self.state.current_message_id = message_id.clone();
                (
                    message,
                    message.get("content").and_then(|v| v.as_array()),
                    message_id,
                )
            } else if let Some(content) = value.get("content").and_then(|v| v.as_array()) {
                // Format: {"type": "assistant", "content": [...]}  (direct format)
                (value, Some(content), None)
            } else {
                return Ok(events);
            };

        if let Some(content_array) = content_array {
            for content_item in content_array {
                let content_type = content_item.get("type").and_then(|v| v.as_str()).unwrap_or("");

                match content_type {
                    "text" => {
                        if let Some(text) = content_item.get("text").and_then(|v| v.as_str()) {
                            // Check if this is a complete message or streaming delta
                            if self.state.current_message.is_some() {
                                // Streaming delta
                                self.state.current_message.as_mut().unwrap().push_str(text);
                                events.push(AgentEvent::StreamingText {
                                    delta: text.to_string(),
                                    message_id: message_id.clone(),
                                });
                            } else {
                                // Complete message
                                events.push(AgentEvent::Message {
                                    content: text.to_string(),
                                    id: message_id.clone(),
                                });
                            }
                        }
                    }

                    "tool_use" => {
                        let tool_id = content_item
                            .get("id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();

                        let tool_name = content_item
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown")
                            .to_string();

                        let input = content_item.get("input").cloned().unwrap_or(Value::Null);

                        // Extract description from input if available
                        let description =
                            input.get("description").and_then(|v| v.as_str()).map(String::from);

                        // Track active tool call
                        self.state.active_tool_calls.insert(
                            tool_id.clone(),
                            ToolCallInfo {
                                id: tool_id.clone(),
                                name: tool_name.clone(),
                                started_at: chrono::Utc::now(),
                            },
                        );

                        // Check if this tool outputs structured data we can parse
                        // TodoWrite, Glob, and other tools that output structured JSON
                        if tool_name == "TodoWrite" || tool_name == "Glob" {
                            if let Some(evt) = Self::parse_structured_from_value(&input) {
                                events.push(evt);
                            }
                        }

                        // Always show the ToolCall event for debugging and completeness
                        events.push(AgentEvent::ToolCall {
                            id: tool_id,
                            name: tool_name,
                            input,
                            description,
                        });
                    }

                    _ => {
                        debug!(
                            "Unknown content type in assistant message: {}",
                            content_type
                        );
                    }
                }
            }
        }

        // Check for usage information (from message_source)
        if let Some(usage) = message_source.get("usage") {
            let input_tokens =
                usage.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0) as u32;

            let output_tokens =
                usage.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(0) as u32;

            let cache_tokens =
                usage.get("cache_read_input_tokens").and_then(|v| v.as_u64()).map(|v| v as u32);

            events.push(AgentEvent::Usage {
                input_tokens,
                output_tokens,
                cache_tokens,
                total_cost: None, // Can be calculated externally if needed
            });
        }

        Ok(events)
    }

    fn parse_user_message(&mut self, value: &Value) -> Result<Vec<AgentEvent>, String> {
        let mut events = Vec::new();

        // Check for tool results in user messages
        if let Some(message) = value.get("message") {
            if let Some(content_array) = message.get("content").and_then(|v| v.as_array()) {
                for content_item in content_array {
                    if let Some(tool_result) = content_item.get("tool_result") {
                        let tool_use_id = content_item
                            .get("tool_use_id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();

                        let content = tool_result
                            .get("content")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();

                        let is_error =
                            tool_result.get("is_error").and_then(|v| v.as_bool()).unwrap_or(false);

                        // Remove from active tool calls
                        self.state.active_tool_calls.remove(&tool_use_id);

                        // Try to interpret content as structured JSON (todos/paths)
                        if let Ok(json) = serde_json::from_str::<Value>(&content) {
                            if let Some(evt) = Self::parse_structured_from_value(&json) {
                                events.push(evt);
                            } else {
                                events.push(AgentEvent::ToolResult {
                                    tool_use_id,
                                    content,
                                    is_error,
                                });
                            }
                        } else {
                            events.push(AgentEvent::ToolResult {
                                tool_use_id,
                                content,
                                is_error,
                            });
                        }
                    } else if content_item.get("type").and_then(|v| v.as_str())
                        == Some("tool_result")
                    {
                        // Alternative format for tool results
                        let tool_use_id = content_item
                            .get("tool_use_id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();

                        let content = content_item
                            .get("content")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();

                        let is_error =
                            content_item.get("is_error").and_then(|v| v.as_bool()).unwrap_or(false);

                        // Remove from active tool calls
                        self.state.active_tool_calls.remove(&tool_use_id);

                        // Try structured parse as well
                        if let Ok(json) = serde_json::from_str::<Value>(&content) {
                            if let Some(evt) = Self::parse_structured_from_value(&json) {
                                events.push(evt);
                            } else {
                                events.push(AgentEvent::ToolResult {
                                    tool_use_id,
                                    content,
                                    is_error,
                                });
                            }
                        } else {
                            events.push(AgentEvent::ToolResult {
                                tool_use_id,
                                content,
                                is_error,
                            });
                        }
                    }
                }
            }
        }

        Ok(events)
    }

    /// Attempt to map a generic JSON value into a StructuredPayload event
    fn parse_structured_from_value(v: &Value) -> Option<AgentEvent> {
        // Todos
        if let Some(todos_val) = v.get("todos").and_then(|t| t.as_array()) {
            let mut items: Vec<TodoItem> = Vec::new();
            let mut pending = 0u32;
            let mut in_progress = 0u32;
            let mut done = 0u32;
            for t in todos_val {
                let text = t
                    .get("text")
                    .or_else(|| t.get("task"))
                    .or_else(|| t.get("content"))
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string();
                let status =
                    t.get("status").and_then(|x| x.as_str()).unwrap_or("pending").to_string();
                match status.as_str() {
                    "done" | "completed" => done += 1,
                    "in_progress" | "active" => in_progress += 1,
                    _ => pending += 1,
                }
                items.push(TodoItem { text, status });
            }
            let title = v.get("title").and_then(|x| x.as_str()).map(|s| s.to_string());
            return Some(AgentEvent::Structured(StructuredPayload::TodoList {
                title,
                items,
                pending,
                in_progress,
                done,
            }));
        }
        // Paths
        if let Some(paths) = v.get("paths").and_then(|p| p.as_array()) {
            let mut out: Vec<String> = Vec::new();
            for p in paths {
                if let Some(s) = p.as_str() {
                    out.push(s.to_string());
                }
            }
            if !out.is_empty() {
                let total = v
                    .get("total")
                    .and_then(|t| t.as_u64())
                    .map(|u| u as usize)
                    .unwrap_or(out.len());
                return Some(AgentEvent::Structured(StructuredPayload::GlobResults {
                    paths: out,
                    total,
                }));
            }
        }

        // Detect Cargo.toml structure
        if let Some(package) = v.get("package").and_then(|p| p.as_object()) {
            let name = package.get("name").and_then(|n| n.as_str()).unwrap_or("unknown");
            let version = package.get("version").and_then(|v| v.as_str()).unwrap_or("0.0.0");
            let edition = package.get("edition").and_then(|e| e.as_str()).unwrap_or("2021");

            let json_str = format!("📦 Cargo: {} v{} (edition {})", name, version, edition);
            return Some(AgentEvent::Structured(StructuredPayload::PrettyJson(
                json_str,
            )));
        }

        // Generic key-value detection for Read tool results
        if v.is_object() && !v.as_object().unwrap().is_empty() {
            // Pretty print with 2-space indentation
            if let Ok(pretty) = serde_json::to_string_pretty(&v) {
                return Some(AgentEvent::Structured(StructuredPayload::PrettyJson(
                    pretty,
                )));
            }
        }

        None
    }
}

impl AgentOutputParser for ClaudeJsonParser {
    fn parse_line(&mut self, line: &str) -> Result<Vec<AgentEvent>, String> {
        // Handle incomplete lines by buffering
        let complete_line = if !self.state.line_buffer.is_empty() {
            let buffered = format!("{}{}", self.state.line_buffer, line);
            self.state.line_buffer.clear();
            buffered
        } else {
            line.to_string()
        };

        // Skip empty lines
        if complete_line.trim().is_empty() {
            return Ok(vec![]);
        }

        // Try to parse as JSON
        match self.parse_json_event(&complete_line) {
            Ok(events) => Ok(events),
            Err(e) => {
                // If parsing fails, it might be an incomplete line
                if line.ends_with('}') {
                    // Complete JSON that failed to parse
                    warn!(
                        "Failed to parse complete JSON line: {} - Error: {}",
                        complete_line, e
                    );
                    Err(e)
                } else {
                    // Incomplete line, buffer it
                    self.state.line_buffer = complete_line;
                    Ok(vec![])
                }
            }
        }
    }

    fn flush(&mut self) -> Vec<AgentEvent> {
        let mut events = Vec::new();

        // Flush any buffered message
        if let Some(message) = self.state.current_message.take() {
            events.push(AgentEvent::Message {
                content: message,
                id: self.state.current_message_id.take(),
            });
        }

        // Clear line buffer
        self.state.line_buffer.clear();

        events
    }

    fn agent_type(&self) -> &str {
        "claude-json"
    }

    fn reset(&mut self) {
        self.state = ParserState::default();
    }
}

impl Default for ClaudeJsonParser {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod structured_parsing_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_parse_todo_list() {
        let json = json!({
            "todos": [
                {"text": "Task 1", "status": "pending"},
                {"text": "Task 2", "status": "in_progress"},
                {"text": "Task 3", "status": "done"}
            ]
        });

        let event = ClaudeJsonParser::parse_structured_from_value(&json);
        assert!(matches!(
            event,
            Some(AgentEvent::Structured(StructuredPayload::TodoList { .. }))
        ));

        if let Some(AgentEvent::Structured(StructuredPayload::TodoList {
            items,
            pending,
            in_progress,
            done,
            ..
        })) = event
        {
            assert_eq!(items.len(), 3);
            assert_eq!(pending, 1);
            assert_eq!(in_progress, 1);
            assert_eq!(done, 1);
        }
    }

    #[test]
    fn test_parse_glob_results() {
        let json = json!({
            "paths": ["src/main.rs", "src/lib.rs", "Cargo.toml"],
            "total": 3
        });

        let event = ClaudeJsonParser::parse_structured_from_value(&json);
        assert!(matches!(
            event,
            Some(AgentEvent::Structured(
                StructuredPayload::GlobResults { .. }
            ))
        ));

        if let Some(AgentEvent::Structured(StructuredPayload::GlobResults { paths, total })) = event
        {
            assert_eq!(paths.len(), 3);
            assert_eq!(total, 3);
            assert!(paths.contains(&"src/main.rs".to_string()));
        }
    }

    #[test]
    fn test_parse_cargo_toml() {
        let json = json!({
            "package": {
                "name": "my-app",
                "version": "1.0.0",
                "edition": "2021"
            }
        });

        let event = ClaudeJsonParser::parse_structured_from_value(&json);
        assert!(matches!(
            event,
            Some(AgentEvent::Structured(StructuredPayload::PrettyJson(_)))
        ));

        if let Some(AgentEvent::Structured(StructuredPayload::PrettyJson(json_str))) = event {
            assert!(json_str.contains("📦 Cargo:"));
            assert!(json_str.contains("my-app"));
            assert!(json_str.contains("v1.0.0"));
            assert!(json_str.contains("edition 2021"));
        }
    }

    #[test]
    fn test_parse_generic_json() {
        let json = json!({
            "key1": "value1",
            "key2": 42,
            "nested": {
                "inner": "data"
            }
        });

        let event = ClaudeJsonParser::parse_structured_from_value(&json);
        assert!(matches!(
            event,
            Some(AgentEvent::Structured(StructuredPayload::PrettyJson(_)))
        ));

        if let Some(AgentEvent::Structured(StructuredPayload::PrettyJson(json_str))) = event {
            // Should be pretty-printed JSON
            assert!(json_str.contains("key1"));
            assert!(json_str.contains("value1"));
            assert!(json_str.contains("nested"));
        }
    }

    #[test]
    fn test_todo_status_variations() {
        // Test with 'completed' instead of 'done'
        let json = json!({
            "todos": [
                {"text": "Task 1", "status": "completed"},
                {"text": "Task 2", "status": "active"}
            ]
        });

        let event = ClaudeJsonParser::parse_structured_from_value(&json);
        if let Some(AgentEvent::Structured(StructuredPayload::TodoList {
            items,
            pending,
            done,
            in_progress,
            ..
        })) = event
        {
            assert_eq!(items.len(), 2);
            assert_eq!(done, 1); // 'completed' is now properly recognized as done
            assert_eq!(in_progress, 1); // 'active' is now properly recognized as in_progress
            assert_eq!(pending, 0); // No pending items since both are recognized
        }
    }

    #[test]
    fn test_todo_with_content_field() {
        // Test that we can parse todos with "content" field (used by TodoWrite)
        let json = json!({
            "todos": [
                {"content": "First task", "status": "pending"},
                {"content": "Second task", "status": "in_progress"},
                {"content": "Third task", "status": "done"}
            ]
        });

        let event = ClaudeJsonParser::parse_structured_from_value(&json).unwrap();

        if let AgentEvent::Structured(StructuredPayload::TodoList {
            items,
            pending,
            in_progress,
            done,
            ..
        }) = event
        {
            assert_eq!(items.len(), 3);
            assert_eq!(items[0].text, "First task");
            assert_eq!(items[1].text, "Second task");
            assert_eq!(items[2].text, "Third task");
            assert_eq!(pending, 1);
            assert_eq!(in_progress, 1);
            assert_eq!(done, 1);
        } else {
            panic!("Expected TodoList");
        }
    }

    #[test]
    fn test_todo_with_task_field() {
        // Test with 'task' field instead of 'text'
        let json = json!({
            "todos": [
                {"task": "Alternative field name", "status": "pending"}
            ]
        });

        let event = ClaudeJsonParser::parse_structured_from_value(&json);
        if let Some(AgentEvent::Structured(StructuredPayload::TodoList { items, .. })) = event {
            assert_eq!(items.len(), 1);
            assert_eq!(items[0].text, "Alternative field name");
        }
    }

    #[test]
    fn test_empty_json_object() {
        let json = json!({});

        let event = ClaudeJsonParser::parse_structured_from_value(&json);
        // Empty objects should return None
        assert!(event.is_none());
    }

    #[test]
    fn test_todo_write_tool_use() {
        let mut parser = ClaudeJsonParser::new();

        // Simulate TodoWrite tool_use event
        let line = r#"{"type":"assistant","content":[{"type":"tool_use","id":"123","name":"TodoWrite","input":{"todos":[{"content":"Write tests","status":"done","activeForm":"Writing tests"},{"content":"Implement feature","status":"in_progress","activeForm":"Implementing feature"},{"content":"Review code","status":"pending","activeForm":"Reviewing code"}]}}]}"#;

        let events = parser.parse_line(line).unwrap();

        // Should generate both Structured event and ToolCall event
        assert!(
            events.len() >= 2,
            "Expected at least 2 events, got {}",
            events.len()
        );

        // Check for structured todo list
        let has_structured_todo = events.iter().any(|e| {
            matches!(
                e,
                AgentEvent::Structured(StructuredPayload::TodoList { .. })
            )
        });
        assert!(
            has_structured_todo,
            "Should detect TodoWrite and create structured todo list"
        );

        // Check that ToolCall event is also generated
        let has_tool_call = events.iter().any(|e| matches!(e, AgentEvent::ToolCall { .. }));
        assert!(
            has_tool_call,
            "Should also generate ToolCall event for completeness"
        );

        // Verify the todo list content
        if let Some(AgentEvent::Structured(StructuredPayload::TodoList {
            items,
            done,
            in_progress,
            pending,
            ..
        })) = events.iter().find(|e| {
            matches!(
                e,
                AgentEvent::Structured(StructuredPayload::TodoList { .. })
            )
        }) {
            assert_eq!(items.len(), 3);
            assert_eq!(*done, 1);
            assert_eq!(*in_progress, 1);
            assert_eq!(*pending, 1);
        }
    }
}
