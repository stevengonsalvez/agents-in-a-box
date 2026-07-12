//! e38.36 — offline empty-state panel for a downed Hangar daemon.
//!
//! When the plugin's daemon link is `Disconnected` / `Error` the body is
//! otherwise a blank void (Todo/InProgress/Done all `(0)`, a red `○ offline`
//! dot top-right, no guidance) and reads as broken (Stevie screenshot
//! 2026-06-12). DECISION (Stevie): manual + clear empty-state — no host-SDK
//! spawn plumbing.
//!
//! This widget is a **pure render helper** (no state, no IO) that paints a
//! centred, framed panel over the body explaining the daemon is offline and
//! offering BOTH a one-key `[s] start daemon` action AND the literal CLI command
//! `ainb hangar daemon start` for a manual start. An optional error line surfaces
//! a failed `[s]` attempt without crashing the plugin.
//!
//! ```text
//!            ┌─ ⚠  Hangar daemon offline ──────────────────┐
//!            │                                              │
//!            │  The Hangar daemon is not running, so there  │
//!            │  is no data to show.                         │
//!            │                                              │
//!            │  [s] start daemon                            │
//!            │                                              │
//!            │  Or start it manually:                       │
//!            │  ainb hangar daemon start                    │
//!            └──────────────────────────────────────────────┘
//! ```

use ainb_plugin_sdk::{Cell, Color, Coord, WireBuffer};

use crate::shell::DAEMON_START_ARGS;

/// Gold — the palette CTA / warning-frame colour.
const GOLD: Color = Color::rgb(255, 215, 0);
/// Soft-white body text.
const TEXT: Color = Color::rgb(220, 220, 230);
/// Muted secondary text (the manual-start preamble).
const MUTED: Color = Color::rgb(150, 150, 165);
/// Error red for a failed start attempt.
const ERROR_RED: Color = Color::rgb(220, 80, 80);
/// Dim backdrop fill behind the panel so it reads as a floating surface.
const BACKDROP: Color = Color::rgb(20, 20, 28);

/// The panel title (carries the marker the render test greps for).
pub const TITLE: &str = "⚠  Hangar daemon offline";

/// The explanatory body lines (kept short so they survive the 80-column floor).
const BODY: &[&str] = &[
    "The Hangar daemon is not running, so there",
    "is no data to show.",
];

/// The one-key start hint.
const START_HINT: &str = "[s] start daemon";

/// The manual-start preamble printed above the literal CLI command.
const MANUAL_PREAMBLE: &str = "Or start it manually:";

/// The literal manual-start CLI command, assembled from the single source of
/// truth shared with the real spawn ([`DAEMON_START_ARGS`]) so the on-screen
/// hint and the real `[s]` action can never drift.
#[must_use]
pub fn manual_command() -> String {
    format!("ainb {}", DAEMON_START_ARGS.join(" "))
}

/// Render the offline empty-state panel centred in the body, into `buf`.
///
/// `start_error` is `Some(msg)` after a failed `[s]` attempt; it surfaces a red
/// error line beneath the command so a start failure is visible rather than
/// silent. `start_status` is `Some(msg)` while a `[s]` start is in flight (the
/// redial window) — a gold progress line so the panel shows the start is being
/// worked on instead of appearing ignored. A degenerate viewport still paints
/// the title + the start hint + the literal command so the guidance is never
/// wholly lost.
pub fn render_offline_empty_state(
    buf: &mut WireBuffer,
    area_w: u16,
    area_h: u16,
    start_error: Option<&str>,
    start_status: Option<&str>,
) {
    let command = manual_command();
    // Content lines in render order: body, blank, start hint, blank, preamble,
    // command, then an optional error / in-flight status line. Width drives the
    // frame width.
    let mut lines: Vec<(&str, Color)> = Vec::new();
    for b in BODY {
        lines.push((b, TEXT));
    }
    lines.push(("", TEXT));
    lines.push((START_HINT, GOLD));
    lines.push(("", TEXT));
    lines.push((MANUAL_PREAMBLE, MUTED));
    lines.push((command.as_str(), TEXT));
    if let Some(err) = start_error {
        lines.push((err, ERROR_RED));
    }
    if let Some(status) = start_status {
        lines.push(("", TEXT));
        lines.push((status, GOLD));
    }

    // Inner content width = widest line (incl. title), capped to the viewport.
    let widest = lines
        .iter()
        .map(|(s, _)| display_w(s))
        .chain(std::iter::once(display_w(TITLE)))
        .max()
        .unwrap_or(0);
    // Frame interior is content + 2-col padding each side; clamp to viewport.
    let inner_w = widest.saturating_add(4);
    let frame_w = inner_w.min(area_w.saturating_sub(2)).max(1);
    let lines_h = u16::try_from(lines.len()).unwrap_or(u16::MAX);
    // Frame height = top border + content lines + bottom border.
    let frame_h = lines_h.saturating_add(2);

    if area_w < 8 || area_h < frame_h + 1 {
        // Degenerate viewport — paint at least the title, the start hint, and the
        // literal command so the guidance survives.
        put_str(buf, 0, 0, TITLE, GOLD, area_w);
        put_str(buf, 0, 1, START_HINT, GOLD, area_w);
        put_str(buf, 0, 2, &command, TEXT, area_w);
        return;
    }

    let left = (area_w.saturating_sub(frame_w)) / 2;
    let right = left + frame_w; // exclusive
    let top = (area_h.saturating_sub(frame_h)) / 2;
    let bottom = top + frame_h - 1; // inclusive border row

    // Backdrop fill for the whole framed region so it floats over the body.
    for y in top..=bottom {
        for x in left..right {
            let mut cell = Cell::new(" ");
            cell.bg = Some(BACKDROP);
            buf.push(Coord::new(x, y), cell);
        }
    }

    draw_frame(buf, left, right, top, bottom);
    // Inline title on the top edge, just inside the left corner.
    put_str(buf, left + 3, top, TITLE, GOLD, right);

    // Content lines, indented two columns inside the frame.
    let text_x = left + 2;
    let text_right = right.saturating_sub(1);
    for (i, (line, color)) in lines.iter().enumerate() {
        let y = top + 1 + u16::try_from(i).unwrap_or(0);
        put_str(buf, text_x, y, line, *color, text_right);
    }
}

