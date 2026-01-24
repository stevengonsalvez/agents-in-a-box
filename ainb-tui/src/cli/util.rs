// ABOUTME: Shared CLI utilities for session lookup and common operations
//
// Provides consistent session finding logic across all CLI commands.
// Uses prefix matching for both UUID and workspace name for user convenience.

use anyhow::{anyhow, Result};
use uuid::Uuid;

use crate::interactive::session_manager::{SessionMetadata, SessionStore};

/// Find a session by ID (full or partial UUID) or workspace name prefix
///
/// Matching priority:
/// 1. Exact UUID match
/// 2. UUID prefix match (e.g., "abc" matches "abc12345-...")
/// 3. Workspace name prefix match (case-insensitive)
///
/// Returns an error if no match is found or if multiple sessions match.
pub fn find_session(id_or_name: &str) -> Result<SessionMetadata> {
    let store = SessionStore::load();
    find_session_in_store(id_or_name, &store)
}

/// Find a session within a given store (testable version)
///
/// This function accepts a store reference for easier unit testing.
pub fn find_session_in_store(id_or_name: &str, store: &SessionStore) -> Result<SessionMetadata> {
    if store.sessions.is_empty() {
        return Err(anyhow!("No sessions found. Run 'ainb run' to create a session."));
    }

    // First, try exact UUID match
    if let Ok(uuid) = Uuid::parse_str(id_or_name) {
        for session in store.sessions.values() {
            if session.session_id == uuid {
                return Ok(session.clone());
            }
        }
    }

    // Try UUID prefix match (case-insensitive)
    let id_lower = id_or_name.to_lowercase();
    let uuid_matches: Vec<&SessionMetadata> = store
        .sessions
        .values()
        .filter(|s| s.session_id.to_string().to_lowercase().starts_with(&id_lower))
        .collect();

    match uuid_matches.len() {
        1 => return Ok(uuid_matches[0].clone()),
        n if n > 1 => {
            let ids: Vec<String> = uuid_matches
                .iter()
                .map(|s| format!("  {} ({})", &s.session_id.to_string()[..8], s.workspace_name))
                .collect();
            return Err(anyhow!(
                "Ambiguous session ID prefix '{id_or_name}'. Matches:\n{}",
                ids.join("\n")
            ));
        }
        _ => {}
    }

    // Try workspace name prefix match (case-insensitive)
    let name_matches: Vec<&SessionMetadata> = store
        .sessions
        .values()
        .filter(|s| s.workspace_name.to_lowercase().starts_with(&id_lower))
        .collect();

    match name_matches.len() {
        1 => return Ok(name_matches[0].clone()),
        n if n > 1 => {
            let names: Vec<String> = name_matches
                .iter()
                .map(|s| format!("  {} ({})", s.workspace_name, &s.session_id.to_string()[..8]))
                .collect();
            return Err(anyhow!(
                "Ambiguous session name prefix '{id_or_name}'. Matches:\n{}",
                names.join("\n")
            ));
        }
        _ => {}
    }

    // No match found - provide helpful error message
    let available: Vec<String> = store
        .sessions
        .values()
        .map(|s| format!("  {} ({})", &s.session_id.to_string()[..8], s.workspace_name))
        .collect();

    if available.is_empty() {
        Err(anyhow!("No sessions found. Run 'ainb run' to create a session."))
    } else {
        Err(anyhow!(
            "No session found matching '{id_or_name}'. Available sessions:\n{}",
            available.join("\n")
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use std::path::PathBuf;

    fn create_test_store() -> SessionStore {
        let mut store = SessionStore::default();

        let session1 = SessionMetadata {
            session_id: Uuid::parse_str("12345678-1234-1234-1234-123456789abc").unwrap(),
            tmux_session_name: "tmux_project-a".to_string(),
            worktree_path: PathBuf::from("/tmp/project-a"),
            workspace_name: "project-alpha".to_string(),
            created_at: Utc::now(),
        };

        let session2 = SessionMetadata {
            session_id: Uuid::parse_str("abcdef12-abcd-abcd-abcd-abcdef123456").unwrap(),
            tmux_session_name: "tmux_project-b".to_string(),
            worktree_path: PathBuf::from("/tmp/project-b"),
            workspace_name: "project-beta".to_string(),
            created_at: Utc::now(),
        };

        store.sessions.insert(session1.tmux_session_name.clone(), session1);
        store.sessions.insert(session2.tmux_session_name.clone(), session2);

        store
    }

    #[test]
    fn test_find_by_exact_uuid() {
        let store = create_test_store();
        let result = find_session_in_store("12345678-1234-1234-1234-123456789abc", &store);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().workspace_name, "project-alpha");
    }

    #[test]
    fn test_find_by_uuid_prefix() {
        let store = create_test_store();
        let result = find_session_in_store("12345678", &store);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().workspace_name, "project-alpha");
    }

    #[test]
    fn test_find_by_workspace_prefix() {
        let store = create_test_store();
        let result = find_session_in_store("project-a", &store);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().workspace_name, "project-alpha");
    }

    #[test]
    fn test_find_by_workspace_prefix_case_insensitive() {
        let store = create_test_store();
        let result = find_session_in_store("PROJECT-B", &store);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().workspace_name, "project-beta");
    }

    #[test]
    fn test_ambiguous_prefix() {
        let store = create_test_store();
        let result = find_session_in_store("project-", &store);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Ambiguous"));
    }

    #[test]
    fn test_not_found() {
        let store = create_test_store();
        let result = find_session_in_store("nonexistent", &store);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("No session found"));
    }

    #[test]
    fn test_empty_store() {
        let store = SessionStore::default();
        let result = find_session_in_store("anything", &store);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("No sessions found"));
    }
}
