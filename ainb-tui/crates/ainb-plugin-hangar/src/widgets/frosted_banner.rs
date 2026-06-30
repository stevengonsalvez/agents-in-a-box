//! Frosted progress-banner widget (P4.8).
//!
//! The active-task banner pinned across every screen. "Frosted" in a TUI is the
//! closest analog to `backdrop-blur`: a dim modifier + a double-line border that
//! reads as a distinct floating surface without blocking the scroll content
//! underneath. It renders two content rows:
//!
//! ```text
//! ⬡  claude-agent  is working   ◴ 9m 42s  ·  14 tools   [X]
//! ▌ Analyzing middleware structure...
//! ```
//!
//! Pure render — it takes an [`ActiveTaskBanner`] + the latest transcript line and
//! paints them. Width-aware: at the 80-column floor the **message** line is
//! truncated, never the agent label, the elapsed/tool stats, or the `[X]` cancel
//! control — those stay legible because they are the actionable bits (P4.8
//! acceptance: "truncates message not glyphs").

use ainb_plugin_sdk::{Cell, Color, Coord, WireBuffer};

use crate::screen::ActiveTaskBanner;

/// Violet agent glyph (matches the agent-row / working-chip accent).
const AGENT_VIOLET: Color = Color::rgb(180, 120, 220);
/// Emerald message glyph (agent-prose lane).
const AGENT_GREEN: Color = Color::rgb(80, 200, 120);
/// Soft-white header text.
const TEXT: Color = Color::rgb(220, 220, 230);
/// Muted secondary text (stats).
const MUTED: Color = Color::rgb(150, 150, 165);
/// Gold `[X]` cancel control.
const CONTROL: Color = Color::rgb(255, 215, 0);

/// The agent glyph leading the header row.
const AGENT_GLYPH: char = '⬡';
/// The message glyph leading the second row (agent-prose lane).
const MSG_GLYPH: char = '▌';

/// Render the banner into `buf`: the header row at `top`, the latest-message row
/// at `top + 1`, both clipped to `area_w`.
///
/// The header is laid out so the right-anchored `[X]` cancel control is reserved
/// first, then the agent label + stats fill from the left; only the *message*
/// line is allowed to truncate.
pub fn render_frosted_banner(
    buf: &mut WireBuffer,
    area_w: u16,
    top: u16,
    banner: &ActiveTaskBanner,
    latest_message: &str,
) {
    // --- Header row ---
    // Reserve the right-anchored `[X]` control.
    let control = "[X]";
    let control_w = u16::try_from(control.chars().count()).unwrap_or(3);
    if let Some(control_x) = area_w.checked_sub(control_w) {
        put_str(buf, control_x, top, control, CONTROL, area_w);
    }

    let mut x: u16 = 0;
    x = put_char(buf, x, top, AGENT_GLYPH, AGENT_VIOLET, area_w);
    x = put_char(buf, x, top, ' ', TEXT, area_w);
    x = put_str(buf, x, top, &banner.agent_label, TEXT, area_w);
    x = put_str(buf, x, top, " is working  ", TEXT, area_w);
    let stats = format!(
        "◴ {}  ·  {} tools",
        format_elapsed(banner.elapsed_secs),
        banner.tool_calls
    );
    // Stats clip before the reserved control region.
    let stats_right = area_w.saturating_sub(control_w + 1);
    let _ = put_str(buf, x, top, &stats, MUTED, stats_right);

    // --- Message row (the only line allowed to truncate) ---
    let msg_row = top + 1;
    let mut mx: u16 = 0;
    mx = put_char(buf, mx, msg_row, MSG_GLYPH, AGENT_GREEN, area_w);
    mx = put_char(buf, mx, msg_row, ' ', TEXT, area_w);
    put_str(buf, mx, msg_row, latest_message, TEXT, area_w);
}

/// Format whole seconds as `9m 42s` (or `42s` under a minute).
fn format_elapsed(secs: u64) -> String {
    let m = secs / 60;
    let s = secs % 60;
    if m > 0 {
        format!("{m}m {s:02}s")
    } else {
        format!("{s}s")
    }
}

/// Write `s` at `(x, row)` in `color`, clipping at `right` (exclusive). Returns
/// the next free column. Multi-byte safe.
fn put_str(buf: &mut WireBuffer, x: u16, row: u16, s: &str, color: Color, right: u16) -> u16 {
    let mut cx = x;
    for ch in s.chars() {
        cx = put_char(buf, cx, row, ch, color, right);
    }
    cx
}

/// Write one `ch` at `(x, row)` in `color`, clipping at `right`. Returns next col.
fn put_char(buf: &mut WireBuffer, x: u16, row: u16, ch: char, color: Color, right: u16) -> u16 {
    if x >= right {
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
    use ainb_hangar_core::ids::TaskId;

    fn banner() -> ActiveTaskBanner {
        ActiveTaskBanner {
            task_id: TaskId::from_str("t1").unwrap(),
            agent_label: "claude-agent".into(),
            elapsed_secs: 582,
            tool_calls: 14,
        }
    }

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

    /// The header shows the agent label, elapsed `9m 42s`, tool count, and `[X]`.
    #[test]
    fn header_shows_label_elapsed_tools_and_control() {
        let mut buf = WireBuffer::new(80, 2);
        render_frosted_banner(&mut buf, 80, 0, &banner(), "msg");
        let header = row_text(&buf, 0, 80);
        assert!(header.contains("claude-agent"));
        assert!(header.contains("9m 42s"));
        assert!(header.contains("14 tools"));
        assert!(header.contains("[X]"));
    }
}
