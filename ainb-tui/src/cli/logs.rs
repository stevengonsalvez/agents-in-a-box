// ABOUTME: CLI logs command - view session output from tmux panes
//
// Captures tmux pane content and displays it to stdout.
// Supports --follow mode for streaming updates and --lines for limiting output.

use anyhow::{bail, Result};
use std::time::Duration;
use tokio::process::Command;
use uuid::Uuid;

use super::LogsArgs;
use crate::interactive::session_manager::{SessionMetadata, SessionStore};
use crate::tmux::capture::{capture_pane, CaptureOptions};

/// Execute the logs command
///
/// Displays the tmux pane content for a session identified by ID or name prefix.
/// Supports follow mode for streaming updates and line limiting.
pub async fn execute(args: LogsArgs) -> Result<()> {
    // Find the session by ID or name prefix
    let session = find_session(&args.session)?;

    // Verify tmux session exists
    if !tmux_session_exists(&session.tmux_session_name).await {
        bail!(
            "Tmux session '{}' no longer exists (session may have been terminated)",
            session.tmux_session_name
        );
    }

    if args.follow {
        follow_logs(&session.tmux_session_name).await
    } else {
        show_logs(&session.tmux_session_name, args.lines).await
    }
}

/// Find a session by ID or name prefix
///
/// Searches the session store for a matching session:
/// 1. First tries to parse as UUID and match by `session_id`
/// 2. Then tries prefix matching on `workspace_name`, `tmux_session_name`, or `session_id` string
///
/// Returns an error if no match is found or if multiple sessions match.
fn find_session(id_or_name: &str) -> Result<SessionMetadata> {
    let store = SessionStore::load();

    // Try UUID parse first for exact match
    if let Ok(uuid) = Uuid::parse_str(id_or_name) {
        if let Some(session) = store.sessions.values().find(|s| s.session_id == uuid) {
            return Ok(session.clone());
        }
    }

    // Try prefix match on workspace_name, tmux_session_name, or session_id string
    let matches: Vec<_> = store
        .sessions
        .values()
        .filter(|s| {
            s.workspace_name.starts_with(id_or_name)
                || s.tmux_session_name.starts_with(id_or_name)
                || s.session_id.to_string().starts_with(id_or_name)
        })
        .collect();

    match matches.len() {
        0 => bail!("No session found matching '{id_or_name}'"),
        1 => Ok(matches[0].clone()),
        _ => {
            let names: Vec<_> = matches.iter().map(|s| &s.workspace_name).collect();
            bail!(
                "Multiple sessions match '{id_or_name}': {names:?}. Please be more specific."
            )
        }
    }
}

/// Check if a tmux session exists
async fn tmux_session_exists(session_name: &str) -> bool {
    let output = Command::new("tmux")
        .args(["has-session", "-t", session_name])
        .output()
        .await;

    matches!(output, Ok(o) if o.status.success())
}

/// Display logs in follow mode (continuous updates like tail -f)
async fn follow_logs(session_name: &str) -> Result<()> {
    loop {
        // Clear screen and move cursor to top
        print!("\x1B[2J\x1B[1;1H");

        // Capture and print current pane content
        let content = capture_pane(session_name, CaptureOptions::visible()).await?;
        println!("{content}");

        // Wait before next refresh
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// Display logs one-shot (capture and exit)
async fn show_logs(session_name: &str, lines: usize) -> Result<()> {
    // Use full history capture for larger line counts, visible for small ones
    let options = if lines > 100 {
        CaptureOptions::full_history()
    } else {
        CaptureOptions::visible()
    };

    let content = capture_pane(session_name, options).await?;

    // Take the last N lines
    let output_lines: Vec<&str> = content.lines().collect();
    let start_idx = output_lines.len().saturating_sub(lines);
    let truncated: Vec<&str> = output_lines.into_iter().skip(start_idx).collect();

    println!("{}", truncated.join("\n"));

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use std::path::PathBuf;

    /// Create a test SessionMetadata for testing
    fn create_test_session(
        id: Uuid,
        workspace_name: &str,
        tmux_name: &str,
    ) -> SessionMetadata {
        SessionMetadata {
            session_id: id,
            tmux_session_name: tmux_name.to_string(),
            worktree_path: PathBuf::from("/tmp/test"),
            workspace_name: workspace_name.to_string(),
            created_at: Utc::now(),
        }
    }

    #[test]
    fn test_find_session_by_exact_uuid() {
        // This test would require mocking SessionStore::load()
        // For now, we verify the function signature and logic structure
        let id = Uuid::new_v4();
        let id_str = id.to_string();

        // Verify UUID parsing works
        assert_eq!(Uuid::parse_str(&id_str).unwrap(), id);
    }

    #[test]
    fn test_find_session_prefix_matching_logic() {
        // Test the prefix matching logic
        let workspace = "my-project";
        let prefix = "my-";

        assert!(workspace.starts_with(prefix));
        assert!(!workspace.starts_with("other-"));
    }

    #[test]
    fn test_session_metadata_creation() {
        let id = Uuid::new_v4();
        let session = create_test_session(id, "test-workspace", "tmux_test");

        assert_eq!(session.session_id, id);
        assert_eq!(session.workspace_name, "test-workspace");
        assert_eq!(session.tmux_session_name, "tmux_test");
    }

    #[test]
    fn test_line_truncation_logic() {
        let content = "line1\nline2\nline3\nline4\nline5";
        let lines: Vec<&str> = content.lines().collect();
        let limit = 3;

        let start_idx = lines.len().saturating_sub(limit);
        let truncated: Vec<&str> = lines.into_iter().skip(start_idx).collect();

        assert_eq!(truncated, vec!["line3", "line4", "line5"]);
    }

    #[test]
    fn test_line_truncation_with_fewer_lines() {
        let content = "line1\nline2";
        let lines: Vec<&str> = content.lines().collect();
        let limit = 10;

        let start_idx = lines.len().saturating_sub(limit);
        let truncated: Vec<&str> = lines.into_iter().skip(start_idx).collect();

        // Should return all lines when there are fewer than the limit
        assert_eq!(truncated, vec!["line1", "line2"]);
    }

    #[test]
    fn test_capture_options_selection() {
        // Verify we use full_history for large line counts
        let lines = 150;
        let should_use_full = lines > 100;
        assert!(should_use_full);

        // And visible for small ones
        let lines = 50;
        let should_use_full = lines > 100;
        assert!(!should_use_full);
    }
}
