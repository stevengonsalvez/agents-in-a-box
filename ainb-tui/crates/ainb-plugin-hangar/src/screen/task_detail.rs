//! P4.4 — Task detail + transcript screen: the pure reducer + width-aware render.
//!
//! The task-detail screen (hotkey `2`, or `enter` on an issue-list row) shows a
//! single task: a header (issue title + status), the streaming transcript in the
//! main region, and a progressive-disclosure sidebar (assignee / project / dates
//! / PRs). As with every Hangar screen, the reducer ([`reduce_task_detail`]) is
//! **pure** — it folds a key press or a host [`HangarEvent`] into a new
//! [`TaskDetailState`] plus an optional [`TaskDetailIntent`] for the plugin glue
//! to act on (retry / cancel the task). No IO, no `tokio`, no socket, so every
//! transition is exhaustively unit-testable (the P4.4 RED tests in
//! `tests/transcript_reducer_test.rs`).
//!
//! The plugin holds **zero domain data of its own**
//! (`project_ainb_plugin_owns_data_plane`): the transcript is the daemon's
//! task event stream, folded into a render-state cache. [`TaskDetailState`] is
//! that cache and nothing more.
//!
//! ## Streaming append + sticky-bottom auto-scroll
//!
//! Each [`HangarEvent::TaskMessage`] (and interleaved [`HangarEvent::CommentAdded`])
//! addressed to the bound task lands at the *bottom* of the transcript in arrival
//! order. While the viewport is *stuck to the bottom* the scroll offset tracks the
//! tail so the newest line stays visible; the moment the user scrolls up (`k`)
//! sticky releases and new messages no longer move the viewport off the user's
//! position — they re-stick only by scrolling back to the bottom (`G` / `j` past
//! the end).
//!
//! ## Retry / cancel lifecycle gating
//!
//! `R` (retry) is only meaningful once the task reached a terminal state
//! (succeeded / failed); `X` (cancel) is only meaningful while it is running, and
//! opens a confirm modal (Esc aborts, Enter confirms → [`TaskDetailIntent::CancelTask`]).
//! The reducer tracks lifecycle from the task events themselves so these keys are
//! total no-ops when not applicable. `x` (delete the bound issue) opens its own
//! confirm modal the same way (Esc aborts, Enter → [`TaskDetailIntent::DeleteIssue`]);
//! it is not lifecycle-gated — the daemon rejects a delete with active tasks and
//! that rejection surfaces as a note.
//!
//! ## Collapsible thinking runs
//!
//! A long consecutive run of [`MessageKind::Thinking`] lines is a UX-§7 grouping
//! candidate: the raw transcript keeps every line, but the *visible* view folds a
//! run of [`THINKING_COLLAPSE_THRESHOLD`] or more into a single collapsed-group
//! entry so reasoning doesn't bury the prose + tool flow.

use ainb_hangar_core::acceptance::{AcceptanceCriterion, checked_count, legacy_placeholder_id};
use ainb_hangar_core::ids::TaskId;
use ainb_hangar_proto::events::{HangarEvent, IssueRow, MessageKind, TaskResult};
use ainb_hangar_proto::pr_status::{CiRollup, MergeState, Mergeable, PrStatus};
use ainb_plugin_sdk::{Cell, Color, Coord, WireBuffer};

use crate::widgets::transcript::{render_transcript, transcript_color, transcript_glyph};

/// A consecutive run of this many [`MessageKind::Thinking`] lines (or more)
/// folds into a single collapsed-group entry in the visible view (UX §7).
pub const THINKING_COLLAPSE_THRESHOLD: usize = 4;

/// Gold accent for the PR badge (matches the ainb-tui chrome CTA gold).
const BADGE_GOLD: Color = Color::rgb(255, 215, 0);
/// Muted gray for the `[o] open` keybinding hint next to the badge.
const HINT_MUTED: Color = Color::rgb(120, 120, 140);
/// The leading badge glyph + label painted before the URL (`▶ PR `).
const BADGE_PREFIX: &str = "▶ PR ";
/// The keybinding hint painted next to the URL (two-space gap + `[o] open`).
const BADGE_HINT: &str = "  [o] open";
/// Green for a passing CI rollup / a clean mergeable PR (e38.34).
const STATUS_GREEN: Color = Color::rgb(120, 220, 120);
/// Red for a failing CI rollup / a merge conflict (e38.34) — visually distinct
/// from the green pass + the muted unknown so a glance reads the state.
const STATUS_RED: Color = Color::rgb(240, 100, 100);
/// Amber for a pending (still-running) CI rollup (e38.34).
const STATUS_AMBER: Color = Color::rgb(230, 190, 90);
/// Cornflower-blue for the run's branch line (tcp T2, agents-in-a-box-ch3) —
/// distinct from the gold PR badge so the two artifacts never read as one.
const BRANCH_COLOR: Color = Color::rgb(100, 149, 237);
/// Cornflower-blue for the detail-card border (63d; the style-guide border hue).
const CARD_BORDER: Color = Color::rgb(100, 149, 237);
/// Gold for the detail-card title (63d; the style-guide title/CTA gold).
const CARD_TITLE: Color = Color::rgb(255, 215, 0);
/// Soft white for the detail-card field VALUES (63d; the style-guide body ink).
const CARD_VALUE: Color = Color::rgb(220, 220, 230);
/// Muted gray for the detail-card field LABELS (63d; the style-guide muted hue).
const CARD_LABEL: Color = Color::rgb(120, 120, 140);
/// The em-dash placeholder painted for an unset card field (63d).
const CARD_UNSET: &str = "—";
/// Selection green for the `▶` marker on the criterion under the acceptance
/// cursor (the repo-wide TUI selection colour).
const SELECTION_GREEN: Color = Color::rgb(100, 200, 100);
/// The leading glyph + label painted before the branch name (`⎇ branch `).
const BRANCH_PREFIX: &str = "⎇ branch ";
/// Accent for the comment-compose input bar (a calm emerald, distinct from the
/// gold PR badge so the two bars never read as the same control).
const COMPOSE_ACCENT: Color = Color::rgb(120, 220, 160);
/// The leading glyph + label painted before the typed comment body (`💬 `).
const COMPOSE_PREFIX: &str = "💬 ";
/// The keybinding hint painted after the caret on the compose bar.
const COMPOSE_HINT: &str = "  [enter] post  [esc] cancel";
/// The keybinding hint painted after the target on the delete-confirm bar (63l.5).
const DELETE_HINT: &str = "  [enter] confirm  [esc] cancel";

/// Where the task is in its lifecycle, derived from the task event stream.
///
/// Drives the retry / cancel key gating: retry needs a terminal state, cancel
/// needs [`TaskLifecycle::Running`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskLifecycle {
    /// Enqueued, not yet started (the default before any task event).
    Queued,
    /// Actively running (`TaskStarted` seen, no terminal event yet).
    Running,
    /// Finished successfully.
    Succeeded,
    /// Finished with a failure.
    Failed,
    /// Cancelled before completion.
    Cancelled,
}

impl TaskLifecycle {
    /// `true` when the task reached a terminal state and a retry is meaningful.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Cancelled)
    }
}

/// One line in the transcript: a typed transcript message or an interleaved
/// comment.
///
/// Both carry a body and a [`MessageKind`] lane so the renderer can colour them
/// uniformly; comments render in the slate "tool result" lane as a neutral
/// interleave (the reference renders human comments distinctly from agent prose
/// without their own taxonomy colour).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptEntry {
    kind: MessageKind,
    body: String,
    /// `true` when this entry is a human comment rather than an agent message.
    is_comment: bool,
}

impl TranscriptEntry {
    /// Build a transcript line in `kind`'s lane with `body` text. `is_comment`
    /// marks an interleaved human comment (the collapse grouping skips it). The
    /// live task stream builds these internally; the JSONL timeline parser
    /// ([`crate::widgets::jsonl_timeline`]) uses this to turn a disk transcript into
    /// the same [`ViewEntry`]s the streamed transcript renders through.
    #[must_use]
    pub const fn new(kind: MessageKind, body: String, is_comment: bool) -> Self {
        Self {
            kind,
            body,
            is_comment,
        }
    }

    /// The taxonomy lane this entry renders in.
    #[must_use]
    pub const fn kind(&self) -> MessageKind {
        self.kind
    }

    /// The line text.
    #[must_use]
    pub fn body(&self) -> &str {
        &self.body
    }

    /// `true` when this entry originated as an issue comment, not a task message.
    #[must_use]
    pub const fn is_comment(&self) -> bool {
        self.is_comment
    }
}

/// A view entry the renderer paints: either a single [`TranscriptEntry`] or a
/// collapsed run of consecutive thinking lines.
///
/// Built on demand from the raw transcript by [`TaskDetailState::visible_entries`]
/// so the raw event log is never mutated by display grouping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ViewEntry {
    /// A single transcript line rendered verbatim.
    Line(TranscriptEntry),
    /// A folded run of `count` consecutive thinking lines (UX §7 collapse).
    CollapsedThinking {
        /// How many thinking lines this group folds.
        count: usize,
    },
}

impl ViewEntry {
    /// A single rendered line in `kind`'s lane with `body` text — the shape the
    /// JSONL timeline parser emits ([`crate::widgets::jsonl_timeline`]).
    #[must_use]
    pub fn line(kind: MessageKind, body: impl Into<String>) -> Self {
        Self::Line(TranscriptEntry::new(kind, body.into(), false))
    }

