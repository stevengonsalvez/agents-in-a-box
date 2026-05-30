//! P4.3 — Issue list screen: the pure reducer + width-aware render.
//!
//! The issue list is the default landing screen (hotkey `1`). It renders a
//! workspace's [`IssueRow`]s grouped by lifecycle status into three columns —
//! Todo / In Progress / Done — with row selection, filter chips, and a
//! type-narrow filter-input mode. As with the screen router ([`crate::screen`]),
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
use ainb_plugin_sdk::{Cell, Color, Coord, WireBuffer};

/// The three status columns issues are bucketed into for display.
///
/// The mapping from the wire `state` string to a column is owned by
/// [`IssueColumn::for_state`]; everything the daemon doesn't explicitly mark
/// done or in-progress falls into [`IssueColumn::Todo`] so a new lifecycle
/// string never silently vanishes from the board.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IssueColumn {
    /// Not started — the default bucket (`"open"`, `"todo"`, unknown states).
    Todo,
    /// Actively being worked (`"in_progress"`).
    InProgress,
    /// Terminal / closed (`"done"`, `"closed"`).
    Done,
}

impl IssueColumn {
    /// Bucket a wire `state` string into its display column.
    ///
    /// Unknown states fall into [`IssueColumn::Todo`] (fail-visible, not
    /// fail-hidden) so the board never drops a row the daemon sent.
    #[must_use]
    pub fn for_state(state: &str) -> Self {
        match state {
            "in_progress" => Self::InProgress,
            "done" | "closed" => Self::Done,
            _ => Self::Todo,
        }
    }

    /// The column header label (without the count suffix).
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Todo => "Todo",
            Self::InProgress => "In Progress",
            Self::Done => "Done",
        }
    }

    /// The three columns in left-to-right display order.
    #[must_use]
    pub const fn all() -> [Self; 3] {
        [Self::Todo, Self::InProgress, Self::Done]
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

/// Whether the screen is in normal navigation or filter-text-entry mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IssueListMode {
    /// Normal row navigation (`j`/`k`, `enter`, `c`, …).
    Normal,
    /// `/` filter-input mode: keystrokes append to the [`IssueListState::query`].
    FilterInput,
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
    /// Whether we are navigating or typing a filter.
    mode: IssueListMode,
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
    /// Start the create-issue flow.
    CreateIssue,
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
        IssueListMode::Normal => reduce_normal_key(state, c),
    }
}

/// Normal-mode key handling: navigation + intent-raising keys.
fn reduce_normal_key(state: &IssueListState, c: char) -> IssueListReduction {
    match c {
        'j' => move_selection_down(state),
        'k' => move_selection_up(state),
        '/' => enter_filter_mode(state),
        'c' => with_intent(state.clone(), IssueListIntent::CreateIssue),
        'a' => state.selected_row().map_or_else(
            || unchanged(state),
            |row| with_intent(state.clone(), IssueListIntent::OpenAgentPicker(row.id.clone())),
        ),
        // Enter (delivered as '\n' or '\r') opens the selected row's task detail.
        '\n' | '\r' => state.selected_row().map_or_else(
            || unchanged(state),
            |row| with_intent(state.clone(), IssueListIntent::OpenTaskDetail(row.id.clone())),
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

    // Width split: status/assignee take fixed caps, the title absorbs the rest.
    let status_w: u16 = 13; // "In Progress" + padding
    let assignee_w: u16 = area_w.saturating_sub(status_w).min(20);
    let title_w = area_w
        .saturating_sub(status_w)
        .saturating_sub(assignee_w)
        .max(8);

    // The chip bar consumed `top`; the columns start one row below it.
    let mut row = top.saturating_add(1);
    let mut visible_index = 0usize;
    for column in IssueColumn::all() {
        if row >= bottom {
            break;
        }
        // Column header with live count, e.g. "Todo (3)".
        let header = format!("{} ({})", column.label(), state.column_count(column));
        put_str(buf, 0, row, &header, GOLD, area_w);
        row = row.saturating_add(1);

        for r in state.rows_in_column(column) {
            if row >= bottom {
                break;
            }
            let selected = visible_index == state.selected;
            let marker_color = if selected { SELECTION_GREEN } else { MUTED_GRAY };
            put_str(buf, 0, row, if selected { "▶ " } else { "  " }, marker_color, area_w);

            let text_color = if selected { SOFT_WHITE } else { MUTED_GRAY };
            put_str(buf, 2, row, &clip(&r.title, title_w), text_color, area_w);

            let ax = 2u16.saturating_add(title_w).saturating_add(1);
            let assignee = r.assignee.as_deref().unwrap_or("—");
            put_str(buf, ax, row, &clip(assignee, assignee_w), MUTED_GRAY, area_w);

            visible_index = visible_index.saturating_add(1);
            row = row.saturating_add(1);
        }
    }
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
            workspace_id: "ws".into(),
            title: format!("Issue {id}"),
            description: None,
            state: state.into(),
            assignee: assignee.map(ToString::to_string),
            creator: "member:alice".into(),
            created_at: 0,
        }
    }

    /// Unknown lifecycle strings fall into Todo (fail-visible).
    #[test]
    fn unknown_state_buckets_to_todo() {
        assert_eq!(IssueColumn::for_state("open"), IssueColumn::Todo);
        assert_eq!(IssueColumn::for_state("weird"), IssueColumn::Todo);
        assert_eq!(IssueColumn::for_state("in_progress"), IssueColumn::InProgress);
        assert_eq!(IssueColumn::for_state("done"), IssueColumn::Done);
        assert_eq!(IssueColumn::for_state("closed"), IssueColumn::Done);
    }

    /// `k` moves the selection up and saturates at the top.
    #[test]
    fn k_key_moves_selection_up_and_saturates() {
        let s = IssueListState::with_rows(vec![
            row("i1", "open", None),
            row("i2", "open", None),
        ]);
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
            Some(IssueListIntent::OpenAgentPicker(IssueId::from_str("i1").unwrap()))
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

    /// The renderer draws a 50-issue fixture at the 80×24 floor without writing
    /// a single out-of-bounds cell (cross-cutting 80×24 floor guard, P4.md:537).
    #[test]
    fn renders_50_issues_at_floor_without_overflow() {
        const FLOOR_W: u16 = 80;
        const FLOOR_H: u16 = 24;
        let rows: Vec<IssueRow> = (0..50)
            .map(|i| row(&format!("i{i}"), if i % 3 == 0 { "done" } else { "open" }, Some("agent:c")))
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
}
