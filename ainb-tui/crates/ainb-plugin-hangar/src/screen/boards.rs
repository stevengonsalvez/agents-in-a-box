//! P4 / D8 — the user-defined Boards screen (hotkey `B`).
//!
//! The Boards screen renders the workspace's USER-DEFINED kanban boards (the P4
//! upgrade over the fixed four-column [`kanban`](crate::screen::kanban) task
//! board, which it lives alongside — additive, no regression). A board has any
//! number of ordered columns, each optionally mapped to a task-FSM status so a
//! card AUTO-MOVES there when its work reaches that state (`card = issue`). The
//! daemon owns the data (`hangar/boards_list`); this screen is a pure view over a
//! [`BoardsListResult`] snapshot plus a focus cursor, and its reducer lifts every
//! mutation into the matching `hangar/board_*` RPC — the plugin owns zero domain
//! state (`project_ainb_plugin_owns_data_plane`).
//!
//! ## What renders
//!
//! The focused board's columns paint THROUGH the shared card-board widget
//! ([`card_board`]) so the Boards screen looks like every other board surface. A
//! card shows `#<display_id>`, the issue title, and — when its latest task
//! `done` — a leading `✓` success marker (the card-green-on-succeeded signal in
//! text form; the colour gate is the vhs frame-read layer). A board's cards whose
//! column was deleted render in a trailing `unmapped` pseudo-column so they never
//! vanish. A hint band under the board title renders the column/card key bindings
//! NEXT to the widget (`feedback_keybinding_hints_near_control`).
//!
//! ## Reducer
//!
//! [`reduce_boards`] folds a [`BoardsEvent`] into a new [`BoardsState`] plus an
//! optional [`BoardsIntent`] the glue lifts into a daemon RPC (run a card, attach
//! to it, add/rename/delete/reorder a column, add a card, toggle the board's
//! auto-move, create/delete a board). Pure: no IO, no input mutation.

use ainb_hangar_proto::snapshots::{BoardCardWireRow, BoardsListResult};
use ainb_plugin_sdk::{Cell, Color, Coord, WireBuffer};

use crate::widgets::card_board::{self, BoardCard, PriorityChip};

/// Gold accent for the board title + active auto-move toggle.
const GOLD: Color = Color::rgb(255, 215, 0);
/// Muted grey for hints + the off auto-move toggle.
const MUTED: Color = Color::rgb(120, 120, 140);
/// Green for the "auto-move ON" indicator.
const GREEN: Color = Color::rgb(100, 200, 120);
/// Amber-red for the "couldn't load boards" error state.
const ERROR_RED: Color = Color::rgb(235, 90, 90);

/// The load state of the Boards screen.
///
/// Lets [`render_boards`] tell a genuinely-empty workspace (offer to create a
/// board) apart from a fetch that has not answered yet (loading) or has failed
/// (error) — the daemon owns the data, this only reflects its load state
/// (`project_ainb_plugin_owns_data_plane`). Without it a daemon error reads as an
/// invitation to create a board.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum BoardsStatus {
    /// The `hangar/boards_list` fetch is in flight (or has not fired yet). The
    /// default, so a fresh state before the first reply reads as "loading", not
    /// "empty".
    #[default]
    Loading,
    /// A snapshot has been applied — the board list (possibly empty) is current.
    Loaded,
    /// The fetch (or a mutation reply) failed; carries the daemon/parse error for
    /// the render.
    Error(String),
}

/// A card flattened for the Boards render (derived from a [`BoardCardWireRow`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CardView {
    /// The placed issue's id (the run / attach / move RPCs carry this).
    pub issue_id: String,
    /// The issue title (the card's label).
    pub title: String,
    /// The short issue id rendered on the card header.
    pub display_id: String,
    /// The issue's latest task status (`done` turns the card green / `✓`).
    pub state: Option<String>,
}

impl CardView {
    fn from_wire(w: &BoardCardWireRow) -> Self {
        Self {
            issue_id: w.issue_id.clone(),
            title: w.title.clone(),
            display_id: w.display_id.clone(),
            state: w.state.clone(),
        }
    }

    /// Whether the card's work has succeeded (its latest task is `done`).
    #[must_use]
    pub fn is_succeeded(&self) -> bool {
        self.state.as_deref() == Some("done")
    }
}

