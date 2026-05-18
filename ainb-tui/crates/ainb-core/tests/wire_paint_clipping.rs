use ainb::ui::wire_paint::paint_wire_buffer;
use ainb_plugin_protocol::wire_buffer::{Cell, Coord, WireBuffer};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

#[test]
fn cells_outside_wire_viewport_are_clipped() {
    let mut wire = WireBuffer::new(5, 3);
    wire.push(Coord::new(0, 0), Cell::new("A"));
    wire.push(Coord::new(99, 99), Cell::new("Z"));
    wire.push(Coord::new(7, 1), Cell::new("Y"));

    let mut target = Buffer::empty(Rect::new(0, 0, 10, 5));
    paint_wire_buffer(&mut target, &wire, Rect::new(0, 0, 5, 3));

    assert_eq!(target.get(0, 0).symbol(), "A");
    // out-of-tile cells must not be painted anywhere inside target
    assert_ne!(target.get(9, 4).symbol(), "Z");
}

#[test]
fn cells_outside_tile_rect_are_clipped() {
    let mut wire = WireBuffer::new(10, 10);
    wire.push(Coord::new(6, 0), Cell::new("X")); // x=6 > tile width 5
    wire.push(Coord::new(0, 4), Cell::new("Y")); // y=4 > tile height 3
    wire.push(Coord::new(0, 0), Cell::new("A"));

    let mut target = Buffer::empty(Rect::new(0, 0, 20, 10));
    paint_wire_buffer(&mut target, &wire, Rect::new(2, 2, 5, 3));

    assert_eq!(target.get(2, 2).symbol(), "A");
    assert!(target.get(8, 2).symbol() != "X");
    assert!(target.get(2, 6).symbol() != "Y");
}

#[test]
fn paint_does_not_panic_on_oversized_wire_buffer() {
    let mut wire = WireBuffer::new(100, 100);
    for x in 0..100u16 {
        wire.push(Coord::new(x, 0), Cell::new("?"));
    }
    let mut target = Buffer::empty(Rect::new(0, 0, 10, 5));
    paint_wire_buffer(&mut target, &wire, Rect::new(0, 0, 5, 3));
    for x in 0..5u16 {
        assert_eq!(target.get(x, 0).symbol(), "?");
    }
}
