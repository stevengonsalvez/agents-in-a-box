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
//! total no-ops when not applicable.
//!
//! ## Collapsible thinking runs
//!
//! A long consecutive run of [`MessageKind::Thinking`] lines is a UX-§7 grouping
//! candidate: the raw transcript keeps every line, but the *visible* view folds a
//! run of [`THINKING_COLLAPSE_THRESHOLD`] or more into a single collapsed-group
//! entry so reasoning doesn't bury the prose + tool flow.

use ainb_hangar_core::ids::TaskId;
use ainb_hangar_proto::events::{HangarEvent, IssueRow, MessageKind, TaskResult};
use ainb_plugin_sdk::WireBuffer;

use crate::widgets::transcript::{render_transcript, transcript_glyph, transcript_color};

/// A consecutive run of this many [`MessageKind::Thinking`] lines (or more)
/// folds into a single collapsed-group entry in the visible view (UX §7).
pub const THINKING_COLLAPSE_THRESHOLD: usize = 4;

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
/// interleave (Multica renders human comments distinctly from agent prose
/// without their own taxonomy colour).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptEntry {
    kind: MessageKind,
    body: String,
    /// `true` when this entry is a human comment rather than an agent message.
    is_comment: bool,
}

impl TranscriptEntry {
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
}

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
pub fn reduce_task_detail(
    state: &TaskDetailState,
    ev: TaskDetailEvent,
) -> TaskDetailReduction {
    match ev {
        TaskDetailEvent::Key(c) => reduce_key(state, c),
        TaskDetailEvent::Esc => reduce_esc(state),
        TaskDetailEvent::Event(event) => fold_event(state, event),
    }
}

/// Handle a printable key. When the cancel modal is open it captures Enter
/// (confirm) and any other key is a no-op until Esc closes it.
fn reduce_key(state: &TaskDetailState, c: char) -> TaskDetailReduction {
    if state.cancel_modal_open {
        return match c {
            '\n' | '\r' => confirm_cancel(state),
            // Any other key while the modal is open does nothing (Esc aborts via
            // the dedicated event).
            _ => unchanged(state),
        };
    }
    match c {
        'j' => scroll_down(state),
        'k' => scroll_up(state),
        // Retry only once terminal; otherwise a no-op.
        'R' if state.lifecycle.is_terminal() => with_intent(
            state.clone(),
            TaskDetailIntent::RetryTask(state.task_id.clone()),
        ),
        // Cancel only while running; opens the confirm modal (no intent yet).
        'X' if state.lifecycle == TaskLifecycle::Running => open_cancel_modal(state),
        _ => unchanged(state),
    }
}

/// Handle Esc: abort the cancel modal if open; otherwise a no-op (the router
/// owns leaving the screen).
fn reduce_esc(state: &TaskDetailState) -> TaskDetailReduction {
    if state.cancel_modal_open {
        let mut next = state.clone();
        next.cancel_modal_open = false;
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
        HangarEvent::TaskMessage { task_id, kind, body } if task_id == state.task_id => {
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
        HangarEvent::TaskFinished { task_id, result, .. } if task_id == state.task_id => {
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
const fn with_intent(
    state: TaskDetailState,
    intent: TaskDetailIntent,
) -> TaskDetailReduction {
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
    // Sidebar takes a right-hand cap; it collapses when the area is too narrow
    // to leave the transcript a usable column.
    let sidebar_w: u16 = if area_w >= 60 { 24 } else { 0 };
    let main_w = area_w.saturating_sub(sidebar_w);

    // The transcript paints the visible (collapsed) view linearly.
    render_transcript(buf, main_w, top, bottom, &state.visible_entries());

    if sidebar_w > 0 {
        let sidebar_x = main_w;
        crate::widgets::sidebar::render_sidebar(
            buf, sidebar_x, top, bottom, sidebar_w, &state.issue,
        );
    }
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
