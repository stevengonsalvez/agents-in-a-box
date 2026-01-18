// ABOUTME: Behavioral tests for SessionStore format compatibility between CLI and TUI
//
// Verifies JSON format, round-trip persistence, and keyed operations by tmux session name.

use ainb::interactive::session_manager::{SessionMetadata, SessionStore};
use chrono::{Datelike, Utc};
use std::path::PathBuf;
use uuid::Uuid;

/// Helper: Create a SessionMetadata with specified values
fn create_session_metadata(
    tmux_name: &str,
    worktree_path: &str,
    workspace_name: &str,
) -> SessionMetadata {
    SessionMetadata {
        session_id: Uuid::new_v4(),
        tmux_session_name: tmux_name.to_string(),
        worktree_path: PathBuf::from(worktree_path),
        workspace_name: workspace_name.to_string(),
        created_at: Utc::now(),
    }
}

/// Helper: Create a SessionMetadata with a fixed UUID for deterministic tests
fn create_session_metadata_with_id(
    session_id: Uuid,
    tmux_name: &str,
    worktree_path: &str,
    workspace_name: &str,
) -> SessionMetadata {
    SessionMetadata {
        session_id,
        tmux_session_name: tmux_name.to_string(),
        worktree_path: PathBuf::from(worktree_path),
        workspace_name: workspace_name.to_string(),
        created_at: Utc::now(),
    }
}

/// Test 1: Save and load preserves all data (roundtrip)
#[test]
fn test_session_store_roundtrip() {
    // Arrange: Create a store with multiple sessions
    let mut store = SessionStore::default();

    let session1 = create_session_metadata(
        "tmux_feature-auth",
        "/path/to/worktree1",
        "my-project",
    );
    let session2 = create_session_metadata(
        "tmux_bugfix-123",
        "/path/to/worktree2",
        "another-project",
    );

    let session1_id = session1.session_id;
    let session2_id = session2.session_id;
    let session1_path = session1.worktree_path.clone();
    let session2_path = session2.worktree_path.clone();
    let session1_created = session1.created_at;
    let session2_created = session2.created_at;

    store.upsert(session1);
    store.upsert(session2);

    // Act: Serialize and deserialize
    let json = serde_json::to_string_pretty(&store).expect("serialize failed");
    let loaded: SessionStore = serde_json::from_str(&json).expect("deserialize failed");

    // Assert: All data preserved
    assert_eq!(loaded.sessions.len(), 2);

    let loaded_session1 = loaded.find_by_tmux_name("tmux_feature-auth")
        .expect("session1 not found");
    assert_eq!(loaded_session1.session_id, session1_id);
    assert_eq!(loaded_session1.worktree_path, session1_path);
    assert_eq!(loaded_session1.workspace_name, "my-project");
    assert_eq!(loaded_session1.created_at, session1_created);

    let loaded_session2 = loaded.find_by_tmux_name("tmux_bugfix-123")
        .expect("session2 not found");
    assert_eq!(loaded_session2.session_id, session2_id);
    assert_eq!(loaded_session2.worktree_path, session2_path);
    assert_eq!(loaded_session2.workspace_name, "another-project");
    assert_eq!(loaded_session2.created_at, session2_created);
}

/// Test 2: Parse exact TUI JSON format
#[test]
fn test_session_store_json_format_matches_tui() {
    // Arrange: JSON in the exact format the TUI produces
    let tui_json = r#"{
  "sessions": {
    "tmux_session-name": {
      "session_id": "550e8400-e29b-41d4-a716-446655440000",
      "tmux_session_name": "tmux_session-name",
      "worktree_path": "/Users/dev/project/.worktrees/feature-branch",
      "workspace_name": "my-workspace",
      "created_at": "2026-01-17T18:25:46.150247Z"
    }
  }
}"#;

    // Act: Parse the JSON
    let store: SessionStore = serde_json::from_str(tui_json)
        .expect("failed to parse TUI format");

    // Assert: All fields parsed correctly
    assert_eq!(store.sessions.len(), 1);

    let session = store.find_by_tmux_name("tmux_session-name")
        .expect("session not found");

    assert_eq!(
        session.session_id.to_string(),
        "550e8400-e29b-41d4-a716-446655440000"
    );
    assert_eq!(session.tmux_session_name, "tmux_session-name");
    assert_eq!(
        session.worktree_path,
        PathBuf::from("/Users/dev/project/.worktrees/feature-branch")
    );
    assert_eq!(session.workspace_name, "my-workspace");

    // Verify datetime parsing (ISO 8601 format)
    assert_eq!(session.created_at.year(), 2026);
    assert_eq!(session.created_at.month(), 1);
    assert_eq!(session.created_at.day(), 17);
}

