//! P4.3 — Issue list screen: the pure reducer + width-aware render.
//!
//! The issue list is the default landing screen (hotkey `1`). It renders a
//! workspace's [`IssueRow`]s grouped by lifecycle status into the five canonical
//! columns — Backlog / Todo / In Progress / In Review / Done (63l.3) — with row
//! selection, filter chips, and a type-narrow filter-input mode. As with the
//! screen router ([`crate::screen`]),
//! the reducer ([`reduce_issue_list`]) is **pure**: it folds a key press or a
//! host [`HangarEvent`] into a new [`IssueListState`] plus an optional
//! [`IssueListIntent`] for the plugin glue to act on (open a task, start the
//! create flow). No IO, no `tokio`, no socket — so every transition is
//! exhaustively unit-testable, which is what the P4.3 RED tests in
//! `tests/issue_list_reducer_test.rs` exercise.
//!
//! The plugin holds **zero domain data of its own**
//! (`project_ainb_plugin_owns_data_plane`): the cached rows are the daemon's
//! read model, pulled over RPC and kept current by folding the event stream.
//! `IssueListState` is that render-state cache and nothing more.
//!
//! Status grouping maps each wire `state` through [`IssueColumn::for_state`],
//! which delegates to the shared canonical five-status `IssueLifecycle`
//! (Backlog / Todo / In Progress / In Review / Done), folding legacy strings
//! (`open` / `closed`) forward into the right column. A `TaskStarted` event promotes
//! the task's issue into In Progress without waiting for an `IssueUpdated`,
//! because the daemon reports task lifecycle before it rewrites the issue row.

use std::collections::HashMap;

use ainb_hangar_core::ids::IssueId;
use ainb_hangar_proto::events::{HangarEvent, IssueRow};
use ainb_hangar_proto::lifecycle::IssueLifecycle;
use ainb_plugin_sdk::{Cell, Color, Coord, WireBuffer};

/// The number of status columns the board renders — the five canonical
/// lifecycle statuses (63l.3). Kept as a single constant so the column enum, the
/// card-board render, and the per-column scroll offsets stay in lockstep.
pub(crate) const COLUMN_COUNT: usize = IssueLifecycle::ALL.len();

/// The five status columns issues are bucketed into for display, one per
/// canonical [`IssueLifecycle`] status (63l.3).
///
/// The mapping from the wire `state` string to a column is owned by
/// [`IssueColumn::for_state`], which delegates to the canonical
/// [`IssueLifecycle::for_state`] — the SINGLE source of truth the daemon also
/// maps through. Legacy `open` and any unknown string fall into
/// [`IssueColumn::Todo`], legacy `closed` into [`IssueColumn::Done`], so a row
/// never silently vanishes from the board.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IssueColumn {
    /// Not yet triaged into active work (`"backlog"`) — the leftmost column.
    Backlog,
    /// Triaged, ready to start (`"todo"`, legacy `"open"`, unknown states).
    Todo,
    /// Actively being worked (`"in_progress"`).
    InProgress,
    /// Work done, awaiting review / merge (`"in_review"`).
    InReview,
    /// Terminal / closed (`"done"`, legacy `"closed"`).
    Done,
}

impl IssueColumn {
    /// Bucket a wire `state` string into its display column, via the canonical
    /// [`IssueLifecycle`] vocabulary (the one source of truth the daemon shares).
    ///
    /// Unknown states fall into [`IssueColumn::Todo`] (fail-visible, not
    /// fail-hidden) so the board never drops a row the daemon sent.
    #[must_use]
    pub fn for_state(state: &str) -> Self {
        Self::from_lifecycle(IssueLifecycle::for_state(state))
    }

    /// Map a canonical [`IssueLifecycle`] status to its display column (63l.4):
    /// the seam the mouse layer uses to route a `MoveCard{to_status}` /
    /// `ScrollColumn{status}` intent (which carries an [`IssueLifecycle`]) to the
    /// matching display column. The two enums are 1:1 by design.
    #[must_use]
    pub const fn from_lifecycle(status: IssueLifecycle) -> Self {
        match status {
            IssueLifecycle::Backlog => Self::Backlog,
            IssueLifecycle::Todo => Self::Todo,
            IssueLifecycle::InProgress => Self::InProgress,
            IssueLifecycle::InReview => Self::InReview,
            IssueLifecycle::Done => Self::Done,
        }
    }

    /// The column header label (without the count suffix) — the canonical
    /// [`IssueLifecycle::label`].
    #[must_use]
    pub const fn label(self) -> &'static str {
        self.lifecycle().label()
    }

    /// The canonical [`IssueLifecycle`] status this column renders.
    #[must_use]
    const fn lifecycle(self) -> IssueLifecycle {
        match self {
            Self::Backlog => IssueLifecycle::Backlog,
            Self::Todo => IssueLifecycle::Todo,
            Self::InProgress => IssueLifecycle::InProgress,
            Self::InReview => IssueLifecycle::InReview,
            Self::Done => IssueLifecycle::Done,
        }
    }

    /// The five columns in left-to-right display order (`backlog` … `done`).
    #[must_use]
    pub const fn all() -> [Self; COLUMN_COUNT] {
        [
            Self::Backlog,
            Self::Todo,
            Self::InProgress,
            Self::InReview,
            Self::Done,
        ]
    }
}

/// The 0-based left-to-right board index of a column (63l.4) — the index into
/// the [`IssueColumn::all`] order and the per-column `scroll_offsets`. Pinned to
/// the canonical [`IssueLifecycle::order`] so the column geometry, the scroll
/// offsets, and the hit-map all agree on which column is which.
const fn column_index(column: IssueColumn) -> usize {
    column.lifecycle().order()
}

/// The status glyph painted before a card-board column's name (63l.2): a small
/// progress-arc family mirroring Linear's status icons — empty for backlog,
/// filling through in-progress / in-review, solid for done.
const fn column_glyph(column: IssueColumn) -> char {
    match column {
        IssueColumn::Backlog => '☰',
        IssueColumn::Todo => '○',
        IssueColumn::InProgress => '◔',
        IssueColumn::InReview => '◑',
        IssueColumn::Done => '●',
    }
}

/// The filter chips that narrow which rows are visible (UX §1).
///
/// `Members` / `Agents` filter by assignee actor kind; `Mine` is a placeholder
/// for the current-user filter (wired to the auth identity in P5 — until then it
/// behaves like [`FilterChip::All`]). The reducer keeps the data so the chip is
/// already plumbed when P5 lands the identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterChip {
    /// Show every row.
    All,
    /// Show only rows assigned to a human member (`member:*`).
    Members,
    /// Show only rows assigned to an agent (`agent:*`).
    Agents,
    /// Show only rows assigned to the current user (P5 — currently `All`).
    Mine,
}

impl FilterChip {
    /// The chip label rendered in the chip bar.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Members => "Members",
            Self::Agents => "Agents",
            Self::Mine => "Mine",
        }
    }

    /// The chips in display order.
    #[must_use]
    pub const fn all() -> [Self; 4] {
        [Self::All, Self::Members, Self::Agents, Self::Mine]
    }

    /// Whether `row` passes this filter.
    ///
    /// An unassigned row passes only the [`FilterChip::All`] / [`FilterChip::Mine`]
    /// chips (it has no member/agent kind to match).
    fn accepts(self, row: &IssueRow) -> bool {
        match self {
            // `Mine` is a P5 placeholder that currently behaves as `All`.
            Self::All | Self::Mine => true,
            Self::Members => assignee_kind(row) == Some(ActorKind::Member),
            Self::Agents => assignee_kind(row) == Some(ActorKind::Agent),
        }
    }
}

