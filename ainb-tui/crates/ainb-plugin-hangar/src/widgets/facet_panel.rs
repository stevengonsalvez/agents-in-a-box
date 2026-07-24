//! Faceted-filter panel overlay widget (multica-gap #10).
//!
//! A centered, rounded, backdrop-filled overlay that lists every facet section
//! (Status / Priority / Label / Assignee / Due) with its in-scope values, each a
//! `[x]`/`[ ]` checkbox + gold-accented count (`Todo (3)`), the active section's
//! header accented and its cursor row marked with a selection-green `▶`. It shows
//! ALL sections at once (multica's expanded facet picker) so a single capture
//! proves the per-value drill-down counts across facets.
//!
//! Pure render: it paints from a `(sections)` snapshot only — no state, no IO.
//! Values + counts come from the caller's
//! [`facet_counts`](crate::screen::issue_list::IssueListState::facet_counts);
//! zero-count values are already omitted upstream so the picker only lists
//! options present in-scope (multica parity).

use ainb_plugin_sdk::{Cell, Color, Coord, WireBuffer};

/// Gold — the style guide's CTA gold: panel title, active section header, counts.
const GOLD: Color = Color::rgb(255, 215, 0);
/// Cornflower blue — the panel border.
const BORDER: Color = Color::rgb(100, 149, 237);
/// Selection green — the cursor `▶` marker.
const SELECTION_GREEN: Color = Color::rgb(100, 200, 100);
/// Soft white — value labels.
const SOFT_WHITE: Color = Color::rgb(220, 220, 230);
/// Muted gray — inactive section headers, unchecked boxes, the help line.
const MUTED_GRAY: Color = Color::rgb(120, 120, 140);
/// Panel backdrop — the overlay surface the content floats on.
const PANEL_BG: Color = Color::rgb(30, 30, 40);

/// One selectable value row in a facet section: its display label, in-scope
/// count, and whether it is currently selected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FacetValueRow {
    /// The value's display label (`Todo`, `P0`, `bug`, `Member`, `Overdue`).
    pub label: String,
    /// The drill-down count for this value in the current scope (always > 0).
    pub count: usize,
    /// Whether this value is currently in the facet's selection.
    pub selected: bool,
}

/// One facet section: its header label, whether it is the active (focused)
/// section, its in-scope value rows, and — for the active section only — the
/// cursor index into `values`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FacetSectionView {
    /// The section header (`Status`, `Priority`, …).
    pub label: String,
    /// Whether this section is the focused one (its header + cursor accented).
    pub active: bool,
    /// The in-scope value rows (zero-count values already omitted).
    pub values: Vec<FacetValueRow>,
    /// The cursor row within `values`, `Some` only for the active section.
    pub cursor: Option<usize>,
}

/// The panel title rendered on the top border.
const TITLE: &str = "⛃ Filters";
/// The one-line key legend on the bottom inner row.
const HELP: &str = "Space toggle · ←→ section · C clear · Esc close";

