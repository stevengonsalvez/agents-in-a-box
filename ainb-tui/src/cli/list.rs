// ABOUTME: CLI list command - list all sessions with status
//
// Lists sessions from ~/.agents-in-a-box/sessions.json
// Shows: session ID, workspace, status (Claude running, idle, stopped), tmux session name

use super::{ListArgs, OutputFormat};
use crate::interactive::session_manager::{SessionMetadata, SessionStore};
use crate::tmux::ClaudeProcessDetector;
use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::Serialize;

/// Session status as displayed in the list
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionStatus {
    /// Claude is running (tmux exists and Claude status bar detected)
    ClaudeRunning,
    /// Tmux session exists but Claude is not running
    Idle,
    /// Tmux session does not exist
    Stopped,
}

impl SessionStatus {
    /// Get the display icon for this status
    #[must_use]
    pub const fn icon(&self) -> &'static str {
        match self {
            Self::ClaudeRunning => "\u{25cf} Claude", // filled circle
            Self::Idle => "\u{25cb} Idle",            // empty circle
            Self::Stopped => "\u{23f8} Stopped",      // pause icon
        }
    }
}

/// A session with enriched status information for display
#[derive(Debug, Clone, Serialize)]
pub struct SessionInfo {
    pub session_id: String,
    pub tmux_session_name: String,
    pub workspace_name: String,
    pub worktree_path: String,
    pub created_at: DateTime<Utc>,
    pub is_running: bool,
    pub claude_active: bool,
}

impl SessionInfo {
    /// Create `SessionInfo` from metadata and status
    #[must_use]
    pub fn from_metadata(metadata: &SessionMetadata, is_running: bool, claude_active: bool) -> Self {
        Self {
            session_id: metadata.session_id.to_string(),
            tmux_session_name: metadata.tmux_session_name.clone(),
            workspace_name: metadata.workspace_name.clone(),
            worktree_path: metadata.worktree_path.display().to_string(),
            created_at: metadata.created_at,
            is_running,
            claude_active,
        }
    }

    /// Get the status of this session
    #[must_use]
    pub const fn status(&self) -> SessionStatus {
        if !self.is_running {
            SessionStatus::Stopped
        } else if self.claude_active {
            SessionStatus::ClaudeRunning
        } else {
            SessionStatus::Idle
        }
    }
}

/// Execute the list command
pub async fn execute(args: ListArgs, format: OutputFormat) -> Result<()> {
    let sessions = list_sessions(&args).await?;

    match format {
        OutputFormat::Json => output_json(&sessions)?,
        OutputFormat::Text => output_text(&sessions),
    }

    Ok(())
}

/// List sessions with filtering applied
#[allow(clippy::unused_async)] // Async for consistency with other CLI commands
pub async fn list_sessions(args: &ListArgs) -> Result<Vec<SessionInfo>> {
    let store = SessionStore::load();
    let detector = ClaudeProcessDetector::new();

    let mut sessions = Vec::new();

    for metadata in store.sessions.values() {
        // Check tmux session existence and Claude status
        let (is_running, claude_active) = detector
            .get_session_health(&metadata.tmux_session_name)
            .unwrap_or((false, false));

        let info = SessionInfo::from_metadata(metadata, is_running, claude_active);

        // Apply filters
        if args.running && !is_running {
            continue;
        }

        if let Some(ref workspace_filter) = args.workspace {
            if !info.workspace_name.contains(workspace_filter) {
                continue;
            }
        }

        sessions.push(info);
    }

    // Sort by created_at descending (newest first)
    sessions.sort_by(|a, b| b.created_at.cmp(&a.created_at));

    Ok(sessions)
}

/// Output sessions as JSON
fn output_json(sessions: &[SessionInfo]) -> Result<()> {
    let json = serde_json::to_string_pretty(sessions)?;
    println!("{json}");
    Ok(())
}