/// The two assignee actor kinds the filter chips discriminate on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActorKind {
    Member,
    Agent,
}

/// Classify a row's assignee as member / agent / neither.
///
/// Reads the `type:id` wire form ([`IssueRow::assignee`]) without pulling in the
/// store's `ActorRef` parser — the plugin only needs the discriminant prefix.
fn assignee_kind(row: &IssueRow) -> Option<ActorKind> {
    match row.assignee.as_deref()?.split_once(':') {
        Some(("member", _)) => Some(ActorKind::Member),
        Some(("agent", _)) => Some(ActorKind::Agent),
        _ => None,
    }
}

/// Whether the screen is in normal navigation, filter-text-entry, or
/// create-title-entry mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IssueListMode {
    /// Normal row navigation (`j`/`k`, `enter`, `c`, …).
    Normal,
    /// `/` filter-input mode: keystrokes append to the [`IssueListState::query`].
    FilterInput,
    /// `c` create-input mode (e38.29): keystrokes append to the
    /// [`IssueListState::create_title`]; Enter submits a non-blank title (raising
    /// [`IssueListIntent::CreateIssue`]), Esc/empty-Enter abort.
    CreateInput,
}

/// The render-state cache for the issue list.
///
/// Holds the daemon's issue rows (pulled over RPC, kept current by folding the
/// event stream), the current selection, active chip, query, and input mode.
/// All fields are private; tests and the renderer read through accessors so the
/// invariant "`selected` is always in range of the *visible* rows" stays
/// internal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueListState {
    /// Cached rows in daemon-supplied order. Source of truth is the daemon.
    rows: Vec<IssueRow>,
    /// Index into the *visible* (filtered) rows of the current selection.
    selected: usize,
    /// The active filter chip.
    filter: FilterChip,
    /// The free-text query typed in filter-input mode (case-insensitive
    /// substring over the title).
    query: String,
    /// Whether we are navigating, typing a filter, or typing a new-issue title.
    mode: IssueListMode,
    /// The new-issue title typed in [`IssueListMode::CreateInput`] (e38.29).
    /// Empty when not creating; cleared on submit / abort.
    create_title: String,
    /// Maps a queued/running task to the issue it works on, so a `TaskStarted`
    /// event can promote the right issue to In Progress (the event carries only
    /// the task id, the queue carried the issue id).
    task_issue: HashMap<String, IssueId>,
    /// Per-column vertical scroll offset (63l.4): the first visible card index in
    /// each canonical [`IssueColumn`], indexed by [`IssueColumn::all`] order. A
    /// wheel-scroll over a column's body nudges its entry; the card-board render
    /// skips this many leading cards in that column. All zero on a fresh snapshot.
    scroll_offsets: [usize; COLUMN_COUNT],
    /// The issue id the pointer is hovering over (63l.4), or `None` off every
    /// card. The card-board render lifts the hovered card's border so the cursor
    /// target reads before a click. Cleared when the pointer moves to empty space.
    hovered_id: Option<String>,
}

impl Default for IssueListState {
    /// An empty list: no rows, `All` chip, normal mode.
    fn default() -> Self {
        Self {
            rows: Vec::new(),
            selected: 0,
            filter: FilterChip::All,
            query: String::new(),
            mode: IssueListMode::Normal,
            create_title: String::new(),
            task_issue: HashMap::new(),
            scroll_offsets: [0; COLUMN_COUNT],
            hovered_id: None,
        }
    }
}

impl IssueListState {
    /// Seed the cache with an initial row set (e.g. an RPC snapshot).
    #[must_use]
    pub fn with_rows(rows: Vec<IssueRow>) -> Self {
        Self {
            rows,
            ..Self::default()
        }
    }

    /// The selection index into the visible rows.
    #[must_use]
    pub const fn selected_index(&self) -> usize {
        self.selected
    }

    /// The active input mode.
    #[must_use]
    pub const fn mode(&self) -> IssueListMode {
        self.mode
    }

    /// `true` when the screen is capturing free text (filter or create input), so
    /// the plugin glue routes every key (including the global tab-switch chars)
    /// into this screen's reducer rather than letting them switch tabs (e38.29).
    #[must_use]
    pub const fn is_capturing_text(&self) -> bool {
        matches!(
            self.mode,
            IssueListMode::FilterInput | IssueListMode::CreateInput
        )
    }

    /// Abort the create-input flow (Esc): drop the typed title and return to
    /// normal navigation (e38.29). A no-op when not creating.
    pub fn abort_create(&mut self) {
        if self.mode == IssueListMode::CreateInput {
            self.mode = IssueListMode::Normal;
            self.create_title.clear();
        }
    }

    /// The active filter chip.
    #[must_use]
    pub const fn filter(&self) -> FilterChip {
        self.filter
    }

    /// The current filter-input query text.
    #[must_use]
    pub fn query(&self) -> &str {
        &self.query
    }

    /// The new-issue title being typed in [`IssueListMode::CreateInput`] (e38.29),
    /// or the empty string when not in create mode.
    #[must_use]
    pub fn create_title(&self) -> &str {
        &self.create_title
    }

    /// Iterate the rows passing the active chip + query, in daemon order.
    pub fn visible_rows(&self) -> impl Iterator<Item = &IssueRow> {
        let q = self.query.to_lowercase();
        self.rows.iter().filter(move |r| {
            self.filter.accepts(r) && (q.is_empty() || r.title.to_lowercase().contains(&q))
        })
    }

    /// Iterate the visible rows that fall into `column`, in daemon order.
    pub fn rows_in_column(&self, column: IssueColumn) -> impl Iterator<Item = &IssueRow> {
        self.visible_rows()
            .filter(move |r| IssueColumn::for_state(&r.state) == column)
    }

    /// Count of visible rows in `column` (for the `Todo (12)` header suffix).
    #[must_use]
    pub fn column_count(&self, column: IssueColumn) -> usize {
        self.rows_in_column(column).count()
    }

    /// Flatten the visible rows into the five card-board columns (63l.2/63l.4) the
    /// board render paints and the mouse layer hit-tests against. Each canonical
    /// [`IssueColumn`] becomes a
    /// [`BoardColumn`](crate::widgets::card_board::BoardColumn) of
    /// [`BoardCard`](crate::widgets::card_board::BoardCard)s, in left-to-right
    /// order, carrying the per-card data a card paints (id, display id, title,
    /// priority chip, assignee initial) and the column's live scroll offset
    /// (63l.4). `render_issue_list` paints from THESE columns and `rebuild_hit_map`
    /// hit-tests against the SAME geometry, so the paint and the hit-test can never
    /// drift.
    #[must_use]
    pub fn board_columns(&self) -> Vec<crate::widgets::card_board::BoardColumn> {
        use crate::widgets::card_board::{BoardCard, BoardColumn, PriorityChip};
        IssueColumn::all()
            .into_iter()
            .enumerate()
            .map(|(idx, column)| {
                let cards =
                    self.rows_in_column(column)
                        .map(|r| BoardCard {
                            issue_id: r.id.as_str().to_string(),
                            display_id: r
                                .display_id
                                .clone()
                                .unwrap_or_else(|| r.id.as_str().to_string()),
                            title: r.title.clone(),
                            priority: PriorityChip::from_priority(r.priority),
                            assignee_initial: r.assignee.as_deref().and_then(|a| {
                                a.split_once(':').map_or(a, |(_, id)| id).chars().next()
                            }),
                        })
                        .collect::<Vec<_>>();
                // Clamp the stored offset to the column's card count so a column
                // that shrank (a moved/deleted card) never scrolls past its last
                // card into a blank body.
                let scroll_offset = self.scroll_offsets[idx].min(cards.len().saturating_sub(1));
                BoardColumn {
                    glyph: column_glyph(column),
                    name: column.label().to_string(),
                    cards,
                    scroll_offset,
                }
            })
            .collect()
    }

