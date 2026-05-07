// ABOUTME: Claude process detection by parsing tmux status bar content

#![allow(dead_code)]

use anyhow::{Context, Result};
use std::process::Command;
use tracing::{debug, warn};

/// Detects if Claude CLI is running in a tmux session by analyzing the status bar
///
/// The Claude CLI shows a distinctive status bar pattern when running:
/// - "Model: Sonnet 4.5" (or other model names)
/// - "Cost: $X.XX"
/// - "Session: Xm" (session duration)
/// - "Ctx: Xk" (context size)
///
/// This detection is more reliable than process checks because:
/// 1. Claude may run as a child process of tmux or shell
/// 2. Process names may vary across environments
/// 3. The status bar is Claude's own UI identifier
#[derive(Debug, Clone)]
pub struct ClaudeProcessDetector;

impl ClaudeProcessDetector {
    /// Create a new Claude process detector
    pub fn new() -> Self {
        Self
    }

    /// Detect if Claude is running in the given tmux session
    ///
    /// # Arguments
    /// * `tmux_session_name` - The name of the tmux session to check
    ///
    /// # Returns
    /// * `Ok(true)` - Claude is running (status bar detected)
    /// * `Ok(false)` - Claude is not running (tmux exists but no Claude status bar)
    /// * `Err(_)` - Error capturing pane or tmux session doesn't exist
    pub fn is_claude_running(&self, tmux_session_name: &str) -> Result<bool> {
        debug!("Checking if Claude is running in tmux session: {}", tmux_session_name);

        // Capture the tmux pane content
        let pane_content = self.capture_pane_content(tmux_session_name)
            .context("Failed to capture tmux pane content")?;

        // Look for Claude's status bar indicators
        let is_running = self.has_claude_status_bar(&pane_content);

        debug!(
            "Claude detection for session '{}': {}",
            tmux_session_name,
            if is_running { "RUNNING" } else { "NOT RUNNING" }
        );

        Ok(is_running)
    }

    /// Capture the content of the tmux pane
    ///
    /// Uses `tmux capture-pane -p -e -J -t <session>` to get the full pane output
    fn capture_pane_content(&self, tmux_session_name: &str) -> Result<String> {
        let output = Command::new("tmux")
            .args(&[
                "capture-pane",
                "-p",  // Print to stdout
                "-e",  // Include escape sequences
                "-J",  // Join wrapped lines
                "-t",  // Target session
                tmux_session_name,
            ])
            .output()
            .context("Failed to execute tmux capture-pane")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!(
                "tmux capture-pane failed for session '{}': {}",
                tmux_session_name,
                stderr
            );
        }

        let content = String::from_utf8(output.stdout)
            .context("Failed to parse tmux pane output as UTF-8")?;

        Ok(content)
    }

    /// Check if the pane content contains Claude's status bar indicators
    ///
    /// Claude's status bar has distinctive patterns:
    /// - "Model: " followed by model name (e.g., "Sonnet 4.5", "Opus", "Haiku")
    /// - "Cost: $" followed by cost amount
    /// - "Session: " followed by duration
    /// - "Ctx: " followed by context size
    ///
    /// We check for multiple indicators to avoid false positives
    pub fn has_claude_status_bar(&self, content: &str) -> bool {
        let has_model = content.contains("Model: ");
        let has_cost = content.contains("Cost: $");
        let has_session = content.contains("Session: ");
        let has_ctx = content.contains("Ctx: ");

        // Require at least 2 indicators to confirm Claude is running
        // This reduces false positives while being resilient to UI changes
        let indicator_count = [has_model, has_cost, has_session, has_ctx]
            .iter()
            .filter(|&&x| x)
            .count();

        let is_claude_running = indicator_count >= 2;

        if is_claude_running {
            debug!("Claude status bar detected (indicators: {})", indicator_count);
        } else {
            debug!(
                "Claude status bar NOT detected (indicators: {}, need >= 2)",
                indicator_count
            );
        }

        is_claude_running
    }

    /// Check if a tmux session exists
    ///
    /// # Arguments
    /// * `tmux_session_name` - The name of the tmux session to check
    ///
    /// # Returns
    /// * `Ok(true)` - Session exists
    /// * `Ok(false)` - Session doesn't exist
    /// * `Err(_)` - Error running tmux command
    pub fn session_exists(&self, tmux_session_name: &str) -> Result<bool> {
        let output = Command::new("tmux")
            .args(&["has-session", "-t", tmux_session_name])
            .output()
            .context("Failed to execute tmux has-session")?;

        Ok(output.status.success())
    }

    /// Get detailed status information about Claude in a tmux session
    ///
    /// Returns a tuple of (tmux_exists, claude_running)
    pub fn get_session_health(&self, tmux_session_name: &str) -> Result<(bool, bool)> {
        let tmux_exists = self.session_exists(tmux_session_name)?;

        if !tmux_exists {
            return Ok((false, false));
        }

        let claude_running = match self.is_claude_running(tmux_session_name) {
            Ok(running) => running,
            Err(e) => {
                warn!(
                    "Failed to detect Claude in session '{}': {}",
                    tmux_session_name, e
                );
                // If we can't detect, assume not running
                false
            }
        };

        Ok((tmux_exists, claude_running))
    }
}

impl Default for ClaudeProcessDetector {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_claude_status_bar_detection() {
        let detector = ClaudeProcessDetector::new();

        // Test with Claude status bar present
        let claude_running = r#"
            Some output...
            Model: Sonnet 4.5  Cost: $0.45  Session: 12m  Ctx: 25k
            More output...
        "#;
        assert!(detector.has_claude_status_bar(claude_running));

        // Test with partial status bar (still should detect)
        let partial_status = r#"
            Some output...
            Model: Opus  Cost: $1.23
            More output...
        "#;
        assert!(detector.has_claude_status_bar(partial_status));

        // Test without Claude status bar
        let no_claude = r#"
            Just a regular shell
            $ ls -la
            total 64
        "#;
        assert!(!detector.has_claude_status_bar(no_claude));

        // Test with only one indicator (should not detect)
        let single_indicator = r#"
            Some output that mentions Model: Something
            But no other Claude indicators
        "#;
        assert!(!detector.has_claude_status_bar(single_indicator));
    }

    #[test]
    fn test_status_bar_variations() {
        let detector = ClaudeProcessDetector::new();

        // Different model names
        let opus = "Model: Opus  Cost: $2.50";
        assert!(detector.has_claude_status_bar(opus));

        let haiku = "Model: Haiku  Cost: $0.10";
        assert!(detector.has_claude_status_bar(haiku));

        // Different cost formats
        let high_cost = "Model: Opus  Cost: $123.45  Session: 45m";
        assert!(detector.has_claude_status_bar(high_cost));

        // Different session durations
        let long_session = "Model: Sonnet 4.5  Session: 2h15m  Ctx: 150k";
        assert!(detector.has_claude_status_bar(long_session));
    }
}
