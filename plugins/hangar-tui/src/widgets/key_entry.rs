//! Key-entry modal widget — password-style masked input (P4.7).
//!
//! The settings screen's "add key" flow (`n` on the Keys section) overlays this
//! centred modal. By design it takes only the **length** of the in-flight value,
//! never the value itself, and paints that many `*` mask characters — so the
//! secret cannot leak through the render path any more than it can through a
//! `Debug` (the value lives in the `KeyMaterial` newtype, whose `Debug` redacts).
//!
//! Pure render — no state, no IO. Width-aware: the modal is clamped to fit the
//! 80×24 floor.

use ainb_plugin_sdk::{Cell, Color, Coord, WireBuffer};

/// Cornflower-blue modal border.
const BORDER: Color = Color::rgb(100, 149, 237);
/// Gold title.
const TITLE: Color = Color::rgb(255, 215, 0);
/// Soft-white mask + hint text.
const TEXT: Color = Color::rgb(220, 220, 230);
/// Muted hint.
const HINT: Color = Color::rgb(120, 120, 140);

/// Render the key-entry modal centred in an `area_w` × `area_h` area, painting
/// `masked_len` mask characters for the in-flight value (never the value).
pub fn render_key_entry_modal(buf: &mut WireBuffer, area_w: u16, area_h: u16, masked_len: usize) {
    let modal_w: u16 = 44.min(area_w);
    let modal_h: u16 = 6.min(area_h);
    let x0 = (area_w.saturating_sub(modal_w)) / 2;
    let y0 = (area_h.saturating_sub(modal_h)) / 2;
    let x1 = x0 + modal_w - 1;
    let y1 = y0 + modal_h - 1;

    // Border.
    for x in x0..=x1 {
        put_char(buf, x, y0, '─', BORDER);
        put_char(buf, x, y1, '─', BORDER);
    }
    for y in y0..=y1 {
        put_char(buf, x0, y, '│', BORDER);
        put_char(buf, x1, y, '│', BORDER);
    }
    put_char(buf, x0, y0, '╭', BORDER);
    put_char(buf, x1, y0, '╮', BORDER);
    put_char(buf, x0, y1, '╰', BORDER);
    put_char(buf, x1, y1, '╯', BORDER);

    put_str(buf, x0 + 2, y0, " Add LLM key ", TITLE, x1);
    // Masked input field: only `*`, capped to the modal interior.
    let field_w = modal_w.saturating_sub(4) as usize;
    let mask: String = std::iter::repeat_n('*', masked_len.min(field_w)).collect();
    put_str(buf, x0 + 2, y0 + 2, &mask, TEXT, x1);
    put_str(buf, x0 + 2, y1 - 1, "Tab reveal · Enter save · Esc cancel", HINT, x1);
}

/// Write `s` at `(x, row)` in `color`, clipping at column `right` (exclusive).
fn put_str(buf: &mut WireBuffer, x: u16, row: u16, s: &str, color: Color, right: u16) {
    let mut cx = x;
    for ch in s.chars() {
        if cx >= right {
            break;
        }
        let mut cell = Cell::new(ch.to_string());
        cell.fg = Some(color);
        buf.push(Coord::new(cx, row), cell);
        cx = cx.saturating_add(1);
    }
}

/// Write one `ch` at `(x, row)` in `color`.
fn put_char(buf: &mut WireBuffer, x: u16, row: u16, ch: char, color: Color) {
    let mut cell = Cell::new(ch.to_string());
    cell.fg = Some(color);
    buf.push(Coord::new(x, row), cell);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The modal renders exactly `masked_len` mask chars and no raw value.
    #[test]
    fn renders_mask_chars_only() {
        let mut buf = WireBuffer::new(80, 24);
        render_key_entry_modal(&mut buf, 80, 24, 5);
        let stars = buf
            .cells
            .iter()
            .filter(|(_, c)| c.symbol == "*")
            .count();
        assert_eq!(stars, 5);
    }
}
