// ABOUTME: Claude API client implementation for direct communication with Anthropic API

#![allow(dead_code)]

use crate::claude::streaming::ClaudeStreamingResponse;
use crate::claude::types::{
    ClaudeAuth, ClaudeChatSession, ClaudeMessage, ClaudeRequest, ClaudeResponse,
};
use anyhow::{Context, Result, anyhow};
use reqwest::Client;
use serde_json;
use std::collections::HashMap;
// StreamExt imported but currently unused - may be needed for future streaming functionality
use tracing::{debug, error, info};
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct ClaudeApiClient {
    client: Client,
    auth: ClaudeAuth,
    base_url: String,
}

impl ClaudeApiClient {
    /// Create a new Claude API client with default configuration
    pub fn new() -> Result<Self> {
        let auth = ClaudeAuth::default();
        if !auth.is_configured() {
            return Err(anyhow!(
                "Claude API client requires authentication. Set ANTHROPIC_API_KEY environment variable or configure OAuth."
            ));
        }

        let client = Client::builder()
            .user_agent("agents-in-a-box/0.1.0")
            .timeout(std::time::Duration::from_secs(60))
            .build()
            .context("Failed to create HTTP client")?;

        Ok(Self {
            client,
            auth: auth.clone(),
            base_url: auth.base_url,
        })
    }

    /// Create a new Claude API client with custom authentication
    pub fn with_auth(auth: ClaudeAuth) -> Result<Self> {
        if !auth.is_configured() {
            return Err(anyhow!("Claude API client requires valid authentication"));
        }

        let client = Client::builder()
            .user_agent("agents-in-a-box/0.1.0")
            .timeout(std::time::Duration::from_secs(60))
            .build()
            .context("Failed to create HTTP client")?;

        Ok(Self {
            client,
            base_url: auth.base_url.clone(),
            auth,
        })
    }

    /// Send a single message and get a complete response
    pub async fn send_message(
        &self,
        message: &str,
        context: Option<&[ClaudeMessage]>,
    ) -> Result<String> {
        let mut messages = Vec::new();

        // Add context messages if provided
        if let Some(context_msgs) = context {
            messages.extend(context_msgs.iter().cloned());
        }

        // Add the user message
        messages.push(ClaudeMessage::user(message.to_string()));

        let request = ClaudeRequest {
            messages,
            stream: Some(false),
            ..Default::default()
        };

        let response = self.send_request(&request).await?;

        // Extract the text content from the response
        if let Some(content) = response.content.first() {
            Ok(content.text.clone())
        } else {
            Err(anyhow!("No content in Claude response"))
        }
    }

    /// Start a streaming conversation
    pub async fn stream_message(
        &self,
        message: &str,
        context: Option<&[ClaudeMessage]>,
    ) -> Result<ClaudeStreamingResponse> {
        let mut messages = Vec::new();

        // Add context messages if provided
        if let Some(context_msgs) = context {
            messages.extend(context_msgs.iter().cloned());
        }

        // Add the user message
        messages.push(ClaudeMessage::user(message.to_string()));

        let request = ClaudeRequest {
            messages,
            stream: Some(true),
            ..Default::default()
        };

        self.stream_request(&request).await
    }

    /// Send a request and get a complete response
    async fn send_request(&self, request: &ClaudeRequest) -> Result<ClaudeResponse> {
        let auth_header = self
            .auth
            .get_auth_header()
            .ok_or_else(|| anyhow!("No valid authentication configured"))?;

        debug!(
            "Sending Claude API request: {} messages",
            request.messages.len()
        );

        let response = self
            .client
            .post(&format!("{}/v1/messages", self.base_url))
            .header("Authorization", auth_header)
            .header("Content-Type", "application/json")
            .header("anthropic-version", "2023-06-01")
            .json(request)
            .send()
            .await
            .context("Failed to send request to Claude API")?;

        if !response.status().is_success() {
            let status = response.status();
            let error_text = response.text().await.unwrap_or_default();
            return Err(anyhow!("Claude API error {}: {}", status, error_text));
        }

        let claude_response: ClaudeResponse =
            response.json().await.context("Failed to parse Claude API response")?;

        debug!(
            "Received Claude response: {} content blocks",
            claude_response.content.len()
        );
        Ok(claude_response)
    }