    /// `true` when this is a [`ViewEntry::CollapsedThinking`] fold marker.
    #[must_use]
    pub const fn is_collapsed_group(&self) -> bool {
        matches!(self, Self::CollapsedThinking { .. })
    }
}

/// The render-state cache for the task-detail screen.
///
/// Holds the bound task id + its issue row (the header source), the lifecycle
/// derived from task events, the raw transcript in arrival order, the scroll
/// offset + sticky-bottom flag, and whether the cancel-confirm modal is open.
/// All fields are private; the renderer and tests read through accessors so the
/// "offset is always a valid transcript index" invariant stays internal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskDetailState {
    task_id: TaskId,
    issue: IssueRow,
    lifecycle: TaskLifecycle,
    transcript: Vec<TranscriptEntry>,
    /// Index of the transcript line pinned to the bottom of the viewport.
    scroll_offset: usize,
    /// While `true`, appending a message advances [`Self::scroll_offset`] to the
    /// tail so the newest line stays visible.
    stuck_to_bottom: bool,
    /// Whether the cancel-confirm modal is open.
    cancel_modal_open: bool,
    /// Whether the `x` delete-confirm modal is open (63l.5). Mutually exclusive
    /// with the cancel + compose modals; Enter confirms → [`TaskDetailIntent::DeleteIssue`],
    /// Esc aborts. The daemon guards against deleting an issue with active tasks,
    /// so the confirm always opens and a rejection surfaces as a note downstream.
    delete_modal_open: bool,
    /// The comment-compose modal buffer (e38.5). `Some(buf)` while the modal is
    /// open (`buf` is the in-progress comment text, possibly empty); `None` when
    /// closed. While open the modal captures every key as text input, so it is
    /// mutually exclusive with the scroll / retry / cancel keys.
    compose: Option<String>,
    /// The last fetched PR check + merge status (e38.34), shown on the badge next
    /// to the URL. Defaults to all-`Unknown` (rendered as a muted `…`) until a
    /// `hangar/pr_status_refresh` answers; only meaningful when [`Self::pr_url`]
    /// is `Some`. A merged status is reflected by the daemon's auto-Done move, so
    /// the plugin never transitions on its own.
    pr_status: PrStatus,
    /// The run's worktree branch (`ainb/<slug>`) the task committed on (tcp T2,
    /// agents-in-a-box-ch3), or `None` when the run made no commits / the detail
    /// was opened without a per-run branch (e.g. from the issue list). Seeded from
    /// the opening task card's [`TaskCardRow::branch`](ainb_hangar_proto::events::TaskCardRow);
    /// rendered as a branch line under the PR badge (progressive disclosure).
    branch: Option<String>,
    /// Which acceptance criterion the `a` cursor sits on (multica parity
    /// #11-rest), or `None` when none is selected yet. `t` toggles the selected
    /// one; both keys are no-ops on an issue with no criteria.
    acceptance_cursor: Option<usize>,
}

/// The all-`Unknown` PR status, const-constructible so [`TaskDetailState::new`]
/// stays a `const fn`. (`PrStatus::default()` is not `const`.)
const UNKNOWN_PR_STATUS: PrStatus = PrStatus {
    ci: CiRollup::Unknown,
    mergeable: Mergeable::Unknown,
    state: MergeState::Unknown,
};

impl TaskDetailState {
    /// A fresh task-detail state bound to `task_id` for `issue`, empty transcript,
    /// stuck to the bottom, no modal, lifecycle [`TaskLifecycle::Queued`].
    #[must_use]
    pub const fn new(task_id: TaskId, issue: IssueRow) -> Self {
        Self {
            task_id,
            issue,
            lifecycle: TaskLifecycle::Queued,
            transcript: Vec::new(),
            scroll_offset: 0,
            stuck_to_bottom: true,
            cancel_modal_open: false,
            delete_modal_open: false,
            compose: None,
            pr_status: UNKNOWN_PR_STATUS,
            branch: None,
            acceptance_cursor: None,
        }
    }

    /// The bound task id.
    #[must_use]
    pub const fn task_id(&self) -> &TaskId {
        &self.task_id
    }

    /// The issue row backing the header.
    #[must_use]
    pub const fn issue(&self) -> &IssueRow {
        &self.issue
    }

    /// The PR URL captured for this task's issue (P9.1 capture, P9.2 surface), or
    /// `None` when no task on the issue opened a PR. Drives the gold PR badge and
    /// gates the `o` (open-in-browser) key — when this is `None` the badge is
    /// absent and `o` is a no-op (no silent open of nothing).
    #[must_use]
    pub fn pr_url(&self) -> Option<&str> {
        self.issue.pr_url.as_deref()
    }

    /// The last fetched PR check + merge status (e38.34). All-`Unknown` until a
    /// `hangar/pr_status_refresh` answers; the badge renders it next to the URL.
    #[must_use]
    pub const fn pr_status(&self) -> PrStatus {
        self.pr_status
    }

    /// The run's `ainb/<slug>` worktree branch (tcp T2, agents-in-a-box-ch3), or
    /// `None` when the run made no commits / the detail carries no per-run branch.
    /// The detail view renders it as a branch line under the PR badge.
    #[must_use]
    pub fn branch(&self) -> Option<&str> {
        self.branch.as_deref()
    }

    /// Seed the run's branch when opening the detail from a task card carrying one
    /// (agents-in-a-box-ch3). `None` clears it (a run with no committed branch).
    pub fn set_branch(&mut self, branch: Option<String>) {
        self.branch = branch;
    }

    /// Apply a freshly fetched PR status (e38.34) — the reducer calls this when a
    /// `hangar/pr_status_refresh` reply lands so the badge re-renders the CI +
    /// merge state on the next paint.
    pub const fn set_pr_status(&mut self, status: PrStatus) {
        self.pr_status = status;
    }

    /// The current lifecycle.
    #[must_use]
    pub const fn lifecycle(&self) -> TaskLifecycle {
        self.lifecycle
    }

    /// Iterate the raw transcript in arrival order.
    pub fn transcript(&self) -> impl Iterator<Item = &TranscriptEntry> {
        self.transcript.iter()
    }

    /// Number of raw transcript lines (collapsing does not shrink this).
    #[must_use]
    pub const fn transcript_len(&self) -> usize {
        self.transcript.len()
    }

    /// The scroll offset (index of the line pinned to the viewport bottom).
    #[must_use]
    pub const fn scroll_offset(&self) -> usize {
        self.scroll_offset
    }

    /// Whether the viewport is stuck to the transcript bottom (auto-scroll on).
    #[must_use]
    pub const fn is_stuck_to_bottom(&self) -> bool {
        self.stuck_to_bottom
    }

    /// Whether the cancel-confirm modal is open.
    #[must_use]
    pub const fn cancel_modal_open(&self) -> bool {
        self.cancel_modal_open
    }

    /// Whether the `x` delete-confirm modal is open (63l.5).
    #[must_use]
    pub const fn delete_modal_open(&self) -> bool {
        self.delete_modal_open
    }

    /// The comment-compose buffer when the compose modal is open (e38.5), or
    /// `None` when it is closed. `Some("")` is an open-but-empty modal.
    #[must_use]
    pub fn compose_buffer(&self) -> Option<&str> {
        self.compose.as_deref()
    }

    /// The display view: raw entries with long consecutive thinking runs folded
    /// into [`ViewEntry::CollapsedThinking`] markers (UX §7). Pure — derived from
    /// the raw transcript, never mutating it.
    #[must_use]
    pub fn visible_entries(&self) -> Vec<ViewEntry> {
        let mut out = Vec::new();
        let mut i = 0;
        while i < self.transcript.len() {
            let entry = &self.transcript[i];
            if entry.kind == MessageKind::Thinking && !entry.is_comment {
                // Measure the consecutive thinking run starting at `i`.
                let run_start = i;
                while i < self.transcript.len()
                    && self.transcript[i].kind == MessageKind::Thinking
                    && !self.transcript[i].is_comment
                {
                    i += 1;
                }
                let count = i - run_start;
                if count >= THINKING_COLLAPSE_THRESHOLD {
                    out.push(ViewEntry::CollapsedThinking { count });
                } else {
                    for e in &self.transcript[run_start..i] {
                        out.push(ViewEntry::Line(e.clone()));
                    }
                }
            } else {
                out.push(ViewEntry::Line(entry.clone()));
                i += 1;
            }
        }
        out
    }
}

/// An input the task-detail reducer folds into [`TaskDetailState`].
///
/// Key presses arrive as [`TaskDetailEvent::Key`]; `Esc` is modelled separately
/// because it is not a printable char (it aborts the cancel modal); host stream
/// events arrive wrapped in [`TaskDetailEvent::Event`].
// Reduction enum: `Event(HangarEvent)` dominates the size, the rest are scalar
// key inputs. Short-lived, reducer-folded, not a hot allocation path — left
// unboxed for consistency with the other screen reducers.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskDetailEvent {
    /// A printable key was pressed (`'j'`, `'R'`, `'X'`, `'\n'`, …).
    Key(char),
    /// The Escape key was pressed (aborts the cancel-confirm modal).
    Esc,
    /// A domain event arrived on the subscribed `task:{id}` stream.
    Event(HangarEvent),
}

