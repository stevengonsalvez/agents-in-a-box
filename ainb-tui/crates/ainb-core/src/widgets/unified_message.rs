// ABOUTME: Adapter layer between AgentEvent enum and flexible message structure
// Provides unified message handling with content blocks and metadata support

use crate::agent_parsers::{AgentEvent, types::StructuredPayload};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

/// Unified message structure that bridges AgentEvent and flexible message handling
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnifiedMessage {
    /// The original event that was converted
    pub original_event: AgentEvent,
    /// The type of message this represents
    pub message_type: MessageType,
    /// Content blocks that make up the message
    pub content_blocks: Vec<ContentBlock>,
    /// Additional metadata for routing and processing
    pub metadata: HashMap<String, Value>,
}

/// Types of messages in the unified system
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MessageType {
    System,
    Assistant,
    User,
    ToolUse,
    ToolResult,
    Thinking,
    Summary,
    Error,
}

/// Individual content blocks within a message
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ContentBlock {
    Text(String),
    Tool {
        id: String,
        name: String,
        input: Value,
    },
    Result {
        content: String,
        is_error: bool,
    },
    Thinking {
        content: String,
    },
}

impl From<AgentEvent> for UnifiedMessage {
    fn from(event: AgentEvent) -> Self {
        let mut metadata = HashMap::new();

        let (message_type, content_blocks) = match &event {
            AgentEvent::SessionInfo { model, tools, session_id, mcp_servers } => {
                metadata.insert("model".to_string(), Value::String(model.clone()));
                metadata.insert("session_id".to_string(), Value::String(session_id.clone()));
                metadata.insert("tools".to_string(), Value::Array(tools.iter().map(|t| Value::String(t.clone())).collect()));
                if let Some(servers) = mcp_servers {
                    metadata.insert("mcp_servers".to_string(), serde_json::to_value(servers).unwrap_or(Value::Null));
                }

                (
                    MessageType::System,
                    vec![ContentBlock::Text(format!("Session started with model: {}", model))],
                )
            }

            AgentEvent::Thinking { content } => {
                (
                    MessageType::Thinking,
                    vec![ContentBlock::Thinking {
                        content: content.clone(),
                    }],
                )
            }

            AgentEvent::Message { content, id } => {
                if let Some(msg_id) = id {
                    metadata.insert("message_id".to_string(), Value::String(msg_id.clone()));
                }

                (
                    MessageType::Assistant,
                    vec![ContentBlock::Text(content.clone())],
                )
            }

            AgentEvent::StreamingText { delta, message_id } => {
                if let Some(msg_id) = message_id {
                    metadata.insert("message_id".to_string(), Value::String(msg_id.clone()));
                }
                metadata.insert("streaming".to_string(), Value::Bool(true));

                (
                    MessageType::Assistant,
                    vec![ContentBlock::Text(delta.clone())],
                )
            }

            AgentEvent::ToolCall { id, name, input, description } => {
                metadata.insert("tool_id".to_string(), Value::String(id.clone()));
                metadata.insert("tool_name".to_string(), Value::String(name.clone()));
                if let Some(desc) = description {
                    metadata.insert("description".to_string(), Value::String(desc.clone()));
                }

                (
                    MessageType::ToolUse,
                    vec![ContentBlock::Tool {
                        id: id.clone(),
                        name: name.clone(),
                        input: input.clone(),
                    }],
                )
            }

            AgentEvent::ToolResult { tool_use_id, content, is_error } => {
                metadata.insert("tool_use_id".to_string(), Value::String(tool_use_id.clone()));
                metadata.insert("is_error".to_string(), Value::Bool(*is_error));

                (
                    MessageType::ToolResult,
                    vec![ContentBlock::Result {
                        content: content.clone(),
                        is_error: *is_error,
                    }],
                )
            }

            AgentEvent::Error { message, code } => {
                metadata.insert("error".to_string(), Value::String(message.clone()));
                if let Some(error_code) = code {
                    metadata.insert("error_code".to_string(), Value::String(error_code.clone()));
                }

                (
                    MessageType::Error,
                    vec![ContentBlock::Text(format!("Error: {}", message))],
                )
            }

            AgentEvent::Usage { input_tokens, output_tokens, cache_tokens, total_cost } => {
                metadata.insert("input_tokens".to_string(), Value::Number(serde_json::Number::from(*input_tokens)));
                metadata.insert("output_tokens".to_string(), Value::Number(serde_json::Number::from(*output_tokens)));
                if let Some(cache) = cache_tokens {
                    metadata.insert("cache_tokens".to_string(), Value::Number(serde_json::Number::from(*cache)));
                }
                if let Some(cost) = total_cost {
                    metadata.insert("total_cost".to_string(), Value::Number(serde_json::Number::from_f64(*cost).unwrap_or(serde_json::Number::from(0))));
                }
                (MessageType::System, vec![])
            }

            AgentEvent::Custom { event_type, data } => {
                metadata.insert("event_type".to_string(), Value::String(event_type.clone()));
                metadata.insert("custom_data".to_string(), data.clone());

                (
                    MessageType::System,
                    vec![ContentBlock::Text(format!("Custom event: {}", event_type))],
                )
            }

            AgentEvent::Structured(payload) => {
                match payload {
                    StructuredPayload::TodoList { title, items: _, pending, in_progress, done } => {
                        metadata.insert("structured_type".to_string(), Value::String("todo_list".to_string()));
                        metadata.insert("pending".to_string(), Value::Number((*pending).into()));
                        metadata.insert("in_progress".to_string(), Value::Number((*in_progress).into()));
                        metadata.insert("done".to_string(), Value::Number((*done).into()));

                        let summary = if let Some(t) = title {
                            format!("Todo List: {} ({} pending, {} in progress, {} done)", t, pending, in_progress, done)
                        } else {
                            format!("Todo List: {} pending, {} in progress, {} done", pending, in_progress, done)
                        };

                        (
                            MessageType::Summary,
                            vec![ContentBlock::Text(summary)],
                        )
                    }
                    StructuredPayload::GlobResults { paths: _, total } => {
                        metadata.insert("structured_type".to_string(), Value::String("glob_results".to_string()));
                        metadata.insert("total".to_string(), Value::Number((*total).into()));

                        let summary = format!("Found {} file(s)", total);
                        (
                            MessageType::Summary,
                            vec![ContentBlock::Text(summary)],
                        )
                    }
                    StructuredPayload::PrettyJson(json_str) => {
                        metadata.insert("structured_type".to_string(), Value::String("pretty_json".to_string()));

                        (
                            MessageType::System,
                            vec![ContentBlock::Text(json_str.clone())],
                        )
                    }
                }
            }
        };

        UnifiedMessage {
            original_event: event,
            message_type,
            content_blocks,
            metadata,
        }
    }
}

