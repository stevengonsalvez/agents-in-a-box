//! P9.2 RED — task-detail PR badge render snapshots (insta inline).
//!
//! When the bound issue's row carries a `pr_url` (captured by P9.1, surfaced on
//! the wire by P9.2), the task-detail screen paints a single gold badge row
//! `▶ PR <url>` with a muted `[o] open` hint next to it (keybinding-hint-near-
//! control). When `pr_url` is `None` there is NO badge row at all — the snapshot
//! delta is a *removed* line, never a `PR: none` placeholder.
//!
//! Width-aware (60 / 100 / 160 cols): the URL is truncated by `chars()` (never
//! bytes — `reference_rust_utf8_truncate_trap`) so a narrow pane clips the URL
//! without panicking on a multi-byte boundary.
//!
//! Snapshots are built from a glyph map of the rendered [`WireBuffer`] (one line
//! per row, unwritten cells are spaces), each line `trim_end`-ed so the golden
//! carries no trailing whitespace.

use ainb_hangar_core::ids::{IssueId, TaskId};
use ainb_hangar_proto::events::IssueRow;
use ainb_plugin_hangar::screen::task_detail::{render_task_detail, TaskDetailState};
use ainb_plugin_sdk::WireBuffer;

fn issue_with_pr(pr_url: Option<&str>) -> IssueRow {
    IssueRow {
        id: IssueId::from_str("i1").unwrap(),
        workspace_id: "ws".into(),
        title: "Refactor API".into(),
        description: None,
        state: "done".into(),
        assignee: Some("agent:claude-agent".into()),
        creator: "member:alice".into(),
        created_at: 0,
        pr_url: pr_url.map(String::from),
    }
}

fn state(pr_url: Option<&str>) -> TaskDetailState {
    TaskDetailState::new(TaskId::from_str("t1").unwrap(), issue_with_pr(pr_url))
}

/// Reconstruct the rendered glyph column (first `cols` columns) per row as
/// trimmed lines, joined with `\n`.
fn glyph_map(buf: &WireBuffer, cols: u16) -> String {
    let mut grid = vec![vec![' '; cols as usize]; buf.height as usize];
    for (coord, cell) in &buf.cells {
        if coord.y < buf.height && coord.x < cols {
            if let Some(ch) = cell.symbol.chars().next() {
                grid[coord.y as usize][coord.x as usize] = ch;
            }
        }
    }
    grid.into_iter()
        .map(|r| r.into_iter().collect::<String>().trim_end().to_string())
        .collect::<Vec<_>>()
        .join("\n")
        .trim_end_matches('\n')
        .to_string()
}

/// The rendered badge row (row 0) only, `trim_end`-ed. The sidebar paints below
/// the badge at a width-dependent column offset, so the badge-shape goldens pin
/// only the badge row (the no-badge case asserts the whole map separately).
fn badge_row(buf: &WireBuffer, cols: u16) -> String {
    let mut row = vec![' '; cols as usize];
    for (coord, cell) in &buf.cells {
        if coord.y == 0 && coord.x < cols {
            if let Some(ch) = cell.symbol.chars().next() {
                row[coord.x as usize] = ch;
            }
        }
    }
    row.into_iter().collect::<String>().trim_end().to_string()
}

/// At 100 cols, a present PR URL paints the gold badge row + the `[o] open`
/// hint next to it.
#[test]
fn pr_badge_renders_at_100_cols() {
    let s = state(Some("https://example.com/pr/1"));
    let mut buf = WireBuffer::new(100, 8);
    render_task_detail(&mut buf, 100, 0, 8, &s);
    insta::assert_snapshot!(badge_row(&buf, 100), @"▶ PR https://example.com/pr/1  [o] open");
}

/// At 160 cols, the full URL fits with the hint.
#[test]
fn pr_badge_renders_at_160_cols() {
    let s = state(Some("https://example.com/pr/1"));
    let mut buf = WireBuffer::new(160, 8);
    render_task_detail(&mut buf, 160, 0, 8, &s);
    insta::assert_snapshot!(badge_row(&buf, 160), @"▶ PR https://example.com/pr/1  [o] open");
}

/// At 60 cols (the narrow sidebar-collapse threshold), the badge still paints;
/// a long URL is clipped by `chars()` within the row.
#[test]
fn pr_badge_renders_at_60_cols() {
    let s = state(Some("https://example.com/pr/1"));
    let mut buf = WireBuffer::new(60, 8);
    render_task_detail(&mut buf, 60, 0, 8, &s);
    insta::assert_snapshot!(badge_row(&buf, 60), @"▶ PR https://example.com/pr/1  [o] open");
}

/// A multi-byte URL is truncated on a char boundary (never a byte split), so a
/// narrow pane clips without panicking.
#[test]
fn pr_badge_truncates_long_url_on_char_boundary() {
    // A deliberately long URL with a multi-byte char near the clip point.
    let s = state(Some(
        "https://example.com/pull/привет-very-long-branch-name-here-1234567890",
    ));
    let mut buf = WireBuffer::new(40, 8);
    // Should not panic; the row is clipped to the 40-col width.
    render_task_detail(&mut buf, 40, 0, 8, &s);
    let line0 = badge_row(&buf, 40);
    assert!(
        line0.starts_with("▶ PR https://example.com/pull/"),
        "got: {line0}"
    );
    // Clipped to the pane width by chars (≤ 40 glyphs), never split mid-codepoint.
    assert!(
        line0.chars().count() <= 40,
        "badge row exceeded pane width: {line0}"
    );
}

/// No PR URL → NO badge row at all (the snapshot delta is a removed line, not a
/// `PR: none` placeholder).
#[test]
fn no_pr_url_renders_no_badge_row() {
    let s = state(None);
    let mut buf = WireBuffer::new(100, 8);
    render_task_detail(&mut buf, 100, 0, 8, &s);
    let map = glyph_map(&buf, 100);
    assert!(
        !map.contains("PR "),
        "no-PR task must not render a PR badge: {map:?}"
    );
    assert!(
        !map.contains("[o] open"),
        "no-PR task must not render the open hint: {map:?}"
    );
}