/// A side-effect the plugin glue performs after a task-detail [`reduce_task_detail`].
///
/// The reducer is pure, so it surfaces the *desire* to mutate the task as an
/// intent and lets the IO layer fire the RPC.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskDetailIntent {
    /// Re-run the (terminal) task (`R`).
    RetryTask(TaskId),
    /// Cancel the (running) task, confirmed in the modal (`X` then Enter).
    CancelTask(TaskId),
    /// Delete the bound issue, confirmed in the `x` modal (`x` then Enter, 63l.5).
    /// The plugin glue fires `hangar/issue_delete` over the same deferred seam the
    /// issue-list `x` uses, then navigates back to the issue list. Carries the
    /// bound issue id.
    DeleteIssue(ainb_hangar_core::ids::IssueId),
    /// Post a comment on the bound issue (`c`, type, Enter) — the plugin glue
    /// fires `hangar/comment_add` over the daemon socket (e38.5). Carries the
    /// issue the comment is for and the (non-empty) typed body.
    AddComment {
        /// The issue the comment is posted on (the bound `issue.id`).
        issue_id: ainb_hangar_core::ids::IssueId,
        /// The typed comment body (guaranteed non-empty by the reducer).
        body: String,
    },
    /// Tick / untick ONE acceptance criterion (`a` to select, `t` to toggle;
    /// multica parity #11-rest). The plugin glue fires
    /// `hangar/issue_criterion_set`; the daemon's `IssueUpdated` push refreshes
    /// the card, so the glue only surfaces an error note on failure.
    SetCriterionChecked {
        /// The issue the criterion belongs to (the bound `issue.id`).
        issue_id: ainb_hangar_core::ids::IssueId,
        /// The stable criterion id (`ac-…`) — never a positional index, so a
        /// future reorder cannot tick the wrong criterion.
        criterion_id: String,
        /// The state to set (the inverse of what is currently rendered).
        checked: bool,
    },
}

/// The result of folding one [`TaskDetailEvent`] into a [`TaskDetailState`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskDetailReduction {
    /// The next task-detail state.
    pub state: TaskDetailState,
    /// A side-effect for the plugin glue, if any.
    pub intent: Option<TaskDetailIntent>,
}

/// Fold one [`TaskDetailEvent`] into `state`, returning the next state and any
/// [`TaskDetailIntent`]. Pure: no IO, no mutation of the input `state`.
#[must_use]
pub fn reduce_task_detail(state: &TaskDetailState, ev: TaskDetailEvent) -> TaskDetailReduction {
    match ev {
        TaskDetailEvent::Key(c) => reduce_key(state, c),
        TaskDetailEvent::Esc => reduce_esc(state),
        TaskDetailEvent::Event(event) => fold_event(state, event),
    }
}

/// Handle a printable key. When a modal is open it captures input: the
/// compose modal (e38.5) eats every key as text (Enter submits, Backspace
/// deletes); the cancel modal captures Enter (confirm) and ignores the rest.
fn reduce_key(state: &TaskDetailState, c: char) -> TaskDetailReduction {
    // The compose modal owns input while open: type / delete / submit.
    if state.compose.is_some() {
        return reduce_compose_key(state, c);
    }
    if state.cancel_modal_open {
        return match c {
            '\n' | '\r' => confirm_cancel(state),
            // Any other key while the modal is open does nothing (Esc aborts via
            // the dedicated event).
            _ => unchanged(state),
        };
    }
    if state.delete_modal_open {
        return match c {
            '\n' | '\r' => confirm_delete(state),
            // Any other key while the modal is open does nothing (Esc aborts via
            // the dedicated event).
            _ => unchanged(state),
        };
    }
    match c {
        'j' => scroll_down(state),
        'k' => scroll_up(state),
        // Open the comment-compose modal (`c`); captures input until Enter/Esc.
        'c' => open_compose(state),
        // Retry only once terminal; otherwise a no-op.
        'R' if state.lifecycle.is_terminal() => with_intent(
            state.clone(),
            TaskDetailIntent::RetryTask(state.task_id.clone()),
        ),
        // Cancel only while running; opens the confirm modal (no intent yet).
        'X' if state.lifecycle == TaskLifecycle::Running => open_cancel_modal(state),
        // Delete the bound issue (`x`); opens the confirm modal (no intent yet).
        // Not lifecycle-gated: the daemon rejects a delete with active tasks and
        // that rejection surfaces as a note, so the confirm always opens.
        'x' => open_delete_modal(state),
        // #11-rest: `a` walks the acceptance cursor, `t` toggles the selected
        // criterion. Both are no-ops on an issue without criteria.
        'a' => advance_acceptance_cursor(state),
        't' => toggle_selected_criterion(state),
        _ => unchanged(state),
    }
}

/// Move the acceptance cursor to the next criterion, wrapping; select the FIRST
/// when none is selected. A no-op when the issue has no criteria.
fn advance_acceptance_cursor(state: &TaskDetailState) -> TaskDetailReduction {
    let len = acceptance_view(state.issue()).len();
    if len == 0 {
        return unchanged(state);
    }
    let mut next = state.clone();
    next.acceptance_cursor = Some(match state.acceptance_cursor {
        Some(idx) => (idx + 1) % len,
        None => 0,
    });
    TaskDetailReduction {
        state: next,
        intent: None,
    }
}

/// Toggle the criterion under the acceptance cursor, emitting
/// [`TaskDetailIntent::SetCriterionChecked`] for its STABLE id. A no-op when
/// nothing is selected or the cursor has fallen off a shortened list.
fn toggle_selected_criterion(state: &TaskDetailState) -> TaskDetailReduction {
    let criteria = acceptance_view(state.issue());
    let Some(idx) = state.acceptance_cursor else {
        return unchanged(state);
    };
    let Some(criterion) = criteria.get(idx) else {
        return unchanged(state);
    };
    with_intent(
        state.clone(),
        TaskDetailIntent::SetCriterionChecked {
            issue_id: state.issue.id.clone(),
            criterion_id: criterion.id.clone(),
            checked: !criterion.checked,
        },
    )
}

/// Compose-modal key handling (e38.5): Enter submits a non-empty body (closing
/// the modal + emitting [`TaskDetailIntent::AddComment`]), Backspace deletes the
/// last char, any other printable char appends. Enter on an empty/whitespace
/// body is a no-op that keeps the modal open (never an empty comment).
fn reduce_compose_key(state: &TaskDetailState, c: char) -> TaskDetailReduction {
    let mut buf = state.compose.clone().unwrap_or_default();
    match c {
        '\n' | '\r' => {
            if buf.trim().is_empty() {
                // Empty body: keep the modal open, submit nothing.
                return unchanged(state);
            }
            let mut next = state.clone();
            next.compose = None;
            return with_intent(
                next,
                TaskDetailIntent::AddComment {
                    issue_id: state.issue.id.clone(),
                    body: buf,
                },
            );
        }
        '\u{8}' | '\u{7f}' => {
            buf.pop();
        }
        other => buf.push(other),
    }
    let mut next = state.clone();
    next.compose = Some(buf);
    no_intent(next)
}

/// Open the comment-compose modal with an empty buffer (`c`).
fn open_compose(state: &TaskDetailState) -> TaskDetailReduction {
    let mut next = state.clone();
    next.compose = Some(String::new());
    no_intent(next)
}

/// Handle Esc: close whichever modal is open (compose discards its draft, cancel
/// aborts); otherwise a no-op (the router owns leaving the screen).
fn reduce_esc(state: &TaskDetailState) -> TaskDetailReduction {
    if state.compose.is_some() {
        let mut next = state.clone();
        next.compose = None;
        no_intent(next)
    } else if state.cancel_modal_open {
        let mut next = state.clone();
        next.cancel_modal_open = false;
        no_intent(next)
    } else if state.delete_modal_open {
        let mut next = state.clone();
        next.delete_modal_open = false;
        no_intent(next)
    } else {
        unchanged(state)
    }
}

/// Open the cancel-confirm modal (does not emit an intent yet).
fn open_cancel_modal(state: &TaskDetailState) -> TaskDetailReduction {
    let mut next = state.clone();
    next.cancel_modal_open = true;
    no_intent(next)
}

/// Confirm the cancel modal: close it and emit the cancel intent.
fn confirm_cancel(state: &TaskDetailState) -> TaskDetailReduction {
    let mut next = state.clone();
    next.cancel_modal_open = false;
    with_intent(next, TaskDetailIntent::CancelTask(state.task_id.clone()))
}

/// Open the `x` delete-confirm modal (63l.5; does not emit an intent yet).
fn open_delete_modal(state: &TaskDetailState) -> TaskDetailReduction {
    let mut next = state.clone();
    next.delete_modal_open = true;
    no_intent(next)
}

/// Confirm the delete modal: close it and emit the delete intent for the bound
/// issue (63l.5).
fn confirm_delete(state: &TaskDetailState) -> TaskDetailReduction {
    let mut next = state.clone();
    next.delete_modal_open = false;
    with_intent(next, TaskDetailIntent::DeleteIssue(state.issue.id.clone()))
}

/// Scroll the viewport up one line, releasing sticky-bottom.
fn scroll_up(state: &TaskDetailState) -> TaskDetailReduction {
    let mut next = state.clone();
    next.stuck_to_bottom = false;
    next.scroll_offset = next.scroll_offset.saturating_sub(1);
    no_intent(next)
}

/// Scroll the viewport down one line; re-sticks to the bottom on reaching the
/// tail.
fn scroll_down(state: &TaskDetailState) -> TaskDetailReduction {
    let mut next = state.clone();
    let last = next.transcript.len().saturating_sub(1);
    if next.scroll_offset >= last {
        next.scroll_offset = last;
        next.stuck_to_bottom = true;
    } else {
        next.scroll_offset += 1;
        if next.scroll_offset >= last {
            next.stuck_to_bottom = true;
        }
    }
    no_intent(next)
}

