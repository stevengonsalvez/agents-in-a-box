//! P8.4 — Kanban board screen: four columns + card widget.
//!
//! The Kanban board (hotkey `K`) lays the workspace's task queue out as four
//! width-aware columns — `queued` / `running` / `done` / `failed` — each holding
//! the task cards bucketed into it. A card's ID LINE reads
//! `#<short_id> · <parent issue title>`; its TITLE line carries the run identity:
//! the assignee agent BY NAME, the task age (`5m` / `2h` / `3d`), the status, and
//! the run's durable artifacts (branch + PR chip).
//!
//! Focus walks columns with `←` / `→` and rows with `↑` / `↓`; `Shift+←` / `Shift+→`
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
//! ## Two ids the card must NOT show raw
//!
//! A [`TaskCardRow`] carries an `agent_id` and an `issue_id`, both 26-char ULIDs.
//! Painted raw they swamp a ~27-char card line and read as noise. Worse, N
//! dispatch runs of ONE issue (a squad fan-out, or a rerun) become N opaque,
//! seemingly unrelated cards. Both are therefore resolved CLIENT-SIDE against
//! snapshots the plugin already holds (the `hangar/agents_list` roster and the
//! `hangar/issues_list` rows) by [`KanbanState::set_agent_names`] and
//! [`KanbanState::set_issue_titles`]. No wire field carries the resolved text.
//!
//! Both seams are idempotent and order-independent: the three snapshots are fired
//! in one batch and land in any order, so each is applied from BOTH sides (the
//! tasks snapshot resolves against the cached rosters, and each roster snapshot
//! re-resolves the cards already on the board). An id that resolves to nothing
//! (a deleted agent, an orphan task) falls back to a SHORT form, never the ULID.
//!
//! ## Why the parent issue sits on the ID line, not the title
//!
//! The card widget wraps its title to exactly TWO lines and ellipsis-cuts the
//! overflow, while its id line carries `#<short_id>` alone and is ~35 cells of
//! dead space. Leading the TITLE with the parent issue therefore spent the run's
//! own budget on context and pushed the tail off the tile: a finished run's `PR ✓`
//! chip was silently elided (`tripwire_tcp_card_branch_pr_e2e`). The parent is
//! context and belongs on the identity line beside the id; the title line is the
//! run (agent, age, status, branch, PR) and keeps its whole two-line budget.
//!
//! As with every Hangar screen the reducer ([`reduce_kanban`]) is **pure**: it
//! folds a directional / move / event input into a new [`KanbanState`] plus an
//! optional [`KanbanIntent`] the plugin glue lifts into the matching daemon RPC.
//! The card rows come from the daemon (`hangar/tasks_list`); the plugin owns zero
//! domain data (`project_ainb_plugin_owns_data_plane`).

use std::collections::BTreeMap;

use ainb_hangar_proto::events::{HangarEvent, TaskCardRow};
use ainb_hangar_proto::pr_status::{CiRollup, PrStatus};
use ainb_plugin_sdk::WireBuffer;

use crate::widgets::card_board::{self, BoardCard, PriorityChip};

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
    /// The executing agent's HUMAN-READABLE label: the roster `display_name`
    /// resolved from [`agent_id`](Self::agent_id) by
    /// [`KanbanState::set_agent_names`].
    ///
    /// Seeded to [`short_id`] of the agent id at build time, so a board rendered
    /// before the `hangar/agents_list` snapshot lands (or a card whose agent was
    /// deleted from the roster) shows a short `#`-style token rather than a raw
    /// 26-char ULID that swamps the tile.
    pub agent_label: String,
    /// The parent issue this run was dispatched for, or `None` for an orphan task.
    pub issue_id: Option<String>,
    /// The parent issue's TITLE, resolved from [`issue_id`](Self::issue_id) by
    /// [`KanbanState::set_issue_titles`]; `None` for an orphan task or until the
    /// `hangar/issues_list` snapshot lands.
    ///
    /// A dispatch fan-out puts N cards for ONE issue on the board (a squad, or a
    /// rerun). Without the title those N cards read as N unrelated things; with
    /// it they visibly share a parent and differ only by agent.
    pub issue_title: Option<String>,
    /// The raw lifecycle status (drives the status chip colour).
    pub status: String,
    /// Creation timestamp (epoch ms), kept for re-computing age on re-render.
    pub created_at: i64,
    /// The worktree branch (`ainb/<slug>`) the run committed on (tcp T2), or
    /// `None` when the run made no commits — the durable artifact surfaced on the
    /// card once a run completes with commits.
    pub branch: Option<String>,
    /// The captured PR URL (P9.1), or `None` — drives the card's PR chip.
    pub pr_url: Option<String>,
    /// The PR's CI + merge status (tcp T2), or `None` when the card has no PR.
    pub pr_status: Option<PrStatus>,
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
    /// The task id the pointer is hovering over (63l.6), or `None` off every
    /// card. The card-board render lifts the hovered card's border so the cursor
    /// target reads before a click. Cleared when the pointer moves to empty space.
    hovered_id: Option<String>,
}

