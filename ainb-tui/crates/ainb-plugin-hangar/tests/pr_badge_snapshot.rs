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
use ainb_hangar_proto::pr_status::{CiRollup, MergeState, Mergeable, PrStatus};
use ainb_plugin_hangar::screen::ScreenStates;
use ainb_plugin_hangar::screen::task_detail::{TaskDetailState, render_task_detail};
use ainb_plugin_sdk::WireBuffer;

fn issue_with_pr(pr_url: Option<&str>) -> IssueRow {
    IssueRow {
        id: IssueId::from_str("i1").unwrap(),
        display_id: None,
        workspace_id: "ws".into(),
        title: "Refactor API".into(),
        description: None,
        state: "done".into(),
        assignee: Some("agent:claude-agent".into()),
        creator: "member:alice".into(),
        created_at: 0,
        priority: 0,
        due_date: None,
        labels: Vec::new(),
        pr_url: pr_url.map(String::from),
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
    }
}

fn state(pr_url: Option<&str>) -> TaskDetailState {
    TaskDetailState::new(TaskId::from_str("t1").unwrap(), issue_with_pr(pr_url))
}

/// A state bound to a PR with an explicit fetched [`PrStatus`] (e38.34).
fn state_with_status(pr_url: &str, status: PrStatus) -> TaskDetailState {
    let mut s = TaskDetailState::new(TaskId::from_str("t1").unwrap(), issue_with_pr(Some(pr_url)));
    s.set_pr_status(status);
    s
}

/// The badge row (row 0) as `(symbol, fg)` pairs, ordered by column, skipping
/// blank cells — so a test can assert both the glyphs AND their colours.
fn badge_cells(buf: &WireBuffer) -> Vec<(String, Option<ainb_plugin_sdk::Color>)> {
    let mut cells: Vec<(u16, String, Option<ainb_plugin_sdk::Color>)> = buf
        .cells
        .iter()
        .filter(|(coord, _)| coord.y == 0)
        .map(|(coord, cell)| (coord.x, cell.symbol.clone(), cell.fg))
        .collect();
    cells.sort_by_key(|(x, _, _)| *x);
    cells.into_iter().map(|(_, sym, fg)| (sym, fg)).collect()
}

/// The set of distinct foreground colours painted on the badge row.
fn badge_colors(buf: &WireBuffer) -> std::collections::HashSet<ainb_plugin_sdk::Color> {
    badge_cells(buf).into_iter().filter_map(|(_, fg)| fg).collect()
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
    insta::assert_snapshot!(badge_row(&buf, 100), @"▶ PR https://example.com/pr/1 CI …  [o] open");
}

/// At 160 cols, the full URL fits with the hint.
#[test]
fn pr_badge_renders_at_160_cols() {
    let s = state(Some("https://example.com/pr/1"));
    let mut buf = WireBuffer::new(160, 8);
    render_task_detail(&mut buf, 160, 0, 8, &s);
    insta::assert_snapshot!(badge_row(&buf, 160), @"▶ PR https://example.com/pr/1 CI …  [o] open");
}

