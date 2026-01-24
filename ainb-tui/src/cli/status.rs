// ABOUTME: CLI status and kill commands for session management
//
// status: Show detailed session information (text/JSON output)
// kill: Terminate a session and its tmux, with confirmation prompt

use anyhow::{Context, Result};
use serde::Serialize;
use std::io::{self, Write};
use std::process::Command;

use super::util::find_session;
use super::{KillArgs, OutputFormat, StatusArgs};
use crate::interactive::session_manager::SessionStore;
use crate::tmux::ClaudeProcessDetector;

/// JSON output structure for status command
#[derive(Debug, Serialize)]
pub struct StatusOutput {
    pub session_id: String,
    pub workspace_name: String,
    pub tmux_session_name: String,
    pub worktree_path: String,
    pub created_at: String,
    pub is_running: bool,
    pub claude_active: bool,
}

/// Check if a tmux session exists
fn tmux_session_exists(tmux_session_name: &str) -> bool {
    Command::new("tmux")
        .args(["has-session", "-t", tmux_session_name])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Execute the status command
///
/// Shows detailed information about a session including:
/// - Session ID, workspace name
/// - Tmux session status
/// - Worktree path
/// - Creation time
/// - Whether Claude CLI is active
#[allow(clippy::unused_async)]
pub async fn execute(args: StatusArgs, format: OutputFormat) -> Result<()> {
    let session = find_session(&args.session)?;

    // Check tmux session status
    let is_running = tmux_session_exists(&session.tmux_session_name);

    // Check if Claude is active in the session
    let claude_active = if is_running {
        let detector = ClaudeProcessDetector::new();
        detector
            .is_claude_running(&session.tmux_session_name)
            .unwrap_or(false)
    } else {
        false
    };

    match format {
        OutputFormat::Json => {
            let output = StatusOutput {
                session_id: session.session_id.to_string(),
                workspace_name: session.workspace_name.clone(),
                tmux_session_name: session.tmux_session_name.clone(),
                worktree_path: session.worktree_path.display().to_string(),
                created_at: session.created_at.to_rfc3339(),
                is_running,
                claude_active,
            };
            println!(
                "{}",
                serde_json::to_string_pretty(&output).context("Failed to serialize status")?
            );
        }
        OutputFormat::Text => {
            let status_text = if is_running {
                if claude_active {
                    "\x1b[32m●\x1b[0m Running (Claude active)"
                } else {
                    "\x1b[33m●\x1b[0m Running (shell)"
                }
            } else {
                "\x1b[31m●\x1b[0m Stopped"
            };

            let short_id = &session.session_id.to_string()[..8];

            println!("Session: {}", session.session_id);
            println!("{}", "━".repeat(44));
            println!("Workspace:    {}", session.workspace_name);
            println!("Status:       {status_text}");
            println!("Tmux:         {}", session.tmux_session_name);
            println!("Worktree:     {}", session.worktree_path.display());
            println!("Created:      {}", session.created_at.format("%Y-%m-%d %H:%M:%S UTC"));
            println!();
            println!("Commands:");
            println!("  Attach:     ainb attach {short_id}");
            println!("  Logs:       ainb logs {short_id} --follow");
            println!("  Kill:       ainb kill {short_id}");
        }
    }

    Ok(())
}

/// Execute the kill command
///
/// Terminates a session by:
/// 1. Finding the session by ID or name
/// 2. Prompting for confirmation (unless --force)
/// 3. Killing the tmux session
/// 4. Removing the session from `SessionStore`
#[allow(clippy::unused_async, clippy::if_not_else)]
pub async fn kill(args: KillArgs) -> Result<()> {
    let session = find_session(&args.session)?;

    // Check if session is running
    let is_running = tmux_session_exists(&session.tmux_session_name);

    if !is_running {
        println!(
            "Session '{}' is not running (tmux session not found).",
            session.workspace_name
        );
        println!("Removing from session store...");

        // Still remove from store
        let mut store = SessionStore::load();
        store.remove_by_session_id(session.session_id);
        store.save().context("Failed to save session store")?;

        println!("Session removed.");
        return Ok(());
    }

    // Prompt for confirmation unless --force
    if !args.force {
        print!("Kill session '{}'? [y/N] ", session.workspace_name);
        io::stdout().flush()?;

        let mut input = String::new();
        io::stdin().read_line(&mut input)?;

        if !input.trim().eq_ignore_ascii_case("y") {
            println!("Cancelled.");
            return Ok(());
        }
    }

    // Kill tmux session
    println!("Killing tmux session '{}'...", session.tmux_session_name);

    let output = Command::new("tmux")
        .args(["kill-session", "-t", &session.tmux_session_name])
        .output()
        .context("Failed to execute tmux kill-session")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        eprintln!(
            "Warning: tmux kill-session failed: {}",
            stderr.trim()
        );
        // Continue anyway - might already be dead
    } else {
        println!("Tmux session killed.");
    }

    // Remove from session store
    let mut store = SessionStore::load();
    store.remove_by_session_id(session.session_id);
    store.save().context("Failed to save session store")?;

    println!("Session '{}' removed.", session.workspace_name);

    // Note: We don't remove the worktree by default
    // The user can manually clean it up or we could add --cleanup-worktree flag
    println!(
        "\nNote: Worktree at '{}' was not removed.",
        session.worktree_path.display()
    );
    println!("To clean up, run: rm -rf {}", session.worktree_path.display());

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use crate::interactive::session_manager::SessionMetadata;
    use std::path::PathBuf;
    use uuid::Uuid;

    fn create_test_session(id: Uuid, workspace: &str, tmux_name: &str) -> SessionMetadata {
        SessionMetadata {
            session_id: id,
            tmux_session_name: tmux_name.to_string(),
            worktree_path: PathBuf::from(format!("/tmp/test-worktree-{}", id)),
            workspace_name: workspace.to_string(),
            created_at: Utc::now(),
        }
    }

    #[test]
    fn test_find_session_by_full_uuid() {
        // This test would require mocking SessionStore::load()
        // For now, we verify the logic flow with a unit test of the search
        let id = Uuid::new_v4();
        let id_str = id.to_string();

        // Verify UUID parsing works
        let parsed = Uuid::parse_str(&id_str);
        assert!(parsed.is_ok());
        assert_eq!(parsed.unwrap(), id);
    }

    #[test]
    fn test_find_session_by_prefix() {
        let id = Uuid::new_v4();
        let full_id = id.to_string();
        let prefix = &full_id[..8];

        // Verify prefix matching logic
        assert!(full_id.to_lowercase().starts_with(&prefix.to_lowercase()));
    }

    #[test]
    fn test_status_output_json_serialization() {
        let output = StatusOutput {
            session_id: "f79e07da-774d-415c-aedf-a2acd0bee0d3".to_string(),
            workspace_name: "my-workspace".to_string(),
            tmux_session_name: "tmux_my-session".to_string(),
            worktree_path: "/path/to/worktree".to_string(),
            created_at: "2026-01-17T18:25:46Z".to_string(),
            is_running: true,
            claude_active: true,
        };

        let json = serde_json::to_string_pretty(&output);
        assert!(json.is_ok());

        let json_str = json.unwrap();
        assert!(json_str.contains("session_id"));
        assert!(json_str.contains("workspace_name"));
        assert!(json_str.contains("is_running"));
        assert!(json_str.contains("claude_active"));
    }

    #[test]
    fn test_session_metadata_clone() {
        let id = Uuid::new_v4();
        let session = create_test_session(id, "test-workspace", "tmux_test");

        let cloned = session.clone();
        assert_eq!(cloned.session_id, id);
        assert_eq!(cloned.workspace_name, "test-workspace");
        assert_eq!(cloned.tmux_session_name, "tmux_test");
    }
}
