//! The Phase 3 scroll seal: renderer-local state is applied by the ratatui
//! host, and `AppState` no longer carries any of it.

#![allow(missing_docs)]

use ainb::app::keymap::UiAction;
use ainb::app::state::AppState;
use ainb::app::ui_state::UiState;
use ainb::components::LayoutComponent;
use ainb::components::session_list::SessionListComponent;
use ainb::models::{Session, Workspace};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::layout::Rect;

/// The sessions panel rect the hit-test fixture pins, mirrored by the render
/// below so painting and hit-testing agree on the same geometry.
const SESSIONS_RECT: Rect = Rect {
    x: 0,
    y: 3,
    width: 40,
    height: 20,
};

fn apply_all(ui: &mut UiState, layout: &mut LayoutComponent, actions: &[UiAction]) {
    let state = AppState::new();
    for action in actions {
        ui.apply(*action, layout, &state);
    }
}

#[test]
fn logs_scroll_actions_walk_the_offset_and_flip_auto_scroll() {
    let mut ui = UiState::default();
    let mut layout = LayoutComponent::new();

    // A fresh pane sits at the bottom with auto-scroll armed.
    assert_eq!(layout.live_logs_mut().scroll_offset(), 0);
    assert!(layout.live_logs_mut().auto_scroll());

    // Three downs walk the offset out; scrolling by hand disarms auto-scroll.
    apply_all(
        &mut ui,
        &mut layout,
        &[
            UiAction::ScrollLogsDown,
            UiAction::ScrollLogsDown,
            UiAction::ScrollLogsDown,
        ],
    );
    assert_eq!(layout.live_logs_mut().scroll_offset(), 3);
    assert!(!layout.live_logs_mut().auto_scroll());

    // One up walks it back, and the floor holds at zero rather than wrapping.
    apply_all(
        &mut ui,
        &mut layout,
        &[UiAction::ScrollLogsUp, UiAction::ScrollLogsUp],
    );
    assert_eq!(layout.live_logs_mut().scroll_offset(), 1);
    apply_all(&mut ui, &mut layout, &[UiAction::ScrollLogsUp; 4]);
    assert_eq!(layout.live_logs_mut().scroll_offset(), 0);

    // `end` re-arms auto-scroll; `home` disarms it again.
    apply_all(&mut ui, &mut layout, &[UiAction::ScrollLogsToBottom]);
    assert!(layout.live_logs_mut().auto_scroll());
    apply_all(&mut ui, &mut layout, &[UiAction::ScrollLogsToTop]);
    assert_eq!(layout.live_logs_mut().scroll_offset(), 0);
    assert!(!layout.live_logs_mut().auto_scroll());

    // `space` is the explicit toggle.
    apply_all(&mut ui, &mut layout, &[UiAction::ToggleAutoScroll]);
    assert!(layout.live_logs_mut().auto_scroll());

    assert!(
        ui.needs_redraw,
        "a scroll the user can see must ask for a repaint"
    );
}

/// Shift+arrow enters preview scroll mode as part of the same keypress; the
/// in-mode keys move without re-entering, and `esc` leaves.
#[test]
fn preview_scroll_actions_enter_and_exit_scroll_mode() {
    let mut ui = UiState::default();
    let mut layout = LayoutComponent::new();

    assert!(!layout.tmux_preview_mut().is_scroll_mode());

    apply_all(&mut ui, &mut layout, &[UiAction::ScrollPreviewUp]);
    assert!(
        layout.tmux_preview_mut().is_scroll_mode(),
        "shift+up must arm scroll mode on the same keypress that moves"
    );

    apply_all(
        &mut ui,
        &mut layout,
        &[
            UiAction::PreviewScrollUp,
            UiAction::PreviewPageUp,
            UiAction::PreviewScrollDown,
            UiAction::PreviewPageDown,
        ],
    );
    assert!(
        layout.tmux_preview_mut().is_scroll_mode(),
        "the in-mode keys must not drop out of scroll mode"
    );

    apply_all(&mut ui, &mut layout, &[UiAction::PreviewExitScroll]);
    assert!(!layout.tmux_preview_mut().is_scroll_mode());
}

