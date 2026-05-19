// paint helper is the inverse of capture: render a paragraph into one buffer,
// scrape its cells into a WireBuffer, paint into a second buffer via the
// helper, assert the two buffers are cell-for-cell equal.

use ainb::ui::wire_paint::paint_wire_buffer;
use ainb_plugin_protocol::wire_buffer::{Cell as WireCell, Color as WireColor, Coord, WireBuffer};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, Borders, Paragraph, Widget};

const MOD_BOLD: u16 = 1;
const MOD_DIM: u16 = 2;
const MOD_ITALIC: u16 = 4;
const MOD_UNDERLINED: u16 = 8;
const MOD_REVERSED: u16 = 16;

fn ratatui_color_to_wire(c: Color) -> Option<WireColor> {
    match c {
        Color::Rgb(r, g, b) => Some(WireColor::rgb(r, g, b)),
        Color::Reset => None,
        other => panic!("test fixture only emits Rgb / Reset, got {other:?}"),
    }
}

fn ratatui_modifier_to_wire(m: Modifier) -> u16 {
    let mut b = 0u16;
    if m.contains(Modifier::BOLD) {
        b |= MOD_BOLD;
    }
    if m.contains(Modifier::DIM) {
        b |= MOD_DIM;
    }
    if m.contains(Modifier::ITALIC) {
        b |= MOD_ITALIC;
    }
    if m.contains(Modifier::UNDERLINED) {
        b |= MOD_UNDERLINED;
    }
    if m.contains(Modifier::REVERSED) {
        b |= MOD_REVERSED;
    }
    b
}

fn capture_buffer_into_wire(src: &Buffer, area: Rect) -> WireBuffer {
    let mut w = WireBuffer::new(area.width, area.height);
    for y in 0..area.height {
        for x in 0..area.width {
            let cell = src.get(area.x + x, area.y + y);
            w.push(
                Coord::new(x, y),
                WireCell {
                    symbol: cell.symbol().to_string(),
                    fg: ratatui_color_to_wire(cell.fg),
                    bg: ratatui_color_to_wire(cell.bg),
                    modifier: ratatui_modifier_to_wire(cell.modifier),
                },
            );
        }
    }
    w
}

#[test]
fn paint_is_inverse_of_capture_for_paragraph() {
    let area = Rect::new(0, 0, 30, 5);

    let mut buf_a = Buffer::empty(area);
    let block = Block::default()
        .borders(Borders::ALL)
        .style(Style::default().fg(Color::Rgb(100, 149, 237)));
    let paragraph = Paragraph::new("hello world")
        .style(Style::default().fg(Color::Rgb(255, 215, 0)).add_modifier(Modifier::BOLD))
        .block(block);
    paragraph.render(area, &mut buf_a);

    let wire = capture_buffer_into_wire(&buf_a, area);

    let mut buf_b = Buffer::empty(area);
    paint_wire_buffer(&mut buf_b, &wire, area);

    for y in 0..area.height {
        for x in 0..area.width {
            let a = buf_a.get(x, y);
            let b = buf_b.get(x, y);
            assert_eq!(a.symbol(), b.symbol(), "symbol differs at ({x},{y})");
            assert_eq!(a.fg, b.fg, "fg differs at ({x},{y})");
            assert_eq!(a.bg, b.bg, "bg differs at ({x},{y})");
            assert_eq!(a.modifier, b.modifier, "modifier differs at ({x},{y})");
        }
    }
}