/// One column of the focused board: its id, name, FSM mapping, and its cards.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnView {
    /// The column's stable id (reorder / card-move key off this).
    pub id: String,
    /// The column's display name.
    pub name: String,
    /// The task-status this column maps to (the auto-move target), or `None`.
    pub fsm_state: Option<String>,
    /// Whether the column auto-moves matching cards in.
    pub auto_move: bool,
    /// The cards in this column, in board order.
    pub cards: Vec<CardView>,
}

/// One board with its columns + its unmapped-card pool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoardView {
    /// The board's id.
    pub id: String,
    /// The board's name.
    pub name: String,
    /// The per-board auto-move master toggle.
    pub auto_move: bool,
    /// The board's columns, left-to-right.
    pub columns: Vec<ColumnView>,
    /// Cards whose column was deleted — rendered in a trailing pool.
    pub unmapped: Vec<CardView>,
}

/// The render-state cache for the Boards screen.
///
/// Holds the boards + a `(board, column, card)` focus cursor, clamped on every
/// mutation so a snapshot refresh never dangles the cursor past the end.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BoardsState {
    boards: Vec<BoardView>,
    focused_board: usize,
    focused_col: usize,
    focused_card: usize,
    /// The fetch load state — distinguishes empty from loading/error at render.
    status: BoardsStatus,
}

impl BoardsState {
    /// Build the screen state from a `hangar/boards_list` snapshot, keeping the
    /// focus cursor within bounds (reset to the first card of the first board).
    #[must_use]
    pub fn from_snapshot(snapshot: &BoardsListResult) -> Self {
        let boards = snapshot
            .boards
            .iter()
            .map(|b| BoardView {
                id: b.id.clone(),
                name: b.name.clone(),
                auto_move: b.auto_move,
                columns: b
                    .columns
                    .iter()
                    .map(|c| ColumnView {
                        id: c.id.clone(),
                        name: c.name.clone(),
                        fsm_state: c.fsm_state.clone(),
                        auto_move: c.auto_move,
                        cards: c.cards.iter().map(CardView::from_wire).collect(),
                    })
                    .collect(),
                unmapped: b.unmapped.iter().map(CardView::from_wire).collect(),
            })
            .collect();
        let mut state = Self {
            boards,
            focused_board: 0,
            focused_col: 0,
            focused_card: 0,
            status: BoardsStatus::Loaded,
        };
        state.clamp();
        state
    }

    /// The current load status (loading / loaded / error) the render branches on.
    #[must_use]
    pub const fn status(&self) -> &BoardsStatus {
        &self.status
    }

    /// Mark the boards fetch (or a mutation reply) as failed, preserving any board
    /// already shown — the error only surfaces when the list is empty (an initial
    /// or failed fetch), so a transient mutation error never blanks a live board.
    pub fn set_error(&mut self, message: impl Into<String>) {
        self.status = BoardsStatus::Error(message.into());
    }

    /// The boards, in list order.
    #[must_use]
    pub fn boards(&self) -> &[BoardView] {
        &self.boards
    }

    /// The focused board, if any.
    #[must_use]
    pub fn focused_board(&self) -> Option<&BoardView> {
        self.boards.get(self.focused_board)
    }

    /// The focused card, if any (the run / attach source).
    #[must_use]
    pub fn focused_card(&self) -> Option<&CardView> {
        let board = self.boards.get(self.focused_board)?;
        board
            .columns
            .get(self.focused_col)
            .and_then(|c| c.cards.get(self.focused_card))
    }

    /// The focused column, if any (the rename / delete / reorder / add-card
    /// target).
    #[must_use]
    pub fn focused_column(&self) -> Option<&ColumnView> {
        self.boards
            .get(self.focused_board)
            .and_then(|b| b.columns.get(self.focused_col))
    }

    /// The `(board, column)` focus indices, for the render highlight.
    #[must_use]
    pub const fn focus(&self) -> (usize, usize, usize) {
        (self.focused_board, self.focused_col, self.focused_card)
    }

