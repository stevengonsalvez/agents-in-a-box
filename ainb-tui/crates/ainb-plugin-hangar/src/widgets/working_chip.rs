//! Working-agents avatar-stack chip (the reference's `WorkspaceAgentWorkingChip`).
//!
//! Rendered top-right on the issue list (UX §2 journey 5): a small stack of up
//! to three agent glyphs followed by a `+N` overflow badge when more agents are
//! working than fit. Pure render — the caller supplies the count of currently
//! working agents; the widget paints `⬡⬡⬡ +2` (or just `⬡⬡` for two, etc.).
//!
//! Right-aligned and width-aware: when the chip would not fit before
//! `right_edge` it is dropped entirely rather than overlapping the tabs
//! (`project_ainb_tui_width_aware_panels`). Returns the start column actually
//! used, or `None` when it was dropped, so the caller can reserve the space.

use ainb_plugin_sdk::{Cell, Color, Coord, WireBuffer};

/// Violet ring colour for the agent glyph, matching the agent-row accent
/// (reference UX §12.1 polymorphic-actor differentiation).
const AGENT_VIOLET: Color = Color::rgb(180, 120, 220);
/// Muted text for the overflow `+N` badge.
const MUTED_GRAY: Color = Color::rgb(120, 120, 140);

/// The agent glyph rendered per avatar in the stack.
const AGENT_GLYPH: char = '⬡';
/// Maximum avatars shown before collapsing the remainder into a `+N` badge.
const MAX_AVATARS: usize = 3;

/// Render the working-agents chip right-aligned so it ends at `right_edge`
/// (exclusive) on `row`. `working_count` is the number of agents currently in a
/// working state.
///
/// Returns `Some(start_col)` when the chip was drawn, or `None` when
/// `working_count == 0` (nothing to show) or it could not fit before column 0.
pub fn render_working_chip(
    buf: &mut WireBuffer,
    row: u16,
    right_edge: u16,
    working_count: usize,
) -> Option<u16> {
    if working_count == 0 {
        return None;
    }
    let avatars = working_count.min(MAX_AVATARS);
    let overflow = working_count.saturating_sub(MAX_AVATARS);
    let badge = if overflow > 0 {
        format!(" +{overflow}")
    } else {
        String::new()
    };

    let width = u16::try_from(avatars).unwrap_or(u16::MAX)
        + u16::try_from(badge.chars().count()).unwrap_or(0);
    let start = right_edge.checked_sub(width)?;

    let mut x = start;
    for _ in 0..avatars {
        let mut cell = Cell::new(AGENT_GLYPH.to_string());
        cell.fg = Some(AGENT_VIOLET);
        buf.push(Coord::new(x, row), cell);
        x = x.saturating_add(1);
    }
    for ch in badge.chars() {
        let mut cell = Cell::new(ch.to_string());
        cell.fg = Some(MUTED_GRAY);
        buf.push(Coord::new(x, row), cell);
        x = x.saturating_add(1);
    }
    Some(start)
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

    /// Zero working agents renders nothing.
    #[test]
    fn no_chip_when_no_agents_working() {
        let mut buf = WireBuffer::new(80, 1);
        assert_eq!(render_working_chip(&mut buf, 0, 80, 0), None);
        assert!(buf.cells.is_empty());
    }

    /// Two working agents render two glyphs and no overflow badge.
    #[test]
    fn two_agents_render_two_glyphs_no_badge() {
        let mut buf = WireBuffer::new(80, 1);
        render_working_chip(&mut buf, 0, 80, 2);
        let text = row_text(&buf, 0, 80);
        assert_eq!(text.matches(AGENT_GLYPH).count(), 2);
        assert!(!text.contains('+'), "unexpected overflow badge: {text:?}");
    }

    /// Five working agents cap at three glyphs plus a `+2` overflow badge.
    #[test]
    fn overflow_caps_avatars_and_shows_badge() {
        let mut buf = WireBuffer::new(80, 1);
        let start = render_working_chip(&mut buf, 0, 80, 5).unwrap();
        let text = row_text(&buf, 0, 80);
        assert_eq!(text.matches(AGENT_GLYPH).count(), MAX_AVATARS);
        assert!(text.contains("+2"), "missing overflow badge: {text:?}");
        // Right-aligned: the chip ends at the right edge.
        assert!(start < 80);
    }

    /// The chip is dropped when it can't fit before column 0.
    #[test]
    fn chip_dropped_when_no_room() {
        let mut buf = WireBuffer::new(2, 1);
        // 5 agents → 3 glyphs + " +2" = 6 cols, but right_edge is 2.
        assert_eq!(render_working_chip(&mut buf, 0, 2, 5), None);
    }
}
