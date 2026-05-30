//! Transcript renderer — the 5-colour message taxonomy (Multica UX §7 verbatim).
//!
//! The task-detail screen (P4.4) streams a task's transcript: a sequence of
//! [`MessageKind`]-lanes, each with one glyph + colour. This widget is a **pure
//! render helper** — it takes the already-derived display view
//! ([`crate::screen::task_detail::ViewEntry`]) and paints one entry per row,
//! glyph in the taxonomy colour followed by the body text clipped at the area
//! width. It holds no state and does no IO; the owning screen's reducer owns the
//! transcript buffer and the collapse grouping.
//!
//! The taxonomy is locked verbatim from UX §7 — five lanes, no more, no less:
//!
//! | Kind        | Colour                     | Glyph |
//! |-------------|----------------------------|-------|
//! | Agent text  | emerald `(80, 200, 120)`   | `▌`   |
//! | Thinking    | violet  `(180, 120, 220)`  | `*`   |
//! | Tool call   | blue    `(100, 160, 240)`  | `→`   |
//! | Tool result | slate   `(140, 145, 160)`  | `←`   |
//! | Error       | red     `(220, 100, 100)`  | `!`   |
//!
//! Width-aware: each line is `<glyph> <body>`, clipped at `area_w`; a body that
//! would overflow the area is truncated rather than wrapped, keeping one entry
//! per row at the 80×24 floor (`project_ainb_tui_width_aware_panels`).

use ainb_plugin_sdk::{Cell, Color, Coord, WireBuffer};
use ainb_hangar_proto::events::MessageKind;

use crate::screen::task_detail::ViewEntry;

/// Emerald lane for agent prose (UX §7).
const AGENT: Color = Color::rgb(80, 200, 120);
/// Violet lane for agent thinking (UX §7).
const THINKING: Color = Color::rgb(180, 120, 220);
/// Blue lane for tool calls (UX §7).
const TOOL_CALL: Color = Color::rgb(100, 160, 240);
/// Slate lane for tool results / interleaved comments (UX §7).
const TOOL_RESULT: Color = Color::rgb(140, 145, 160);
/// Red lane for errors (UX §7).
const ERROR: Color = Color::rgb(220, 100, 100);

/// The taxonomy glyph for `kind` (the leading marker on every transcript line).
#[must_use]
pub const fn transcript_glyph(kind: MessageKind) -> char {
    match kind {
        MessageKind::Agent => '▌',
        MessageKind::Thinking => '*',
        MessageKind::ToolCall => '→',
        MessageKind::ToolResult => '←',
        MessageKind::Error => '!',
    }
}

/// The taxonomy colour for `kind`.
#[must_use]
pub const fn transcript_color(kind: MessageKind) -> Color {
    match kind {
        MessageKind::Agent => AGENT,
        MessageKind::Thinking => THINKING,
        MessageKind::ToolCall => TOOL_CALL,
        MessageKind::ToolResult => TOOL_RESULT,
        MessageKind::Error => ERROR,
    }
}

/// Render the `entries` view into `buf` as one row per entry, starting at `top`
/// and stopping before `bottom` (exclusive) or when `entries` is exhausted.
///
/// Each line is `<glyph> <body>`: the glyph painted in the entry's taxonomy
/// colour, then a space, then the body in the same lane colour, clipped at
/// `area_w`. A [`ViewEntry::CollapsedThinking`] run paints a single violet fold
/// marker (`* … N thinking lines (t to expand)`).
pub fn render_transcript(
    buf: &mut WireBuffer,
    area_w: u16,
    top: u16,
    bottom: u16,
    entries: &[ViewEntry],
) {
    let mut row = top;
    for entry in entries {
        if row >= bottom {
            break;
        }
        match entry {
            ViewEntry::Line(line) => {
                let color = transcript_color(line.kind());
                let glyph = transcript_glyph(line.kind());
                put_line(buf, row, area_w, glyph, line.body(), color);
            }
            ViewEntry::CollapsedThinking { count } => {
                let body = format!("… {count} thinking lines (t to expand)");
                put_line(buf, row, area_w, '*', &body, THINKING);
            }
        }
        row = row.saturating_add(1);
    }
}

/// Paint `<glyph> <body>` at column 0 of `row` in `color`, clipping at `area_w`.
/// Multi-byte safe; truncates the body rather than wrapping.
fn put_line(buf: &mut WireBuffer, row: u16, area_w: u16, glyph: char, body: &str, color: Color) {
    let mut x: u16 = 0;
    x = put_char(buf, x, row, glyph, color, area_w);
    x = put_char(buf, x, row, ' ', color, area_w);
    for ch in body.chars() {
        if x >= area_w {
            break;
        }
        x = put_char(buf, x, row, ch, color, area_w);
    }
}

/// Write one `ch` at `(x, row)` in `color`, clipping at `area_w`. Returns the
/// next free column.
fn put_char(buf: &mut WireBuffer, x: u16, row: u16, ch: char, color: Color, area_w: u16) -> u16 {
    if x >= area_w {
        return x;
    }
    let mut cell = Cell::new(ch.to_string());
    cell.fg = Some(color);
    buf.push(Coord::new(x, row), cell);
    x.saturating_add(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every taxonomy lane maps to a distinct glyph.
    #[test]
    fn glyphs_are_distinct_per_lane() {
        let glyphs = [
            transcript_glyph(MessageKind::Agent),
            transcript_glyph(MessageKind::Thinking),
            transcript_glyph(MessageKind::ToolCall),
            transcript_glyph(MessageKind::ToolResult),
            transcript_glyph(MessageKind::Error),
        ];
        let mut sorted = glyphs.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), 5, "taxonomy glyphs must be distinct");
    }

    /// Every taxonomy lane maps to a distinct colour.
    #[test]
    fn colours_are_distinct_per_lane() {
        let colours = [
            transcript_color(MessageKind::Agent),
            transcript_color(MessageKind::Thinking),
            transcript_color(MessageKind::ToolCall),
            transcript_color(MessageKind::ToolResult),
            transcript_color(MessageKind::Error),
        ];
        for (i, a) in colours.iter().enumerate() {
            for b in &colours[i + 1..] {
                assert_ne!(a, b, "taxonomy colours must be distinct");
            }
        }
    }
}
