// ABOUTME: Claude API streaming response handling for real-time chat interface

#![allow(dead_code)]

use crate::claude::types::ClaudeStreamingEvent;
use anyhow::{Result, anyhow};
use futures_util::StreamExt;
use reqwest::Response;
use serde_json;
use std::pin::Pin;
use tokio_stream::Stream;
use tracing::{debug, error, warn};

pub struct ClaudeStreamingResponse {
    inner: Pin<Box<dyn Stream<Item = Result<ClaudeStreamingEvent>> + Send>>,
    is_complete: bool,
    accumulated_text: String,
}

impl ClaudeStreamingResponse {
    pub async fn from_response(response: Response) -> Result<Self> {
        let stream = response.bytes_stream();

        let event_stream = stream
            .map(|chunk| match chunk {
                Ok(bytes) => Self::parse_streaming_chunk(&bytes),
                Err(e) => {
                    error!("Error reading streaming response: {}", e);
                    vec![Err(anyhow!("Stream error: {}", e))]
                }
            })
            .flat_map(futures_util::stream::iter);

        Ok(Self {
            inner: Box::pin(event_stream),
            is_complete: false,
            accumulated_text: String::new(),
        })
    }

    fn parse_streaming_chunk(bytes: &[u8]) -> Vec<Result<ClaudeStreamingEvent>> {
        let text = match std::str::from_utf8(bytes) {
            Ok(t) => t,
            Err(e) => {
                error!("Invalid UTF-8 in streaming response: {}", e);
                return vec![Err(anyhow!("Invalid UTF-8: {}", e))];
            }
        };

        let mut events = Vec::new();

        // Claude API uses Server-Sent Events format
        for line in text.lines() {
            if line.starts_with("data: ") {
                let data = &line[6..]; // Remove "data: " prefix

                // Skip empty data or [DONE] marker
                if data.is_empty() || data == "[DONE]" {
                    continue;
                }

                match serde_json::from_str::<ClaudeStreamingEvent>(data) {
                    Ok(event) => {
                        debug!("Parsed streaming event: {:?}", event);
                        events.push(Ok(event));
                    }
                    Err(e) => {
                        warn!("Failed to parse streaming event: {} - Data: {}", e, data);
                        // Don't treat parse errors as fatal - continue processing
                    }
                }
            }
        }

        events
    }

    /// Get the next streaming event
    pub async fn next(&mut self) -> Option<Result<ClaudeStreamingEvent>> {
        if self.is_complete {
            return None;
        }

        match self.inner.next().await {
            Some(Ok(event)) => {
                // Track completion
                if matches!(event, ClaudeStreamingEvent::MessageStop) {
                    self.is_complete = true;
                }

                // Accumulate text for easy access
                if let ClaudeStreamingEvent::ContentBlockDelta { delta, .. } = &event {
                    self.accumulated_text.push_str(&delta.text);
                }

                Some(Ok(event))
            }
            Some(Err(e)) => {
                self.is_complete = true;
                Some(Err(e))
            }
            None => {
                self.is_complete = true;
                None
            }
        }
    }

    /// Check if the stream is complete
    pub fn is_complete(&self) -> bool {
        self.is_complete
    }

    /// Get all accumulated text so far
    pub fn accumulated_text(&self) -> &str {
        &self.accumulated_text
    }

    /// Collect all remaining events into text
    pub async fn collect_all_text(&mut self) -> Result<String> {
        let mut full_text = self.accumulated_text.clone();

        while let Some(event_result) = self.next().await {
            match event_result {
                Ok(ClaudeStreamingEvent::ContentBlockDelta { delta, .. }) => {
                    full_text.push_str(&delta.text);
                }
                Ok(ClaudeStreamingEvent::Error { error }) => {
                    return Err(anyhow!("Claude API error: {}", error.message));
                }
                Ok(_) => {
                    // Other events don't contribute to text
                }
                Err(e) => {
                    return Err(e);
                }
            }
        }

        Ok(full_text)
    }
}

// Helper type for managing streaming state in UI components
#[derive(Debug, Clone)]
pub enum StreamingState {
    Idle,
    Connecting,
    Streaming { partial_response: String },
    Complete { full_response: String },
    Error { message: String },
}

impl StreamingState {
    pub fn is_active(&self) -> bool {
        matches!(
            self,
            StreamingState::Connecting | StreamingState::Streaming { .. }
        )
    }

    pub fn get_current_text(&self) -> Option<&str> {
        match self {
            StreamingState::Streaming { partial_response } => Some(partial_response),
            StreamingState::Complete { full_response } => Some(full_response),
            _ => None,
        }
    }

    pub fn is_complete(&self) -> bool {
        matches!(self, StreamingState::Complete { .. })
    }

    pub fn is_error(&self) -> bool {
        matches!(self, StreamingState::Error { .. })
    }
}

impl Default for StreamingState {
    fn default() -> Self {
        StreamingState::Idle
    }
}

// Utility for handling streaming events in UI components
pub struct StreamingEventHandler {
    current_response: String,
    state: StreamingState,
}

impl StreamingEventHandler {
    pub fn new() -> Self {
        Self {
            current_response: String::new(),
            state: StreamingState::Idle,
        }
    }

    pub fn start_streaming(&mut self) {
        self.current_response.clear();
        self.state = StreamingState::Connecting;
    }

    pub fn handle_event(&mut self, event: ClaudeStreamingEvent) -> bool {
        match event {
            ClaudeStreamingEvent::MessageStart { .. } => {
                self.state = StreamingState::Streaming {
                    partial_response: self.current_response.clone(),
                };
            }
            ClaudeStreamingEvent::ContentBlockDelta { delta, .. } => {
                self.current_response.push_str(&delta.text);
                self.state = StreamingState::Streaming {
                    partial_response: self.current_response.clone(),
                };
            }
            ClaudeStreamingEvent::MessageStop => {
                self.state = StreamingState::Complete {
                    full_response: self.current_response.clone(),
                };
                return true; // Streaming complete
            }
            ClaudeStreamingEvent::Error { error } => {
                self.state = StreamingState::Error {
                    message: error.message,
                };
                return true; // Streaming complete (with error)
            }
            _ => {
                // Other events don't require action
            }
        }
        false // Streaming continues
    }

    pub fn get_state(&self) -> &StreamingState {
        &self.state
    }

    pub fn reset(&mut self) {
        self.current_response.clear();
        self.state = StreamingState::Idle;
    }
}

impl Default for StreamingEventHandler {
    fn default() -> Self {
        Self::new()
    }
}