    /// Clamp the cursor into the current board's bounds.
    fn clamp(&mut self) {
        if self.boards.is_empty() {
            self.focused_board = 0;
            self.focused_col = 0;
            self.focused_card = 0;
            return;
        }
        self.focused_board = self.focused_board.min(self.boards.len() - 1);
        let board = &self.boards[self.focused_board];
        let ncols = board.columns.len();
        if ncols == 0 {
            self.focused_col = 0;
            self.focused_card = 0;
            return;
        }
        self.focused_col = self.focused_col.min(ncols - 1);
        let ncards = board.columns[self.focused_col].cards.len();
        self.focused_card = self.focused_card.min(ncards.saturating_sub(1));
    }
}

/// An input the Boards reducer folds into [`BoardsState`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BoardsEvent {
    /// Focus one column left (`←`).
    FocusLeft,
    /// Focus one column right (`→`).
    FocusRight,
    /// Focus one card up (`↑`).
    FocusUp,
    /// Focus one card down (`↓`).
    FocusDown,
    /// Switch to the next board (`]`).
    NextBoard,
    /// Switch to the previous board (`[`).
    PrevBoard,
    /// Create a new board (`b`) — the glue names it and fires `board_create`. The
    /// only board mutation that works with NO board focused (the empty-state
    /// affordance), so a fresh workspace is never a dead end.
    CreateBoard,
    /// Run the focused card via the existing dispatch (`enter`).
    RunFocusedCard,
    /// Attach to the focused card's session (`a`).
    AttachFocusedCard,
    /// Add a column to the focused board (`n`) — the glue prompts for the name.
    AddColumn,
    /// Rename the focused column (`r`).
    RenameColumn,
    /// Delete the focused column (`x`); its cards park unmapped.
    DeleteColumn,
    /// Add a card to the focused column (`c`).
    AddCard,
    /// Move the focused column one place left (`⇧←`).
    ReorderColumnLeft,
    /// Move the focused column one place right (`⇧→`).
    ReorderColumnRight,
    /// Toggle the focused board's auto-move master toggle (`m`).
    ToggleAutoMove,
}

/// A side-effect the plugin glue performs after a Boards reduction — each lifts
/// into a `hangar/board_*` RPC (the daemon owns the real mutation).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BoardsIntent {
    /// Create a new board (`hangar/board_create`). Raised unconditionally — it is
    /// the empty-state affordance, so it must fire even with no board focused.
    CreateBoard,
    /// Run the card's issue via the existing dispatch with the assignee profile
    /// (`hangar/task_transition` / dispatch path). Card = issue.
    RunCard {
        /// The board the card sits on.
        board_id: String,
        /// The issue to dispatch.
        issue_id: String,
    },
    /// Attach to the card's running session (the existing attach affordance).
    AttachCard {
        /// The board the card sits on.
        board_id: String,
        /// The issue whose session to attach to.
        issue_id: String,
    },
    /// Prompt for a name and add a column (`hangar/board_column_add`).
    AddColumn {
        /// The board to append a column to.
        board_id: String,
    },
    /// Prompt for a new name and rename the focused column
    /// (`hangar/board_column_update`).
    RenameColumn {
        /// The board the column belongs to.
        board_id: String,
        /// The column to rename.
        column_id: String,
    },
    /// Delete the focused column (`hangar/board_column_delete`); cards park
    /// unmapped.
    DeleteColumn {
        /// The board the column belongs to.
        board_id: String,
        /// The column to delete.
        column_id: String,
    },
    /// Prompt for an issue and add a card to the focused column
    /// (`hangar/board_card_add`).
    AddCard {
        /// The board to add the card to.
        board_id: String,
        /// The column to place the card in.
        column_id: String,
    },
    /// Reorder the focused column: the board's new full column-id order
    /// (`hangar/board_column_reorder`).
    ReorderColumns {
        /// The board to reorder.
        board_id: String,
        /// The columns in their new left-to-right order.
        column_ids: Vec<String>,
    },
    /// Flip the board's auto-move master toggle (`hangar/board_update`).
    ToggleAutoMove {
        /// The board to retune.
        board_id: String,
        /// The new toggle value.
        auto_move: bool,
    },
}

/// The result of folding one [`BoardsEvent`] into a [`BoardsState`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoardsReduction {
    /// The next state.
    pub state: BoardsState,
    /// A side-effect for the plugin glue, if any.
    pub intent: Option<BoardsIntent>,
}