impl Default for KanbanState {
    fn default() -> Self {
        Self {
            columns: BoardColumn::all().map(Column::empty),
            focused_col: 0,
            focused_row: 0,
            hovered_id: None,
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
            hovered_id: None,
        };
        state.snap_focus_to_card();
        state
    }

    /// Resolve every card's [`agent_label`](CardSummary::agent_label) against the
    /// `agent_id -> display_name` roster the `hangar/agents_list` snapshot carries,
    /// so a card reads `claude · 7d · done` rather than a raw 26-char ULID.
    ///
    /// Idempotent and order-independent: the two snapshots (`hangar/tasks_list`
    /// and `hangar/agents_list`) are fired in one batch and may land in either
    /// order, so BOTH `set_tasks` and `set_actors` call this seam. An id with no
    /// roster entry (a deleted agent) keeps its [`short_id`] fallback, never the
    /// full ULID.
    pub fn set_agent_names(&mut self, names: &BTreeMap<String, String>) {
        if names.is_empty() {
            return;
        }
        for col in &mut self.columns {
            for card in &mut col.cards {
                if let Some(name) = names.get(&card.agent_id) {
                    card.agent_label.clone_from(name);
                }
            }
        }
    }

    /// Resolve every card's [`issue_title`](CardSummary::issue_title) against the
    /// `issue_id -> title` map the `hangar/issues_list` snapshot carries, so N
    /// dispatch runs of ONE issue read as N runs of that issue rather than N
    /// unrelated cards.
    ///
    /// Same order-independent contract as [`set_agent_names`](Self::set_agent_names):
    /// both `set_tasks` and `set_issues` call it, whichever snapshot lands first.
    /// An orphan task, or one whose issue is not in the snapshot, keeps `None` and
    /// simply renders the bare `#<short_id>` id line.
    pub fn set_issue_titles(&mut self, titles: &BTreeMap<String, String>) {
        if titles.is_empty() {
            return;
        }
        for col in &mut self.columns {
            for card in &mut col.cards {
                if let Some(t) = card.issue_id.as_ref().and_then(|id| titles.get(id)) {
                    card.issue_title = Some(t.clone());
                }
            }
        }
    }

    /// Flatten the four board columns into the shared card-board
    /// [`BoardColumn`](card_board::BoardColumn)s the render paints and the mouse
    /// layer hit-tests against (63l.6), computing each card's age against `now_ms`.
    ///
    /// Each task card maps onto the card anatomy: the id line is
    /// `#<short_id> · <issue>`, the two title lines carry
    /// `<agent> · <age> · <status>` plus the run's artifacts (so the bead's
    /// required id + title + state + age all read on the tile), the priority chip
    /// comes from the row, and the assignee initial is the agent NAME's first char.
    /// The same geometry feeds `render_kanban` and the hit-map, so paint + hit-test
    /// never drift.
    #[must_use]
    pub fn board_columns(&self, now_ms: i64) -> Vec<card_board::BoardColumn> {
        self.columns
            .iter()
            .map(|col| {
                let cards = col
                    .cards
                    .iter()
                    .map(|c| BoardCard {
                        not_dispatched: false,
                        issue_id: c.task_id.clone(),
                        display_id: card_id_line(c),
                        title: card_title(c, now_ms),
                        priority: PriorityChip::from_priority(0),
                        assignee_initial: c.agent_label.chars().next(),
                        linked: false,
                        subtasks: None,
                    })
                    .collect::<Vec<_>>();
                card_board::BoardColumn {
                    glyph: column_glyph(col.status),
                    name: col.status.label().to_string(),
                    cards,
                    scroll_offset: col.scroll_offset.min(cards_len_floor(&col.cards)),
                }
            })
            .collect()
    }

