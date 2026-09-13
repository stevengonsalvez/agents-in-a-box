// ABOUTME: Plain text parser - fallback parser for non-JSON agent outputs
// Converts plain text output into Message events

use super::types::{AgentEvent, AgentOutputParser, ParserState};

/// Simple parser for plain text output
pub struct PlainTextParser {
    state: ParserState,
}

impl PlainTextParser {
    pub fn new() -> Self {
        Self {
            state: ParserState::default(),
        }
    }
}

impl AgentOutputParser for PlainTextParser {
    fn parse_line(&mut self, line: &str) -> Result<Vec<AgentEvent>, String> {
        // For plain text, just accumulate lines into messages
        if line.trim().is_empty() {
            return Ok(vec![]);
        }

        // Each non-empty line becomes a message
        Ok(vec![AgentEvent::Message {
            content: line.to_string(),
            id: None,
        }])
    }

    fn flush(&mut self) -> Vec<AgentEvent> {
        // Nothing to flush for plain text
        vec![]
    }

    fn agent_type(&self) -> &str {
        "plain-text"
    }

    fn reset(&mut self) {
        self.state = ParserState::default();
    }
}

impl Default for PlainTextParser {
    fn default() -> Self {
        Self::new()
    }
}
