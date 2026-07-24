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

use super::boards::{AgentChip, RepoOption, repo_candidates};

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

    /// The next chip in display order, wrapping `Mine → All`. Drives the
    /// `Tab` chip-cycle binding on the Issues screen.
    #[must_use]
    pub const fn next(self) -> Self {
        match self {
            Self::All => Self::Members,
            Self::Members => Self::Agents,
            Self::Agents => Self::Mine,
            Self::Mine => Self::All,
        }
    }

    /// The previous chip in display order, wrapping `All → Mine`. Drives the
    /// `Shift+Tab` chip-cycle binding on the Issues screen.
    #[must_use]
    pub const fn prev(self) -> Self {
        match self {
            Self::All => Self::Mine,
            Self::Members => Self::All,
            Self::Agents => Self::Members,
            Self::Mine => Self::Agents,
        }
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
/// the staged create-wizard mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IssueListMode {
    /// Normal row navigation (`j`/`k`, `enter`, `c`, …).
    Normal,
    /// `/` filter-input mode: keystrokes append to the [`IssueListState::query`].
    FilterInput,
    /// `c` create-wizard mode (Phase 5): the staged Title → Repo →
    /// `SourceBranch` → `TargetBranch` → Agent overlay
    /// ([`IssueListState::wizard`]) is open and captures every key.
    ///
    /// Enter advances / commits (raising [`IssueListIntent::CreateAndRun`] at
    /// the final Agent stage), Esc cancels the WHOLE wizard from any stage in
    /// one press.
    CreateInput,
    /// `x` delete-confirm mode (63d): a RED confirm overlay is open over the
    /// selected row ([`IssueListState::confirm_delete`]). Enter emits
    /// [`IssueListIntent::DeleteIssue`], Esc cancels in one press; every other key
    /// is captured (so a stray tab-switch char never fires behind the modal).
    ConfirmDelete,
    /// Second-chance confirm raised when a delete was REFUSED because the issue
    /// still has active run(s): an amber overlay offering to cancel the run(s)
    /// and delete ([`IssueListState::confirm_cancel_delete`]). `c`/`C`/Enter emit
    /// [`IssueListIntent::CancelAndDeleteIssue`], Esc backs out; every other key
    /// is captured. Entered from the plugin glue on the daemon's active-tasks
    /// delete rejection, never directly by a key press.
    ConfirmCancelDelete,
}

/// The target of an open `x` delete-confirm overlay (63d): the issue id the
/// [`IssueListIntent::DeleteIssue`] will carry, plus a human label
/// (`<display_id> <title>`) the red overlay renders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingDelete {
    /// The issue the confirmed delete removes.
    pub id: IssueId,
    /// The human label shown in the overlay (`HGR-7 Fix the widget`).
    pub label: String,
}

/// A raw key folded into the open create wizard (Phase 5).
///
/// Mirrors the Boards `BoardsKey` shape: a `Char` is text in an input stage but
/// ignored in a picker; `Up`/`Down` move a picker cursor but are ignored in an
/// input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WizardKey {
    /// A printable character.
    Char(char),
    /// Backspace (delete the last input char).
    Backspace,
    /// Enter (create when the required fields are satisfied, else jump focus to
    /// the missing one).
    Enter,
    /// Escape (cancel the WHOLE wizard).
    Esc,
    /// Cursor up / previous row (also moves the `@` dropdown cursor when open).
    Up,
    /// Cursor down / next row (also moves the `@` dropdown cursor when open).
    Down,
    /// Cursor left — cycle the focused picker row's value backwards (Repo / Agent).
    Left,
    /// Cursor right — cycle the focused picker row's value forwards (Repo / Agent).
    Right,
    /// Tab — move focus to the next row (wraps).
    Tab,
    /// Shift+Tab — move focus to the previous row (wraps).
    BackTab,
}

/// A focusable row in the create-wizard card.
///
/// The five rows render top-to-bottom in this order; ↑↓ / Tab / Shift+Tab move
/// focus between them (wrapping, mirroring the host new-session Configure form).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WizardRow {
    /// The issue title text row (REQUIRED — a blank title blocks create).
    Title,
    /// The multi-line brief text row (OPTIONAL): free-form task description that
    /// becomes `issue.description` and, via `build_prompt`, the `claude -p`
    /// prompt. Enter here inserts a NEWLINE (never fires create); printable chars
    /// append, Backspace edits. A `/name` typed in it reaches the agent verbatim.
    Brief,
    /// The linked-issue text row (OPTIONAL, single-line): a free-form upstream
    /// reference (a URL or `owner/repo#123`) stored as `issue.external_ref` for
    /// traceability and appended to the dispatched brief. Enter here commits like
    /// the other single-line rows; blank is fine.
    Link,
    /// The repo picker row (REQUIRED — `@` fuzzy dropdown or ←/→ cycle; a
    /// repo-less create is impossible).
    Repo,
    /// The SOURCE branch the run branches FROM (text, prefilled `main`).
    Source,
    /// The TARGET branch a future PR lands INTO (text, prefilled `main`).
    Target,
    /// The provider agent picker (←/→ cycle [`AgentChip::ALL`]; always valid).
    Agent,
}

impl WizardRow {
    /// The rows in render / focus-cycle order (Title → Brief → Link → Repo →
    /// Source → Target → Agent).
    pub const ALL: [Self; 7] = [
        Self::Title,
        Self::Brief,
        Self::Link,
        Self::Repo,
        Self::Source,
        Self::Target,
        Self::Agent,
    ];

    /// This row's index in [`Self::ALL`] (the focus cursor position).
    fn index(self) -> usize {
        Self::ALL.iter().position(|r| *r == self).unwrap_or(0)
    }
}

/// One NAMED workspace agent the create wizard's Agent row can target (V3-F3).
///
/// Injected by the glue from the same `hangar/agents_list` snapshot the `a`
/// assign picker uses (agent actors only — members are filtered out). `actor_ref`
/// is the canonical `agent:<id>` form persisted as the new issue's assignee and
/// carried as the run's assignee override, so the dispatch routes to THIS agent
/// (not the alphabetically-first fallback). `label` is the display name.
///
/// When the roster is empty (a workspace with no named agents yet) the Agent row
/// falls back to the [`AgentChip`] provider chips, so a create is never blocked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WizardAgent {
    /// Canonical `agent:<id>` reference (the assignee wire form).
    pub actor_ref: String,
    /// Human-readable agent name shown on the row.
    pub label: String,
}

/// The Issues create wizard (Phase 5): a single centered form showing every field
/// at once — Title / Repo / Source / Target / Agent — with a focused-row cursor.
///
/// Unlike the earlier staged flow, all fields exist from the moment the wizard
/// opens; ↑↓ / Tab move focus, ←/→ cycle the picker rows, typing edits the
/// focused text row, and `@` opens the repo fuzzy dropdown. Enter creates ONLY
/// when the required fields are satisfied (a non-blank title AND a picked repo —
/// the agent always carries a default), otherwise it jumps focus to the missing
/// required row. This preserves the non-negotiable guard: a title-only /
/// repo-less / agent-less issue (the inert `◇ None` card) can never be created.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateWizard {
    /// Which row currently has focus.
    focus: WizardRow,
    /// The title typed so far (REQUIRED — trimmed-blank blocks create).
    title: String,
    /// The multi-line brief typed so far (OPTIONAL): free text with embedded
    /// newlines, carried through to `issue.description`. Blank is allowed.
    brief: String,
    /// The single-line linked-issue reference typed so far (OPTIONAL): a URL or
    /// `owner/repo#123` carried through to `issue.external_ref`. Blank is allowed.
    link: String,
    /// The picked repo's wire ref, or `None` until one is chosen (REQUIRED).
    repo_ref: Option<String>,
    /// The post-`@` fuzzy query filtering the repo dropdown.
    repo_query: String,
    /// `Some(cursor)` while the `@` dropdown is open, `None` while closed.
    repo_dropdown: Option<usize>,
    /// The SOURCE branch text (prefilled `main`; blank = repo default at dispatch).
    source_branch: String,
    /// The TARGET branch text (prefilled `main`; blank = unset).
    target_branch: String,
    /// The highlighted agent chip (index into [`AgentChip::ALL`]).
    agent_cursor: usize,
    /// 0046 sub-issues: when the wizard was opened as an "add sub-issue" (`s` on a
    /// highlighted row, or the context-menu `Add sub-issue`), the parent issue's
    /// wire id, carried into the `hangar/issue_create` payload so the daemon links
    /// the new issue as a child. `None` = a top-level issue (the plain `c` create).
    /// Never user-typed: it is only ever pre-bound from the highlighted row.
    parent_issue_id: Option<String>,
    /// The parent issue's human display (its `HGR-<n>` id + title) shown in the
    /// read-only `Sub-issue of …` banner atop the wizard. Set iff `parent_issue_id`
    /// is set.
    parent_display: Option<String>,
}

impl Default for CreateWizard {
    /// A fresh wizard: focus on Title, no repo picked yet, branches prefilled
    /// `main`, agent defaulted to the first chip (claude).
    fn default() -> Self {
        Self {
            focus: WizardRow::Title,
            title: String::new(),
            brief: String::new(),
            link: String::new(),
            repo_ref: None,
            repo_query: String::new(),
            repo_dropdown: None,
            source_branch: "main".to_string(),
            target_branch: "main".to_string(),
            agent_cursor: 0,
            parent_issue_id: None,
            parent_display: None,
        }
    }
}

impl CreateWizard {
    /// The row that currently has focus.
    #[must_use]
    pub const fn focus(&self) -> WizardRow {
        self.focus
    }

