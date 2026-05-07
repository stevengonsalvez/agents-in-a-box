// ABOUTME: Tmux session management for Claude Code interactions
//
// Manages the lifecycle of tmux sessions including:
// - Session creation and initialization
// - Attach/detach operations with Ctrl+Q support
// - Content capture for live preview
// - Clean session cleanup

#![allow(dead_code)]

use crate::tmux::capture::{capture_pane, CaptureOptions};
use crate::tmux::pty_wrapper::PtyWrapper;
use anyhow::{Context, Result};
use std::path::Path;
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio::time::{sleep, Duration};

/// Attach state for a tmux session
#[derive(Debug, Clone)]
pub enum AttachState {
    /// Session is detached
    Detached,
    /// Session is attached with a channel to signal detachment
    Attached { cancel_tx: mpsc::Sender<()> },
}

/// Main struct for managing a tmux session
pub struct TmuxSession {
    /// Sanitized session name (used as tmux session name)
    sanitized_name: String,
    /// Program to run in the session (e.g., "claude", "aider")
    program: String,
    /// Current PTY connection (if attached)
    pty: Option<PtyWrapper>,
    /// Current attach state
    attach_state: AttachState,
}

impl std::fmt::Debug for TmuxSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TmuxSession")
            .field("sanitized_name", &self.sanitized_name)
            .field("program", &self.program)
            .field("pty", &self.pty.is_some())
            .field("attach_state", &self.attach_state)
            .finish()
    }
}

impl TmuxSession {
    /// Create a new tmux session manager
    ///
    /// # Arguments
    /// * `name` - The base name for the session (will be sanitized)
    /// * `program` - The program to run in the session
    ///
    /// # Returns
    /// * A new `TmuxSession` instance
    pub fn new(name: String, program: String) -> Self {
        let sanitized_name = Self::sanitize_name(&name);

        Self {
            sanitized_name,
            program,
            pty: None,
            attach_state: AttachState::Detached,
        }
    }

    /// Sanitize a session name for use with tmux
    ///
    /// # Arguments
    /// * `name` - The name to sanitize
    ///
    /// # Returns
    /// * A sanitized name with "tmux_" prefix and invalid characters replaced
    fn sanitize_name(name: &str) -> String {
        let base_name = name
            .strip_prefix("tmux_")
            .unwrap_or(name);

        let cleaned = base_name
            .replace(' ', "_")
            .replace('.', "_")
            .replace('/', "_")
            .replace(':', "_");
        format!("tmux_{}", cleaned)
    }

    /// Start the tmux session
    ///
    /// # Arguments
    /// * `work_dir` - The working directory for the session
    ///
    /// # Returns
    /// * `Result<()>` - Success or an error
    pub async fn start(&mut self, work_dir: &Path) -> Result<()> {
        // Check if session already exists
        if self.does_session_exist().await {
            tracing::warn!(
                "Tmux session '{}' already exists, killing it first",
                self.sanitized_name
            );
            self.cleanup().await?;
        }

        // Create new detached tmux session
        let status = Command::new("tmux")
            .args([
                "new-session",
                "-d", // Detached
                "-s",
                &self.sanitized_name,
                "-c",
                work_dir.to_str().context("Invalid work directory path")?,
                "-x",
                "80", // Width
                "-y",
                "24", // Height
                &self.program,
            ])
            .status()
            .await
            .context("Failed to start tmux session")?;

        if !status.success() {
            anyhow::bail!("Failed to create tmux session '{}'", self.sanitized_name);
        }

        // Configure tmux session settings
        self.configure_session().await?;

        // Wait a moment for the session to initialize
        sleep(Duration::from_millis(100)).await;

        tracing::info!("Started tmux session: {}", self.sanitized_name);
        Ok(())
    }