/// Fold a host [`HangarEvent`] into the cache. Only events addressed to the bound
/// task affect the transcript / lifecycle; everything else is ignored (no
/// cross-talk between task-detail subscriptions).
fn fold_event(state: &TaskDetailState, event: HangarEvent) -> TaskDetailReduction {
    let mut next = state.clone();
    match event {
        HangarEvent::TaskMessage {
            task_id,
            kind,
            body,
        } if task_id == state.task_id => {
            push_entry(
                &mut next,
                TranscriptEntry {
                    kind,
                    body,
                    is_comment: false,
                },
            );
        }
        // Comments interleave chronologically in the slate (tool-result) lane.
        HangarEvent::CommentAdded(comment) if comment.issue_id == state.issue.id => {
            push_entry(
                &mut next,
                TranscriptEntry {
                    kind: MessageKind::ToolResult,
                    body: comment.body,
                    is_comment: true,
                },
            );
        }
        HangarEvent::TaskStarted { task_id, .. } if task_id == state.task_id => {
            next.lifecycle = TaskLifecycle::Running;
        }
        HangarEvent::TaskFinished {
            task_id, result, ..
        } if task_id == state.task_id => {
            next.lifecycle = match result {
                TaskResult::Success => TaskLifecycle::Succeeded,
                TaskResult::Failure => TaskLifecycle::Failed,
                TaskResult::Cancelled => TaskLifecycle::Cancelled,
            };
        }
        // Progress, presence, issue events, and events for other tasks don't
        // change this screen.
        _ => {}
    }
    no_intent(next)
}

/// Append `entry` to the transcript, advancing the scroll offset to the tail
/// when stuck to the bottom (auto-scroll).
fn push_entry(state: &mut TaskDetailState, entry: TranscriptEntry) {
    state.transcript.push(entry);
    if state.stuck_to_bottom {
        state.scroll_offset = state.transcript.len().saturating_sub(1);
    }
}

/// A reduction that changes state but emits no intent.
const fn no_intent(state: TaskDetailState) -> TaskDetailReduction {
    TaskDetailReduction {
        state,
        intent: None,
    }
}

/// A reduction carrying `intent` alongside `state`.
const fn with_intent(state: TaskDetailState, intent: TaskDetailIntent) -> TaskDetailReduction {
    TaskDetailReduction {
        state,
        intent: Some(intent),
    }
}

/// A no-op reduction: state cloned unchanged, no intent.
fn unchanged(state: &TaskDetailState) -> TaskDetailReduction {
    no_intent(state.clone())
}

// ---------------------------------------------------------------------------
// Width-aware rendering
// ---------------------------------------------------------------------------

/// Render the task-detail screen into `buf` between rows `top` and `bottom`.
///
/// Three-region layout (width-aware, derived from `area_w`): a one-row header
/// (issue title + status), the transcript filling the main region, and a
/// right-hand sidebar (progressive disclosure — see [`crate::widgets::sidebar`]).
/// The transcript is the dominant region; the sidebar takes a fixed cap on the
/// right that collapses away under narrow widths.
///
/// At the P4.4 GREEN bar the rendering is linear (no virtualisation): the visible
/// view ([`TaskDetailState::visible_entries`]) is painted top-down. The
/// REFACTOR note in `P4.md` defers virtualisation to >500-message buffers.
pub fn render_task_detail(
    buf: &mut WireBuffer,
    area_w: u16,
    top: u16,
    bottom: u16,
    state: &TaskDetailState,
) {
    // 63d: the issue DETAIL CARD spans the top of the area, so even a never-run
    // issue reads as a real card. The PR badge / branch / transcript start on the
    // first row BELOW it (+ its optional `Runs:` line).
    let card_bottom = render_detail_card(buf, area_w, top, bottom, state);

    // The PR badge (P9.2) takes the first row below the card when present,
    // pushing the transcript + sidebar down one row. When absent there is NO
    // badge row at all (the layout shifts up) — never a `PR: none` placeholder.
    let mut body_top = state.pr_url().map_or(card_bottom, |url| {
        render_pr_badge(buf, area_w, card_bottom, url, state.pr_status());
        card_bottom.saturating_add(1)
    });

    // The run's branch line (tcp T2, agents-in-a-box-ch3) sits right under the PR
    // badge (or at the top when there is no PR). Progressive disclosure, exactly
    // like the badge: a run with no committed branch renders NO row (the
    // transcript shifts up), never a `branch: none` placeholder.
    if let Some(branch) = state.branch() {
        if body_top < bottom {
            render_branch_row(buf, area_w, body_top, branch);
            body_top = body_top.saturating_add(1);
        }
    }

    // The compose modal (e38.5) / delete-confirm modal (63l.5), when open, take
    // the bottom row as a bar, shrinking the transcript region by one row so the
    // two never overlap. They are mutually exclusive (both capture input).
    let body_bottom = if state.compose.is_some() {
        let bar_row = bottom.saturating_sub(1);
        render_compose_bar(buf, area_w, bar_row, state.compose.as_deref().unwrap_or(""));
        bar_row
    } else if state.delete_modal_open {
        let bar_row = bottom.saturating_sub(1);
        render_delete_bar(buf, area_w, bar_row, state.issue());
        bar_row
    } else {
        bottom
    };

    // Sidebar takes a right-hand cap; it collapses when the area is too narrow
    // to leave the transcript a usable column.
    let sidebar_w: u16 = if area_w >= 60 { 24 } else { 0 };
    let main_w = area_w.saturating_sub(sidebar_w);

    // The transcript paints the visible (collapsed) view linearly.
    render_transcript(buf, main_w, body_top, body_bottom, &state.visible_entries());

    if sidebar_w > 0 {
        let sidebar_x = main_w;
        crate::widgets::sidebar::render_sidebar(
            buf,
            sidebar_x,
            body_top,
            body_bottom,
            sidebar_w,
            &state.issue,
        );
    }
}

/// Paint the single-row comment-compose input bar at `(0, row)` (e38.5):
/// `💬 <typed body>▏` in the compose accent followed by a muted
/// `[enter] post  [esc] cancel` keybinding hint (hint-near-control). Clipped by
/// **chars** at `area_w` (multi-byte safe) so a long draft truncates cleanly.
fn render_compose_bar(buf: &mut WireBuffer, area_w: u16, row: u16, body: &str) {
    let mut cx = 0u16;
    cx = put_clipped(buf, cx, row, COMPOSE_PREFIX, COMPOSE_ACCENT, area_w);
    cx = put_clipped(buf, cx, row, body, COMPOSE_ACCENT, area_w);
    // A block caret so the cursor position is visible while typing.
    cx = put_clipped(buf, cx, row, "▏", COMPOSE_ACCENT, area_w);
    let _ = put_clipped(buf, cx, row, COMPOSE_HINT, HINT_MUTED, area_w);
}

/// Paint the single-row delete-confirm bar at `(0, row)` (63l.5):
/// `🗑 delete <HGR-n · title>?` in the destructive red followed by a muted
/// `[enter] confirm  [esc] cancel` keybinding hint. Clipped by **chars** at
/// `area_w` (multi-byte safe) so a long title truncates cleanly. Red because the
/// delete is irreversible — the bar makes the target unmistakable before Enter.
fn render_delete_bar(buf: &mut WireBuffer, area_w: u16, row: u16, issue: &IssueRow) {
    let prompt = issue.display_id.as_ref().map_or_else(
        || format!("🗑 delete {}?", issue.title),
        |d| format!("🗑 delete {d} · {}?", issue.title),
    );
    let mut cx = 0u16;
    cx = put_clipped(buf, cx, row, &prompt, STATUS_RED, area_w);
    let _ = put_clipped(buf, cx, row, DELETE_HINT, HINT_MUTED, area_w);
}

/// Paint the single-row PR badge at `(0, row)`: `▶ PR <url>` in gold, then the
/// CI rollup and merge status (e38.34), then a muted `[o] open` keybinding hint
/// (hint-near-control).
///
/// The status reads in two colour-coded segments right after the URL:
/// - **CI**: ` CI ✓` (green pass) / ` CI ✗` (red fail) / ` CI …` (amber pending /
///   muted unknown) — so a glance distinguishes a green build from a broken one.
/// - **mergeable**: ` ✓ mergeable` (green) / ` ✗ CONFLICT` (red) — a conflict is
///   loud + red, never the same colour as a clean merge. An `Unknown` mergeable
///   (GitHub still computing) paints nothing, never a false token.
///
/// The whole row is clipped at `area_w` by **chars** (never bytes —
/// `reference_rust_utf8_truncate_trap`) so a narrow pane truncates without
/// panicking on a multi-byte boundary; a tight width drops the trailing segments
/// first (URL → CI → mergeable → hint), keeping the most-load-bearing data left.
fn render_pr_badge(buf: &mut WireBuffer, area_w: u16, row: u16, url: &str, status: PrStatus) {
    let mut cx = 0u16;
    cx = put_clipped(buf, cx, row, BADGE_PREFIX, BADGE_GOLD, area_w);
    cx = put_clipped(buf, cx, row, url, BADGE_GOLD, area_w);
    let (ci_label, ci_color) = ci_segment(status.ci);
    cx = put_clipped(buf, cx, row, ci_label, ci_color, area_w);
    if let Some((label, color)) = mergeable_segment(status.mergeable) {
        cx = put_clipped(buf, cx, row, label, color, area_w);
    }
    let _ = put_clipped(buf, cx, row, BADGE_HINT, HINT_MUTED, area_w);
}

