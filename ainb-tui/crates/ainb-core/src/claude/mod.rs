// ABOUTME: Claude API integration module for direct Claude chat functionality
// Provides streaming chat interface without container dependency

pub mod client;
pub mod streaming;
pub mod types;

pub use client::ClaudeApiClient;
pub use types::ClaudeMessage;
