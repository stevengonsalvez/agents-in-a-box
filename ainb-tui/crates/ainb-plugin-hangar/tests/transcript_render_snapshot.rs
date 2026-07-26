//! P4.4 RED — transcript render snapshots (insta inline).
//!
//! The transcript renderer paints the 5-colour taxonomy (UX §7 verbatim): each
//! [`MessageKind`] gets one glyph + colour lane. These snapshots pin the visible
//! glyph column + body so the taxonomy stays exactly five distinct lanes in
//! order, and that a long thinking run collapses to a single fold marker.
//!
//! Snapshots are built from a *glyph map* of the rendered [`WireBuffer`] (one
//! line per row, unwritten cells are spaces) and `trim_end`-ed per line so the
//! golden carries no trailing whitespace (`reference_insta_trailing_newline_trap`
//! — we never compare a raw buffer dump with a maybe-stripped newline).

use ainb_hangar_core::ids::{IssueId, TaskId};
use ainb_hangar_proto::events::{HangarEvent, IssueRow, MessageKind};
use ainb_plugin_hangar::screen::task_detail::{
    TaskDetailEvent, TaskDetailState, reduce_task_detail, render_task_detail,
};
use ainb_plugin_sdk::WireBuffer;

fn issue_row() -> IssueRow {
    IssueRow {
        origin_type: None,
        origin_id: None,
        id: IssueId::from_str("i1").unwrap(),
        display_id: None,
        workspace_id: "ws".into(),
        title: "Refactor API".into(),
        description: None,
        state: "in_progress".into(),
        assignee: Some("agent:claude-agent".into()),
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

fn seed(events: &[(MessageKind, &str)]) -> TaskDetailState {
    let mut s = TaskDetailState::new(TaskId::from_str("t1").unwrap(), issue_row());
    for (kind, body) in events {
        s = reduce_task_detail(
            &s,
            TaskDetailEvent::Event(HangarEvent::TaskMessage {
                task_id: TaskId::from_str("t1").unwrap(),
                kind: *kind,
                body: (*body).into(),
            }),
        )
        .state;
    }
    s
}

/// Reconstruct the rendered glyph column (first `cols` columns) per row as
/// trimmed lines, joined with `\n`. Deterministic — reads the sparse buffer in
/// row/col order.
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

/// All 5 taxonomy lanes render, one per row, glyphs in taxonomy order:
/// `▌` agent, `*` thinking, `→` tool call, `←` tool result, `!` error.
#[test]
fn transcript_renders_all_5_colors_in_order() {
    let s = seed(&[
        (MessageKind::Agent, "writing code"),
        (MessageKind::Thinking, "considering edge cases"),
        (MessageKind::ToolCall, "read file"),
        (MessageKind::ToolResult, "ok 200 lines"),
        (MessageKind::Error, "boom"),
    ]);
    let mut buf = WireBuffer::new(40, 8);
    render_task_detail(&mut buf, 40, 0, 8, &s);
    insta::assert_snapshot!(glyph_map(&buf, 40), @r###"
    ▌ writing code
    * considering edge cases
    → read file
    ← ok 200 lines
    ! boom
    "###);
}

/// A long consecutive thinking run collapses to a single fold marker rather than
/// rendering every line.
#[test]
fn transcript_renders_collapsed_old_history() {
    let mut events: Vec<(MessageKind, &str)> = vec![(MessageKind::Agent, "start")];
    let thinks: Vec<String> = (0..12).map(|i| format!("think {i}")).collect();
    for t in &thinks {
        events.push((MessageKind::Thinking, t));
    }
    events.push((MessageKind::ToolCall, "run"));
    let s = seed(&events);

    let mut buf = WireBuffer::new(40, 8);
    render_task_detail(&mut buf, 40, 0, 8, &s);
    insta::assert_snapshot!(glyph_map(&buf, 40), @r###"
    ▌ start
    * … 12 thinking lines (t to expand)
    → run
    "###);
}
