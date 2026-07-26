//! Task-detail right-hand sidebar — progressive disclosure (reference UX §12.8).
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

use ainb_hangar_proto::events::IssueRow;
use ainb_plugin_sdk::{Cell, Color, Coord, WireBuffer};

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
    if !issue.labels.is_empty() {
        row = put_labels(buf, x, row, bottom, width, &issue.labels);
    }
    if let Some(desc) = issue.description.as_deref() {
        let _ = put_field(buf, x, row, bottom, width, "Notes", desc);
    }
}

/// Paint a `Labels: ‹bug› ‹p0›` row at `(x, row)`: the `Labels` label in
/// cornflower-blue followed by the violet chip run, clipped at `x + width`.
/// Returns the next free row (a no-op returning `row` when `row >= bottom`).
fn put_labels(
    buf: &mut WireBuffer,
    x: u16,
    row: u16,
    bottom: u16,
    width: u16,
    labels: &[String],
) -> u16 {
    if row >= bottom {
        return row;
    }
    let right = x.saturating_add(width);
    let mut cx = put_str(buf, x, row, "Labels", LABEL, right);
    cx = put_str(buf, cx, row, ": ", LABEL, right);
    let _ = crate::widgets::label_chip::render_label_chips(buf, cx, row, right, labels);
    row.saturating_add(1)
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
            display_id: None,
            workspace_id: "acme".into(),
            title: "Refactor API".into(),
            description: description.map(String::from),
            state: "in_progress".into(),
            assignee: assignee.map(String::from),
            creator: "member:alice".into(),
            created_at: 0,
            priority: 0,
            due_date: None,
            labels: Vec::new(),
            pr_url: None,
            branch: None,
            repo_ref: None,
            agent: None,
            source_branch: None,
            target_branch: None,
            external_ref: None,
            run_count: 0,
            last_run_status: None,
            last_run_at: None,
            parent_id: None,
            child_total: 0,
            child_done: 0,
            acceptance_criteria: Vec::new(),
            acceptance: Vec::new(),
            context_refs: Vec::new(),
            dependencies: Vec::new(),
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
        render_sidebar(
            &mut buf,
            0,
            0,
            8,
            40,
            &issue(None, Some("agent:claude-agent")),
        );
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

    /// The Assignee row surfaces the polymorphic actor KIND: a member assignee
    /// paints its `member:` prefix, distinct from the `agent:` an agent shows
    /// (agents-are-team-members symmetry, made visible in the TUI). Mutation-
    /// provable: strip the kind prefix from the snapshot ref and this goes red.
    #[test]
    fn assignee_row_distinguishes_member_from_agent_kind() {
        let mut member_buf = WireBuffer::new(48, 8);
        render_sidebar(
            &mut member_buf,
            0,
            0,
            8,
            48,
            &issue(None, Some("member:dana")),
        );
        let member_row = row_text(&member_buf, 1, 48);
        assert!(
            member_row.contains("member:dana"),
            "member kind shown: {member_row}"
        );
        assert!(!member_row.contains("agent:"), "not painted as an agent");

        let mut agent_buf = WireBuffer::new(48, 8);
        render_sidebar(
            &mut agent_buf,
            0,
            0,
            8,
            48,
            &issue(None, Some("agent:claude")),
        );
        let agent_row = row_text(&agent_buf, 1, 48);
        assert!(
            agent_row.contains("agent:claude"),
            "agent kind shown: {agent_row}"
        );
        assert!(!agent_row.contains("member:"), "not painted as a member");
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
        render_sidebar(
            &mut buf,
            0,
            0,
            8,
            40,
            &issue(Some("needs review"), Some("member:bob")),
        );
        assert!(row_text(&buf, 3, 40).contains("needs review"));
    }

    /// Progressive disclosure: a labelled issue renders a `Labels:` chip row
    /// after Project, and a label-less issue omits it (e38.10).
    #[test]
    fn labels_row_renders_when_present_and_omits_when_empty() {
        // Label-less: no Labels row at index 3 (it would be Notes / empty).
        let mut buf = WireBuffer::new(40, 8);
        render_sidebar(&mut buf, 0, 0, 8, 40, &issue(None, Some("member:bob")));
        assert!(
            !row_text(&buf, 3, 40).contains("Labels"),
            "label-less issue must omit the Labels row"
        );

        // Labelled: the Labels row renders with the chip after Project (row 3).
        let mut labelled = issue(None, Some("member:bob"));
        labelled.labels = vec!["bug".into(), "p0".into()];
        let mut buf = WireBuffer::new(40, 8);
        render_sidebar(&mut buf, 0, 0, 8, 40, &labelled);
        let line = row_text(&buf, 3, 40);
        assert!(line.contains("Labels"), "Labels row label: {line:?}");
        assert!(line.contains("‹bug›"), "bug chip: {line:?}");
        assert!(line.contains("‹p0›"), "p0 chip: {line:?}");
    }
}