/// Fold one [`BoardsEvent`] into `state`. Pure: no IO, no input mutation.
#[must_use]
pub fn reduce_boards(state: &BoardsState, ev: BoardsEvent) -> BoardsReduction {
    match ev {
        BoardsEvent::FocusLeft => nav_col(state, -1),
        BoardsEvent::FocusRight => nav_col(state, 1),
        BoardsEvent::FocusUp => nav_card(state, -1),
        BoardsEvent::FocusDown => nav_card(state, 1),
        BoardsEvent::NextBoard => nav_board(state, 1),
        BoardsEvent::PrevBoard => nav_board(state, -1),
        // Create works with no board focused — the empty-state escape hatch.
        BoardsEvent::CreateBoard => BoardsReduction {
            state: state.clone(),
            intent: Some(BoardsIntent::CreateBoard),
        },
        BoardsEvent::RunFocusedCard => card_intent(state, |b, c| BoardsIntent::RunCard {
            board_id: b.id.clone(),
            issue_id: c.issue_id.clone(),
        }),
        BoardsEvent::AttachFocusedCard => card_intent(state, |b, c| BoardsIntent::AttachCard {
            board_id: b.id.clone(),
            issue_id: c.issue_id.clone(),
        }),
        BoardsEvent::AddColumn => board_intent(state, |b| BoardsIntent::AddColumn {
            board_id: b.id.clone(),
        }),
        BoardsEvent::RenameColumn => column_intent(state, |b, col| BoardsIntent::RenameColumn {
            board_id: b.id.clone(),
            column_id: col.id.clone(),
        }),
        BoardsEvent::DeleteColumn => column_intent(state, |b, col| BoardsIntent::DeleteColumn {
            board_id: b.id.clone(),
            column_id: col.id.clone(),
        }),
        BoardsEvent::AddCard => column_intent(state, |b, col| BoardsIntent::AddCard {
            board_id: b.id.clone(),
            column_id: col.id.clone(),
        }),
        BoardsEvent::ReorderColumnLeft => reorder(state, -1),
        BoardsEvent::ReorderColumnRight => reorder(state, 1),
        BoardsEvent::ToggleAutoMove => board_intent(state, |b| BoardsIntent::ToggleAutoMove {
            board_id: b.id.clone(),
            auto_move: !b.auto_move,
        }),
    }
}

/// Move the column focus by `delta`, clamped, resetting the card focus.
fn nav_col(state: &BoardsState, delta: i32) -> BoardsReduction {
    let Some(board) = state.focused_board() else {
        return unchanged(state);
    };
    if board.columns.is_empty() {
        return unchanged(state);
    }
    let max = i32::try_from(board.columns.len() - 1).unwrap_or(0);
    let cur = i32::try_from(state.focused_col).unwrap_or(0);
    let mut next = state.clone();
    next.focused_col = usize::try_from((cur + delta).clamp(0, max)).unwrap_or(0);
    next.focused_card = 0;
    next.clamp();
    no_intent(next)
}

/// Move the card focus by `delta` within the focused column, clamped.
fn nav_card(state: &BoardsState, delta: i32) -> BoardsReduction {
    let Some(col) = state.focused_column() else {
        return unchanged(state);
    };
    if col.cards.is_empty() {
        return unchanged(state);
    }
    let max = i32::try_from(col.cards.len() - 1).unwrap_or(0);
    let cur = i32::try_from(state.focused_card).unwrap_or(0);
    let mut next = state.clone();
    next.focused_card = usize::try_from((cur + delta).clamp(0, max)).unwrap_or(0);
    no_intent(next)
}

/// Switch boards by `delta`, clamped, resetting the column/card focus.
fn nav_board(state: &BoardsState, delta: i32) -> BoardsReduction {
    if state.boards.is_empty() {
        return unchanged(state);
    }
    let max = i32::try_from(state.boards.len() - 1).unwrap_or(0);
    let cur = i32::try_from(state.focused_board).unwrap_or(0);
    let mut next = state.clone();
    next.focused_board = usize::try_from((cur + delta).clamp(0, max)).unwrap_or(0);
    next.focused_col = 0;
    next.focused_card = 0;
    next.clamp();
    no_intent(next)
}