/// Test 3: Sessions are indexed by tmux_session_name
#[test]
fn test_session_store_keyed_by_tmux_name() {
    // Arrange: Create store and add sessions
    let mut store = SessionStore::default();

    let session = create_session_metadata(
        "tmux_my-feature",
        "/path/to/worktree",
        "workspace",
    );

    store.upsert(session);

    // Act: Serialize to JSON
    let json = serde_json::to_string(&store).expect("serialize failed");

    // Assert: JSON uses tmux name as key
    let parsed: serde_json::Value = serde_json::from_str(&json).expect("parse failed");

    // The sessions object should have the tmux name as key
    let sessions_obj = parsed.get("sessions").expect("no sessions field");
    assert!(
        sessions_obj.get("tmux_my-feature").is_some(),
        "Session should be keyed by tmux_session_name"
    );

    // Also verify via the API
    assert!(store.find_by_tmux_name("tmux_my-feature").is_some());
    assert!(store.find_by_tmux_name("nonexistent").is_none());

    // Check tracked_tmux_names includes our session
    let tracked = store.tracked_tmux_names();
    assert!(tracked.contains(&"tmux_my-feature"));
}

/// Test 4: Upsert updates existing session (same tmux name)
#[test]
fn test_session_store_upsert_updates_existing() {
    // Arrange: Create store with initial session
    let mut store = SessionStore::default();

    let original_id = Uuid::new_v4();
    let original_session = create_session_metadata_with_id(
        original_id,
        "tmux_shared-name",
        "/original/path",
        "original-workspace",
    );

    store.upsert(original_session);
    assert_eq!(store.sessions.len(), 1);

    // Act: Upsert with same tmux name but different data
    let updated_id = Uuid::new_v4();
    let updated_session = create_session_metadata_with_id(
        updated_id,
        "tmux_shared-name",  // Same tmux name
        "/updated/path",     // Different path
        "updated-workspace", // Different workspace
    );

    store.upsert(updated_session);

    // Assert: Still only one session, but with updated values
    assert_eq!(store.sessions.len(), 1);

    let session = store.find_by_tmux_name("tmux_shared-name")
        .expect("session not found");

    // Should have the NEW values
    assert_eq!(session.session_id, updated_id);
    assert_eq!(session.worktree_path, PathBuf::from("/updated/path"));
    assert_eq!(session.workspace_name, "updated-workspace");

    // Should NOT have the old values
    assert_ne!(session.session_id, original_id);
}

/// Test 5: Remove by tmux name works
#[test]
fn test_session_store_remove_by_tmux_name() {
    // Arrange: Create store with multiple sessions
    let mut store = SessionStore::default();

    store.upsert(create_session_metadata(
        "tmux_to-keep",
        "/path/keep",
        "workspace-keep",
    ));
    store.upsert(create_session_metadata(
        "tmux_to-remove",
        "/path/remove",
        "workspace-remove",
    ));
    store.upsert(create_session_metadata(
        "tmux_also-keep",
        "/path/also-keep",
        "workspace-also-keep",
    ));

    assert_eq!(store.sessions.len(), 3);

    // Act: Remove by tmux name
    store.remove_by_tmux_name("tmux_to-remove");

    // Assert: Only the target session removed
    assert_eq!(store.sessions.len(), 2);
    assert!(store.find_by_tmux_name("tmux_to-keep").is_some());
    assert!(store.find_by_tmux_name("tmux_also-keep").is_some());
    assert!(store.find_by_tmux_name("tmux_to-remove").is_none());

    // Removing non-existent should be a no-op
    store.remove_by_tmux_name("tmux_nonexistent");
    assert_eq!(store.sessions.len(), 2);
}

