//! Crisp B4 §2.3 — the EXECUTION VIEW, pinned as a whole pane.
//!
//! The one screen the renovation is for: open a ticket, see the run. These
//! snapshots pin the whole 100×24 pane rather than a row at a time, because the
//! defect they exist to catch is a LAYOUT one — the audit's finding was six
//! header rows of `unassigned` above 80% empty screen, and no single assertion
//! would have caught that.
//!
//! Snapshots are a glyph map of the rendered [`WireBuffer`] (one line per row,
//! unwritten cells are spaces), `trim_end`-ed per line so the golden carries no
//! trailing whitespace (`reference_insta_trailing_newline_trap`).

use ainb_hangar_core::ids::{IssueId, TaskId};
use ainb_hangar_proto::events::{HangarEvent, IssueRow, MessageKind};
use ainb_hangar_proto::pr_status::{CiRollup, MergeState, Mergeable, PrStatus};
use ainb_plugin_hangar::screen::task_detail::{
    ActivityRow, RunRow, TaskDetailEvent, TaskDetailState, reduce_task_detail, render_task_detail,
};
use ainb_plugin_hangar::vocab::RunState;
use ainb_plugin_sdk::WireBuffer;

/// 2026-09-02T09:00:00Z, the render clock every snapshot ticks against.
const NOW: i64 = 1_788_339_600_000;
const MINUTE: i64 = 60_000;

