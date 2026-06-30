//! Filter-chip bar widget.
//!
//! A horizontal row of selectable chips (`[All] [Members] [Agents] [Mine]`),
//! the active one accented, the rest muted. Pure render — the *which-chip*
//! state lives in the owning screen's reducer
//! ([`crate::screen::issue_list::FilterChip`] for P4.3); this widget only paints
//! a `(labels, active_index)` pair, so the same bar serves the issue list (P4.3)
//! and the skill manager (P4.6, whose chips are `All / Used / Unused / Mine`).
//!
//! Width-aware: chips are drawn left-to-right and clipped at `area_w`; a chip
//! that would overflow the area is dropped rather than wrapped, keeping the bar
//! a single row at the 80×24 floor (`project_ainb_tui_width_aware_panels`).

use ainb_plugin_sdk::{Cell, Color, Coord, WireBuffer};

/// Accent for the active chip.
const ACTIVE: Color = Color::rgb(255, 215, 0);
/// Muted text for inactive chips.
const INACTIVE: Color = Color::rgb(120, 120, 140);

/// Render a chip bar at `(0, row)`: each label wrapped in `[ ]`, the chip at
/// `active_index` accented gold, the rest muted gray. Returns the next free
/// column after the last chip drawn.
///
/// Chips are clipped at `area_w`; one that would start past the right edge is
/// skipped (the bar never wraps to a second row).
pub fn render_chip_bar(
    buf: &mut WireBuffer,
    row: u16,
    area_w: u16,
    labels: &[&str],
    active_index: usize,
) -> u16 {
    let mut x: u16 = 0;
    for (i, label) in labels.iter().enumerate() {
        let color = if i == active_index { ACTIVE } else { INACTIVE };
        // `[Label] ` — bracket pair + trailing gap. Stop if we've run out of row.
        if x >= area_w {
            break;
        }
        x = put_str(buf, x, row, "[", INACTIVE, area_w);
        x = put_str(buf, x, row, label, color, area_w);
        x = put_str(buf, x, row, "] ", INACTIVE, area_w);
    }
    x
}

/// Write `s` at `(x, row)` in `color`, clipping at `area_w`. Returns the next
/// free column. Multi-byte safe.
fn put_str(buf: &mut WireBuffer, x: u16, row: u16, s: &str, color: Color, area_w: u16) -> u16 {
    let mut cx = x;
    for ch in s.chars() {
        if cx >= area_w {
            break;
        }
        let mut cell = Cell::new(ch.to_string());
        cell.fg = Some(color);
        buf.push(Coord::new(cx, row), cell);
        cx = cx.saturating_add(1);
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

    /// The active chip is gold, the others muted, and every label appears.
    #[test]
    fn active_chip_is_accented() {
        let mut buf = WireBuffer::new(80, 1);
        render_chip_bar(&mut buf, 0, 80, &["All", "Members", "Agents", "Mine"], 2);
        let text = row_text(&buf, 0, 80);
        assert!(text.contains("All"));
        assert!(text.contains("Agents"));
        // The `Agents` label (index 2) should be painted gold.
        let agents_start = text.find("Agents").unwrap();
        let agents_cell = buf
            .cells
            .iter()
            .find(|(c, _)| c.y == 0 && c.x as usize == agents_start)
            .map(|(_, cell)| cell.fg);
        assert_eq!(agents_cell, Some(Some(ACTIVE)));
    }

    /// Chips never write past the area width (single-row, clipped).
    #[test]
    fn chips_clip_at_area_width() {
        let mut buf = WireBuffer::new(12, 1);
        render_chip_bar(&mut buf, 0, 12, &["All", "Members", "Agents", "Mine"], 0);
        for (coord, _) in &buf.cells {
            assert!(coord.x < 12, "chip wrote out of bounds at x={}", coord.x);
        }
    }
}
