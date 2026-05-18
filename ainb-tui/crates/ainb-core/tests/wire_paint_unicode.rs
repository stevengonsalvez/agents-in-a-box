use ainb::ui::wire_paint::paint_wire_buffer;
use ainb_plugin_protocol::wire_buffer::{Cell, Coord, WireBuffer};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

#[test]
fn ascii_grapheme_paints_one_cell() {
    let mut wire = WireBuffer::new(3, 1);
    wire.push(Coord::new(0, 0), Cell::new("a"));
    wire.push(Coord::new(1, 0), Cell::new("b"));
    let mut target = Buffer::empty(Rect::new(0, 0, 3, 1));
    paint_wire_buffer(&mut target, &wire, Rect::new(0, 0, 3, 1));
    assert_eq!(target.get(0, 0).symbol(), "a");
    assert_eq!(target.get(1, 0).symbol(), "b");
}

#[test]
fn multi_byte_grapheme_paints_into_the_cell() {
    // Multi-byte single-width grapheme (é = U+00E9, single column).
    let mut wire = WireBuffer::new(2, 1);
    wire.push(Coord::new(0, 0), Cell::new("é"));
    wire.push(Coord::new(1, 0), Cell::new("X"));
    let mut target = Buffer::empty(Rect::new(0, 0, 2, 1));
    paint_wire_buffer(&mut target, &wire, Rect::new(0, 0, 2, 1));
    assert_eq!(target.get(0, 0).symbol(), "é");
    assert_eq!(target.get(1, 0).symbol(), "X");
}

#[test]
fn wide_grapheme_paint_does_not_panic_or_overflow() {
    // Emoji like 🚀 is width 2 in most terminals. The host paint helper must
    // not panic, and adjacent cells must remain accessible.
    let mut wire = WireBuffer::new(3, 1);
    wire.push(Coord::new(0, 0), Cell::new("🚀"));
    wire.push(Coord::new(1, 0), Cell::new("X"));
    wire.push(Coord::new(2, 0), Cell::new("Y"));
    let mut target = Buffer::empty(Rect::new(0, 0, 3, 1));
    paint_wire_buffer(&mut target, &wire, Rect::new(0, 0, 3, 1));
    assert_eq!(target.get(0, 0).symbol(), "🚀");
    // we only assert no panic and no out-of-bounds for cells 1 & 2
    let _ = target.get(1, 0).symbol();
    let _ = target.get(2, 0).symbol();
}
