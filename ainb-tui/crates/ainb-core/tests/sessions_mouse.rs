//! Sessions screen mouse behavior regression tests.

use ainb::app::mouse::{Gesture, gesture};
use ainb::app::screens::ids as screen_ids;
use ainb::app::state::FocusedPane;
use ainb::app::ui_state::UiState;
use ainb::app::{AppState, Btn, Effect, Intent, Keymap, Pos, TerminalTarget, dispatch};
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
    state.shell.current_screen = screen_ids::SESSION_LIST.to_string();
    state.sessions.selected_workspace_index = Some(0);
    state.sessions.selected_session_index = Some(0);

    let mut workspace = Workspace::new("repo".to_string(), "/tmp/repo".into());
    for index in 0..count {
        workspace.add_session(Session::new(
            format!("session-{}", index + 1),
            "/tmp/repo".to_string(),
        ));
    }
    state.sessions.workspaces = vec![workspace];

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

/// Left-click at (`x`, `y`) through dispatch, with `ui` as the host that
/// hit-tests it, exactly as the run loop does.
fn click(state: &mut AppState, ui: &mut UiState, x: u16, y: u16) -> Vec<Effect> {
    let press = Intent::Mouse(Pos { x, y }, Btn::Left);
    written_by_host(dispatch(state, &Keymap::defaults(), ui, press))
}

/// Write the stores `effects` persist, as the host does after the step, and
/// hand back the rest.
fn written_by_host(effects: Vec<Effect>) -> Vec<Effect> {
    effects
        .into_iter()
        .filter(|effect| {
            let Effect::Persist(store) = effect else {
                return true;
            };
            ainb::config::persist::write(store).expect("the host writes the store");
            false
        })
        .collect()
}

/// A drag, release or hover, then whatever intent it finished, with the
/// stores it changed written as the host writes them after the step.
fn finish_gesture(kind: Gesture, state: &mut AppState, ui: &mut UiState, x: u16, y: u16) {
    if let Some(intent) = gesture(kind, Pos { x, y }, &*state, ui) {
        let _ = written_by_host(dispatch(state, &Keymap::defaults(), ui, intent));
    }
}

#[test]
fn sessions_mouse_click_selects_session_row_without_async_work() {
    let _guard = HOME_LOCK.lock().expect("home env lock");
    let (_home, mut state, mut ui) = state_with_two_sessions();

    let second = session_row_y(&ui, 1);
    let effects = click(&mut state, &mut ui, 8, second);

    assert!(effects.is_empty());
    assert_eq!(state.sessions.selected_workspace_index, Some(0));
    assert_eq!(state.sessions.selected_session_index, Some(1));
    assert!(state.shell.pending_async_action.is_some());
}

#[test]
fn sessions_mouse_double_click_attaches_selected_session_row() {
    let _guard = HOME_LOCK.lock().expect("home env lock");
    let (_home, mut state, mut ui) = state_with_two_sessions();
    // The reducer attaches only a session that has a tmux session.
    state.sessions.workspaces[0].sessions[1].tmux_session_name = Some("tmux_second".to_string());

    let row = session_row_y(&ui, 1);
    let first = click(&mut state, &mut ui, 8, row);
    let second = click(&mut state, &mut ui, 8, row);

    let session_id = state.sessions.workspaces[0].sessions[1].id;
    assert!(first.is_empty());
    assert_eq!(
        second,
        vec![Effect::AttachTerminal(TerminalTarget::Session {
            id: session_id,
            tmux_session: ainb::app::TmuxSessionName::new("tmux_second").expect("valid name"),
        })]
    );
    assert_eq!(state.sessions.selected_workspace_index, Some(0));
    assert_eq!(state.sessions.selected_session_index, Some(1));
}

#[test]
fn sessions_mouse_double_click_requires_same_attachable_row() {
    let _guard = HOME_LOCK.lock().expect("home env lock");
    let (_home, mut state, mut ui) = state_with_two_sessions();

    let first_row = session_row_y(&ui, 0);
    let second_row = session_row_y(&ui, 1);
    let first = click(&mut state, &mut ui, 8, first_row);
    let second = click(&mut state, &mut ui, 8, second_row);

    assert!(first.is_empty());
    assert!(second.is_empty());
    assert_eq!(state.sessions.selected_workspace_index, Some(0));
    assert_eq!(state.sessions.selected_session_index, Some(1));
}

#[test]
fn sessions_mouse_drag_resizes_and_persists_on_release_only() {
    let _guard = HOME_LOCK.lock().expect("home env lock");
    let (home, mut state, mut ui) = state_with_two_sessions();

    click(&mut state, &mut ui, 39, 8);
    finish_gesture(Gesture::Drag, &mut state, &mut ui, 55, 8);

    assert_eq!(ui.sessions_pane.expanded_width(120), 56);
    let config_path = home.path().join(".agents-in-a-box/config/config.toml");
    assert!(
        !config_path.exists(),
        "drag hot path should not persist config before mouse release"
    );

    finish_gesture(Gesture::Release, &mut state, &mut ui, 55, 8);

    let config = std::fs::read_to_string(config_path).expect("persisted config");
    let row = ui.sessions_pane.last_content_width().expect("drawn row");
    let fraction = f64::from(56u16) / f64::from(row);
    assert!(
        config.contains(&format!("sessions_sidebar_fraction = {fraction}")),
        "{config}"
    );
    assert!(!config.contains("sessions_sidebar_width"), "{config}");
}

#[test]
fn sessions_mouse_toggle_collapses_and_expands_sidebar() {
    let _guard = HOME_LOCK.lock().expect("home env lock");
    let (home, mut state, mut ui) = state_with_two_sessions();

    click(&mut state, &mut ui, 2, 3);
    assert!(ui.sessions_pane.collapsed);
    assert_eq!(ui.sessions_pane.effective_width(120), 5);

    ui.sessions_pane.set_layout(Rect::new(0, 3, 5, 20), Rect::new(5, 3, 115, 20));
    click(&mut state, &mut ui, 2, 4);

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
    assert_eq!(state.shell.focused_pane, FocusedPane::Sessions);
    assert_eq!(state.sessions.selected_session_index, Some(3));
    assert!(state.shell.pending_async_action.is_some());
}

#[test]
fn sessions_mouse_wheel_up_over_sessions_moves_selection() {
    let _guard = HOME_LOCK.lock().expect("home env lock");
    let (_home, mut state, ui) = state_with_sessions(5);
    state.sessions.selected_session_index = Some(3);

    let handled = state.scroll_session_list_by_mouse(&ui.sessions_pane, 8, 6, false, 2);

    assert!(handled);
    assert_eq!(state.shell.focused_pane, FocusedPane::Sessions);
    assert_eq!(state.sessions.selected_session_index, Some(1));
}

#[test]
fn sessions_mouse_wheel_over_preview_preserves_log_scroll_path() {
    let _guard = HOME_LOCK.lock().expect("home env lock");
    let (_home, mut state, ui) = state_with_sessions(5);
    state.shell.focused_pane = FocusedPane::Sessions;

    let handled = state.scroll_session_list_by_mouse(&ui.sessions_pane, 50, 6, true, 3);

    assert!(!handled);
    assert_eq!(state.shell.focused_pane, FocusedPane::LiveLogs);
    assert_eq!(state.sessions.selected_session_index, Some(0));
    assert!(state.shell.pending_async_action.is_none());
}
