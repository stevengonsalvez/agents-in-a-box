//! Pure-function helper for painting a plugin's [`WireBuffer`] into a
//! ratatui [`Buffer`] at a given [`Rect`].
//!
//! The helper is *the* host-side compositing primitive: subprocess plugins
//! emit a `WireBuffer`, the host calls [`paint_wire_buffer`] for each
//! visible tile, and the buffer ends up cell-correct on screen.
//!
//! Cells outside the wire viewport (`coord.x >= wire.width || coord.y >=
//! wire.height`) or outside the destination `area` are silently clipped —
//! the helper never panics on coordinate math.
//!
//! Modifier-bit semantics match `app::screens::builtin::modifier_bits_to_modifiers`
//! (bit 0 = BOLD, 1 = DIM, 2 = ITALIC, 3 = UNDERLINED, 4 = REVERSED). The
//! mapping is duplicated here intentionally; future P4 work will refactor
//! `PluginScreen::render` to call this helper and the duplicate goes away.

use ainb_plugin_protocol::wire_buffer::{Cell, Color as WireColor, WireBuffer};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier};

const MOD_BOLD: u16 = 1;
const MOD_DIM: u16 = 2;
const MOD_ITALIC: u16 = 4;
const MOD_UNDERLINED: u16 = 8;
const MOD_REVERSED: u16 = 16;

/// Paint every cell of `wire` into `target` at the offset given by `area`.
///
/// Cells whose `(coord.x, coord.y)` falls outside either the wire viewport
/// (`wire.width × wire.height`) or `area` are dropped silently.
pub fn paint_wire_buffer(target: &mut Buffer, wire: &WireBuffer, area: Rect) {
    for (coord, cell) in &wire.cells {
        if coord.x >= wire.width || coord.y >= wire.height {
            continue;
        }
        if coord.x >= area.width || coord.y >= area.height {
            continue;
        }
        let tx = area.x.saturating_add(coord.x);
        let ty = area.y.saturating_add(coord.y);
        let dst = target.get_mut(tx, ty);
        dst.set_symbol(&cell.symbol);
        dst.set_fg(rgb_to_color(cell.fg));
        dst.set_bg(rgb_to_color(cell.bg));
        dst.set_style(
            ratatui::style::Style::default()
                .add_modifier(modifier_bits_to_modifiers(cell.modifier)),
        );
    }
}

/// Translate a wire RGB triple (or `None`) into a ratatui [`Color`].
pub fn rgb_to_color(c: Option<WireColor>) -> Color {
    match c {
        None => Color::Reset,
        Some(rgb) => Color::Rgb(rgb.r, rgb.g, rgb.b),
    }
}

/// Translate a wire-style modifier bitfield into ratatui's [`Modifier`] set.
pub fn modifier_bits_to_modifiers(b: u16) -> Modifier {
    let mut m = Modifier::empty();
    if b & MOD_BOLD != 0 {
        m |= Modifier::BOLD;
    }
    if b & MOD_DIM != 0 {
        m |= Modifier::DIM;
    }
    if b & MOD_ITALIC != 0 {
        m |= Modifier::ITALIC;
    }
    if b & MOD_UNDERLINED != 0 {
        m |= Modifier::UNDERLINED;
    }
    if b & MOD_REVERSED != 0 {
        m |= Modifier::REVERSED;
    }
    m
}

#[allow(dead_code)]
fn cell_to_style(cell: &Cell) -> ratatui::style::Style {
    ratatui::style::Style::default()
        .fg(rgb_to_color(cell.fg))
        .bg(rgb_to_color(cell.bg))
        .add_modifier(modifier_bits_to_modifiers(cell.modifier))
}