    /// Configure tmux session settings (history, mouse mode, clipboard, etc.)
    async fn configure_session(&self) -> Result<()> {
        // Set history limit
        Command::new("tmux")
            .args([
                "set-option",
                "-t",
                &self.sanitized_name,
                "history-limit",
                "10000",
            ])
            .status()
            .await?;

        // Enable mouse scrolling
        Command::new("tmux")
            .args(["set-option", "-t", &self.sanitized_name, "mouse", "on"])
            .status()
            .await?;

        // Configure clipboard integration
        crate::tmux::configure_clipboard(&self.sanitized_name).await?;

        // macOS: Configure reattach-to-user-namespace for audio/clipboard access
        // Uses centralized function with shell validation and proper error handling
        if let Err(e) = crate::tmux::configure_macos_user_namespace(&self.sanitized_name).await {
            tracing::warn!(
                "Failed to configure macOS user namespace for session {}: {}",
                self.sanitized_name,
                e
            );
            // Continue anyway - this is optional functionality
        }

        Ok(())
    }

    /// Attach to the tmux session
    ///
    /// Returns a receiver that will be notified when detachment occurs (Ctrl+Q)
    ///
    /// # Returns
    /// * `Result<mpsc::Receiver<()>>` - A receiver for detach signal or an error
    pub async fn attach(&mut self) -> Result<mpsc::Receiver<()>> {
        // Note: For attach functionality, we'll rely on the attach_handler which will
        // suspend the TUI and exec tmux attach directly. This is simpler and more reliable
        // than trying to proxy through a PTY.

        // Create channel for detach signaling
        let (cancel_tx, cancel_rx) = mpsc::channel(1);
        self.attach_state = AttachState::Attached {
            cancel_tx: cancel_tx.clone(),
        };

        Ok(cancel_rx)
    }

    /// Detach from the tmux session
    ///
    /// # Returns
    /// * `Result<()>` - Success or an error
    pub async fn detach(&mut self) -> Result<()> {
        // Close PTY
        self.pty = None;
        self.attach_state = AttachState::Detached;

        tracing::info!("Detached from tmux session: {}", self.sanitized_name);
        Ok(())
    }

    /// Capture visible pane content
    ///
    /// # Returns
    /// * `Result<String>` - The captured content or an error
    pub async fn capture_pane_content(&self) -> Result<String> {
        capture_pane(&self.sanitized_name, CaptureOptions::visible()).await
    }

    /// Capture full scrollback history
    ///
    /// # Returns
    /// * `Result<String>` - The captured content or an error
    pub async fn capture_full_history(&self) -> Result<String> {
        capture_pane(&self.sanitized_name, CaptureOptions::full_history()).await
    }

    /// Check if the tmux session exists
    ///
    /// # Returns
    /// * `bool` - True if the session exists, false otherwise
    pub async fn does_session_exist(&self) -> bool {
        let output = Command::new("tmux")
            .args(["has-session", "-t", &self.sanitized_name])
            .output()
            .await;

        matches!(output, Ok(output) if output.status.success())
    }

    /// Clean up the tmux session
    ///
    /// # Returns
    /// * `Result<()>` - Success or an error
    pub async fn cleanup(&mut self) -> Result<()> {
        // Detach first if attached
        if matches!(self.attach_state, AttachState::Attached { .. }) {
            self.detach().await?;
        }

        // Kill the tmux session
        let output = Command::new("tmux")
            .args(["kill-session", "-t", &self.sanitized_name])
            .output()
            .await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            tracing::warn!("Failed to kill tmux session: {}", stderr);
        } else {
            tracing::info!("Cleaned up tmux session: {}", self.sanitized_name);
        }

        Ok(())
    }

    /// Get the sanitized session name
    pub fn name(&self) -> &str {
        &self.sanitized_name
    }
}

impl Drop for TmuxSession {
    fn drop(&mut self) {
        // Note: We can't use async in Drop, so we just set the state
        // The actual cleanup should be done explicitly via cleanup()
        self.pty = None;
        self.attach_state = AttachState::Detached;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sanitize_name() {
        assert_eq!(
            TmuxSession::sanitize_name("my session"),
            "tmux_my_session"
        );
        assert_eq!(
            TmuxSession::sanitize_name("test.name/with:chars"),
            "tmux_test_name_with_chars"
        );
        assert_eq!(
            TmuxSession::sanitize_name("tmux_already_sanitized"),
            "tmux_already_sanitized"
        );
    }

    #[test]
    fn test_new_session() {
        let session = TmuxSession::new("test".to_string(), "bash".to_string());
        assert_eq!(session.name(), "tmux_test");
        assert!(matches!(session.attach_state, AttachState::Detached));
    }
}
