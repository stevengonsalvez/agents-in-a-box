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
//! Status grouping reads the wire `state` string verbatim
//! ([`IssueColumn::for_state`]); the daemon owns the canonical lifecycle, the
//! plugin only buckets the strings it is handed. A `TaskStarted` event promotes
//! the task's issue into In Progress without waiting for an `IssueUpdated`,
//! because the daemon reports task lifecycle before it rewrites the issue row.

use std::collections::HashMap;

use ainb_hangar_core::ids::IssueId;
use ainb_hangar_proto::events::{HangarEvent, IssueRow};
use ainb_hangar_proto::lifecycle::IssueLifecycle;
use ainb_plugin_sdk::{Cell, Color, Coord, WireBuffer};

/// The number of status columns the board renders — the five canonical
/// lifecycle statuses (63l.3). Kept as a single constant so the column enum and
/// the render's [`SectionBands`] split stay in lockstep.
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

    /// Map a canonical [`IssueLifecycle`] status to its display column. The two
    /// enums are 1:1 by design; this is the seam that keeps the plugin's column
    /// vocabulary pinned to the proto source of truth.
    #[must_use]
    const fn from_lifecycle(status: IssueLifecycle) -> Self {
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
// Width-aware rendering
// ---------------------------------------------------------------------------

/// Column header accent.
const GOLD: Color = Color::rgb(255, 215, 0);
/// Muted text for unfocused rows + counts.
const MUTED_GRAY: Color = Color::rgb(120, 120, 140);
/// Primary row text.
const SOFT_WHITE: Color = Color::rgb(220, 220, 230);
/// Selection-row highlight.
const SELECTION_GREEN: Color = Color::rgb(100, 200, 100);

/// Render the issue list into `buf` between `top` and `bottom`.
///
/// All sub-widths derive from `area_w` (`project_ainb_tui_width_aware_panels`):
/// the title gets the lion's share, the assignee and status columns are sized to
/// their content caps. The selection row is highlighted; column headers carry
/// their live counts (`Todo (3)`).
///
/// `working_count` is the number of agents currently working, surfaced as the
/// top-right avatar-stack chip (Multica `WorkspaceAgentWorkingChip`).
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

    // Width split: status/assignee take fixed caps, the title absorbs the rest.
    let cols = ColumnWidths::for_area(area_w);

    // e38.39 — body-filling layout. The three status sections each get a
    // proportional vertical *band* of the body so they spread down the pane
    // instead of clustering at the top with a vast void below. The chip bar
    // consumed `top`; the column body runs from `col_top` to `bottom`, split into
    // three bands of `bottom - col_top` rows (the remainder lands on the earlier
    // bands so no row is wasted). Each section paints its header on its band's
    // first row and its issue rows beneath, clamped to the band end — a dense
    // section truncates within its share rather than pushing the next section's
    // header off-screen, which is what keeps the board readable and overflow-free
    // at the 80×24 floor.
    let col_top = top.saturating_add(1);
    let bands = SectionBands::split(col_top, bottom);

    let mut visible_index = 0usize;
    for (column, band) in IssueColumn::all().into_iter().zip(bands) {
        // Column header with live count, e.g. "Todo (3)", anchored at the band top.
        if band.start < bottom {
            let header = format!("{} ({})", column.label(), state.column_count(column));
            put_str(buf, 0, band.start, &header, GOLD, area_w);
        }

        let mut row = band.start.saturating_add(1);
        for r in state.rows_in_column(column) {
            // Paint only while inside this section's band; rows past the band are
            // truncated (their count already shows in the header). `visible_index`
            // still advances so the selection marker tracks the global visible
            // order even across a truncated row.
            if row < band.end {
                render_issue_row(buf, area_w, row, r, visible_index == state.selected, &cols);
                row = row.saturating_add(1);
            }
            visible_index = visible_index.saturating_add(1);
        }
    }
}

/// The derived sub-widths for an issue row, sized from the area width
/// (`project_ainb_tui_width_aware_panels`): the status/assignee columns take
/// fixed caps and the title absorbs the rest.
struct ColumnWidths {
    title_w: u16,
    assignee_w: u16,
}

