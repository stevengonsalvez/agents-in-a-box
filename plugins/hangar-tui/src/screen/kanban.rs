//! P8.4 — Kanban board screen: four columns + card widget.
//!
//! The Kanban board (hotkey `K`) lays the workspace's task queue out as four
//! width-aware columns — `queued` / `running` / `done` / `failed` — each holding
//! the task cards bucketed into it. A card shows `#<short_id>`, the assignee
//! agent, the task age (`5m` / `2h` / `3d`), and a coloured status chip. Focus
//! walks columns with `←` / `→` and rows with `↑` / `↓`; `Shift+←` / `Shift+→`
//! drags the focused card to the adjacent column, which the plugin glue lifts
//! into a `hangar/task_transition` daemon RPC (the real store FSM move, not a
//! local re-bucket).
//!
//! ## Status → column mapping
//!
//! The real lifecycle has **six** statuses
//! ([`ainb_hangar_core::task_status::TaskStatus`]:
//! `queued/dispatched/running/done/failed/cancelled`); the board has four
//! columns. The mapping ([`BoardColumn::for_status`]) folds the pending pair and
//! the failed pair so every status lands in exactly one column:
//!
//! | column    | statuses                |
//! |-----------|-------------------------|
//! | `queued`  | `queued`, `dispatched`  |
//! | `running` | `running`               |
//! | `done`    | `done`                  |
//! | `failed`  | `failed`, `cancelled`   |
//!
//! An unknown wire token falls into `queued` (fail-visible, not fail-hidden) so a
//! future status never silently drops a card off the board.
//!
//! As with every Hangar screen the reducer ([`reduce_kanban`]) is **pure**: it
//! folds a directional / move / event input into a new [`KanbanState`] plus an
//! optional [`KanbanIntent`] the plugin glue lifts into the matching daemon RPC.
//! The card rows come from the daemon (`hangar/tasks_list`); the plugin owns zero
//! domain data (`project_ainb_plugin_owns_data_plane`).

use ainb_hangar_proto::events::{HangarEvent, TaskCardRow};
use ainb_plugin_sdk::{Cell, Color, Coord, WireBuffer};

/// Title / accent gold.
const GOLD: Color = Color::rgb(255, 215, 0);
/// Selected-card marker + focused-column header green.
const SELECTION_GREEN: Color = Color::rgb(100, 200, 100);
/// Primary card text.
const SOFT_WHITE: Color = Color::rgb(220, 220, 230);
/// Muted text (unfocused headers, hints).
const MUTED_GRAY: Color = Color::rgb(120, 120, 140);
/// Running / active accent (blue).
const RUNNING_BLUE: Color = Color::rgb(100, 149, 237);
/// Failed / cancelled accent (red).
const WARN_RED: Color = Color::rgb(220, 120, 100);

/// The four board columns, in left-to-right display order.
///
/// The mapping from the six wire statuses is owned by [`BoardColumn::for_status`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoardColumn {
    /// Enqueued / dispatched, awaiting a runtime (`queued`, `dispatched`).
    Queued,
    /// Actively executing (`running`).
    Running,
    /// Completed successfully (`done`).
    Done,
    /// Failed or cancelled (`failed`, `cancelled`).
    Failed,
}

impl BoardColumn {
    /// The four columns in left-to-right display order.
    #[must_use]
    pub const fn all() -> [Self; 4] {
        [Self::Queued, Self::Running, Self::Done, Self::Failed]
    }

    /// The column header label (without the count suffix).
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Done => "done",
            Self::Failed => "failed",
        }
    }

    /// Bucket a wire `status` token into its board column.
    ///
    /// Folds the pending pair (`queued`+`dispatched`) and the failed pair
    /// (`failed`+`cancelled`); an unknown token falls into [`Self::Queued`]
    /// (fail-visible) so a new lifecycle status never silently drops a card.
    #[must_use]
    pub fn for_status(status: &str) -> Self {
        match status {
            "running" => Self::Running,
            "done" => Self::Done,
            "failed" | "cancelled" => Self::Failed,
            // "queued", "dispatched", and any unknown token.
            _ => Self::Queued,
        }
    }

    /// The canonical [`ainb_hangar_core::task_status::TaskStatus`] a card lands on
    /// when **dropped into** this column. The pending column targets `queued` (the
    /// re-queue entry point); the failed column targets `cancelled` (the
    /// user-driven terminal, distinct from a runtime `failed`).
    #[must_use]
    pub const fn drop_status(self) -> ainb_hangar_core::task_status::TaskStatus {
        use ainb_hangar_core::task_status::TaskStatus;
        match self {
            Self::Queued => TaskStatus::Queued,
            Self::Running => TaskStatus::Running,
            Self::Done => TaskStatus::Done,
            Self::Failed => TaskStatus::Cancelled,
        }
    }
}