    /// The `(column_index, card_index)` of the selected card within the board
    /// columns (63l.4), or `None` when nothing is selected / the selection falls
    /// outside the visible board. The card-board render draws the heavy clay
    /// border on this card. Resolved by matching the selected visible row's id
    /// against the per-column card lists, so the selection a keyboard `j`/`k` (or
    /// a mouse `Select`) moved is the card that reads as raised.
    #[must_use]
    pub fn selected_board_card(&self) -> Option<(usize, usize)> {
        let selected_id = self.selected_row()?.id.as_str().to_string();
        self.board_position_of(&selected_id)
    }

    /// The `(column_index, card_index)` of the card the card-board render draws
    /// with the heavy clay border (63l.4): the HOVERED card when the pointer is
    /// over one (the cursor target reads before a click), else the keyboard /
    /// mouse-`Select` selection. `None` when neither resolves to a visible card.
    #[must_use]
    pub fn highlight_board_card(&self) -> Option<(usize, usize)> {
        if let Some(hover) = self.hovered_id.as_deref() {
            if let Some(pos) = self.board_position_of(hover) {
                return Some(pos);
            }
        }
        self.selected_board_card()
    }

    /// The `(column_index, card_index)` of the card carrying issue `id` within the
    /// board columns, or `None` when it is not visible. Shared by the selection +
    /// hover highlight resolvers (63l.4).
    fn board_position_of(&self, id: &str) -> Option<(usize, usize)> {
        for (col_idx, column) in IssueColumn::all().into_iter().enumerate() {
            if let Some(card_idx) = self
                .rows_in_column(column)
                .position(|r| r.id.as_str() == id)
            {
                return Some((col_idx, card_idx));
            }
        }
        None
    }

    /// The id of the card the pointer is hovering, if any (63l.4). The render
    /// lifts this card's border so the cursor target reads before a click.
    #[must_use]
    pub fn hovered_id(&self) -> Option<&str> {
        self.hovered_id.as_deref()
    }

    /// Set (or clear with `None`) the hovered card id (63l.4). A pointer move over
    /// a card highlights it; a move onto empty space clears it.
    pub fn set_hover(&mut self, id: Option<String>) {
        self.hovered_id = id;
    }

    /// Scroll one canonical `column` vertically by `delta` rows (63l.4): `+1`
    /// reveals a card further down, `-1` scrolls back up. The offset saturates at
    /// `0` (never negative) and is clamped against the column's card count by
    /// [`Self::board_columns`] so a scroll past the last card is a no-op. A
    /// wheel-scroll over a column's body drives this.
    pub fn scroll_column(&mut self, column: IssueColumn, delta: i32) {
        let idx = column_index(column);
        let current = self.scroll_offsets[idx];
        let next = if delta >= 0 {
            current.saturating_add(delta.unsigned_abs() as usize)
        } else {
            current.saturating_sub(delta.unsigned_abs() as usize)
        };
        // Cap at the last card so a column never scrolls into a blank body.
        let card_count = self.rows_in_column(column).count();
        self.scroll_offsets[idx] = next.min(card_count.saturating_sub(1));
    }

    /// Optimistically move the issue with `id` into `to_status`'s column (63l.4),
    /// mutating its cached `state` so the board reflects a cross-column drag
    /// immediately — ahead of the daemon's reconciling `IssueUpdated` push. A
    /// no-op when no cached row carries that id (a stale drag). The daemon
    /// `hangar/issue_update{state}` RPC (fired by the plugin glue) is the source of
    /// truth; this local move keeps the UI responsive and is reconciled (not
    /// duplicated) when the event arrives.
    ///
    /// Returns the issue's wire id when a row was moved (so the caller knows a real
    /// move happened and can fire the RPC), else `None`.
    pub fn move_issue_to(&mut self, id: &str, to_status: IssueLifecycle) -> Option<String> {
        let row = self.rows.iter_mut().find(|r| r.id.as_str() == id)?;
        row.state = to_status.as_str().to_string();
        self.clamp_selection();
        Some(id.to_string())
    }

    /// Reorder the issue with `id` to `to_index` within its own column (63l.4),
    /// moving its row in the daemon-order `rows` vec so the card-board paints it at
    /// the new slot. `to_index` is clamped to the column's bounds. A no-op when no
    /// cached row carries that id.
    ///
    /// ## Reorder model: local order, not a priority rewrite
    ///
    /// A same-column drag is treated as a *display reorder only* — it reseats the
    /// row in the local cache and does NOT rewrite the issue's `priority` (the
    /// board's vertical axis is daemon order, not priority). A silent priority
    /// nudge on every within-column drag would surprise the user and round-trip a
    /// mutation they didn't intend; the cross-column drag (which IS a real
    /// lifecycle change) is the one that fires a daemon RPC. The local order
    /// reconciles to the daemon's on the next snapshot re-pull.
    pub fn reorder_within_column(&mut self, id: &str, to_index: usize) {
        // Resolve the dragged row's column + its absolute index in `rows`.
        let Some((from_abs, column)) = self
            .rows
            .iter()
            .enumerate()
            .find(|(_, r)| r.id.as_str() == id)
            .map(|(i, r)| (i, IssueColumn::for_state(&r.state)))
        else {
            return;
        };
        // The absolute `rows` indices of every row in that column, in order.
        let column_abs: Vec<usize> = self
            .rows
            .iter()
            .enumerate()
            .filter(|(_, r)| IssueColumn::for_state(&r.state) == column)
            .map(|(i, _)| i)
            .collect();
        let Some(from_pos) = column_abs.iter().position(|&i| i == from_abs) else {
            return;
        };
        // Clamp the target slot into the column's range.
        let to_pos = to_index.min(column_abs.len().saturating_sub(1));
        if to_pos == from_pos {
            return;
        }
        // Lift the dragged row out, then reinsert it relative to the column member
        // at the target slot. After the removal the column membership shifts: the
        // anchor member's absolute index drops by one if it sat after the removed
        // row. Dragging DOWN lands the row just AFTER the anchor; dragging UP just
        // BEFORE it.
        let row = self.rows.remove(from_abs);
        let anchor_abs_orig = column_abs[to_pos];
        let anchor_abs = if anchor_abs_orig > from_abs {
            anchor_abs_orig.saturating_sub(1)
        } else {
            anchor_abs_orig
        };
        let insert_at = if to_pos > from_pos {
            anchor_abs.saturating_add(1)
        } else {
            anchor_abs
        };
        self.rows.insert(insert_at.min(self.rows.len()), row);
        self.clamp_selection();
    }