/// Reducer intents routed through the same enum must not touch the layout.
#[test]
fn a_non_scroll_ui_action_leaves_the_layout_alone() {
    let mut ui = UiState::default();
    let mut layout = LayoutComponent::new();

    apply_all(&mut ui, &mut layout, &[UiAction::ScrollLogsDown]);
    ui.needs_redraw = false;

    apply_all(&mut ui, &mut layout, &[UiAction::SessionStartRename]);

    assert_eq!(layout.live_logs_mut().scroll_offset(), 1);
    assert!(!ui.needs_redraw, "a reducer intent is not a repaint reason");
}

/// The hit test resolves a click through the row heights the RENDER recorded,
/// because a session row is two terminal lines and a header is one. Painting
/// the real component is the only way to get a map that matches what the user
/// clicked on.
#[test]
fn mouse_hit_test_resolves_a_click_through_the_painted_row_map() {
    let mut state = AppState::new();
    state.selected_workspace_index = Some(0);
    state.selected_session_index = Some(0);

    let mut workspace = Workspace::new("repo".to_string(), "/tmp/repo".into());
    for index in 0..3 {
        workspace.add_session(Session::new(
            format!("session-{}", index + 1),
            "/tmp/repo".to_string(),
        ));
    }
    state.workspaces = vec![workspace];

    let mut ui = UiState::default();
    ui.sessions_pane.set_layout(SESSIONS_RECT, Rect::new(40, 3, 80, 20));

    let mut terminal = Terminal::new(TestBackend::new(120, 30)).expect("test terminal");
    let mut list = SessionListComponent::new();
    terminal
        .draw(|frame| list.render(frame, SESSIONS_RECT, &state, &mut ui))
        .expect("draw sessions panel");

    // Row 0 is the workspace header (one line); every session below it is two.
    assert_eq!(
        ui.sessions_pane.row_index_at(8, SESSIONS_RECT.y + 1),
        Some(0)
    );
    assert_eq!(
        ui.sessions_pane.row_index_at(8, SESSIONS_RECT.y + 2),
        Some(1)
    );
    assert_eq!(
        ui.sessions_pane.row_index_at(8, SESSIONS_RECT.y + 3),
        Some(1)
    );
    assert_eq!(
        ui.sessions_pane.row_index_at(8, SESSIONS_RECT.y + 4),
        Some(2)
    );

    // A point outside the pane is nobody's row.
    assert_eq!(ui.sessions_pane.row_index_at(80, SESSIONS_RECT.y + 2), None);
    assert_eq!(ui.sessions_pane.row_index_at(8, SESSIONS_RECT.y), None);

    // Both panes report what they contain, so focus routing has a fixture too.
    assert!(ui.sessions_pane.contains_sessions_point(8, 10));
    assert!(!ui.sessions_pane.contains_preview_point(8, 10));
    assert!(ui.sessions_pane.contains_preview_point(80, 10));

    // The row index maps onto the session the user aimed at, not the header.
    let target = state
        .session_list_row_at_mouse(&ui.sessions_pane, 8, SESSIONS_RECT.y + 3)
        .expect("a session row");
    state.select_session_list_row(target);
    assert_eq!(state.selected_session_index, Some(0));
}

/// The seal itself: no `Rect` survives in `AppState`. A geometry field there is
/// a renderer's measurement leaking into state every other surface shares.
#[test]
fn app_state_carries_no_terminal_geometry() {
    const STATE_SOURCE: &str = include_str!("../src/app/state.rs");

    let offenders: Vec<&str> = STATE_SOURCE
        .lines()
        .filter(|line| line.contains("ratatui::layout::Rect") || line.contains("Rect>"))
        .collect();

    assert!(
        offenders.is_empty(),
        "AppState must hold no terminal geometry, found: {offenders:#?}"
    );
}
