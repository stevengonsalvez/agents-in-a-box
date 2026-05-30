//! Sessions screen mouse behavior regression tests.

use ainb::app::events::AppEvent;
use ainb::app::screens::ids as screen_ids;
use ainb::app::state::FocusedPane;
use ainb::app::{AppState, EventHandler};
use ainb::models::{Session, Workspace};
use ratatui::layout::Rect;
use std::sync::Mutex;

static HOME_LOCK: Mutex<()> = Mutex::new(());

fn state_with_sessions(count: usize) -> (tempfile::TempDir, AppState) {
    let temp_home = tempfile::tempdir().expect("temp home");
    std::env::set_var("HOME", temp_home.path());

    let mut state = AppState::new();
    state.current_screen = screen_ids::SESSION_LIST.to_string();
    state.selected_workspace_index = Some(0);
    state.selected_session_index = Some(0);

    let mut workspace = Workspace::new("repo".to_string(), "/tmp/repo".into());
    for index in 0..count {
        workspace.add_session(Session::new(
            format!("session-{}", index + 1),
            "/tmp/repo".to_string(),
        ));
    }
    state.workspaces = vec![workspace];

    state
        .sessions_pane_state
        .set_layout(Rect::new(0, 3, 40, 20), Rect::new(40, 3, 80, 20));
    state.sessions_pane_state.set_list_scroll_offset(0);

    (temp_home, state)
}

fn state_with_two_sessions() -> (tempfile::TempDir, AppState) {
    state_with_sessions(2)
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
fn sessions_mouse_double_click_attaches_selected_session_row() {
    let _guard = HOME_LOCK.lock().expect("home env lock");
    let (_home, mut state) = state_with_two_sessions();

    let first = EventHandler::handle_mouse_event(AppEvent::MouseClick { x: 8, y: 6 }, &mut state);
    let second = EventHandler::handle_mouse_event(AppEvent::MouseClick { x: 8, y: 6 }, &mut state);

    assert!(first.is_none());
    assert!(matches!(second, Some(AppEvent::AttachTmuxSession)));
    assert_eq!(state.selected_workspace_index, Some(0));
    assert_eq!(state.selected_session_index, Some(1));
}

#[test]
fn sessions_mouse_double_click_requires_same_attachable_row() {
    let _guard = HOME_LOCK.lock().expect("home env lock");
    let (_home, mut state) = state_with_two_sessions();

    let first = EventHandler::handle_mouse_event(AppEvent::MouseClick { x: 8, y: 5 }, &mut state);
    let second = EventHandler::handle_mouse_event(AppEvent::MouseClick { x: 8, y: 6 }, &mut state);

    assert!(first.is_none());
    assert!(second.is_none());
    assert_eq!(state.selected_workspace_index, Some(0));
    assert_eq!(state.selected_session_index, Some(1));
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
    let (home, mut state) = state_with_two_sessions();

    EventHandler::handle_mouse_event(AppEvent::MouseClick { x: 2, y: 3 }, &mut state);
    assert!(state.sessions_pane_state.collapsed);
    assert_eq!(state.sessions_pane_state.effective_width(120), 5);

    state
        .sessions_pane_state
        .set_layout(Rect::new(0, 3, 5, 20), Rect::new(5, 3, 115, 20));
    EventHandler::handle_mouse_event(AppEvent::MouseClick { x: 2, y: 4 }, &mut state);

    assert!(!state.sessions_pane_state.collapsed);
    assert_eq!(state.sessions_pane_state.effective_width(120), 40);

    let config = std::fs::read_to_string(home.path().join(".agents-in-a-box/config/config.toml"))
        .expect("persisted config");
    assert!(config.contains("sessions_sidebar_collapsed = false"));
}

#[test]
fn sessions_mouse_wheel_down_over_sessions_moves_selection() {
    let _guard = HOME_LOCK.lock().expect("home env lock");
    let (_home, mut state) = state_with_sessions(5);

    let handled = state.scroll_session_list_by_mouse(8, 6, true, 3);

    assert!(handled);
    assert_eq!(state.focused_pane, FocusedPane::Sessions);
    assert_eq!(state.selected_session_index, Some(3));
    assert!(state.pending_async_action.is_some());
}

#[test]
fn sessions_mouse_wheel_up_over_sessions_moves_selection() {
    let _guard = HOME_LOCK.lock().expect("home env lock");
    let (_home, mut state) = state_with_sessions(5);
    state.selected_session_index = Some(3);

    let handled = state.scroll_session_list_by_mouse(8, 6, false, 2);

    assert!(handled);
    assert_eq!(state.focused_pane, FocusedPane::Sessions);
    assert_eq!(state.selected_session_index, Some(1));
}

#[test]
fn sessions_mouse_wheel_over_preview_preserves_log_scroll_path() {
    let _guard = HOME_LOCK.lock().expect("home env lock");
    let (_home, mut state) = state_with_sessions(5);
    state.focused_pane = FocusedPane::Sessions;

    let handled = state.scroll_session_list_by_mouse(50, 6, true, 3);

    assert!(!handled);
    assert_eq!(state.focused_pane, FocusedPane::LiveLogs);
    assert_eq!(state.selected_session_index, Some(0));
    assert!(state.pending_async_action.is_none());
}