/// A flattened card the board renders — derived from a [`TaskCardRow`].
///
/// Holds only what the card widget paints, so the renderer never re-parses the
/// wire row. `age_label` is computed from `created_at` against a render-time
/// "now" by [`Column::cards_from`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CardSummary {
    /// The full task id (the move RPC carries this).
    pub task_id: String,
    /// The short id rendered on the card (`#<short_id>`).
    pub short_id: String,
    /// The executing agent's id.
    pub agent_id: String,
    /// The raw lifecycle status (drives the status chip colour).
    pub status: String,
    /// Creation timestamp (epoch ms), kept for re-computing age on re-render.
    pub created_at: i64,
}

/// One board column: its status bucket, its cards, and its vertical scroll.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Column {
    /// Which status bucket this column holds.
    pub status: BoardColumn,
    /// The cards in this column, in board order.
    pub cards: Vec<CardSummary>,
    /// First visible card index (vertical scroll within the column).
    pub scroll_offset: usize,
}

impl Column {
    /// An empty column for `status`.
    const fn empty(status: BoardColumn) -> Self {
        Self {
            status,
            cards: Vec::new(),
            scroll_offset: 0,
        }
    }
}

/// The render-state cache for the Kanban board screen.
///
/// Holds the four columns + the focused `(column, row)` cursor. All fields are
/// public-read through accessors; the cursor is clamped on every mutation so a
/// card-move or a snapshot refresh never leaves it dangling past the end.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KanbanState {
    columns: [Column; 4],
    focused_col: usize,
    focused_row: usize,
}

impl Default for KanbanState {
    fn default() -> Self {
        Self {
            columns: BoardColumn::all().map(Column::empty),
            focused_col: 0,
            focused_row: 0,
        }
    }
}

impl KanbanState {
    /// Build the board from a `hangar/tasks_list` snapshot, computing each card's
    /// age against `now_ms`. Cursor is reset to the first column / row and then
    /// clamped to the first non-empty cell so a fresh board lands on a card.
    #[must_use]
    pub fn from_tasks(tasks: &[TaskCardRow], now_ms: i64) -> Self {
        let columns = BoardColumn::all().map(|status| Column {
            status,
            cards: cards_for(tasks, status, now_ms),
            scroll_offset: 0,
        });
        let mut state = Self {
            columns,
            focused_col: 0,
            focused_row: 0,
        };
        state.snap_focus_to_card();
        state
    }

    /// The four columns, left-to-right.
    #[must_use]
    pub const fn columns(&self) -> &[Column; 4] {
        &self.columns
    }

    /// The focused column index (`0..4`).
    #[must_use]
    pub const fn focused_col(&self) -> usize {
        self.focused_col
    }

    /// The focused row index within the focused column.
    #[must_use]
    pub const fn focused_row(&self) -> usize {
        self.focused_row
    }

    /// The card under the focus cursor, if any (the move source).
    #[must_use]
    pub fn focused_card(&self) -> Option<&CardSummary> {
        self.columns[self.focused_col].cards.get(self.focused_row)
    }

    /// Move the focus cursor to the first column that has a card, leaving it on
    /// the empty board's first column when none do. Keeps the row in bounds.
    fn snap_focus_to_card(&mut self) {
        if self.columns[self.focused_col].cards.is_empty() {
            if let Some(idx) = self.columns.iter().position(|c| !c.cards.is_empty()) {
                self.focused_col = idx;
            }
        }
        self.clamp_row();
    }

    /// Clamp `focused_row` into the focused column's bounds (0 when empty).
    fn clamp_row(&mut self) {
        let len = self.columns[self.focused_col].cards.len();
        self.focused_row = self.focused_row.min(len.saturating_sub(1));
        if len == 0 {
            self.focused_row = 0;
        }
    }
}