/// Reorder the focused column one place in `dir`, emitting the board's new full
/// column order. A no-op at the board edge or with no focused board.
fn reorder(state: &BoardsState, dir: i32) -> BoardsReduction {
    let Some(board) = state.focused_board() else {
        return unchanged(state);
    };
    let n = board.columns.len();
    if n < 2 {
        return unchanged(state);
    }
    let from = state.focused_col;
    let to = i32::try_from(from).unwrap_or(0) + dir;
    if !(0..i32::try_from(n).unwrap_or(0)).contains(&to) {
        return unchanged(state);
    }
    let to = usize::try_from(to).unwrap_or(0);
    let mut ids: Vec<String> = board.columns.iter().map(|c| c.id.clone()).collect();
    ids.swap(from, to);
    // Follow the moved column with the focus so a repeated reorder keeps dragging
    // the same column.
    let mut next = state.clone();
    next.focused_col = to;
    next.focused_card = 0;
    next.clamp();
    BoardsReduction {
        state: next,
        intent: Some(BoardsIntent::ReorderColumns {
            board_id: board.id.clone(),
            column_ids: ids,
        }),
    }
}

/// Emit an intent derived from the focused board + focused card, if both exist.
fn card_intent(
    state: &BoardsState,
    f: impl Fn(&BoardView, &CardView) -> BoardsIntent,
) -> BoardsReduction {
    match (state.focused_board(), state.focused_card()) {
        (Some(b), Some(c)) => BoardsReduction {
            state: state.clone(),
            intent: Some(f(b, c)),
        },
        _ => unchanged(state),
    }
}

/// Emit an intent derived from the focused board, if it exists.
fn board_intent(state: &BoardsState, f: impl Fn(&BoardView) -> BoardsIntent) -> BoardsReduction {
    match state.focused_board() {
        Some(b) => BoardsReduction {
            state: state.clone(),
            intent: Some(f(b)),
        },
        None => unchanged(state),
    }
}

/// Emit an intent derived from the focused board + focused column, if both exist.
fn column_intent(
    state: &BoardsState,
    f: impl Fn(&BoardView, &ColumnView) -> BoardsIntent,
) -> BoardsReduction {
    match (state.focused_board(), state.focused_column()) {
        (Some(b), Some(col)) => BoardsReduction {
            state: state.clone(),
            intent: Some(f(b, col)),
        },
        _ => unchanged(state),
    }
}

/// A reduction that changes state but emits no intent.
const fn no_intent(state: BoardsState) -> BoardsReduction {
    BoardsReduction {
        state,
        intent: None,
    }
}

/// A no-op reduction: state cloned unchanged, no intent.
fn unchanged(state: &BoardsState) -> BoardsReduction {
    no_intent(state.clone())
}

// ---------------------------------------------------------------------------
// Render
// ---------------------------------------------------------------------------

/// Render the Boards screen into `buf` between rows `top` and `bottom`.
///
/// Row `top` carries the board title + the `< n/m >` board pager + the auto-move
/// master toggle; row `top+1` is the key-hint band (`feedback_keybinding_hints_
/// near_control`); the card-board fills the rest. An empty state (no boards)
/// paints a prompt to create one.
pub fn render_boards(buf: &mut WireBuffer, area_w: u16, top: u16, bottom: u16, state: &BoardsState) {
    let Some(board) = state.focused_board() else {
        render_no_board(buf, area_w, top, state.status());
        return;
    };

    // Title + board pager + auto-move toggle.
    let title = format!("Board: {}", board.name);
    let mut x = put_str(buf, 0, top, &title, GOLD, area_w);
    let pager = format!(
        "   [{}/{}]",
        state.focused_board + 1,
        state.boards.len().max(1)
    );
    x = put_str(buf, x, top, &pager, MUTED, area_w);
    // Auto-move master toggle indicator, colour-coded.
    x = put_str(buf, x, top, "   auto-move ", MUTED, area_w);
    let (toggle, colour) = if board.auto_move {
        ("ON", GREEN)
    } else {
        ("off", MUTED)
    };
    put_str(buf, x, top, toggle, colour, area_w);

    // Hint band NEXT to the widget (letters next to the controls they affect).
    let hint_row = top.saturating_add(1);
    render_hint_band(buf, area_w, hint_row);

    // Card-board body below the hint band.
    let body_top = top.saturating_add(2);
    if body_top >= bottom {
        return;
    }
    let columns = board_columns(board);
    let (fb, fc, fcard) = state.focus();
    // Only highlight when the focus is on this (focused) board.
    let selected = if fb == state.focus().0 && !columns.is_empty() {
        Some((fc, fcard))
    } else {
        None
    };
    let _ = card_board::render_card_board(buf, area_w, body_top, bottom, &columns, selected);
}