/// Render the facet panel centered in the `[top, bottom]` band, `area_w` wide.
///
/// Draws a rounded blue frame over a panel-bg backdrop, the gold title inlaid on
/// the top edge, then each section header followed by its `[x] label (count)`
/// value rows; the active section's header is gold and its cursor row carries a
/// selection-green `▶`. Content is clipped to the inner box (never overflows the
/// band or the width). A no-op when the band is too small to frame.
pub fn render(
    buf: &mut WireBuffer,
    area_w: u16,
    top: u16,
    bottom: u16,
    sections: &[FacetSectionView],
) {
    if area_w < 8 || bottom <= top + 2 {
        return;
    }
    // Box geometry: a comfortable fixed width clamped to the area, tall enough
    // for the title, every section header + its values, and the help line —
    // clamped to the available band.
    let inner_w: u16 = area_w.saturating_sub(4).clamp(4, 46);
    let box_w = inner_w + 2;
    let left = (area_w.saturating_sub(box_w)) / 2;
    let right = left + box_w; // exclusive

    let content_rows: u16 = sections
        .iter()
        .map(|s| 1 + u16::try_from(s.values.len()).unwrap_or(u16::MAX))
        .fold(0u16, u16::saturating_add);
    // title border + content + help + bottom border, clamped to the band.
    let want_h = content_rows.saturating_add(3);
    let avail_h = bottom.saturating_sub(top);
    let box_h = want_h.min(avail_h).max(3);
    let box_top = top + (avail_h.saturating_sub(box_h)) / 2;
    let box_bottom = box_top + box_h - 1; // inclusive

    // Backdrop fill so the overlay reads as opaque over the board.
    for y in box_top..=box_bottom {
        for x in left..right {
            let mut cell = Cell::new(" ".to_string());
            cell.bg = Some(PANEL_BG);
            buf.push(Coord::new(x, y), cell);
        }
    }
    draw_frame(buf, left, right, box_top, box_bottom);
    // Title inlaid on the top edge: "╭─ ⛃ Filters ─…".
    put(buf, left + 2, box_top, "─ ", GOLD, right);
    let after = put(buf, left + 4, box_top, TITLE, GOLD, right);
    put(buf, after, box_top, " ", GOLD, right);

    let inner_left = left + 1;
    let inner_right = right - 1; // exclusive clip
    let last_content_row = box_bottom.saturating_sub(1); // reserve for help line
    let mut y = box_top + 1;
    for section in sections {
        if y >= last_content_row {
            break;
        }
        // Section header: gold when active, muted otherwise.
        let header_color = if section.active { GOLD } else { MUTED_GRAY };
        let marker = if section.active { "▾ " } else { "  " };
        let hx = put(buf, inner_left + 1, y, marker, header_color, inner_right);
        put(buf, hx, y, &section.label, header_color, inner_right);
        y += 1;
        for (i, value) in section.values.iter().enumerate() {
            if y >= last_content_row {
                break;
            }
            let is_cursor = section.active && section.cursor == Some(i);
            // Cursor marker in selection green, else a blank gutter.
            if is_cursor {
                put(buf, inner_left + 1, y, "▶", SELECTION_GREEN, inner_right);
            }
            let checkbox = if value.selected { "[x] " } else { "[ ] " };
            let cb_color = if value.selected {
                SELECTION_GREEN
            } else {
                MUTED_GRAY
            };
            let cx = put(buf, inner_left + 3, y, checkbox, cb_color, inner_right);
            let lx = put(buf, cx, y, &value.label, SOFT_WHITE, inner_right);
            // The count in the EXACT `(<n>)` form the tripwire asserts.
            put(
                buf,
                lx,
                y,
                &format!(" ({})", value.count),
                GOLD,
                inner_right,
            );
            y += 1;
        }
    }
    // Help legend on the bottom inner row.
    put(
        buf,
        inner_left + 1,
        box_bottom.saturating_sub(1),
        HELP,
        MUTED_GRAY,
        inner_right,
    );
}

/// Draw a rounded blue frame from `(left, top)` to `(right-1, bottom)` over the
/// panel backdrop (inclusive corners).
fn draw_frame(buf: &mut WireBuffer, left: u16, right: u16, top: u16, bottom: u16) {
    let last = right.saturating_sub(1);
    put_frame(buf, left, top, '╭');
    put_frame(buf, last, top, '╮');
    put_frame(buf, left, bottom, '╰');
    put_frame(buf, last, bottom, '╯');
    for x in (left + 1)..last {
        put_frame(buf, x, top, '─');
        put_frame(buf, x, bottom, '─');
    }
    for y in (top + 1)..bottom {
        put_frame(buf, left, y, '│');
        put_frame(buf, last, y, '│');
    }
}

/// Write one blue frame glyph at `(x, y)` over the panel backdrop.
fn put_frame(buf: &mut WireBuffer, x: u16, y: u16, ch: char) {
    let mut cell = Cell::new(ch.to_string());
    cell.fg = Some(BORDER);
    cell.bg = Some(PANEL_BG);
    buf.push(Coord::new(x, y), cell);
}

/// Write `s` at `(x, row)` in `color` over the panel backdrop, clipping at
/// `right` (exclusive). Multi-byte safe. Returns the next free column.
fn put(buf: &mut WireBuffer, x: u16, row: u16, s: &str, color: Color, right: u16) -> u16 {
    let mut cx = x;
    for ch in s.chars() {
        if cx >= right {
            break;
        }
        let mut cell = Cell::new(ch.to_string());
        cell.fg = Some(color);
        cell.bg = Some(PANEL_BG);
        buf.push(Coord::new(cx, row), cell);
        cx = cx.saturating_add(1);
    }
    cx
}
