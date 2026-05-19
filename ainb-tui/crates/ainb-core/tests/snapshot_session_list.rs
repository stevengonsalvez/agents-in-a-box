// Snapshot test for the session_list component at min (30×10) and large
// (200×60) viewports. Drives P2.session_list acceptance per
// .agents/plans/bsp-tiling.md §P2.

use ainb::app::AppState;
use ainb::components::session_list::SessionListComponent;
use ainb::models::{Session, Workspace};
use insta::assert_snapshot;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::layout::Rect;
use std::path::PathBuf;

fn build_state(n_sessions: usize) -> AppState {
    let mut state = AppState::default();
    let mut ws = Workspace::new("project".to_string(), PathBuf::from("/project"));
    for i in 0..n_sessions {
        let session = Session::new(
            format!("session-{i:02}"),
            ws.path.to_string_lossy().to_string(),
        );
        ws.add_session(session);
    }
    state.workspaces.push(ws);
    state.selected_workspace_index = Some(0);
    state.selected_session_index = Some(0);
    state
}

fn render_to_string(w: u16, h: u16, state: &AppState) -> String {
    let backend = TestBackend::new(w, h);
    let mut terminal = Terminal::new(backend).expect("Terminal::new");
    let mut comp = SessionListComponent::default();
    terminal
        .draw(|f| {
            let area = Rect::new(0, 0, w, h);
            comp.render(f, area, state);
        })
        .expect("draw");
    buffer_to_string(terminal.backend().buffer())
}

fn buffer_to_string(buf: &ratatui::buffer::Buffer) -> String {
    let area = buf.area;
    let mut out = String::new();
    for y in 0..area.height {
        for x in 0..area.width {
            out.push_str(buf.get(area.x + x, area.y + y).symbol());
        }
        out.push('\n');
    }
    out
}

#[test]
fn session_list_renders_at_min_30x10() {
    let state = build_state(3);
    let rendered = render_to_string(30, 10, &state);
    assert_snapshot!(rendered);
}

#[test]
fn session_list_renders_at_large_200x60() {
    let state = build_state(20);
    let rendered = render_to_string(200, 60, &state);
    assert_snapshot!(rendered);
}
