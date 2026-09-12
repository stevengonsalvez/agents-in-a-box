//! Sessions screen mouse behavior regression tests.

use ainb::app::events::AppEvent;
use ainb::app::screens::ids as screen_ids;
use ainb::app::state::FocusedPane;
use ainb::app::ui_state::UiState;
use ainb::app::{AppState, EventHandler};
use ainb::components::session_list::SessionListComponent;
use ainb::models::{Session, Workspace};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::layout::Rect;
use std::sync::Mutex;

static HOME_LOCK: Mutex<()> = Mutex::new(());

/// The sessions panel rect the fixture pins, mirrored by the render below so
/// hit-testing and painting agree on the same geometry.
const SESSIONS_RECT: Rect = Rect {
    x: 0,
    y: 3,
    width: 40,
    height: 20,
};

fn state_with_sessions(count: usize) -> (tempfile::TempDir, AppState, UiState) {
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

    let mut ui = UiState::default();
    ui.sessions_pane.set_layout(SESSIONS_RECT, Rect::new(40, 3, 80, 20));
    ui.sessions_pane.set_list_scroll_offset(0);
    // Hit-testing reads the per-item heights the renderer records, because a
    // session row is taller than the one line a header takes. Painting the
    // real component is the only way to get heights that match what the user
    // clicks on; a fixture that assumed one row per line silently mapped every
    // click onto the wrong session.
    paint_session_list(&state, &mut ui);

    (temp_home, state, ui)
}

/// Draw the real sessions panel once so `SessionsPaneState` carries the item
/// heights and scroll offset of an actual frame.
fn paint_session_list(state: &AppState, ui: &mut UiState) {
    let mut terminal = Terminal::new(TestBackend::new(120, 30)).expect("test terminal");
    let mut list = SessionListComponent::new();
    terminal
        .draw(|frame| list.render(frame, SESSIONS_RECT, state, ui))
        .expect("draw sessions panel");
}

/// First terminal row occupied by list row `row_index`, resolved through the
/// same hit test the mouse handler uses. Tests name the row they mean instead
/// of a `y` that goes stale the moment a row grows a second line.
fn row_y(ui: &UiState, row_index: usize) -> u16 {
    let first = SESSIONS_RECT.y + 1;
    let last = SESSIONS_RECT.y + SESSIONS_RECT.height - 1;
    (first..last)
        .find(|&y| ui.sessions_pane.row_index_at(8, y) == Some(row_index))
        .unwrap_or_else(|| panic!("list row {row_index} is not on screen"))
}

/// Row 0 is the workspace header, so session `n` is list row `n + 1`.
fn session_row_y(ui: &UiState, session_index: usize) -> u16 {
    row_y(ui, session_index + 1)
}

fn state_with_two_sessions() -> (tempfile::TempDir, AppState, UiState) {
    state_with_sessions(2)
}

#[test]
fn sessions_mouse_click_selects_session_row_without_async_work() {
    let _guard = HOME_LOCK.lock().expect("home env lock");
    let (_home, mut state, mut ui) = state_with_two_sessions();

    let second = session_row_y(&ui, 1);
    let outcome = EventHandler::handle_mouse_event(
        AppEvent::MouseClick { x: 8, y: second },
        &mut state,
        &mut ui,
    );

    assert!(outcome.is_none());
    assert_eq!(state.selected_workspace_index, Some(0));
    assert_eq!(state.selected_session_index, Some(1));
    assert!(state.pending_async_action.is_some());
}

#[test]
fn sessions_mouse_double_click_attaches_selected_session_row() {
    let _guard = HOME_LOCK.lock().expect("home env lock");
    let (_home, mut state, mut ui) = state_with_two_sessions();

    let row = session_row_y(&ui, 1);
    let first = EventHandler::handle_mouse_event(
        AppEvent::MouseClick { x: 8, y: row },
        &mut state,
        &mut ui,
    );
    let second = EventHandler::handle_mouse_event(
        AppEvent::MouseClick { x: 8, y: row },
        &mut state,
        &mut ui,
    );

    assert!(first.is_none());
    assert!(matches!(second, Some(AppEvent::AttachTmuxSession)));
    assert_eq!(state.selected_workspace_index, Some(0));
    assert_eq!(state.selected_session_index, Some(1));
}