/// Build the [`CardSummary`] list for one board column from the wire rows.
fn cards_for(tasks: &[TaskCardRow], col: BoardColumn, now_ms: i64) -> Vec<CardSummary> {
    let _ = now_ms; // age is computed at render time so a re-render re-ages.
    tasks
        .iter()
        .filter(|t| BoardColumn::for_status(&t.status) == col)
        .map(|t| CardSummary {
            task_id: t.id.as_str().to_string(),
            short_id: short_id(t.id.as_str()),
            agent_id: t.agent_id.clone(),
            status: t.status.clone(),
            created_at: t.created_at,
        })
        .collect()
}

/// The short id rendered on a card: the last 6 chars of the id (char-safe), or
/// the whole id when it is already short.
fn short_id(id: &str) -> String {
    let n = id.chars().count();
    if n <= 6 {
        return id.to_string();
    }
    id.chars().skip(n - 6).collect()
}

/// An input the Kanban reducer folds into [`KanbanState`].
// Reduction enum: `Event(HangarEvent)` dominates the size, the rest are scalar
// focus/drag inputs. Short-lived, reducer-folded, not a hot allocation path —
// left unboxed for consistency with the other screen reducers.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KanbanEvent {
    /// Move focus one column left (`←`).
    FocusLeft,
    /// Move focus one column right (`→`).
    FocusRight,
    /// Move focus one row up (`↑`).
    FocusUp,
    /// Move focus one row down (`↓`).
    FocusDown,
    /// Drag the focused card one column left (`Shift+←`).
    MoveCardLeft,
    /// Drag the focused card one column right (`Shift+→`).
    MoveCardRight,
    /// A host stream event (e.g. [`HangarEvent::TaskStarted`]) that may change a
    /// card's column.
    Event(HangarEvent),
}

/// A side-effect the plugin glue performs after a Kanban reduction.
///
/// The only intent is a card move, which the glue lifts into a
/// `hangar/task_transition` RPC (the real store FSM move).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KanbanIntent {
    /// Move `task_id` to `to_status` (`hangar/task_transition`). Raised by
    /// `Shift+←` / `Shift+→` on a focused card.
    MoveCard {
        /// The task to move.
        task_id: String,
        /// The target status wire token (`queued` / `running` / `done` /
        /// `cancelled`), the [`BoardColumn::drop_status`] of the destination.
        to_status: String,
    },
}

/// The result of folding one [`KanbanEvent`] into a [`KanbanState`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KanbanReduction {
    /// The next state.
    pub state: KanbanState,
    /// A side-effect for the plugin glue, if any.
    pub intent: Option<KanbanIntent>,
}

/// Fold one [`KanbanEvent`] into `state`. Pure: no IO, no input mutation.
#[must_use]
pub fn reduce_kanban(state: &KanbanState, ev: KanbanEvent) -> KanbanReduction {
    match ev {
        KanbanEvent::FocusLeft => focus_col(state, -1),
        KanbanEvent::FocusRight => focus_col(state, 1),
        KanbanEvent::FocusUp => focus_row(state, -1),
        KanbanEvent::FocusDown => focus_row(state, 1),
        KanbanEvent::MoveCardLeft => move_card(state, -1),
        KanbanEvent::MoveCardRight => move_card(state, 1),
        KanbanEvent::Event(event) => fold_event(state, &event),
    }
}

/// Move the focused column by `delta` (clamped to `0..4`), then clamp the row
/// into the new column (so landing on a shorter column doesn't dangle).
fn focus_col(state: &KanbanState, delta: i32) -> KanbanReduction {
    let mut next = state.clone();
    let cur = i32::try_from(next.focused_col).unwrap_or(0);
    next.focused_col = usize::try_from((cur + delta).clamp(0, 3)).unwrap_or(0);
    next.clamp_row();
    no_intent(next)
}

/// Move the focused row by `delta` within the focused column (clamped), tracking
/// the scroll offset so the focused card stays visible.
fn focus_row(state: &KanbanState, delta: i32) -> KanbanReduction {
    let len = state.columns[state.focused_col].cards.len();
    if len == 0 {
        return unchanged(state);
    }
    let mut next = state.clone();
    let max = i32::try_from(len - 1).unwrap_or(0);
    let cur = i32::try_from(next.focused_row).unwrap_or(0);
    next.focused_row = usize::try_from((cur + delta).clamp(0, max)).unwrap_or(0);
    no_intent(next)
}

