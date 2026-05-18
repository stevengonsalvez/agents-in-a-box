// Snapshot test for claude_chat at min (30×10) and large (200×60).
// Renders against default AppState (no chat content).

use ainb::app::AppState;
use ainb::components::claude_chat::ClaudeChatComponent;
use insta::assert_snapshot;
use ratatui::backend::TestBackend;
use ratatui::layout::Rect;
use ratatui::Terminal;

fn render_to_string(w: u16, h: u16) -> String {
    let backend = TestBackend::new(w, h);
    let mut terminal = Terminal::new(backend).expect("Terminal::new");
    let mut comp = ClaudeChatComponent::new();
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
fn claude_chat_renders_empty_at_min_30x10() {
    let rendered = render_to_string(30, 10);
    assert_snapshot!(rendered);
}

#[test]
fn claude_chat_renders_empty_at_large_200x60() {
    let rendered = render_to_string(200, 60);
    assert_snapshot!(rendered);
}