    /// Resolve the `(column, card)` slot of the card the render draws with the
    /// heavy highlight border (63l.6): the HOVERED card when the pointer is over
    /// one (the cursor target reads before a click), else the keyboard focus.
    /// `None` when neither resolves to a visible card.
    #[must_use]
    pub fn highlight_board_card(&self) -> Option<(usize, usize)> {
        if let Some(hover) = self.hovered_id.as_deref() {
            for (ci, col) in self.columns.iter().enumerate() {
                if let Some(ri) = col.cards.iter().position(|c| c.task_id == hover) {
                    return Some((ci, ri));
                }
            }
        }
        // Fall back to the keyboard focus when nothing is hovered.
        (!self.columns[self.focused_col].cards.is_empty())
            .then_some((self.focused_col, self.focused_row))
    }

    /// The task id the pointer is hovering, if any (63l.6).
    #[must_use]
    pub fn hovered_id(&self) -> Option<&str> {
        self.hovered_id.as_deref()
    }

    /// Set (or clear with `None`) the hovered card id (63l.6). A pointer move over
    /// a card highlights it; a move onto empty space clears it.
    pub fn set_hover(&mut self, id: Option<String>) {
        self.hovered_id = id;
    }

    /// Scroll board column `index` (`0..4`) vertically by `delta` rows (63l.6):
    /// `+1` reveals a card further down, `-1` scrolls back up. The offset saturates
    /// at `0` and is capped at the column's last card so a scroll past the end is a
    /// no-op. A wheel-scroll over a column's body drives this. An out-of-range
    /// index is ignored.
    pub fn scroll_column(&mut self, index: usize, delta: i32) {
        let Some(col) = self.columns.get_mut(index) else {
            return;
        };
        let next = if delta >= 0 {
            col.scroll_offset.saturating_add(delta.unsigned_abs() as usize)
        } else {
            col.scroll_offset.saturating_sub(delta.unsigned_abs() as usize)
        };
        col.scroll_offset = next.min(cards_len_floor(&col.cards));
    }

    /// The task id of the card at board slot `(column, card)`, if any (63l.6) — the
    /// click target a `ClickOpen` resolves to a task to open.
    #[must_use]
    pub fn task_id_at(&self, column: usize, card: usize) -> Option<&str> {
        self.columns
            .get(column)
            .and_then(|c| c.cards.get(card))
            .map(|c| c.task_id.as_str())
    }

    /// Move the keyboard focus onto the card carrying `task_id` (63l.6 mouse
    /// click), so a pointer click lands the selection on the clicked card exactly
    /// as a keyboard walk would. A no-op when no card carries that id.
    pub fn focus_task(&mut self, task_id: &str) {
        for (ci, col) in self.columns.iter().enumerate() {
            if let Some(ri) = col.cards.iter().position(|c| c.task_id == task_id) {
                self.focused_col = ci;
                self.focused_row = ri;
                return;
            }
        }
    }

