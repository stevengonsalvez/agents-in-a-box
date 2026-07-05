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
    /// The exact tmux session name an interactive run spawned for this card's
    /// latest task (`tmux_hangar-<task_id>`, ccc / D6), or `None` for a headless
    /// task / no run. The attach affordance surfaces it as `tmux attach -t <name>`.
    pub session_name: Option<String>,
}

impl CardView {
    fn from_wire(w: &BoardCardWireRow) -> Self {
        Self {
            issue_id: w.issue_id.clone(),
            title: w.title.clone(),
            display_id: w.display_id.clone(),
            state: w.state.clone(),
            session_name: w.session_name.clone(),
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

/// The launch mode of a card run (D6 `Run ▾`): a headless provider run
/// (`claude -p` / `codex exec`) or an interactive YOLO session. Both dispatch
/// through the one provider-runner path today; the choice is carried to the
/// daemon so the D6 launch surface records which the user asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunMode {
    /// A background headless run — the default (`claude -p` / `codex exec`).
    Headless,
    /// An interactive YOLO session.
    Interactive,
}

impl RunMode {
    /// The `hangar/board_card_run` `mode` wire token.
    #[must_use]
    pub const fn wire(self) -> &'static str {
        match self {
            Self::Headless => "headless",
            Self::Interactive => "interactive",
        }
    }

    /// The `Run ▾` menu label.
    const fn label(self) -> &'static str {
        match self {
            Self::Headless => "Headless (claude -p)",
            Self::Interactive => "Interactive (YOLO)",
        }
    }

    /// The two modes in menu order (Headless first / default).
    const ALL: [Self; 2] = [Self::Headless, Self::Interactive];
}

/// A provider agent the card-create overlay offers (spec F1/F4).
///
/// The three provider kinds the picker shows. `copilot` is SELECTABLE (F8) — the
/// choice is persisted at create — but a *run* on it is refused by the daemon
/// until its runner lands; the F8 gate fires at dispatch, not here. Defaults to
/// [`AgentChip::Claude`], the terminal F4 cascade fallback, so a card created
/// without touching the chips still routes to a dispatchable provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AgentChip {
    /// The `claude` provider (the default / cascade fallback).
    #[default]
    Claude,
    /// The `codex` provider.
    Codex,
    /// The `copilot` provider — picker-visible, dispatch-gated (F8).
    Copilot,
}

impl AgentChip {
    /// The `agent` wire token the card params carry (spec F1/F4).
    #[must_use]
    pub const fn wire(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Copilot => "copilot",
        }
    }

    /// The chip's picker label (copilot flags its F8 dispatch gate).
    const fn label(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Copilot => "copilot (dispatch gated — F8)",
        }
    }

    /// The chips in picker order (claude first — the cascade's safe default).
    const ALL: [Self; 3] = [Self::Claude, Self::Codex, Self::Copilot];

    /// The chip at `idx`, clamped to [`AgentChip::Claude`].
    fn at(idx: usize) -> Self {
        Self::ALL.get(idx).copied().unwrap_or(Self::Claude)
    }

    /// This chip's index in [`AgentChip::ALL`] (the picker cursor it pre-selects).
    fn index(self) -> usize {
        Self::ALL.iter().position(|a| *a == self).unwrap_or(0)
    }
}

/// One pickable repo in the card-create `@` dropdown (spec F2/F3).
///
/// Its display `label` and the `repo_ref` a pick persists — an absolute checkout
/// path, or the literal `scratch`. The glue builds the roster from
/// `hangar/repo_list` (favorites pinned first + recency, then scanned); the
/// reducer prepends [`RepoOption::scratch`] so scratch is ALWAYS the first,
/// guaranteed-launchable choice (F2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoOption {
    /// The display label (a favorite's alias / a scanned repo's name / `scratch`).
    pub label: String,
    /// The value persisted on the card: an absolute checkout path, or `scratch`.
    pub repo_ref: String,
    /// Whether this is a ★ favorite (rendered with a star, pinned ahead of scans).
    pub is_favorite: bool,
}

impl RepoOption {
    /// The always-first `scratch` option (F2): the guaranteed launchable repo the
    /// picker points a repo-less user at.
    #[must_use]
    pub fn scratch() -> Self {
        Self { label: "scratch".to_string(), repo_ref: "scratch".to_string(), is_favorite: false }
    }
}

/// The `@` dropdown candidates for `query`: [`RepoOption::scratch`] first
/// (ALWAYS, the F2 guaranteed repo), then the injected roster
/// (favorites-first + recency order preserved) fuzzy-filtered on `query`.
fn repo_candidates(repos: &[RepoOption], query: &str) -> Vec<RepoOption> {
    let mut out = vec![RepoOption::scratch()];
    out.extend(
        repos
            .iter()
            .filter(|r| fuzzy_matches(&r.label, query) || fuzzy_matches(&r.repo_ref, query))
            .cloned(),
    );
    out
}

/// Case-insensitive subsequence match: every char of `query`, in order, appears
/// somewhere in `candidate`. An empty query matches everything (the F3 fuzzy
/// filter — the daemon's roster order is preserved by the caller).
fn fuzzy_matches(candidate: &str, query: &str) -> bool {
    let mut q = query.chars().flat_map(char::to_lowercase).peekable();
    for c in candidate.chars().flat_map(char::to_lowercase) {
        match q.peek() {
            Some(&qc) if qc == c => {
                q.next();
            }
            Some(_) => {}
            None => break,
        }
    }
    q.peek().is_none()
}