    /// The currently selected visible row, if any.
    #[must_use]
    pub fn selected_row(&self) -> Option<&IssueRow> {
        self.visible_rows().nth(self.selected)
    }

    /// Select the row whose issue id matches `id` (e38.13 command-palette jump).
    ///
    /// Resets the chip filter to `All` and clears the query first so the target is
    /// guaranteed visible (a search hit may live under any state, and the active
    /// filter could otherwise hide it), then points the selection at its visible
    /// index. A no-op when no cached row carries that id (a stale palette hit).
    pub fn select_by_id(&mut self, id: &str) {
        self.filter = FilterChip::All;
        self.query.clear();
        let idx = self.visible_rows().position(|r| r.id.as_str() == id);
        if let Some(idx) = idx {
            self.selected = idx;
        }
    }

    /// Number of currently visible rows (selection upper bound).
    fn visible_len(&self) -> usize {
        self.visible_rows().count()
    }

    /// Clamp `selected` into `0..visible_len` after a mutation that may have
    /// shrunk the visible set (filter change, deletion).
    fn clamp_selection(&mut self) {
        let len = self.visible_len();
        if len == 0 {
            self.selected = 0;
        } else if self.selected >= len {
            self.selected = len - 1;
        }
    }
}

/// An input the issue-list reducer folds into [`IssueListState`].
///
/// Key presses arrive as [`IssueListEvent::Key`]; chip selection has its own
/// variant ([`IssueListEvent::SetFilter`]) because it is raised by the chip bar
/// rather than a single keystroke; host stream events arrive wrapped in
/// [`IssueListEvent::Event`].
// Reduction enum: `Event(HangarEvent)` dominates the size, the rest are scalar
// inputs. Short-lived, reducer-folded, not a hot allocation path — left unboxed
// for consistency with the other screen reducers (boxing would only add churn).
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IssueListEvent {
    /// A printable key was pressed (`'j'`, `'\n'` for enter, `'/'`, `'c'`, …).
    Key(char),
    /// The active filter chip was changed.
    SetFilter(FilterChip),
    /// A domain event arrived on the subscribed stream.
    Event(HangarEvent),
}

/// A side-effect the plugin glue performs after an issue-list [`reduce_issue_list`].
///
/// The reducer is pure, so it surfaces the *desire* to navigate or open a flow
/// as an intent and lets the IO layer carry it out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IssueListIntent {
    /// Open the task-detail screen for the issue under the selection.
    OpenTaskDetail(IssueId),
    /// Open the agent-picker modal for the issue under the selection.
    OpenAgentPicker(IssueId),
    /// Submit a new issue with the typed `title` (e38.29). Raised when Enter is
    /// pressed on a non-blank title in [`IssueListMode::CreateInput`]; the plugin
    /// glue lifts it into a `hangar/issue_create` RPC.
    CreateIssue {
        /// The non-blank title typed in the create input.
        title: String,
    },
}

/// The result of folding one [`IssueListEvent`] into an [`IssueListState`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueListReduction {
    /// The next issue-list state.
    pub state: IssueListState,
    /// A side-effect for the plugin glue, if any.
    pub intent: Option<IssueListIntent>,
}

/// Fold one [`IssueListEvent`] into `state`, returning the next state and any
/// [`IssueListIntent`]. Pure: no IO, no mutation of the input `state`.
#[must_use]
pub fn reduce_issue_list(state: &IssueListState, ev: IssueListEvent) -> IssueListReduction {
    match ev {
        IssueListEvent::Key(c) => reduce_key(state, c),
        IssueListEvent::SetFilter(chip) => set_filter(state, chip),
        IssueListEvent::Event(event) => fold_event(state, event),
    }
}

/// Handle a printable-key press, dispatching on the active input mode.
fn reduce_key(state: &IssueListState, c: char) -> IssueListReduction {
    match state.mode {
        IssueListMode::FilterInput => reduce_filter_input_key(state, c),
        IssueListMode::CreateInput => reduce_create_input_key(state, c),
        IssueListMode::Normal => reduce_normal_key(state, c),
    }
}

/// Normal-mode key handling: navigation + intent-raising keys.
fn reduce_normal_key(state: &IssueListState, c: char) -> IssueListReduction {
    match c {
        'j' => move_selection_down(state),
        'k' => move_selection_up(state),
        '/' => enter_filter_mode(state),
        'c' => enter_create_mode(state),
        'a' => state.selected_row().map_or_else(
            || unchanged(state),
            |row| {
                with_intent(
                    state.clone(),
                    IssueListIntent::OpenAgentPicker(row.id.clone()),
                )
            },
        ),
        // Enter (delivered as '\n' or '\r') opens the selected row's task detail.
        '\n' | '\r' => state.selected_row().map_or_else(
            || unchanged(state),
            |row| {
                with_intent(
                    state.clone(),
                    IssueListIntent::OpenTaskDetail(row.id.clone()),
                )
            },
        ),
        _ => unchanged(state),
    }
}

/// Filter-input-mode key handling: Enter/Esc leave the mode, Backspace deletes,
/// any other printable char appends to the query.
fn reduce_filter_input_key(state: &IssueListState, c: char) -> IssueListReduction {
    let mut next = state.clone();
    match c {
        // Commit / abort filter entry: drop back to navigation. The query
        // itself stays applied (Enter) or is cleared (Esc handled by the router).
        '\n' | '\r' => next.mode = IssueListMode::Normal,
        // Backspace.
        '\u{8}' | '\u{7f}' => {
            next.query.pop();
        }
        other => next.query.push(other),
    }
    next.clamp_selection();
    no_intent(next)
}

/// Move the selection one row down, saturating at the last visible row (`j`).
fn move_selection_down(state: &IssueListState) -> IssueListReduction {
    let len = state.visible_len();
    if len == 0 {
        return unchanged(state);
    }
    let mut next = state.clone();
    next.selected = (next.selected + 1).min(len - 1);
    no_intent(next)
}

/// Move the selection one row up, saturating at the top (`k`).
fn move_selection_up(state: &IssueListState) -> IssueListReduction {
    let mut next = state.clone();
    next.selected = next.selected.saturating_sub(1);
    no_intent(next)
}

/// Enter filter-input mode (`/`).
fn enter_filter_mode(state: &IssueListState) -> IssueListReduction {
    let mut next = state.clone();
    next.mode = IssueListMode::FilterInput;
    no_intent(next)
}

/// Enter create-input mode (`c`, e38.29): start typing a new-issue title with an
/// empty buffer. No intent yet — the title is captured first, then Enter submits.
fn enter_create_mode(state: &IssueListState) -> IssueListReduction {
    let mut next = state.clone();
    next.mode = IssueListMode::CreateInput;
    next.create_title = String::new();
    no_intent(next)
}