    /// Send a streaming request
    async fn stream_request(&self, request: &ClaudeRequest) -> Result<ClaudeStreamingResponse> {
        let auth_header = self
            .auth
            .get_auth_header()
            .ok_or_else(|| anyhow!("No valid authentication configured"))?;

        debug!(
            "Starting Claude API streaming request: {} messages",
            request.messages.len()
        );

        let response = self
            .client
            .post(&format!("{}/v1/messages", self.base_url))
            .header("Authorization", auth_header)
            .header("Content-Type", "application/json")
            .header("anthropic-version", "2023-06-01")
            .header("Accept", "text/event-stream")
            .json(request)
            .send()
            .await
            .context("Failed to send streaming request to Claude API")?;

        if !response.status().is_success() {
            let status = response.status();
            let error_text = response.text().await.unwrap_or_default();
            return Err(anyhow!(
                "Claude API streaming error {}: {}",
                status,
                error_text
            ));
        }

        ClaudeStreamingResponse::from_response(response).await
    }

    /// Test the API connection
    pub async fn test_connection(&self) -> Result<()> {
        info!("Testing Claude API connection");

        match self.send_message("Hello", None).await {
            Ok(response) => {
                info!(
                    "Claude API connection successful, response length: {} chars",
                    response.len()
                );
                Ok(())
            }
            Err(e) => {
                error!("Claude API connection failed: {}", e);
                Err(e)
            }
        }
    }

    /// Get available models (if supported by API)
    pub async fn get_models(&self) -> Result<Vec<String>> {
        // Anthropic API doesn't currently provide a models endpoint
        // Return the known models
        Ok(vec![
            "claude-3-5-sonnet-20241022".to_string(),
            "claude-3-opus-20240229".to_string(),
            "claude-3-haiku-20240307".to_string(),
        ])
    }

    /// Load authentication from agents-in-a-box config files
    pub fn load_auth_from_config() -> Result<ClaudeAuth> {
        let home_dir =
            dirs::home_dir().ok_or_else(|| anyhow!("Could not determine home directory"))?;

        // Try API key from .env file first
        let env_file = home_dir.join(".agents-in-a-box/.env");
        if env_file.exists() {
            if let Ok(contents) = std::fs::read_to_string(&env_file) {
                for line in contents.lines() {
                    if let Some(api_key) = line.strip_prefix("ANTHROPIC_API_KEY=") {
                        info!("Found API key in .env file");
                        return Ok(ClaudeAuth::from_api_key(api_key.to_string()));
                    }
                }
            }
        }

        // Try OAuth credentials
        let auth_dir = home_dir.join(".agents-in-a-box/auth");
        let credentials_file = auth_dir.join(".credentials.json");
        let claude_json_file = auth_dir.join(".claude.json");

        if credentials_file.exists() && claude_json_file.exists() {
            // Load OAuth token from .claude.json (this is where OAuth tokens are stored)
            if let Ok(contents) = std::fs::read_to_string(&claude_json_file) {
                if let Ok(oauth_data) = serde_json::from_str::<serde_json::Value>(&contents) {
                    if let Some(token) = oauth_data.get("access_token").and_then(|t| t.as_str()) {
                        info!("Found OAuth token in .claude.json");
                        return Ok(ClaudeAuth::from_oauth_token(token.to_string()));
                    }
                }
            }
        }

        // Try environment variable as fallback
        if let Ok(api_key) = std::env::var("ANTHROPIC_API_KEY") {
            info!("Found API key in environment variable");
            return Ok(ClaudeAuth::from_api_key(api_key));
        }

        Err(anyhow!(
            "No Claude authentication found. Set up API key or OAuth authentication."
        ))
    }
}