/// An interactive text/pick overlay open over the Boards body.
///
/// The card-level keys open one of these instead of firing a bare intent, so a
/// card create carries a typed title + a picked profile and a column rename
/// carries a typed name. The reducer folds keystrokes into the open overlay and
/// raises the matching [`BoardsIntent`] on commit; the glue lifts it to a daemon
/// RPC (`project_ainb_plugin_owns_data_plane` — the daemon owns the mutation).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BoardsOverlay {
    /// Typing a new card's title on `column_id` (`c`, stage 1). Enter advances to
    /// the repo pick (F1 overlay order: title → repo → agent → profile → column).
    CardTitle {
        /// The column the new card lands in.
        column_id: String,
        /// The title typed so far.
        title: String,
    },
    /// Picking the new card's repo (`c`, stage 2 — spec F2/F3). `@` opens the
    /// fuzzy dropdown over the injected roster (scratch always first); Enter on a
    /// highlighted repo advances to the agent chips. Repo is REQUIRED — Enter with
    /// the dropdown closed re-opens it (the pointer at scratch) rather than
    /// advancing repo-less.
    CardRepo {
        /// The column the new card lands in.
        column_id: String,
        /// The title typed in stage 1.
        title: String,
        /// The post-`@` fuzzy query (empty until `@` opens the dropdown).
        query: String,
        /// The dropdown state: `Some(cursor)` while open (after `@`), `None` while
        /// the field is closed (the prompt is shown).
        dropdown: Option<usize>,
    },
    /// Picking the new card's provider agent chip (`c`, stage 3 — spec F1/F4).
    /// ↑↓ move over claude / codex / copilot; Enter advances to the profile pick.
    /// Pre-selected to the cascade default. Copilot is selectable (F8).
    CardAgent {
        /// The column the new card lands in.
        column_id: String,
        /// The title typed in stage 1.
        title: String,
        /// The repo picked in stage 2.
        repo_ref: String,
        /// The highlighted chip (index into [`AgentChip::ALL`]).
        cursor: usize,
    },
    /// Picking the new card's assignee profile (`c`, stage 4 — the title / repo /
    /// agent are set). Enter commits the create; the cursor indexes the injected
    /// profile roster.
    CardProfile {
        /// The column the new card lands in.
        column_id: String,
        /// The title typed in stage 1.
        title: String,
        /// The repo picked in stage 2.
        repo_ref: String,
        /// The agent chosen in stage 3.
        agent: AgentChip,
        /// The highlighted profile (index into the roster).
        cursor: usize,
    },
    /// Typing a column's new name (`r`). Enter commits the rename.
    ColumnRename {
        /// The column being renamed.
        column_id: String,
        /// The name typed so far (seeded with the current name).
        name: String,
    },
    /// Choosing the run mode for the focused card (`Enter`, the D6 `Run ▾`). Enter
    /// commits the highlighted mode.
    RunMode {
        /// The card's issue to launch.
        issue_id: String,
        /// The highlighted mode (index into [`RunMode::ALL`]).
        cursor: usize,
    },
}

/// A raw key folded into an open [`BoardsOverlay`]. The reducer interprets each
/// per the overlay type (a `Char` is text in an input but ignored in a picker;
/// `Up`/`Down` move a picker cursor but are ignored in an input).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoardsKey {
    /// A printable character.
    Char(char),
    /// Backspace (delete the last input char).
    Backspace,
    /// Enter (advance / commit the overlay).
    Enter,
    /// Escape (cancel / step back the overlay).
    Esc,
    /// Cursor up (move a picker selection up).
    Up,
    /// Cursor down (move a picker selection down).
    Down,
}

/// The render-state cache for the Boards screen.
///
/// Holds the boards + a `(board, column, card)` focus cursor, clamped on every
/// mutation so a snapshot refresh never dangles the cursor past the end. Carries
/// the profile roster (injected by the glue for the card-create picker) and the
/// open interactive overlay, both preserved across a `boards_list` refresh.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BoardsState {
    boards: Vec<BoardView>,
    focused_board: usize,
    focused_col: usize,
    focused_card: usize,
    /// The fetch load state — distinguishes empty from loading/error at render.
    status: BoardsStatus,
    /// The assignee-profile roster (slugs) the card-create picker offers, injected
    /// by the glue from its cached `profile/list` and preserved across refreshes.
    profiles: Vec<String>,
    /// The `@`-autocomplete repo roster (spec F3), injected by the glue from
    /// `hangar/repo_list` (favorites-first + recency order preserved) and kept
    /// across refreshes. `scratch` is NOT in here — the reducer prepends it always.
    repos: Vec<RepoOption>,
    /// The agent chip the card-create picker pre-selects (spec F4 cascade),
    /// injected by the glue (defaults to [`AgentChip::Claude`]).
    default_agent: AgentChip,
    /// The open interactive overlay (card create / column rename / run mode), or
    /// `None`. Preserved across a `boards_list` refresh so a background refresh
    /// while typing never drops the input.
    overlay: Option<BoardsOverlay>,
    /// A transient status note (e.g. the attach feedback, or a run's routed
    /// agent), rendered in the title row and cleared by the next mutation.
    note: Option<String>,
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
            profiles: Vec::new(),
            repos: Vec::new(),
            default_agent: AgentChip::default(),
            overlay: None,
            note: None,
        };
        state.clamp();
        state
    }

    /// Set a transient status note (shown in the title row until the next refresh
    /// carries it forward or a fresh note replaces it).
    pub fn set_note(&mut self, note: impl Into<String>) {
        self.note = Some(note.into());
    }

    /// The transient status note, if any.
    #[must_use]
    pub fn note(&self) -> Option<&str> {
        self.note.as_deref()
    }

    /// Inject the assignee-profile roster (slugs) the card-create picker offers.
    /// Called by the glue whenever `profile/list` refreshes; kept out of the wire
    /// snapshot so the pure reducer never depends on IO.
    pub fn set_profiles(&mut self, profiles: Vec<String>) {
        self.profiles = profiles;
    }

    /// The injected assignee-profile roster (slugs).
    #[must_use]
    pub fn profiles(&self) -> &[String] {
        &self.profiles
    }

    /// Inject the `@`-autocomplete repo roster (spec F3), favorites-first order
    /// preserved. Called by the glue whenever `hangar/repo_list` refreshes; kept
    /// out of the wire snapshot so the pure reducer never depends on IO.
    pub fn set_repos(&mut self, repos: Vec<RepoOption>) {
        self.repos = repos;
    }

    /// The injected repo roster (without the always-prepended `scratch`).
    #[must_use]
    pub fn repos(&self) -> &[RepoOption] {
        &self.repos
    }

    /// Inject the agent chip the card-create picker pre-selects (spec F4 cascade).
    pub const fn set_default_agent(&mut self, agent: AgentChip) {
        self.default_agent = agent;
    }

    /// The cascade-default agent chip the card-create picker pre-selects.
    #[must_use]
    pub const fn default_agent(&self) -> AgentChip {
        self.default_agent
    }

    /// The open interactive overlay, if any (the render paints it).
    #[must_use]
    pub const fn overlay(&self) -> Option<&BoardsOverlay> {
        self.overlay.as_ref()
    }

    /// Carry the profile roster + open overlay from `prev` onto a freshly-built
    /// snapshot state, so a `boards_list` refresh never drops the injected roster
    /// or an in-flight input. The clamp re-runs so the carried focus stays valid.
    pub fn adopt_context(&mut self, prev: &Self) {
        self.profiles.clone_from(&prev.profiles);
        self.repos.clone_from(&prev.repos);
        self.default_agent = prev.default_agent;
        self.overlay.clone_from(&prev.overlay);
        self.note.clone_from(&prev.note);
        self.clamp();
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
    /// Cancel the focused card's in-flight run (`C`, tcp T3 / F6). A no-op with
    /// no card focused; a card whose latest run is terminal is refused by the
    /// daemon (reported as a note).
    CancelFocusedCard,
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
    /// A key folded into the open interactive overlay (card create / column
    /// rename / run mode). A no-op when no overlay is open.
    Key(BoardsKey),
}

