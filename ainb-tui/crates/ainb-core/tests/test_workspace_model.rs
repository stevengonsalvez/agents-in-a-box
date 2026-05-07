// ABOUTME: Unit tests for Workspace model to ensure proper session management and operations

use ainb::models::{Session, SessionStatus, Workspace};
use std::path::PathBuf;

#[test]
fn test_workspace_creation() {
    let workspace = Workspace::new("test-workspace".to_string(), PathBuf::from("/path/to/repo"));

    assert_eq!(workspace.name, "test-workspace");
    assert_eq!(workspace.path, PathBuf::from("/path/to/repo"));
    assert_eq!(workspace.sessions.len(), 0);
}

#[test]
fn test_workspace_add_session() {
    let mut workspace = Workspace::new("test".to_string(), PathBuf::from("/workspace"));
    let session = Session::new(
        "test-session".to_string(),
        workspace.path.to_string_lossy().to_string(),
    );
    let session_id = session.id;

    workspace.add_session(session);

    assert_eq!(workspace.sessions.len(), 1);
    assert_eq!(workspace.sessions[0].id, session_id);
}

#[test]
fn test_workspace_remove_session() {
    let mut workspace = Workspace::new("test".to_string(), PathBuf::from("/workspace"));
    let session = Session::new(
        "test-session".to_string(),
        workspace.path.to_string_lossy().to_string(),
    );
    let session_id = session.id;

    workspace.add_session(session);
    assert_eq!(workspace.sessions.len(), 1);

    let removed = workspace.remove_session(&session_id);
    assert!(removed);
    assert_eq!(workspace.sessions.len(), 0);

    let not_removed = workspace.remove_session(&session_id);
    assert!(!not_removed);
}

#[test]
fn test_workspace_get_session() {
    let mut workspace = Workspace::new("test".to_string(), PathBuf::from("/workspace"));
    let session = Session::new(
        "test-session".to_string(),
        workspace.path.to_string_lossy().to_string(),
    );
    let session_id = session.id;

    workspace.add_session(session);

    let found_session = workspace.get_session(&session_id);
    assert!(found_session.is_some());
    assert_eq!(found_session.unwrap().id, session_id);

    let not_found = workspace.get_session(&uuid::Uuid::new_v4());
    assert!(not_found.is_none());
}

#[test]
fn test_workspace_get_session_mut() {
    let mut workspace = Workspace::new("test".to_string(), PathBuf::from("/workspace"));
    let session = Session::new(
        "test-session".to_string(),
        workspace.path.to_string_lossy().to_string(),
    );
    let session_id = session.id;

    workspace.add_session(session);

    let found_session = workspace.get_session_mut(&session_id);
    assert!(found_session.is_some());

    let session_mut = found_session.unwrap();
    session_mut.set_status(SessionStatus::Running);

    let updated_session = workspace.get_session(&session_id).unwrap();
    assert!(matches!(updated_session.status, SessionStatus::Running));
}

#[test]
fn test_workspace_running_sessions() {
    let mut workspace = Workspace::new("test".to_string(), PathBuf::from("/workspace"));

    let mut session1 = Session::new(
        "session1".to_string(),
        workspace.path.to_string_lossy().to_string(),
    );
    session1.set_status(SessionStatus::Running);

    let mut session2 = Session::new(
        "session2".to_string(),
        workspace.path.to_string_lossy().to_string(),
    );
    session2.set_status(SessionStatus::Stopped);

    let mut session3 = Session::new(
        "session3".to_string(),
        workspace.path.to_string_lossy().to_string(),
    );
    session3.set_status(SessionStatus::Running);

    workspace.add_session(session1);
    workspace.add_session(session2);
    workspace.add_session(session3);

    let running_sessions = workspace.running_sessions();
    assert_eq!(running_sessions.len(), 2);
    assert!(running_sessions.iter().all(|s| s.status.is_running()));
}

#[test]
fn test_workspace_session_count() {
    let mut workspace = Workspace::new("test".to_string(), PathBuf::from("/workspace"));
    assert_eq!(workspace.session_count(), 0);

    workspace.add_session(Session::new(
        "session1".to_string(),
        workspace.path.to_string_lossy().to_string(),
    ));
    assert_eq!(workspace.session_count(), 1);

    workspace.add_session(Session::new(
        "session2".to_string(),
        workspace.path.to_string_lossy().to_string(),
    ));
    assert_eq!(workspace.session_count(), 2);
}
