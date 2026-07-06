//! P5.6 — danger-full-access warning modal (full-width gold-border overlay).
//!
//! ainb runs agents with `danger-full-access`: full filesystem + network, no
//! container sandbox at v1 (the build-plan "trust-the-user-with-warnings"
//! decision). This modal surfaces that once on first run (and, when the
//! daemon→plugin event push lands, on the first provider invocation per session)
//! so the user makes an informed choice before an agent touches their machine.
//!
//! It is a **full-width banner overlay** with a gold rounded border (the
//! palette's CTA colour — `../.claude/skills/tui-screen/SKILL.md`), drawn over
//! whatever screen is active. Pure render: it takes the viewport and paints the
//! framed warning + the `[y] I understand` accept hint. The dismiss key (`y`)
//! and the ack persistence are the plugin glue's concern (a host `write_plugin_data`
//! / `state.toml` write), not this widget's.
//!
//! ```text
//! ┌─ ⚠  danger-full-access ─────────────────────────────────────────┐
//! │  Agents run with FULL filesystem and network access — no        │
//! │  container sandbox. They can read, write, and delete any file    │
//! │  your user can, and reach any network your machine can.          │
//! │  Only run agents you trust.                                      │
//! │                                                                  │
//! │  [y] I understand — continue        [q] quit                     │
//! └──────────────────────────────────────────────────────────────────┘
//! ```

use ainb_plugin_sdk::{Cell, Color, Coord, WireBuffer};

/// Gold — the palette CTA / warning-frame colour.
const GOLD: Color = Color::rgb(255, 215, 0);
/// Soft-white body text.
const TEXT: Color = Color::rgb(220, 220, 230);
/// Muted secondary text (the quit hint).
const MUTED: Color = Color::rgb(150, 150, 165);
/// Dim backdrop fill behind the modal so it reads as a floating surface.
const BACKDROP: Color = Color::rgb(20, 20, 28);

/// The modal title (carries the marker the e2e tripwire greps for).
pub const TITLE: &str = "⚠  danger-full-access";

/// The warning body lines (kept short so they survive the 80-column floor).
const BODY: &[&str] = &[
    "Agents run with FULL filesystem and network access — no",
    "container sandbox. They can read, write, and delete any",
    "file your user can, and reach any network your machine can.",
    "Only run agents you trust.",
];

/// The accept-hint line.
const ACCEPT_HINT: &str = "[y] I understand — continue";
/// The quit-hint suffix.
const QUIT_HINT: &str = "[q] quit";

/// The number of warning body lines (kept in sync with [`BODY`]).
const BODY_LINES: u16 = 4;

/// Modal geometry: how many rows the framed box occupies (border + body +
/// spacer + hint row). Public so the plugin can reserve / centre it.
#[must_use]
pub const fn modal_height() -> u16 {
    // top border + body + spacer + hint + bottom border.
    BODY_LINES + 4
}

/// Render the danger-full-access modal centred vertically, spanning `area_w`
/// with a one-column inset on each side, into `buf`.
///
/// Full-width: the frame runs `inset..area_w-inset`; the body text is clipped at
/// the right border so it never overruns. The whole framed region (border +
/// interior) is backdrop-filled first so it floats over the screen beneath.
pub fn render_danger_access_modal(buf: &mut WireBuffer, area_w: u16, area_h: u16) {
    let inset: u16 = 1;
    let left = inset;
    let right = area_w.saturating_sub(inset); // exclusive
    if right <= left + 4 || area_h < modal_height() + 2 {
        // Degenerate viewport — paint at least the title so the tripwire marker
        // and the accept key are still present.
        put_str(buf, 0, 0, TITLE, GOLD, area_w);
        put_str(buf, 0, 1, ACCEPT_HINT, GOLD, area_w);
        return;
    }

    let h = modal_height();
    let top = (area_h.saturating_sub(h)) / 2;
    let bottom = top + h - 1; // inclusive border row

    // Backdrop fill for the whole framed region.
    for y in top..=bottom {
        for x in left..right {
            let mut cell = Cell::new(" ");
            cell.bg = Some(BACKDROP);
            buf.push(Coord::new(x, y), cell);
        }
    }

    // Border (rounded gold frame) + an inline title on the top edge.
    draw_frame(buf, left, right, top, bottom);
    // Title sits just inside the top-left corner: "┌─ ⚠  danger-full-access ─…"
    let title_x = left + 3;
    put_str(buf, title_x, top, TITLE, GOLD, right);

    // Body text, one line per row, indented two columns inside the frame.
    let text_x = left + 2;
    let text_right = right.saturating_sub(1);
    for (i, line) in BODY.iter().enumerate() {
        let y = top + 1 + u16::try_from(i).unwrap_or(0);
        put_str(buf, text_x, y, line, TEXT, text_right);
    }

    // Hint row: accept (gold) then quit (muted), one blank row above it.
    let hint_y = bottom - 1;
    let mut hx = put_str(buf, text_x, hint_y, ACCEPT_HINT, GOLD, text_right);
    hx = hx.saturating_add(6);
    put_str(buf, hx, hint_y, QUIT_HINT, MUTED, text_right);
}