/// A side-effect the plugin glue performs after a Boards reduction — each lifts
/// into a `hangar/board_*` RPC (the daemon owns the real mutation).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BoardsIntent {
    /// Create a new board (`hangar/board_create`). Raised unconditionally — it is
    /// the empty-state affordance, so it must fire even with no board focused.
    CreateBoard,
    /// Launch the card's issue on its assignee profile now, in `mode`
    /// (`hangar/board_card_run`). Card = issue (D6, D16).
    RunCard {
        /// The board the card sits on.
        board_id: String,
        /// The issue to dispatch.
        issue_id: String,
        /// The chosen launch mode (headless / interactive).
        mode: RunMode,
    },
    /// Attach to the card's running session (the existing attach affordance).
    AttachCard {
        /// The board the card sits on.
        board_id: String,
        /// The issue whose session to attach to.
        issue_id: String,
    },
    /// Cancel the card's in-flight run (`hangar/board_card_cancel`, tcp T3 / F6).
    /// Card = issue: the daemon resolves the issue's active task and kills it.
    CancelCard {
        /// The board the card sits on.
        board_id: String,
        /// The issue whose active run to cancel.
        issue_id: String,
    },
    /// Prompt for a name and add a column (`hangar/board_column_add`).
    AddColumn {
        /// The board to append a column to.
        board_id: String,
    },
    /// Rename the focused column to the typed name
    /// (`hangar/board_column_update`).
    RenameColumn {
        /// The board the column belongs to.
        board_id: String,
        /// The column to rename.
        column_id: String,
        /// The new column name (non-blank).
        name: String,
    },
    /// Delete the focused column (`hangar/board_column_delete`); cards park
    /// unmapped.
    DeleteColumn {
        /// The board the column belongs to.
        board_id: String,
        /// The column to delete.
        column_id: String,
    },
    /// Create a new card (issue) from the typed title + picked repo + agent +
    /// assignee profile and place it in the focused column
    /// (`hangar/board_card_create`, spec F1-F4).
    CreateCard {
        /// The board to add the card to.
        board_id: String,
        /// The column to place the card in.
        column_id: String,
        /// The new issue's title (non-blank).
        title: String,
        /// The picked repo (an absolute checkout path or `scratch`) — REQUIRED (F2).
        repo_ref: String,
        /// The picked provider agent (spec F1/F4).
        agent: AgentChip,
        /// The picked assignee profile slug, or `None` (unassigned).
        assignee_profile: Option<String>,
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
        // Run opens the `Run ▾` mode picker over the focused card (Enter commits
        // the highlighted mode). A no-op with no card focused.
        BoardsEvent::RunFocusedCard => open_overlay(state, |_, c| BoardsOverlay::RunMode {
            issue_id: c.issue_id.clone(),
            cursor: 0,
        }),
        BoardsEvent::AttachFocusedCard => card_intent(state, |b, c| BoardsIntent::AttachCard {
            board_id: b.id.clone(),
            issue_id: c.issue_id.clone(),
        }),
        BoardsEvent::CancelFocusedCard => card_intent(state, |b, c| BoardsIntent::CancelCard {
            board_id: b.id.clone(),
            issue_id: c.issue_id.clone(),
        }),
        BoardsEvent::AddColumn => board_intent(state, |b| BoardsIntent::AddColumn {
            board_id: b.id.clone(),
        }),
        // Rename opens the column-name input seeded with the current name.
        BoardsEvent::RenameColumn => open_column_overlay(state, |col| BoardsOverlay::ColumnRename {
            column_id: col.id.clone(),
            name: col.name.clone(),
        }),
        BoardsEvent::DeleteColumn => column_intent(state, |b, col| BoardsIntent::DeleteColumn {
            board_id: b.id.clone(),
            column_id: col.id.clone(),
        }),
        // Add-card opens the title input on the focused column (stage 1 of the
        // create; Enter advances to the profile pick).
        BoardsEvent::AddCard => open_column_overlay(state, |col| BoardsOverlay::CardTitle {
            column_id: col.id.clone(),
            title: String::new(),
        }),
        BoardsEvent::ReorderColumnLeft => reorder(state, -1),
        BoardsEvent::ReorderColumnRight => reorder(state, 1),
        BoardsEvent::ToggleAutoMove => board_intent(state, |b| BoardsIntent::ToggleAutoMove {
            board_id: b.id.clone(),
            auto_move: !b.auto_move,
        }),
        BoardsEvent::Key(k) => reduce_overlay_key(state, k),
    }
}

/// Fold a key into the open overlay. A no-op when no overlay is open (so a stray
/// key never mutates the board). Each overlay interprets the key by its type.
fn reduce_overlay_key(state: &BoardsState, key: BoardsKey) -> BoardsReduction {
    let Some(overlay) = state.overlay.clone() else {
        return unchanged(state);
    };
    match overlay {
        BoardsOverlay::CardTitle { column_id, title } => {
            card_title_key(state, &column_id, title, key)
        }
        BoardsOverlay::CardRepo {
            column_id,
            title,
            query,
            dropdown,
        } => card_repo_key(state, &column_id, title, query, dropdown, key),
        BoardsOverlay::CardAgent {
            column_id,
            title,
            repo_ref,
            cursor,
        } => card_agent_key(state, &column_id, title, repo_ref, cursor, key),
        BoardsOverlay::CardProfile {
            column_id,
            title,
            repo_ref,
            agent,
            cursor,
        } => card_profile_key(state, &column_id, title, repo_ref, agent, cursor, key),
        BoardsOverlay::ColumnRename { column_id, name } => {
            column_rename_key(state, &column_id, name, key)
        }
        BoardsOverlay::RunMode { issue_id, cursor } => {
            run_mode_key(state, &issue_id, cursor, key)
        }
    }
}

/// Stage 1 of card create: type the title. Enter advances to the profile pick
/// (blank title holds the input open); Esc cancels; Backspace edits.
fn card_title_key(
    state: &BoardsState,
    column_id: &str,
    mut title: String,
    key: BoardsKey,
) -> BoardsReduction {
    match key {
        BoardsKey::Esc => close_overlay(state),
        BoardsKey::Backspace => {
            title.pop();
            set_overlay(
                state,
                BoardsOverlay::CardTitle {
                    column_id: column_id.to_string(),
                    title,
                },
            )
        }
        BoardsKey::Char(c) => {
            title.push(c);
            set_overlay(
                state,
                BoardsOverlay::CardTitle {
                    column_id: column_id.to_string(),
                    title,
                },
            )
        }
        BoardsKey::Enter => {
            if title.trim().is_empty() {
                // A blank title is not a card — keep the input open.
                return set_overlay(
                    state,
                    BoardsOverlay::CardTitle {
                        column_id: column_id.to_string(),
                        title,
                    },
                );
            }
            // Advance to the repo pick (F1 order: title → repo → agent → profile).
            set_overlay(
                state,
                BoardsOverlay::CardRepo {
                    column_id: column_id.to_string(),
                    title,
                    query: String::new(),
                    dropdown: None,
                },
            )
        }
        BoardsKey::Up | BoardsKey::Down => set_overlay(
            state,
            BoardsOverlay::CardTitle {
                column_id: column_id.to_string(),
                title,
            },
        ),
    }
}