/// At 60 cols (the narrow sidebar-collapse threshold), the badge still paints;
/// a long URL is clipped by `chars()` within the row.
#[test]
fn pr_badge_renders_at_60_cols() {
    let s = state(Some("https://example.com/pr/1"));
    let mut buf = WireBuffer::new(60, 8);
    render_task_detail(&mut buf, 60, 0, 8, &s);
    insta::assert_snapshot!(badge_row(&buf, 60), @"▶ PR https://example.com/pr/1 CI …  [o] open");
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

/// e38.34 — a failing + conflicting PR renders DISTINCTLY from a passing +
/// mergeable one: different glyphs (`CI ✗`/`CONFLICT` vs `CI ✓`/`mergeable`) AND
/// a red accent absent from the all-green badge. The mutation "badge ignores
/// status" (both render identically) flips these assertions red.
#[test]
fn failing_conflict_renders_distinctly_from_passing_mergeable() {
    let url = "https://example.com/pr/1";

    let green = state_with_status(
        url,
        PrStatus {
            ci: CiRollup::Pass,
            mergeable: Mergeable::Mergeable,
            state: MergeState::Open,
        },
    );
    let mut green_buf = WireBuffer::new(120, 8);
    render_task_detail(&mut green_buf, 120, 0, 8, &green);
    let green_row = badge_row(&green_buf, 120);

    let red = state_with_status(
        url,
        PrStatus {
            ci: CiRollup::Fail,
            mergeable: Mergeable::Conflicting,
            state: MergeState::Open,
        },
    );
    let mut red_buf = WireBuffer::new(120, 8);
    render_task_detail(&mut red_buf, 120, 0, 8, &red);
    let red_row = badge_row(&red_buf, 120);

    // The two badges read differently at the glyph level.
    assert_ne!(
        green_row, red_row,
        "a failing/conflicting PR must not render the same row as a passing one"
    );
    // Passing + mergeable reads green check + the word `mergeable`, no CONFLICT.
    assert!(green_row.contains("CI ✓"), "green row: {green_row:?}");
    assert!(
        green_row.contains("✓ mergeable"),
        "green row: {green_row:?}"
    );
    assert!(!green_row.contains("CONFLICT"), "green row: {green_row:?}");
    // Failing + conflicting reads a red cross + a loud `CONFLICT`, no mergeable.
    assert!(red_row.contains("CI ✗"), "red row: {red_row:?}");
    assert!(red_row.contains("✗ CONFLICT"), "red row: {red_row:?}");
    assert!(!red_row.contains("mergeable"), "red row: {red_row:?}");

    // And the colours differ: the failing badge paints a red accent the all-green
    // badge never uses (a colour-blind glyph delta alone would not prove this).
    let red_accent = ainb_plugin_sdk::Color::rgb(240, 100, 100);
    assert!(
        badge_colors(&red_buf).contains(&red_accent),
        "the failing/conflicting badge must paint a red accent"
    );
    assert!(
        !badge_colors(&green_buf).contains(&red_accent),
        "the passing/mergeable badge must never paint the failure-red accent"
    );
}

/// e38.34 — the default (un-refreshed) status shows a muted `CI …` and no
/// mergeable token, so the badge reads "status loading" rather than a false
/// state until a refresh answers.
#[test]
fn unknown_status_shows_muted_ci_and_no_mergeable() {
    let s = state_with_status("https://example.com/pr/1", PrStatus::default());
    let mut buf = WireBuffer::new(120, 8);
    render_task_detail(&mut buf, 120, 0, 8, &s);
    let row = badge_row(&buf, 120);
    assert!(
        row.contains("CI …"),
        "unknown CI shows a muted ellipsis: {row:?}"
    );
    assert!(
        !row.contains("mergeable") && !row.contains("CONFLICT"),
        "an unknown mergeable paints no token: {row:?}"
    );
}

/// e38.34 — applying a fetched status to the OPEN task-detail (the reply path,
/// `ScreenStates::set_task_detail_pr_status`) surfaces on the rendered badge: a
/// freshly-opened task-detail starts at the muted `CI …`, and after the refresh
/// reply lands the badge reads the new CI + CONFLICT state.
#[test]
fn applying_refresh_reply_updates_the_open_badge() {
    let task = TaskId::from_str("task-1").unwrap();
    let mut states = ScreenStates::default();
    states.open_task_detail(task, issue_with_pr(Some("https://example.com/pr/1")), None);

    // Before the reply: muted unknown CI, no mergeable token.
    let before = states.task_detail.as_ref().unwrap();
    let mut buf = WireBuffer::new(120, 8);
    render_task_detail(&mut buf, 120, 0, 8, before);
    let row = badge_row(&buf, 120);
    assert!(row.contains("CI …"), "pre-refresh badge: {row:?}");
    assert!(!row.contains("CONFLICT"), "pre-refresh badge: {row:?}");

    // Apply the reply (the daemon answered a failing, conflicting PR).
    states.set_task_detail_pr_status(PrStatus {
        ci: CiRollup::Fail,
        mergeable: Mergeable::Conflicting,
        state: MergeState::Open,
    });

    // After the reply: the badge reflects the fetched state.
    let after = states.task_detail.as_ref().unwrap();
    let mut buf = WireBuffer::new(120, 8);
    render_task_detail(&mut buf, 120, 0, 8, after);
    let row = badge_row(&buf, 120);
    assert!(row.contains("CI ✗"), "post-refresh badge: {row:?}");
    assert!(row.contains("✗ CONFLICT"), "post-refresh badge: {row:?}");
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

/// The single glyph row `y` of the buffer, `trim_end`-ed (multi-byte safe).
fn nth_row(buf: &WireBuffer, y: u16, cols: u16) -> String {
    let mut row = vec![' '; cols as usize];
    for (coord, cell) in &buf.cells {
        if coord.y == y && coord.x < cols {
            if let Some(ch) = cell.symbol.chars().next() {
                row[coord.x as usize] = ch;
            }
        }
    }
    row.into_iter().collect::<String>().trim_end().to_string()
}

/// A task-detail state carrying the run's branch (agents-in-a-box-ch3).
fn state_with_branch(pr_url: Option<&str>, branch: &str) -> TaskDetailState {
    let mut s = TaskDetailState::new(TaskId::from_str("t1").unwrap(), issue_with_pr(pr_url));
    s.set_branch(Some(branch.to_string()));
    s
}

/// agents-in-a-box-ch3: a run with a committed branch surfaces it in the detail
/// view on its OWN line right under the PR badge — `⎇ branch ainb/<slug>` — so the
/// durable artifact reads in the detail exactly as it does on the Kanban card.
#[test]
fn branch_line_renders_under_the_pr_badge() {
    let s = state_with_branch(Some("https://example.com/pr/1"), "ainb/refactor-api-a1b2c3");
    let mut buf = WireBuffer::new(100, 8);
    render_task_detail(&mut buf, 100, 0, 8, &s);

    // Row 0 is the PR badge; row 1 is the branch line right beneath it.
    insta::assert_snapshot!(nth_row(&buf, 0, 100), @"▶ PR https://example.com/pr/1 CI …  [o] open");
    insta::assert_snapshot!(nth_row(&buf, 1, 100), @"⎇ branch ainb/refactor-api-a1b2c3");
}

/// With no PR badge, the branch line still surfaces — at the TOP row (the layout
/// shifts up), so a run that opened no PR but committed a branch still shows it.
#[test]
fn branch_line_renders_at_top_when_no_pr_badge() {
    let s = state_with_branch(None, "ainb/hotfix-9f9f9f");
    let mut buf = WireBuffer::new(100, 8);
    render_task_detail(&mut buf, 100, 0, 8, &s);
    assert_eq!(nth_row(&buf, 0, 100), "⎇ branch ainb/hotfix-9f9f9f");
}

/// Progressive disclosure: a run with NO branch renders no branch line — never a
/// `branch: none` placeholder (the transcript occupies the row instead).
#[test]
fn no_branch_renders_no_branch_line() {
    let s = state(Some("https://example.com/pr/1"));
    let mut buf = WireBuffer::new(100, 8);
    render_task_detail(&mut buf, 100, 0, 8, &s);
    let map = glyph_map(&buf, 100);
    assert!(
        !map.contains("⎇ branch"),
        "a branchless run must not render a branch line: {map:?}"
    );
}
