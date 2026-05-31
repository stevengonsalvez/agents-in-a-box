//! Task-detail right-hand sidebar — progressive disclosure (Multica UX §12.8).
//!
//! A narrow column to the right of the transcript that surfaces the task's
//! metadata: status and assignee are *always* shown; the description (and, in
//! later phases, dates / labels / PRs) only appears when present. Pure render —
//! it takes an [`IssueRow`] and paints labelled rows, holding no state and doing
//! no IO.
//!
//! Progressive disclosure keeps the panel quiet: a row whose value is `None`
//! (e.g. an unset description) is omitted entirely rather than rendered as an
//! empty `Field:` line, so the sidebar grows only as the task accrues metadata.
//!
//! Width-aware: every value is clipped at `width`; the caller (the task-detail
//! renderer) collapses the sidebar away entirely under narrow widths, so this
//! widget assumes it has been given a usable column.

use ainb_plugin_sdk::{Cell, Color, Coord, WireBuffer};
use ainb_hangar_proto::events::IssueRow;

/// Cornflower-blue label colour, matching the ainb-tui chrome accent.
const LABEL: Color = Color::rgb(100, 149, 237);
/// Soft-white value text.
const VALUE: Color = Color::rgb(220, 220, 230);

/// Render the sidebar for `issue` into the column starting at `x`, from `top`
/// to `bottom` (exclusive), `width` cells wide.
///
/// Rows are painted top-down: `Status` and `Assignee` always; `Project` (the
/// workspace slug) always; `Description` only when the issue carries one
/// (progressive disclosure). Each row is `Label: value`, the label in
/// cornflower-blue and the value in soft-white, clipped at `width`.
pub fn render_sidebar(
    buf: &mut WireBuffer,
    x: u16,
    top: u16,
    bottom: u16,
    width: u16,
    issue: &IssueRow,
) {
    let mut row = top;
    // Always-shown rows.
    row = put_field(buf, x, row, bottom, width, "Status", &issue.state);
    let assignee = issue.assignee.as_deref().unwrap_or("unassigned");
    row = put_field(buf, x, row, bottom, width, "Assignee", assignee);
    row = put_field(buf, x, row, bottom, width, "Project", &issue.workspace_id);
    // Progressive disclosure: only when present.
    if let Some(desc) = issue.description.as_deref() {
        let _ = put_field(buf, x, row, bottom, width, "Notes", desc);
    }
}

/// Paint a `Label: value` row at `(x, row)` clipped at `x + width`, returning the
/// next free row (a no-op returning `row` when `row >= bottom`).
fn put_field(
    buf: &mut WireBuffer,
    x: u16,
    row: u16,
    bottom: u16,
    width: u16,
    label: &str,
    value: &str,
) -> u16 {
    if row >= bottom {
        return row;
    }
    let right = x.saturating_add(width);
    let mut cx = put_str(buf, x, row, label, LABEL, right);
    cx = put_str(buf, cx, row, ": ", LABEL, right);
    let _ = put_str(buf, cx, row, value, VALUE, right);
    row.saturating_add(1)
}

/// Write `s` at `(x, row)` in `color`, clipping at column `right` (exclusive).
/// Returns the next free column. Multi-byte safe.
fn put_str(buf: &mut WireBuffer, x: u16, row: u16, s: &str, color: Color, right: u16) -> u16 {
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
    cx
}

#[cfg(test)]
mod tests {
    use super::*;
    use ainb_hangar_core::ids::IssueId;

    fn issue(description: Option<&str>, assignee: Option<&str>) -> IssueRow {
        IssueRow {
            id: IssueId::from_str("i1").unwrap(),
            workspace_id: "acme".into(),
            title: "Refactor API".into(),
            description: description.map(String::from),
            state: "in_progress".into(),
            assignee: assignee.map(String::from),
            creator: "member:alice".into(),
            created_at: 0,
            pr_url: None,
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

    /// Status, assignee, and project always render.
    #[test]
    fn always_shown_rows_render() {
        let mut buf = WireBuffer::new(40, 8);
        render_sidebar(&mut buf, 0, 0, 8, 40, &issue(None, Some("agent:claude-agent")));
        assert!(row_text(&buf, 0, 40).contains("in_progress"));
        assert!(row_text(&buf, 1, 40).contains("agent:claude-agent"));
        assert!(row_text(&buf, 2, 40).contains("acme"));
    }

    /// An unset assignee renders as `unassigned`, not an empty value.
    #[test]
    fn unset_assignee_renders_unassigned() {
        let mut buf = WireBuffer::new(40, 8);
        render_sidebar(&mut buf, 0, 0, 8, 40, &issue(None, None));
        assert!(row_text(&buf, 1, 40).contains("unassigned"));
    }

    /// Progressive disclosure: a `None` description omits the Notes row entirely.
    #[test]
    fn absent_description_omits_notes_row() {
        let mut buf = WireBuffer::new(40, 8);
        render_sidebar(&mut buf, 0, 0, 8, 40, &issue(None, Some("member:bob")));
        // Row 3 (after status/assignee/project) must be empty — no Notes line.
        assert_eq!(row_text(&buf, 3, 40).trim(), "");
    }

    /// A present description renders the Notes row.
    #[test]
    fn present_description_renders_notes_row() {
        let mut buf = WireBuffer::new(40, 8);
        render_sidebar(&mut buf, 0, 0, 8, 40, &issue(Some("needs review"), Some("member:bob")));
        assert!(row_text(&buf, 3, 40).contains("needs review"));
    }
}