/// Render the no-board state on row `top`, branching on the load `status` so an
/// empty workspace (create prompt), an in-flight fetch (loading), and a failed
/// fetch (error) never read as one another.
fn render_no_board(buf: &mut WireBuffer, area_w: u16, top: u16, status: &BoardsStatus) {
    match status {
        // A failed fetch is an error, never an invitation to create a board.
        BoardsStatus::Error(msg) => {
            let x = put_str(buf, 2, top, "Couldn't load boards — ", ERROR_RED, area_w);
            put_str(buf, x, top, msg, MUTED, area_w);
        }
        // The fetch has not answered yet.
        BoardsStatus::Loading => {
            put_str(buf, 2, top, "Loading boards…", MUTED, area_w);
        }
        // Genuinely empty: offer the create affordance.
        BoardsStatus::Loaded => {
            put_str(buf, 2, top, "No boards yet — press", MUTED, area_w);
            put_str(buf, 24, top, " b ", GOLD, area_w);
            put_str(buf, 27, top, "to create one.", MUTED, area_w);
        }
    }
}

/// The key-hint band rendered under the board title — the column/card bindings
/// next to the widget they drive.
fn render_hint_band(buf: &mut WireBuffer, area_w: u16, row: u16) {
    let hints: [(&str, &str); 8] = [
        ("↵", "run"),
        ("a", "attach"),
        ("n", "add col"),
        ("r", "rename"),
        ("x", "del col"),
        ("c", "add card"),
        ("⇧←→", "reorder"),
        ("m", "auto-move"),
    ];
    let mut x = 0u16;
    for (key, desc) in hints {
        x = put_str(buf, x, row, key, GOLD, area_w);
        x = put_str(buf, x, row, ":", MUTED, area_w);
        x = put_str(buf, x, row, desc, MUTED, area_w);
        x = put_str(buf, x, row, "  ", MUTED, area_w);
        if x >= area_w {
            break;
        }
    }
}

/// Flatten a board's columns (plus a trailing `unmapped` pseudo-column when it
/// holds parked cards) into the shared card-board columns.
fn board_columns(board: &BoardView) -> Vec<card_board::BoardColumn> {
    let mut columns: Vec<card_board::BoardColumn> = board
        .columns
        .iter()
        .map(|c| card_board::BoardColumn {
            glyph: if c.auto_move { '◈' } else { '○' },
            name: column_header(c),
            cards: c.cards.iter().map(card_view_to_board_card).collect(),
            scroll_offset: 0,
        })
        .collect();
    if !board.unmapped.is_empty() {
        columns.push(card_board::BoardColumn {
            glyph: '⚑',
            name: "unmapped".to_string(),
            cards: board.unmapped.iter().map(card_view_to_board_card).collect(),
            scroll_offset: 0,
        });
    }
    columns
}

/// A column header carrying its FSM mapping (`Done→done` / `Done↦done` for an
/// auto-move column) so the mapping reads on the board.
fn column_header(c: &ColumnView) -> String {
    match (&c.fsm_state, c.auto_move) {
        (Some(fs), true) => format!("{} ↦{}", c.name, fs),
        (Some(fs), false) => format!("{} ·{}", c.name, fs),
        (None, _) => c.name.clone(),
    }
}

/// Map a [`CardView`] onto a card-board card. A succeeded card gets a leading
/// `✓` marker on its id line (the card-green-on-success signal in text form).
fn card_view_to_board_card(c: &CardView) -> BoardCard {
    let marker = if c.is_succeeded() { "✓ #" } else { "#" };
    BoardCard {
        issue_id: c.issue_id.clone(),
        display_id: format!("{marker}{}", c.display_id),
        title: c.title.clone(),
        priority: PriorityChip::from_priority(0),
        assignee_initial: c.title.chars().next(),
    }
}

