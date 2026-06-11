//! Label-chip run widget (e38.10).
//!
//! Renders an issue's labels as a compact horizontal run of chips —
//! `‹bug› ‹p0›` — in a calm violet, used on the issue-list rows and the
//! task-detail sidebar so a label renders identically wherever it appears
//! (Multica visual parity, the `LabelChip` component). Pure render: it takes the
//! label names and paints cells, holding no state and doing no IO.
//!
//! Width-aware: chips are drawn left-to-right and clipped at `right`
//! (exclusive); a chip that would overflow is dropped rather than wrapped, so the
//! run stays on its single row at the 80×24 floor
//! (`project_ainb_tui_width_aware_panels`). Multi-byte safe — every write
//! iterates `char`s, never bytes (`reference_rust_utf8_truncate_trap`).

use ainb_plugin_sdk::{Cell, Color, Coord, WireBuffer};

/// Violet accent for label chips (distinct from the gold headers + green
/// selection so a chip reads as its own kind of token).
const LABEL_VIOLET: Color = Color::rgb(180, 150, 230);

/// Left bracket glyph wrapping each label.
const CHIP_OPEN: char = '‹';
/// Right bracket glyph wrapping each label.
const CHIP_CLOSE: char = '›';

/// Render `labels` as a chip run starting at `(x, row)`, clipped at `right`.
///
/// Each label is wrapped `‹name›` with a trailing space; the whole run is
/// painted in [`LABEL_VIOLET`]. Returns the next free column after the last chip
/// drawn (or `x` unchanged when `labels` is empty). A chip that would start at or
/// past `right` is skipped, and a chip is clipped mid-glyph at `right` rather
/// than wrapping — the run never spills onto a second row.
pub fn render_label_chips(
    buf: &mut WireBuffer,
    x: u16,
    row: u16,
    right: u16,
    labels: &[String],
) -> u16 {
    let mut cx = x;
    for label in labels {
        if cx >= right {
            break;
        }
        cx = put_char(buf, cx, row, CHIP_OPEN, right);
        cx = put_str(buf, cx, row, label, right);
        cx = put_char(buf, cx, row, CHIP_CLOSE, right);
        cx = put_char(buf, cx, row, ' ', right);
    }
    cx
}

/// Write a single `ch` at `(x, row)` in the label colour if `x < right`,
/// returning the next column.
fn put_char(buf: &mut WireBuffer, x: u16, row: u16, ch: char, right: u16) -> u16 {
    if x >= right {
        return x;
    }
    let mut cell = Cell::new(ch.to_string());
    cell.fg = Some(LABEL_VIOLET);
    buf.push(Coord::new(x, row), cell);
    x.saturating_add(1)
}

/// Write `s` at `(x, row)` in the label colour, clipping by **chars** at column
/// `right` (exclusive). Returns the next free column. Multi-byte safe.
fn put_str(buf: &mut WireBuffer, x: u16, row: u16, s: &str, right: u16) -> u16 {
    let mut cx = x;
    for ch in s.chars() {
        if cx >= right {
            break;
        }
        cx = put_char(buf, cx, row, ch, right);
    }
    cx
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Read a row back out of a `WireBuffer` for assertion; unwritten cells are
    /// spaces.
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

    /// Each label is wrapped `‹name›` and painted in the violet accent.
    #[test]
    fn chips_render_wrapped_and_violet() {
        let mut buf = WireBuffer::new(40, 1);
        let end = render_label_chips(&mut buf, 0, 0, 40, &["bug".into(), "p0".into()]);
        let text = row_text(&buf, 0, 40);
        assert!(text.contains("‹bug›"), "bug chip: {text:?}");
        assert!(text.contains("‹p0›"), "p0 chip: {text:?}");
        assert!(end > 0, "returns the advanced cursor");
        // The first glyph is painted violet.
        let first = buf
            .cells
            .iter()
            .find(|(c, _)| c.x == 0 && c.y == 0)
            .map(|(_, cell)| cell.fg);
        assert_eq!(first, Some(Some(LABEL_VIOLET)));
    }

    /// An empty label set draws nothing and leaves the cursor where it started.
    #[test]
    fn empty_labels_draw_nothing() {
        let mut buf = WireBuffer::new(40, 1);
        let end = render_label_chips(&mut buf, 5, 0, 40, &[]);
        assert_eq!(end, 5, "cursor unchanged on empty input");
        assert!(buf.cells.is_empty(), "nothing painted");
    }

    /// Chips never write past `right` (single-row, clipped mid-run).
    #[test]
    fn chips_clip_at_right_bound() {
        let mut buf = WireBuffer::new(12, 1);
        render_label_chips(
            &mut buf,
            0,
            0,
            10,
            &["bug".into(), "regression".into(), "p0".into()],
        );
        for (coord, _) in &buf.cells {
            assert!(coord.x < 10, "chip wrote past the bound at x={}", coord.x);
        }
    }

    /// A multi-byte label name clips on a char boundary, never panicking on a
    /// byte split (`reference_rust_utf8_truncate_trap`).
    #[test]
    fn multibyte_label_clips_on_char_boundary() {
        let mut buf = WireBuffer::new(8, 1);
        // The label is wider than the bound; clipping must not panic.
        render_label_chips(&mut buf, 0, 0, 6, &["日本語ラベル".into()]);
        for (coord, _) in &buf.cells {
            assert!(coord.x < 6, "multibyte chip wrote past the bound");
        }
    }
}