/// Stage 2 of card create: pick the repo (spec F2/F3). `@` opens the fuzzy
/// dropdown over the injected roster (scratch always first); ↑↓ move the
/// highlight, Enter picks it and advances to the agent chips. Repo is REQUIRED —
/// Enter with the dropdown closed re-opens it (the pointer at scratch) rather
/// than advancing repo-less. Esc closes an open dropdown, else steps back to the
/// title input.
fn card_repo_key(
    state: &BoardsState,
    column_id: &str,
    title: String,
    mut query: String,
    dropdown: Option<usize>,
    key: BoardsKey,
) -> BoardsReduction {
    let reopen = |state: &BoardsState, query: String, dropdown: Option<usize>| {
        set_overlay(
            state,
            BoardsOverlay::CardRepo {
                column_id: column_id.to_string(),
                title: title.clone(),
                query,
                dropdown,
            },
        )
    };
    match (dropdown, key) {
        // Field closed: `@` opens the dropdown; Esc steps back to the title.
        (None, BoardsKey::Esc) => set_overlay(
            state,
            BoardsOverlay::CardTitle {
                column_id: column_id.to_string(),
                title,
            },
        ),
        (None, BoardsKey::Char('@')) => reopen(state, String::new(), Some(0)),
        // Enter with no repo picked yet re-opens the dropdown (F2: repo REQUIRED,
        // scratch always first) rather than advancing.
        (None, BoardsKey::Enter) => reopen(state, String::new(), Some(0)),
        (None, _) => reopen(state, query, None),
        // Dropdown open: Esc closes it back to the field.
        (Some(_), BoardsKey::Esc) => reopen(state, String::new(), None),
        (Some(_), BoardsKey::Char(c)) => {
            // A second `@` is a literal filter char, not a re-open. Any edit resets
            // the highlight to the top of the (re-filtered) candidate list.
            query.push(c);
            reopen(state, query, Some(0))
        }
        (Some(_), BoardsKey::Backspace) => {
            query.pop();
            reopen(state, query, Some(0))
        }
        (Some(cursor), BoardsKey::Up) => {
            reopen(state, query, Some(cursor.saturating_sub(1)))
        }
        (Some(cursor), BoardsKey::Down) => {
            let n = repo_candidates(state.repos(), &query).len();
            reopen(state, query, Some((cursor + 1).min(n.saturating_sub(1))))
        }
        (Some(cursor), BoardsKey::Enter) => {
            let candidates = repo_candidates(state.repos(), &query);
            let Some(picked) = candidates.get(cursor).or_else(|| candidates.first()) else {
                // Impossible (scratch is always present), but never advance repo-less.
                return reopen(state, query, Some(0));
            };
            set_overlay(
                state,
                BoardsOverlay::CardAgent {
                    column_id: column_id.to_string(),
                    title,
                    repo_ref: picked.repo_ref.clone(),
                    cursor: state.default_agent().index(),
                },
            )
        }
    }
}

/// Stage 3 of card create: pick the provider agent chip (spec F1/F4). ↑↓ move
/// over claude / codex / copilot; Enter advances to the profile pick. Copilot is
/// selectable (F8 — the dispatch gate fires at run). Esc steps back to the repo
/// pick.
fn card_agent_key(
    state: &BoardsState,
    column_id: &str,
    title: String,
    repo_ref: String,
    cursor: usize,
    key: BoardsKey,
) -> BoardsReduction {
    let reopen = |state: &BoardsState, cursor: usize| {
        set_overlay(
            state,
            BoardsOverlay::CardAgent {
                column_id: column_id.to_string(),
                title: title.clone(),
                repo_ref: repo_ref.clone(),
                cursor,
            },
        )
    };
    match key {
        BoardsKey::Esc => set_overlay(
            state,
            BoardsOverlay::CardRepo {
                column_id: column_id.to_string(),
                title,
                query: String::new(),
                dropdown: None,
            },
        ),
        BoardsKey::Up => reopen(state, cursor.saturating_sub(1)),
        BoardsKey::Down => reopen(state, (cursor + 1).min(AgentChip::ALL.len() - 1)),
        BoardsKey::Enter => set_overlay(
            state,
            BoardsOverlay::CardProfile {
                column_id: column_id.to_string(),
                title,
                repo_ref,
                agent: AgentChip::at(cursor),
                cursor: 0,
            },
        ),
        BoardsKey::Char(_) | BoardsKey::Backspace => reopen(state, cursor),
    }
}

/// Stage 4 of card create: pick the assignee profile. Up/Down move the cursor
/// over the injected roster; Enter commits the create carrying the title + repo +
/// agent + profile (with a `None` profile when the roster is empty); Esc steps
/// back to the agent chips.
fn card_profile_key(
    state: &BoardsState,
    column_id: &str,
    title: String,
    repo_ref: String,
    agent: AgentChip,
    cursor: usize,
    key: BoardsKey,
) -> BoardsReduction {
    let n = state.profiles.len();
    let reopen = |state: &BoardsState, cursor: usize| {
        set_overlay(
            state,
            BoardsOverlay::CardProfile {
                column_id: column_id.to_string(),
                title: title.clone(),
                repo_ref: repo_ref.clone(),
                agent,
                cursor,
            },
        )
    };
    match key {
        BoardsKey::Esc => set_overlay(
            state,
            BoardsOverlay::CardAgent {
                column_id: column_id.to_string(),
                title,
                repo_ref,
                cursor: agent.index(),
            },
        ),
        BoardsKey::Up => reopen(state, cursor.saturating_sub(1)),
        BoardsKey::Down => reopen(state, (cursor + 1).min(n.saturating_sub(1))),
        BoardsKey::Enter => {
            let Some(board) = state.focused_board() else {
                return close_overlay(state);
            };
            let assignee_profile = state.profiles.get(cursor).cloned();
            let mut next = state.clone();
            next.overlay = None;
            BoardsReduction {
                state: next,
                intent: Some(BoardsIntent::CreateCard {
                    board_id: board.id.clone(),
                    column_id: column_id.to_string(),
                    title,
                    repo_ref,
                    agent,
                    assignee_profile,
                }),
            }
        }
        BoardsKey::Char(_) | BoardsKey::Backspace => reopen(state, cursor),
    }
}