impl UnifiedMessage {
    /// Check if this message contains text content
    pub fn has_text_content(&self) -> bool {
        self.content_blocks.iter().any(|block| {
            matches!(
                block,
                ContentBlock::Text(_) | ContentBlock::Thinking { .. }
            )
        })
    }

    /// Get all text content from the message
    pub fn get_text_content(&self) -> Vec<String> {
        self.content_blocks
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text(text) => Some(text.clone()),
                ContentBlock::Thinking { content } => Some(content.clone()),
                ContentBlock::Result { content, .. } => Some(content.clone()),
                _ => None,
            })
            .collect()
    }

    /// Check if this is a tool-related message
    pub fn is_tool_message(&self) -> bool {
        matches!(
            self.message_type,
            MessageType::ToolUse | MessageType::ToolResult
        )
    }

    /// Check if this is an error message
    pub fn is_error(&self) -> bool {
        self.message_type == MessageType::Error
            || self.content_blocks.iter().any(|block| {
                matches!(block, ContentBlock::Result { is_error: true, .. })
            })
    }

    /// Check if this is a system control message
    pub fn is_system_control(&self) -> bool {
        self.message_type == MessageType::System
            && matches!(
                &self.original_event,
                AgentEvent::SessionInfo { .. }
                    | AgentEvent::Usage { .. }
                    | AgentEvent::Custom { .. }
            )
    }

    /// Get the primary content as a string if available
    pub fn primary_content(&self) -> Option<String> {
        self.content_blocks.first().and_then(|block| match block {
            ContentBlock::Text(text) => Some(text.clone()),
            ContentBlock::Thinking { content } => Some(content.clone()),
            ContentBlock::Result { content, .. } => Some(content.clone()),
            ContentBlock::Tool { name, .. } => Some(format!("Tool: {}", name)),
        })
    }

    /// Route this message to the appropriate handler
    pub fn routing_key(&self) -> &str {
        match self.message_type {
            MessageType::System => "system",
            MessageType::Assistant => "assistant",
            MessageType::User => "user",
            MessageType::ToolUse => "tool.use",
            MessageType::ToolResult => "tool.result",
            MessageType::Thinking => "thinking",
            MessageType::Summary => "summary",
            MessageType::Error => "error",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use crate::agent_parsers::types::TodoItem;

    #[test]
    fn test_message_conversion() {
        let event = AgentEvent::Message {
            content: "Hello, world!".to_string(),
            id: Some("msg-123".to_string()),
        };

        let unified = UnifiedMessage::from(event.clone());

        assert_eq!(unified.message_type, MessageType::Assistant);
        assert_eq!(unified.content_blocks.len(), 1);
        assert!(matches!(
            &unified.content_blocks[0],
            ContentBlock::Text(text) if text == "Hello, world!"
        ));
        assert!(unified.has_text_content());
        assert_eq!(unified.routing_key(), "assistant");
        assert_eq!(unified.metadata.get("message_id").unwrap(), "msg-123");
    }

    #[test]
    fn test_streaming_text_conversion() {
        let event = AgentEvent::StreamingText {
            delta: "I can help with that.".to_string(),
            message_id: Some("stream-456".to_string()),
        };

        let unified = UnifiedMessage::from(event.clone());

        assert_eq!(unified.message_type, MessageType::Assistant);
        assert_eq!(unified.get_text_content(), vec!["I can help with that."]);
        assert_eq!(unified.routing_key(), "assistant");
        assert_eq!(unified.metadata.get("streaming").unwrap(), &json!(true));
    }

    #[test]
    fn test_tool_call_conversion() {
        let event = AgentEvent::ToolCall {
            id: "tool-123".to_string(),
            name: "calculator".to_string(),
            input: json!({"operation": "add", "a": 5, "b": 3}),
            description: Some("Calculate sum".to_string()),
        };

        let unified = UnifiedMessage::from(event.clone());

        assert_eq!(unified.message_type, MessageType::ToolUse);
        assert!(unified.is_tool_message());
        assert_eq!(unified.metadata.get("tool_name").unwrap(), "calculator");
        assert_eq!(unified.metadata.get("description").unwrap(), "Calculate sum");
        assert_eq!(unified.routing_key(), "tool.use");

        assert!(matches!(
            &unified.content_blocks[0],
            ContentBlock::Tool { name, .. } if name == "calculator"
        ));
    }

    #[test]
    fn test_tool_result_conversion() {
        let event = AgentEvent::ToolResult {
            tool_use_id: "tool-123".to_string(),
            content: "Result: 8".to_string(),
            is_error: false,
        };

        let unified = UnifiedMessage::from(event.clone());

        assert_eq!(unified.message_type, MessageType::ToolResult);
        assert!(unified.is_tool_message());
        assert!(!unified.is_error());
        assert_eq!(unified.get_text_content(), vec!["Result: 8"]);
        assert_eq!(unified.routing_key(), "tool.result");
    }

    #[test]
    fn test_error_tool_result() {
        let event = AgentEvent::ToolResult {
            tool_use_id: "tool-456".to_string(),
            content: "Error: Division by zero".to_string(),
            is_error: true,
        };

        let unified = UnifiedMessage::from(event.clone());

        assert!(unified.is_error());
        assert_eq!(unified.metadata.get("is_error").unwrap(), &json!(true));
    }

    #[test]
    fn test_thinking_message_conversion() {
        let event = AgentEvent::Thinking {
            content: "Let me think about this...".to_string(),
        };

        let unified = UnifiedMessage::from(event.clone());

        assert_eq!(unified.message_type, MessageType::Thinking);
        assert!(unified.has_text_content());
        assert_eq!(
            unified.primary_content(),
            Some("Let me think about this...".to_string())
        );
        assert_eq!(unified.routing_key(), "thinking");
    }

    #[test]
    fn test_session_info_conversion() {
        let event = AgentEvent::SessionInfo {
            model: "claude-3".to_string(),
            tools: vec!["calculator".to_string(), "web_search".to_string()],
            session_id: "session-789".to_string(),
            mcp_servers: None,
        };

        let unified = UnifiedMessage::from(event.clone());

        assert_eq!(unified.message_type, MessageType::System);
        assert!(unified.get_text_content()[0].contains("Session started with model: claude-3"));
        assert!(unified.is_system_control());
        assert_eq!(unified.routing_key(), "system");
    }

    #[test]
    fn test_usage_event_conversion() {
        let event = AgentEvent::Usage {
            input_tokens: 100,
            output_tokens: 250,
            cache_tokens: Some(50),
            total_cost: Some(0.005),
        };

        let unified = UnifiedMessage::from(event.clone());

        // Usage events are now filtered and return empty
        assert_eq!(unified.message_type, MessageType::System);
        assert!(unified.is_system_control());
        // Text content is empty since usage events are filtered
        assert!(unified.get_text_content().is_empty());
        // Metadata is still preserved for internal tracking
        assert_eq!(unified.metadata.get("input_tokens").unwrap(), &json!(100));
        assert_eq!(unified.metadata.get("cache_tokens").unwrap(), &json!(50));
    }

    #[test]
    fn test_error_event_conversion() {
        let event = AgentEvent::Error {
            message: "Connection timeout".to_string(),
            code: Some("TIMEOUT".to_string()),
        };

        let unified = UnifiedMessage::from(event.clone());

        assert_eq!(unified.message_type, MessageType::Error);
        assert!(unified.is_error());
        assert_eq!(
            unified.metadata.get("error").unwrap(),
            "Connection timeout"
        );
        assert_eq!(
            unified.metadata.get("error_code").unwrap(),
            "TIMEOUT"
        );
        assert!(unified.primary_content().unwrap().contains("Error:"));
        assert_eq!(unified.routing_key(), "error");
    }

    #[test]
    fn test_structured_todo_list_conversion() {
        let event = AgentEvent::Structured(StructuredPayload::TodoList {
            title: Some("My Tasks".to_string()),
            items: vec![
                TodoItem { text: "Task 1".to_string(), status: "pending".to_string() },
                TodoItem { text: "Task 2".to_string(), status: "in_progress".to_string() },
            ],
            pending: 1,
            in_progress: 1,
            done: 0,
        });

        let unified = UnifiedMessage::from(event.clone());

        assert_eq!(unified.message_type, MessageType::Summary);
        assert!(unified.primary_content().unwrap().contains("Todo List: My Tasks"));
        assert!(unified.primary_content().unwrap().contains("1 pending, 1 in progress, 0 done"));
        assert_eq!(unified.routing_key(), "summary");
    }

    #[test]
    fn test_structured_glob_results_conversion() {
        let event = AgentEvent::Structured(StructuredPayload::GlobResults {
            paths: vec!["file1.rs".to_string(), "file2.rs".to_string()],
            total: 2,
        });

        let unified = UnifiedMessage::from(event.clone());

        assert_eq!(unified.message_type, MessageType::Summary);
        assert_eq!(unified.primary_content(), Some("Found 2 file(s)".to_string()));
        assert_eq!(unified.metadata.get("total").unwrap(), &json!(2));
    }

    #[test]
    fn test_custom_event_conversion() {
        let event = AgentEvent::Custom {
            event_type: "special_event".to_string(),
            data: json!({"key": "value"}),
        };

        let unified = UnifiedMessage::from(event.clone());

        assert_eq!(unified.message_type, MessageType::System);
        assert!(unified.is_system_control());
        assert_eq!(unified.primary_content(), Some("Custom event: special_event".to_string()));
        assert_eq!(unified.metadata.get("event_type").unwrap(), "special_event");
    }

    #[test]
    fn test_metadata_preservation() {
        let event = AgentEvent::ToolCall {
            id: "complex-tool".to_string(),
            name: "data_processor".to_string(),
            input: json!({
                "mode": "batch",
                "items": [1, 2, 3]
            }),
            description: None,
        };

        let unified = UnifiedMessage::from(event.clone());

        assert_eq!(unified.metadata.len(), 2);
        assert_eq!(
            unified.metadata.get("tool_id").unwrap(),
            "complex-tool"
        );
        assert_eq!(
            unified.metadata.get("tool_name").unwrap(),
            "data_processor"
        );
    }

    #[test]
    fn test_serialization_roundtrip() {
        let event = AgentEvent::Thinking {
            content: "Analyzing...".to_string(),
        };
        let unified = UnifiedMessage::from(event);

        let serialized = serde_json::to_string(&unified).unwrap();
        let deserialized: UnifiedMessage = serde_json::from_str(&serialized).unwrap();

        assert_eq!(deserialized.message_type, MessageType::Thinking);
        assert_eq!(deserialized.get_text_content(), vec!["Analyzing..."]);
    }
}