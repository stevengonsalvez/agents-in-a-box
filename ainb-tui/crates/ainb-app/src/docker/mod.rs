// ABOUTME: Docker integration for managing development containers

pub mod agents_dev;
pub mod builder;
pub mod container_manager;
pub mod log_streaming;
pub mod session_container;
pub mod session_lifecycle;
pub mod session_progress;

pub use agents_dev::{AgentsDevConfig, AgentsDevProgress, create_agents_dev_session};
pub use builder::ImageBuilder;
pub use container_manager::{ContainerError, ContainerManager};
pub use log_streaming::LogStreamingCoordinator;
pub use session_container::{ContainerConfig, ContainerStatus, SessionContainer};
pub use session_lifecycle::SessionLifecycleManager;
pub use session_progress::SessionProgress;

/// Execute a command interactively in a container, blocking until it exits.
///
/// The command owns the terminal while it runs: the host releases its input
/// modes first (see [`crate::host::TerminalHandoff`]) and reclaims them after.
pub async fn exec_interactive_blocking(
    container_id: &str,
    command: Vec<String>,
) -> Result<std::process::ExitStatus, ContainerError> {
    use std::process::{Command, Stdio};

    tracing::info!(
        "Executing blocking interactive command in container {}: {:?}",
        container_id,
        command
    );

    crate::host::release_terminal().map_err(|e| {
        ContainerError::OperationFailed(format!("Failed to release the terminal: {}", e))
    })?;

    // Execute docker command in foreground
    let mut cmd = Command::new("docker");
    cmd.arg("exec").arg("-it").arg(container_id);

    for arg in command {
        cmd.arg(arg);
    }

    cmd.stdin(Stdio::inherit()).stdout(Stdio::inherit()).stderr(Stdio::inherit());

    let result = cmd.status();

    crate::host::reclaim_terminal().map_err(|e| {
        ContainerError::OperationFailed(format!("Failed to reclaim the terminal: {}", e))
    })?;

    match result {
        Ok(status) => Ok(status),
        Err(e) => Err(ContainerError::OperationFailed(format!(
            "Failed to execute docker command: {}",
            e
        ))),
    }
}
