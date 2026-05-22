//! Sessions screen mouse behavior regression tests.

use ainb::app::events::AppEvent;
use ainb::app::screens::ids as screen_ids;
use ainb::app::{AppState, EventHandler};
use ainb::models::{Session, Workspace};
use ratatui::layout::Rect;
use std::sync::Mutex;

static HOME_LOCK: Mutex<()> = Mutex::new(());

fn state_with_two_sessions() -> (tempfile::TempDir, AppState) {
    let temp_home = tempfile::tempdir().expect("temp home");
    std::env::set_var("HOME", temp_home.path());

    let mut state = AppState::new();
    state.current_screen = screen_ids::SESSION_LIST.to_string();
    state.selected_workspace_index = Some(0);
    state.selected_session_index = Some(0);

    let mut workspace = Workspace::new("repo".to_string(), "/tmp/repo".into());
    workspace.add_session(Session::new("one".to_string(), "/tmp/repo".to_string()));
    workspace.add_session(Session::new("two".to_string(), "/tmp/repo".to_string()));
    state.workspaces = vec![workspace];

    state
        .sessions_pane_state
        .set_layout(Rect::new(0, 3, 40, 20), Rect::new(40, 3, 80, 20));
    state.sessions_pane_state.set_list_scroll_offset(0);

    (temp_home, state)
}

#[test]
fn sessions_mouse_click_selects_session_row_without_async_work() {
    let _guard = HOME_LOCK.lock().expect("home env lock");
    let (_home, mut state) = state_with_two_sessions();

    // y=3 is the top border, y=4 workspace header, y=5 first session, y=6 second session.
    let outcome = EventHandler::handle_mouse_event(AppEvent::MouseClick { x: 8, y: 6 }, &mut state);

    assert!(outcome.is_none());
    assert_eq!(state.selected_workspace_index, Some(0));
    assert_eq!(state.selected_session_index, Some(1));
    assert!(state.pending_async_action.is_some());
}

#[test]
fn sessions_mouse_drag_resizes_and_persists_on_release_only() {
    let _guard = HOME_LOCK.lock().expect("home env lock");
    let (home, mut state) = state_with_two_sessions();

    EventHandler::handle_mouse_event(AppEvent::MouseClick { x: 39, y: 8 }, &mut state);
    EventHandler::handle_mouse_event(AppEvent::MouseDragging { x: 55, y: 8 }, &mut state);

    assert_eq!(state.sessions_pane_state.preferred_width, 56);
    let config_path = home.path().join(".agents-in-a-box/config/config.toml");
    assert!(
        !config_path.exists(),
        "drag hot path should not persist config before mouse release"
    );

    EventHandler::handle_mouse_event(AppEvent::MouseDragEnd { x: 55, y: 8 }, &mut state);

    let config = std::fs::read_to_string(config_path).expect("persisted config");
    assert!(config.contains("sessions_sidebar_width = 56"));
}

#[test]
fn sessions_mouse_toggle_collapses_and_expands_sidebar() {
    let _guard = HOME_LOCK.lock().expect("home env lock");
    let (_home, mut state) = state_with_two_sessions();

    EventHandler::handle_mouse_event(AppEvent::MouseClick { x: 2, y: 3 }, &mut state);
    assert!(state.sessions_pane_state.collapsed);
    assert_eq!(state.sessions_pane_state.effective_width(120), 5);

    state
        .sessions_pane_state
        .set_layout(Rect::new(0, 3, 5, 20), Rect::new(5, 3, 115, 20));
    EventHandler::handle_mouse_event(AppEvent::MouseClick { x: 1, y: 3 }, &mut state);

    assert!(!state.sessions_pane_state.collapsed);
    assert_eq!(state.sessions_pane_state.effective_width(120), 40);
}