impl ColumnWidths {
    /// Derive the row sub-widths for a body `area_w`.
    fn for_area(area_w: u16) -> Self {
        let status_w: u16 = 13; // "In Progress" + padding
        let assignee_w = area_w.saturating_sub(status_w).min(20);
        let title_w = area_w
            .saturating_sub(status_w)
            .saturating_sub(assignee_w)
            .max(8);
        Self {
            title_w,
            assignee_w,
        }
    }
}

/// One status section's vertical band: `[start, end)` rows of the body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Band {
    /// The band's header row (inclusive).
    start: u16,
    /// One past the band's last issue row (exclusive).
    end: u16,
}

/// The status sections' bands, splitting the body `[col_top, bottom)` into
/// [`COLUMN_COUNT`] contiguous, non-overlapping vertical slices — one per
/// canonical lifecycle column.
struct SectionBands {
    bands: [Band; COLUMN_COUNT],
}

impl SectionBands {
    /// Divide `[col_top, bottom)` into [`COLUMN_COUNT`] contiguous bands. The body
    /// height is split evenly; the remainder rows go to the earlier bands so the
    /// whole height is used and the last band still ends exactly at `bottom`. A
    /// body too short to give every section a row degrades gracefully (zero-height
    /// bands simply paint nothing — never out of bounds).
    fn split(col_top: u16, bottom: u16) -> [Band; COLUMN_COUNT] {
        let avail = bottom.saturating_sub(col_top);
        let n = u16::try_from(COLUMN_COUNT).unwrap_or(u16::MAX);
        let base = avail / n;
        let extra = avail % n; // leftover rows handed to the earliest bands
        let mut start = col_top;
        let mut bands = [Band { start, end: start }; COLUMN_COUNT];
        for (i, band) in bands.iter_mut().enumerate() {
            let h = base + u16::from(u16::try_from(i).unwrap_or(u16::MAX) < extra);
            // Clamp end >= start: when `bottom < col_top` (only reachable from a
            // degenerate sub-`col_top` pane) the band stays empty rather than
            // inverting to `{start, end < start}`.
            let end = start.saturating_add(h).min(bottom).max(start);
            *band = Band { start, end };
            start = end;
        }
        bands
    }
}

impl IntoIterator for SectionBands {
    type Item = Band;
    type IntoIter = std::array::IntoIter<Band, COLUMN_COUNT>;

    fn into_iter(self) -> Self::IntoIter {
        self.bands.into_iter()
    }
}

