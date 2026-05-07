// ABOUTME: PTY (Pseudo-Terminal) wrapper for tmux session interactions
//
// Abstracts PTY operations for attaching to tmux sessions, providing:
// - Async read/write operations
// - Terminal resize support
// - Clean abstraction over portable-pty

#![allow(dead_code)]

use anyhow::Result;
use portable_pty::{CommandBuilder, MasterPty, PtySize};
use std::sync::{Arc, Mutex as StdMutex};

/// Wrapper around a PTY master for tmux session interactions
pub struct PtyWrapper {
    master: Arc<StdMutex<Box<dyn MasterPty + Send>>>,
}

impl PtyWrapper {
    /// Start a new PTY with the given command
    ///
    /// # Arguments
    /// * `cmd` - The command to execute in the PTY
    ///
    /// # Returns
    /// * `Result<Self>` - A new PtyWrapper instance or an error
    pub fn start(cmd: CommandBuilder) -> Result<Self> {
        let pty_system = portable_pty::native_pty_system();

        // Create PTY with default size (will be resized as needed)
        let pair = pty_system.openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })?;

        let _child = pair.slave.spawn_command(cmd)?;

        // Note: child process continues running

        Ok(Self {
            master: Arc::new(StdMutex::new(pair.master)),
        })
    }

    /// Resize the PTY
    ///
    /// # Arguments
    /// * `cols` - Number of columns
    /// * `rows` - Number of rows
    ///
    /// # Returns
    /// * `Result<()>` - Success or an error
    pub fn resize(&self, cols: u16, rows: u16) -> Result<()> {
        let master = self.master.lock().map_err(|e| anyhow::anyhow!("Lock error: {}", e))?;
        master.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;
        Ok(())
    }

    /// Get the master PTY for direct access
    pub fn master(&self) -> Arc<StdMutex<Box<dyn MasterPty + Send>>> {
        Arc::clone(&self.master)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use portable_pty::CommandBuilder;

    #[test]
    fn test_pty_wrapper_creation() {
        let mut cmd = CommandBuilder::new("echo");
        cmd.arg("test");

        let result = PtyWrapper::start(cmd);
        assert!(result.is_ok());
    }

    #[test]
    fn test_pty_wrapper_resize() {
        let mut cmd = CommandBuilder::new("cat");
        let pty = PtyWrapper::start(cmd).unwrap();

        let result = pty.resize(100, 50);
        assert!(result.is_ok());
    }
}
