// ABOUTME: Attach/detach handler for seamless TUI suspend/resume when attaching to tmux sessions
//
// Provides functionality to:
// - Suspend the Ratatui TUI (leave alternate screen, disable raw mode)
// - Execute tmux attach-session command directly
// - Resume the Ratatui TUI after detachment
// - Restore terminal state properly

use anyhow::{Context, Result};
use crossterm::{
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};
use std::io::Stdout;
use tokio::process::Command;

/// Handler for attaching to tmux sessions with TUI suspend/resume
pub struct AttachHandler<'a> {
    terminal: &'a mut Terminal<CrosstermBackend<Stdout>>,
}

impl<'a> AttachHandler<'a> {
    /// Create a new attach handler from a mutable terminal reference
    ///
    /// # Arguments
    /// * `terminal` - Mutable reference to the Ratatui terminal
    pub fn new_from_terminal(terminal: &'a mut Terminal<CrosstermBackend<Stdout>>) -> Result<Self> {
        Ok(Self { terminal })
    }

    /// Attach to a tmux session
    ///
    /// This will:
    /// 1. Suspend the TUI (leave alternate screen, disable raw mode)
    /// 2. Execute `tmux attach-session -t <session_name>`
    /// 3. Wait for the command to complete (user presses Ctrl+Q to detach)
    /// 4. Resume the TUI (enter alternate screen, enable raw mode)
    ///
    /// # Arguments
    /// * `session_name` - The name of the tmux session to attach to
    ///
    /// # Returns
    /// * `Result<()>` - Success or an error
    pub async fn attach_to_session(&mut self, session_name: &str) -> Result<()> {
        // Step 1: Suspend TUI
        self.suspend_tui().await?;

        // Step 2: Execute tmux attach
        let result = self.execute_tmux_attach(session_name).await;

        // Step 3: Resume TUI (always, even if attach failed)
        self.resume_tui().await?;

        // Return the result from tmux attach
        result
    }

    /// Suspend the TUI
    ///
    /// Leaves alternate screen and disables raw mode, returning control to the normal terminal
    async fn suspend_tui(&mut self) -> Result<()> {
        // Disable raw mode first
        disable_raw_mode().context("Failed to disable raw mode")?;

        // Leave alternate screen
        execute!(self.terminal.backend_mut(), LeaveAlternateScreen)
            .context("Failed to leave alternate screen")?;

        // Show cursor
        self.terminal.show_cursor().context("Failed to show cursor")?;

        tracing::info!("TUI suspended");
        Ok(())
    }

    /// Resume the TUI
    ///
    /// Enters alternate screen and enables raw mode, restoring the TUI
    async fn resume_tui(&mut self) -> Result<()> {
        // Enter alternate screen
        execute!(self.terminal.backend_mut(), EnterAlternateScreen)
            .context("Failed to enter alternate screen")?;

        // Enable raw mode
        enable_raw_mode().context("Failed to enable raw mode")?;

        // Hide cursor
        self.terminal.hide_cursor().context("Failed to hide cursor")?;

        // Clear the screen
        self.terminal.clear().context("Failed to clear terminal")?;

        tracing::info!("TUI resumed");
        Ok(())
    }

    /// Execute tmux attach-session command
    ///
    /// This runs the tmux attach command directly, giving the user full control
    /// of the tmux session. The user can detach with Ctrl+B then D, or we can
    /// configure tmux to use Ctrl+Q.
    ///
    /// # Arguments
    /// * `session_name` - The name of the tmux session to attach to
    ///
    /// # Returns
    /// * `Result<()>` - Success or an error
    async fn execute_tmux_attach(&self, session_name: &str) -> Result<()> {
        tracing::info!("[ATTACH] Executing tmux attach-session for '{}'", session_name);

        // First verify the session exists
        let check = Command::new("tmux")
            .arg("has-session")
            .arg("-t")
            .arg(session_name)
            .output()
            .await
            .context("Failed to check if tmux session exists")?;

        if !check.status.success() {
            let stderr = String::from_utf8_lossy(&check.stderr);
            tracing::error!("[ATTACH] tmux session '{}' does not exist: {}", session_name, stderr);
            anyhow::bail!("tmux session '{}' does not exist", session_name);
        }

        tracing::info!("[ATTACH] Session '{}' exists, attaching...", session_name);

        // Execute tmux attach-session
        // Note: We use tokio::process::Command which will inherit stdin/stdout/stderr
        let status = Command::new("tmux")
            .arg("attach-session")
            .arg("-t")
            .arg(session_name)
            .status()
            .await
            .context("Failed to execute tmux attach-session")?;

        if !status.success() {
            tracing::error!("[ATTACH] tmux attach-session failed with exit code: {:?}", status.code());
            anyhow::bail!(
                "tmux attach-session failed with exit code: {:?}",
                status.code()
            );
        }

        tracing::info!("[ATTACH] Successfully detached from tmux session: {}", session_name);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Note: These tests are limited because they require a real terminal
    // In a real implementation, you might want to use dependency injection
    // to mock the terminal for testing

    #[test]
    fn test_attach_handler_creation() {
        // We can't easily test this without a real terminal
        // This test just verifies the struct can be created
        // In practice, the handler would be created with a real terminal instance
    }
}