/// Create-input-mode key handling (e38.29): Enter submits a non-blank title
/// (leaving the mode + emitting [`IssueListIntent::CreateIssue`]), Backspace
/// deletes the last char, any other printable char appends. Enter on a
/// blank/whitespace title is a no-op that keeps the mode open (never an empty
/// issue). Esc is handled by the router (it clears the buffer via
/// [`IssueListState::abort_create`]).
fn reduce_create_input_key(state: &IssueListState, c: char) -> IssueListReduction {
    let mut next = state.clone();
    match c {
        '\n' | '\r' => {
            if next.create_title.trim().is_empty() {
                // Blank title: keep the mode open, submit nothing.
                return no_intent(next);
            }
            let title = next.create_title.trim().to_string();
            next.mode = IssueListMode::Normal;
            next.create_title = String::new();
            return with_intent(next, IssueListIntent::CreateIssue { title });
        }
        '\u{8}' | '\u{7f}' => {
            next.create_title.pop();
        }
        other => next.create_title.push(other),
    }
    no_intent(next)
}

/// Apply a new filter chip and re-clamp the selection into the new visible set.
fn set_filter(state: &IssueListState, chip: FilterChip) -> IssueListReduction {
    let mut next = state.clone();
    next.filter = chip;
    next.clamp_selection();
    no_intent(next)
}

/// Fold a host [`HangarEvent`] into the cached rows.
fn fold_event(state: &IssueListState, event: HangarEvent) -> IssueListReduction {
    let mut next = state.clone();
    match event {
        HangarEvent::IssueCreated(row) | HangarEvent::IssueUpdated(row) => {
            upsert_row(&mut next.rows, row);
        }
        HangarEvent::IssueDeleted { issue_id } => {
            next.rows.retain(|r| r.id != issue_id);
        }
        HangarEvent::TaskQueued {
            task_id, issue_id, ..
        } => {
            // Remember which issue this task belongs to so a later TaskStarted
            // (which carries only the task id) can promote the right issue.
            next.task_issue
                .insert(task_id.as_str().to_string(), issue_id);
        }
        HangarEvent::TaskStarted { task_id, .. } => {
            if let Some(issue_id) = next.task_issue.get(task_id.as_str()).cloned() {
                promote_to_in_progress(&mut next.rows, &issue_id);
            }
        }
        // Remaining events (progress, message, finished, comment, presence) do
        // not change the issue-list board; the task-detail screen consumes them.
        _ => {}
    }
    next.clamp_selection();
    no_intent(next)
}

/// Insert `row` or replace the existing row with the same id (preserving its
/// position so the board doesn't reshuffle on an in-place update).
fn upsert_row(rows: &mut Vec<IssueRow>, row: IssueRow) {
    if let Some(existing) = rows.iter_mut().find(|r| r.id == row.id) {
        *existing = row;
    } else {
        rows.push(row);
    }
}

/// Flip the issue with `issue_id` into the `in_progress` lifecycle state so it
/// renders in the In Progress column.
fn promote_to_in_progress(rows: &mut [IssueRow], issue_id: &IssueId) {
    if let Some(row) = rows.iter_mut().find(|r| &r.id == issue_id) {
        row.state = "in_progress".to_string();
    }
}

/// A reduction that changes state but emits no intent.
const fn no_intent(state: IssueListState) -> IssueListReduction {
    IssueListReduction {
        state,
        intent: None,
    }
}

/// A reduction carrying `intent` alongside `state`.
const fn with_intent(state: IssueListState, intent: IssueListIntent) -> IssueListReduction {
    IssueListReduction {
        state,
        intent: Some(intent),
    }
}

/// A no-op reduction: state cloned unchanged, no intent.
fn unchanged(state: &IssueListState) -> IssueListReduction {
    no_intent(state.clone())
}

// ---------------------------------------------------------------------------
// Card-board rendering (63l.4)
// ---------------------------------------------------------------------------

/// Muted text for the create-issue input bar's keybinding hint.
const MUTED_GRAY: Color = Color::rgb(120, 120, 140);

/// Render the issue list into `buf` between `top` and `bottom` (63l.4).
///
/// The chip bar takes the first body row; the five-column Linear-style card-board
/// ([`crate::widgets::card_board`]) fills the rest — `backlog` … `done` columns
/// side by side, each a per-column-scrollable stack of bordered cards showing the
/// `HGR-<n>` id, title, priority chip, and assignee. The selected (or hovered)
/// card carries the heavy clay border. The inline create-issue input overlays the
/// bottom row when active.
///
/// `working_count` is the number of agents currently working, surfaced as the
/// top-right avatar-stack chip (the reference's `WorkspaceAgentWorkingChip`).
pub fn render_issue_list(
    buf: &mut WireBuffer,
    area_w: u16,
    top: u16,
    bottom: u16,
    state: &IssueListState,
    working_count: usize,
) {
    // Filter-chip bar on the first body row, reusing the shared widget so the
    // chips render identically here and on the skill manager (P4.6).
    let chip_labels: Vec<&str> = FilterChip::all().iter().map(|c| c.label()).collect();
    let active_chip = FilterChip::all()
        .iter()
        .position(|c| *c == state.filter)
        .unwrap_or(0);
    crate::widgets::filter_chip::render_chip_bar(buf, top, area_w, &chip_labels, active_chip);
    // Working-agents avatar stack, right-aligned on the same chip row.
    crate::widgets::working_chip::render_working_chip(buf, top, area_w, working_count);

    // e38.29: the inline create-issue input bar, when active, takes the bottom row
    // as a single-line text input (`New issue · Title: <typed>▏`). Drawn last so it
    // overlays the list; the rows above keep rendering as context.
    if state.mode == IssueListMode::CreateInput {
        render_create_bar(buf, area_w, bottom.saturating_sub(1), &state.create_title);
    }

    // 63l.4 — the Issues screen is the headline of the redesign: it renders
    // through the Linear-style card-board widget (five status columns side by
    // side, each a scrollable stack of bordered cards) rather than the old
    // vertical section bands. The board runs from the first body row below the
    // chip bar (`col_top`) to the footer (`bottom`).
    //
    // The selected card carries the heavy clay border; when the pointer is
    // hovering a card it takes the highlight instead (the cursor target reads
    // before a click), falling back to the keyboard selection when nothing is
    // hovered. The per-column scroll offsets ride on the board columns
    // ([`IssueListState::board_columns`]), so the SAME geometry feeds the render,
    // the hit-map, and the mouse layer.
    let col_top = top.saturating_add(1);
    let columns = state.board_columns();
    let highlight = state.highlight_board_card();
    let _ = crate::widgets::card_board::render_card_board(
        buf, area_w, col_top, bottom, &columns, highlight,
    );
}

/// Accent for the create-issue input bar (a calm emerald, distinct from the
/// gold headers + green selection so the create prompt reads as its own mode).
const CREATE_ACCENT: Color = Color::rgb(120, 200, 160);

/// Render the inline create-issue input bar at `(0, row)` (e38.29):
/// `New issue · Title: <typed>▏` in the create accent followed by a muted
/// keybinding hint. The caret `▏` sits after the typed text. Char-safe via
/// [`put_str`].
fn render_create_bar(buf: &mut WireBuffer, area_w: u16, row: u16, title: &str) {
    // `New issue` + `Title:` are the prompt labels the tripwire detects to know
    // the create input is active.
    let prompt = format!("New issue · Title: {title}▏");
    let next = put_str(buf, 0, row, &prompt, CREATE_ACCENT, area_w);
    let hint = "  (Enter submit · Esc cancel)";
    put_str(buf, next, row, hint, MUTED_GRAY, area_w);
}

