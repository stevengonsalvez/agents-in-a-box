// ABOUTME: Common types for agent output parsing - unified representation across different AI agents
// This module defines the common event types that all agent parsers convert to

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

/// Structured payloads extracted from JSON for richer rendering
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum StructuredPayload {
    /// Todo list with counts and optional title
    TodoList {
        title: Option<String>,
        items: Vec<TodoItem>,
        pending: u32,
        in_progress: u32,
        done: u32,
    },
    /// File path list (e.g., glob results)
    GlobResults { paths: Vec<String>, total: usize },
    /// Generic pretty-printed JSON fallback
    PrettyJson(String),
}

/// Single todo item
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TodoItem {
    pub text: String,
    pub status: String, // "pending" | "in_progress" | "done"
}

/// Unified representation of events from AI agents
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AgentEvent {
    /// Initial session information
    SessionInfo {
        model: String,
        tools: Vec<String>,
        session_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        mcp_servers: Option<Vec<McpServerInfo>>,
    },

    /// Agent is thinking (for agents that expose thinking)
    Thinking { content: String },

    /// Text message from agent
    Message {
        content: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },

    /// Streaming text delta (incremental updates)
    StreamingText {
        delta: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        message_id: Option<String>,
    },

    /// Tool call initiated by agent
    ToolCall {
        id: String,
        name: String,
        input: Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        description: Option<String>,
    },

    /// Result from a tool call
    ToolResult {
        tool_use_id: String,
        content: String,
        is_error: bool,
    },

    /// Error event
    Error {
        message: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        code: Option<String>,
    },

    /// Usage statistics
    Usage {
        input_tokens: u32,
        output_tokens: u32,
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_tokens: Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        total_cost: Option<f64>,
    },

    /// Custom event for agent-specific features
    Custom { event_type: String, data: Value },

    /// Structured event for rich rendering (todos, paths, etc.)
    Structured(StructuredPayload),
}

/// MCP Server information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerInfo {
    pub name: String,
    pub status: String,
}

/// State tracking for streaming parsers
#[derive(Debug, Clone, Default)]
pub struct ParserState {
    /// Current message being built
    pub current_message: Option<String>,
    /// Current message ID for streaming
    pub current_message_id: Option<String>,
    /// Active tool calls waiting for results
    pub active_tool_calls: HashMap<String, ToolCallInfo>,
    /// Buffer for incomplete JSON lines
    pub line_buffer: String,
    /// Whether we're in a thinking block
    pub in_thinking: bool,
}

/// Information about an active tool call
#[derive(Debug, Clone)]
pub struct ToolCallInfo {
    pub id: String,
    pub name: String,
    pub started_at: chrono::DateTime<chrono::Utc>,
}

/// Trait for parsing agent-specific output formats
pub trait AgentOutputParser: Send + Sync {
    /// Parse a line of output and return any complete events
    fn parse_line(&mut self, line: &str) -> Result<Vec<AgentEvent>, String>;

    /// Flush any pending events (e.g., incomplete streaming text)
    fn flush(&mut self) -> Vec<AgentEvent>;

    /// Get the agent type this parser handles
    fn agent_type(&self) -> &str;

    /// Reset parser state
    fn reset(&mut self);
}

/// Factory for creating appropriate parser based on detected format
pub struct ParserFactory;

impl ParserFactory {
    /// Create a parser based on detected output format
    pub fn create_parser(first_line: &str) -> Box<dyn AgentOutputParser> {
        // Try to detect JSON format, allowing timestamp prefixes
        let content = if let Some(start) = first_line.find('{') {
            &first_line[start..]
        } else {
            first_line
        };
        if content.starts_with('{') && content.contains("\"type\"") {
            Box::new(crate::agent_parsers::claude_json::ClaudeJsonParser::new())
        } else {
            // Default to plain text parser
            Box::new(crate::agent_parsers::plain_text::PlainTextParser::new())
        }
    }

    /// Create a parser for a specific agent type
    pub fn create_for_agent(agent_type: &str) -> Box<dyn AgentOutputParser> {
        match agent_type.to_lowercase().as_str() {
            "claude" | "claude-json" => {
                Box::new(crate::agent_parsers::claude_json::ClaudeJsonParser::new())
            }
            "plain" | "text" => Box::new(crate::agent_parsers::plain_text::PlainTextParser::new()),
            _ => Box::new(crate::agent_parsers::plain_text::PlainTextParser::new()),
        }
    }
}

#[cfg(test)]
mod parser_factory_tests {
    use super::ParserFactory;

    #[test]
    fn detects_json_with_timestamp_prefix() {
        let line = "2025-09-08T19:20:30.123Z {\"type\":\"assistant\"}";
        let parser = ParserFactory::create_parser(line);
        assert_eq!(parser.agent_type(), "claude-json");
    }
}