/// Draw a rounded gold frame from `(left, top)` to `(right-1, bottom)`.
fn draw_frame(buf: &mut WireBuffer, left: u16, right: u16, top: u16, bottom: u16) {
    let last = right.saturating_sub(1);
    // Corners.
    put_char_at(buf, left, top, '┌');
    put_char_at(buf, last, top, '┐');
    put_char_at(buf, left, bottom, '└');
    put_char_at(buf, last, bottom, '┘');
    // Horizontal edges.
    for x in (left + 1)..last {
        put_char_at(buf, x, top, '─');
        put_char_at(buf, x, bottom, '─');
    }
    // Vertical edges.
    for y in (top + 1)..bottom {
        put_char_at(buf, left, y, '│');
        put_char_at(buf, last, y, '│');
    }
}

/// Write one gold frame glyph at `(x, y)` over the backdrop.
fn put_char_at(buf: &mut WireBuffer, x: u16, y: u16, ch: char) {
    let mut cell = Cell::new(ch.to_string());
    cell.fg = Some(GOLD);
    cell.bg = Some(BACKDROP);
    buf.push(Coord::new(x, y), cell);
}

/// Write `s` at `(x, row)` in `color`, clipping at `right` (exclusive). Returns
/// the next free column. Multi-byte safe; preserves the modal backdrop.
fn put_str(buf: &mut WireBuffer, x: u16, row: u16, s: &str, color: Color, right: u16) -> u16 {
    let mut cx = x;
    for ch in s.chars() {
        if cx >= right {
            break;
        }
        let mut cell = Cell::new(ch.to_string());
        cell.fg = Some(color);
        cell.bg = Some(BACKDROP);
        buf.push(Coord::new(cx, row), cell);
        cx = cx.saturating_add(1);
    }
    cx
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

    fn full_text(buf: &WireBuffer, width: u16, height: u16) -> String {
        (0..height).map(|r| row_text(buf, r, width)).collect::<Vec<_>>().join("\n")
    }

    /// `BODY_LINES` must track the actual body slice length (geometry relies on it).
    #[test]
    fn body_lines_const_matches_slice() {
        assert_eq!(BODY.len(), BODY_LINES as usize);
    }

    /// The modal renders the `danger-full-access` marker (the tripwire grep) and
    /// the accept hint.
    #[test]
    fn renders_marker_and_accept_hint() {
        let mut buf = WireBuffer::new(80, 24);
        render_danger_access_modal(&mut buf, 80, 24);
        let text = full_text(&buf, 80, 24);
        assert!(
            text.contains("danger-full-access"),
            "missing marker:\n{text}"
        );
        assert!(text.contains("[y]"), "missing accept hint:\n{text}");
        assert!(
            text.contains("filesystem and network"),
            "missing warning body:\n{text}"
        );
    }

    /// The frame is gold and the body is soft-white (palette compliance,
    /// non-vacuous colour assertion).
    #[test]
    fn frame_is_gold() {
        let mut buf = WireBuffer::new(80, 24);
        render_danger_access_modal(&mut buf, 80, 24);
        let gold_corner = buf
            .cells
            .iter()
            .find(|(_, c)| c.symbol == "┌")
            .expect("top-left corner present");
        assert_eq!(gold_corner.1.fg, Some(GOLD), "frame corner must be gold");
    }

    /// Degenerate viewport still paints the marker + accept key (never panics,
    /// never silently empty).
    #[test]
    fn degenerate_viewport_still_shows_marker() {
        let mut buf = WireBuffer::new(10, 2);
        render_danger_access_modal(&mut buf, 10, 2);
        let text = full_text(&buf, 10, 2);
        assert!(
            text.contains("danger"),
            "degenerate must still warn:\n{text}"
        );
    }

    /// Full-width: the frame's right edge tracks the viewport (minus the inset),
    /// not a fixed narrow box.
    #[test]
    fn frame_spans_full_width() {
        let mut buf = WireBuffer::new(120, 24);
        render_danger_access_modal(&mut buf, 120, 24);
        // The right border column should be near the right edge (120 - inset - 1).
        let max_frame_x = buf
            .cells
            .iter()
            .filter(|(_, c)| c.symbol == "┐" || c.symbol == "┘")
            .map(|(coord, _)| coord.x)
            .max()
            .expect("right corners present");
        assert!(
            max_frame_x >= 117,
            "frame not full-width: right edge at {max_frame_x}"
        );
    }
}
