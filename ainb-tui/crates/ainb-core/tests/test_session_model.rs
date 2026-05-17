// ABOUTME: Unit tests for Session and related models to ensure data integrity and behavior

use ainb::models::{GitChanges, Session, SessionStatus};
// Tests don't need chrono::Utc directly

#[test]
fn test_session_creation() {
    let session = Session::new("test-session".to_string(), "/path/to/workspace".to_string());

    assert_eq!(session.name, "test-session");
    assert_eq!(session.workspace_path, "/path/to/workspace");
    assert_eq!(session.branch_name, "claude/test-session");
    assert!(matches!(session.status, SessionStatus::Stopped));
    assert_eq!(session.container_id, None);
    assert_eq!(session.git_changes.total(), 0);
}

#[test]
fn test_session_branch_name_formatting() {
    let session1 = Session::new("Fix Auth Bug".to_string(), "/workspace".to_string());
    assert_eq!(session1.branch_name, "claude/fix-auth-bug");

    let session2 = Session::new("add new feature".to_string(), "/workspace".to_string());
    assert_eq!(session2.branch_name, "claude/add-new-feature");
}

#[test]
fn test_session_status_indicator() {
    assert_eq!(SessionStatus::Running.indicator(), "●");
    assert_eq!(SessionStatus::Stopped.indicator(), "⏸");
    assert_eq!(SessionStatus::Error("test".to_string()).indicator(), "✗");
}

#[test]
fn test_session_status_is_running() {
    assert!(SessionStatus::Running.is_running());
    assert!(!SessionStatus::Stopped.is_running());
    assert!(!SessionStatus::Error("test".to_string()).is_running());
}

#[test]
fn test_session_update_last_accessed() {
    let mut session = Session::new("test".to_string(), "/workspace".to_string());
    let original_time = session.last_accessed;

    std::thread::sleep(std::time::Duration::from_millis(1));
    session.update_last_accessed();

    assert!(session.last_accessed > original_time);
}

#[test]
fn test_session_set_status_updates_time() {
    let mut session = Session::new("test".to_string(), "/workspace".to_string());
    let original_time = session.last_accessed;

    std::thread::sleep(std::time::Duration::from_millis(1));
    session.set_status(SessionStatus::Running);

    assert!(matches!(session.status, SessionStatus::Running));
    assert!(session.last_accessed > original_time);
}

#[test]
fn test_git_changes_total() {
    let mut changes = GitChanges::default();
    assert_eq!(changes.total(), 0);

    changes.added = 5;
    changes.modified = 3;
    changes.deleted = 2;
    assert_eq!(changes.total(), 10);
}

#[test]
fn test_git_changes_format() {
    let mut changes = GitChanges::default();
    assert_eq!(changes.format(), "No changes");

    changes.added = 42;
    changes.modified = 0;
    changes.deleted = 13;
    assert_eq!(changes.format(), "+42 ~0 -13");
}

#[test]
fn test_session_serialization() {
    let session = Session::new("test".to_string(), "/workspace".to_string());

    let serialized = serde_json::to_string(&session).expect("Failed to serialize session");
    let deserialized: Session =
        serde_json::from_str(&serialized).expect("Failed to deserialize session");

    assert_eq!(session.id, deserialized.id);
    assert_eq!(session.name, deserialized.name);
    assert_eq!(session.workspace_path, deserialized.workspace_path);
    assert_eq!(session.branch_name, deserialized.branch_name);
}
