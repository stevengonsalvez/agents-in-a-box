// ABOUTME: Type definitions for Claude API integration

#![allow(dead_code)]

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClaudeRole {
    #[serde(rename = "user")]
    User,
    #[serde(rename = "assistant")]
    Assistant,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaudeMessage {
    pub role: ClaudeRole,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<chrono::DateTime<chrono::Utc>>,
}

impl ClaudeMessage {
    pub fn user(content: String) -> Self {
        Self {
            role: ClaudeRole::User,
            content,
            timestamp: Some(chrono::Utc::now()),
        }
    }

    pub fn assistant(content: String) -> Self {
        Self {
            role: ClaudeRole::Assistant,
            content,
            timestamp: Some(chrono::Utc::now()),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ClaudeRequest {
    pub model: String,
    pub messages: Vec<ClaudeMessage>,
    pub max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,
}

impl Default for ClaudeRequest {
    fn default() -> Self {
        Self {
            model: "claude-3-5-sonnet-20241022".to_string(),
            messages: Vec::new(),
            max_tokens: 4096,
            system: None,
            temperature: Some(0.7),
            stream: Some(false),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ClaudeResponse {
    pub id: String,
    pub content: Vec<ClaudeContent>,
    pub model: String,
    pub role: ClaudeRole,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<String>,
    pub usage: ClaudeUsage,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ClaudeContent {
    #[serde(rename = "type")]
    pub content_type: String,
    pub text: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ClaudeUsage {
    pub input_tokens: u32,
    pub output_tokens: u32,
}

// Streaming response types
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub enum ClaudeStreamingEvent {
    #[serde(rename = "message_start")]
    MessageStart { message: ClaudeStreamingMessage },
    #[serde(rename = "content_block_start")]
    ContentBlockStart {
        index: u32,
        content_block: ClaudeStreamingContentBlock,
    },
    #[serde(rename = "content_block_delta")]
    ContentBlockDelta {
        index: u32,
        delta: ClaudeStreamingDelta,
    },
    #[serde(rename = "content_block_stop")]
    ContentBlockStop { index: u32 },
    #[serde(rename = "message_delta")]
    MessageDelta { delta: ClaudeStreamingMessageDelta },
    #[serde(rename = "message_stop")]
    MessageStop,
    #[serde(rename = "error")]
    Error { error: ClaudeApiError },
}

#[derive(Debug, Clone, Deserialize)]
pub struct ClaudeStreamingMessage {
    pub id: String,
    pub model: String,
    pub role: ClaudeRole,
    pub usage: ClaudeUsage,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ClaudeStreamingContentBlock {
    #[serde(rename = "type")]
    pub block_type: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ClaudeStreamingDelta {
    #[serde(rename = "type")]
    pub delta_type: String,
    pub text: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ClaudeStreamingMessageDelta {
    pub stop_reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ClaudeApiError {
    pub message: String,
    #[serde(rename = "type")]
    pub error_type: String,
}

// Chat session state
#[derive(Debug, Clone)]
pub struct ClaudeChatSession {
    pub session_id: uuid::Uuid,
    pub messages: Vec<ClaudeMessage>,
    pub is_streaming: bool,
    pub current_response: Option<String>,
    pub total_tokens_used: u32,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub last_activity: chrono::DateTime<chrono::Utc>,
}

impl ClaudeChatSession {
    pub fn new(session_id: uuid::Uuid) -> Self {
        let now = chrono::Utc::now();
        Self {
            session_id,
            messages: Vec::new(),
            is_streaming: false,
            current_response: None,
            total_tokens_used: 0,
            created_at: now,
            last_activity: now,
        }
    }

    pub fn add_message(&mut self, message: ClaudeMessage) {
        self.messages.push(message);
        self.last_activity = chrono::Utc::now();
    }

    pub fn start_streaming(&mut self) {
        self.is_streaming = true;
        self.current_response = Some(String::new());
        self.last_activity = chrono::Utc::now();
    }

    pub fn append_to_current_response(&mut self, text: &str) {
        if let Some(ref mut response) = self.current_response {
            response.push_str(text);
        }
        self.last_activity = chrono::Utc::now();
    }

    pub fn finish_streaming(&mut self) {
        if let Some(response) = self.current_response.take() {
            self.add_message(ClaudeMessage::assistant(response));
        }
        self.is_streaming = false;
    }

    pub fn get_conversation_context(&self, max_messages: usize) -> Vec<ClaudeMessage> {
        // Return the last N messages for context, excluding system messages
        self.messages.iter().rev().take(max_messages).rev().cloned().collect()
    }
}

// Authentication configuration
#[derive(Debug, Clone)]
pub struct ClaudeAuth {
    pub api_key: Option<String>,
    pub oauth_token: Option<String>,
    pub base_url: String,
}

impl Default for ClaudeAuth {
    fn default() -> Self {
        Self {
            api_key: std::env::var("ANTHROPIC_API_KEY").ok(),
            oauth_token: None,
            base_url: "https://api.anthropic.com".to_string(),
        }
    }
}

impl ClaudeAuth {
    pub fn from_api_key(api_key: String) -> Self {
        Self {
            api_key: Some(api_key),
            oauth_token: None,
            base_url: "https://api.anthropic.com".to_string(),
        }
    }

    pub fn from_oauth_token(oauth_token: String) -> Self {
        Self {
            api_key: None,
            oauth_token: Some(oauth_token),
            base_url: "https://api.anthropic.com".to_string(),
        }
    }

    pub fn is_configured(&self) -> bool {
        self.api_key.is_some() || self.oauth_token.is_some()
    }

    pub fn get_auth_header(&self) -> Option<String> {
        if let Some(ref api_key) = self.api_key {
            Some(format!("Bearer {}", api_key))
        } else if let Some(ref oauth_token) = self.oauth_token {
            Some(format!("Bearer {}", oauth_token))
        } else {
            None
        }
    }
}
