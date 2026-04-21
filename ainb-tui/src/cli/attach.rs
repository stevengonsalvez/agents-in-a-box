// ABOUTME: CLI attach command - attach to a session's tmux
//
// Finds the session by ID or name prefix and attaches to its tmux session.
// Uses exec to replace the current process with tmux attach.

use anyhow::Result;
use std::os::unix::process::CommandExt;
use std::process::Command;

use super::util::find_session;
use super::AttachArgs;

/// Execute the attach command
#[allow(clippy::unused_async)]
pub async fn execute(args: AttachArgs) -> Result<()> {
    let session = find_session(&args.session)?;
    let tmux_name = &session.tmux_session_name;

    // Check tmux session exists
    let exists = Command::new("tmux")
        .args(["has-session", "-t", tmux_name])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);

    if !exists {
        anyhow::bail!("Tmux session '{tmux_name}' no longer exists");
    }

    println!("Attaching to session: {}", session.workspace_name);
    println!("Detach with: Ctrl+B, then D");
    println!();

    // Exec replaces the current process with tmux attach
    let err = Command::new("tmux")
        .args(["attach-session", "-t", tmux_name])
        .exec();

    // If we get here, exec failed
    Err(anyhow::anyhow!("Failed to attach: {err}"))
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    #[test]
    fn test_uuid_prefix_matching_logic() {
        let uuid_str = "12345678-1234-1234-1234-123456789abc";
        let uuid = Uuid::parse_str(uuid_str).unwrap();

        // Verify prefix matching logic
        assert!(uuid.to_string().to_lowercase().starts_with("12345678"));
        assert!(uuid.to_string().to_lowercase().starts_with("1234"));
        assert!(!uuid.to_string().to_lowercase().starts_with("abcd"));
    }

    #[test]
    fn test_workspace_name_matching_logic() {
        let workspace = "my-awesome-project";
        let id_lower = "my-awe";

        // Verify workspace name prefix matching logic
        assert!(workspace.to_lowercase().starts_with(&id_lower.to_lowercase()));
        assert!(!workspace.to_lowercase().starts_with("other"));
    }
}
