//! P8.4 / 63l.6 — Kanban board render snapshots + status→column mapping.
//!
//! Seeds six tasks across the six lifecycle statuses, renders [`KanbanState`]
//! THROUGH the shared Linear-style card-board (63l.6) to a 120×30 backing buffer,
//! and pins the layout with `insta::assert_snapshot!` (trailing newline trimmed
//! per `reference_insta_trailing_newline_trap`). The snapshot proves the four
//! status columns carry their bucket counts and each task renders as a bordered
//! card showing `#<short_id>`, the agent + age + status, and a priority chip.
//! A non-vacuous colour check backs the heavy highlight border on the focused
//! card.

use ainb_hangar_proto::events::TaskCardRow;
use ainb_plugin_hangar::screen::kanban::{BoardColumn, KanbanState, render_kanban};
use ainb_plugin_sdk::{Color, WireBuffer};

/// Fixed render clock so the age labels are deterministic.
const NOW_MS: i64 = 1_700_000_600_000;
/// `CLAY` — the heavy highlight border colour of the focused card-board card.
const CLAY: Color = Color::rgb(210, 130, 90);

fn task(id: &str, agent: &str, status: &str, created_at: i64) -> TaskCardRow {
    TaskCardRow {
        id: ainb_hangar_core::ids::TaskId::from_str(id).unwrap(),
        workspace_id: "default".into(),
        agent_id: agent.into(),
        issue_id: Some("issue-1".into()),
        status: status.into(),
        priority: 0,
        created_at,
        branch: None,
        pr_url: None,
        pr_status: None,
    }
}

/// Six tasks: 2 queued (queued+dispatched), 1 running, 1 done, 2 failed
/// (failed+cancelled). Ages spread across minutes/hours/days from `NOW_MS`.
fn six_tasks() -> Vec<TaskCardRow> {
    vec![
        task(
            "01HANGARTASKQUEUED01",
            "claude-agent",
            "queued",
            NOW_MS - 300_000,
        ), // 5m
        task(
            "01HANGARTASKDISPTCH02",
            "gpt-agent",
            "dispatched",
            NOW_MS - 7_200_000,
        ), // 2h
        task(
            "01HANGARTASKRUNNING03",
            "claude-agent",
            "running",
            NOW_MS - 60_000,
        ), // 1m
        task(
            "01HANGARTASKDONE0004",
            "claude-agent",
            "done",
            NOW_MS - 259_200_000,
        ), // 3d
        task(
            "01HANGARTASKFAILED05",
            "gpt-agent",
            "failed",
            NOW_MS - 600_000,
        ), // 10m
        task(
            "01HANGARTASKCANCEL06",
            "claude-agent",
            "cancelled",
            NOW_MS - 3_600_000,
        ), // 1h
    ]
}

/// Flatten the buffer into a `\n`-joined glyph map, each line `trim_end`-ed and
/// the whole map trailing-newline-trimmed (insta trap).
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

/// The six statuses bucket into exactly the four board columns.
#[test]
fn status_mapping_covers_all_six_statuses() {
    assert_eq!(BoardColumn::for_status("queued"), BoardColumn::Queued);
    assert_eq!(BoardColumn::for_status("dispatched"), BoardColumn::Queued);
    assert_eq!(BoardColumn::for_status("running"), BoardColumn::Running);
    assert_eq!(BoardColumn::for_status("done"), BoardColumn::Done);
    assert_eq!(BoardColumn::for_status("failed"), BoardColumn::Failed);
    assert_eq!(BoardColumn::for_status("cancelled"), BoardColumn::Failed);
    // An unknown token is fail-visible (lands in queued), never dropped.
    assert_eq!(BoardColumn::for_status("weird-new"), BoardColumn::Queued);
}

/// The board buckets the six tasks into 2 / 1 / 1 / 2 across the four columns.
#[test]
fn board_buckets_counts() {
    let state = KanbanState::from_tasks(&six_tasks(), NOW_MS);
    let cols = state.columns();
    assert_eq!(cols[0].status, BoardColumn::Queued);
    assert_eq!(cols[0].cards.len(), 2, "queued+dispatched → 2");
    assert_eq!(cols[1].cards.len(), 1, "running → 1");
    assert_eq!(cols[2].cards.len(), 1, "done → 1");
    assert_eq!(cols[3].cards.len(), 2, "failed+cancelled → 2");
}

/// Full-board render snapshot at 120×30: four card-board headers with counts +
/// card fields, painted THROUGH the shared card-board (63l.6).
#[test]
fn render_full_board_snapshot() {
    let state = KanbanState::from_tasks(&six_tasks(), NOW_MS);
    let mut buf = WireBuffer::new(120, 30);
    render_kanban(&mut buf, 120, 0, 30, &state, NOW_MS);
    let full = glyph_map(&buf, 120);

    // Four column headers carry their status glyph + bucket count (card-board form).
    assert!(full.contains("queued (2)"), "queued header/count:\n{full}");
    assert!(
        full.contains("running (1)"),
        "running header/count:\n{full}"
    );
    assert!(full.contains("done (1)"), "done header/count:\n{full}");
    assert!(full.contains("failed (2)"), "failed header/count:\n{full}");

    // Card fields: `#<short_id>` id line, agent + age + status in the title.
    assert!(full.contains("#EUED01"), "queued short id:\n{full}");
    assert!(full.contains("#NING03"), "running short id:\n{full}");
    assert!(full.contains("claude-agent"), "assignee:\n{full}");
    assert!(full.contains("gpt-agent"), "assignee 2:\n{full}");
    // Age labels (5m queued, 1m running, 3d done, 10m failed, 1h cancelled).
    assert!(full.contains("5m"), "5m age:\n{full}");
    assert!(full.contains("3d"), "3d age:\n{full}");
    // The second card in the failed/cancelled bucket renders too (its short id),
    // proving both tasks bucket into the same column and both paint.
    assert!(full.contains("#NCEL06"), "cancelled short id:\n{full}");
    // The running status token renders intact in the card title.
    assert!(full.contains("running"), "running status:\n{full}");
    // The card-board paints bordered, rounded cards (the rounded corner glyph is
    // the card-board signature — the old band layout painted bare rows).
    assert!(full.contains('╭'), "rounded card borders:\n{full}");

    insta::assert_snapshot!(full);
}

/// The focused card carries the heavy clay highlight border (63l.6) — proof the
/// render goes through the shared card-board, which raises the focused tile.
#[test]
fn focused_card_has_heavy_clay_border() {
    let state = KanbanState::from_tasks(&six_tasks(), NOW_MS);
    let mut buf = WireBuffer::new(120, 30);
    render_kanban(&mut buf, 120, 0, 30, &state, NOW_MS);
    let heavy_in_clay =
        buf.cells.iter().any(|(_, cell)| cell.symbol == "┏" && cell.fg == Some(CLAY));
    assert!(
        heavy_in_clay,
        "the focused card must carry the heavy clay border (card-board highlight)"
    );
}