/// Write `s` at `(x, row)` in `color`, clipping at `area_w`. Returns the next
/// free column (char-safe).
fn put_str(buf: &mut WireBuffer, x: u16, row: u16, s: &str, color: Color, area_w: u16) -> u16 {
    let mut cx = x;
    for ch in s.chars() {
        if cx >= area_w {
            break;
        }
        let mut cell = Cell::new(ch.to_string());
        cell.fg = Some(color);
        buf.push(Coord::new(cx, row), cell);
        cx = cx.saturating_add(1);
    }
    cx
}

#[cfg(test)]
mod tests {
    use super::*;
    use ainb_hangar_proto::snapshots::{BoardColumnWireRow, BoardWireRow};

    fn card(issue: &str, title: &str, state: Option<&str>) -> BoardCardWireRow {
        BoardCardWireRow {
            issue_id: issue.into(),
            title: title.into(),
            display_id: issue.chars().rev().take(5).collect::<String>().chars().rev().collect(),
            state: state.map(str::to_string),
        }
    }

    fn col(id: &str, name: &str, fsm: Option<&str>, auto: bool, cards: Vec<BoardCardWireRow>) -> BoardColumnWireRow {
        BoardColumnWireRow {
            id: id.into(),
            name: name.into(),
            ord: 0,
            fsm_state: fsm.map(str::to_string),
            auto_move: auto,
            cards,
        }
    }

    fn one_board() -> BoardsListResult {
        BoardsListResult {
            boards: vec![BoardWireRow {
                id: "b1".into(),
                name: "Sprint".into(),
                auto_move: true,
                columns: vec![
                    col("c1", "Todo", None, false, vec![card("issue-1", "Refactor API", None)]),
                    col("c2", "Doing", Some("running"), true, vec![]),
                    col("c3", "Done", Some("done"), true, vec![card("issue-2", "Fix flaky test", Some("done"))]),
                ],
                unmapped: Vec::new(),
            }],
        }
    }

    /// Navigation walks columns + cards and clamps at the edges.
    #[test]
    fn navigation_walks_and_clamps() {
        let state = BoardsState::from_snapshot(&one_board());
        // Focus starts on the first card of the first column.
        assert_eq!(state.focused_card().unwrap().issue_id, "issue-1");
        // Right to the empty Doing column: no card focused.
        let r = reduce_boards(&state, BoardsEvent::FocusRight);
        assert!(r.state.focused_card().is_none(), "Doing is empty");
        // Right again to Done: its card focuses.
        let r = reduce_boards(&r.state, BoardsEvent::FocusRight);
        assert_eq!(r.state.focused_card().unwrap().issue_id, "issue-2");
        // Right at the edge is a no-op.
        let edge = reduce_boards(&r.state, BoardsEvent::FocusRight);
        assert_eq!(edge.state.focus().1, 2, "clamped at last column");
    }

    /// Enter on a focused card raises a RunCard intent carrying the issue.
    #[test]
    fn run_focused_card_emits_run_intent() {
        let state = BoardsState::from_snapshot(&one_board());
        let r = reduce_boards(&state, BoardsEvent::RunFocusedCard);
        assert_eq!(
            r.intent,
            Some(BoardsIntent::RunCard {
                board_id: "b1".into(),
                issue_id: "issue-1".into()
            })
        );
    }

    /// Toggling auto-move emits the flipped value for the focused board.
    #[test]
    fn toggle_auto_move_flips_the_board_value() {
        let state = BoardsState::from_snapshot(&one_board());
        let r = reduce_boards(&state, BoardsEvent::ToggleAutoMove);
        assert_eq!(
            r.intent,
            Some(BoardsIntent::ToggleAutoMove {
                board_id: "b1".into(),
                auto_move: false
            }),
            "the board starts auto-move ON, so the toggle emits off"
        );
    }

