//! P9.2 — task-detail PR badge render snapshots (insta inline).
//!
//! When the bound issue's row carries a `pr_url` (captured by P9.1, surfaced on
//! the wire by P9.2), the task-detail screen names it on the RUN HEAD's second
//! row alongside the run's branch: `ainb/…N9GP8 → main · PR #8 ✓` (crisp B4
//! §2.3). When `pr_url` is `None` there is NO chip at all — the snapshot delta
//! is a *removed* token, never a `PR: none` placeholder.
//!
//! The row replaced the full-URL badge (`▶ PR https://… CI …  [o] open`): the
//! URL cost 45 cells to say what `#8` says, on the one line that now has to
//! carry the branch as well, and `o` still opens it.
//!
//! Width-aware (60 / 100 / 160 cols): every segment is clipped by `chars()`
//! (never bytes — `reference_rust_utf8_truncate_trap`) so a narrow pane clips
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
        acceptance_criteria: Vec::new(),
        acceptance: Vec::new(),
        context_refs: Vec::new(),
        dependencies: Vec::new(),
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

/// The rendered badge row (row 0) only, `trim_end`-ed. These states carry no
/// runs, so the head's artifact row IS row 0 (the run card above it appears only
/// once the tasks snapshot gives the screen a run).
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

/// At 100 cols, a present PR URL paints the gold chip with its CI glyph.
#[test]
fn pr_badge_renders_at_100_cols() {
    let s = state(Some("https://example.com/pr/1"));
    let mut buf = WireBuffer::new(100, 8);
    render_task_detail(&mut buf, 100, 0, 8, &s, 0);
    insta::assert_snapshot!(badge_row(&buf, 100), @"PR #1 …");
}

/// At 160 cols, the same chip — the row does not grow with the pane.
#[test]
fn pr_badge_renders_at_160_cols() {
    let s = state(Some("https://example.com/pr/1"));
    let mut buf = WireBuffer::new(160, 8);
    render_task_detail(&mut buf, 160, 0, 8, &s, 0);
    insta::assert_snapshot!(badge_row(&buf, 160), @"PR #1 …");
}

/// At 60 cols (the narrow-pane threshold), the chip still paints in full: it is
/// six cells, so there is no width at which the PR goes missing.
#[test]
fn pr_badge_renders_at_60_cols() {
    let s = state(Some("https://example.com/pr/1"));
    let mut buf = WireBuffer::new(60, 8);
    render_task_detail(&mut buf, 60, 0, 8, &s, 0);
    insta::assert_snapshot!(badge_row(&buf, 60), @"PR #1 …");
}

