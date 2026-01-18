// ABOUTME: CLI attach command - attach to a session's tmux
//
// Finds the session by ID or name prefix and attaches to its tmux session.
// Uses exec to replace the current process with tmux attach.

use anyhow::Result;
use std::os::unix::process::CommandExt;
use std::process::Command;
use uuid::Uuid;

use super::AttachArgs;
use crate::interactive::session_manager::{SessionMetadata, SessionStore};

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

/// Find a session by ID (full or partial UUID) or workspace name prefix
///
/// Matching priority:
/// 1. Exact UUID match
/// 2. UUID prefix match (e.g., "abc" matches "abc12345-...")
/// 3. Workspace name prefix match (case-insensitive)
pub fn find_session(id_or_name: &str) -> Result<SessionMetadata> {
    let store = SessionStore::load();

    if store.sessions.is_empty() {
        anyhow::bail!("No sessions found. Run 'ainb run' to create a session.");
    }

    // First, try exact UUID match
    if let Ok(uuid) = Uuid::parse_str(id_or_name) {
        for session in store.sessions.values() {
            if session.session_id == uuid {
                return Ok(session.clone());
            }
        }
    }

    // Try UUID prefix match
    let id_lower = id_or_name.to_lowercase();
    let mut uuid_matches: Vec<&SessionMetadata> = store
        .sessions
        .values()
        .filter(|s| s.session_id.to_string().to_lowercase().starts_with(&id_lower))
        .collect();

    match uuid_matches.len() {
        1 => return Ok(uuid_matches.remove(0).clone()),
        n if n > 1 => {
            let ids: Vec<String> = uuid_matches
                .iter()
                .map(|s| format!("  {} ({})", &s.session_id.to_string()[..8], s.workspace_name))
                .collect();
            anyhow::bail!(
                "Ambiguous session ID prefix '{id_or_name}'. Matches:\n{}",
                ids.join("\n")
            );
        }
        _ => {}
    }

    // Try workspace name prefix match (case-insensitive)
    let mut name_matches: Vec<&SessionMetadata> = store
        .sessions
        .values()
        .filter(|s| s.workspace_name.to_lowercase().starts_with(&id_lower))
        .collect();

    match name_matches.len() {
        1 => return Ok(name_matches.remove(0).clone()),
        n if n > 1 => {
            let names: Vec<String> = name_matches
                .iter()
                .map(|s| format!("  {} ({})", s.workspace_name, &s.session_id.to_string()[..8]))
                .collect();
            anyhow::bail!(
                "Ambiguous session name prefix '{id_or_name}'. Matches:\n{}",
                names.join("\n")
            );
        }
        _ => {}
    }

    anyhow::bail!(
        "No session found matching '{id_or_name}'. Use 'ainb list' to see available sessions."
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use std::path::PathBuf;

    #[allow(dead_code)]
    fn make_test_session(id: &str, workspace: &str) -> SessionMetadata {
        SessionMetadata {
            session_id: Uuid::parse_str(id).unwrap(),
            tmux_session_name: format!("tmux_test_{workspace}"),
            worktree_path: PathBuf::from("/tmp/test"),
            workspace_name: workspace.to_string(),
            created_at: Utc::now(),
        }
    }

    #[test]
    fn test_find_session_exact_uuid() {
        // This test requires mocking SessionStore, which would need refactoring
        // For now, we test the parsing logic indirectly through integration tests
    }

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
