// ABOUTME: Unit tests for AppState to ensure navigation and state management work correctly

use ainb::app::AppState;
use ainb::models::{Session, SessionStatus, Workspace};
use std::path::PathBuf;

fn create_test_state() -> AppState {
    let mut state = AppState::default();

    let mut workspace1 = Workspace::new("project1".to_string(), PathBuf::from("/project1"));
    let mut session1 = Session::new(
        "session1".to_string(),
        workspace1.path.to_string_lossy().to_string(),
    );
    session1.set_status(SessionStatus::Running);
    let session2 = Session::new(
        "session2".to_string(),
        workspace1.path.to_string_lossy().to_string(),
    );

    workspace1.add_session(session1);
    workspace1.add_session(session2);

    let mut workspace2 = Workspace::new("project2".to_string(), PathBuf::from("/project2"));
    let session3 = Session::new(
        "session3".to_string(),
        workspace2.path.to_string_lossy().to_string(),
    );
    workspace2.add_session(session3);

    state.workspaces.push(workspace1);
    state.workspaces.push(workspace2);
    state.selected_workspace_index = Some(0);
    state.selected_session_index = Some(0);

    state
}

#[test]
fn test_app_state_creation() {
    let state = AppState::new();

    // AppState::new() should create an empty initial state
    assert!(state.workspaces.is_empty());
    assert!(state.selected_workspace_index.is_none());
    assert!(!state.should_quit);
    assert!(!state.help_visible);
}

#[test]
fn test_selected_session() {
    let state = create_test_state();

    let selected_session = state.selected_session();
    assert!(selected_session.is_some());
    assert_eq!(selected_session.unwrap().name, "session1");
}

#[test]
fn test_selected_session_none() {
    let mut state = AppState::default();
    assert!(state.selected_session().is_none());

    let workspace = Workspace::new("empty".to_string(), PathBuf::from("/empty"));
    state.workspaces.push(workspace);
    state.selected_workspace_index = Some(0);

    assert!(state.selected_session().is_none());
}

#[test]
fn test_next_session() {
    let mut state = create_test_state();

    assert_eq!(state.selected_session_index, Some(0));

    state.next_session();
    assert_eq!(state.selected_session_index, Some(1));

    state.next_session();
    assert_eq!(state.selected_session_index, Some(0)); // wraps around
}

#[test]
fn test_previous_session() {
    let mut state = create_test_state();
    state.selected_session_index = Some(1);

    state.previous_session();
    assert_eq!(state.selected_session_index, Some(0));

    state.previous_session();
    assert_eq!(state.selected_session_index, Some(1)); // wraps around
}

#[test]
fn test_next_workspace() {
    let mut state = create_test_state();

    assert_eq!(state.selected_workspace_index, Some(0));
    assert_eq!(state.selected_session_index, Some(0));

    state.next_workspace();
    assert_eq!(state.selected_workspace_index, Some(1));
    assert_eq!(state.selected_session_index, Some(0)); // resets to first session

    state.next_workspace();
    assert_eq!(state.selected_workspace_index, Some(0)); // wraps around
}

#[test]
fn test_previous_workspace() {
    let mut state = create_test_state();
    state.selected_workspace_index = Some(1);

    state.previous_workspace();
    assert_eq!(state.selected_workspace_index, Some(0));
    assert_eq!(state.selected_session_index, Some(0));

    state.previous_workspace();
    assert_eq!(state.selected_workspace_index, Some(1)); // wraps around
}

#[test]
fn test_toggle_help() {
    let mut state = AppState::default();

    assert!(!state.help_visible);

    state.toggle_help();
    assert!(state.help_visible);

    state.toggle_help();
    assert!(!state.help_visible);
}

#[test]
fn test_quit() {
    let mut state = AppState::default();

    assert!(!state.should_quit);

    state.quit();
    assert!(state.should_quit);
}

#[test]
fn test_navigation_with_empty_workspace() {
    let mut state = AppState::default();
    let workspace = Workspace::new("empty".to_string(), PathBuf::from("/empty"));
    state.workspaces.push(workspace);
    state.selected_workspace_index = Some(0);
    state.selected_session_index = None;

    state.next_session();
    assert_eq!(state.selected_session_index, None);

    state.previous_session();
    assert_eq!(state.selected_session_index, None);
}