    /// Reordering the focused column right emits the board's new full id order
    /// and follows the moved column with the focus.
    #[test]
    fn reorder_right_emits_new_order_and_follows_focus() {
        let state = BoardsState::from_snapshot(&one_board());
        let r = reduce_boards(&state, BoardsEvent::ReorderColumnRight);
        assert_eq!(
            r.intent,
            Some(BoardsIntent::ReorderColumns {
                board_id: "b1".into(),
                column_ids: vec!["c2".into(), "c1".into(), "c3".into()],
            })
        );
        assert_eq!(r.state.focus().1, 1, "focus follows the moved column");
    }

    /// A delete-column on the focused column emits the column id.
    #[test]
    fn delete_focused_column_emits_intent() {
        let state = BoardsState::from_snapshot(&one_board());
        let r = reduce_boards(&state, BoardsEvent::DeleteColumn);
        assert_eq!(
            r.intent,
            Some(BoardsIntent::DeleteColumn {
                board_id: "b1".into(),
                column_id: "c1".into()
            })
        );
    }

    /// Flatten the buffer into `\n`-joined rows for a substring assertion.
    fn painted(buf: &WireBuffer) -> String {
        let mut grid = vec![vec![' '; buf.width as usize]; buf.height as usize];
        for (coord, cell) in &buf.cells {
            if coord.y < buf.height && coord.x < buf.width {
                if let Some(ch) = cell.symbol.chars().next() {
                    grid[coord.y as usize][coord.x as usize] = ch;
                }
            }
        }
        grid.into_iter()
            .map(|r| r.into_iter().collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// A LOADED but empty board list renders the create prompt (the genuine
    /// empty-workspace affordance).
    #[test]
    fn loaded_empty_renders_create_prompt() {
        let state = BoardsState::from_snapshot(&BoardsListResult { boards: Vec::new() });
        assert_eq!(state.status(), &BoardsStatus::Loaded);
        let mut buf = WireBuffer::new(80, 10);
        render_boards(&mut buf, 80, 0, 10, &state);
        let map = painted(&buf);
        assert!(map.contains("No boards yet"), "create prompt:\n{map}");
    }

    /// A default (never-fetched) state is LOADING — it must NOT read as an empty
    /// workspace inviting a create.
    #[test]
    fn default_state_renders_loading_not_create_prompt() {
        let state = BoardsState::default();
        assert_eq!(state.status(), &BoardsStatus::Loading);
        let mut buf = WireBuffer::new(80, 10);
        render_boards(&mut buf, 80, 0, 10, &state);
        let map = painted(&buf);
        assert!(map.contains("Loading boards"), "loading state:\n{map}");
        assert!(
            !map.contains("No boards yet"),
            "loading must not read as empty:\n{map}"
        );
    }

    /// A FAILED fetch renders a distinct error, never the create prompt — a daemon
    /// failure must not read as an invitation to create a board (P4 / D8).
    #[test]
    fn error_state_renders_error_not_create_prompt() {
        let mut state = BoardsState::default();
        state.set_error("connection refused");
        assert!(matches!(state.status(), BoardsStatus::Error(_)));
        let mut buf = WireBuffer::new(80, 10);
        render_boards(&mut buf, 80, 0, 10, &state);
        let map = painted(&buf);
        assert!(map.contains("Couldn't load boards"), "error banner:\n{map}");
        assert!(map.contains("connection refused"), "error detail:\n{map}");
        assert!(
            !map.contains("No boards yet"),
            "an error must not read as an empty workspace:\n{map}"
        );
    }

    /// An empty snapshot leaves no focused board and every card intent is a
    /// no-op — but CreateBoard still fires (the empty-state escape hatch).
    #[test]
    fn empty_snapshot_is_inert() {
        let state = BoardsState::from_snapshot(&BoardsListResult { boards: Vec::new() });
        assert!(state.focused_board().is_none());
        assert_eq!(reduce_boards(&state, BoardsEvent::RunFocusedCard).intent, None);
        assert_eq!(reduce_boards(&state, BoardsEvent::ToggleAutoMove).intent, None);
        // CreateBoard is the one mutation that works with no board focused, so the
        // empty state is never a dead end.
        assert_eq!(
            reduce_boards(&state, BoardsEvent::CreateBoard).intent,
            Some(BoardsIntent::CreateBoard),
            "create-board fires even on an empty board list"
        );
    }
}