fn issue_row() -> IssueRow {
    IssueRow {
        subscriber_count: 0,
        subscribed: false,
        reactions: Vec::new(),
        properties: Vec::new(),
        metadata: Vec::new(),
        last_dispatch_reason: None,
        last_dispatch_detail: None,
        last_dispatch_at: None,
        origin_type: None,
        origin_id: None,
        id: IssueId::from_str("i1").unwrap(),
        display_id: Some("HGR-5".into()),
        workspace_id: "01M1FH6AG5YJF1SWORKSPACE00".into(),
        title: "Ticket stats: GET /api/tickets/stats".into(),
        description: Some(
            "Add GET /api/tickets/stats to the hono api under api/: returns {total, open, closed}."
                .into(),
        ),
        state: "in_progress".into(),
        assignee: Some("agent:01M1FHM2YSRSXZQFR29ZAYF56V".into()),
        creator: "member:me".into(),
        created_at: 1_788_249_600_000, // 2026-09-01
        priority: 1,
        due_date: None,
        labels: Vec::new(),
        pr_url: None,
        branch: None,
        repo_ref: Some("/home/claude/ainb-e2e-home/projects/boxtrack".into()),
        agent: Some("claude".into()),
        source_branch: Some("main".into()),
        target_branch: Some("main".into()),
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

fn run(task_id: &str, agent: &str, state: RunState, started_min_ago: i64) -> RunRow {
    RunRow {
        task_id: task_id.into(),
        short_id: task_id.chars().rev().take(6).collect::<String>(),
        agent: agent.into(),
        state,
        started_at: NOW - started_min_ago * MINUTE,
        finished_at: None,
        cost_cents: None,
        branch: None,
        pr_url: None,
        pr_status: None,
    }
}

/// A screen bound to `task_id`, with `lines` already in its transcript.
fn state_with(task_id: &str, lines: &[(MessageKind, &str)]) -> TaskDetailState {
    let mut s = TaskDetailState::new(TaskId::from_str(task_id).unwrap(), issue_row());
    for (kind, body) in lines {
        s = reduce_task_detail(
            &s,
            TaskDetailEvent::Event(HangarEvent::TaskMessage {
                task_id: TaskId::from_str(task_id).unwrap(),
                kind: *kind,
                body: (*body).into(),
            }),
        )
        .state;
    }
    s
}

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

/// The headline shot: a LIVE run, its transcript streaming under it, the older
/// attempts listed above it, and the issue's narrative beside them.
#[test]
fn live_run_paints_the_whole_execution_view() {
    let mut s = state_with(
        "01M1GVN6MAF3121GEDM1E66KW5",
        &[
            (MessageKind::Thinking, "reading api/src/db.ts"),
            (MessageKind::ToolCall, "Edit api/src/routes.ts"),
            (MessageKind::ToolResult, "3 files changed"),
            (
                MessageKind::Agent,
                "Registering the route before the 404 handler",
            ),
        ],
    );
    s.set_resolved_names(Some("impl-1".into()), Some("impl-1".into()), None);

    let mut live = run("01M1GVN6MAF3121GEDM1E66KW5", "impl-1", RunState::Running, 7);
    live.started_at = NOW - 7 * MINUTE - 17_000;
    live.branch = Some("ainb/01M1FKF4BDNQ3JK5CQSS3N9GP8".into());
    live.pr_url = Some("https://github.com/acme/boxtrack/pull/8".into());
    live.pr_status = Some(PrStatus {
        ci: CiRollup::Pass,
        mergeable: Mergeable::Mergeable,
        state: MergeState::Open,
    });
    live.cost_cents = Some(42);

    let mut failed = run("01M1GVN6MAF3121GEDM1E66KW6", "rev-1", RunState::Failed, 9);
    failed.finished_at = Some(NOW - 4 * MINUTE);
    failed.cost_cents = Some(11);
    let mut done = run("01M1GVN6MAF3121GEDM1E66KW7", "impl-1", RunState::Done, 21);
    done.finished_at = Some(NOW - 18 * MINUTE);

    s.set_runs(vec![done, failed, live]);
    s.set_activity(vec![
        ActivityRow {
            at_ms: NOW - 7 * MINUTE,
            text: "impl-1 claimed the issue".into(),
        },
        ActivityRow {
            at_ms: NOW - 6 * MINUTE,
            text: "you moved todo → in progress".into(),
        },
    ]);

    let mut buf = WireBuffer::new(100, 24);
    render_task_detail(&mut buf, 100, 0, 24, &s, NOW);
    insta::assert_snapshot!(glyph_map(&buf, 100));
}

/// A never-run issue: no run card, no execution log, no activity column — the
/// card, then an empty transcript. Nothing on this screen claims a run that is
/// not there.
#[test]
fn a_never_run_issue_paints_no_run_card_and_no_log() {
    let mut s = state_with("task-i1", &[]);
    s.set_resolved_names(Some("impl-1".into()), None, None);
    let mut buf = WireBuffer::new(100, 24);
    render_task_detail(&mut buf, 100, 0, 24, &s, NOW);
    insta::assert_snapshot!(glyph_map(&buf, 100));
}

/// A running run whose transcript is still empty says so. This is the state
/// that can mislead: the run card's elapsed reads the render clock, so it ticks
/// whether or not anything is streaming, and an advancing number over a blank
/// pane reads as evidence that something is happening. A finished run that left
/// no transcript says a different, equally factual thing.
#[test]
fn an_empty_transcript_says_why_rather_than_ticking_over_a_blank_pane() {
    let mut s = state_with("01M1GVN6MAF3121GEDM1E66KW5", &[]);
    s.set_runs(vec![run(
        "01M1GVN6MAF3121GEDM1E66KW5",
        "impl-1",
        RunState::Running,
        7,
    )]);
    let mut buf = WireBuffer::new(100, 24);
    render_task_detail(&mut buf, 100, 0, 24, &s, NOW);
    let live = glyph_map(&buf, 100);
    assert!(
        live.contains("◔ impl-1 is working · 7m 00s"),
        "the clock ticks:\n{live}"
    );
    assert!(
        live.contains("waiting for the first line of this run"),
        "and the empty pane says why:\n{live}"
    );

    // The same run, finished with nothing recorded: a different, still factual
    // line — never the waiting one, which would now be a lie.
    let mut done = run("01M1GVN6MAF3121GEDM1E66KW5", "impl-1", RunState::Done, 7);
    done.finished_at = Some(NOW - 2 * MINUTE);
    s.set_runs(vec![done]);
    let mut buf = WireBuffer::new(100, 24);
    render_task_detail(&mut buf, 100, 0, 24, &s, NOW);
    let terminal = glyph_map(&buf, 100);
    assert!(
        terminal.contains("this run recorded no transcript"),
        "a finished run with no log says so:\n{terminal}"
    );
    assert!(
        !terminal.contains("waiting for"),
        "and never claims it is still coming:\n{terminal}"
    );
}

/// `enter` expands the NEXT run. The card, the `▶` marker and the transcript
/// pane move together — and the pane says why it is empty for a run whose
/// transcript the durable read cannot serve.
#[test]
fn enter_expands_the_next_run_and_the_pane_follows() {
    let mut s = state_with(
        "01M1GVN6MAF3121GEDM1E66KW5",
        &[(MessageKind::Agent, "Registering the route")],
    );
    let live = run("01M1GVN6MAF3121GEDM1E66KW5", "impl-1", RunState::Running, 2);
    let mut failed = run("01M1GVN6MAF3121GEDM1E66KW6", "rev-1", RunState::Failed, 9);
    failed.finished_at = Some(NOW - 4 * MINUTE);
    s.set_runs(vec![live, failed]);

    // Opens on the bound (live) run: its transcript is the one on screen.
    assert_eq!(s.expanded_run().map(|r| r.agent.as_str()), Some("impl-1"));
    assert!(s.transcript_is_for_expanded_run());

    let s = reduce_task_detail(&s, TaskDetailEvent::Key('\r')).state;
    assert_eq!(s.expanded_run().map(|r| r.agent.as_str()), Some("rev-1"));
    assert!(
        !s.transcript_is_for_expanded_run(),
        "the transcript on screen belongs to the OTHER run"
    );

    let mut buf = WireBuffer::new(100, 24);
    render_task_detail(&mut buf, 100, 0, 24, &s, NOW);
    insta::assert_snapshot!(glyph_map(&buf, 100));
}