/// Paint a single issue `row` at `y`: selection marker, clipped title, assignee,
/// and trailing label chips. Extracted so the section-band loop stays legible.
fn render_issue_row(
    buf: &mut WireBuffer,
    area_w: u16,
    y: u16,
    r: &IssueRow,
    selected: bool,
    cols: &ColumnWidths,
) {
    let marker_color = if selected {
        SELECTION_GREEN
    } else {
        MUTED_GRAY
    };
    put_str(
        buf,
        0,
        y,
        if selected { "▶ " } else { "  " },
        marker_color,
        area_w,
    );

    let text_color = if selected { SOFT_WHITE } else { MUTED_GRAY };
    put_str(buf, 2, y, &clip(&r.title, cols.title_w), text_color, area_w);

    let ax = 2u16.saturating_add(cols.title_w).saturating_add(1);
    let assignee = r.assignee.as_deref().unwrap_or("—");
    let next_x = put_str(
        buf,
        ax,
        y,
        &clip(assignee, cols.assignee_w),
        MUTED_GRAY,
        area_w,
    );

    // Label chips trail the assignee in whatever width is left, clipped at the
    // area edge (a row with no labels paints nothing here).
    if !r.labels.is_empty() {
        let chips_x = next_x.saturating_add(1);
        crate::widgets::label_chip::render_label_chips(buf, chips_x, y, area_w, &r.labels);
    }
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

/// Clip `s` to at most `w` display columns (char-based, multi-byte safe).
fn clip(s: &str, w: u16) -> String {
    s.chars().take(w as usize).collect()
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

    /// A labelled issue renders its label chip on the row (e38.10).
    #[test]
    fn labelled_issue_renders_chip() {
        let mut r = row("i1", "open", Some("agent:claude"));
        r.labels = vec!["bug".into()];
        let s = IssueListState::with_rows(vec![r]);

        // Tall enough that the five canonical bands each get a header + an issue
        // row (the labelled `open` issue lands in the Todo band).
        let mut buf = WireBuffer::new(80, 16);
        render_issue_list(&mut buf, 80, 1, 15, &s, 0);

        let painted = painted_text(&buf);
        assert!(
            painted.contains("‹bug›"),
            "labelled issue must render its chip: {painted:?}"
        );
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

    /// The board renders ALL FIVE canonical lifecycle columns, each with its live
    /// count, and buckets a representative row into each (63l.3 plugin render
    /// proof). A `backlog` and an `in_review` row prove the two new columns are
    /// not dropped, and the legacy `open` / `closed` tokens still land under
    /// Todo / Done via the canonical helper.
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

        // Tall enough that every five-band header gets a row.
        let mut buf = WireBuffer::new(80, 24);
        render_issue_list(&mut buf, 80, 1, 23, &s, 0);
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
                row(
                    &format!("i{i}"),
                    if i % 3 == 0 { "done" } else { "open" },
                    Some("agent:c"),
                )
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

    /// The y-row at which a header line (e.g. `"Todo ("`, `"Done ("`) is painted,
    /// scanning the body region `[top, bottom)`.
    fn header_row(buf: &WireBuffer, top: u16, bottom: u16, label: &str) -> Option<u16> {
        (top..bottom).find(|&y| row_text(buf, y, buf.width).contains(label))
    }

    /// e38.39 — a populated landing must USE the body height: the three status
    /// sections distribute down the pane instead of clustering at the top with a
    /// vast void below. With a sparse-but-populated fixture at the 80×24 floor the
    /// last section header (`Done`) must land in the lower portion of the body,
    /// and the body's lowest painted content row must reach a sensible fraction of
    /// the available height — so there is no large empty band before the footer.
    ///
    /// Reverting to the old top-packed layout (sections stacked tightly from the
    /// first body row) lands `Done` near the top and the fill assertion fails.
    #[test]
    fn populated_landing_fills_body_height() {
        const FLOOR_W: u16 = 80;
        const FLOOR_H: u16 = 24;
        let top = 1u16;
        let bottom = FLOOR_H - 1; // footer pinned on the last row

        // Sparse but populated: a few rows in each of the three status columns.
        let mut rows = Vec::new();
        for i in 0..3 {
            rows.push(row(&format!("t{i}"), "open", Some("agent:c")));
        }
        for i in 0..2 {
            rows.push(row(&format!("p{i}"), "in_progress", Some("agent:c")));
        }
        for i in 0..2 {
            rows.push(row(&format!("d{i}"), "done", Some("agent:c")));
        }
        let s = IssueListState::with_rows(rows);

        let mut buf = WireBuffer::new(FLOOR_W, FLOOR_H);
        render_issue_list(&mut buf, FLOOR_W, top, bottom, &s, 0);

        // The body band runs from the first column row (top + 1, below the chip
        // bar) to `bottom`. The `Done` header is the last of the three sections; in
        // a body-filling layout it sits in the lower portion, not the top cluster.
        let body_top = top + 1;
        let body_h = bottom - body_top;
        let done_y =
            header_row(&buf, body_top, bottom, "Done (").expect("Done header must be painted");
        let done_frac = f64::from(done_y - body_top) / f64::from(body_h);
        assert!(
            done_frac >= 0.5,
            "Done section must distribute into the lower half of the body \
             (done_y={done_y}, body_top={body_top}, body_h={body_h}, frac={done_frac:.2}); \
             the layout is still top-packed with a void below",
        );

        // The lowest painted body row must reach near the bottom of the body, so
        // there is no large empty band between the content and the footer.
        let lowest = (body_top..bottom)
            .rev()
            .find(|&y| !row_text(&buf, y, FLOOR_W).trim().is_empty())
            .expect("body must paint at least one row");
        let fill_frac = f64::from(lowest - body_top + 1) / f64::from(body_h);
        assert!(
            fill_frac >= 0.75,
            "body content must reach at least 75% of the available height \
             (lowest={lowest}, body_top={body_top}, body_h={body_h}, frac={fill_frac:.2}); \
             a sparse void dominates the pane",
        );
    }

    /// e38.39 — the body-filling layout must NOT overflow at the 80×24 floor with a
    /// dense fixture (the section bands clamp to their share, never past `bottom`).
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