    /// The card carrying `task_id`, if any (63l.6) — the source the click-open
    /// path reads to build the task-detail screen for the clicked task.
    #[must_use]
    pub fn card_for_task(&self, task_id: &str) -> Option<&CardSummary> {
        self.columns.iter().flat_map(|c| c.cards.iter()).find(|c| c.task_id == task_id)
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

/// Chars of the parent issue's title the id line shows before eliding.
///
/// The id line is `#<short_id> · <issue>` clipped at the card's inner width, which
/// is 42 on the reference 180-col board and 27 on the narrow 120-col one. Capping
/// here means a long title ends in a visible `…` rather than being hard-clipped by
/// the widget mid-word on the wide board.
const ISSUE_TITLE_CAP: usize = 24;

/// The card's ID line: `#<short_id>`, then the parent issue's title (elided at
/// [`ISSUE_TITLE_CAP`]) when it has resolved.
///
/// The parent names the card so N dispatch runs of ONE issue (a squad fan-out, or
/// a rerun) read as N runs of that issue rather than N unrelated cards. It rides
/// the id line (the card's identity row, otherwise ~35 cells of dead space)
/// rather than the title, which the widget wraps to two lines and ellipsis-cuts:
/// spending that budget on context is what pushed a finished run's `PR ✓` chip off
/// the tile. An orphan task, or one whose issue snapshot has not landed, renders
/// the bare `#<short_id>` with no dangling separator.
fn card_id_line(c: &CardSummary) -> String {
    c.issue_title.as_deref().map_or_else(
        || format!("#{}", c.short_id),
        |t| format!("#{} · {}", c.short_id, elide(t, ISSUE_TITLE_CAP)),
    )
}

/// The card's title line: `<agent> · <age> · <status>`, then the run's durable
/// artifacts when present (tcp T2): the `ainb/<slug>` branch it committed on and a
/// `PR <ci>` chip, so a finished run's branch + PR read on the tile itself.
///
/// Deliberately carries the RUN and nothing else. The widget wraps this to exactly
/// two lines and ellipsis-cuts the overflow, so every char spent ahead of the
/// branch is a char of the branch + PR chip that falls off the tile; the parent
/// issue is on the id line ([`card_id_line`]) for exactly that reason.
fn card_title(c: &CardSummary, now_ms: i64) -> String {
    let mut title = format!(
        "{} · {} · {}",
        c.agent_label,
        age_label(c.created_at, now_ms),
        c.status
    );
    if let Some(branch) = c.branch.as_deref() {
        title.push_str(" · ");
        title.push_str(branch);
    }
    if let Some(chip) = card_pr_chip(c) {
        title.push_str(" · ");
        title.push_str(&chip);
    }
    title
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
            // Un-resolved until the roster arrives; never the full ULID.
            agent_label: short_id(&t.agent_id),
            issue_id: t.issue_id.clone(),
            // Unresolved until the issues snapshot lands.
            issue_title: None,
            status: t.status.clone(),
            created_at: t.created_at,
            branch: t.branch.clone(),
            pr_url: t.pr_url.clone(),
            pr_status: t.pr_status,
        })
        .collect()
}

/// The compact PR chip a card renders when it has a captured PR (tcp T2):
/// `PR <ci>` using the same CI glyphs the task-detail badge does (`✓`/`✗`/`…`),
/// so a passing / failing / pending / unknown rollup reads at a glance. `None`
/// when the card has no PR.
fn card_pr_chip(card: &CardSummary) -> Option<String> {
    card.pr_url.as_ref()?;
    let ci = match card.pr_status.map(|s| s.ci) {
        Some(CiRollup::Pass) => "✓",
        Some(CiRollup::Fail) => "✗",
        // Pending / Unknown / no status yet all read as in-flight.
        _ => "…",
    };
    Some(format!("PR {ci}"))
}