/// Column rename input: type the new name. Enter commits (blank holds the input
/// open); Esc cancels; Backspace edits.
fn column_rename_key(
    state: &BoardsState,
    column_id: &str,
    mut name: String,
    key: BoardsKey,
) -> BoardsReduction {
    let reopen = |state: &BoardsState, name: String| {
        set_overlay(
            state,
            BoardsOverlay::ColumnRename {
                column_id: column_id.to_string(),
                name,
            },
        )
    };
    match key {
        BoardsKey::Esc => close_overlay(state),
        BoardsKey::Backspace => {
            name.pop();
            reopen(state, name)
        }
        BoardsKey::Char(c) => {
            name.push(c);
            reopen(state, name)
        }
        BoardsKey::Up | BoardsKey::Down => reopen(state, name),
        BoardsKey::Enter => {
            if name.trim().is_empty() {
                return reopen(state, name);
            }
            let Some(board) = state.focused_board() else {
                return close_overlay(state);
            };
            let mut next = state.clone();
            next.overlay = None;
            BoardsReduction {
                state: next,
                intent: Some(BoardsIntent::RenameColumn {
                    board_id: board.id.clone(),
                    column_id: column_id.to_string(),
                    name,
                }),
            }
        }
    }
}

/// The `Run ▾` mode picker: Up/Down toggle Headless/Interactive; Enter commits
/// the highlighted mode as a [`BoardsIntent::RunCard`]; Esc cancels.
fn run_mode_key(
    state: &BoardsState,
    issue_id: &str,
    cursor: usize,
    key: BoardsKey,
) -> BoardsReduction {
    match key {
        BoardsKey::Esc => close_overlay(state),
        BoardsKey::Up => set_overlay(
            state,
            BoardsOverlay::RunMode {
                issue_id: issue_id.to_string(),
                cursor: cursor.saturating_sub(1),
            },
        ),
        BoardsKey::Down => set_overlay(
            state,
            BoardsOverlay::RunMode {
                issue_id: issue_id.to_string(),
                cursor: (cursor + 1).min(RunMode::ALL.len() - 1),
            },
        ),
        BoardsKey::Enter => {
            let Some(board) = state.focused_board() else {
                return close_overlay(state);
            };
            let mode = RunMode::ALL.get(cursor).copied().unwrap_or(RunMode::Headless);
            let mut next = state.clone();
            next.overlay = None;
            BoardsReduction {
                state: next,
                intent: Some(BoardsIntent::RunCard {
                    board_id: board.id.clone(),
                    issue_id: issue_id.to_string(),
                    mode,
                }),
            }
        }
        BoardsKey::Char(_) | BoardsKey::Backspace => set_overlay(
            state,
            BoardsOverlay::RunMode {
                issue_id: issue_id.to_string(),
                cursor,
            },
        ),
    }
}

/// Open an overlay derived from the focused board + focused card, if both exist.
fn open_overlay(
    state: &BoardsState,
    f: impl Fn(&BoardView, &CardView) -> BoardsOverlay,
) -> BoardsReduction {
    match (state.focused_board(), state.focused_card()) {
        (Some(b), Some(c)) => set_overlay(state, f(b, c)),
        _ => unchanged(state),
    }
}

/// Open an overlay derived from the focused column, if one exists.
fn open_column_overlay(
    state: &BoardsState,
    f: impl Fn(&ColumnView) -> BoardsOverlay,
) -> BoardsReduction {
    match state.focused_column() {
        Some(col) => set_overlay(state, f(col)),
        None => unchanged(state),
    }
}

/// Replace the open overlay, emitting no intent.
fn set_overlay(state: &BoardsState, overlay: BoardsOverlay) -> BoardsReduction {
    let mut next = state.clone();
    next.overlay = Some(overlay);
    no_intent(next)
}

/// Close any open overlay, emitting no intent.
fn close_overlay(state: &BoardsState) -> BoardsReduction {
    let mut next = state.clone();
    next.overlay = None;
    no_intent(next)
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
    x = put_str(buf, x, top, toggle, colour, area_w);
    // A transient note (attach feedback / run's routed agent) trails the toggle.
    if let Some(note) = state.note() {
        x = put_str(buf, x, top, "   ", MUTED, area_w);
        put_str(buf, x, top, note, GOLD, area_w);
    }

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

    // An open interactive overlay paints a banner over the top body rows so the
    // typed title / picked profile / run mode is visible (and greppable by the
    // e2e tripwire) without a full modal layer.
    if let Some(overlay) = state.overlay() {
        render_overlay(buf, area_w, body_top, overlay, state.profiles(), state.repos());
    }
}

/// Paint the open overlay as a two-row banner at `row`: a prompt line plus its
/// current value / selection. Kept text-only (no box) so it reads on an 80-col
/// pane and a tripwire can assert the label + typed value.
fn render_overlay(
    buf: &mut WireBuffer,
    area_w: u16,
    row: u16,
    overlay: &BoardsOverlay,
    profiles: &[String],
    repos: &[RepoOption],
) {
    let value_row = row.saturating_add(1);
    match overlay {
        BoardsOverlay::CardTitle { title, .. } => {
            put_str(buf, 0, row, "New card title (Enter → repo, Esc cancel):", GOLD, area_w);
            let shown = format!("> {title}\u{2588}");
            put_str(buf, 0, value_row, &shown, GREEN, area_w);
        }
        BoardsOverlay::CardRepo { title, query, dropdown, .. } => {
            render_card_repo(buf, area_w, row, value_row, title, query, *dropdown, repos);
        }
        BoardsOverlay::CardAgent { title, cursor, .. } => {
            let prompt = format!("Agent for \"{title}\" (↑↓ pick, Enter → profile):");
            put_str(buf, 0, row, &prompt, GOLD, area_w);
            let mut x = 0u16;
            for (i, chip) in AgentChip::ALL.iter().enumerate() {
                let sel = i == *cursor;
                let colour = if sel { GREEN } else { MUTED };
                let marker = if sel { "▶ " } else { "  " };
                x = put_str(buf, x, value_row, marker, colour, area_w);
                x = put_str(buf, x, value_row, chip.label(), colour, area_w);
                x = put_str(buf, x, value_row, "   ", MUTED, area_w);
                if x >= area_w {
                    break;
                }
            }
        }
        BoardsOverlay::CardProfile { title, cursor, .. } => {
            let prompt = format!("Assignee profile for \"{title}\" (↑↓ pick, Enter run-ready):");
            put_str(buf, 0, row, &prompt, GOLD, area_w);
            if profiles.is_empty() {
                put_str(buf, 0, value_row, "> (no profiles — card is unassigned)", MUTED, area_w);
            } else {
                let mut x = 0u16;
                for (i, slug) in profiles.iter().enumerate() {
                    let sel = i == *cursor;
                    let marker = if sel { "[" } else { " " };
                    let end = if sel { "]" } else { " " };
                    let colour = if sel { GREEN } else { MUTED };
                    x = put_str(buf, x, value_row, marker, colour, area_w);
                    x = put_str(buf, x, value_row, slug, colour, area_w);
                    x = put_str(buf, x, value_row, end, colour, area_w);
                    x = put_str(buf, x, value_row, " ", MUTED, area_w);
                    if x >= area_w {
                        break;
                    }
                }
            }
        }
        BoardsOverlay::ColumnRename { name, .. } => {
            put_str(buf, 0, row, "Rename column (Enter commit, Esc cancel):", GOLD, area_w);
            let shown = format!("> {name}\u{2588}");
            put_str(buf, 0, value_row, &shown, GREEN, area_w);
        }
        BoardsOverlay::RunMode { cursor, .. } => {
            put_str(buf, 0, row, "Run ▾ (↑↓ pick mode, Enter launch, Esc cancel):", GOLD, area_w);
            let mut x = 0u16;
            for (i, mode) in RunMode::ALL.iter().enumerate() {
                let sel = i == *cursor;
                let colour = if sel { GREEN } else { MUTED };
                let marker = if sel { "▶ " } else { "  " };
                x = put_str(buf, x, value_row, marker, colour, area_w);
                x = put_str(buf, x, value_row, mode.label(), colour, area_w);
                x = put_str(buf, x, value_row, "   ", MUTED, area_w);
                if x >= area_w {
                    break;
                }
            }
        }
    }
}

