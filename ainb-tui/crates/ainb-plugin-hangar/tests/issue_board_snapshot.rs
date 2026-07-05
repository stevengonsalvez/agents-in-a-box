//! 63l.7 — Issues board render golden.
//!
//! The Issues screen is the headline of the mouse-driven board redesign (63l.4):
//! it renders THROUGH the shared Linear-style card-board widget — five status
//! columns side by side, each a per-column-scrollable stack of bordered cards.
//! This pins that render with an `insta` glyph golden (trailing newline trimmed
//! per `reference_insta_trailing_newline_trap`) so a regression to the old
//! vertical band layout — or a dropped column / id / priority chip — fails here
//! instead of silently shipping.
//!
//! A golden that only asserted "non-empty" would be vacuous, so the golden is
//! paired with explicit CELL-CONTENT assertions: every one of the five canonical
//! column headers with its live count, each seeded card's `HGR-<n>` display id,
//! and a coloured priority chip (label + glyph). The five-column horizontal
//! spread (Backlog left, Done right) is checked off the painted header positions,
//! and a non-vacuous colour check backs the heavy clay highlight border the
//! card-board raises on the selected card.

use ainb_hangar_core::ids::IssueId;
use ainb_hangar_proto::events::IssueRow;
use ainb_plugin_hangar::IssueListState;
use ainb_plugin_hangar::screen::issue_list::render_issue_list;
use ainb_plugin_sdk::{Color, WireBuffer};

/// `CLAY` — the heavy highlight border colour of the selected card-board card.
const CLAY: Color = Color::rgb(210, 130, 90);

/// One seeded issue row in `state`, with a deterministic `HGR-<n>` display id and
/// urgency so the card anatomy (id line + priority chip) renders predictably.
fn row(id: &str, display: &str, state: &str, priority: i64, assignee: Option<&str>) -> IssueRow {
    IssueRow {
        id: IssueId::from_str(id).unwrap(),
        display_id: Some(display.into()),
        workspace_id: "default".into(),
        title: format!("Issue {id}"),
        description: None,
        state: state.into(),
        assignee: assignee.map(ToString::to_string),
        creator: "member:alice".into(),
        created_at: 1_700_000_000_000,
        priority,
        due_date: None,
        labels: Vec::new(),
        pr_url: None,
        branch: None,
    }
}

/// One issue in each canonical lifecycle column, with spread priorities so each
/// priority chip variant is exercised across the board.
fn five_column_fixture() -> Vec<IssueRow> {
    vec![
        row("i-backlog", "HGR-1", "backlog", 0, Some("member:alice")),
        row("i-todo", "HGR-2", "todo", 1, Some("agent:claude")),
        row("i-prog", "HGR-3", "in_progress", 2, Some("agent:gpt")),
        row("i-review", "HGR-4", "in_review", 3, Some("member:bob")),
        row("i-done", "HGR-5", "done", 0, None),
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

/// The start column of `label` painted anywhere in the buffer, row-major. The
/// canonical headers are ASCII, so the byte index equals the char column.
fn label_x(full: &str, label: &str) -> Option<usize> {
    full.lines().find_map(|line| line.find(label))
}

/// The full Issues board golden at 120×24: five card-board columns side by side,
/// each header carrying its live count, each seeded issue a bordered card with its
/// `HGR-<n>` id and a priority chip.
#[test]
fn render_full_issue_board_snapshot() {
    let state = IssueListState::with_rows(five_column_fixture());
    let mut buf = WireBuffer::new(120, 24);
    // Body region: chip bar on row 1, board below, footer pinned at the last row.
    render_issue_list(&mut buf, 120, 1, 23, &state, 2);
    let full = glyph_map(&buf, 120);

    insta::assert_snapshot!(full);

    // POSITIVE — every canonical column header with its live count (the board
    // buckets one issue into each of the five columns).
    for header in [
        "Backlog (1)",
        "Todo (1)",
        "In Progress (1)",
        "In Review (1)",
        "Done (1)",
    ] {
        assert!(
            full.contains(header),
            "missing column header {header:?}:\n{full}"
        );
    }

    // POSITIVE — each seeded card paints its `HGR-<n>` display id.
    for id in ["HGR-1", "HGR-2", "HGR-3", "HGR-4", "HGR-5"] {
        assert!(full.contains(id), "missing card display id {id:?}:\n{full}");
    }

    // POSITIVE — the priority chips render across the spread (Medium/High/Urgent
    // labels + the diamond chip glyph). HGR-2=Medium, HGR-3=High, HGR-4=Urgent.
    for chip in ["Medium", "High", "Urgent"] {
        assert!(
            full.contains(chip),
            "missing priority chip {chip:?}:\n{full}"
        );
    }
    assert!(full.contains('◆'), "missing priority chip glyph:\n{full}");

    // The card-board paints rounded card borders (its signature; the old band
    // layout painted bare rows).
    assert!(full.contains('╭'), "missing rounded card borders:\n{full}");

    // The five columns spread HORIZONTALLY: Backlog leftmost, Done rightmost on
    // the same header band — proving the board uses the width, not a vertical
    // stack (which would order the headers top-to-bottom in one column).
    let backlog_x = label_x(&full, "Backlog (").expect("Backlog header painted");
    let done_x = label_x(&full, "Done (").expect("Done header painted");
    assert!(
        done_x > backlog_x,
        "Done must sit RIGHT of Backlog (horizontal columns): \
         backlog_x={backlog_x}, done_x={done_x}\n{full}"
    );
    assert!(
        done_x >= (120 * 4 / 5) - 4,
        "Done must occupy the rightmost fifth of a five-column board (done_x={done_x})\n{full}"
    );

    // NON-VACUOUS COLOUR CHECK — the selected (first visible) card carries the
    // heavy clay highlight border the card-board raises.
    let heavy_in_clay =
        buf.cells.iter().any(|(_, cell)| cell.symbol == "┏" && cell.fg == Some(CLAY));
    assert!(
        heavy_in_clay,
        "the selected issue card must carry the heavy clay highlight border"
    );
}