/// Drag the focused card one column in `dir` (`-1` left, `+1` right): emit the
/// [`KanbanIntent::MoveCard`] for the destination column's drop status. A no-op
/// at the board edge or with no focused card (the daemon owns the real move; the
/// board re-buckets on the next snapshot, so state is left unchanged here).
fn move_card(state: &KanbanState, dir: i32) -> KanbanReduction {
    let dest = i32::try_from(state.focused_col).unwrap_or(0) + dir;
    if !(0..=3).contains(&dest) {
        return unchanged(state);
    }
    let Some(card) = state.focused_card() else {
        return unchanged(state);
    };
    let dest_col = BoardColumn::all()[usize::try_from(dest).unwrap_or(0)];
    with_intent(
        state.clone(),
        KanbanIntent::MoveCard {
            task_id: card.task_id.clone(),
            to_status: dest_col.drop_status().as_str().to_string(),
        },
    )
}

/// Fold a host event: a task lifecycle change re-buckets the board so a status
/// change elsewhere reflects within one tick. The reducer can't re-fetch, so it
/// moves the matching card to its new column from the local model (the next
/// snapshot reconciles authoritatively).
fn fold_event(state: &KanbanState, event: &HangarEvent) -> KanbanReduction {
    let (task_id, new_status) = match event {
        HangarEvent::TaskStarted { task_id, .. } => (task_id.as_str(), "running"),
        HangarEvent::TaskFinished {
            task_id, result, ..
        } => (
            task_id.as_str(),
            match result {
                ainb_hangar_proto::events::TaskResult::Success => "done",
                ainb_hangar_proto::events::TaskResult::Failure => "failed",
                ainb_hangar_proto::events::TaskResult::Cancelled => "cancelled",
            },
        ),
        _ => return unchanged(state),
    };
    let mut next = state.clone();
    if rebucket_card(&mut next, task_id, new_status) {
        next.snap_focus_to_card();
        no_intent(next)
    } else {
        unchanged(state)
    }
}

/// Move the card with `task_id` to the column for `new_status`, updating its
/// stored status. Returns `true` when a card actually moved.
fn rebucket_card(state: &mut KanbanState, task_id: &str, new_status: &str) -> bool {
    let dest = BoardColumn::for_status(new_status);
    let mut found: Option<CardSummary> = None;
    for col in &mut state.columns {
        if let Some(pos) = col.cards.iter().position(|c| c.task_id == task_id) {
            if col.status == dest {
                // Already in the right column: just refresh the status token.
                col.cards[pos].status = new_status.to_string();
                return true;
            }
            let mut card = col.cards.remove(pos);
            card.status = new_status.to_string();
            found = Some(card);
            break;
        }
    }
    let Some(card) = found else {
        return false;
    };
    if let Some(col) = state.columns.iter_mut().find(|c| c.status == dest) {
        col.cards.push(card);
        return true;
    }
    false
}

/// A reduction that changes state but emits no intent.
const fn no_intent(state: KanbanState) -> KanbanReduction {
    KanbanReduction {
        state,
        intent: None,
    }
}

/// A reduction carrying `intent` alongside `state`.
const fn with_intent(state: KanbanState, intent: KanbanIntent) -> KanbanReduction {
    KanbanReduction {
        state,
        intent: Some(intent),
    }
}

/// A no-op reduction: state cloned unchanged, no intent.
fn unchanged(state: &KanbanState) -> KanbanReduction {
    no_intent(state.clone())
}

// ---------------------------------------------------------------------------
// Width-aware four-column render
// ---------------------------------------------------------------------------

/// Each card occupies this many rows (title, agent, age+chip).
const CARD_ROWS: u16 = 3;

/// Render the Kanban board into `buf` between rows `top` and `bottom`.
///
/// The width is split into four equal columns (`area.width / 4`, the
/// `Constraint::Percentage(25)` × 4 of `project_ainb_tui_width_aware_panels`).
/// Each column paints a header (`<label> (<count>)`, the focused column's in
/// green) and its cards top-to-bottom from `scroll_offset`; the focused card
/// carries a `▶` marker in [`SELECTION_GREEN`]. Card titles truncate via
/// `chars().take(...)` (never byte-slice — the rust-utf8-truncate trap).
///
/// `now_ms` is the render-time clock the card ages are computed against.
pub fn render_kanban(
    buf: &mut WireBuffer,
    area_w: u16,
    top: u16,
    bottom: u16,
    state: &KanbanState,
    now_ms: i64,
) {
    let col_w = (area_w / 4).max(1);
    for (i, col) in state.columns.iter().enumerate() {
        let x0 = u16::try_from(i).unwrap_or(0) * col_w;
        let focused = i == state.focused_col;
        render_column(buf, x0, col_w, top, bottom, col, focused, state, now_ms);
    }
}

