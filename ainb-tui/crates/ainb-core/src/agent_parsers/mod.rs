// ABOUTME: Agent output parser module - provides modular parsing for different AI agent outputs
// Supports Claude JSON streaming, plain text, and extensible for future agents

pub mod claude_json;
pub mod plain_text;
pub mod types;

pub use claude_json::ClaudeJsonParser;
#[allow(unused_imports)]
pub use plain_text::PlainTextParser;
#[allow(unused_imports)]
pub use types::{AgentEvent, AgentOutputParser, McpServerInfo, ParserFactory, ParserState};
