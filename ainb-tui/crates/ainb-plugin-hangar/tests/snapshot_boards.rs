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
    BoardCardWireRow, BoardColumnWireRow, BoardWireRow, BoardsListResult, CardMemberChip,
};
use ainb_plugin_hangar::{
    BoardsEvent, BoardsKey, BoardsState, BoardsStatus, RepoOption, reduce_boards, render_boards,
};
use ainb_plugin_sdk::WireBuffer;

fn card(issue: &str, title: &str, state: Option<&str>) -> BoardCardWireRow {
    BoardCardWireRow {
        issue_id: issue.into(),
        title: title.into(),
        display_id: issue.into(),
        state: state.map(str::to_string),
        session_name: None,
        repo_ref: None,
        agent: None,
        squad_id: None,
        member_states: Vec::new(),
        blocked_by: Vec::new(),
        auto_run: false,
        blocks: Vec::new(),
        related: Vec::new(),
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
                col(
                    "c1",
                    "Backlog",
                    None,
                    false,
                    vec![card("bk01", "Write the spec", None)],
                ),
                col(
                    "c2",
                    "Queued",
                    Some("queued"),
                    true,
                    vec![card("q002", "Wire the RPC", Some("queued"))],
                ),
                col(
                    "c3",
                    "Running",
                    Some("running"),
                    true,
                    vec![card("r003", "Build the board", Some("running"))],
                ),
                col(
                    "c4",
                    "Failed",
                    Some("failed"),
                    true,
                    vec![card("f004", "Flaky migration", Some("failed"))],
                ),
                col(
                    "c5",
                    "Done",
                    Some("done"),
                    true,
                    vec![card("d005", "Ship the tables", Some("done"))],
                ),
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

/// Loading state: the fetch has not answered — a "Loading boards…" line renders,
/// NOT the empty-workspace create prompt (a never-fetched state must not read as
/// empty).
#[test]
fn render_loading_board_snapshot() {
    let state = BoardsState::default();
    assert_eq!(state.status(), &BoardsStatus::Loading);
    let mut buf = WireBuffer::new(120, 20);
    render_boards(&mut buf, 120, 0, 20, &state);
    let map = glyph_map(&buf, 120);
    assert!(map.contains("Loading boards"), "loading line:\n{map}");
    assert!(
        !map.contains("No boards yet"),
        "not the empty prompt:\n{map}"
    );
    insta::assert_snapshot!(map);
}

/// Error state: a failed fetch renders a distinct "Couldn't load boards" banner
/// with the daemon error, never the create prompt — a daemon failure must not
/// read as an invitation to create a board (P4 / D8).
#[test]
fn render_error_board_snapshot() {
    let mut state = BoardsState::default();
    state.set_error("daemon error: workspace/subscribe rejected");
    let mut buf = WireBuffer::new(120, 20);
    render_boards(&mut buf, 120, 0, 20, &state);
    let map = glyph_map(&buf, 120);
    assert!(map.contains("Couldn't load boards"), "error banner:\n{map}");
    assert!(
        !map.contains("No boards yet"),
        "not the empty prompt:\n{map}"
    );
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
    // The hint band renders the controls next to the widget; the F6 card verbs
    // (timeline / remove / move) lead so they read even before the column verbs.
    assert!(
        map.contains("run") && map.contains("timeline") && map.contains("remove"),
        "hint band:\n{map}"
    );
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
    assert!(
        map.contains("Board: Delivery"),
        "title at narrow width:\n{map}"
    );
    insta::assert_snapshot!(map);
}

// ---------------------------------------------------------------------------
// tcp T4 / F7 — squad-from-card + card dependencies render.
// ---------------------------------------------------------------------------

/// A one-column board whose card is BLOCKED by an unfinished blocker card (tcp T4
/// / F7): the card renders the 🔒 marker + the blocker ref in its title badge.
fn blocked_card_board() -> BoardsListResult {
    let blocked = BoardCardWireRow {
        blocked_by: vec!["ock-1".into()],
        ..card("dep01", "Ship the migration", None)
    };
    BoardsListResult {
        boards: vec![BoardWireRow {
            id: "b1".into(),
            name: "Delivery".into(),
            auto_move: true,
            columns: vec![col("c1", "Todo", None, false, vec![blocked])],
            unmapped: Vec::new(),
        }],
    }
}

/// A one-column board whose card is assigned a SQUAD and has run (tcp T4 / F7):
/// the card renders one member chip per fanned-out member task + the auto-run
/// marker.
fn squad_card_board() -> BoardsListResult {
    let squad = BoardCardWireRow {
        squad_id: Some("sq-1".into()),
        auto_run: true,
        member_states: vec![
            CardMemberChip {
                agent_id: "a-lead".into(),
                agent_name: "lead".into(),
                state: Some("running".into()),
            },
            CardMemberChip {
                agent_id: "a-m1".into(),
                agent_name: "m1".into(),
                state: Some("queued".into()),
            },
        ],
        ..card("sqd01", "Fan this out", Some("running"))
    };
    BoardsListResult {
        boards: vec![BoardWireRow {
            id: "b1".into(),
            name: "Delivery".into(),
            auto_move: true,
            columns: vec![col("c1", "Running", Some("running"), true, vec![squad])],
            unmapped: Vec::new(),
        }],
    }
}

/// A BLOCKED card renders 🔒 (the blocked marker) + the blocker ref in its badge —
/// the F7 blocked-state on the board.
#[test]
fn render_blocked_card_snapshot() {
    let state = BoardsState::from_snapshot(&blocked_card_board());
    let mut buf = WireBuffer::new(120, 20);
    render_boards(&mut buf, 120, 0, 20, &state);
    let map = glyph_map(&buf, 120);
    assert!(
        map.contains('🔒'),
        "blocked card shows the lock marker:\n{map}"
    );
    assert!(map.contains("ock-1"), "the blocker ref renders:\n{map}");
    insta::assert_snapshot!(map);
}

/// A SQUAD card renders one member chip per fanned-out member (name:state) + the
/// auto-run marker — the F7 squad-from-card state on the board.
#[test]
fn render_squad_card_snapshot() {
    let state = BoardsState::from_snapshot(&squad_card_board());
    let mut buf = WireBuffer::new(120, 20);
    render_boards(&mut buf, 120, 0, 20, &state);
    let map = glyph_map(&buf, 120);
    assert!(
        map.contains('👥'),
        "squad card shows the members marker:\n{map}"
    );
    assert!(
        map.contains("lead:running") && map.contains("m1:queued"),
        "per-member chips render:\n{map}"
    );
    assert!(map.contains('⏵'), "auto-run marker renders:\n{map}");
    insta::assert_snapshot!(map);
}

// ---------------------------------------------------------------------------
// Card-create parity overlay (spec F1-F4): title → repo (`@` autocomplete) →
// agent chips → profile → column. Driven through the public reducer so each
// snapshot pins the REAL rendered banner over the board body.
// ---------------------------------------------------------------------------

/// A repo roster (one ★ favorite + one scanned) for the card-create snapshots.
fn repo_roster() -> Vec<RepoOption> {
    vec![
        RepoOption {
            label: "ainb".into(),
            repo_ref: "/src/ainb".into(),
            is_favorite: true,
            is_remote_only: false,
        },
        RepoOption {
            label: "widget".into(),
            repo_ref: "/src/widget".into(),
            is_favorite: false,
            is_remote_only: false,
        },
    ]
}

/// Open the card-create overlay on the focused column of a five-column board,
/// type `title`, then apply each key in `keys`.
fn card_overlay(title: &str, keys: &[BoardsKey]) -> BoardsState {
    let mut state = BoardsState::from_snapshot(&five_column_board());
    state.set_repos(repo_roster());
    state.set_profiles(vec!["claude-agent".into(), "codex-agent".into()]);
    state = reduce_boards(&state, BoardsEvent::AddCard).state;
    for ch in title.chars() {
        state = reduce_boards(&state, BoardsEvent::Key(BoardsKey::Char(ch))).state;
    }
    for k in keys {
        state = reduce_boards(&state, BoardsEvent::Key(*k)).state;
    }
    state
}

/// Stage 1 (title): the overlay banner renders the prompt + typed title over the
/// board body, advancing to the repo pick on Enter.
#[test]
fn render_card_title_overlay_snapshot() {
    let state = card_overlay("Refactor", &[]);
    let mut buf = WireBuffer::new(120, 20);
    render_boards(&mut buf, 120, 0, 20, &state);
    let map = glyph_map(&buf, 120);
    assert!(map.contains("New card title"), "title prompt:\n{map}");
    assert!(map.contains("Refactor"), "typed title:\n{map}");
    insta::assert_snapshot!(map);
}

/// Stage 2, field CLOSED (the "empty" repo state): before `@`, the prompt points
/// the user at scratch (the always-available F2 fallback).
#[test]
fn render_card_repo_closed_snapshot() {
    let state = card_overlay("Refactor", &[BoardsKey::Enter]);
    let mut buf = WireBuffer::new(120, 20);
    render_boards(&mut buf, 120, 0, 20, &state);
    let map = glyph_map(&buf, 120);
    assert!(map.contains("Repo for"), "repo prompt:\n{map}");
    assert!(
        map.contains("scratch always available"),
        "closed-field scratch pointer:\n{map}"
    );
    insta::assert_snapshot!(map);
}

/// Stage 2, dropdown OPEN (`@`): scratch first, then the roster with the ★
/// favorite pinned ahead of the scanned repo (spec F3).
#[test]
fn render_card_repo_at_open_snapshot() {
    let state = card_overlay("Refactor", &[BoardsKey::Enter, BoardsKey::Char('@')]);
    let mut buf = WireBuffer::new(120, 20);
    render_boards(&mut buf, 120, 0, 20, &state);
    let map = glyph_map(&buf, 120);
    assert!(map.contains("Repo for"), "repo prompt:\n{map}");
    assert!(map.contains("scratch"), "scratch always first:\n{map}");
    assert!(
        map.contains("ainb") && map.contains('★'),
        "★ favorite in the dropdown:\n{map}"
    );
    insta::assert_snapshot!(map);
}

/// Stage 3 (agent chips): claude / codex / copilot, copilot flagged with its F8
/// dispatch gate; the cascade default (claude) is highlighted.
#[test]
fn render_card_agent_chips_snapshot() {
    // title → repo (`@` → pick scratch at cursor 0) → agent.
    let state = card_overlay(
        "Refactor",
        &[BoardsKey::Enter, BoardsKey::Char('@'), BoardsKey::Enter],
    );
    let mut buf = WireBuffer::new(120, 20);
    render_boards(&mut buf, 120, 0, 20, &state);
    let map = glyph_map(&buf, 120);
    assert!(map.contains("Agent for"), "agent prompt:\n{map}");
    assert!(
        map.contains("claude") && map.contains("codex") && map.contains("copilot"),
        "three chips:\n{map}"
    );
    assert!(map.contains("F8"), "copilot dispatch gate flagged:\n{map}");
    insta::assert_snapshot!(map);
}

/// The repo dropdown at narrow width (60 cols) clips rather than overflowing.
#[test]
fn render_card_repo_narrow_snapshot() {
    let state = card_overlay("Refactor", &[BoardsKey::Enter, BoardsKey::Char('@')]);
    const W: u16 = 60;
    const H: u16 = 24;
    let mut buf = WireBuffer::new(W, H);
    render_boards(&mut buf, W, 0, H, &state);
    for (coord, _) in &buf.cells {
        assert!(
            coord.x < W && coord.y < H,
            "overlay wrote out-of-bounds cell at ({}, {})",
            coord.x,
            coord.y
        );
    }
    let map = glyph_map(&buf, W);
    insta::assert_snapshot!(map);
}