/// Render one column: header + its visible cards.
#[allow(clippy::too_many_arguments)]
fn render_column(
    buf: &mut WireBuffer,
    x0: u16,
    col_w: u16,
    top: u16,
    bottom: u16,
    col: &Column,
    focused: bool,
    state: &KanbanState,
    now_ms: i64,
) {
    let header = format!("{} ({})", col.status.label(), col.cards.len());
    let header_color = if focused { SELECTION_GREEN } else { GOLD };
    put_str(
        buf,
        x0,
        top,
        &truncate(&header, col_w as usize),
        header_color,
        x0 + col_w,
    );

    let body_top = top + 2;
    let mut row = body_top;
    for (idx, card) in col.cards.iter().enumerate().skip(col.scroll_offset) {
        if row + CARD_ROWS > bottom + 1 {
            break;
        }
        let card_focused = focused && idx == state.focused_row;
        render_card(buf, x0, col_w, row, card, card_focused, now_ms);
        row += CARD_ROWS + 1;
    }
}

/// Render one card across three rows in its column slot.
fn render_card(
    buf: &mut WireBuffer,
    x0: u16,
    col_w: u16,
    row: u16,
    card: &CardSummary,
    focused: bool,
    now_ms: i64,
) {
    let right = x0 + col_w;
    let marker = if focused { '▶' } else { ' ' };
    let title_color = if focused { SELECTION_GREEN } else { SOFT_WHITE };
    // Row 0: `▶ #<short_id>`.
    let title = format!("{marker} #{}", card.short_id);
    put_str(
        buf,
        x0,
        row,
        &truncate(&title, col_w as usize),
        title_color,
        right,
    );
    // Row 1: agent id, indented under the marker.
    put_str(
        buf,
        x0 + 2,
        row + 1,
        &truncate(&card.agent_id, col_w.saturating_sub(2) as usize),
        SOFT_WHITE,
        right,
    );
    // Row 2: `<age>  <status chip>`.
    let age = age_label(card.created_at, now_ms);
    let mut x = put_str(buf, x0 + 2, row + 2, &age, MUTED_GRAY, right);
    x += 1;
    put_str(
        buf,
        x,
        row + 2,
        &card.status,
        status_color(&card.status),
        right,
    );
}

/// The status-chip colour for a wire status token.
fn status_color(status: &str) -> Color {
    match BoardColumn::for_status(status) {
        BoardColumn::Running => RUNNING_BLUE,
        BoardColumn::Done => SELECTION_GREEN,
        BoardColumn::Failed => WARN_RED,
        BoardColumn::Queued => MUTED_GRAY,
    }
}

/// A compact relative-age label (`5m` / `2h` / `3d`) from `created_at` to `now`.
/// A future / zero delta reads `0m`; sub-hour is minutes, sub-day is hours, else
/// days. (Char-cheap and deterministic for a fixed render clock.)
fn age_label(created_at_ms: i64, now_ms: i64) -> String {
    let delta_ms = now_ms.saturating_sub(created_at_ms).max(0);
    let mins = delta_ms / 60_000;
    if mins < 60 {
        format!("{mins}m")
    } else if mins < 60 * 24 {
        format!("{}h", mins / 60)
    } else {
        format!("{}d", mins / (60 * 24))
    }
}

/// Truncate `s` to `max` chars with an ellipsis, char-safe (multi-byte aware) —
/// never byte-slice (the rust-utf8-truncate trap).
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let take = max.saturating_sub(1);
    let mut out: String = s.chars().take(take).collect();
    out.push('…');
    out
}

/// Write `s` at `(x, row)` in `color`, clipping at `right`. Returns the next free
/// column. Char-safe (iterates `char`s, not bytes).
fn put_str(buf: &mut WireBuffer, x: u16, row: u16, s: &str, color: Color, right: u16) -> u16 {
    let mut cx = x;
    for ch in s.chars() {
        if cx >= right {
            break;
        }
        let mut cell = Cell::new(ch.to_string());
        cell.fg = Some(color);
        buf.push(Coord::new(cx, row), cell);
        cx = cx.saturating_add(1);
    }
    cx
}