    /// The title typed so far.
    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }

    /// The multi-line brief typed so far (may contain embedded newlines; may be
    /// empty).
    #[must_use]
    pub fn brief(&self) -> &str {
        &self.brief
    }

    /// The single-line linked-issue reference typed so far (may be empty).
    #[must_use]
    pub fn link(&self) -> &str {
        &self.link
    }

    /// The parent issue's wire id when this wizard is an "add sub-issue" (`s` /
    /// context-menu `Add sub-issue`), else `None` (a top-level `c` create).
    #[must_use]
    pub fn parent_issue_id(&self) -> Option<&str> {
        self.parent_issue_id.as_deref()
    }

    /// The parent issue's human display for the `Sub-issue of …` banner, set iff
    /// [`Self::parent_issue_id`] is set.
    #[must_use]
    pub fn parent_display(&self) -> Option<&str> {
        self.parent_display.as_deref()
    }

    /// The picked repo ref, or `None` until one is chosen.
    #[must_use]
    pub fn repo_ref(&self) -> Option<&str> {
        self.repo_ref.as_deref()
    }

    /// The post-`@` fuzzy query.
    #[must_use]
    pub fn repo_query(&self) -> &str {
        &self.repo_query
    }

    /// `Some(cursor)` while the `@` dropdown is open.
    #[must_use]
    pub const fn repo_dropdown(&self) -> Option<usize> {
        self.repo_dropdown
    }

    /// The SOURCE branch text (may be blank).
    #[must_use]
    pub fn source_branch(&self) -> &str {
        &self.source_branch
    }

    /// The TARGET branch text (may be blank).
    #[must_use]
    pub fn target_branch(&self) -> &str {
        &self.target_branch
    }

    /// The highlighted agent chip index.
    #[must_use]
    pub const fn agent_cursor(&self) -> usize {
        self.agent_cursor
    }
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
    /// Whether we are navigating, typing a filter, or in the create wizard.
    mode: IssueListMode,
    /// The staged create wizard (Phase 5), open while `mode` is
    /// [`IssueListMode::CreateInput`]. `None` otherwise.
    wizard: Option<CreateWizard>,
    /// The `@`-dropdown repo roster for the wizard's repo stage, injected by the
    /// glue from the same `hangar/repo_list` snapshot the Boards screen gets
    /// (favorites-first + recency order preserved). `scratch` is NOT in here —
    /// [`repo_candidates`] prepends it always.
    repos: Vec<RepoOption>,
    /// The NAMED workspace-agent roster the wizard's Agent row cycles (V3-F3),
    /// injected by the glue from the same `hangar/agents_list` snapshot the `a`
    /// assign picker uses (agent actors only). Empty on a workspace with no named
    /// agents, in which case the Agent row falls back to the provider chips.
    agents: Vec<WizardAgent>,
    /// A transient status note (create/run dispatch feedback or failure),
    /// rendered on the bottom row and replaced by the next note / cleared when a
    /// new wizard opens. Errors surface HERE, never silently dropped.
    note: Option<String>,
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
    /// The open `x` delete-confirm target (63d), set while `mode` is
    /// [`IssueListMode::ConfirmDelete`]. `None` otherwise.
    confirm_delete: Option<PendingDelete>,
    /// The open "cancel run(s) & delete" target, set while `mode` is
    /// [`IssueListMode::ConfirmCancelDelete`] (armed by the plugin glue when a
    /// delete is refused for active tasks). `None` otherwise.
    confirm_cancel_delete: Option<PendingDelete>,
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
            wizard: None,
            repos: Vec::new(),
            agents: Vec::new(),
            note: None,
            task_issue: HashMap::new(),
            scroll_offsets: [0; COLUMN_COUNT],
            hovered_id: None,
            confirm_delete: None,
            confirm_cancel_delete: None,
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
            IssueListMode::FilterInput
                | IssueListMode::CreateInput
                | IssueListMode::ConfirmDelete
                | IssueListMode::ConfirmCancelDelete
        )
    }

    /// Abort the create wizard (Esc): drop the WHOLE staged overlay — whatever
    /// stage it is on — and return to normal navigation in one press (Phase 5,
    /// never trap the user). A no-op when not creating.
    pub fn abort_create(&mut self) {
        if self.mode == IssueListMode::CreateInput {
            self.mode = IssueListMode::Normal;
            self.wizard = None;
        }
    }

    /// Cancel the `x` delete-confirm overlay (Esc, 63d): drop the RED overlay and
    /// return to normal navigation in one press. A no-op when not confirming.
    pub fn abort_confirm_delete(&mut self) {
        if self.mode == IssueListMode::ConfirmDelete {
            self.mode = IssueListMode::Normal;
            self.confirm_delete = None;
        }
    }

    /// The open delete-confirm target (63d), or `None` when not confirming — the
    /// renderer draws the RED overlay from this.
    #[must_use]
    pub const fn confirm_delete(&self) -> Option<&PendingDelete> {
        self.confirm_delete.as_ref()
    }

    /// Cancel the "cancel run(s) & delete" overlay (Esc): drop it and return to
    /// normal navigation in one press. A no-op when not in that mode.
    pub fn abort_confirm_cancel_delete(&mut self) {
        if self.mode == IssueListMode::ConfirmCancelDelete {
            self.mode = IssueListMode::Normal;
            self.confirm_cancel_delete = None;
        }
    }

    /// The open "cancel run(s) & delete" target, or `None` when not in that mode —
    /// the renderer draws the amber overlay from this.
    #[must_use]
    pub const fn confirm_cancel_delete(&self) -> Option<&PendingDelete> {
        self.confirm_cancel_delete.as_ref()
    }

    /// Arm the "cancel run(s) & delete" overlay for issue `id` — the seam the
    /// plugin glue drives when the daemon refuses a delete because the issue still
    /// has active run(s). Selects the row (so the target is visible) and enters
    /// [`IssueListMode::ConfirmCancelDelete`]; a `c`/`C`/Enter then emits
    /// [`IssueListIntent::CancelAndDeleteIssue`]. A no-op when no cached row
    /// carries that id (the row already vanished).
    pub fn open_confirm_cancel_delete_for(&mut self, id: &str) {
        self.select_by_id(id);
        let Some(row) = self.selected_row().filter(|r| r.id.as_str() == id) else {
            return;
        };
        let label = match &row.display_id {
            Some(display) => format!("{display} {}", row.title),
            None => row.title.clone(),
        };
        let pending = PendingDelete {
            id: row.id.clone(),
            label,
        };
        self.mode = IssueListMode::ConfirmCancelDelete;
        self.confirm_cancel_delete = Some(pending);
        // Supersede any stale dispatch / delete-failure note.
        self.note = None;
    }

    /// Open the `x` RED confirm overlay over the row carrying issue `id` (63l.5):
    /// the seam the context-menu Delete uses so it lands in the SAME confirm flow
    /// as the keyboard `x`. Selects the row first (resetting the chip/query so the
    /// target is visible), then arms the overlay from it. A no-op when no cached
    /// row carries that id (a stale menu). Enter then fires
    /// [`IssueListIntent::DeleteIssue`] exactly as the keyboard path does.
    pub fn open_confirm_delete_for(&mut self, id: &str) {
        self.select_by_id(id);
        // Only arm the overlay when the selection actually resolved to that id —
        // `select_by_id` is a no-op for an unknown id, so guard against confirming
        // a delete on whatever row happened to be selected.
        if self.selected_row().map(|r| r.id.as_str()) != Some(id) {
            return;
        }
        self.arm_confirm_delete();
    }

    /// Arm the delete-confirm overlay over the CURRENT selection (63d): build the
    /// pending target from the selected row and enter [`IssueListMode::ConfirmDelete`].
    /// A no-op when nothing is selected. Shared by the keyboard `x` path and the
    /// context-menu Delete route so both raise an identical overlay.
    fn arm_confirm_delete(&mut self) {
        let Some(row) = self.selected_row() else {
            return;
        };
        // Prefer the human display id (`HGR-7`) with the title; fall back to the
        // raw id when a pre-63l.3 snapshot lacks a display id.
        let label = match &row.display_id {
            Some(display) => format!("{display} {}", row.title),
            None => row.title.clone(),
        };
        let pending = PendingDelete {
            id: row.id.clone(),
            label,
        };
        self.mode = IssueListMode::ConfirmDelete;
        self.confirm_delete = Some(pending);
        // A fresh confirm supersedes any stale dispatch note.
        self.note = None;
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

    /// The open create wizard (Phase 5), or `None` when not creating.
    #[must_use]
    pub const fn wizard(&self) -> Option<&CreateWizard> {
        self.wizard.as_ref()
    }

    /// Inject the `@`-dropdown repo roster for the wizard's repo stage (Phase 5),
    /// from the glue's cached `hangar/repo_list` (the same snapshot the Boards
    /// card-create dropdown draws from).
    pub fn set_repos(&mut self, repos: Vec<RepoOption>) {
        self.repos = repos;
    }

    /// Inject the NAMED workspace-agent roster the wizard's Agent row targets
    /// (V3-F3), from the glue's cached `hangar/agents_list` snapshot (agent actors
    /// only). Empty leaves the Agent row on the provider-chip fallback.
    pub fn set_agents(&mut self, agents: Vec<WizardAgent>) {
        self.agents = agents;
    }

    /// The NAMED workspace-agent roster the wizard's Agent row cycles (V3-F3).
    #[must_use]
    pub fn agents(&self) -> &[WizardAgent] {
        &self.agents
    }

    /// The transient status note (dispatch feedback / failure), if any.
    #[must_use]
    pub fn note(&self) -> Option<&str> {
        self.note.as_deref()
    }

    /// Surface a transient status note on the bottom row (Phase 5): the glue
    /// reports every create/update/run reply — success AND failure — through
    /// this, so a dispatch error is never silent.
    pub fn set_note(&mut self, note: impl Into<String>) {
        self.note = Some(note.into());
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
        self.visible_rows().filter(move |r| IssueColumn::for_state(&r.state) == column)
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
                            linked: r.external_ref.as_deref().is_some_and(|e| !e.trim().is_empty()),
                            // 0046: the sub-issue roll-up, so a PARENT card shows a
                            // `⊟ done/total` badge that flips to gold `1/1` when its
                            // last child completes. `None` for a childless issue.
                            subtasks: (r.child_total > 0).then_some((r.child_done, r.child_total)),
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
            if let Some(card_idx) = self.rows_in_column(column).position(|r| r.id.as_str() == id) {
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
    /// A structured key for the open create wizard (Phase 5): unlike [`Self::Key`]
    /// it carries Up/Down/Esc, which the picker stages (repo dropdown, agent
    /// chips) need. Ignored when no wizard is open.
    Wizard(WizardKey),
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
    /// Commit the create wizard (Phase 5): create the issue AND dispatch it.
    /// Raised ONLY by Enter on the wizard's final Agent stage — there is no path
    /// to this intent without an agent, so a title-only inert issue (assignee
    /// `◇ None`, never runs) cannot be created from this screen. The plugin glue
    /// lifts it into `hangar/issue_create` → `hangar/issue_update` (persist repo /
    /// agent / branches) → `hangar/issue_run`.
    CreateAndRun {
        /// The non-blank title typed in stage 1.
        title: String,
        /// The multi-line brief (OPTIONAL): free text carried through to
        /// `issue.description` and the `claude -p` prompt. `None` when blank.
        brief: Option<String>,
        /// The linked-issue reference (OPTIONAL): a URL or `owner/repo#123` carried
        /// through to `issue.external_ref` for traceability. `None` when blank.
        external_ref: Option<String>,
        /// The repo picked in stage 2 (REQUIRED — an absolute path, `scratch`, or
        /// a remote indicator the daemon clones).
        repo_ref: String,
        /// The source branch the run branches FROM; `None` = the repo default.
        source_branch: Option<String>,
        /// The target branch a future PR lands INTO; `None` = unset.
        target_branch: Option<String>,
        /// The provider agent wire token (`claude` / `codex` / `copilot`) when the
        /// Agent row fell back to the provider chips (no named agents in the
        /// workspace); `None` when a NAMED agent was targeted instead (its own
        /// provider drives the run — see [`Self::CreateAndRun::assignee`]).
        agent: Option<String>,
        /// The NAMED workspace agent targeted by the Agent row, as its canonical
        /// `agent:<id>` ref (V3-F3): persisted as the new issue's assignee AND
        /// carried as the run's assignee override so the dispatch routes to it.
        /// `None` when the roster was empty and a provider chip was chosen instead.
        assignee: Option<String>,
        /// 0046 sub-issues: the parent issue's wire id when the wizard was opened
        /// as an "add sub-issue" (`s` / context-menu `Add sub-issue`), threaded into
        /// `hangar/issue_create` so the daemon links the new issue as a child.
        /// `None` for a top-level `c` create.
        parent_issue_id: Option<String>,
    },
    /// 0046 sub-issues: mark the highlighted issue **Done** from the keyboard
    /// (`d`), the same lifecycle move the context-menu `Move to ▸ Done` raises,
    /// so it routes through `hangar/issue_update{state:"done"}` on the daemon and
    /// fires the child-done → parent cascade. Carries the target issue id.
    MarkDone(IssueId),
    /// Delete the confirmed issue (63d): raised ONLY by Enter on the `x` RED
    /// confirm overlay. The plugin glue lifts it into `hangar/issue_delete`; the
    /// daemon's `IssueDeleted` push then drops the row from the list.
    DeleteIssue(IssueId),
    /// Cancel the issue's active run(s) THEN delete it: raised by confirming the
    /// "cancel run(s) & delete" overlay (armed after a delete was refused for
    /// active tasks). The plugin glue fires `hangar/issue_cancel_active` and, on
    /// its success reply, retries `hangar/issue_delete` (cancel commits before the
    /// delete).
    CancelAndDeleteIssue(IssueId),
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
        IssueListEvent::Wizard(k) => reduce_wizard_key(state, k),
        IssueListEvent::SetFilter(chip) => set_filter(state, chip),
        IssueListEvent::Event(event) => fold_event(state, event),
    }
}

/// Handle a printable-key press, dispatching on the active input mode.
fn reduce_key(state: &IssueListState, c: char) -> IssueListReduction {
    match state.mode {
        IssueListMode::FilterInput => reduce_filter_input_key(state, c),
        // A plain char while the wizard is open folds in as a wizard key, so the
        // legacy `Key(char)` path (Enter as '\n', Backspace as '\u{8}') keeps
        // working alongside the structured [`IssueListEvent::Wizard`] events.
        IssueListMode::CreateInput => reduce_wizard_key(state, wizard_key_from_char(c)),
        IssueListMode::ConfirmDelete => reduce_confirm_delete_key(state, c),
        IssueListMode::ConfirmCancelDelete => reduce_confirm_cancel_delete_key(state, c),
        IssueListMode::Normal => reduce_normal_key(state, c),
    }
}

/// Map the legacy reducer char vocabulary onto a [`WizardKey`].
const fn wizard_key_from_char(c: char) -> WizardKey {
    match c {
        '\n' | '\r' => WizardKey::Enter,
        '\u{8}' | '\u{7f}' => WizardKey::Backspace,
        other => WizardKey::Char(other),
    }
}

/// Normal-mode key handling: navigation + intent-raising keys.
fn reduce_normal_key(state: &IssueListState, c: char) -> IssueListReduction {
    match c {
        'j' => move_selection_down(state),
        'k' => move_selection_up(state),
        '/' => enter_filter_mode(state),
        'c' => enter_create_mode(state),
        // 0046: `s` opens the create wizard as an "add sub-issue" with the
        // highlighted row pre-bound as the parent (never user-typed). Lowercase so
        // it falls through to this reducer: the host reserves UPPERCASE `S` as the
        // global Squads tab-switch (`routing_event`).
        's' => enter_create_subissue_mode(state),
        // 0046: `d` marks the highlighted issue Done through the daemon
        // (`hangar/issue_update{state:"done"}`), firing the child-done cascade
        // when the row is a sub-issue. No-op when nothing is selected. Lowercase:
        // UPPERCASE `D` is the host's global Daemon-health tab-switch.
        'd' => state.selected_row().map_or_else(
            || unchanged(state),
            |row| with_intent(state.clone(), IssueListIntent::MarkDone(row.id.clone())),
        ),
        'x' => enter_confirm_delete(state),
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

/// Open the create wizard (`c`, Phase 5) as a fresh single-form card focused on
/// the Title row. No intent yet — Enter commits only once the required fields are
/// satisfied.
fn enter_create_mode(state: &IssueListState) -> IssueListReduction {
    let mut next = state.clone();
    next.mode = IssueListMode::CreateInput;
    next.wizard = Some(CreateWizard::default());
    // A fresh wizard supersedes any stale dispatch note.
    next.note = None;
    no_intent(next)
}

/// Open the create wizard as an "add sub-issue" (`s`, 0046): identical to the
/// plain `c` create except the highlighted row is pre-bound as the parent (its
/// wire id + a `HGR-<n> title` display for the read-only banner). No-op when
/// nothing is selected (a sub-issue always needs a parent row to hang from).
fn enter_create_subissue_mode(state: &IssueListState) -> IssueListReduction {
    let Some(row) = state.selected_row() else {
        return unchanged(state);
    };
    // Prefer the human display id (`HGR-7`) with the title; fall back to the raw
    // id when a pre-63l.3 snapshot lacks a display id (mirrors `arm_confirm_delete`).
    let display = match &row.display_id {
        Some(d) => format!("{d} {}", row.title),
        None => row.title.clone(),
    };
    let mut wizard = CreateWizard::default();
    wizard.parent_issue_id = Some(row.id.as_str().to_string());
    wizard.parent_display = Some(display);
    let mut next = state.clone();
    next.mode = IssueListMode::CreateInput;
    next.wizard = Some(wizard);
    next.note = None;
    no_intent(next)
}

/// Swap the open wizard for `wizard`, emitting no intent (a stage edit /
/// transition).
fn set_wizard(state: &IssueListState, wizard: CreateWizard) -> IssueListReduction {
    let mut next = state.clone();
    next.wizard = Some(wizard);
    no_intent(next)
}

/// Cancel the WHOLE wizard (Esc from any stage, Phase 5): back to normal
/// navigation in one press, everything typed is dropped.
fn cancel_wizard(state: &IssueListState) -> IssueListReduction {
    let mut next = state.clone();
    next.mode = IssueListMode::Normal;
    next.wizard = None;
    no_intent(next)
}

/// Open the `x` delete-confirm overlay over the selected row (63d). No intent yet
/// — the RED overlay collects the Enter/Esc decision first. A no-op when the list
/// has no rows (nothing to delete), so `x` on an empty board never traps the user.
fn enter_confirm_delete(state: &IssueListState) -> IssueListReduction {
    if state.selected_row().is_none() {
        return unchanged(state);
    }
    let mut next = state.clone();
    next.arm_confirm_delete();
    no_intent(next)
}

/// Delete-confirm key handling (63d): Enter emits [`IssueListIntent::DeleteIssue`]
/// for the pending target, Esc cancels; every other key is captured (the overlay
/// is modal). All paths that leave confirm mode reset back to normal navigation.
fn reduce_confirm_delete_key(state: &IssueListState, c: char) -> IssueListReduction {
    match c {
        // Enter (delivered as '\n' / '\r') confirms the delete.
        '\n' | '\r' => {
            let Some(pending) = state.confirm_delete.clone() else {
                // Defensive: no target — just drop back to navigation.
                return cancel_confirm_delete(state);
            };
            let mut next = state.clone();
            next.mode = IssueListMode::Normal;
            next.confirm_delete = None;
            with_intent(next, IssueListIntent::DeleteIssue(pending.id))
        }
        // Esc (delivered as the ESC char to the pure reducer) cancels.
        '\u{1b}' => cancel_confirm_delete(state),
        // Any other key is swallowed — the modal stays open until Enter / Esc.
        _ => unchanged(state),
    }
}

/// Cancel the delete-confirm overlay (Esc): back to normal navigation, target
/// dropped, no intent.
fn cancel_confirm_delete(state: &IssueListState) -> IssueListReduction {
    let mut next = state.clone();
    next.mode = IssueListMode::Normal;
    next.confirm_delete = None;
    no_intent(next)
}

/// "Cancel run(s) & delete" key handling: `c` / `C` / Enter emit
/// [`IssueListIntent::CancelAndDeleteIssue`] for the pending target, Esc backs
/// out; every other key is captured (the overlay is modal). All exits reset back
/// to normal navigation.
fn reduce_confirm_cancel_delete_key(state: &IssueListState, c: char) -> IssueListReduction {
    match c {
        // Confirm: `c`/`C` (matching the "[C] cancel & delete" hint) or Enter.
        'c' | 'C' | '\n' | '\r' => {
            let Some(pending) = state.confirm_cancel_delete.clone() else {
                return cancel_confirm_cancel_delete(state);
            };
            let mut next = state.clone();
            next.mode = IssueListMode::Normal;
            next.confirm_cancel_delete = None;
            with_intent(next, IssueListIntent::CancelAndDeleteIssue(pending.id))
        }
        // Esc backs out, leaving the run(s) untouched.
        '\u{1b}' => cancel_confirm_cancel_delete(state),
        // Any other key is swallowed — the modal stays open until confirm / Esc.
        _ => unchanged(state),
    }
}

/// Back out of the "cancel run(s) & delete" overlay (Esc): normal navigation,
/// target dropped, no intent.
fn cancel_confirm_cancel_delete(state: &IssueListState) -> IssueListReduction {
    let mut next = state.clone();
    next.mode = IssueListMode::Normal;
    next.confirm_cancel_delete = None;
    no_intent(next)
}

/// Fold one [`WizardKey`] into the open single-form create wizard (Phase 5). Esc
/// cancels the whole overlay; while the `@` dropdown is open it captures the nav
/// keys; otherwise ↑↓/Tab move focus, ←/→ cycle the focused picker row, typing
/// edits the focused text row, and Enter creates (or jumps to the missing
/// required row). A wizard key with no wizard open is a no-op.
fn reduce_wizard_key(state: &IssueListState, key: WizardKey) -> IssueListReduction {
    let Some(mut wizard) = state.wizard.clone() else {
        return unchanged(state);
    };
    if key == WizardKey::Esc {
        return cancel_wizard(state);
    }
    // The `@` dropdown is modal over the repo row while open: it owns the nav and
    // edit keys so the user can filter + pick without the focus cursor moving off.
    if wizard.repo_dropdown.is_some() {
        return wizard_dropdown_key(state, wizard, key);
    }
    match key {
        // Enter on the Brief inserts a NEWLINE (multi-line editing) and must NOT
        // fire create; every other row's Enter is the existing commit point.
        // Guard the empty buffer: Enter with nothing typed is a no-op, never a
        // seeded leading `\n`. That newline would otherwise reach
        // `issue.description` byte-verbatim (see `wizard_try_create`, which sends
        // the brief EXACTLY as typed and only trims to detect all-blank),
        // displacing any leading `/name` skill line off position 0 and breaking
        // headless dispatch under `claude -p`.
        WizardKey::Enter if wizard.focus == WizardRow::Brief => {
            if !wizard.brief.is_empty() {
                wizard.brief.push('\n');
            }
            set_wizard(state, wizard)
        }
        WizardKey::Enter => wizard_try_create(state, wizard),
        WizardKey::Tab | WizardKey::Down => wizard_move_focus(state, wizard, true),
        WizardKey::BackTab | WizardKey::Up => wizard_move_focus(state, wizard, false),
        WizardKey::Left => wizard_cycle_value(state, wizard, false),
        WizardKey::Right => wizard_cycle_value(state, wizard, true),
        WizardKey::Char(c) => wizard_type_char(state, wizard, c),
        WizardKey::Backspace => wizard_backspace(state, wizard),
        // Esc handled above.
        WizardKey::Esc => unchanged(state),
    }
}

/// Step `cur` one position `forward` (or backward) in a ring of `len`, wrapping.
/// `len` is assumed non-zero by the callers (the row / chip / candidate lists are
/// always non-empty).
const fn ring_step(cur: usize, len: usize, forward: bool) -> usize {
    if forward {
        (cur + 1) % len
    } else if cur == 0 {
        len - 1
    } else {
        cur - 1
    }
}

/// Move the focus cursor one row `forward` (or backward), wrapping (mirrors the
/// host new-session Configure `cycle_focus`). Never leaves the form.
fn wizard_move_focus(
    state: &IssueListState,
    mut wizard: CreateWizard,
    forward: bool,
) -> IssueListReduction {
    let next = ring_step(wizard.focus.index(), WizardRow::ALL.len(), forward);
    wizard.focus = WizardRow::ALL[next];
    set_wizard(state, wizard)
}

/// Cycle the focused picker row's value one step `forward` (or backward) —
/// Repo / Agent only. A no-op on the text rows: ←/→ never edit text.
fn wizard_cycle_value(
    state: &IssueListState,
    mut wizard: CreateWizard,
    forward: bool,
) -> IssueListReduction {
    match wizard.focus {
        WizardRow::Repo => {
            // Cycle the (unfiltered) roster — scratch always first. From "none
            // picked", → lands on scratch and ← on the last candidate, so ←/→ is
            // a full alternative to the `@` dropdown for picking a repo.
            let candidates = repo_candidates(&state.repos, "");
            let Some(last) = candidates.len().checked_sub(1) else {
                return set_wizard(state, wizard);
            };
            let current = wizard
                .repo_ref
                .as_deref()
                .and_then(|r| candidates.iter().position(|c| c.repo_ref == r));
            let next = match current {
                Some(i) => ring_step(i, candidates.len(), forward),
                None if forward => 0,
                None => last,
            };
            wizard.repo_ref = Some(candidates[next].repo_ref.clone());
        }
        WizardRow::Agent => {
            // Cycle the NAMED workspace-agent roster when the glue injected one
            // (V3-F3); otherwise cycle the provider chips (the fallback for a
            // workspace with no named agents). `agent_cursor` indexes whichever is
            // active — the fixed roster length keeps the cursor in range.
            let n = if state.agents.is_empty() {
                AgentChip::ALL.len()
            } else {
                state.agents.len()
            };
            wizard.agent_cursor = ring_step(wizard.agent_cursor, n, forward);
        }
        WizardRow::Title
        | WizardRow::Brief
        | WizardRow::Link
        | WizardRow::Source
        | WizardRow::Target => {}
    }
    set_wizard(state, wizard)
}

/// Type one char into the focused row: append to the focused text row (Title /
/// Brief / Link / Source / Target), or open the `@` repo dropdown on the Repo row.
/// Any other key on a picker row is ignored.
fn wizard_type_char(
    state: &IssueListState,
    mut wizard: CreateWizard,
    c: char,
) -> IssueListReduction {
    match wizard.focus {
        WizardRow::Title => wizard.title.push(c),
        WizardRow::Brief => wizard.brief.push(c),
        WizardRow::Link => wizard.link.push(c),
        WizardRow::Source => wizard.source_branch.push(c),
        WizardRow::Target => wizard.target_branch.push(c),
        WizardRow::Repo => {
            if c == '@' {
                // Open the fuzzy dropdown fresh at scratch (cursor 0).
                wizard.repo_query = String::new();
                wizard.repo_dropdown = Some(0);
            }
            // Any non-`@` char on the closed repo row is ignored — the picker is
            // driven by `@` / ←→, not free text.
        }
        WizardRow::Agent => {}
    }
    set_wizard(state, wizard)
}

/// Delete the last char of the focused text row (Title / Source / Target). A
/// no-op on the picker rows.
fn wizard_backspace(state: &IssueListState, mut wizard: CreateWizard) -> IssueListReduction {
    match wizard.focus {
        WizardRow::Title => {
            wizard.title.pop();
        }
        WizardRow::Brief => {
            wizard.brief.pop();
        }
        WizardRow::Link => {
            wizard.link.pop();
        }
        WizardRow::Source => {
            wizard.source_branch.pop();
        }
        WizardRow::Target => {
            wizard.target_branch.pop();
        }
        WizardRow::Repo | WizardRow::Agent => {}
    }
    set_wizard(state, wizard)
}

/// Handle a key while the `@` repo dropdown is open: chars filter, Backspace
/// deletes, ↑↓/←→ move the cursor, Enter picks the highlighted candidate (closing
/// the dropdown), Tab/Shift+Tab close the dropdown and move focus. Never picks
/// repo-less — Enter always resolves to a real candidate (scratch is always
/// present).
fn wizard_dropdown_key(
    state: &IssueListState,
    mut wizard: CreateWizard,
    key: WizardKey,
) -> IssueListReduction {
    let cursor = wizard.repo_dropdown.unwrap_or(0);
    match key {
        WizardKey::Char(c) => {
            wizard.repo_query.push(c);
            wizard.repo_dropdown = Some(0);
        }
        WizardKey::Backspace => {
            wizard.repo_query.pop();
            wizard.repo_dropdown = Some(0);
        }
        WizardKey::Up | WizardKey::Left => {
            wizard.repo_dropdown = Some(cursor.saturating_sub(1));
        }
        WizardKey::Down | WizardKey::Right => {
            let n = repo_candidates(&state.repos, &wizard.repo_query).len();
            wizard.repo_dropdown = Some((cursor + 1).min(n.saturating_sub(1)));
        }
        WizardKey::Enter => {
            let candidates = repo_candidates(&state.repos, &wizard.repo_query);
            if let Some(picked) = candidates.get(cursor).or_else(|| candidates.first()) {
                wizard.repo_ref = Some(picked.repo_ref.clone());
            }
            wizard.repo_dropdown = None;
        }
        WizardKey::Tab | WizardKey::BackTab => {
            // Commit the highlighted candidate before leaving so tabbing away
            // does not silently discard the pick.
            let candidates = repo_candidates(&state.repos, &wizard.repo_query);
            if let Some(picked) = candidates.get(cursor).or_else(|| candidates.first()) {
                wizard.repo_ref = Some(picked.repo_ref.clone());
            }
            wizard.repo_dropdown = None;
            return wizard_move_focus(state, wizard, matches!(key, WizardKey::Tab));
        }
        // Esc is handled by the caller (cancels the whole wizard).
        WizardKey::Esc => {}
    }
    set_wizard(state, wizard)
}

/// Enter with the dropdown closed: create when the REQUIRED fields are satisfied
/// (a non-blank title AND a picked repo — the agent always carries a default),
/// raising the one-and-only [`IssueListIntent::CreateAndRun`]. When a required
/// field is missing, DO NOT create — jump focus to it (and open the `@` dropdown
/// for the repo) so the user is guided, never silently blocked. This is the whole
/// guard: an agent-less / repo-less / title-only issue is impossible to create.
fn wizard_try_create(state: &IssueListState, mut wizard: CreateWizard) -> IssueListReduction {
    if wizard.title.trim().is_empty() {
        wizard.focus = WizardRow::Title;
        return set_wizard(state, wizard);
    }
    let Some(repo_ref) = wizard.repo_ref.clone() else {
        // Repo REQUIRED: guide the user to it with the dropdown open at scratch.
        wizard.focus = WizardRow::Repo;
        wizard.repo_query = String::new();
        wizard.repo_dropdown = Some(0);
        return set_wizard(state, wizard);
    };
    let mut next = state.clone();
    next.mode = IssueListMode::Normal;
    next.wizard = None;
    // Blank branch inputs mean "unset" — the daemon resolves the default. Branch
    // refs are trimmed (whitespace in a ref is never meaningful).
    let opt = |s: &str| {
        let t = s.trim();
        if t.is_empty() {
            None
        } else {
            Some(t.to_string())
        }
    };
    // The Brief is the opposite: it reaches `issue.description` VERBATIM (it may
    // carry leading `/name` skill lines and embedded newlines that `claude -p`
    // executes at dispatch). Only a wholly-blank brief collapses to `None`; a
    // present brief is sent EXACTLY as typed — never trimmed, escaped, or
    // normalised.
    let brief = if wizard.brief.trim().is_empty() {
        None
    } else {
        Some(wizard.brief.clone())
    };
    // The Agent row resolves to EITHER a named workspace agent (its `agent:<id>`
    // ref becomes the issue's assignee + the run's assignee override, so dispatch
    // routes to it) OR — when the roster is empty — a provider chip (today's
    // deterministic fallback: no assignee, so the daemon resolves the workspace's
    // first agent under the chosen provider). Exactly one of the two is set.
    let (agent, assignee) = match state.agents.get(wizard.agent_cursor) {
        Some(named) => (None, Some(named.actor_ref.clone())),
        None => (
            Some(AgentChip::at(wizard.agent_cursor).wire().to_string()),
            None,
        ),
    };
    with_intent(
        next,
        IssueListIntent::CreateAndRun {
            title: wizard.title.trim().to_string(),
            brief,
            // The linked-issue ref is trimmed (surrounding whitespace in a URL /
            // `owner/repo#123` is never meaningful); blank collapses to `None`.
            external_ref: opt(&wizard.link),
            repo_ref,
            source_branch: opt(&wizard.source_branch),
            target_branch: opt(&wizard.target_branch),
            agent,
            assignee,
            // 0046: carry the pre-bound parent through so an `s`-opened wizard
            // creates a CHILD (the daemon links it); a plain `c` create is `None`.
            parent_issue_id: wizard.parent_issue_id.clone(),
        },
    )
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
            next.task_issue.insert(task_id.as_str().to_string(), issue_id);
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
    let active_chip = FilterChip::all().iter().position(|c| *c == state.filter).unwrap_or(0);
    crate::widgets::filter_chip::render_chip_bar(buf, top, area_w, &chip_labels, active_chip);
    // Working-agents avatar stack, right-aligned on the same chip row.
    crate::widgets::working_chip::render_working_chip(buf, top, area_w, working_count);

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

    // Phase 5: overlays paint AFTER the board so they win at every shared cell
    // (last write wins). The create wizard is a centered bordered card over the
    // whole body region; the `x` delete-confirm is the RED bottom-strip overlay;
    // otherwise a transient dispatch note (launch feedback / failure) paints on
    // the bottom row.
    if let Some(wizard) = state.wizard() {
        render_wizard(
            buf,
            area_w,
            col_top,
            bottom,
            wizard,
            &state.repos,
            &state.agents,
        );
    } else if let Some(pending) = state.confirm_delete() {
        // 63d: the RED delete-confirm overlay on the two bottom rows.
        render_confirm_delete(
            buf,
            area_w,
            bottom.saturating_sub(2),
            bottom.saturating_sub(1),
            pending,
        );
    } else if let Some(pending) = state.confirm_cancel_delete() {
        // The amber "cancel run(s) & delete" overlay on the two bottom rows.
        render_confirm_cancel_delete(
            buf,
            area_w,
            bottom.saturating_sub(2),
            bottom.saturating_sub(1),
            pending,
        );
    } else if let Some(note) = state.note() {
        put_str(
            buf,
            0,
            bottom.saturating_sub(1),
            note,
            CREATE_ACCENT,
            area_w,
        );
    }
}

/// Accent for the create-issue input bar (a calm emerald, distinct from the
/// gold headers + green selection so the create prompt reads as its own mode).
const CREATE_ACCENT: Color = Color::rgb(120, 200, 160);

/// The clay-red used across the plugin for destructive / offline states (the
/// style guide's `OFFLINE_RED`); the `x` delete-confirm overlay paints in it so a
/// destructive action reads as dangerous at a glance (63d).
const OFFLINE_RED: Color = Color::rgb(220, 120, 100);

/// Render the RED delete-confirm overlay on the two bottom rows (63d): a prompt
/// naming the target + the irreversibility, then the key legend. Char-safe via
/// [`put_str`]. Enter deletes, Esc cancels — both wired in the reducer.
fn render_confirm_delete(
    buf: &mut WireBuffer,
    area_w: u16,
    row: u16,
    value_row: u16,
    pending: &PendingDelete,
) {
    put_str(
        buf,
        0,
        row,
        &format!("Delete issue {}? This removes its history.", pending.label),
        OFFLINE_RED,
        area_w,
    );
    put_str(
        buf,
        0,
        value_row,
        "Enter=delete  Esc=cancel",
        OFFLINE_RED,
        area_w,
    );
}

/// Amber used for the "cancel run(s) & delete" overlay — a caution accent
/// distinct from the destructive clay-red, so the second-chance prompt reads as a
/// recoverable choice rather than the point of no return.
const CAUTION_AMBER: Color = Color::rgb(230, 180, 90);

/// Render the amber "cancel run(s) & delete" overlay on the two bottom rows: the
/// prompt explaining the issue still has active run(s), then the key legend.
/// Char-safe via [`put_str`]. `c`/`C`/Enter cancels-then-deletes, Esc backs out —
/// all wired in the reducer.
fn render_confirm_cancel_delete(
    buf: &mut WireBuffer,
    area_w: u16,
    row: u16,
    value_row: u16,
    pending: &PendingDelete,
) {
    put_str(
        buf,
        0,
        row,
        &format!("{} has active run(s) blocking delete.", pending.label),
        CAUTION_AMBER,
        area_w,
    );
    put_str(
        buf,
        0,
        value_row,
        "c=cancel run(s) & delete  Esc=keep",
        CAUTION_AMBER,
        area_w,
    );
}

/// Gold — the create-wizard card's border + title (the style guide's CTA gold).
const GOLD: Color = Color::rgb(255, 215, 0);
/// Selection green for the focused row's value + the highlighted dropdown pick.
const SELECTION_GREEN: Color = Color::rgb(100, 200, 100);
/// Soft-white for the unfocused rows' values (style guide body text).
const SOFT_WHITE: Color = Color::rgb(220, 220, 230);
/// Cornflower blue — the local (📁) repo marker, mirroring the host new-session
/// picker's `RowKind::Local` colour so a path-backed repo reads the same in both.
const CORNFLOWER_BLUE: Color = Color::rgb(100, 149, 237);
/// Dim backdrop behind the card so it reads as a floating surface over the board.
const CARD_BACKDROP: Color = Color::rgb(20, 20, 28);

/// The card title inlaid on its top border.
const WIZARD_TITLE: &str = "✦ New task";
/// The in-card footer hint naming the nav keys.
const WIZARD_HINT: &str = "↑↓ row   ←→ value   Enter create   Esc cancel";
/// The card's FIXED-row height: top border + the 6 single-line field rows
/// (Title / Link / Repo / Source / Target / Agent) + spacer + hint + bottom
/// border. The multi-line Brief row adds `brief_rows` on top of this, and the `@`
/// repo picker adds its visible dropdown rows while open (see [`render_wizard`]);
/// the card is exactly this tall only in the degenerate all-empty
/// single-line-brief case.
const WIZARD_CARD_H: u16 = 10;
/// The most wrapped Brief lines the card shows at once; a longer brief
/// scroll-follows the newest text within this window. Bounds card growth so the
/// Brief never blows the viewport.
const BRIEF_WINDOW: u16 = 5;
/// The card's preferred width (clamped to the viewport minus insets).
const WIZARD_CARD_W: u16 = 54;
/// The most repo candidates the open `@` dropdown shows at once; a longer roster
/// scroll-follows the cursor within this window and flags the overflow with a
/// `… N more` affordance. Bounds how far the card can grow so it never blows the
/// viewport.
const REPO_DROPDOWN_WINDOW: u16 = 6;

/// Render the create wizard as a single centered bordered card over the body
/// region `[top, bottom]` (Phase 5): a gold rounded frame titled `✦ New task`,
/// every field (Title / Repo / Source / Target / Agent) on its own row with the
/// focused row's value in green, and a footer hint. The `@` repo dropdown, when
/// open, expands inline on the Repo row (scratch first). Degenerate viewports
/// fall back to the title + hint so the card never panics or renders empty.
/// Char-safe via [`put_card_str`].
fn render_wizard(
    buf: &mut WireBuffer,
    area_w: u16,
    top: u16,
    bottom: u16,
    wizard: &CreateWizard,
    repos: &[RepoOption],
    agents: &[WizardAgent],
) {
    let inset: u16 = 2;
    let max_w = area_w.saturating_sub(inset * 2);
    let card_w = WIZARD_CARD_W.min(max_w);
    let region_h = bottom.saturating_sub(top).saturating_add(1);

    // Degenerate viewport: paint at least the title + hint (never panic / empty).
    // The Brief always adds at least one line, so the true minimum is one row
    // taller than the fixed-row height.
    if card_w < 24 || region_h < WIZARD_CARD_H + 1 {
        put_card_str(buf, 0, top, WIZARD_TITLE, GOLD, area_w, false);
        put_card_str(
            buf,
            0,
            top.saturating_add(1),
            WIZARD_HINT,
            GOLD,
            area_w,
            false,
        );
        return;
    }

    // The Brief row grows the card by its visible wrapped-line count (always >= 1).
    // The value column starts 12 in from the card's left and the Brief marker eats
    // a further 2, so the wrap width is `card_w - 15` (matched inside
    // [`render_brief`] so the counted rows equal the painted lines).
    let brief_value_w = card_w.saturating_sub(15);
    let brief_rows = brief_visible_rows(wizard, brief_value_w, region_h);
    // While the `@` dropdown is open the card GROWS by the number of candidate
    // rows it shows (filter line reuses the Repo row itself). The window is capped
    // at [`REPO_DROPDOWN_WINDOW`] and further shrunk to whatever the viewport can
    // hold AFTER the Brief has taken its rows, so the card never spills past
    // `region_h` — compact again on close.
    let dropdown_rows =
        repo_dropdown_visible_rows(wizard, repos, region_h.saturating_sub(brief_rows));
    // 0046: an "add sub-issue" wizard (`s`) shows a read-only `Sub-issue of …`
    // banner on its own row above the fields. It grows the card by 1 only when the
    // viewport still has room after the Brief + dropdown have taken theirs, so a
    // tight viewport degrades gracefully (banner dropped, fields intact). A plain
    // `c` create has no parent, so the card is byte-identical to before.
    let banner_rows = u16::from(
        wizard.parent_display().is_some() && region_h > WIZARD_CARD_H + brief_rows + dropdown_rows,
    );
    let card_h = WIZARD_CARD_H + brief_rows + dropdown_rows + banner_rows;

    let left = (area_w.saturating_sub(card_w)) / 2;
    let right = left + card_w; // exclusive
    let card_top = top + (region_h - card_h) / 2;
    let card_bottom = card_top + card_h - 1;

    // Backdrop fill so the card fully occludes the board beneath it.
    for y in card_top..=card_bottom {
        for x in left..right {
            let mut cell = Cell::new(" ");
            cell.bg = Some(CARD_BACKDROP);
            buf.push(Coord::new(x, y), cell);
        }
    }
    draw_card_frame(buf, left, right, card_top, card_bottom);
    // Title inlaid on the top edge: "┌─ ✦ New task ─…".
    put_card_str(buf, left + 3, card_top, WIZARD_TITLE, GOLD, right, true);

    // Field rows: a left label column then the value. Each field is one row EXCEPT
    // the Repo row while its dropdown is open, which spans `1 + dropdown_rows`
    // (filter line + candidate window), pushing the rows below it down — so `y`
    // runs rather than being a fixed `card_top + 1 + i`.
    let label_x = left + 2;
    let value_x = left + 12;
    let text_right = right.saturating_sub(1);
    let mut y = card_top + 1;
    // 0046: the read-only `Sub-issue of <HGR-n title>` banner (only when the wizard
    // was opened via `s` with a pre-bound parent). Cornflower so it reads as chrome,
    // not an editable field; it precedes the Title row and never takes focus.
    if banner_rows > 0 {
        if let Some(parent) = wizard.parent_display() {
            put_card_str(
                buf,
                label_x,
                y,
                &format!("Sub-issue of {parent}"),
                CORNFLOWER_BLUE,
                text_right,
                true,
            );
        }
        y = y.saturating_add(banner_rows);
    }
    for field in WizardRow::ALL {
        let focused = wizard.focus() == field;
        let label = wizard_row_label(field);
        let label_colour = if focused { GOLD } else { MUTED_GRAY };
        put_card_str(buf, label_x, y, label, label_colour, value_x, true);
        if field == WizardRow::Brief {
            render_brief(buf, value_x, y, text_right, wizard, brief_rows);
            y = y.saturating_add(brief_rows);
            continue;
        }
        if field == WizardRow::Repo && wizard.repo_dropdown().is_some() {
            render_repo_dropdown(buf, value_x, y, text_right, wizard, repos, dropdown_rows);
            y = y.saturating_add(1 + dropdown_rows);
            continue;
        }
        render_wizard_field(buf, value_x, y, text_right, field, wizard, repos, agents);
        y = y.saturating_add(1);
    }

    // Footer hint, one blank spacer row above it (left as backdrop).
    let hint_y = card_bottom.saturating_sub(1);
    put_card_str(
        buf,
        label_x,
        hint_y,
        WIZARD_HINT,
        MUTED_GRAY,
        text_right,
        true,
    );
}

/// How many candidate rows the open `@` dropdown paints (0 when closed): the
/// candidate count capped at [`REPO_DROPDOWN_WINDOW`], then shrunk so the grown
/// card still fits `region_h` (the compact frame plus these rows). Keeps the card
/// growth bounded and the small-viewport fallback intact.
fn repo_dropdown_visible_rows(wizard: &CreateWizard, repos: &[RepoOption], region_h: u16) -> u16 {
    if wizard.repo_dropdown().is_none() {
        return 0;
    }
    let n = u16::try_from(repo_candidates(repos, wizard.repo_query()).len()).unwrap_or(u16::MAX);
    let want = n.min(REPO_DROPDOWN_WINDOW);
    // The card must fit: WIZARD_CARD_H + rows <= region_h.
    let budget = region_h.saturating_sub(WIZARD_CARD_H);
    want.min(budget)
}

/// How many wrapped lines the Brief row paints: the brief's wrapped-line count
/// (at `value_w`), floored at 1 (the empty brief still shows one row for its
/// placeholder / cursor), capped at [`BRIEF_WINDOW`], then shrunk so the grown
/// card still fits `region_h` (the fixed frame plus these rows). Keeps the growth
/// bounded and the small-viewport fallback intact.
fn brief_visible_rows(wizard: &CreateWizard, value_w: u16, region_h: u16) -> u16 {
    let lines = u16::try_from(wrap_text(wizard.brief(), value_w as usize).len())
        .unwrap_or(u16::MAX)
        .max(1);
    let want = lines.min(BRIEF_WINDOW);
    // The fixed rows always fit (the degenerate check guaranteed region_h >=
    // WIZARD_CARD_H + 1), so the budget is at least 1.
    let budget = region_h.saturating_sub(WIZARD_CARD_H).max(1);
    want.min(budget)
}

/// Wrap `text` into display lines at most `width` chars wide, honouring embedded
/// `\n` as hard breaks. Char-boundary wrapping (not word-aware) — a brief is
/// free text and the cells are what the card must fit. Always returns at least
/// one (possibly empty) line so the Brief row is never zero-height.
fn wrap_text(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut out = Vec::new();
    for segment in text.split('\n') {
        let chars: Vec<char> = segment.chars().collect();
        if chars.is_empty() {
            out.push(String::new());
            continue;
        }
        let mut i = 0;
        while i < chars.len() {
            let end = (i + width).min(chars.len());
            out.push(chars[i..end].iter().collect());
            i = end;
        }
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

/// The placeholder shown on an empty, unfocused Brief row so the optional field
/// reads as skippable rather than broken.
const BRIEF_PLACEHOLDER: &str = "(optional — describe the task)";

/// Render the multi-line Brief value at `(x, y)` over `rows` lines, clipped at
/// `right`. The focused row gets a `▶` marker + green text and a block cursor on
/// the last line; unfocused shows soft-white (or the muted placeholder when
/// empty). A brief longer than `rows` scroll-follows its newest line (an editor
/// caret stays visible while typing). The marker sits on the first painted line;
/// continuation lines indent to align under it.
fn render_brief(
    buf: &mut WireBuffer,
    x: u16,
    y: u16,
    right: u16,
    wizard: &CreateWizard,
    rows: u16,
) {
    let focused = wizard.focus() == WizardRow::Brief;
    let value_colour = if focused { SELECTION_GREEN } else { SOFT_WHITE };
    let marker = if focused { "▶ " } else { "  " };
    let value_x = x.saturating_add(2);
    let width = right.saturating_sub(value_x).max(1) as usize;
    let raw = wizard.brief();
    if raw.is_empty() && !focused {
        put_card_str(buf, x, y, marker, value_colour, right, true);
        put_card_str(buf, value_x, y, BRIEF_PLACEHOLDER, MUTED_GRAY, right, true);
        return;
    }
    let mut lines = wrap_text(raw, width);
    if focused {
        // A block cursor on the newest line — appended after wrapping so it never
        // forces an extra wrap.
        if let Some(last) = lines.last_mut() {
            last.push('\u{2588}');
        }
    }
    // Show the LAST `rows` wrapped lines so the caret stays in view while typing.
    let rows = rows.max(1) as usize;
    let start = lines.len().saturating_sub(rows);
    for (i, line) in lines[start..].iter().enumerate() {
        let ly = y.saturating_add(u16::try_from(i).unwrap_or(u16::MAX));
        let m = if i == 0 { marker } else { "  " };
        let cx = put_card_str(buf, x, ly, m, value_colour, right, true);
        put_card_str(buf, cx, ly, line, value_colour, right, true);
    }
}

/// The label shown in the card's left column for `row`.
const fn wizard_row_label(row: WizardRow) -> &'static str {
    match row {
        WizardRow::Title => "Title",
        WizardRow::Brief => "Brief",
        WizardRow::Link => "Linked",
        WizardRow::Repo => "Repo",
        WizardRow::Source => "Source",
        WizardRow::Target => "Target",
        WizardRow::Agent => "Agent",
    }
}

/// Render one field's marker + value at `(x, y)`, clipped at `right`. The focused
/// row gets a `▶` marker + green value; the others a blank marker + soft-white.
/// The Repo row expands the inline `@` dropdown when it is open; the Target row
/// shows `(unset)` only when blank + unfocused.
#[allow(clippy::too_many_arguments)]
fn render_wizard_field(
    buf: &mut WireBuffer,
    x: u16,
    y: u16,
    right: u16,
    row: WizardRow,
    wizard: &CreateWizard,
    repos: &[RepoOption],
    agents: &[WizardAgent],
) {
    let focused = wizard.focus() == row;
    let value_colour = if focused { SELECTION_GREEN } else { SOFT_WHITE };
    let marker = if focused { "▶ " } else { "  " };
    let cx = put_card_str(buf, x, y, marker, value_colour, right, true);
    let text = |raw: &str| -> String {
        if focused {
            format!("{raw}\u{2588}")
        } else {
            raw.to_string()
        }
    };
    match row {
        WizardRow::Title => {
            put_card_str(buf, cx, y, &text(wizard.title()), value_colour, right, true);
        }
        // The multi-line Brief is painted by [`render_brief`] (it spans several
        // rows), so the single-row path never routes here.
        WizardRow::Brief => {}
        WizardRow::Link => {
            let raw = wizard.link();
            let shown = if focused {
                text(raw)
            } else if raw.is_empty() {
                "(optional — link an upstream issue)".to_string()
            } else {
                raw.to_string()
            };
            let colour = if !focused && raw.is_empty() {
                MUTED_GRAY
            } else {
                value_colour
            };
            put_card_str(buf, cx, y, &shown, colour, right, true);
        }
        WizardRow::Repo => {
            // The open-dropdown case is painted by the caller ([`render_wizard`])
            // because it spans multiple rows; here the row is always CLOSED.
            render_chosen_repo(buf, cx, y, right, wizard.repo_ref(), repos, focused);
        }
        WizardRow::Source => {
            put_card_str(
                buf,
                cx,
                y,
                &text(wizard.source_branch()),
                value_colour,
                right,
                true,
            );
        }
        WizardRow::Target => {
            let raw = wizard.target_branch();
            let shown = if focused {
                text(raw)
            } else if raw.is_empty() {
                "(unset)".to_string()
            } else {
                raw.to_string()
            };
            put_card_str(buf, cx, y, &shown, value_colour, right, true);
        }
        WizardRow::Agent => {
            // A named workspace agent when the roster is injected (V3-F3), else the
            // provider-chip fallback label. `agent_cursor` indexes whichever list
            // is active; an out-of-range cursor degrades to the provider chip.
            let label = agents.get(wizard.agent_cursor()).map_or_else(
                || AgentChip::at(wizard.agent_cursor()).label(),
                |named| named.label.as_str(),
            );
            put_card_str(buf, cx, y, label, value_colour, right, true);
        }
    }
}

/// The marker glyph + its colour for `repo` in the repo picker, mirroring the
/// host new-session picker's kind markers: `◇` scratch, `★☁` remote-only, `★`
/// favorite (both gold), `📁` a local/path-backed repo (cornflower).
fn repo_marker(repo: &RepoOption) -> (&'static str, Color) {
    if repo.repo_ref == "scratch" {
        ("◇ ", MUTED_GRAY)
    } else if repo.is_remote_only {
        ("★☁ ", GOLD)
    } else if repo.is_favorite {
        ("★ ", GOLD)
    } else {
        ("📁 ", CORNFLOWER_BLUE)
    }
}

/// The dimmed locator painted after a candidate's label so identically-named
/// repos are distinguishable — the `repo_ref` (absolute checkout path, `owner/repo`
/// or URL for a remote-only favorite). `None` when it would merely echo the label
/// (scratch, or a raw ref used as its own label), so the row never reads
/// `scratch  scratch`.
fn repo_locator(repo: &RepoOption) -> Option<&str> {
    if repo.repo_ref == "scratch" || repo.repo_ref == repo.label {
        None
    } else {
        Some(&repo.repo_ref)
    }
}

/// Render the CLOSED Repo row's chosen value at `(x, y)`: `<marker> <label>
/// <dimmed path>` for the picked candidate (the path disambiguates same-named
/// repos, left-truncated to keep its tail when the card is narrow), or the empty
/// prompt when nothing is picked. A focused, picked row gets a subtle ` (←→/@)`
/// re-pick affordance so changing a wrong choice is discoverable.
fn render_chosen_repo(
    buf: &mut WireBuffer,
    x: u16,
    y: u16,
    right: u16,
    repo_ref: Option<&str>,
    repos: &[RepoOption],
    focused: bool,
) {
    let value_colour = if focused { SELECTION_GREEN } else { SOFT_WHITE };
    let Some(repo_ref) = repo_ref else {
        put_card_str(
            buf,
            x,
            y,
            "(pick a repo — @ or ←→)",
            value_colour,
            right,
            true,
        );
        return;
    };
    // Resolve the picked ref to its roster candidate for the marker + label + path;
    // a raw ref with no matching candidate falls back to a local (📁) row whose
    // label IS the ref.
    let candidate = repo_candidates(repos, "")
        .into_iter()
        .find(|c| c.repo_ref == repo_ref)
        .unwrap_or_else(|| RepoOption {
            label: repo_ref.to_string(),
            repo_ref: repo_ref.to_string(),
            is_favorite: false,
            is_remote_only: false,
        });
    let (marker, marker_colour) = repo_marker(&candidate);
    let mut cx = put_card_str(buf, x, y, marker, marker_colour, right, true);
    cx = put_card_str(buf, cx, y, &candidate.label, value_colour, right, true);
    if let Some(locator) = repo_locator(&candidate) {
        cx = put_card_str(buf, cx, y, "  ", MUTED_GRAY, right, true);
        let avail = right.saturating_sub(cx) as usize;
        cx = put_card_str(
            buf,
            cx,
            y,
            &left_truncate(locator, avail),
            MUTED_GRAY,
            right,
            true,
        );
    }
    if focused {
        put_card_str(
            buf,
            cx.saturating_add(1),
            y,
            "(←→/@)",
            MUTED_GRAY,
            right,
            true,
        );
    }
}

/// Render the open `@` dropdown as a VERTICAL list mirroring the host new-session
/// picker: a filter line (`@query▌`) on the Repo row itself, then one candidate
/// per row below — `▸ <marker> <label>  <dimmed path>` — the highlighted pick
/// green-arrowed, the rest muted. `visible_rows` candidate rows scroll-follow the
/// cursor; a longer roster flags its overflow with a `… N more` affordance on the
/// filter line. Stays inside the card frame (clipped at `right`).
fn render_repo_dropdown(
    buf: &mut WireBuffer,
    x: u16,
    y: u16,
    right: u16,
    wizard: &CreateWizard,
    repos: &[RepoOption],
    visible_rows: u16,
) {
    let cursor = wizard.repo_dropdown().unwrap_or(0);
    let query = wizard.repo_query();
    let candidates = repo_candidates(repos, query);
    let n = candidates.len();
    // Filter line: the live `@query` with a block cursor, on the Repo row.
    let fx = put_card_str(
        buf,
        x,
        y,
        &format!("@{query}\u{2588}"),
        SELECTION_GREEN,
        right,
        true,
    );

    // Scroll-follow window: keep the cursor in view, anchored at the window bottom
    // once it scrolls past the first page. Derived purely from the cursor so no
    // scroll state has to live on the wizard (the reducer already clamps the
    // cursor into `0..n`).
    let window = visible_rows as usize;
    let start = cursor.saturating_sub(window.saturating_sub(1));
    let end = (start + window).min(n);
    if end < n {
        // More candidates below the window — flag the overflow inline.
        put_card_str(
            buf,
            fx.saturating_add(1),
            y,
            &format!("… {} more", n - end),
            MUTED_GRAY,
            right,
            true,
        );
    }

    for (row_i, i) in (start..end).enumerate() {
        let repo = &candidates[i];
        let cy = y.saturating_add(1 + u16::try_from(row_i).unwrap_or(0));
        let selected = i == cursor;
        let arrow = if selected { "▸ " } else { "  " };
        let arrow_colour = if selected {
            SELECTION_GREEN
        } else {
            MUTED_GRAY
        };
        let mut cx = put_card_str(buf, x, cy, arrow, arrow_colour, right, true);
        let (marker, marker_colour) = repo_marker(repo);
        cx = put_card_str(buf, cx, cy, marker, marker_colour, right, true);
        let label_colour = if selected {
            SELECTION_GREEN
        } else {
            SOFT_WHITE
        };
        cx = put_card_str(buf, cx, cy, &repo.label, label_colour, right, true);
        if let Some(locator) = repo_locator(repo) {
            cx = put_card_str(buf, cx, cy, "  ", MUTED_GRAY, right, true);
            let avail = right.saturating_sub(cx) as usize;
            put_card_str(
                buf,
                cx,
                cy,
                &left_truncate(locator, avail),
                MUTED_GRAY,
                right,
                true,
            );
        }
    }
}

/// Truncate `s` to `max` characters KEEPING THE TAIL (prefixed with `…` when cut),
/// so a long repo path stays disambiguating — the tail (the repo's own folder) is
/// what tells two same-named checkouts apart. Multi-byte safe (operates on chars).
fn left_truncate(s: &str, max: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max {
        return s.to_string();
    }
    if max <= 1 {
        return "…".to_string();
    }
    let tail: String = chars[chars.len() - (max - 1)..].iter().collect();
    format!("…{tail}")
}

/// Draw a rounded gold card frame from `(left, top)` to `(right-1, bottom)` over
/// the backdrop.
fn draw_card_frame(buf: &mut WireBuffer, left: u16, right: u16, top: u16, bottom: u16) {
    let last = right.saturating_sub(1);
    put_card_char(buf, left, top, '╭');
    put_card_char(buf, last, top, '╮');
    put_card_char(buf, left, bottom, '╰');
    put_card_char(buf, last, bottom, '╯');
    for x in (left + 1)..last {
        put_card_char(buf, x, top, '─');
        put_card_char(buf, x, bottom, '─');
    }
    for y in (top + 1)..bottom {
        put_card_char(buf, left, y, '│');
        put_card_char(buf, last, y, '│');
    }
}

/// Write one gold frame glyph at `(x, y)` over the card backdrop.
fn put_card_char(buf: &mut WireBuffer, x: u16, y: u16, ch: char) {
    let mut cell = Cell::new(ch.to_string());
    cell.fg = Some(GOLD);
    cell.bg = Some(CARD_BACKDROP);
    buf.push(Coord::new(x, y), cell);
}

/// Write `s` at `(x, row)` in `color`, clipping at `right` (exclusive). When
/// `backdrop` is set the cells carry the card backdrop so the text floats on the
/// card surface. Multi-byte safe. Returns the next free column.
fn put_card_str(
    buf: &mut WireBuffer,
    x: u16,
    row: u16,
    s: &str,
    color: Color,
    right: u16,
    backdrop: bool,
) -> u16 {
    let mut cx = x;
    for ch in s.chars() {
        if cx >= right {
            break;
        }
        let mut cell = Cell::new(ch.to_string());
        cell.fg = Some(color);
        if backdrop {
            cell.bg = Some(CARD_BACKDROP);
        }
        buf.push(Coord::new(cx, row), cell);
        cx = cx.saturating_add(1);
    }
    cx
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
            branch: None,
            repo_ref: None,
            agent: None,
            source_branch: None,
            target_branch: None,
            external_ref: None,
            run_count: 0,
            last_run_status: None,
            last_run_at: None,
            parent_id: None,
            child_total: 0,
            child_done: 0,
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
        let rows: Vec<IssueRow> = (0..4).map(|i| row(&format!("t{i}"), "todo", None)).collect();
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
        let order: Vec<&str> = s.rows_in_column(IssueColumn::Todo).map(|r| r.id.as_str()).collect();
        assert_eq!(order, vec!["t0", "t1", "t2"]);

        // Drag t0 to slot 2 (the bottom of the Todo column) — a downward move.
        s.reorder_within_column("t0", 2);
        let order: Vec<&str> = s.rows_in_column(IssueColumn::Todo).map(|r| r.id.as_str()).collect();
        assert_eq!(order, vec!["t1", "t2", "t0"], "t0 reseats to the bottom");

        // Drag t0 back to slot 0 (the top) — an upward move.
        s.reorder_within_column("t0", 0);
        let order: Vec<&str> = s.rows_in_column(IssueColumn::Todo).map(|r| r.id.as_str()).collect();
        assert_eq!(order, vec!["t0", "t1", "t2"], "t0 reseats back to the top");

        // The In Progress column is untouched.
        let prog: Vec<&str> =
            s.rows_in_column(IssueColumn::InProgress).map(|r| r.id.as_str()).collect();
        assert_eq!(prog, vec!["p0"]);

        // A reorder of an unknown id is a no-op.
        let before: Vec<String> =
            s.rows_in_column(IssueColumn::Todo).map(|r| r.id.as_str().to_string()).collect();
        s.reorder_within_column("ghost", 0);
        let after: Vec<String> =
            s.rows_in_column(IssueColumn::Todo).map(|r| r.id.as_str().to_string()).collect();
        assert_eq!(before, after);
    }

    /// Phase 5 — the create wizard renders as a single centered card showing ALL
    /// five field labels at once, the typed title, the `main`-prefilled branches,
    /// the default agent, and the nav-key footer hint, inside a gold rounded
    /// frame. Pins the card layout the manual `just dev` walk exercises.
    #[test]
    fn wizard_card_renders_all_fields_and_hint() {
        let s = reduce_issue_list(&IssueListState::default(), IssueListEvent::Key('c')).state;
        let s = reduce_issue_list(&s, IssueListEvent::Key('F')).state;
        let mut buf = WireBuffer::new(120, 24);
        render_issue_list(&mut buf, 120, 1, 23, &s, 0);
        let painted = painted_text(&buf);
        for needle in [
            "New task",
            "Title",
            "Brief",
            "Repo",
            "Source",
            "Target",
            "Agent",
            "main",
            "claude",
            "↑↓ row",
            "Enter create",
            "Esc cancel",
        ] {
            assert!(
                painted.contains(needle),
                "missing {needle:?} in card:\n{painted}"
            );
        }
        // The typed title shows on the (focused) Title row.
        assert!(painted.contains('F'), "typed title missing:\n{painted}");
        // The card frame contributes a gold rounded top-left corner (the board's
        // own card corners are not gold, so this is the wizard's frame).
        let has_gold_corner = buf.cells.iter().any(|(_, c)| c.symbol == "╭" && c.fg == Some(GOLD));
        assert!(
            has_gold_corner,
            "card must have a gold rounded top-left corner"
        );
    }

    /// A multi-line Brief renders its wrapped value inside the card AND grows the
    /// card's painted-row span versus an empty Brief — the dynamic-height contract
    /// the repo dropdown already established, reused for the Brief region.
    #[test]
    fn wizard_brief_renders_wrapped_and_grows_card() {
        let painted_row_span = |s: &IssueListState| -> u16 {
            let mut buf = WireBuffer::new(120, 24);
            render_issue_list(&mut buf, 120, 1, 23, s, 0);
            let ys: Vec<u16> = buf
                .cells
                .iter()
                .filter(|(_, c)| c.fg == Some(GOLD) && (c.symbol == "│" || c.symbol == "╭"))
                .map(|(coord, _)| coord.y)
                .collect();
            let (min, max) = (ys.iter().min().copied(), ys.iter().max().copied());
            max.unwrap_or(0).saturating_sub(min.unwrap_or(0))
        };

        // Empty-Brief baseline card span.
        let empty = reduce_issue_list(&IssueListState::default(), IssueListEvent::Key('c')).state;
        let empty_span = painted_row_span(&empty);

        // Focus Brief, type enough to wrap onto several lines.
        let s = reduce_issue_list(&empty, IssueListEvent::Wizard(WizardKey::Down)).state; // Brief
        assert_eq!(s.wizard().unwrap().focus(), WizardRow::Brief);
        let long = "reproduce the login 500 then patch the handler and add a regression test";
        let s = type_into(&s, long);

        let mut buf = WireBuffer::new(120, 24);
        render_issue_list(&mut buf, 120, 1, 23, &s, 0);
        let painted = painted_text(&buf);
        // A leading slice of the brief renders inside the card.
        assert!(
            painted.contains("reproduce the login"),
            "wrapped brief value must render:\n{painted}"
        );
        // The card grew to make room for the wrapped brief lines.
        assert!(
            painted_row_span(&s) > empty_span,
            "card must grow for a multi-line brief ({} !> {empty_span})",
            painted_row_span(&s)
        );
    }

    /// Enter on an EMPTY Brief must be a no-op — never seed a leading `\n`. That
    /// stray newline would flow byte-verbatim into `issue.description` and shove a
    /// leading `/name` skill line off position 0, breaking headless `claude -p`
    /// dispatch. Enter AFTER typed text still inserts a newline for genuine
    /// multi-line editing, and no leading newline ever appears.
    #[test]
    fn wizard_brief_enter_on_empty_does_not_seed_leading_newline() {
        // Open the wizard, focus the Brief row.
        let s = reduce_issue_list(&IssueListState::default(), IssueListEvent::Key('c')).state;
        let s = reduce_issue_list(&s, IssueListEvent::Wizard(WizardKey::Down)).state; // Brief
        assert_eq!(s.wizard().unwrap().focus(), WizardRow::Brief);

        // Enter on the empty buffer: no-op (RED before the fix — would be "\n").
        let s = reduce_issue_list(&s, IssueListEvent::Wizard(WizardKey::Enter)).state;
        assert_eq!(
            s.wizard().unwrap().brief(),
            "",
            "Enter on an empty Brief must not seed a leading newline"
        );
        // Enter still does NOT fire create — the wizard is still open.
        assert!(s.wizard().is_some(), "Enter on Brief must not create");

        // Type text, Enter (inserts a newline), type more: the embedded newline is
        // preserved and there is no leading newline.
        let s = type_into(&s, "Read");
        let s = reduce_issue_list(&s, IssueListEvent::Wizard(WizardKey::Enter)).state;
        let s = type_into(&s, "the docs");
        assert_eq!(
            s.wizard().unwrap().brief(),
            "Read\nthe docs",
            "mid-content Enter must preserve embedded newlines with no leading \\n"
        );
        assert!(
            !s.wizard().unwrap().brief().starts_with('\n'),
            "brief must never begin with a newline"
        );
    }

    /// A `RepoOption` roster fixture: a local scan + a favorite whose path differs
    /// from its alias, so a render test can assert the dimmed locator.
    fn repo_roster() -> Vec<RepoOption> {
        vec![
            RepoOption {
                label: "rosetta".into(),
                repo_ref: "/Users/dev/work/rosetta".into(),
                is_favorite: false,
                is_remote_only: false,
            },
            RepoOption {
                label: "acme".into(),
                repo_ref: "/Users/dev/fav/acme".into(),
                is_favorite: true,
                is_remote_only: false,
            },
        ]
    }

    /// Opening the `@` dropdown renders a VERTICAL list (new-session style): the
    /// `@`-filter line, then one candidate per row with its marker, scratch first,
    /// and the highlighted pick carrying the green `▸` arrow. Each candidate sits on
    /// its own buffer row (proving the list is vertical, not the old single line).
    #[test]
    fn wizard_card_repo_dropdown_lists_vertically() {
        let mut s = reduce_issue_list(&IssueListState::default(), IssueListEvent::Key('c')).state;
        s.set_repos(repo_roster());
        let s = type_into(&s, "Fix");
        // Move focus past Brief to the Repo row, then open the dropdown.
        let s = reduce_issue_list(&s, IssueListEvent::Wizard(WizardKey::Down)).state; // Brief
        let s = reduce_issue_list(&s, IssueListEvent::Wizard(WizardKey::Down)).state; // Link
        let s = reduce_issue_list(&s, IssueListEvent::Wizard(WizardKey::Down)).state; // Repo
        let s = reduce_issue_list(&s, IssueListEvent::Wizard(WizardKey::Char('@'))).state;
        let mut buf = WireBuffer::new(120, 24);
        render_issue_list(&mut buf, 120, 1, 23, &s, 0);
        let painted = painted_text(&buf);
        // The filter line and every candidate label are present.
        for needle in ["@", "scratch", "rosetta", "acme", "▸"] {
            assert!(painted.contains(needle), "missing {needle:?}:\n{painted}");
        }
        // Each candidate's marker glyph is unique to the card (never in the board
        // behind it): ◇ scratch, 📁 local, ★ favorite. They must land on THREE
        // distinct, strictly increasing rows — proof the list is vertical, scratch
        // first — and the green ▸ arrow marks scratch (cursor 0 on open).
        let marker_row = |glyph: &str| {
            buf.cells.iter().find(|(_, c)| c.symbol == glyph).map(|(coord, _)| coord.y)
        };
        let scratch_row = marker_row("◇").expect("scratch marker painted");
        let rosetta_row = marker_row("📁").expect("local marker painted");
        let acme_row = marker_row("★").expect("favorite marker painted");
        assert!(
            scratch_row < rosetta_row && rosetta_row < acme_row,
            "candidates must render on distinct increasing rows (scratch first): \
             {scratch_row} < {rosetta_row} < {acme_row}"
        );
        let arrow_row = buf
            .cells
            .iter()
            .find(|(_, c)| c.symbol == "▸")
            .map(|(coord, _)| coord.y)
            .expect("green selection arrow painted");
        assert_eq!(
            arrow_row, scratch_row,
            "▸ must mark the highlighted scratch row"
        );
    }

    /// A picked local repo's CLOSED row shows its marker + label + the dimmed path,
    /// so which repo was chosen is unambiguous. The path substring is painted.
    #[test]
    fn wizard_picked_repo_shows_marker_label_and_path() {
        let mut s = reduce_issue_list(&IssueListState::default(), IssueListEvent::Key('c')).state;
        s.set_repos(repo_roster());
        let s = type_into(&s, "Fix");
        // Focus Repo (past Brief), cycle ←→ to pick `rosetta` (scratch=0, rosetta=1).
        let s = reduce_issue_list(&s, IssueListEvent::Wizard(WizardKey::Down)).state; // Brief
        let s = reduce_issue_list(&s, IssueListEvent::Wizard(WizardKey::Down)).state; // Link
        let s = reduce_issue_list(&s, IssueListEvent::Wizard(WizardKey::Down)).state; // Repo
        let s = reduce_issue_list(&s, IssueListEvent::Wizard(WizardKey::Right)).state; // scratch
        let s = reduce_issue_list(&s, IssueListEvent::Wizard(WizardKey::Right)).state; // rosetta
        assert_eq!(
            s.wizard().unwrap().repo_ref(),
            Some("/Users/dev/work/rosetta")
        );
        let mut buf = WireBuffer::new(120, 24);
        render_issue_list(&mut buf, 120, 1, 23, &s, 0);
        let painted = painted_text(&buf);
        assert!(painted.contains("rosetta"), "label missing:\n{painted}");
        assert!(
            painted.contains("/Users/dev/work/rosetta"),
            "dimmed path missing:\n{painted}"
        );
    }

    /// Two repos with the SAME basename render distinguishable dimmed paths, so the
    /// picker can tell them apart — the whole point of the locator.
    #[test]
    fn wizard_dropdown_distinguishes_same_basename_repos() {
        let roster = vec![
            RepoOption {
                label: "rosetta".into(),
                repo_ref: "/Users/dev/a/rosetta".into(),
                is_favorite: false,
                is_remote_only: false,
            },
            RepoOption {
                label: "rosetta".into(),
                repo_ref: "/Users/dev/b/rosetta".into(),
                is_favorite: false,
                is_remote_only: false,
            },
        ];
        let mut s = reduce_issue_list(&IssueListState::default(), IssueListEvent::Key('c')).state;
        s.set_repos(roster);
        let s = type_into(&s, "Fix");
        let s = reduce_issue_list(&s, IssueListEvent::Wizard(WizardKey::Down)).state; // Brief
        let s = reduce_issue_list(&s, IssueListEvent::Wizard(WizardKey::Down)).state; // Link
        let s = reduce_issue_list(&s, IssueListEvent::Wizard(WizardKey::Down)).state; // Repo
        let s = reduce_issue_list(&s, IssueListEvent::Wizard(WizardKey::Char('@'))).state;
        let mut buf = WireBuffer::new(120, 24);
        render_issue_list(&mut buf, 120, 1, 23, &s, 0);
        let painted = painted_text(&buf);
        assert!(
            painted.contains("/Users/dev/a/rosetta") && painted.contains("/Users/dev/b/rosetta"),
            "both same-basename paths must render distinctly:\n{painted}"
        );
    }

    /// A locator too wide for the field is truncated from the LEFT — the
    /// disambiguating tail (the deepest path segments) is what survives, prefixed
    /// with `…`. Keeping the head instead would collapse same-basename repos back
    /// into indistinguishable prefixes, so this behaviour is load-bearing.
    #[test]
    fn left_truncate_keeps_the_disambiguating_tail() {
        let path = "/Users/dev/very/long/workspace/path/to/rosetta";
        let out = left_truncate(path, 12);
        assert!(
            out.starts_with('…'),
            "truncated locator must lead with …: {out}"
        );
        assert!(out.ends_with("to/rosetta"), "must keep the tail: {out}");
        assert!(
            !out.contains("/Users/dev"),
            "must NOT keep the non-disambiguating head: {out}"
        );
        // Exactly `max` chars wide (… + max-1 tail chars).
        assert_eq!(
            out.chars().count(),
            12,
            "truncated width must be `max`: {out}"
        );
        // A locator that already fits is returned untouched.
        assert_eq!(left_truncate("short", 12), "short");
    }

    /// The card GROWS while the dropdown is open (more painted rows) and returns to
    /// the compact height when it closes — measured by the span of painted rows.
    #[test]
    fn wizard_card_grows_on_open_and_compacts_on_close() {
        let mut base =
            reduce_issue_list(&IssueListState::default(), IssueListEvent::Key('c')).state;
        base.set_repos(repo_roster());
        let base = type_into(&base, "Fix");
        let base = reduce_issue_list(&base, IssueListEvent::Wizard(WizardKey::Down)).state; // Brief
        let base = reduce_issue_list(&base, IssueListEvent::Wizard(WizardKey::Down)).state; // Link
        let base = reduce_issue_list(&base, IssueListEvent::Wizard(WizardKey::Down)).state; // Repo

        let painted_row_span = |s: &IssueListState| -> u16 {
            let mut buf = WireBuffer::new(120, 24);
            render_issue_list(&mut buf, 120, 1, 23, s, 0);
            // Rows carrying a gold frame glyph bound the card top/bottom.
            let ys: Vec<u16> = buf
                .cells
                .iter()
                .filter(|(_, c)| c.fg == Some(GOLD) && (c.symbol == "│" || c.symbol == "╭"))
                .map(|(coord, _)| coord.y)
                .collect();
            let (min, max) = (ys.iter().min().copied(), ys.iter().max().copied());
            max.unwrap_or(0).saturating_sub(min.unwrap_or(0))
        };

        let closed_span = painted_row_span(&base);
        let open = reduce_issue_list(&base, IssueListEvent::Wizard(WizardKey::Char('@'))).state;
        let open_span = painted_row_span(&open);
        assert!(
            open_span > closed_span,
            "card must grow when the dropdown opens ({open_span} !> {closed_span})"
        );

        // Closing (Enter picks + closes) returns to the compact span.
        let closed_again = reduce_issue_list(&open, IssueListEvent::Wizard(WizardKey::Enter)).state;
        assert_eq!(
            painted_row_span(&closed_again),
            closed_span,
            "card must return to compact height on close"
        );
    }

    /// A roster longer than the visible window scrolls: paging the cursor Down past
    /// the window reveals a later candidate that was off-screen at open.
    #[test]
    fn wizard_dropdown_scrolls_window_on_down() {
        let roster: Vec<RepoOption> = (0..12)
            .map(|i| RepoOption {
                label: format!("repo{i:02}"),
                repo_ref: format!("/w/repo{i:02}"),
                is_favorite: false,
                is_remote_only: false,
            })
            .collect();
        let mut s = reduce_issue_list(&IssueListState::default(), IssueListEvent::Key('c')).state;
        s.set_repos(roster);
        let s = type_into(&s, "Fix");
        let s = reduce_issue_list(&s, IssueListEvent::Wizard(WizardKey::Down)).state; // Brief
        let s = reduce_issue_list(&s, IssueListEvent::Wizard(WizardKey::Down)).state; // Link
        let s = reduce_issue_list(&s, IssueListEvent::Wizard(WizardKey::Down)).state; // Repo
        let s = reduce_issue_list(&s, IssueListEvent::Wizard(WizardKey::Char('@'))).state;

        // At open the last repo is off-window (13 candidates incl. scratch, window 6).
        let mut buf = WireBuffer::new(120, 24);
        render_issue_list(&mut buf, 120, 1, 23, &s, 0);
        assert!(
            !painted_text(&buf).contains("repo11"),
            "last repo should be off-window at open"
        );

        // Page the cursor to the bottom; the window scroll-follows it into view.
        let s = (0..12).fold(s, |s, _| {
            reduce_issue_list(&s, IssueListEvent::Wizard(WizardKey::Down)).state
        });
        let mut buf = WireBuffer::new(120, 24);
        render_issue_list(&mut buf, 120, 1, 23, &s, 0);
        assert!(
            painted_text(&buf).contains("repo11"),
            "scrolling down must reveal the last repo"
        );
    }

    /// A degenerate viewport still paints the card title + hint — never panics,
    /// never renders empty.
    #[test]
    fn wizard_card_degenerate_viewport_never_empty() {
        let s = reduce_issue_list(&IssueListState::default(), IssueListEvent::Key('c')).state;
        let mut buf = WireBuffer::new(12, 3);
        render_issue_list(&mut buf, 12, 1, 2, &s, 0);
        let painted = painted_text(&buf);
        assert!(
            painted.contains("New task"),
            "degenerate viewport must still show the card title:\n{painted}"
        );
    }

    /// Type a string into the focused wizard row, char by char.
    fn type_into(state: &IssueListState, text: &str) -> IssueListState {
        text.chars().fold(state.clone(), |s, ch| {
            reduce_issue_list(&s, IssueListEvent::Wizard(WizardKey::Char(ch))).state
        })
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

    /// `x` on a selected row opens the RED confirm overlay targeting that issue,
    /// captures text (so nav keys are swallowed behind the modal), and raises no
    /// intent yet (63d).
    #[test]
    fn x_opens_confirm_delete_for_selected_issue() {
        let s = IssueListState::with_rows(vec![row("i1", "open", None), row("i2", "open", None)]);
        let out = reduce_issue_list(&s, IssueListEvent::Key('x'));
        assert_eq!(out.intent, None, "opening the confirm raises no intent");
        assert_eq!(out.state.mode(), IssueListMode::ConfirmDelete);
        assert!(out.state.is_capturing_text(), "confirm is a modal capture");
        let pending = out.state.confirm_delete().expect("a target is set");
        assert_eq!(pending.id, IssueId::from_str("i1").unwrap());
        assert!(pending.label.contains("Issue i1"), "label names the issue");
    }

    /// Esc cancels the confirm overlay in one press, back to normal navigation
    /// with the target dropped and no intent (63d).
    #[test]
    fn esc_cancels_confirm_delete() {
        let s = IssueListState::with_rows(vec![row("i1", "open", None)]);
        let opened = reduce_issue_list(&s, IssueListEvent::Key('x')).state;
        assert_eq!(opened.mode(), IssueListMode::ConfirmDelete);
        // Esc is delivered to the pure reducer as the ESC char.
        let out = reduce_issue_list(&opened, IssueListEvent::Key('\u{1b}'));
        assert_eq!(out.intent, None, "cancel raises no intent");
        assert_eq!(out.state.mode(), IssueListMode::Normal);
        assert!(out.state.confirm_delete().is_none(), "target dropped");
    }

    /// Enter on the confirm overlay emits the `DeleteIssue` intent for the target
    /// and returns to normal navigation (63d).
    #[test]
    fn enter_confirms_delete_and_emits_intent() {
        let s = IssueListState::with_rows(vec![row("i1", "open", None), row("i2", "open", None)]);
        // Select the second row, then confirm-delete it.
        let s = reduce_issue_list(&s, IssueListEvent::Key('j')).state;
        let opened = reduce_issue_list(&s, IssueListEvent::Key('x')).state;
        let out = reduce_issue_list(&opened, IssueListEvent::Key('\n'));
        assert_eq!(
            out.intent,
            Some(IssueListIntent::DeleteIssue(
                IssueId::from_str("i2").unwrap()
            )),
            "Enter emits DeleteIssue for the selected target"
        );
        assert_eq!(out.state.mode(), IssueListMode::Normal);
        assert!(
            out.state.confirm_delete().is_none(),
            "target dropped after commit"
        );
    }

    /// Arming the "cancel run(s) & delete" overlay (the plugin-glue seam for a
    /// delete refused on active tasks) selects the target row, enters the modal
    /// capture mode, and raises no intent yet.
    #[test]
    fn open_confirm_cancel_delete_arms_the_overlay() {
        let mut s =
            IssueListState::with_rows(vec![row("i1", "open", None), row("i2", "open", None)]);
        s.open_confirm_cancel_delete_for("i2");
        assert_eq!(s.mode(), IssueListMode::ConfirmCancelDelete);
        assert!(
            s.is_capturing_text(),
            "the cancel-delete overlay is a modal capture"
        );
        let pending = s.confirm_cancel_delete().expect("a target is set");
        assert_eq!(pending.id, IssueId::from_str("i2").unwrap());
        // Arming for an unknown id (the row already vanished) is a no-op.
        let mut gone = IssueListState::with_rows(vec![row("i1", "open", None)]);
        gone.open_confirm_cancel_delete_for("nope");
        assert_eq!(
            gone.mode(),
            IssueListMode::Normal,
            "unknown id arms nothing"
        );
        assert!(gone.confirm_cancel_delete().is_none());
    }

    /// `c` (and Enter) on the cancel-delete overlay emits `CancelAndDeleteIssue`
    /// for the target and returns to normal navigation; Esc backs out cleanly.
    #[test]
    fn confirm_cancel_delete_emits_intent_and_esc_backs_out() {
        let mut s =
            IssueListState::with_rows(vec![row("i1", "open", None), row("i2", "open", None)]);
        s.open_confirm_cancel_delete_for("i2");

        // `c` confirms → cancel-then-delete intent for i2.
        let out = reduce_issue_list(&s, IssueListEvent::Key('c'));
        assert_eq!(
            out.intent,
            Some(IssueListIntent::CancelAndDeleteIssue(
                IssueId::from_str("i2").unwrap()
            )),
            "c emits CancelAndDeleteIssue for the target"
        );
        assert_eq!(out.state.mode(), IssueListMode::Normal);
        assert!(
            out.state.confirm_cancel_delete().is_none(),
            "target dropped after commit"
        );

        // Enter is an equivalent confirm.
        let via_enter = reduce_issue_list(&s, IssueListEvent::Key('\n'));
        assert_eq!(
            via_enter.intent,
            Some(IssueListIntent::CancelAndDeleteIssue(
                IssueId::from_str("i2").unwrap()
            )),
            "Enter also confirms cancel-and-delete"
        );

        // Esc backs out with no intent, target dropped.
        let esc = reduce_issue_list(&s, IssueListEvent::Key('\u{1b}'));
        assert_eq!(esc.intent, None, "Esc raises no intent");
        assert_eq!(esc.state.mode(), IssueListMode::Normal);
        assert!(
            esc.state.confirm_cancel_delete().is_none(),
            "target dropped on Esc"
        );
    }

    /// `x` on an empty list is a no-op — no confirm opens, nothing to delete (63d).
    #[test]
    fn x_on_empty_list_is_a_noop() {
        let s = IssueListState::with_rows(Vec::new());
        let out = reduce_issue_list(&s, IssueListEvent::Key('x'));
        assert_eq!(out.intent, None);
        assert_eq!(out.state.mode(), IssueListMode::Normal, "no confirm opens");
        assert!(out.state.confirm_delete().is_none());
    }

    /// `x` does NOT open the confirm while the create wizard is open — the wizard
    /// captures the keystroke as text, leaving its state machine intact (63d).
    #[test]
    fn x_does_not_fire_while_create_wizard_open() {
        let s = IssueListState::with_rows(vec![row("i1", "open", None)]);
        let wizard = reduce_issue_list(&s, IssueListEvent::Key('c')).state;
        assert_eq!(wizard.mode(), IssueListMode::CreateInput);
        let out = reduce_issue_list(&wizard, IssueListEvent::Key('x'));
        assert_eq!(
            out.state.mode(),
            IssueListMode::CreateInput,
            "x is typed into the wizard, never opens a delete confirm"
        );
        assert!(out.state.confirm_delete().is_none());
        // The wizard is still open with the typed 'x' on the (focused) Title row.
        let wizard = out.state.wizard().expect("wizard still open");
        assert_eq!(wizard.focus(), WizardRow::Title);
        assert_eq!(wizard.title(), "x");
    }

    /// `x` does NOT open the confirm while the `/` filter input is open — it is a
    /// query character, not the delete shortcut (63d).
    #[test]
    fn x_does_not_fire_while_filter_input_open() {
        let s = IssueListState::with_rows(vec![row("i1", "open", None)]);
        let filtering = reduce_issue_list(&s, IssueListEvent::Key('/')).state;
        assert_eq!(filtering.mode(), IssueListMode::FilterInput);
        let out = reduce_issue_list(&filtering, IssueListEvent::Key('x'));
        assert_eq!(
            out.state.mode(),
            IssueListMode::FilterInput,
            "x is typed into the filter query, never opens a delete confirm"
        );
        assert!(out.state.confirm_delete().is_none());
        assert_eq!(out.state.query(), "x", "x appended to the filter query");
    }
}