/// A URL whose tail is not a number renders a bare `PR` rather than a fabricated
/// number, and a multi-byte tail is clipped on a char boundary (never a byte
/// split), so a narrow pane clips without panicking.
#[test]
fn pr_badge_truncates_long_url_on_char_boundary() {
    // A deliberately long URL with a multi-byte char in the tail.
    let s = state(Some(
        "https://example.com/pull/привет-very-long-branch-name-here-1234567890",
    ));
    let mut buf = WireBuffer::new(40, 8);
    // Should not panic; the row is clipped to the 40-col width.
    render_task_detail(&mut buf, 40, 0, 8, &s, 0);
    let line0 = badge_row(&buf, 40);
    assert!(line0.starts_with("PR "), "got: {line0}");
    assert!(
        !line0.contains("привет"),
        "a non-numeric tail is not a PR number: {line0}"
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
    render_task_detail(&mut green_buf, 120, 0, 8, &green, 0);
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
    render_task_detail(&mut red_buf, 120, 0, 8, &red, 0);
    let red_row = badge_row(&red_buf, 120);

    // The two badges read differently at the glyph level.
    assert_ne!(
        green_row, red_row,
        "a failing/conflicting PR must not render the same row as a passing one"
    );
    // Passing + mergeable reads a green check and stays quiet about the merge:
    // a clean PR beside an already-green CI tick needs no second tick.
    assert!(green_row.contains("PR #1 ✓"), "green row: {green_row:?}");
    assert!(!green_row.contains("CONFLICT"), "green row: {green_row:?}");
    // Failing + conflicting reads a red cross + a loud `CONFLICT`.
    assert!(red_row.contains("PR #1 ✗"), "red row: {red_row:?}");
    assert!(red_row.contains("✗ CONFLICT"), "red row: {red_row:?}");

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

/// e38.34 — the default (un-refreshed) status shows a muted `…` and no conflict
/// token, so the chip reads "status loading" rather than a false state until a
/// refresh answers.
#[test]
fn unknown_status_shows_muted_ci_and_no_mergeable() {
    let s = state_with_status("https://example.com/pr/1", PrStatus::default());
    let mut buf = WireBuffer::new(120, 8);
    render_task_detail(&mut buf, 120, 0, 8, &s, 0);
    let row = badge_row(&buf, 120);
    assert!(
        row.contains("PR #1 …"),
        "unknown CI shows a muted ellipsis: {row:?}"
    );
    assert!(
        !row.contains("CONFLICT"),
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
    states.open_task_detail(
        task,
        issue_with_pr(Some("https://example.com/pr/1")),
        None,
        None,
    );

    // Before the reply: muted unknown CI, no mergeable token.
    let before = states.task_detail.as_ref().unwrap();
    let mut buf = WireBuffer::new(120, 8);
    render_task_detail(&mut buf, 120, 0, 8, before, 0);
    let row = badge_row(&buf, 120);
    assert!(row.contains("PR #1 …"), "pre-refresh badge: {row:?}");
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
    render_task_detail(&mut buf, 120, 0, 8, after, 0);
    let row = badge_row(&buf, 120);
    assert!(row.contains("PR #1 ✗"), "post-refresh badge: {row:?}");
    assert!(row.contains("✗ CONFLICT"), "post-refresh badge: {row:?}");
}

/// No PR URL → NO badge row at all (the snapshot delta is a removed line, not a
/// `PR: none` placeholder).
#[test]
fn no_pr_url_renders_no_badge_row() {
    let s = state(None);
    let mut buf = WireBuffer::new(100, 8);
    render_task_detail(&mut buf, 100, 0, 8, &s, 0);
    let map = glyph_map(&buf, 100);
    assert!(
        !map.contains("PR "),
        "no-PR task must not render a PR badge: {map:?}"
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

/// agents-in-a-box-ch3 + crisp B4 §2.3: a run with a committed branch surfaces
/// it on the run head's artifact row, ahead of the PR chip and pointing at the
/// target it merges into, so the run's two durable artifacts read as one fact.
#[test]
fn branch_line_renders_beside_the_pr_badge() {
    let s = state_with_branch(Some("https://example.com/pr/1"), "ainb/refactor-api-a1b2c3");
    let mut buf = WireBuffer::new(100, 8);
    render_task_detail(&mut buf, 100, 0, 8, &s, 0);

    insta::assert_snapshot!(nth_row(&buf, 0, 100), @"ainb/refactor-api-a1b2c3 · PR #1 …");
}

/// A human branch is NOT elided (only the daemon's `ainb/<ulid>` run branch is),
/// and with no PR it still surfaces alone — a run that opened no PR but
/// committed a branch must still show it.
#[test]
fn branch_line_renders_alone_when_there_is_no_pr() {
    let s = state_with_branch(None, "ainb/hotfix-9f9f9f");
    let mut buf = WireBuffer::new(100, 8);
    render_task_detail(&mut buf, 100, 0, 8, &s, 0);
    assert_eq!(nth_row(&buf, 0, 100), "ainb/hotfix-9f9f9f");
}

/// The daemon's run branch is elided to the same `…<short id>` tail the Kanban
/// card shows, so one branch reads identically on both screens.
#[test]
fn a_run_branch_elides_exactly_as_the_kanban_card_does() {
    let s = state_with_branch(None, "ainb/01M1FKF4BDNQ3JK5CQSS3N9GP8");
    let mut buf = WireBuffer::new(100, 8);
    render_task_detail(&mut buf, 100, 0, 8, &s, 0);
    assert_eq!(nth_row(&buf, 0, 100), "ainb/…3N9GP8");
}

/// Progressive disclosure: a run with NO branch renders no branch text — never a
/// `branch: none` placeholder (the transcript occupies the row instead).
#[test]
fn no_branch_renders_no_branch_line() {
    let s = state(Some("https://example.com/pr/1"));
    let mut buf = WireBuffer::new(100, 8);
    render_task_detail(&mut buf, 100, 0, 8, &s, 0);
    let map = glyph_map(&buf, 100);
    assert!(
        !map.contains("ainb/"),
        "a branchless run must not render a branch: {map:?}"
    );
}