/// Render the card-create repo stage (spec F2/F3): a prompt line plus either the
/// closed-field hint (type `@` to search) or the open `@` dropdown — scratch
/// always first (★ for favorites), the injected roster fuzzy-filtered on `query`,
/// the highlighted candidate in green. Text-only so a tripwire can assert the
/// label + the picked value on an 80-col pane.
fn render_card_repo(
    buf: &mut WireBuffer,
    area_w: u16,
    row: u16,
    value_row: u16,
    title: &str,
    query: &str,
    dropdown: Option<usize>,
    repos: &[RepoOption],
) {
    let prompt = format!("Repo for \"{title}\" (@ to search, Enter pick — REQUIRED):");
    put_str(buf, 0, row, &prompt, GOLD, area_w);
    let Some(cursor) = dropdown else {
        // Field closed: point at scratch (the always-available F2 fallback).
        put_str(
            buf,
            0,
            value_row,
            "> type @ to pick a repo (scratch always available)",
            MUTED,
            area_w,
        );
        return;
    };
    let candidates = repo_candidates(repos, query);
    let mut x = put_str(buf, 0, value_row, &format!("@{query} "), GREEN, area_w);
    for (i, repo) in candidates.iter().enumerate() {
        let sel = i == cursor;
        let colour = if sel { GREEN } else { MUTED };
        let open = if sel { "[" } else { " " };
        let close = if sel { "]" } else { " " };
        let star = if repo.is_favorite { "★" } else { "" };
        x = put_str(buf, x, value_row, open, colour, area_w);
        x = put_str(buf, x, value_row, star, colour, area_w);
        x = put_str(buf, x, value_row, &repo.label, colour, area_w);
        x = put_str(buf, x, value_row, close, colour, area_w);
        x = put_str(buf, x, value_row, " ", MUTED, area_w);
        if x >= area_w {
            break;
        }
    }
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
    // Card-lifecycle verbs sit next to the card controls (F6): `↵` runs a
    // runnable card AND reruns a finished/failed/cancelled one (same launch
    // path), `C` cancels a running one. `feedback_keybinding_hints_near_control`.
    let hints: [(&str, &str); 9] = [
        ("↵", "run/rerun"),
        ("a", "attach"),
        ("C", "cancel"),
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
            session_name: None,
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

    /// Enter on a focused card opens the `Run ▾` mode picker (no intent yet);
    /// Enter again commits the highlighted mode as a RunCard intent.
    #[test]
    fn run_focused_card_opens_mode_picker_then_commits() {
        let state = BoardsState::from_snapshot(&one_board());
        // Open `Run ▾` — an overlay, not an immediate dispatch.
        let opened = reduce_boards(&state, BoardsEvent::RunFocusedCard);
        assert_eq!(opened.intent, None, "opening the picker fires nothing");
        assert!(matches!(
            opened.state.overlay(),
            Some(BoardsOverlay::RunMode { issue_id, cursor: 0 }) if issue_id == "issue-1"
        ));
        // Enter on the default (Headless) commits the run.
        let run = reduce_boards(&opened.state, BoardsEvent::Key(BoardsKey::Enter));
        assert_eq!(
            run.intent,
            Some(BoardsIntent::RunCard {
                board_id: "b1".into(),
                issue_id: "issue-1".into(),
                mode: RunMode::Headless,
            })
        );
        assert!(run.state.overlay().is_none(), "the picker closes on commit");
    }

    /// `C` on a focused card raises a CancelCard intent carrying the board +
    /// issue (tcp T3 / F6); it is an immediate action, not an overlay.
    #[test]
    fn cancel_focused_card_raises_cancel_intent() {
        let state = BoardsState::from_snapshot(&one_board());
        let out = reduce_boards(&state, BoardsEvent::CancelFocusedCard);
        assert_eq!(
            out.intent,
            Some(BoardsIntent::CancelCard {
                board_id: "b1".into(),
                issue_id: "issue-1".into(),
            })
        );
        assert!(out.state.overlay().is_none(), "cancel opens no overlay");
    }

    /// `C` with no card focused (an empty column) raises nothing — a stray cancel
    /// never fires a card-less RPC.
    #[test]
    fn cancel_with_no_card_focused_is_a_noop() {
        let state = BoardsState::from_snapshot(&one_board());
        // Move right to the empty `Doing` column.
        let empty = reduce_boards(&state, BoardsEvent::FocusRight);
        assert!(empty.state.focused_card().is_none());
        let out = reduce_boards(&empty.state, BoardsEvent::CancelFocusedCard);
        assert_eq!(out.intent, None, "no card focused → no cancel intent");
    }

    /// Rerun (tcp T3 / F6) rides the existing run affordance: `Enter` on a
    /// FINISHED (`done`) card opens the same `Run ▾` picker and commits a fresh
    /// RunCard — the daemon enqueues a new task (fresh worktree), so a finished
    /// card is rerunnable with no distinct keybinding.
    #[test]
    fn enter_reruns_a_finished_card() {
        let state = BoardsState::from_snapshot(&one_board());
        // Focus the Done column's finished card (issue-2, state = done).
        let state = reduce_boards(&state, BoardsEvent::FocusRight).state; // Doing (empty)
        let state = reduce_boards(&state, BoardsEvent::FocusRight).state; // Done
        assert_eq!(state.focused_card().unwrap().issue_id, "issue-2");
        assert_eq!(state.focused_card().unwrap().state.as_deref(), Some("done"));
        // Enter opens the run picker even though the card is terminal (rerun).
        let opened = reduce_boards(&state, BoardsEvent::RunFocusedCard);
        assert!(matches!(
            opened.state.overlay(),
            Some(BoardsOverlay::RunMode { issue_id, .. }) if issue_id == "issue-2"
        ));
        let rerun = reduce_boards(&opened.state, BoardsEvent::Key(BoardsKey::Enter));
        assert_eq!(
            rerun.intent,
            Some(BoardsIntent::RunCard {
                board_id: "b1".into(),
                issue_id: "issue-2".into(),
                mode: RunMode::Headless,
            }),
            "Enter on a done card reruns it via board_card_run"
        );
    }

    /// Down in `Run ▾` selects Interactive, and Enter commits that mode.
    #[test]
    fn run_mode_picker_selects_interactive() {
        let state = BoardsState::from_snapshot(&one_board());
        let opened = reduce_boards(&state, BoardsEvent::RunFocusedCard);
        let moved = reduce_boards(&opened.state, BoardsEvent::Key(BoardsKey::Down));
        let run = reduce_boards(&moved.state, BoardsEvent::Key(BoardsKey::Enter));
        assert_eq!(
            run.intent,
            Some(BoardsIntent::RunCard {
                board_id: "b1".into(),
                issue_id: "issue-1".into(),
                mode: RunMode::Interactive,
            })
        );
    }

    /// Type a title char-by-char, then drive the overlay to the given `key`.
    fn typed_card(state: &BoardsState, title: &str) -> BoardsState {
        let mut s = reduce_boards(state, BoardsEvent::AddCard).state;
        for ch in title.chars() {
            s = reduce_boards(&s, BoardsEvent::Key(BoardsKey::Char(ch))).state;
        }
        s
    }

    /// A repo roster with one ★ favorite and one scanned repo (favorites-first,
    /// as the daemon returns them).
    fn repo_roster() -> Vec<RepoOption> {
        vec![
            RepoOption { label: "ainb".into(), repo_ref: "/src/ainb".into(), is_favorite: true },
            RepoOption { label: "widget".into(), repo_ref: "/src/widget".into(), is_favorite: false },
        ]
    }

    /// The full F1-F4 card-create flow: title → repo (`@` → pick a favorite) →
    /// agent chip → profile → commit, raising CreateCard carrying every field.
    #[test]
    fn add_card_full_flow_repo_agent_profile_commits() {
        let mut state = BoardsState::from_snapshot(&one_board());
        state.set_profiles(vec!["claude-agent".into(), "codex-agent".into()]);
        state.set_repos(repo_roster());

        // Title stage.
        let s = typed_card(&state, "Wire cards");
        // Enter → repo stage (field closed until `@`).
        let s = reduce_boards(&s, BoardsEvent::Key(BoardsKey::Enter)).state;
        assert!(matches!(
            s.overlay(),
            Some(BoardsOverlay::CardRepo { title, dropdown: None, .. }) if title == "Wire cards"
        ));
        // `@` opens the dropdown (scratch first, then the roster).
        let s = reduce_boards(&s, BoardsEvent::Key(BoardsKey::Char('@'))).state;
        assert!(matches!(s.overlay(), Some(BoardsOverlay::CardRepo { dropdown: Some(0), .. })));
        // Down to the ★ favorite (index 1 — scratch is 0), Enter picks it → agent.
        let s = reduce_boards(&s, BoardsEvent::Key(BoardsKey::Down)).state;
        let s = reduce_boards(&s, BoardsEvent::Key(BoardsKey::Enter)).state;
        assert!(matches!(
            s.overlay(),
            Some(BoardsOverlay::CardAgent { repo_ref, cursor: 0, .. }) if repo_ref == "/src/ainb"
        ));
        // Default agent is claude (cursor 0); Down selects codex, Enter → profile.
        let s = reduce_boards(&s, BoardsEvent::Key(BoardsKey::Down)).state;
        let s = reduce_boards(&s, BoardsEvent::Key(BoardsKey::Enter)).state;
        assert!(matches!(
            s.overlay(),
            Some(BoardsOverlay::CardProfile { agent: AgentChip::Codex, repo_ref, .. })
                if repo_ref == "/src/ainb"
        ));
        // Down to the second profile, Enter commits with every picked field.
        let s = reduce_boards(&s, BoardsEvent::Key(BoardsKey::Down)).state;
        let out = reduce_boards(&s, BoardsEvent::Key(BoardsKey::Enter));
        assert_eq!(
            out.intent,
            Some(BoardsIntent::CreateCard {
                board_id: "b1".into(),
                column_id: "c1".into(),
                title: "Wire cards".into(),
                repo_ref: "/src/ainb".into(),
                agent: AgentChip::Codex,
                assignee_profile: Some("codex-agent".into()),
            })
        );
        assert!(out.state.overlay().is_none());
    }

    /// Typing after `@` fuzzy-filters the dropdown; Enter picks the highlighted
    /// scanned repo. Scratch stays index 0 (always first).
    #[test]
    fn repo_dropdown_fuzzy_filters_on_query() {
        let mut state = BoardsState::from_snapshot(&one_board());
        state.set_repos(repo_roster());
        let s = typed_card(&state, "T");
        let s = reduce_boards(&s, BoardsEvent::Key(BoardsKey::Enter)).state; // → repo
        let s = reduce_boards(&s, BoardsEvent::Key(BoardsKey::Char('@'))).state;
        // Type "wid" — only "widget" (and scratch, always-first) survive the filter.
        let mut s = s;
        for ch in "wid".chars() {
            s = reduce_boards(&s, BoardsEvent::Key(BoardsKey::Char(ch))).state;
        }
        // Candidates are [scratch, widget]; Down + Enter picks widget.
        let s = reduce_boards(&s, BoardsEvent::Key(BoardsKey::Down)).state;
        let s = reduce_boards(&s, BoardsEvent::Key(BoardsKey::Enter)).state;
        assert!(matches!(
            s.overlay(),
            Some(BoardsOverlay::CardAgent { repo_ref, .. }) if repo_ref == "/src/widget"
        ));
    }

    /// F2 repo-REQUIRED: Enter with the dropdown closed re-opens it (pointing at
    /// scratch) rather than advancing repo-less — no intent, still in the repo
    /// stage.
    #[test]
    fn repo_required_enter_without_pick_reopens_dropdown() {
        let state = BoardsState::from_snapshot(&one_board());
        let s = typed_card(&state, "x");
        let s = reduce_boards(&s, BoardsEvent::Key(BoardsKey::Enter)).state; // → repo (closed)
        let out = reduce_boards(&s, BoardsEvent::Key(BoardsKey::Enter));
        assert_eq!(out.intent, None, "repo-less create never commits");
        assert!(
            matches!(out.state.overlay(), Some(BoardsOverlay::CardRepo { dropdown: Some(0), .. })),
            "Enter with no repo re-opens the dropdown at scratch"
        );
    }

    /// With no roster injected, the dropdown still offers scratch (always first),
    /// so a repo-less workspace can always launch.
    #[test]
    fn repo_dropdown_offers_scratch_with_no_roster() {
        let state = BoardsState::from_snapshot(&one_board());
        let s = typed_card(&state, "x");
        let s = reduce_boards(&s, BoardsEvent::Key(BoardsKey::Enter)).state;
        let s = reduce_boards(&s, BoardsEvent::Key(BoardsKey::Char('@'))).state;
        // Enter on the (only) candidate picks scratch → agent stage.
        let s = reduce_boards(&s, BoardsEvent::Key(BoardsKey::Enter)).state;
        assert!(matches!(
            s.overlay(),
            Some(BoardsOverlay::CardAgent { repo_ref, .. }) if repo_ref == "scratch"
        ));
    }

    /// The agent chips pre-select the injected F4 cascade default; ↑↓ move over
    /// claude / codex / copilot (copilot is selectable — F8 gates at run).
    #[test]
    fn agent_chips_cascade_preselect_and_reach_copilot() {
        let mut state = BoardsState::from_snapshot(&one_board());
        state.set_default_agent(AgentChip::Codex);
        // Drive to the agent stage via scratch.
        let s = typed_card(&state, "x");
        let s = reduce_boards(&s, BoardsEvent::Key(BoardsKey::Enter)).state;
        let s = reduce_boards(&s, BoardsEvent::Key(BoardsKey::Char('@'))).state;
        let s = reduce_boards(&s, BoardsEvent::Key(BoardsKey::Enter)).state; // scratch → agent
        // Cascade default codex is pre-selected (index 1).
        assert!(matches!(s.overlay(), Some(BoardsOverlay::CardAgent { cursor: 1, .. })));
        // Down to copilot (index 2) — selectable, and Enter advances to profile.
        let s = reduce_boards(&s, BoardsEvent::Key(BoardsKey::Down)).state;
        let s = reduce_boards(&s, BoardsEvent::Key(BoardsKey::Enter)).state;
        assert!(matches!(
            s.overlay(),
            Some(BoardsOverlay::CardProfile { agent: AgentChip::Copilot, .. })
        ));
    }

    /// A blank title never advances — Enter holds the title input open.
    #[test]
    fn blank_card_title_holds_the_input_open() {
        let state = BoardsState::from_snapshot(&one_board());
        let s = reduce_boards(&state, BoardsEvent::AddCard).state;
        let out = reduce_boards(&s, BoardsEvent::Key(BoardsKey::Enter));
        assert_eq!(out.intent, None);
        assert!(matches!(out.state.overlay(), Some(BoardsOverlay::CardTitle { .. })));
    }

    /// With no profiles cached, a card still commits with an unassigned profile
    /// (the full flow: title → scratch → default agent → empty profile).
    #[test]
    fn add_card_with_no_profiles_commits_unassigned() {
        let state = BoardsState::from_snapshot(&one_board());
        let s = typed_card(&state, "x");
        let s = reduce_boards(&s, BoardsEvent::Key(BoardsKey::Enter)).state; // → repo
        let s = reduce_boards(&s, BoardsEvent::Key(BoardsKey::Char('@'))).state;
        let s = reduce_boards(&s, BoardsEvent::Key(BoardsKey::Enter)).state; // scratch → agent
        let s = reduce_boards(&s, BoardsEvent::Key(BoardsKey::Enter)).state; // claude → profile
        let out = reduce_boards(&s, BoardsEvent::Key(BoardsKey::Enter));
        assert_eq!(
            out.intent,
            Some(BoardsIntent::CreateCard {
                board_id: "b1".into(),
                column_id: "c1".into(),
                title: "x".into(),
                repo_ref: "scratch".into(),
                agent: AgentChip::Claude,
                assignee_profile: None,
            })
        );
    }

    /// The fuzzy matcher is a case-insensitive subsequence test.
    #[test]
    fn fuzzy_matches_is_case_insensitive_subsequence() {
        assert!(fuzzy_matches("widget", "wid"));
        assert!(fuzzy_matches("MyRepo", "mr"), "subsequence, case-insensitive");
        assert!(fuzzy_matches("anything", ""), "empty query matches all");
        assert!(!fuzzy_matches("ainb", "xyz"));
    }

    /// `r` opens the column-rename input seeded with the current name; edit +
    /// Enter raises RenameColumn with the new name.
    #[test]
    fn rename_column_edits_and_commits() {
        let state = BoardsState::from_snapshot(&one_board());
        let s = reduce_boards(&state, BoardsEvent::RenameColumn).state;
        assert!(matches!(
            s.overlay(),
            Some(BoardsOverlay::ColumnRename { name, .. }) if name == "Todo"
        ));
        // Backspace the "o", append "ay!" → "Today!".
        let s = reduce_boards(&s, BoardsEvent::Key(BoardsKey::Backspace)).state;
        let mut s = s;
        for ch in "ay!".chars() {
            s = reduce_boards(&s, BoardsEvent::Key(BoardsKey::Char(ch))).state;
        }
        let out = reduce_boards(&s, BoardsEvent::Key(BoardsKey::Enter));
        assert_eq!(
            out.intent,
            Some(BoardsIntent::RenameColumn {
                board_id: "b1".into(),
                column_id: "c1".into(),
                name: "Today!".into(),
            })
        );
    }

    /// Esc cancels an open overlay without raising an intent.
    #[test]
    fn esc_cancels_an_open_overlay() {
        let state = BoardsState::from_snapshot(&one_board());
        let s = reduce_boards(&state, BoardsEvent::AddCard).state;
        let out = reduce_boards(&s, BoardsEvent::Key(BoardsKey::Esc));
        assert_eq!(out.intent, None);
        assert!(out.state.overlay().is_none(), "Esc closes the overlay");
    }

    /// A refresh preserves the injected profile roster + an in-flight overlay so a
    /// background `boards_list` reply never drops the user's typing.
    #[test]
    fn refresh_preserves_profiles_and_open_overlay() {
        let mut state = BoardsState::from_snapshot(&one_board());
        state.set_profiles(vec!["claude-agent".into()]);
        let typing = reduce_boards(&state, BoardsEvent::AddCard).state;
        assert!(typing.overlay().is_some());

        let mut refreshed = BoardsState::from_snapshot(&one_board());
        refreshed.adopt_context(&typing);
        assert_eq!(refreshed.profiles(), ["claude-agent"]);
        assert!(refreshed.overlay().is_some(), "the open title input survives a refresh");
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
