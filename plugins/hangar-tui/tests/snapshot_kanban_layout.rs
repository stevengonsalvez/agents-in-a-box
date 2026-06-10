//! P8.4 — Kanban board render snapshots + status→column mapping.
//!
//! Seeds six tasks across the six lifecycle statuses, renders [`KanbanState`] to a
//! 120×30 backing buffer, and pins the layout with `insta::assert_snapshot!`
//! (trailing newline trimmed per `reference_insta_trailing_newline_trap`). The
//! snapshot proves the four column headers carry their bucket counts and each
//! card shows `#<short_id>`, the assignee, an age label, and a status chip.
//! Non-vacuous colour checks back the selection marker + the status-chip colours.

use ainb_hangar_proto::events::TaskCardRow;
use ainb_plugin_hangar::screen::kanban::{render_kanban, BoardColumn, KanbanState};
use ainb_plugin_sdk::{Color, WireBuffer};

/// Fixed render clock so the age labels are deterministic.
const NOW_MS: i64 = 1_700_000_600_000;
/// `SELECTION_GREEN` — the focused marker / done chip colour.
const SELECTION_GREEN: Color = Color::rgb(100, 200, 100);
/// `RUNNING_BLUE` — the running chip colour.
const RUNNING_BLUE: Color = Color::rgb(100, 149, 237);
/// `WARN_RED` — the failed/cancelled chip colour.
const WARN_RED: Color = Color::rgb(220, 120, 100);

fn task(id: &str, agent: &str, status: &str, created_at: i64) -> TaskCardRow {
    TaskCardRow {
        id: ainb_hangar_core::ids::TaskId::from_str(id).unwrap(),
        workspace_id: "default".into(),
        agent_id: agent.into(),
        issue_id: Some("issue-1".into()),
        status: status.into(),
        priority: 0,
        created_at,
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

/// Full-board render snapshot at 120×30: four headers with counts + card fields.
#[test]
fn render_full_board_snapshot() {
    let state = KanbanState::from_tasks(&six_tasks(), NOW_MS);
    let mut buf = WireBuffer::new(120, 30);
    render_kanban(&mut buf, 120, 0, 30, &state, NOW_MS);
    let full = glyph_map(&buf, 120);

    // Four column headers carry their bucket counts.
    assert!(full.contains("queued (2)"), "queued header/count:\n{full}");
    assert!(
        full.contains("running (1)"),
        "running header/count:\n{full}"
    );
    assert!(full.contains("done (1)"), "done header/count:\n{full}");
    assert!(full.contains("failed (2)"), "failed header/count:\n{full}");

    // Card fields: short ids (last 6 chars), assignee, age, status chip.
    assert!(full.contains("#EUED01"), "queued short id:\n{full}");
    assert!(full.contains("#NING03"), "running short id:\n{full}");
    assert!(full.contains("claude-agent"), "assignee:\n{full}");
    assert!(full.contains("gpt-agent"), "assignee 2:\n{full}");
    // Age labels (5m queued, 1m running, 3d done, 10m failed, 1h cancelled).
    assert!(full.contains("5m"), "5m age:\n{full}");
    assert!(full.contains("3d"), "3d age:\n{full}");
    // Status chips render the raw token next to the card.
    assert!(full.contains("running"), "running chip:\n{full}");
    assert!(full.contains("cancelled"), "cancelled chip:\n{full}");

    insta::assert_snapshot!(full);
}

/// The focused card's `▶` marker is painted in `SELECTION_GREEN` (non-vacuous).
#[test]
fn focused_card_marker_is_green() {
    let state = KanbanState::from_tasks(&six_tasks(), NOW_MS);
    let mut buf = WireBuffer::new(120, 30);
    render_kanban(&mut buf, 120, 0, 30, &state, NOW_MS);
    let green_marker = buf
        .cells
        .iter()
        .any(|(_, cell)| cell.symbol == "▶" && cell.fg == Some(SELECTION_GREEN));
    assert!(
        green_marker,
        "the focused card's `▶` marker must be painted SELECTION_GREEN"
    );
}

/// The status chips carry distinct accents: running blue, failed red, done green
/// (non-vacuous per-column colour check).
#[test]
fn status_chips_carry_distinct_accents() {
    let state = KanbanState::from_tasks(&six_tasks(), NOW_MS);
    let mut buf = WireBuffer::new(120, 30);
    render_kanban(&mut buf, 120, 0, 30, &state, NOW_MS);

    let has_color = |needle: char, color: Color| {
        buf.cells
            .iter()
            .any(|(_, c)| c.symbol == needle.to_string() && c.fg == Some(color))
    };
    // `running` chip → blue (the 'r' of the chip word).
    assert!(has_color('r', RUNNING_BLUE), "running chip must be blue");
    // `cancelled` chip → red (the 'c' of the chip word).
    assert!(has_color('c', WARN_RED), "cancelled chip must be warn-red");
    // `done` chip → green (the 'd' of the chip word, also the done header).
    assert!(has_color('d', SELECTION_GREEN), "done chip must be green");
}