/// Write `s` at `(x, row)` in `color`, clipping at `area_w`. Returns the next
/// free column. Multi-byte safe (iterates `char`s, not bytes).
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

    fn row(id: &str, state: &str, assignee: Option<&str>) -> IssueRow {
        IssueRow {
            id: IssueId::from_str(id).unwrap(),
            display_id: None,
            workspace_id: "ws".into(),
            title: format!("Issue {id}"),
            description: None,
            state: state.into(),
            assignee: assignee.map(ToString::to_string),
            creator: "member:alice".into(),
            created_at: 0,
            priority: 0,
            due_date: None,
            labels: Vec::new(),
            pr_url: None,
        }
    }

    /// Unknown lifecycle strings fall into Todo (fail-visible).
    #[test]
    fn unknown_state_buckets_to_todo() {
        assert_eq!(IssueColumn::for_state("open"), IssueColumn::Todo);
        assert_eq!(IssueColumn::for_state("weird"), IssueColumn::Todo);
        assert_eq!(
            IssueColumn::for_state("in_progress"),
            IssueColumn::InProgress
        );
        assert_eq!(IssueColumn::for_state("done"), IssueColumn::Done);
        assert_eq!(IssueColumn::for_state("closed"), IssueColumn::Done);
    }

    /// `k` moves the selection up and saturates at the top.
    #[test]
    fn k_key_moves_selection_up_and_saturates() {
        let s = IssueListState::with_rows(vec![row("i1", "open", None), row("i2", "open", None)]);
        let down = reduce_issue_list(&s, IssueListEvent::Key('j'));
        assert_eq!(down.state.selected_index(), 1);
        let up = reduce_issue_list(&down.state, IssueListEvent::Key('k'));
        assert_eq!(up.state.selected_index(), 0);
        // Already at top → no underflow.
        let up2 = reduce_issue_list(&up.state, IssueListEvent::Key('k'));
        assert_eq!(up2.state.selected_index(), 0);
    }

    /// `a` on a selected row opens the agent picker for that issue.
    #[test]
    fn a_key_opens_agent_picker_for_selected_issue() {
        let s = IssueListState::with_rows(vec![row("i1", "open", None)]);
        let out = reduce_issue_list(&s, IssueListEvent::Key('a'));
        assert_eq!(
            out.intent,
            Some(IssueListIntent::OpenAgentPicker(
                IssueId::from_str("i1").unwrap()
            ))
        );
    }

    /// Switching to the Members chip then back to All re-clamps the selection
    /// without panicking and restores the wider view.
    #[test]
    fn filter_change_clamps_selection() {
        let mut s = IssueListState::with_rows(vec![
            row("i1", "open", Some("member:a")),
            row("i2", "open", Some("agent:b")),
            row("i3", "open", Some("agent:c")),
        ]);
        // Select the 3rd row, then filter to Members (only 1 visible).
        s = reduce_issue_list(&s, IssueListEvent::Key('j')).state;
        s = reduce_issue_list(&s, IssueListEvent::Key('j')).state;
        assert_eq!(s.selected_index(), 2);
        let filtered = reduce_issue_list(&s, IssueListEvent::SetFilter(FilterChip::Members));
        // Only i1 visible → selection clamps to 0.
        assert_eq!(filtered.state.selected_index(), 0);
        assert_eq!(filtered.state.visible_rows().count(), 1);
    }

    /// Filter-input mode appends typed chars to the query and narrows by title.
    #[test]
    fn filter_input_narrows_by_title_substring() {
        let s = IssueListState::with_rows(vec![
            row("i1", "open", None), // title "Issue i1"
            row("i2", "open", None), // title "Issue i2"
        ]);
        let s = reduce_issue_list(&s, IssueListEvent::Key('/')).state;
        assert_eq!(s.mode(), IssueListMode::FilterInput);
        let s = reduce_issue_list(&s, IssueListEvent::Key('i')).state;
        let s = reduce_issue_list(&s, IssueListEvent::Key('2')).state;
        assert_eq!(s.query(), "i2");
        let visible: Vec<&str> = s.visible_rows().map(|r| r.id.as_str()).collect();
        assert_eq!(visible, vec!["i2"]);
    }

    /// A labelled issue still renders as a card on the board (63l.4). The card
    /// anatomy carries id/title/priority/assignee, not label chips, so the proof
    /// is that the labelled issue paints its id + title inside the board (it is not
    /// dropped just because it carries labels).
    #[test]
    fn labelled_issue_renders_as_a_card() {
        let mut r = row("i1", "open", Some("agent:claude"));
        r.labels = vec!["bug".into()];
        let s = IssueListState::with_rows(vec![r]);

        let mut buf = WireBuffer::new(120, 16);
        render_issue_list(&mut buf, 120, 1, 15, &s, 0);

        let painted = painted_text(&buf);
        assert!(
            painted.contains("Issue i1"),
            "the labelled issue must render as a card on the board: {painted:?}"
        );
    }

    /// A card renders its HGR-<n> display id on the id line (63l.4). The daemon
    /// supplies the id; the card paints it so a person reading the board sees the
    /// human-facing id leading the tile.
    #[test]
    fn card_renders_its_display_id() {
        let mut r = row("i1", "todo", None);
        r.display_id = Some("HGR-7".into());
        let s = IssueListState::with_rows(vec![r]);

        let mut buf = WireBuffer::new(120, 16);
        render_issue_list(&mut buf, 120, 1, 15, &s, 0);

        let painted = painted_text(&buf);
        assert!(
            painted.contains("HGR-7"),
            "the card must render its display id: {painted:?}"
        );
        // The card paints the id line ABOVE the title (the card anatomy order), so
        // the id precedes the title in row-major paint order.
        let id_at = painted.find("HGR-7").expect("display id painted");
        let title_at = painted.find("Issue i1").expect("title painted");
        assert!(
            id_at < title_at,
            "the id line must paint before the title: {painted:?}"
        );
    }

    /// A row with no display id (a pre-63l.3 snapshot) renders the raw issue id on
    /// the card's id line (the card-board falls back to the issue id), and the
    /// title still paints — no panic.
    #[test]
    fn card_without_display_id_falls_back_to_issue_id() {
        let s = IssueListState::with_rows(vec![row("i1", "todo", None)]);
        let mut buf = WireBuffer::new(120, 16);
        render_issue_list(&mut buf, 120, 1, 15, &s, 0);
        let painted = painted_text(&buf);
        assert!(
            painted.contains("Issue i1"),
            "the title must still render without a display id: {painted:?}"
        );
        // The card-board paints the raw issue id when no display id is supplied.
        assert!(
            painted.contains("i1"),
            "the card falls back to the raw issue id: {painted:?}"
        );
    }

    /// 63l.4 — an optimistic cross-column move flips the issue's cached state into
    /// the destination column so the board reflects a drag immediately, and reports
    /// the moved id so the glue knows to fire the daemon RPC.
    #[test]
    fn move_issue_to_flips_cached_state_and_reports_id() {
        let mut s = IssueListState::with_rows(vec![row("i1", "todo", None)]);
        // The issue starts in Todo.
        assert_eq!(s.column_count(IssueColumn::Todo), 1);
        assert_eq!(s.column_count(IssueColumn::InProgress), 0);

        let moved = s.move_issue_to("i1", IssueLifecycle::InProgress);
        assert_eq!(moved.as_deref(), Some("i1"), "the moved id is reported");
        // The board now shows the card under In Progress, not Todo.
        assert_eq!(s.column_count(IssueColumn::Todo), 0);
        assert_eq!(s.column_count(IssueColumn::InProgress), 1);

        // A move of an unknown id is a no-op (no row, no reported id).
        assert_eq!(s.move_issue_to("ghost", IssueLifecycle::Done), None);
    }

    /// 63l.4 — `selected_board_card` / `highlight_board_card` resolve the selected
    /// row to its `(column, card)` slot on the board, and a hover overrides the
    /// selection for the highlight (the cursor target reads before a click).
    #[test]
    fn highlight_resolves_hover_over_selection() {
        let mut s = IssueListState::with_rows(vec![
            row("i-todo", "todo", None),
            row("i-prog", "in_progress", None),
        ]);
        // Selection defaults to the first visible row (i-todo, Todo column = 1).
        assert_eq!(s.selected_board_card(), Some((1, 0)));
        assert_eq!(s.highlight_board_card(), Some((1, 0)));

        // Hovering the in-progress card overrides the highlight to (2, 0).
        s.set_hover(Some("i-prog".to_string()));
        assert_eq!(
            s.highlight_board_card(),
            Some((2, 0)),
            "hover overrides the selection for the highlight"
        );
        // Clearing the hover falls back to the selection.
        s.set_hover(None);
        assert_eq!(s.highlight_board_card(), Some((1, 0)));
    }

    /// 63l.4 — a wheel-scroll over a column nudges its scroll offset, saturating at
    /// `0` upward and capped at the last card downward, and the offset rides on the
    /// board columns so the render skips the scrolled-off cards.
    #[test]
    fn scroll_column_offsets_board_and_saturates() {
        let rows: Vec<IssueRow> = (0..4)
            .map(|i| row(&format!("t{i}"), "todo", None))
            .collect();
        let mut s = IssueListState::with_rows(rows);

        // Scroll the Todo column down twice → offset 2.
        s.scroll_column(IssueColumn::Todo, 1);
        s.scroll_column(IssueColumn::Todo, 1);
        let cols = s.board_columns();
        let todo = &cols[column_index(IssueColumn::Todo)];
        assert_eq!(todo.scroll_offset, 2, "two down-scrolls offset by 2");

        // Scrolling up past the top saturates at 0.
        s.scroll_column(IssueColumn::Todo, -5);
        let cols = s.board_columns();
        assert_eq!(cols[column_index(IssueColumn::Todo)].scroll_offset, 0);

        // Scrolling down past the last card caps at len-1 (never a blank body).
        for _ in 0..10 {
            s.scroll_column(IssueColumn::Todo, 1);
        }
        let cols = s.board_columns();
        assert_eq!(cols[column_index(IssueColumn::Todo)].scroll_offset, 3);
    }

    /// 63l.4 — a same-column reorder reseats the dragged row within its column's
    /// display order WITHOUT rewriting its priority (local order only). The other
    /// columns' rows are untouched.
    #[test]
    fn reorder_within_column_reseats_local_order_only() {
        let mut s = IssueListState::with_rows(vec![
            row("t0", "todo", None),
            row("t1", "todo", None),
            row("t2", "todo", None),
            row("p0", "in_progress", None),
        ]);
        // Todo order starts t0, t1, t2.
        let order: Vec<&str> = s
            .rows_in_column(IssueColumn::Todo)
            .map(|r| r.id.as_str())
            .collect();
        assert_eq!(order, vec!["t0", "t1", "t2"]);

        // Drag t0 to slot 2 (the bottom of the Todo column) — a downward move.
        s.reorder_within_column("t0", 2);
        let order: Vec<&str> = s
            .rows_in_column(IssueColumn::Todo)
            .map(|r| r.id.as_str())
            .collect();
        assert_eq!(order, vec!["t1", "t2", "t0"], "t0 reseats to the bottom");

        // Drag t0 back to slot 0 (the top) — an upward move.
        s.reorder_within_column("t0", 0);
        let order: Vec<&str> = s
            .rows_in_column(IssueColumn::Todo)
            .map(|r| r.id.as_str())
            .collect();
        assert_eq!(order, vec!["t0", "t1", "t2"], "t0 reseats back to the top");

        // The In Progress column is untouched.
        let prog: Vec<&str> = s
            .rows_in_column(IssueColumn::InProgress)
            .map(|r| r.id.as_str())
            .collect();
        assert_eq!(prog, vec!["p0"]);

        // A reorder of an unknown id is a no-op.
        let before: Vec<String> = s
            .rows_in_column(IssueColumn::Todo)
            .map(|r| r.id.as_str().to_string())
            .collect();
        s.reorder_within_column("ghost", 0);
        let after: Vec<String> = s
            .rows_in_column(IssueColumn::Todo)
            .map(|r| r.id.as_str().to_string())
            .collect();
        assert_eq!(before, after);
    }

    /// Reconstruct the full painted text of a rendered buffer (every cell, in
    /// row-major order) so a render assertion can search for headers / glyphs.
    fn painted_text(buf: &WireBuffer) -> String {
        let mut out = String::new();
        for y in 0..buf.height {
            for (coord, cell) in &buf.cells {
                if coord.y == y {
                    out.push_str(&cell.symbol);
                }
            }
        }
        out
    }

    /// The Issues screen renders through the five-column card-board (63l.4): every
    /// canonical lifecycle column appears with its live count header, and a
    /// representative row is bucketed into each as a CARD (its id painted inside a
    /// bordered tile). A `backlog` and an `in_review` row prove the two outer
    /// columns are not dropped, and the legacy `open` / `closed` tokens still land
    /// under Todo / Done via the canonical helper.
    #[test]
    fn renders_all_five_canonical_columns_with_counts() {
        let s = IssueListState::with_rows(vec![
            row("i-backlog", "backlog", None),
            row("i-todo", "todo", None),
            row("i-open", "open", None), // legacy -> Todo
            row("i-prog", "in_progress", None),
            row("i-review", "in_review", None),
            row("i-done", "done", None),
            row("i-closed", "closed", None), // legacy -> Done
        ]);

        // A wide board so every 16-cell column fits its header + card.
        let mut buf = WireBuffer::new(120, 24);
        render_issue_list(&mut buf, 120, 1, 23, &s, 0);
        let painted = painted_text(&buf);

        // Every canonical column header with its live count.
        for header in [
            "Backlog (1)",
            "Todo (2)", // todo + legacy open
            "In Progress (1)",
            "In Review (1)",
            "Done (2)", // done + legacy closed
        ] {
            assert!(
                painted.contains(header),
                "missing column header {header:?} in:\n{painted}"
            );
        }

        // Each column's representative issue renders as a card (its id painted).
        for id in [
            "i-backlog",
            "i-todo",
            "i-open",
            "i-prog",
            "i-review",
            "i-done",
            "i-closed",
        ] {
            assert!(
                painted.contains(id),
                "missing card id {id:?} in:\n{painted}"
            );
        }
        // The board paints bordered, rounded cards — the rounded corner glyph is
        // the card-board signature (the old band layout painted bare rows).
        assert!(
            painted.contains('╭'),
            "the board must paint rounded card borders:\n{painted}"
        );
    }

    /// A card paints its `HGR-<n>` display id and a coloured priority chip (63l.4
    /// card anatomy), so the board reads as Linear-style tiles, not bare rows.
    #[test]
    fn card_paints_display_id_and_priority_chip() {
        let mut r = row("i1", "in_progress", Some("agent:claude"));
        r.display_id = Some("HGR-9".into());
        r.priority = 3; // Urgent
        let s = IssueListState::with_rows(vec![r]);

        let mut buf = WireBuffer::new(120, 24);
        render_issue_list(&mut buf, 120, 1, 23, &s, 0);
        let painted = painted_text(&buf);

        assert!(
            painted.contains("HGR-9"),
            "the card must paint its display id: {painted:?}"
        );
        // The Urgent priority chip's label + filled-diamond glyph render.
        assert!(
            painted.contains("Urgent"),
            "the card must paint its priority chip label: {painted:?}"
        );
        assert!(
            painted.contains('◆'),
            "the card must paint the priority chip glyph: {painted:?}"
        );
    }

    /// Each canonical lifecycle string buckets into its own column, and the
    /// legacy tokens map forward — the plugin delegates to the one
    /// `IssueLifecycle` source of truth (63l.3).
    #[test]
    fn for_state_maps_every_canonical_and_legacy_token() {
        assert_eq!(IssueColumn::for_state("backlog"), IssueColumn::Backlog);
        assert_eq!(IssueColumn::for_state("todo"), IssueColumn::Todo);
        assert_eq!(
            IssueColumn::for_state("in_progress"),
            IssueColumn::InProgress
        );
        assert_eq!(IssueColumn::for_state("in_review"), IssueColumn::InReview);
        assert_eq!(IssueColumn::for_state("done"), IssueColumn::Done);
        // Legacy + unknown.
        assert_eq!(IssueColumn::for_state("open"), IssueColumn::Todo);
        assert_eq!(IssueColumn::for_state("weird"), IssueColumn::Todo);
        assert_eq!(IssueColumn::for_state("closed"), IssueColumn::Done);
    }

    /// The renderer draws a 50-issue fixture at the 80×24 floor without writing
    /// a single out-of-bounds cell (cross-cutting 80×24 floor guard, P4.md:537).
    #[test]
    fn renders_50_issues_at_floor_without_overflow() {
        const FLOOR_W: u16 = 80;
        const FLOOR_H: u16 = 24;
        let rows: Vec<IssueRow> = (0..50)
            .map(|i| {
                let mut r = row(
                    &format!("i{i}"),
                    if i % 3 == 0 { "done" } else { "open" },
                    Some("agent:c"),
                );
                // Give every row a display id so the id+separator+title path is
                // exercised at the floor — the id eats title width, and the row
                // must still never overflow into the assignee column.
                r.display_id = Some(format!("HGR-{i}"));
                r
            })
            .collect();
        let s = IssueListState::with_rows(rows);

        let mut buf = WireBuffer::new(FLOOR_W, FLOOR_H);
        assert!(buf.width >= 80 && buf.height >= 24);
        // Render the body region (row 1 .. last-but-one, leaving chrome rows).
        render_issue_list(&mut buf, FLOOR_W, 1, FLOOR_H - 1, &s, 5);

        for (coord, _) in &buf.cells {
            assert!(
                coord.x < FLOOR_W && coord.y < FLOOR_H,
                "issue list wrote out-of-bounds cell at ({}, {})",
                coord.x,
                coord.y,
            );
        }
    }

    /// Read a row's text back out of a `WireBuffer` for assertion. Cells not
    /// written render as spaces.
    fn row_text(buf: &WireBuffer, row: u16, width: u16) -> String {
        let mut out = vec![' '; width as usize];
        for (coord, cell) in &buf.cells {
            if coord.y == row && coord.x < width {
                if let Some(ch) = cell.symbol.chars().next() {
                    out[coord.x as usize] = ch;
                }
            }
        }
        out.into_iter().collect()
    }

    /// 63l.4 — the card-board spreads its columns HORIZONTALLY across the full
    /// width (Backlog left, Done right), not vertically. With a populated fixture
    /// the leftmost (`Backlog`) and rightmost (`Done`) column headers must sit on
    /// the SAME header row but at opposite ends of the board — the board uses the
    /// width, it doesn't stack the sections down the pane.
    ///
    /// Reverting to the old top-packed vertical band layout stacks the headers down
    /// one column and this side-by-side assertion fails.
    #[test]
    fn columns_spread_horizontally_across_the_board() {
        const W: u16 = 120;
        const H: u16 = 24;
        let top = 1u16;
        let bottom = H - 1; // footer pinned on the last row

        // A few rows in each of three columns so the board is populated.
        let mut rows = Vec::new();
        for i in 0..3 {
            rows.push(row(&format!("b{i}"), "backlog", Some("agent:c")));
        }
        for i in 0..2 {
            rows.push(row(&format!("p{i}"), "in_progress", Some("agent:c")));
        }
        for i in 0..2 {
            rows.push(row(&format!("d{i}"), "done", Some("agent:c")));
        }
        let s = IssueListState::with_rows(rows);

        let mut buf = WireBuffer::new(W, H);
        render_issue_list(&mut buf, W, top, bottom, &s, 0);

        // Find the x of the Backlog header glyph and the Done header glyph; they
        // sit on the same header row at opposite ends of the board.
        let backlog_x = header_glyph_x(&buf, "Backlog (").expect("Backlog header painted");
        let done_x = header_glyph_x(&buf, "Done (").expect("Done header painted");
        assert!(
            done_x > backlog_x,
            "Done must sit to the RIGHT of Backlog (horizontal columns): \
             backlog_x={backlog_x}, done_x={done_x}",
        );
        // Done sits in the rightmost fifth of a five-column board.
        assert!(
            done_x >= W * 4 / 5 - 4,
            "Done column must occupy the rightmost fifth (done_x={done_x}, w={W})",
        );
    }

    /// The `(x, _)` start column of a header label painted anywhere in the buffer
    /// (the column the card-board painted that header at), scanning row-major.
    fn header_glyph_x(buf: &WireBuffer, label: &str) -> Option<u16> {
        for y in 0..buf.height {
            let line = row_text(buf, y, buf.width);
            if let Some(byte_idx) = line.find(label) {
                // `find` returns a byte index; the header labels are ASCII here, so
                // the byte index equals the char column.
                return u16::try_from(byte_idx).ok();
            }
        }
        None
    }

    /// 63l.4 — the card-board must NOT overflow at the 80×24 floor with a dense
    /// fixture (every column clips its cards to the body, never past `bottom`).
    #[test]
    fn body_fill_layout_does_not_overflow_at_floor() {
        const FLOOR_W: u16 = 80;
        const FLOOR_H: u16 = 24;
        let rows: Vec<IssueRow> = (0..40)
            .map(|i| {
                row(
                    &format!("i{i}"),
                    match i % 3 {
                        0 => "open",
                        1 => "in_progress",
                        _ => "done",
                    },
                    Some("agent:c"),
                )
            })
            .collect();
        let s = IssueListState::with_rows(rows);

        let mut buf = WireBuffer::new(FLOOR_W, FLOOR_H);
        render_issue_list(&mut buf, FLOOR_W, 1, FLOOR_H - 1, &s, 5);

        for (coord, _) in &buf.cells {
            assert!(
                coord.x < FLOOR_W && coord.y < FLOOR_H,
                "issue list wrote out-of-bounds cell at ({}, {})",
                coord.x,
                coord.y,
            );
        }
    }
}
