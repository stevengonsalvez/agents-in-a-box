use ainb::ui::wire_paint::paint_wire_buffer;
use ainb_plugin_protocol::wire_buffer::{Cell, Color, Coord, WireBuffer};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color as RatColor, Modifier};

const MOD_BOLD: u16 = 1;
const MOD_DIM: u16 = 2;
const MOD_ITALIC: u16 = 4;
const MOD_UNDERLINED: u16 = 8;
const MOD_REVERSED: u16 = 16;

#[test]
fn cell_fg_maps_to_rgb_color() {
    let mut wire = WireBuffer::new(1, 1);
    wire.push(
        Coord::new(0, 0),
        Cell {
            symbol: "•".into(),
            fg: Some(Color::rgb(255, 100, 50)),
            bg: None,
            modifier: 0,
        },
    );
    let mut target = Buffer::empty(Rect::new(0, 0, 1, 1));
    paint_wire_buffer(&mut target, &wire, Rect::new(0, 0, 1, 1));
    assert_eq!(target.get(0, 0).fg, RatColor::Rgb(255, 100, 50));
}

#[test]
fn cell_bg_maps_to_rgb_color() {
    let mut wire = WireBuffer::new(1, 1);
    wire.push(
        Coord::new(0, 0),
        Cell {
            symbol: " ".into(),
            fg: None,
            bg: Some(Color::rgb(30, 30, 40)),
            modifier: 0,
        },
    );
    let mut target = Buffer::empty(Rect::new(0, 0, 1, 1));
    paint_wire_buffer(&mut target, &wire, Rect::new(0, 0, 1, 1));
    assert_eq!(target.get(0, 0).bg, RatColor::Rgb(30, 30, 40));
}

#[test]
fn cell_no_color_defaults_to_reset() {
    let mut wire = WireBuffer::new(1, 1);
    wire.push(Coord::new(0, 0), Cell::new("X"));
    let mut target = Buffer::empty(Rect::new(0, 0, 1, 1));
    paint_wire_buffer(&mut target, &wire, Rect::new(0, 0, 1, 1));
    assert_eq!(target.get(0, 0).fg, RatColor::Reset);
    assert_eq!(target.get(0, 0).bg, RatColor::Reset);
}

#[test]
fn modifier_bits_map_to_ratatui_modifiers() {
    let pairs: &[(u16, Modifier)] = &[
        (MOD_BOLD, Modifier::BOLD),
        (MOD_DIM, Modifier::DIM),
        (MOD_ITALIC, Modifier::ITALIC),
        (MOD_UNDERLINED, Modifier::UNDERLINED),
        (MOD_REVERSED, Modifier::REVERSED),
    ];
    for &(bit, expected) in pairs {
        let mut wire = WireBuffer::new(1, 1);
        wire.push(
            Coord::new(0, 0),
            Cell {
                symbol: "X".into(),
                fg: None,
                bg: None,
                modifier: bit,
            },
        );
        let mut target = Buffer::empty(Rect::new(0, 0, 1, 1));
        paint_wire_buffer(&mut target, &wire, Rect::new(0, 0, 1, 1));
        assert!(
            target.get(0, 0).modifier.contains(expected),
            "bit={bit:#b} expected {expected:?}, got {:?}",
            target.get(0, 0).modifier
        );
    }
}

#[test]
fn combined_modifier_bits_or_together() {
    let combined = MOD_BOLD | MOD_ITALIC | MOD_UNDERLINED;
    let mut wire = WireBuffer::new(1, 1);
    wire.push(
        Coord::new(0, 0),
        Cell {
            symbol: "T".into(),
            fg: None,
            bg: None,
            modifier: combined,
        },
    );
    let mut target = Buffer::empty(Rect::new(0, 0, 1, 1));
    paint_wire_buffer(&mut target, &wire, Rect::new(0, 0, 1, 1));
    let m = target.get(0, 0).modifier;
    assert!(m.contains(Modifier::BOLD));
    assert!(m.contains(Modifier::ITALIC));
    assert!(m.contains(Modifier::UNDERLINED));
}
