// ABOUTME: Terminal-side Docker helpers. The Docker service layer lives in
// `ainb-app`; this module re-exports it and adds the interactive exec, which
// has to suspend the terminal.

pub use ainb_app::docker::*;

use tracing::info;

/// Execute a command interactively with proper terminal handling (blocks until completion)
pub async fn exec_interactive_blocking(
    container_id: &str,
    command: Vec<String>,
) -> Result<std::process::ExitStatus, ContainerError> {
    use crossterm::{
        execute,
        terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
    };
    use std::io;
    use std::process::{Command, Stdio};

    info!(
        "Executing blocking interactive command in container {}: {:?}",
        container_id, command
    );

    // Exit TUI mode temporarily
    disable_raw_mode().map_err(|e| {
        ContainerError::OperationFailed(format!("Failed to disable raw mode: {}", e))
    })?;
    execute!(io::stdout(), LeaveAlternateScreen).map_err(|e| {
        ContainerError::OperationFailed(format!("Failed to leave alternate screen: {}", e))
    })?;

    // Execute docker command in foreground
    let mut cmd = Command::new("docker");
    cmd.arg("exec").arg("-it").arg(container_id);

    for arg in command {
        cmd.arg(arg);
    }

    cmd.stdin(Stdio::inherit()).stdout(Stdio::inherit()).stderr(Stdio::inherit());

    let result = cmd.status();

    // Restore TUI mode
    enable_raw_mode().map_err(|e| {
        ContainerError::OperationFailed(format!("Failed to re-enable raw mode: {}", e))
    })?;
    execute!(io::stdout(), EnterAlternateScreen).map_err(|e| {
        ContainerError::OperationFailed(format!("Failed to re-enter alternate screen: {}", e))
    })?;

    match result {
        Ok(status) => Ok(status),
        Err(e) => Err(ContainerError::OperationFailed(format!(
            "Failed to execute docker command: {}",
            e
        ))),
    }
}