/// Paint the single-row run-branch line at `(0, row)`: `⎇ branch <name>` in
/// cornflower-blue (tcp T2, agents-in-a-box-ch3), so a finished run's durable
/// `ainb/<slug>` branch reads in the detail view exactly as it does on the Kanban
/// card. Clipped by **chars** at `area_w` (multi-byte safe —
/// `reference_rust_utf8_truncate_trap`) so a narrow pane truncates without
/// panicking on a multi-byte boundary.
fn render_branch_row(buf: &mut WireBuffer, area_w: u16, row: u16, branch: &str) {
    let mut cx = 0u16;
    cx = put_clipped(buf, cx, row, BRANCH_PREFIX, BRANCH_COLOR, area_w);
    let _ = put_clipped(buf, cx, row, branch, BRANCH_COLOR, area_w);
}

/// The CI rollup badge segment: ` CI <glyph>` + its colour (e38.34).
///
/// Always painted (the CI axis is the headline status), with `Unknown` /
/// `Pending` both showing a muted-vs-amber `…` so the badge reads "status
/// loading" rather than blank.
const fn ci_segment(ci: CiRollup) -> (&'static str, Color) {
    match ci {
        CiRollup::Pass => (" CI ✓", STATUS_GREEN),
        CiRollup::Fail => (" CI ✗", STATUS_RED),
        CiRollup::Pending => (" CI …", STATUS_AMBER),
        CiRollup::Unknown => (" CI …", HINT_MUTED),
    }
}

/// The mergeable badge segment: ` ✓ mergeable` / ` ✗ CONFLICT` + colour, or `None`
/// when GitHub has not finished computing mergeability (`Unknown`) so the badge
/// never claims a false state (e38.34).
const fn mergeable_segment(m: Mergeable) -> Option<(&'static str, Color)> {
    match m {
        Mergeable::Mergeable => Some((" ✓ mergeable", STATUS_GREEN)),
        Mergeable::Conflicting => Some((" ✗ CONFLICT", STATUS_RED)),
        Mergeable::Unknown => None,
    }
}

