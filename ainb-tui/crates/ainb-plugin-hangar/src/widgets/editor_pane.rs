//! Editor-pane widget — read-only file content viewer (P4.6).
//!
//! The skill manager's right pane shows the selected file's content. At P4 it is
//! **read-only** (in-place editing is deferred to P6); this widget paints the
//! content line-by-line, clipped to the pane area. Pure render — no state, no IO.
//!
//! Syntax-highlight hinting (`syntect`) is a P6 enhancement note; at the P4 GREEN
//! bar the content paints in neutral soft-white so the pane is legible without
//! the highlight dependency wired in.

use ainb_plugin_sdk::{Cell, Color, Coord, WireBuffer};

/// Soft-white file content.
const CONTENT: Color = Color::rgb(220, 220, 230);
/// Muted placeholder when no file is selected.
const PLACEHOLDER: Color = Color::rgb(120, 120, 140);

/// Render `content` (newline-separated) at `(x, top)` within `width` cells,
/// stopping before `bottom`. A `None` content paints a muted placeholder.
pub fn render_editor_pane(
    buf: &mut WireBuffer,
    x: u16,
    top: u16,
    bottom: u16,
    width: u16,
    content: Option<&str>,
) {
    let right = x.saturating_add(width);
    match content {
        None => {
            put_str(buf, x, top, "(select a file)", PLACEHOLDER, right);
        }
        Some(text) => {
            for (line, row) in text.lines().zip(top..bottom) {
                put_str(buf, x, row, line, CONTENT, right);
            }
        }
    }
}

/// Write `s` at `(x, row)` in `color`, clipping at `right`. Multi-byte safe.
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

#[cfg(test)]
mod tests {
    use super::*;

    fn row_text(buf: &WireBuffer, row: u16, width: u16) -> String {
        let mut out = vec![' '; width as usize];
        for (coord, cell) in &buf.cells {
            if coord.y == row && coord.x < width {
                if let Some(ch) = cell.symbol.chars().next() {
                    out[coord.x as usize] = ch;
                }
            }
        }
        out.into_iter().collect()
    }

    /// A `None` content paints the placeholder.
    #[test]
    fn none_content_paints_placeholder() {
        let mut buf = WireBuffer::new(30, 4);
        render_editor_pane(&mut buf, 0, 0, 4, 30, None);
        assert!(row_text(&buf, 0, 30).contains("select a file"));
    }

    /// Multi-line content paints one line per row.
    #[test]
    fn content_paints_one_line_per_row() {
        let mut buf = WireBuffer::new(30, 4);
        render_editor_pane(&mut buf, 0, 0, 4, 30, Some("first\nsecond"));
        assert!(row_text(&buf, 0, 30).contains("first"));
        assert!(row_text(&buf, 1, 30).contains("second"));
    }
}