/// Draw a rounded gold frame from `(left, top)` to `(right-1, bottom)`.
fn draw_frame(buf: &mut WireBuffer, left: u16, right: u16, top: u16, bottom: u16) {
    let last = right.saturating_sub(1);
    put_char_at(buf, left, top, '┌');
    put_char_at(buf, last, top, '┐');
    put_char_at(buf, left, bottom, '└');
    put_char_at(buf, last, bottom, '┘');
    for x in (left + 1)..last {
        put_char_at(buf, x, top, '─');
        put_char_at(buf, x, bottom, '─');
    }
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

/// Display width of a string in terminal cells (char count; the panel strings
/// are single-width glyphs, so this is exact for our inputs).
fn display_w(s: &str) -> u16 {
    u16::try_from(s.chars().count()).unwrap_or(u16::MAX)
}

/// Write `s` at `(x, row)` in `color`, clipping at `right` (exclusive). Returns
/// the next free column. Multi-byte safe; preserves the panel backdrop.
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

    /// The panel paints the offline guidance, the `[s]` hint, and the literal
    /// manual command — the three things the user needs.
    #[test]
    fn renders_guidance_start_hint_and_command() {
        let mut buf = WireBuffer::new(80, 24);
        render_offline_empty_state(&mut buf, 80, 24, None, None);
        let text = full_text(&buf, 80, 24);
        assert!(text.contains("daemon offline"), "missing title:\n{text}");
        assert!(text.contains("[s]"), "missing start hint:\n{text}");
        assert!(
            text.contains("ainb hangar daemon start"),
            "missing literal command:\n{text}"
        );
        assert!(
            text.contains("not running"),
            "missing offline explanation:\n{text}"
        );
    }

    /// A start failure surfaces a red error line — visible, not silent.
    #[test]
    fn renders_error_line_when_present() {
        let mut buf = WireBuffer::new(80, 24);
        render_offline_empty_state(&mut buf, 80, 24, Some("start failed: not found"), None);
        let text = full_text(&buf, 80, 24);
        assert!(text.contains("start failed"), "missing error line:\n{text}");
        // The error cell is red.
        let has_red = buf.cells.iter().any(|(_, c)| c.fg == Some(ERROR_RED) && c.symbol != " ");
        assert!(has_red, "error line must be red");
    }

    /// The frame is gold (palette compliance, non-vacuous colour assertion).
    #[test]
    fn frame_is_gold() {
        let mut buf = WireBuffer::new(80, 24);
        render_offline_empty_state(&mut buf, 80, 24, None, None);
        let gold_corner = buf
            .cells
            .iter()
            .find(|(_, c)| c.symbol == "┌")
            .expect("top-left corner present");
        assert_eq!(gold_corner.1.fg, Some(GOLD), "frame corner must be gold");
    }

    /// Degenerate viewport still shows the marker + the start hint + the command.
    #[test]
    fn degenerate_viewport_still_shows_guidance() {
        let mut buf = WireBuffer::new(28, 3);
        render_offline_empty_state(&mut buf, 28, 3, None, None);
        let text = full_text(&buf, 28, 3);
        assert!(
            text.contains("[s]"),
            "degenerate must keep start hint:\n{text}"
        );
    }

    /// The on-screen command matches the documented manual-start command exactly.
    #[test]
    fn manual_command_is_documented_string() {
        assert_eq!(manual_command(), "ainb hangar daemon start");
    }

    /// 80×24 floor: the panel never writes a cell outside the area bounds.
    #[test]
    fn renders_at_floor_without_overflow() {
        const W: u16 = 80;
        const H: u16 = 24;
        let mut buf = WireBuffer::new(W, H);
        render_offline_empty_state(&mut buf, W, H, Some("x"), None);
        for (coord, _) in &buf.cells {
            assert!(
                coord.x < W && coord.y < H,
                "panel wrote out-of-bounds cell at ({}, {})",
                coord.x,
                coord.y,
            );
        }
    }
}
