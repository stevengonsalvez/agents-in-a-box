// Snapshot test for live_logs_stream at min (30×10) and large (200×60).
// Tests the empty-state render path (component shows the placeholder message
// when no logs are present). Logged-state snapshots require a test-friendly
// constructor that disables relative-time formatting; that lives in a future
// polish bead.

use ainb::app::AppState;
use ainb::components::live_logs_stream::LiveLogsStreamComponent;
use insta::assert_snapshot;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::layout::Rect;

fn render_to_string(w: u16, h: u16, state: &AppState) -> String {
    let backend = TestBackend::new(w, h);
    let mut terminal = Terminal::new(backend).expect("Terminal::new");
    let mut comp = LiveLogsStreamComponent::new();
    terminal
        .draw(|f| {
            let area = Rect::new(0, 0, w, h);
            comp.render(f, area, state);
        })
        .expect("draw");
    let buf = terminal.backend().buffer();
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
fn live_logs_stream_renders_empty_at_min_30x10() {
    let state = AppState::default();
    let rendered = render_to_string(30, 10, &state);
    assert_snapshot!(rendered);
}

#[test]
fn live_logs_stream_renders_empty_at_large_200x60() {
    let state = AppState::default();
    let rendered = render_to_string(200, 60, &state);
    assert_snapshot!(rendered);
}