/// Write `s` at `(x, row)` in `color`, clipping by **chars** at column `right`
/// (exclusive). Returns the next free column. Multi-byte safe.
fn put_clipped(buf: &mut WireBuffer, x: u16, row: u16, s: &str, color: Color, right: u16) -> u16 {
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

/// Render the issue DETAIL CARD at the top of the task-detail screen (63d): a
/// cornflower-bordered box carrying the issue's status / priority / created /
/// assignee / agent / repo / branches / labels / description, so a never-run
/// issue reads as a real card instead of an almost-empty page. Returns the first
/// row BELOW the card (+ its optional `Runs:` history line) — the caller starts
/// the PR badge / branch / transcript there.
///
/// Width-aware: every value is clipped by **chars** (never bytes — the utf8
/// truncate trap this file documents), and the description wraps to the inner
/// width, capped so the card always leaves room for the transcript below.
///
/// HEIGHT CONTRACT: the card budgets itself to `available - RESERVED_BELOW`
/// (badge + branch + a minimum transcript region) and paints nothing (returns
/// `top` unchanged) when that budget cannot fit a legible card — a short
/// viewport (e.g. the 8-row snapshot panes) keeps the pre-card layout intact,
/// with the PR badge / branch / transcript exactly where they always were.
fn render_detail_card(
    buf: &mut WireBuffer,
    area_w: u16,
    top: u16,
    bottom: u16,
    state: &TaskDetailState,
) -> u16 {
    let issue = state.issue();
    let card_w = area_w;
    let available = bottom.saturating_sub(top);

    // The rows the card must LEAVE BELOW itself: the PR badge (1) + branch line
    // (1) + a minimum legible transcript region (4). The card's whole budget is
    // what remains after this reservation — the transcript feed is the screen's
    // job, the card is context, so the card yields, never the feed.
    const RESERVED_BELOW: u16 = 6;
    // The card's fixed chrome: top border + 4 field rows + divider + bottom
    // border = 7 rows; the description adds ≥1 more, a run history line 1 more.
    const CARD_FIXED_ROWS: u16 = 7;
    /// The description never exceeds this many wrapped lines, so even a tall
    /// viewport keeps the card compact (≈40% of a 30-row pane at worst).
    const DESC_MAX_LINES: u16 = 4;

    let runs_line = issue.run_count > 0;
    let runs_rows = u16::from(runs_line);
    let budget = available.saturating_sub(RESERVED_BELOW);
    let min_needed = CARD_FIXED_ROWS + 1 + runs_rows;
    // Too narrow, or the viewport can't fit a legible card AND the reserved
    // badge/branch/transcript region — skip the card entirely (the pre-card
    // layout), never squeeze the transcript out.
    if card_w < 16 || budget < min_needed {
        return top;
    }
    let inner_right = card_w.saturating_sub(2); // content clip column (exclusive)

    // Wrap the description (or the "no description" placeholder) to the inner
    // width, capped to the row budget left after the fixed chrome + runs line —
    // and to DESC_MAX_LINES absolutely — so the card never eats the transcript.
    let inner_w = card_w.saturating_sub(4).max(1) as usize;
    let desc_text = issue.description.as_deref().unwrap_or("no description");
    let desc_budget = budget
        .saturating_sub(CARD_FIXED_ROWS)
        .saturating_sub(runs_rows)
        .clamp(1, DESC_MAX_LINES) as usize;
    let desc_lines = wrap_chars(desc_text, inner_w, desc_budget);
    let mut row = top;

    // --- top border with the gold title overlaid ---
    draw_card_hline(buf, row, card_w, '╭', '╮');
    let title = match &issue.display_id {
        Some(d) => format!(" 📋 {} · {}", d, issue.title),
        None => format!(" 📋 {}", issue.title),
    };
    put_clipped(buf, 2, row, &title, CARD_TITLE, inner_right);
    row = row.saturating_add(1);

    // --- Status / Priority / Created ---
    card_field_row(
        buf,
        card_w,
        row,
        &[
            ("Status: ", CARD_LABEL),
            (&issue.state, CARD_VALUE),
            ("   Priority: ", CARD_LABEL),
            (&priority_p_label(issue.priority), CARD_VALUE),
            ("   Created: ", CARD_LABEL),
            (&fmt_card_date(issue.created_at), CARD_VALUE),
        ],
    );
    row = row.saturating_add(1);

    // --- Assignee / Agent ---
    let assignee = issue.assignee.as_deref().unwrap_or("unassigned");
    let agent = issue.agent.as_deref().unwrap_or(CARD_UNSET);
    card_field_row(
        buf,
        card_w,
        row,
        &[
            ("Assignee: ", CARD_LABEL),
            (assignee, CARD_VALUE),
            ("   Agent: ", CARD_LABEL),
            (agent, CARD_VALUE),
        ],
    );
    row = row.saturating_add(1);

    // --- Repo / Source → Target ---
    let repo = issue.repo_ref.as_deref().unwrap_or(CARD_UNSET);
    let source = issue.source_branch.as_deref().unwrap_or(CARD_UNSET);
    let target = issue.target_branch.as_deref().unwrap_or(CARD_UNSET);
    card_field_row(
        buf,
        card_w,
        row,
        &[
            ("Repo: ", CARD_LABEL),
            (repo, CARD_VALUE),
            ("   Source: ", CARD_LABEL),
            (source, CARD_VALUE),
            (" → Target: ", CARD_LABEL),
            (target, CARD_VALUE),
        ],
    );
    row = row.saturating_add(1);

    // --- Labels / Due (0014: the deadline shares the Labels row, so the card
    //     height is unchanged and an issue's whole triage state reads in one
    //     glance: Priority above, Labels + Due here) ---
    let labels = if issue.labels.is_empty() {
        CARD_UNSET.to_string()
    } else {
        issue.labels.iter().map(|l| format!("[{l}]")).collect::<Vec<_>>().join(" ")
    };
    let due = issue.due_date.map_or_else(|| CARD_UNSET.to_string(), fmt_card_date);
    card_field_row(
        buf,
        card_w,
        row,
        &[
            ("Labels: ", CARD_LABEL),
            (&labels, CARD_VALUE),
            ("   Due: ", CARD_LABEL),
            (&due, CARD_VALUE),
        ],
    );
    row = row.saturating_add(1);

    // --- Linked upstream issue (0043): only when the card links one, so an
    //     unlinked card reads unchanged. `⧉` marks the traceability ref. ---
    if let Some(link) = issue.external_ref.as_deref().filter(|l| !l.trim().is_empty()) {
        card_field_row(
            buf,
            card_w,
            row,
            &[
                ("Linked: ", CARD_LABEL),
                ("⧉ ", CARD_LABEL),
                (link, CARD_VALUE),
            ],
        );
        row = row.saturating_add(1);
    }

    // --- Acceptance criteria (0048 + #11-rest): a `Acceptance: <done>/<total>`
    //     header then one `☑`/`☐ <criterion>` line per element, rendered ONLY when
    //     non-empty so an issue without them reads unchanged (mirrors the Linked
    //     conditional). A checked line is dimmed to CARD_LABEL so the eye lands on
    //     what is still outstanding. ---
    let criteria = acceptance_view(issue);
    if !criteria.is_empty() {
        let header = format!(
            "Acceptance: {}/{}",
            checked_count(&criteria),
            criteria.len()
        );
        card_field_row(buf, card_w, row, &[(&header, CARD_LABEL)]);
        row = row.saturating_add(1);
        for (idx, criterion) in criteria.iter().enumerate() {
            let marker = if state.acceptance_cursor == Some(idx) {
                "▶ "
            } else {
                "  "
            };
            let glyph = if criterion.checked { "☑ " } else { "☐ " };
            let text_style = if criterion.checked {
                CARD_LABEL
            } else {
                CARD_VALUE
            };
            card_field_row(
                buf,
                card_w,
                row,
                &[
                    (marker, SELECTION_GREEN),
                    (glyph, CARD_LABEL),
                    (&criterion.text, text_style),
                ],
            );
            row = row.saturating_add(1);
        }
    }

    // --- Context references (0048): a header row then one `⧉ <ref>` line per
    //     element, rendered ONLY when non-empty. ---
    if !issue.context_refs.is_empty() {
        card_field_row(buf, card_w, row, &[("Context:", CARD_LABEL)]);
        row = row.saturating_add(1);
        for reference in &issue.context_refs {
            card_field_row(
                buf,
                card_w,
                row,
                &[("  ⧉ ", CARD_LABEL), (reference, CARD_VALUE)],
            );
            row = row.saturating_add(1);
        }
    }

    // --- divider ---
    draw_card_divider(buf, row, card_w);
    row = row.saturating_add(1);

    // --- description (wrapped) ---
    for line in &desc_lines {
        card_field_row(buf, card_w, row, &[(line, CARD_VALUE)]);
        row = row.saturating_add(1);
    }

    // --- bottom border ---
    draw_card_hline(buf, row, card_w, '╰', '╯');
    row = row.saturating_add(1);

    // --- Runs history line (below the card, only when the issue has run) ---
    if runs_line {
        let when = issue.last_run_at.map(fmt_card_date);
        let runs = match (&issue.last_run_status, when) {
            (Some(status), Some(w)) => {
                format!("  Runs: {} (last: {status} {w})", issue.run_count)
            }
            (Some(status), None) => format!("  Runs: {} (last: {status})", issue.run_count),
            _ => format!("  Runs: {}", issue.run_count),
        };
        put_clipped(buf, 0, row, &runs, CARD_LABEL, card_w);
        row = row.saturating_add(1);
    }

    row
}

/// Draw a card horizontal border row (top or bottom) spanning the full width,
/// with the given corner glyphs, in the cornflower border colour (63d).
fn draw_card_hline(buf: &mut WireBuffer, row: u16, card_w: u16, left: char, right: char) {
    let mut s = String::new();
    s.push(left);
    for _ in 1..card_w.saturating_sub(1) {
        s.push('─');
    }
    if card_w >= 2 {
        s.push(right);
    }
    put_clipped(buf, 0, row, &s, CARD_BORDER, card_w);
}

/// Draw the card's inner divider row: `│` edges with a dashed fill between (63d).
fn draw_card_divider(buf: &mut WireBuffer, row: u16, card_w: u16) {
    let mut s = String::new();
    s.push('│');
    for _ in 1..card_w.saturating_sub(1) {
        s.push('─');
    }
    if card_w >= 2 {
        s.push('│');
    }
    put_clipped(buf, 0, row, &s, CARD_BORDER, card_w);
}

/// The structured acceptance criteria to render for `issue`.
///
/// Prefers the #11-rest structured list. When it is empty and the legacy text
/// mirror is not — an OLD daemon that predates #11-rest — the texts are rendered
/// as all-unchecked criteria. That fallback is what keeps the append-only wire
/// rule honest: a new plugin against an old daemon degrades, it never blanks.
fn acceptance_view(issue: &IssueRow) -> Vec<AcceptanceCriterion> {
    if !issue.acceptance.is_empty() {
        return issue.acceptance.clone();
    }
    issue
        .acceptance_criteria
        .iter()
        .enumerate()
        .filter_map(|(idx, text)| AcceptanceCriterion::with_id(&legacy_placeholder_id(idx), text))
        .collect()
}

/// Draw one card content row: the `│` left+right edges in the border colour, then
/// the label/value `segments` laid out left-to-right from the inner column,
/// clipped by **chars** at the right edge (63d, utf8-safe).
fn card_field_row(buf: &mut WireBuffer, card_w: u16, row: u16, segments: &[(&str, Color)]) {
    let inner_right = card_w.saturating_sub(2);
    // Edges first; the content overlays the interior between them.
    put_clipped(buf, 0, row, "│", CARD_BORDER, card_w);
    put_clipped(buf, card_w.saturating_sub(1), row, "│", CARD_BORDER, card_w);
    let mut cx = 2u16;
    for (text, color) in segments {
        if cx >= inner_right {
            break;
        }
        cx = put_clipped(buf, cx, row, text, *color, inner_right);
    }
}

/// Wrap `text` into at most `max_lines` lines of at most `width` CHARS each
/// (utf8-safe — never a byte slice). Greedy word wrap, hard-splitting a word
/// longer than `width`; the last line is ellipsised when the text overflows the
/// cap so a huge description never blows past the card.
fn wrap_chars(text: &str, width: usize, max_lines: usize) -> Vec<String> {
    if width == 0 || max_lines == 0 {
        return Vec::new();
    }
    let mut lines: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut current_len = 0usize;
    for word in text.split_whitespace() {
        let word_len = word.chars().count();
        // A word longer than the line width is hard-split across lines.
        if word_len > width {
            if !current.is_empty() {
                lines.push(std::mem::take(&mut current));
                current_len = 0;
            }
            for ch in word.chars() {
                if current_len == width {
                    lines.push(std::mem::take(&mut current));
                    current_len = 0;
                }
                current.push(ch);
                current_len += 1;
            }
            continue;
        }
        let sep = usize::from(!current.is_empty());
        if current_len + sep + word_len > width {
            lines.push(std::mem::take(&mut current));
            current_len = 0;
        }
        if !current.is_empty() {
            current.push(' ');
            current_len += 1;
        }
        current.push_str(word);
        current_len += word_len;
    }
    if !current.is_empty() {
        lines.push(current);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    // Cap to max_lines, ellipsising the last kept line when we drop content.
    if lines.len() > max_lines {
        lines.truncate(max_lines);
        if let Some(last) = lines.last_mut() {
            let mut chars: Vec<char> = last.chars().collect();
            if chars.len() >= width && width >= 1 {
                chars.truncate(width.saturating_sub(1));
            }
            chars.push('…');
            *last = chars.into_iter().collect();
        }
    }
    lines
}

/// The `P0..P3` label for a wire priority scalar (63d). The scale is `0..3` with
/// HIGHER = MORE URGENT (`3` = P0 urgent, `0` = P3 routine, the default), so the
/// P-number is `3 - priority`.
fn priority_p_label(priority: i64) -> String {
    format!("P{}", 3 - priority.clamp(0, 3))
}

/// Format an epoch-millisecond timestamp as a UTC `YYYY-MM-DD` for the card
/// (63d). Chrono is a dev-only dep here, so the civil date is derived with
/// Hinnant's `days_from_civil` inverse — a pure, allocation-free conversion.
fn fmt_card_date(ms: i64) -> String {
    let days = ms.div_euclid(86_400_000);
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}")
}

/// The inverse of Howard Hinnant's `days_from_civil`: map a day count since the
/// Unix epoch (1970-01-01) to a proleptic-Gregorian `(year, month, day)` in UTC.
/// Exact for the whole i64 range; no leap-second / timezone handling (a calendar
/// date is all the card shows).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Convenience accessor re-exporting the transcript glyph for a [`MessageKind`]
/// so call sites (and tests) can reach the taxonomy mapping without importing the
/// widget module directly.
#[must_use]
pub const fn glyph_for(kind: MessageKind) -> char {
    transcript_glyph(kind)
}

/// Convenience accessor re-exporting the transcript colour for a [`MessageKind`].
#[must_use]
pub const fn color_for(kind: MessageKind) -> ainb_plugin_sdk::Color {
    transcript_color(kind)
}

#[cfg(test)]
mod card_tests {
    use super::*;
    use ainb_hangar_core::ids::{IssueId, TaskId};
    use ainb_plugin_sdk::WireBuffer;

    /// Reconstruct the full painted text of a rendered buffer (row-major) so a
    /// render assertion can search for the card's labels / values.
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

    /// The painted buffer as one string PER ROW, so an assertion can pin a glyph
    /// to the same line as its criterion instead of anywhere on the screen.
    fn painted_rows(buf: &WireBuffer) -> Vec<String> {
        (0..buf.height)
            .map(|y| {
                let mut row: Vec<(u16, &str)> = buf
                    .cells
                    .iter()
                    .filter(|(coord, _)| coord.y == y)
                    .map(|(coord, cell)| (coord.x, cell.symbol.as_str()))
                    .collect();
                row.sort_by_key(|(x, _)| *x);
                row.into_iter().map(|(_, sym)| sym).collect::<String>()
            })
            .collect()
    }

    /// A fully-populated issue row for the card render assertions (63d).
    fn full_issue() -> IssueRow {
        IssueRow {
            id: IssueId::from_str("i1").unwrap(),
            display_id: Some("HGR-1".into()),
            workspace_id: "ws".into(),
            title: "Fix the widget".into(),
            description: Some("The widget breaks on resize.".into()),
            state: "todo".into(),
            assignee: Some("agent:alice".into()),
            creator: "member:me".into(),
            created_at: 1_700_000_000_000,
            priority: 1,
            due_date: None,
            labels: vec!["bug".into(), "p0".into()],
            pr_url: None,
            branch: None,
            repo_ref: Some("/repos/widget".into()),
            agent: Some("codex".into()),
            source_branch: Some("main".into()),
            target_branch: Some("release".into()),
            external_ref: None,
            run_count: 0,
            last_run_status: None,
            last_run_at: None,
            parent_id: None,
            child_total: 0,
            child_done: 0,
            acceptance_criteria: Vec::new(),
            acceptance: Vec::new(),
            context_refs: Vec::new(),
        }
    }

    fn state_for(issue: IssueRow) -> TaskDetailState {
        TaskDetailState::new(TaskId::from_str("task-1").unwrap(), issue)
    }

    /// A never-run issue renders a full detail card — title, every field row, the
    /// description — and NO `Runs:` history line (63d, the headline case).
    #[test]
    fn detail_card_renders_every_field_for_a_never_run_issue() {
        let s = state_for(full_issue());
        let mut buf = WireBuffer::new(80, 30);
        render_task_detail(&mut buf, 80, 0, 29, &s);
        let text = painted_text(&buf);

        assert!(text.contains("📋 HGR-1 · Fix the widget"), "title: {text}");
        assert!(text.contains("Status: "), "status label");
        assert!(text.contains("todo"), "status value");
        assert!(text.contains("Priority: "), "priority label");
        assert!(text.contains("P2"), "priority 1 → P2");
        assert!(text.contains("Created: "), "created label");
        assert!(text.contains("2023-11-14"), "created date: {text}");
        assert!(text.contains("Assignee: "), "assignee label");
        assert!(text.contains("agent:alice"), "assignee value");
        assert!(text.contains("Agent: "), "agent label");
        assert!(text.contains("codex"), "agent value");
        assert!(text.contains("Repo: "), "repo label");
        assert!(text.contains("/repos/widget"), "repo value");
        assert!(text.contains("Source: "), "source label");
        assert!(text.contains("Target: "), "target label");
        assert!(text.contains("release"), "target value");
        assert!(text.contains("Labels: "), "labels label");
        assert!(text.contains("[bug]"), "label chip bug");
        assert!(text.contains("[p0]"), "label chip p0");
        assert!(text.contains("The widget breaks on resize."), "description");
        assert!(
            !text.contains("Runs:"),
            "a never-run issue has no runs line"
        );
    }

    /// An issue with unset card fields renders the em-dash placeholders and
    /// "no description", never a blank card (63d).
    #[test]
    fn detail_card_renders_placeholders_when_unset() {
        let mut issue = full_issue();
        issue.description = None;
        issue.assignee = None;
        issue.agent = None;
        issue.repo_ref = None;
        issue.source_branch = None;
        issue.target_branch = None;
        issue.labels = Vec::new();
        let s = state_for(issue);
        let mut buf = WireBuffer::new(80, 30);
        render_task_detail(&mut buf, 80, 0, 29, &s);
        let text = painted_text(&buf);

        assert!(text.contains("unassigned"), "unset assignee");
        assert!(
            text.contains("—"),
            "em-dash placeholder for unset repo/agent"
        );
        assert!(text.contains("no description"), "unset description");
    }

    /// Parity 28: the deadline renders on the card next to the labels — a real
    /// `YYYY-MM-DD` when set, the unset placeholder when not. Without this the
    /// wizard could author a due date the user could never see.
    #[test]
    fn detail_card_renders_the_due_date() {
        // Set: the calendar day appears under a `Due:` label.
        let mut issue = full_issue();
        issue.due_date = Some(1_785_542_400_000); // 2026-08-01 UTC midnight
        let s = state_for(issue);
        let mut buf = WireBuffer::new(80, 30);
        render_task_detail(&mut buf, 80, 0, 29, &s);
        let text = painted_text(&buf);
        assert!(text.contains("Due: "), "due label: {text}");
        assert!(text.contains("2026-08-01"), "due value: {text}");

        // Unset: the label still renders, with the em-dash placeholder.
        let s = state_for(full_issue());
        let mut buf = WireBuffer::new(80, 30);
        render_task_detail(&mut buf, 80, 0, 29, &s);
        let text = painted_text(&buf);
        assert!(text.contains("Due: "), "due label when unset: {text}");
        assert!(
            !text.contains("2026-08-01"),
            "no stale date on a deadline-less issue: {text}"
        );
    }

    /// A linked upstream issue (0043) renders a subtle `Linked: ⧉ <ref>` line on
    /// the detail card; an unlinked issue shows no such line.
    #[test]
    fn detail_card_renders_linked_line_only_when_linked() {
        // Linked: the ref + the ⧉ glyph appear.
        let mut issue = full_issue();
        issue.external_ref = Some("acme/api#42".into());
        let s = state_for(issue);
        let mut buf = WireBuffer::new(80, 30);
        render_task_detail(&mut buf, 80, 0, 29, &s);
        let text = painted_text(&buf);
        assert!(text.contains("Linked: "), "linked label: {text}");
        assert!(text.contains('⧉'), "link glyph");
        assert!(text.contains("acme/api#42"), "linked ref value");

        // Unlinked: no Linked line at all.
        let s = state_for(full_issue());
        let mut buf = WireBuffer::new(80, 30);
        render_task_detail(&mut buf, 80, 0, 29, &s);
        assert!(
            !painted_text(&buf).contains("Linked: "),
            "an unlinked issue shows no Linked line"
        );
    }

    /// A fresh unchecked criterion with a deterministic id.
    fn crit(id: &str, text: &str) -> AcceptanceCriterion {
        AcceptanceCriterion::with_id(id, text).expect("criterion")
    }

    /// An issue whose SECOND criterion is ticked.
    fn issue_with_one_ticked() -> IssueRow {
        let mut issue = full_issue();
        let mut second = crit("ac-b", "tests green");
        second.tick(1_700, Some("agent:builder"));
        issue.acceptance = vec![crit("ac-a", "builds"), second];
        issue.acceptance_criteria = issue.acceptance.iter().map(|c| c.text.clone()).collect();
        issue
    }

    /// 0048: an issue carrying acceptance criteria renders an `Acceptance:` header
    /// then one line per criterion; an empty list renders NO header.
    #[test]
    fn detail_card_renders_acceptance_block_only_when_present() {
        let mut issue = full_issue();
        issue.acceptance = vec![crit("ac-a", "builds"), crit("ac-b", "tests green")];
        let s = state_for(issue);
        let mut buf = WireBuffer::new(80, 30);
        render_task_detail(&mut buf, 80, 0, 29, &s);
        let text = painted_text(&buf);
        assert!(text.contains("Acceptance:"), "acceptance header: {text}");
        assert!(text.contains("builds"), "first criterion");
        assert!(text.contains("tests green"), "second criterion");

        // Empty list: no header at all.
        let s = state_for(full_issue());
        let mut buf = WireBuffer::new(80, 30);
        render_task_detail(&mut buf, 80, 0, 29, &s);
        assert!(
            !painted_text(&buf).contains("Acceptance:"),
            "an issue with no criteria shows no Acceptance header"
        );
    }

    /// **T7** — the detail-render half of the #11-rest acceptance: a checked
    /// criterion paints `☑`, an unchecked one `☐`, and the header counts them.
    /// The decoys (`☑` on an all-unchecked issue, a `0/2` or `2/2` header) must be
    /// ABSENT.
    #[test]
    fn task_detail_renders_checked_criterion() {
        let s = state_for(issue_with_one_ticked());
        let mut buf = WireBuffer::new(80, 30);
        render_task_detail(&mut buf, 80, 0, 29, &s);
        let text = painted_text(&buf);

        assert!(text.contains("Acceptance: 1/2"), "counted header: {text}");
        assert!(!text.contains("Acceptance: 0/2"), "decoy 0/2: {text}");
        assert!(!text.contains("Acceptance: 2/2"), "decoy 2/2: {text}");

        // Line-precise: the unchecked criterion carries ☐, the ticked one ☑.
        let rows = painted_rows(&buf);
        let unchecked_line = rows
            .iter()
            .find(|l| l.contains("builds"))
            .unwrap_or_else(|| panic!("no line for the unchecked criterion: {text}"));
        assert!(
            unchecked_line.contains('☐') && !unchecked_line.contains('☑'),
            "unchecked line must be ☐ only: {unchecked_line}"
        );
        let checked_line = rows
            .iter()
            .find(|l| l.contains("tests green"))
            .unwrap_or_else(|| panic!("no line for the checked criterion: {text}"));
        assert!(
            checked_line.contains('☑') && !checked_line.contains('☐'),
            "checked line must be ☑ only: {checked_line}"
        );

        // Decoy: an all-unchecked issue paints NO ☑ anywhere.
        let mut plain = full_issue();
        plain.acceptance = vec![crit("ac-a", "builds"), crit("ac-b", "tests green")];
        let s = state_for(plain);
        let mut buf = WireBuffer::new(80, 30);
        render_task_detail(&mut buf, 80, 0, 29, &s);
        let text = painted_text(&buf);
        assert!(text.contains("Acceptance: 0/2"), "header: {text}");
        assert!(
            !text.contains('☑'),
            "an all-unchecked issue paints no ☑: {text}"
        );
    }

    /// The append-only wire fallback: an OLD daemon sends only the text mirror,
    /// so the card still renders the criteria (all unchecked) rather than
    /// blanking the block.
    #[test]
    fn task_detail_falls_back_to_text_mirror_from_an_old_daemon() {
        let mut issue = full_issue();
        issue.acceptance_criteria = vec!["builds".into(), "tests green".into()];
        issue.acceptance = Vec::new();
        let s = state_for(issue);
        let mut buf = WireBuffer::new(80, 30);
        render_task_detail(&mut buf, 80, 0, 29, &s);
        let text = painted_text(&buf);
        assert!(text.contains("Acceptance: 0/2"), "header: {text}");
        assert!(text.contains("builds") && text.contains("tests green"), "{text}");
        assert!(!text.contains('☑'), "an old daemon's rows are unchecked: {text}");
    }

    /// **T8** — `a` selects the first criterion, `t` emits the toggle intent for
    /// that exact STABLE id, and a second `t` on an already-ticked criterion
    /// emits `checked: false`.
    #[test]
    fn task_detail_a_then_t_emits_set_criterion_intent() {
        let s = state_for(issue_with_one_ticked());

        // `t` with no selection is a no-op — never a blind tick of criterion 1.
        let out = reduce_task_detail(&s, TaskDetailEvent::Key('t'));
        assert_eq!(out.intent, None, "t before a raises nothing");

        // `a` selects the FIRST criterion; `t` toggles it to checked.
        let out = reduce_task_detail(&s, TaskDetailEvent::Key('a'));
        assert_eq!(out.state.acceptance_cursor, Some(0));
        let out = reduce_task_detail(&out.state, TaskDetailEvent::Key('t'));
        assert_eq!(
            out.intent,
            Some(TaskDetailIntent::SetCriterionChecked {
                issue_id: s.issue().id.clone(),
                criterion_id: "ac-a".to_string(),
                checked: true,
            }),
            "t ticks the SELECTED criterion by its stable id"
        );

        // A second `a` walks to the ALREADY-ticked criterion; `t` unticks it.
        let out = reduce_task_detail(&s, TaskDetailEvent::Key('a'));
        let out = reduce_task_detail(&out.state, TaskDetailEvent::Key('a'));
        assert_eq!(out.state.acceptance_cursor, Some(1));
        let out = reduce_task_detail(&out.state, TaskDetailEvent::Key('t'));
        assert_eq!(
            out.intent,
            Some(TaskDetailIntent::SetCriterionChecked {
                issue_id: s.issue().id.clone(),
                criterion_id: "ac-b".to_string(),
                checked: false,
            }),
            "t on a ticked criterion unticks it"
        );

        // A third `a` WRAPS back to the first.
        let mut st = s.clone();
        for _ in 0..3 {
            st = reduce_task_detail(&st, TaskDetailEvent::Key('a')).state;
        }
        assert_eq!(st.acceptance_cursor, Some(0), "the cursor wraps");

        // On an issue with NO criteria both keys are inert.
        let empty = state_for(full_issue());
        let out = reduce_task_detail(&empty, TaskDetailEvent::Key('a'));
        assert_eq!(out.state.acceptance_cursor, None);
        assert_eq!(out.intent, None);
        assert_eq!(
            reduce_task_detail(&out.state, TaskDetailEvent::Key('t')).intent,
            None
        );
    }

    /// The `▶` selection marker lands on the criterion under the acceptance
    /// cursor, and nowhere before `a` is pressed.
    #[test]
    fn acceptance_cursor_paints_the_selection_marker() {
        let s = state_for(issue_with_one_ticked());
        let selected = reduce_task_detail(&s, TaskDetailEvent::Key('a')).state;
        let mut buf = WireBuffer::new(80, 30);
        render_task_detail(&mut buf, 80, 0, 29, &selected);
        let rows = painted_rows(&buf);
        let line = rows
            .iter()
            .find(|l| l.contains("builds"))
            .expect("criterion line");
        assert!(line.contains('▶'), "marker on the selected line: {line}");
        let other = rows
            .iter()
            .find(|l| l.contains("tests green"))
            .expect("criterion line");
        assert!(!other.contains('▶'), "marker only on one line: {other}");
    }

    /// 0048: an issue carrying context references renders a `Context:` header then
    /// one line per reference; an empty list renders NO header.
    #[test]
    fn detail_card_renders_context_block_only_when_present() {
        let mut issue = full_issue();
        issue.context_refs = vec!["acme/api#42".into(), "https://docs".into()];
        let s = state_for(issue);
        let mut buf = WireBuffer::new(80, 30);
        render_task_detail(&mut buf, 80, 0, 29, &s);
        let text = painted_text(&buf);
        assert!(text.contains("Context:"), "context header: {text}");
        assert!(text.contains("acme/api#42"), "first reference");
        assert!(text.contains("https://docs"), "second reference");

        // Empty list: no header at all.
        let s = state_for(full_issue());
        let mut buf = WireBuffer::new(80, 30);
        render_task_detail(&mut buf, 80, 0, 29, &s);
        assert!(
            !painted_text(&buf).contains("Context:"),
            "an issue with no context refs shows no Context header"
        );
    }

    /// An issue with run history renders the `Runs:` line below the card with the
    /// count and latest run summary (63d).
    #[test]
    fn detail_card_renders_runs_line_when_issue_has_run() {
        let mut issue = full_issue();
        issue.run_count = 3;
        issue.last_run_status = Some("running".into());
        issue.last_run_at = Some(1_700_000_000_000);
        let s = state_for(issue);
        let mut buf = WireBuffer::new(80, 30);
        render_task_detail(&mut buf, 80, 0, 29, &s);
        let text = painted_text(&buf);

        assert!(text.contains("Runs: 3"), "run count: {text}");
        assert!(text.contains("last: running"), "latest run status");
        assert!(text.contains("2023-11-14"), "latest run date");
    }

    /// HEIGHT CONTRACT regression (the coordinator's Finding 1): at a short
    /// viewport (the 8-row snapshot panes) the card is skipped entirely so the
    /// PR badge stays on row 0 and the transcript region is preserved — the card
    /// must never squeeze the feed out.
    #[test]
    fn detail_card_yields_to_badge_and_transcript_at_short_viewports() {
        let mut issue = full_issue();
        issue.pr_url = Some("https://github.com/o/r/pull/7".into());
        let s = state_for(issue);
        let mut buf = WireBuffer::new(100, 8);
        render_task_detail(&mut buf, 100, 0, 8, &s);
        let text = painted_text(&buf);

        assert!(!text.contains('╭'), "no card border at 8 rows: {text}");
        assert!(text.contains("▶ PR "), "PR badge still renders: {text}");
        // The badge sits on row 0 exactly as before the card existed.
        let row0: String = buf
            .cells
            .iter()
            .filter(|(c, _)| c.y == 0)
            .map(|(_, cell)| cell.symbol.as_str())
            .collect();
        assert!(row0.contains("▶ PR "), "badge pinned to row 0: {row0}");
    }

    /// When the delete-confirm modal is open the bottom row paints the red
    /// delete prompt naming the bound issue plus the confirm/cancel hint (63l.5).
    #[test]
    fn delete_modal_paints_confirm_bar_with_target() {
        let mut s = state_for(full_issue());
        s = reduce_task_detail(&s, TaskDetailEvent::Key('x')).state;
        assert!(s.delete_modal_open(), "x opened the delete modal");
        let mut buf = WireBuffer::new(80, 30);
        render_task_detail(&mut buf, 80, 0, 29, &s);
        let text = painted_text(&buf);
        assert!(text.contains("delete"), "delete prompt painted: {text}");
        assert!(text.contains("Fix the widget"), "names the target issue");
        assert!(text.contains("[enter] confirm"), "confirm hint painted");
        assert!(text.contains("[esc] cancel"), "cancel hint painted");
    }

    /// `priority_p_label` maps the 0..3 urgency scale to P3..P0 (HIGHER = urgent).
    #[test]
    fn priority_p_label_maps_scale() {
        assert_eq!(priority_p_label(0), "P3");
        assert_eq!(priority_p_label(1), "P2");
        assert_eq!(priority_p_label(2), "P1");
        assert_eq!(priority_p_label(3), "P0");
        // Out-of-range clamps rather than underflowing.
        assert_eq!(priority_p_label(9), "P0");
    }

    /// `fmt_card_date` converts epoch-ms to a UTC calendar date without chrono.
    #[test]
    fn fmt_card_date_converts_epoch_ms() {
        assert_eq!(fmt_card_date(0), "1970-01-01");
        assert_eq!(fmt_card_date(1_700_000_000_000), "2023-11-14");
    }

    /// `wrap_chars` wraps on word boundaries, hard-splits an over-long word, and
    /// ellipsises the last line when the text overflows the cap — all by CHARS.
    #[test]
    fn wrap_chars_wraps_and_caps() {
        let lines = wrap_chars("alpha beta gamma delta", 11, 4);
        assert!(
            lines.iter().all(|l| l.chars().count() <= 11),
            "no line over width"
        );
        assert!(lines.len() <= 4);
        // A single word longer than the width is hard-split, never dropped.
        let split = wrap_chars("supercalifragilistic", 5, 4);
        assert!(split.len() > 1, "long word hard-split: {split:?}");
        // Overflow past the cap ellipsises the last kept line.
        let capped = wrap_chars("one two three four five six seven eight", 5, 2);
        assert_eq!(capped.len(), 2);
        assert!(capped[1].ends_with('…'), "overflow ellipsised: {capped:?}");
    }
}
