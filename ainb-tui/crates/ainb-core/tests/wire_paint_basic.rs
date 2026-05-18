use ainb::ui::wire_paint::paint_wire_buffer;
use ainb_plugin_protocol::wire_buffer::{Cell, Coord, WireBuffer};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

fn make_test_buffer(w: u16, h: u16) -> Buffer {
    Buffer::empty(Rect::new(0, 0, w, h))
}

#[test]
fn paints_symbols_at_target_rect_coordinates() {
    let mut wire = WireBuffer::new(5, 3);
    wire.push(Coord::new(0, 0), Cell::new("X"));
    wire.push(Coord::new(1, 0), Cell::new("Y"));
    wire.push(Coord::new(0, 1), Cell::new("Z"));

    let mut target = make_test_buffer(20, 10);
    let tile = Rect::new(10, 5, 5, 3);
    paint_wire_buffer(&mut target, &wire, tile);

    assert_eq!(target.get(10, 5).symbol(), "X");
    assert_eq!(target.get(11, 5).symbol(), "Y");
    assert_eq!(target.get(10, 6).symbol(), "Z");
}

#[test]
fn paints_into_tile_with_zero_offset() {
    let mut wire = WireBuffer::new(3, 1);
    wire.push(Coord::new(0, 0), Cell::new("A"));
    wire.push(Coord::new(1, 0), Cell::new("B"));
    wire.push(Coord::new(2, 0), Cell::new("C"));

    let mut target = make_test_buffer(5, 1);
    paint_wire_buffer(&mut target, &wire, Rect::new(0, 0, 5, 1));

    assert_eq!(target.get(0, 0).symbol(), "A");
    assert_eq!(target.get(1, 0).symbol(), "B");
    assert_eq!(target.get(2, 0).symbol(), "C");
}

#[test]
fn empty_buffer_leaves_target_untouched() {
    let wire = WireBuffer::new(5, 3);
    let mut target = make_test_buffer(20, 10);
    // pre-paint a cell into target
    target.get_mut(0, 0).set_symbol("Q");
    paint_wire_buffer(&mut target, &wire, Rect::new(10, 5, 5, 3));
    // target's existing cell should be untouched
    assert_eq!(target.get(0, 0).symbol(), "Q");
}