impl Default for ClaudeApiClient {
    fn default() -> Self {
        Self::new().expect("Failed to create default Claude API client")
    }
}

// Chat session management
#[derive(Debug)]
pub struct ClaudeChatManager {
    client: ClaudeApiClient,
    sessions: HashMap<Uuid, ClaudeChatSession>,
    active_session: Option<Uuid>,
}

impl ClaudeChatManager {
    pub fn new(client: ClaudeApiClient) -> Self {
        Self {
            client,
            sessions: HashMap::new(),
            active_session: None,
        }
    }

    pub fn create_session(&mut self, session_id: Option<Uuid>) -> Uuid {
        let id = session_id.unwrap_or_else(Uuid::new_v4);
        let session = ClaudeChatSession::new(id);
        self.sessions.insert(id, session);
        self.active_session = Some(id);
        info!("Created new Claude chat session: {}", id);
        id
    }

    pub fn get_active_session(&self) -> Option<&ClaudeChatSession> {
        self.active_session.and_then(|id| self.sessions.get(&id))
    }

    pub fn get_active_session_mut(&mut self) -> Option<&mut ClaudeChatSession> {
        self.active_session.and_then(|id| self.sessions.get_mut(&id))
    }

    pub fn set_active_session(&mut self, session_id: Uuid) -> Result<()> {
        if self.sessions.contains_key(&session_id) {
            self.active_session = Some(session_id);
            Ok(())
        } else {
            Err(anyhow!("Session not found: {}", session_id))
        }
    }

    /// Send a message in the active session
    pub async fn send_message(&mut self, message: &str) -> Result<String> {
        let session_id = self.active_session.ok_or_else(|| anyhow!("No active chat session"))?;

        let context = {
            let session = self
                .sessions
                .get(&session_id)
                .ok_or_else(|| anyhow!("Active session not found"))?;
            session.get_conversation_context(10) // Last 10 messages for context
        };

        let response = self.client.send_message(message, Some(&context)).await?;

        // Update session with both user message and assistant response
        if let Some(session) = self.sessions.get_mut(&session_id) {
            session.add_message(ClaudeMessage::user(message.to_string()));
            session.add_message(ClaudeMessage::assistant(response.clone()));
        }

        Ok(response)
    }

    /// Start streaming a message in the active session
    pub async fn stream_message(&mut self, message: &str) -> Result<ClaudeStreamingResponse> {
        let session_id = self.active_session.ok_or_else(|| anyhow!("No active chat session"))?;

        let context = {
            let session = self
                .sessions
                .get(&session_id)
                .ok_or_else(|| anyhow!("Active session not found"))?;
            session.get_conversation_context(10) // Last 10 messages for context
        };

        // Add user message to session
        if let Some(session) = self.sessions.get_mut(&session_id) {
            session.add_message(ClaudeMessage::user(message.to_string()));
            session.start_streaming();
        }

        self.client.stream_message(message, Some(&context)).await
    }

    /// Append streaming text to the active session
    pub fn append_streaming_text(&mut self, text: &str) {
        if let Some(session) = self.get_active_session_mut() {
            session.append_to_current_response(text);
        }
    }

    /// Finish streaming in the active session
    pub fn finish_streaming(&mut self) {
        if let Some(session) = self.get_active_session_mut() {
            session.finish_streaming();
        }
    }

    pub fn get_all_sessions(&self) -> &HashMap<Uuid, ClaudeChatSession> {
        &self.sessions
    }

    pub fn remove_session(&mut self, session_id: Uuid) -> Option<ClaudeChatSession> {
        if self.active_session == Some(session_id) {
            self.active_session = None;
        }
        self.sessions.remove(&session_id)
    }
}
