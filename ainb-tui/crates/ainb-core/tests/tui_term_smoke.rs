// ABOUTME: Phase-0 smoke test — proves tui-term 0.3.4 + vt100 0.16 render into a
// ratatui 0.30 TestBackend buffer. Guards the dependency-upgrade integration point
// before the embed (Phase 2) builds the real PseudoTerminal render path on it.
//
// pure (no tmux) — vt100 parser + ratatui TestBackend only.

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use tui_term::widget::PseudoTerminal;

/// Render a vt100 screen through tui-term's PseudoTerminal into a TestBackend and
/// assert the parsed glyphs land in the rendered buffer. If this compiles and passes,
/// vt100 0.16 ⇄ tui-term 0.3.4 ⇄ ratatui 0.30 are wired correctly.
#[test]
fn tui_term_renders_vt100_screen_into_ratatui_buffer() {
    let cols: u16 = 20;
    let rows: u16 = 3;

    // Feed plain text to a vt100 parser (the same engine the embed will drive).
    let mut parser = vt100::Parser::new(rows, cols, 0);
    parser.process(b"HELLO");

    let backend = TestBackend::new(cols, rows);
    let mut terminal = Terminal::new(backend).expect("test terminal");

    terminal
        .draw(|frame| {
            let screen = parser.screen();
            let widget = PseudoTerminal::new(screen);
            frame.render_widget(widget, frame.area());
        })
        .expect("draw");

    // Flatten the rendered buffer to a string and assert the glyphs are present.
    let buffer = terminal.backend().buffer();
    let rendered: String = buffer.content().iter().map(|c| c.symbol()).collect();

    assert!(
        rendered.contains("HELLO"),
        "tui-term did not render the vt100 screen into the ratatui buffer; got:\n{rendered}"
    );
}
