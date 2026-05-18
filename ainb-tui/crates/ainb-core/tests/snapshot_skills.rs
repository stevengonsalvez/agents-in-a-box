// Snapshot test for skills view at min (30×10) and large (200×60).

use ainb::components::skills::{render as render_skills, SkillsViewState};
use insta::assert_snapshot;
use ratatui::backend::TestBackend;
use ratatui::layout::Rect;
use ratatui::Terminal;

fn render_to_string(w: u16, h: u16) -> String {
    let backend = TestBackend::new(w, h);
    let mut terminal = Terminal::new(backend).expect("Terminal::new");
    let state = SkillsViewState::default();
    terminal
        .draw(|f| {
            let area = Rect::new(0, 0, w, h);
            render_skills(f, area, &state);
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
fn skills_renders_at_min_30x10() {
    let rendered = render_to_string(30, 10);
    assert_snapshot!(rendered);
}

#[test]
fn skills_renders_at_large_200x60() {
    let rendered = render_to_string(200, 60);
    assert_snapshot!(rendered);
}