/// Clip `s` to at most `cap` CHARS (multi-byte safe), spending the last char on a
/// `…` when it had to cut. A `cap` of 0 yields the empty string.
fn elide(s: &str, cap: usize) -> String {
    if s.chars().count() <= cap {
        return s.to_string();
    }
    let mut out: String = s.chars().take(cap.saturating_sub(1)).collect();
    if cap > 0 {
        out.push('…');
    }
    out
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
// Card-board render (63l.6)
// ---------------------------------------------------------------------------

/// The status glyph painted before a column's name (63l.6): empty queued,
/// filling through running, solid done, a cross for the failed/cancelled bucket.
const fn column_glyph(column: BoardColumn) -> char {
    match column {
        BoardColumn::Queued => '○',
        BoardColumn::Running => '◔',
        BoardColumn::Done => '●',
        BoardColumn::Failed => '✕',
    }
}

/// The largest scroll offset a column may hold: its last card index, so a scroll
/// never lands the body on a blank past-the-end gap. `0` for an empty column.
fn cards_len_floor(cards: &[CardSummary]) -> usize {
    cards.len().saturating_sub(1)
}

/// Render the Kanban board into `buf` between rows `top` and `bottom`.
///
/// Paints THROUGH the shared Linear-style card-board (63l.6): four status columns
/// (`queued` / `running` / `done` / `failed`) side by side, each a
/// per-column-scrollable stack of bordered task cards whose id line is
/// `#<short_id> · <parent issue>` and whose title line is the agent NAME + age +
/// status (+ branch + PR chip), with a priority chip in the footer. The hovered
/// (or keyboard-focused) card carries the heavy clay highlight border.
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
    let columns = state.board_columns(now_ms);
    let highlight = state.highlight_board_card();
    let _ = card_board::render_card_board(buf, area_w, top, bottom, &columns, highlight);
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

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_700_000_600_000;

    fn task(id: &str, status: &str) -> TaskCardRow {
        TaskCardRow {
            id: ainb_hangar_core::ids::TaskId::from_str(id).unwrap(),
            workspace_id: "default".into(),
            agent_id: "claude-agent".into(),
            issue_id: Some("issue-1".into()),
            status: status.into(),
            priority: 0,
            created_at: NOW - 300_000, // 5m
            branch: None,
            pr_url: None,
            pr_status: None,
        }
    }

    /// The `agent_id -> display_name` roster the resolve seam takes.
    fn roster(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(id, name)| ((*id).to_string(), (*name).to_string()))
            .collect()
    }

    /// `board_columns` flattens the four buckets into card-board columns whose
    /// cards carry `#<short_id>`, an agent · age · status title, and the column
    /// glyph + label, so the screen renders THROUGH the shared card-board (63l.6).
    #[test]
    fn board_columns_map_tasks_to_card_board_cards() {
        let mut state = KanbanState::from_tasks(
            &[
                task("01HANGARTASKQUEUED01", "queued"),
                task("01HANGARTASKRUNNING03", "running"),
            ],
            NOW,
        );
        state.set_agent_names(&roster(&[("claude-agent", "claude")]));
        let cols = state.board_columns(NOW);
        assert_eq!(cols.len(), 4, "four board columns");
        assert_eq!(cols[0].name, "queued");
        assert_eq!(cols[0].glyph, '○');
        let queued_card = &cols[0].cards[0];
        assert_eq!(queued_card.issue_id, "01HANGARTASKQUEUED01");
        assert_eq!(queued_card.display_id, "#EUED01");
        assert!(
            queued_card.title.contains("claude")
                && queued_card.title.contains("5m")
                && queued_card.title.contains("queued"),
            "the card title carries agent · age · status: {:?}",
            queued_card.title
        );
        // The running task buckets into the running column.
        assert_eq!(cols[1].cards.len(), 1);
        assert_eq!(cols[1].cards[0].issue_id, "01HANGARTASKRUNNING03");
    }

    /// tcp T2: a finished run's durable artifacts read on the card title — the
    /// `ainb/<slug>` branch it committed on and a `PR <ci>` chip reflecting the
    /// CI rollup. A card with no branch / no PR carries neither (no stray chips).
    #[test]
    fn card_title_surfaces_branch_and_pr_chip() {
        use ainb_hangar_proto::pr_status::{MergeState, Mergeable};
        let mut done = task("01HANGARTASKDONE0001", "done");
        done.branch = Some("ainb/done0001".into());
        done.pr_url = Some("https://github.com/o/r/pull/7".into());
        done.pr_status = Some(PrStatus {
            ci: CiRollup::Pass,
            mergeable: Mergeable::Mergeable,
            state: MergeState::Open,
        });
        // A plain card with no run artifacts (the negative control).
        let plain = task("01HANGARTASKQUEUED01", "queued");

        let state = KanbanState::from_tasks(&[done, plain], NOW);
        let cols = state.board_columns(NOW);
        // done → the Done column (index 2), queued → the Todo column (index 0).
        let done_title = &cols[2].cards[0].title;
        assert!(
            done_title.contains("ainb/done0001"),
            "the committed branch reads on the card: {done_title:?}"
        );
        assert!(
            done_title.contains("PR ✓"),
            "a passing PR shows a PR chip with the pass glyph: {done_title:?}"
        );
        let plain_title = &cols[0].cards[0].title;
        assert!(
            !plain_title.contains("ainb/") && !plain_title.contains("PR "),
            "a card with no run artifacts carries no branch / PR chip: {plain_title:?}"
        );
    }

    /// REGRESSION PIN: a card names its agent, and NEVER prints the raw 26-char
    /// agent ULID.
    ///
    /// The board read `01KXPM2K4DYDTRZ7RHDGAA9Q9X · 7d · done` in the field,
    /// interpolating `agent_id` where the doc comment promised the agent. Four
    /// dispatch runs of one issue became four opaque, seemingly unrelated cards.
    /// Both halves are asserted: the NAME is present AND the id is absent, so a
    /// future refactor cannot half-regress by appending the id alongside the name.
    #[test]
    fn card_names_the_agent_and_never_prints_the_raw_ulid() {
        const AGENT_ULID: &str = "01KXPM2K4DYDTRZ7RHDGAA9Q9X";
        let mut t = task("01KY7SJ2CM6TCC82KG9T8CQ051", "done");
        t.agent_id = AGENT_ULID.into();

        let mut state = KanbanState::from_tasks(&[t], NOW);
        state.set_agent_names(&roster(&[(AGENT_ULID, "claude")]));

        let card = &state.board_columns(NOW)[2].cards[0];
        assert!(
            card.title.starts_with("claude · "),
            "the card leads with the agent NAME: {:?}",
            card.title
        );
        assert!(
            !card.title.contains(AGENT_ULID),
            "the raw agent ULID must never reach the card: {:?}",
            card.title
        );
        assert_eq!(
            card.assignee_initial,
            Some('c'),
            "the assignee pip is the NAME's initial, not the ULID's"
        );
    }

    /// An agent id with no roster entry (a deleted agent, or a board rendered
    /// before `hangar/agents_list` lands) falls back to the SHORT id: the same
    /// last-6 convention the `#<short_id>` line uses, never the full ULID.
    #[test]
    fn unresolvable_agent_falls_back_to_short_id_not_the_ulid() {
        const AGENT_ULID: &str = "01KXPM2K4DYDTRZ7RHDGAA9Q9X";
        let mut t = task("01KY7SJ2CM6TCC82KG9T8CQ051", "done");
        t.agent_id = AGENT_ULID.into();

        let mut state = KanbanState::from_tasks(&[t], NOW);
        // A roster that knows some OTHER agent: the seam runs, this id misses.
        state.set_agent_names(&roster(&[("01KY83MQCPZGPH4YGCZ566Q1GR", "test")]));

        let title = &state.board_columns(NOW)[2].cards[0].title;
        assert!(
            title.starts_with("AA9Q9X · "),
            "an unresolved agent shows the last 6 chars: {title:?}"
        );
        assert!(
            !title.contains(AGENT_ULID),
            "the fallback is still never the full ULID: {title:?}"
        );
    }

    /// The two resolve seams are ORDER-INDEPENDENT: the three snapshots are fired
    /// in one batch, so a roster that arrives AFTER the tasks snapshot must still
    /// re-label the cards already on the board.
    #[test]
    fn roster_arriving_after_the_tasks_snapshot_still_labels_the_cards() {
        const AGENT_ULID: &str = "01KXPM2K4DYDTRZ7RHDGAA9Q9X";
        let mut t = task("01KY7SJ2CM6TCC82KG9T8CQ051", "done");
        t.agent_id = AGENT_ULID.into();

        // Board built with NO roster at all (the tasks snapshot won the race).
        let mut state = KanbanState::from_tasks(&[t], NOW);
        assert!(state.board_columns(NOW)[2].cards[0].title.starts_with("AA9Q9X · "));

        // The roster lands second and re-labels in place.
        state.set_agent_names(&roster(&[(AGENT_ULID, "claude")]));
        let mut titles = BTreeMap::new();
        titles.insert("issue-1".to_string(), "test".to_string());
        state.set_issue_titles(&titles);

        let card = &state.board_columns(NOW)[2].cards[0];
        assert_eq!(card.title, "claude · 5m · done");
        assert_eq!(card.display_id, "#8CQ051 · test");
    }

    /// The parent issue's title names the card ON THE ID LINE, so N dispatch runs
    /// of ONE issue read as N runs of that issue. A long title elides at
    /// [`ISSUE_TITLE_CAP`]; an unresolved issue leaves the bare `#<short_id>` with
    /// no dangling separator. The title line stays the RUN and nothing else.
    #[test]
    fn issue_title_names_the_card_on_the_id_line_and_elides() {
        let mut state = KanbanState::from_tasks(
            &[
                task("01HANGARTASKDONE0001", "done"),
                task("01HANGARTASKDONE0002", "done"),
            ],
            NOW,
        );
        state.set_agent_names(&roster(&[("claude-agent", "claude")]));
        let mut titles = BTreeMap::new();
        titles.insert(
            "issue-1".to_string(),
            "Fix kanban card rendering showing raw ULIDs".to_string(),
        );
        state.set_issue_titles(&titles);

        let card = &state.board_columns(NOW)[2].cards[0];
        assert_eq!(
            card.display_id, "#NE0001 · Fix kanban card renderi…",
            "the parent issue names the card on the id line, elided at the cap"
        );
        assert_eq!(
            card.title, "claude · 5m · done",
            "the title line carries the RUN only, so the artifacts keep their budget"
        );

        // An orphan / unresolved issue leaves the bare id, no dangling separator.
        let mut orphan = KanbanState::from_tasks(&[task("01HANGARTASKDONE0003", "done")], NOW);
        orphan.set_agent_names(&roster(&[("claude-agent", "claude")]));
        orphan.set_issue_titles(&BTreeMap::new());
        let orphan_card = &orphan.board_columns(NOW)[2].cards[0];
        assert_eq!(orphan_card.display_id, "#NE0003");
        assert_eq!(orphan_card.title, "claude · 5m · done");
    }

    /// The two-line title budget at the reference 180-col board: four columns of
    /// 45, each ceding a gutter cell and two border cells, leaves 42 content cells
    /// per line and the widget paints exactly two of them.
    const TWO_LINE_TITLE_BUDGET: usize = 84;

    /// REGRESSION PIN (tcp T2): a finished run's `ainb/<task-ulid>` branch AND its
    /// `PR ✓` chip both fit the card's two-line title budget.
    ///
    /// The widget ellipsis-cuts whatever overflows line two, silently and without
    /// error. Leading this line with the parent issue title pushed a real run's
    /// title to 88 chars and ate the `PR ✓` chip off the tile: green unit tests,
    /// a board that had quietly stopped surfacing PR state
    /// (`tripwire_tcp_card_branch_pr_e2e`). Any future segment added ahead of the
    /// branch must keep this budget.
    #[test]
    fn a_finished_run_keeps_its_branch_and_pr_chip_inside_the_title_budget() {
        const TASK_ULID: &str = "01KYTV3EWKS8C5G66G850SCAKH";
        let mut t = task(TASK_ULID, "done");
        t.branch = Some(format!("ainb/{TASK_ULID}"));
        t.pr_url = Some("https://github.com/o/r/pull/8".into());
        t.pr_status = Some(PrStatus {
            ci: CiRollup::Pass,
            ..PrStatus::default()
        });

        let mut state = KanbanState::from_tasks(&[t], NOW);
        // A 12-char roster name, the longest the real fixtures dispatch under.
        state.set_agent_names(&roster(&[("claude-agent", "claude-agent")]));
        state.set_issue_titles(&BTreeMap::from([(
            "issue-1".to_string(),
            "Cardbranchprtripwire".to_string(),
        )]));

        let card = &state.board_columns(NOW)[2].cards[0];
        assert!(
            card.title.contains(&format!("ainb/{TASK_ULID}")) && card.title.ends_with("PR ✓"),
            "branch + PR chip both on the title: {:?}",
            card.title
        );
        assert!(
            card.title.chars().count() <= TWO_LINE_TITLE_BUDGET,
            "the title must fit the two-line budget of {TWO_LINE_TITLE_BUDGET} or the \
             widget elides the PR chip off the tile ({} chars): {:?}",
            card.title.chars().count(),
            card.title
        );
        // The parent issue still names the card, on the id line, where it costs
        // the run's artifacts nothing.
        assert_eq!(card.display_id, "#0SCAKH · Cardbranchprtripwire");
    }

    /// A captured PR whose CI has not resolved yet (or is unknown) renders the
    /// in-flight `PR …` chip, never a false pass/fail.
    #[test]
    fn card_pr_chip_is_in_flight_when_ci_unresolved() {
        let mut c = task("01HANGARTASKDONE0002", "done");
        c.pr_url = Some("https://github.com/o/r/pull/8".into());
        c.pr_status = None; // status not fetched yet
        let state = KanbanState::from_tasks(&[c], NOW);
        let title = &state.board_columns(NOW)[2].cards[0].title;
        assert!(
            title.contains("PR …"),
            "unresolved CI is in-flight: {title:?}"
        );
    }

    /// A wheel-scroll over a column nudges that column's scroll offset, saturating
    /// at `0` upward and capped at the last card downward — and only the targeted
    /// column moves (the click resolves the column, not a hard-wired one).
    #[test]
    fn scroll_column_offsets_only_that_column_and_saturates() {
        let tasks: Vec<TaskCardRow> =
            (0..4).map(|i| task(&format!("01HANGARTASKQUEUE0{i}"), "queued")).collect();
        let mut state = KanbanState::from_tasks(&tasks, NOW);
        // Scroll the queued column (index 0) down twice → offset 2.
        state.scroll_column(0, 1);
        state.scroll_column(0, 1);
        assert_eq!(state.board_columns(NOW)[0].scroll_offset, 2);
        // Other columns are untouched.
        assert_eq!(state.board_columns(NOW)[1].scroll_offset, 0);
        // Up past the top saturates at 0.
        state.scroll_column(0, -5);
        assert_eq!(state.board_columns(NOW)[0].scroll_offset, 0);
        // Down past the last card caps at len-1 (never a blank body).
        for _ in 0..10 {
            state.scroll_column(0, 1);
        }
        assert_eq!(state.board_columns(NOW)[0].scroll_offset, 3);
        // An out-of-range column index is a no-op (no panic).
        state.scroll_column(99, 1);
    }

    /// A hover resolves the highlighted `(column, card)` slot, overriding the
    /// keyboard focus; clearing the hover falls back to the focus.
    #[test]
    fn highlight_resolves_hover_over_focus() {
        let mut state = KanbanState::from_tasks(
            &[
                task("01HANGARTASKQUEUED01", "queued"),
                task("01HANGARTASKRUNNING03", "running"),
            ],
            NOW,
        );
        // Focus defaults to the first non-empty column/card (queued, slot (0,0)).
        assert_eq!(state.highlight_board_card(), Some((0, 0)));
        // Hovering the running task overrides the highlight to its slot (1, 0).
        state.set_hover(Some("01HANGARTASKRUNNING03".to_string()));
        assert_eq!(state.highlight_board_card(), Some((1, 0)));
        // Clearing the hover falls back to the focus.
        state.set_hover(None);
        assert_eq!(state.highlight_board_card(), Some((0, 0)));
    }

    /// `task_id_at` resolves a board slot back to the task id a click opens.
    #[test]
    fn task_id_at_resolves_the_click_target() {
        let state = KanbanState::from_tasks(&[task("01HANGARTASKRUNNING03", "running")], NOW);
        assert_eq!(state.task_id_at(1, 0), Some("01HANGARTASKRUNNING03"));
        // An empty / out-of-range slot resolves to nothing.
        assert_eq!(state.task_id_at(0, 0), None);
        assert_eq!(state.task_id_at(9, 9), None);
    }
}