#[test]
fn sessions_mouse_double_click_requires_same_attachable_row() {
    let _guard = HOME_LOCK.lock().expect("home env lock");
    let (_home, mut state, mut ui) = state_with_two_sessions();

    let first_row = session_row_y(&ui, 0);
    let second_row = session_row_y(&ui, 1);
    let first = EventHandler::handle_mouse_event(
        AppEvent::MouseClick { x: 8, y: first_row },
        &mut state,
        &mut ui,
    );
    let second = EventHandler::handle_mouse_event(
        AppEvent::MouseClick {
            x: 8,
            y: second_row,
        },
        &mut state,
        &mut ui,
    );

    assert!(first.is_none());
    assert!(second.is_none());
    assert_eq!(state.selected_workspace_index, Some(0));
    assert_eq!(state.selected_session_index, Some(1));
}

#[test]
fn sessions_mouse_drag_resizes_and_persists_on_release_only() {
    let _guard = HOME_LOCK.lock().expect("home env lock");
    let (home, mut state, mut ui) = state_with_two_sessions();

    EventHandler::handle_mouse_event(AppEvent::MouseClick { x: 39, y: 8 }, &mut state, &mut ui);
    EventHandler::handle_mouse_event(AppEvent::MouseDragging { x: 55, y: 8 }, &mut state, &mut ui);

    assert_eq!(ui.sessions_pane.preferred_width, 56);
    let config_path = home.path().join(".agents-in-a-box/config/config.toml");
    assert!(
        !config_path.exists(),
        "drag hot path should not persist config before mouse release"
    );

    EventHandler::handle_mouse_event(AppEvent::MouseDragEnd { x: 55, y: 8 }, &mut state, &mut ui);

    let config = std::fs::read_to_string(config_path).expect("persisted config");
    assert!(config.contains("sessions_sidebar_width = 56"));
}

#[test]
fn sessions_mouse_toggle_collapses_and_expands_sidebar() {
    let _guard = HOME_LOCK.lock().expect("home env lock");
    let (home, mut state, mut ui) = state_with_two_sessions();

    EventHandler::handle_mouse_event(AppEvent::MouseClick { x: 2, y: 3 }, &mut state, &mut ui);
    assert!(ui.sessions_pane.collapsed);
    assert_eq!(ui.sessions_pane.effective_width(120), 5);

    ui.sessions_pane.set_layout(Rect::new(0, 3, 5, 20), Rect::new(5, 3, 115, 20));
    EventHandler::handle_mouse_event(AppEvent::MouseClick { x: 2, y: 4 }, &mut state, &mut ui);

    assert!(!ui.sessions_pane.collapsed);
    assert_eq!(ui.sessions_pane.effective_width(120), 40);

    let config = std::fs::read_to_string(home.path().join(".agents-in-a-box/config/config.toml"))
        .expect("persisted config");
    assert!(config.contains("sessions_sidebar_collapsed = false"));
}

#[test]
fn sessions_mouse_wheel_down_over_sessions_moves_selection() {
    let _guard = HOME_LOCK.lock().expect("home env lock");
    let (_home, mut state, ui) = state_with_sessions(5);

    let handled = state.scroll_session_list_by_mouse(&ui.sessions_pane, 8, 6, true, 3);

    assert!(handled);
    assert_eq!(state.focused_pane, FocusedPane::Sessions);
    assert_eq!(state.selected_session_index, Some(3));
    assert!(state.pending_async_action.is_some());
}

#[test]
fn sessions_mouse_wheel_up_over_sessions_moves_selection() {
    let _guard = HOME_LOCK.lock().expect("home env lock");
    let (_home, mut state, ui) = state_with_sessions(5);
    state.selected_session_index = Some(3);

    let handled = state.scroll_session_list_by_mouse(&ui.sessions_pane, 8, 6, false, 2);

    assert!(handled);
    assert_eq!(state.focused_pane, FocusedPane::Sessions);
    assert_eq!(state.selected_session_index, Some(1));
}

#[test]
fn sessions_mouse_wheel_over_preview_preserves_log_scroll_path() {
    let _guard = HOME_LOCK.lock().expect("home env lock");
    let (_home, mut state, ui) = state_with_sessions(5);
    state.focused_pane = FocusedPane::Sessions;

    let handled = state.scroll_session_list_by_mouse(&ui.sessions_pane, 50, 6, true, 3);

    assert!(!handled);
    assert_eq!(state.focused_pane, FocusedPane::LiveLogs);
    assert_eq!(state.selected_session_index, Some(0));
    assert!(state.pending_async_action.is_none());
}
