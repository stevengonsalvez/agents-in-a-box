//! P4 / D8 — Boards screen render snapshots (design gate c).
//!
//! Renders the user-defined Boards screen ([`render_boards`]) in its three
//! layout-regression states — empty (no boards), a five-column loaded board, and
//! a narrow (60-col) width — to a backing [`WireBuffer`] and pins each with
//! `insta::assert_snapshot!` (trailing newline trimmed per
//! `reference_insta_trailing_newline_trap`). The snapshots prove the board title
//! + auto-move toggle + key-hint band render, the columns carry their FSM mapping,
//! a succeeded card shows its `✓` success marker, and the narrow width degrades
//! without overflowing the area.

use ainb_hangar_proto::snapshots::{
    BoardCardWireRow, BoardColumnWireRow, BoardWireRow, BoardsListResult,
};
use ainb_plugin_hangar::{render_boards, BoardsState};
use ainb_plugin_sdk::WireBuffer;

fn card(issue: &str, title: &str, state: Option<&str>) -> BoardCardWireRow {
    BoardCardWireRow {
        issue_id: issue.into(),
        title: title.into(),
        display_id: issue.into(),
        state: state.map(str::to_string),
    }
}

fn col(
    id: &str,
    name: &str,
    fsm: Option<&str>,
    auto: bool,
    cards: Vec<BoardCardWireRow>,
) -> BoardColumnWireRow {
    BoardColumnWireRow {
        id: id.into(),
        name: name.into(),
        ord: 0,
        fsm_state: fsm.map(str::to_string),
        auto_move: auto,
        cards,
    }
}

/// A five-column board: a manual Backlog, three FSM-mapped columns, and a Done
/// column whose card has succeeded (renders the `✓` marker).
fn five_column_board() -> BoardsListResult {
    BoardsListResult {
        boards: vec![BoardWireRow {
            id: "b1".into(),
            name: "Delivery".into(),
            auto_move: true,
            columns: vec![
                col("c1", "Backlog", None, false, vec![card("bk01", "Write the spec", None)]),
                col("c2", "Queued", Some("queued"), true, vec![card("q002", "Wire the RPC", Some("queued"))]),
                col("c3", "Running", Some("running"), true, vec![card("r003", "Build the board", Some("running"))]),
                col("c4", "Failed", Some("failed"), true, vec![card("f004", "Flaky migration", Some("failed"))]),
                col("c5", "Done", Some("done"), true, vec![card("d005", "Ship the tables", Some("done"))]),
            ],
            unmapped: Vec::new(),
        }],
    }
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

/// Empty state: no boards — a create prompt renders, nothing overflows.
#[test]
fn render_empty_board_snapshot() {
    let state = BoardsState::from_snapshot(&BoardsListResult { boards: Vec::new() });
    let mut buf = WireBuffer::new(120, 20);
    render_boards(&mut buf, 120, 0, 20, &state);
    let map = glyph_map(&buf, 120);
    assert!(map.contains("No boards yet"), "empty prompt:\n{map}");
    insta::assert_snapshot!(map);
}

/// Five-column loaded board at 120×30: title + auto-move ON toggle + hint band +
/// five columns with their FSM mappings + the succeeded card's `✓` marker.
#[test]
fn render_five_column_board_snapshot() {
    let state = BoardsState::from_snapshot(&five_column_board());
    let mut buf = WireBuffer::new(120, 30);
    render_boards(&mut buf, 120, 0, 30, &state);
    let map = glyph_map(&buf, 120);

    assert!(map.contains("Board: Delivery"), "title:\n{map}");
    assert!(map.contains("auto-move"), "auto-move toggle:\n{map}");
    assert!(map.contains("ON"), "toggle ON:\n{map}");
    // The hint band renders the controls next to the widget.
    assert!(map.contains("run") && map.contains("reorder"), "hint band:\n{map}");
    // Columns carry their FSM mapping in the header (auto-move columns use `↦`).
    assert!(map.contains("Backlog"), "manual column:\n{map}");
    assert!(map.contains("done"), "done mapping:\n{map}");
    // The succeeded card shows its success marker.
    assert!(map.contains('✓'), "succeeded card marker:\n{map}");
    // Rounded card borders (the shared card-board signature).
    assert!(map.contains('╭'), "card-board borders:\n{map}");

    insta::assert_snapshot!(map);
}

/// Narrow width (60 cols): the board degrades — clips columns rather than
/// overflowing, and never writes a cell past the area.
#[test]
fn render_narrow_board_snapshot() {
    let state = BoardsState::from_snapshot(&five_column_board());
    const W: u16 = 60;
    const H: u16 = 24;
    let mut buf = WireBuffer::new(W, H);
    render_boards(&mut buf, W, 0, H, &state);
    // No cell may land outside the narrow area.
    for (coord, _) in &buf.cells {
        assert!(
            coord.x < W && coord.y < H,
            "boards render wrote out-of-bounds cell at ({}, {})",
            coord.x,
            coord.y
        );
    }
    let map = glyph_map(&buf, W);
    assert!(map.contains("Board: Delivery"), "title at narrow width:\n{map}");
    insta::assert_snapshot!(map);
}
