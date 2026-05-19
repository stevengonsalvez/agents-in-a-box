// Snapshot test for attached_terminal at min (30×10) and large (200×60).
// Renders against default AppState (no terminal attached).

use ainb::app::AppState;
use ainb::components::attached_terminal::AttachedTerminalComponent;
use insta::assert_snapshot;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::layout::Rect;

fn render_to_string(w: u16, h: u16) -> String {
    let backend = TestBackend::new(w, h);
    let mut terminal = Terminal::new(backend).expect("Terminal::new");
    let comp = AttachedTerminalComponent::new();
    let state = AppState::default();
    terminal
        .draw(|f| {
            let area = Rect::new(0, 0, w, h);
            comp.render(f, area, &state);
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
fn attached_terminal_renders_empty_at_min_30x10() {
    let rendered = render_to_string(30, 10);
    assert_snapshot!(rendered);
}

#[test]
fn attached_terminal_renders_empty_at_large_200x60() {
    let rendered = render_to_string(200, 60);
    assert_snapshot!(rendered);
}