/// Test 6: Remove by session_id (UUID) works
#[test]
fn test_session_store_remove_by_session_id() {
    // Arrange: Create store with sessions having known UUIDs
    let mut store = SessionStore::default();

    let id_to_keep = Uuid::new_v4();
    let id_to_remove = Uuid::new_v4();
    let id_also_keep = Uuid::new_v4();

    store.upsert(create_session_metadata_with_id(
        id_to_keep,
        "tmux_keep-1",
        "/path/1",
        "workspace-1",
    ));
    store.upsert(create_session_metadata_with_id(
        id_to_remove,
        "tmux_remove-me",
        "/path/2",
        "workspace-2",
    ));
    store.upsert(create_session_metadata_with_id(
        id_also_keep,
        "tmux_keep-2",
        "/path/3",
        "workspace-3",
    ));

    assert_eq!(store.sessions.len(), 3);

    // Act: Remove by session_id
    store.remove_by_session_id(id_to_remove);

    // Assert: Only the target session removed
    assert_eq!(store.sessions.len(), 2);
    assert!(store.find_by_tmux_name("tmux_keep-1").is_some());
    assert!(store.find_by_tmux_name("tmux_keep-2").is_some());
    assert!(store.find_by_tmux_name("tmux_remove-me").is_none());

    // Verify the remaining sessions have correct IDs
    let kept_1 = store.find_by_tmux_name("tmux_keep-1").unwrap();
    assert_eq!(kept_1.session_id, id_to_keep);

    let kept_2 = store.find_by_tmux_name("tmux_keep-2").unwrap();
    assert_eq!(kept_2.session_id, id_also_keep);

    // Removing non-existent UUID should be a no-op
    let random_id = Uuid::new_v4();
    store.remove_by_session_id(random_id);
    assert_eq!(store.sessions.len(), 2);
}

/// Additional test: Verify serialized JSON matches expected format exactly
#[test]
fn test_session_store_serialization_format() {
    // Arrange: Create store with known data
    let mut store = SessionStore::default();

    let fixed_id = Uuid::parse_str("12345678-1234-1234-1234-123456789abc").unwrap();
    let mut session = create_session_metadata_with_id(
        fixed_id,
        "tmux_test-session",
        "/test/path",
        "test-workspace",
    );
    // Use a fixed timestamp for deterministic output
    session.created_at = chrono::DateTime::parse_from_rfc3339("2026-01-17T12:00:00Z")
        .unwrap()
        .with_timezone(&Utc);

    store.upsert(session);

    // Act: Serialize
    let json = serde_json::to_string_pretty(&store).expect("serialize failed");

    // Assert: Parse and verify structure
    let parsed: serde_json::Value = serde_json::from_str(&json).expect("parse failed");

    // Verify top-level structure
    assert!(parsed.is_object());
    assert!(parsed.get("sessions").is_some());

    // Verify session structure
    let sessions = parsed.get("sessions").unwrap();
    let session_data = sessions.get("tmux_test-session").unwrap();

    assert_eq!(session_data.get("session_id").unwrap(), "12345678-1234-1234-1234-123456789abc");
    assert_eq!(session_data.get("tmux_session_name").unwrap(), "tmux_test-session");
    assert_eq!(session_data.get("worktree_path").unwrap(), "/test/path");
    assert_eq!(session_data.get("workspace_name").unwrap(), "test-workspace");
    assert!(session_data.get("created_at").is_some());
}

/// Test: Empty store serializes correctly
#[test]
fn test_empty_session_store() {
    let store = SessionStore::default();

    // Serialize empty store
    let json = serde_json::to_string(&store).expect("serialize failed");

    // Parse and verify
    let parsed: serde_json::Value = serde_json::from_str(&json).expect("parse failed");
    let sessions = parsed.get("sessions").expect("no sessions field");

    assert!(sessions.is_object());
    assert_eq!(sessions.as_object().unwrap().len(), 0);

    // Round-trip
    let loaded: SessionStore = serde_json::from_str(&json).expect("deserialize failed");
    assert_eq!(loaded.sessions.len(), 0);
}