/// Output sessions as a text table
fn output_text(sessions: &[SessionInfo]) {
    if sessions.is_empty() {
        println!("No sessions found.");
        return;
    }

    // Print header
    println!("{:<36} {:<25} {:<10} TMUX SESSION", "ID", "WORKSPACE", "STATUS");
    let separator = "-".repeat(100);
    println!("{separator}");

    // Print each session
    for session in sessions {
        let status = session.status();
        let workspace = truncate(&session.workspace_name, 25);
        println!(
            "{:<36} {:<25} {:<10} {}",
            session.session_id, workspace, status.icon(), session.tmux_session_name
        );
    }
}

/// Truncate a string to fit in the given width (character-aware for UTF-8)
fn truncate(s: &str, max_len: usize) -> String {
    if max_len <= 3 {
        return ".".repeat(max_len);
    }
    let char_count = s.chars().count();
    if char_count <= max_len {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_len.saturating_sub(3)).collect();
        format!("{truncated}...")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use uuid::Uuid;

    #[test]
    fn test_session_status_icons() {
        assert_eq!(SessionStatus::ClaudeRunning.icon(), "\u{25cf} Claude");
        assert_eq!(SessionStatus::Idle.icon(), "\u{25cb} Idle");
        assert_eq!(SessionStatus::Stopped.icon(), "\u{23f8} Stopped");
    }

    #[test]
    fn test_session_info_status_claude_running() {
        let metadata = SessionMetadata {
            session_id: Uuid::new_v4(),
            tmux_session_name: "test_session".to_string(),
            worktree_path: PathBuf::from("/tmp/test"),
            workspace_name: "test-workspace".to_string(),
            created_at: Utc::now(),
        };

        let info = SessionInfo::from_metadata(&metadata, true, true);
        assert_eq!(info.status(), SessionStatus::ClaudeRunning);
    }

    #[test]
    fn test_session_info_status_idle() {
        let metadata = SessionMetadata {
            session_id: Uuid::new_v4(),
            tmux_session_name: "test_session".to_string(),
            worktree_path: PathBuf::from("/tmp/test"),
            workspace_name: "test-workspace".to_string(),
            created_at: Utc::now(),
        };

        let info = SessionInfo::from_metadata(&metadata, true, false);
        assert_eq!(info.status(), SessionStatus::Idle);
    }

    #[test]
    fn test_session_info_status_stopped() {
        let metadata = SessionMetadata {
            session_id: Uuid::new_v4(),
            tmux_session_name: "test_session".to_string(),
            worktree_path: PathBuf::from("/tmp/test"),
            workspace_name: "test-workspace".to_string(),
            created_at: Utc::now(),
        };

        let info = SessionInfo::from_metadata(&metadata, false, false);
        assert_eq!(info.status(), SessionStatus::Stopped);
    }

    #[test]
    fn test_truncate_short_string() {
        assert_eq!(truncate("hello", 10), "hello");
    }

    #[test]
    fn test_truncate_long_string() {
        assert_eq!(truncate("hello world", 8), "hello...");
    }

    #[test]
    fn test_truncate_exact_length() {
        assert_eq!(truncate("hello", 5), "hello");
    }

    #[test]
    fn test_session_info_serialization() {
        let metadata = SessionMetadata {
            session_id: Uuid::parse_str("f79e07da-774d-415c-aedf-a2acd0bee0d3").unwrap(),
            tmux_session_name: "tmux_my-session".to_string(),
            worktree_path: PathBuf::from("/tmp/test"),
            workspace_name: "my-workspace".to_string(),
            created_at: Utc::now(),
        };

        let info = SessionInfo::from_metadata(&metadata, true, true);
        let json = serde_json::to_value(&info).unwrap();

        assert_eq!(json["session_id"], "f79e07da-774d-415c-aedf-a2acd0bee0d3");
        assert_eq!(json["tmux_session_name"], "tmux_my-session");
        assert_eq!(json["workspace_name"], "my-workspace");
        assert_eq!(json["is_running"], true);
        assert_eq!(json["claude_active"], true);
    }
}
