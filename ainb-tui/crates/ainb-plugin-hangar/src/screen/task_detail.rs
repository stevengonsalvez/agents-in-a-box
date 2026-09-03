//! P4.4 — Task detail + transcript screen: the pure reducer + width-aware render.
//!
//! The task-detail screen (hotkey `2`, or `enter` on an issue-list row) shows a
//! single task: a detail card (the issue title, ONE meta line, acceptance and
//! properties) above the streaming transcript. As with every Hangar screen, the
//! reducer ([`reduce_task_detail`]) is
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
/// The keybinding hint the live run card offers while the run can be cancelled.
const CANCEL_HINT: &str = "X cancel";
/// Rows the execution log spends at most, so a nine-attempt issue cannot push
/// the transcript it exists to introduce off the screen.
const RUNS_VISIBLE: usize = 4;
/// The activity pane takes a third of the width, between these bounds: narrower
/// than the minimum it says nothing legible, wider than the maximum it starts
/// competing with the transcript it is meant to annotate.
const ACTIVITY_MIN_W: u16 = 26;
/// See [`ACTIVITY_MIN_W`].
const ACTIVITY_MAX_W: u16 = 40;
/// What the transcript pane says when the EXPANDED run is not the run whose
/// transcript the screen holds. `hangar/board_card_timeline` reads an ISSUE's
/// newest run, so an older attempt has nothing to show until the durable read
/// takes a task id (track A, A6).
const OTHER_RUN_NOTE: &str = "no transcript for this run — only the newest run's is readable";
/// What the transcript pane says for a run that is GOING but has produced no
/// line yet. States the fact, not the reason: it is equally true of a run that
/// started a second ago and of one whose executor has no live producer.
const WAITING_NOTE: &str = "waiting for the first line of this run";
/// What the transcript pane says for a FINISHED run that left no transcript
/// behind (no log written, or the log is gone).
const NO_TRANSCRIPT_NOTE: &str = "this run recorded no transcript";
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

    /// Map a task wire `status` (the `tasks_list` snapshot vocabulary) onto the
    /// lifecycle, so a detail screen opened AFTER the task finalized still gates
    /// retry / cancel correctly. Live task events keep overriding this seed; an
    /// unknown status maps to `None` and the caller keeps its current value.
    ///
    /// Without this seed the reducer's default [`TaskLifecycle::Queued`] sticks
    /// for any task that reached a terminal state before the screen subscribed
    /// (e.g. a dispatch that failed in milliseconds), leaving `R` permanently
    /// dead on a task the screen itself reports as failed.
    #[must_use]
    pub fn from_wire_status(status: &str) -> Option<Self> {
        use ainb_hangar_core::task_status::TaskStatus;
        // Exhaustive over the store's status enum: a new variant fails to
        // compile here instead of silently leaving `R` dead on a seeded screen.
        Some(match TaskStatus::parse(status)? {
            TaskStatus::Queued | TaskStatus::Dispatched => Self::Queued,
            TaskStatus::Running => Self::Running,
            TaskStatus::Done => Self::Succeeded,
            TaskStatus::Failed => Self::Failed,
            TaskStatus::Cancelled => Self::Cancelled,
        })
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

/// One run of this issue, as the execution log renders it (crisp B4 §2.3).
///
/// Projected by the glue from the two snapshots that between them know a run:
/// `hangar/tasks_list` (which runs exist, for which issue, under which agent, in
/// which state) and `hangar/run_history` (what a FINISHED run cost). Neither is
/// enough alone — the history has no issue column and carries nothing for a run
/// still going, and the task rows carry no cost.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunRow {
    /// The run's task id — the join key, and how the screen knows which run the
    /// transcript it is holding belongs to.
    pub task_id: String,
    /// The task id's short form, for the row when the agent is unknown.
    pub short_id: String,
    /// The executing agent's display name (already resolved by the glue).
    pub agent: String,
    /// The run's state in the ONE run vocabulary.
    pub state: crate::vocab::RunState,
    /// When the run was queued (epoch ms) — the elapsed clock's zero.
    pub started_at: i64,
    /// When the run finished (epoch ms), or `None` while it is still going.
    pub finished_at: Option<i64>,
    /// The run's cost in whole cents, or `None` when the history has no row for
    /// it yet (every running run, and any run that recorded no usage).
    pub cost_cents: Option<i64>,
    /// The worktree branch the run committed on, or `None`.
    pub branch: Option<String>,
    /// The PR the run opened, or `None`.
    pub pr_url: Option<String>,
    /// That PR's CI + merge status, or `None` when it was never fetched.
    pub pr_status: Option<PrStatus>,
}

impl RunRow {
    /// How long the run has been going (still running) or ran for (terminal).
    #[must_use]
    pub fn elapsed_ms(&self, now_ms: i64) -> i64 {
        self.finished_at.unwrap_or(now_ms).saturating_sub(self.started_at)
    }

    /// The bucket this run sorts into: running first, failed next, everything
    /// else last (§2.3 "running on top, failed first").
    const fn order(&self) -> u8 {
        use crate::vocab::RunState;
        match self.state {
            RunState::Running => 0,
            RunState::Queued => 1,
            RunState::Failed => 2,
            RunState::Done | RunState::Cancelled => 3,
        }
    }
}

/// One line of the activity pane: the issue's narrative, not the run's
/// (crisp B4 §2.3).
///
/// Composed by the glue from the `hangar/issue_timeline` rows the activity modal
/// already reads, so the pane and the modal can never tell different stories.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivityRow {
    /// When it happened (epoch ms) — rendered as a relative age.
    pub at_ms: i64,
    /// The composed line (`impl-1 claimed the issue`).
    pub text: String,
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
    /// The issue assignee's roster display name (crisp B1, defect 8), resolved
    /// by the glue from the cached `hangar/agents_list`; `None` until the roster
    /// lands or when the assignee is not an agent on it. The header paints this
    /// over the raw `agent:<ulid>` actor ref.
    assignee_name: Option<String>,
    /// The display name of the agent executing the bound task, resolved by the
    /// glue from the tasks + agents snapshots; `None` for an issue with no run.
    /// The header's `Agent:` slot paints this ahead of the provider token.
    agent_name: Option<String>,
    /// The issue's currently ACTIVE run (`#<short id> <agent> (<status>)`) when
    /// the tasks snapshot has one, so an `already_active` dispatch refusal can
    /// name the row that blocks it (crisp B1, defect 5). `None` otherwise.
    blocking_run: Option<String>,
    /// Every run of this issue, ordered running-first then failed then the rest
    /// (crisp B4 §2.3), newest first inside each bucket. Empty for an issue that
    /// never ran, and until the tasks snapshot lands.
    runs: Vec<RunRow>,
    /// Which run of [`Self::runs`] is EXPANDED: the one the sticky run card
    /// describes and the transcript pane belongs to. `enter` walks it.
    run_cursor: usize,
    /// The issue's activity narrative for the right-hand pane.
    activity: Vec<ActivityRow>,
    /// How many [`MessageKind::ToolCall`] lines the transcript holds — the run
    /// card's `10 tools`.
    ///
    /// Counted on the way IN rather than scanned per paint: the transcript is
    /// the one buffer on this screen that grows without bound, and the card
    /// repaints on every frame of a live run.
    tool_calls: usize,
    /// Whether [`Self::backfill_transcript`] has already run for this open.
    ///
    /// The timeline request rides a CONSTANT id, so a reply is only ever matched
    /// to the bound task, never to the open that asked for it: open, Esc, open
    /// again, and a late first reply would prepend the run's whole history a
    /// second time. One apply per state, and a state is rebuilt on every open.
    transcript_backfilled: bool,
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
            assignee_name: None,
            agent_name: None,
            blocking_run: None,
            runs: Vec::new(),
            run_cursor: 0,
            activity: Vec::new(),
            tool_calls: 0,
            transcript_backfilled: false,
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

    /// Resolve the header's names from the cached snapshots (crisp B1): the
    /// assignee's roster display name, the bound run's agent name, and the
    /// issue's active run when one blocks a dispatch. The glue calls this at
    /// open AND whenever the agents / tasks snapshot lands, so the order the
    /// batched snapshots arrive in never leaves a raw ULID on screen.
    pub fn set_resolved_names(
        &mut self,
        assignee_name: Option<String>,
        agent_name: Option<String>,
        blocking_run: Option<String>,
    ) {
        self.assignee_name = assignee_name;
        self.agent_name = agent_name;
        self.blocking_run = blocking_run;
    }

    /// The assignee's resolved display name, if the roster knows it.
    #[must_use]
    pub fn assignee_name(&self) -> Option<&str> {
        self.assignee_name.as_deref()
    }

    /// The bound run's agent display name, if the snapshots know it.
    #[must_use]
    pub fn agent_name(&self) -> Option<&str> {
        self.agent_name.as_deref()
    }

    /// Replace the execution log (crisp B4 §2.3), ordering it running-first,
    /// then failed, then the rest, newest first inside each bucket.
    ///
    /// The cursor follows the run it was ON, by id, wherever the re-order puts
    /// it — so a run that finishes (and drops out of the running bucket) keeps
    /// its expanded row rather than handing the transcript pane to whatever slid
    /// into its index.
    ///
    /// It follows the OPERATOR'S choice, not the bound task: this is called on
    /// every `tasks_list` / `agents_list` snapshot, and every non-`TaskMessage`
    /// daemon event arms a re-pull, so recomputing from the bound id would snap
    /// an expanded older attempt back to the live run every few seconds — on a
    /// live issue, which is the only kind where it matters. The bound run is the
    /// fallback for the FIRST call, when nothing is expanded yet.
    pub fn set_runs(&mut self, mut runs: Vec<RunRow>) {
        runs.sort_by(|a, b| {
            a.order()
                .cmp(&b.order())
                .then_with(|| b.started_at.cmp(&a.started_at))
                .then_with(|| a.task_id.cmp(&b.task_id))
        });
        let expanded = self.expanded_run().map(|r| r.task_id.clone());
        let bound = self.task_id.as_str();
        self.run_cursor = expanded
            .and_then(|id| runs.iter().position(|r| r.task_id == id))
            .or_else(|| runs.iter().position(|r| r.task_id == bound))
            .unwrap_or(0);
        self.runs = runs;
    }

    /// Every run of the issue, in render order.
    #[must_use]
    pub fn runs(&self) -> &[RunRow] {
        &self.runs
    }

    /// The EXPANDED run — the one the sticky card describes and the transcript
    /// pane belongs to — or `None` for an issue with no runs.
    #[must_use]
    pub fn expanded_run(&self) -> Option<&RunRow> {
        self.runs.get(self.run_cursor)
    }

    /// Index of the expanded run in [`Self::runs`].
    #[must_use]
    pub const fn run_cursor(&self) -> usize {
        self.run_cursor
    }

    /// Replace the activity pane's lines (newest first).
    pub fn set_activity(&mut self, rows: Vec<ActivityRow>) {
        self.activity = rows;
    }

    /// The activity pane's lines.
    #[must_use]
    pub fn activity(&self) -> &[ActivityRow] {
        &self.activity
    }

    /// How many tool calls the transcript on screen holds.
    #[must_use]
    pub const fn tool_calls(&self) -> usize {
        self.tool_calls
    }

    /// Whether the transcript on screen belongs to the EXPANDED run.
    ///
    /// The durable read (`hangar/board_card_timeline`) serves an ISSUE's newest
    /// run, so expanding an older attempt has nothing to show. Saying so beats
    /// painting the newest run's transcript under an older run's card, which is
    /// the same lie the four status vocabularies were.
    #[must_use]
    pub fn transcript_is_for_expanded_run(&self) -> bool {
        self.expanded_run().is_none_or(|run| run.task_id == self.task_id.as_str())
    }

    /// Append a SYSTEM line to the transcript in the tool-result lane
    /// (`is_comment: false`, so it is not styled as somebody's comment).
    ///
    /// Used to surface the `@`-mention routing outcomes the daemon returns from
    /// `comment_add` (multica parity #2-rest). Before this, the reply was
    /// dropped on the floor, so a mention that was refused or coalesced looked
    /// exactly like one that ran.
    pub fn push_system_line(&mut self, body: String) {
        push_entry(
            self,
            TranscriptEntry {
                kind: MessageKind::ToolResult,
                body,
                is_comment: false,
            },
        );
    }

    /// Backfill the transcript from the run's durable stream-json (crisp B1,
    /// defect 7): `entries` are the parsed lines in stream order and go BEFORE
    /// anything already on screen, so a system line pushed since the open (or a
    /// live message that beat the reply) keeps its place after the history.
    /// Sticky-bottom follows the new tail; a released viewport keeps its line.
    ///
    /// ONCE per open ([`Self::transcript_backfilled`]): a second reply, from an
    /// earlier open of the same task, is dropped rather than doubling the history.
    /// Returns whether this call applied the history.
    pub fn backfill_transcript(&mut self, entries: Vec<TranscriptEntry>) -> bool {
        if entries.is_empty() || self.transcript_backfilled {
            return false;
        }
        self.transcript_backfilled = true;
        let added = entries.len();
        self.tool_calls = self
            .tool_calls
            .saturating_add(entries.iter().filter(|e| e.kind == MessageKind::ToolCall).count());
        let mut transcript = entries;
        transcript.append(&mut self.transcript);
        self.transcript = transcript;
        self.scroll_offset = if self.stuck_to_bottom {
            self.transcript.len().saturating_sub(1)
        } else {
            self.scroll_offset.saturating_add(added)
        };
        true
    }

    /// The current lifecycle.
    #[must_use]
    pub const fn lifecycle(&self) -> TaskLifecycle {
        self.lifecycle
    }

    /// Seed the lifecycle from the bound task's snapshot status at open time.
    /// Live task events folded afterwards keep overriding this value.
    pub const fn seed_lifecycle(&mut self, lifecycle: TaskLifecycle) {
        self.lifecycle = lifecycle;
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
        // `enter` expands the next run of the issue (crisp B4 §2.3). A walking
        // cursor, exactly like `a` on the acceptance criteria, so the execution
        // log needs no arrow keys of its own on a screen where `j`/`k` already
        // belong to the transcript.
        '\n' | '\r' => expand_next_run(state),
        // Open the comment-compose modal (`c`); captures input until Enter/Esc.
        'c' => open_compose(state),
        // Retry only a failed / cancelled run. A run that finished cleanly is
        // refused by the store (`force_requeue` answers DoNotRetry), which used
        // to make `R` a silent no-op (crisp B1, defect 9): say so instead.
        'R' if state.lifecycle == TaskLifecycle::Succeeded => refuse_retry(state),
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

/// What `R` says on a run that finished cleanly (crisp B1, defect 9).
const RETRY_REFUSED_NOTE: &str = "this run finished; R only retries a failed or cancelled run";

/// Say why `R` does nothing on a succeeded run, ONCE. The line is a no-op when
/// it is already the tail of the transcript: key repeat used to grow the
/// transcript by a copy per press (crisp B1 review).
fn refuse_retry(state: &TaskDetailState) -> TaskDetailReduction {
    if state.transcript.last().is_some_and(|e| e.body == RETRY_REFUSED_NOTE) {
        return unchanged(state);
    }
    let mut next = state.clone();
    next.push_system_line(RETRY_REFUSED_NOTE.to_string());
    no_intent(next)
}

/// Expand the NEXT run in the execution log, wrapping (crisp B4 §2.3). A no-op
/// on an issue with fewer than two runs — there is nothing else to expand.
fn expand_next_run(state: &TaskDetailState) -> TaskDetailReduction {
    if state.runs.len() < 2 {
        return unchanged(state);
    }
    let mut next = state.clone();
    next.run_cursor = (state.run_cursor + 1) % state.runs.len();
    no_intent(next)
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
///
/// The glyph flips OPTIMISTICALLY on the card (the same
/// move-then-arm-the-durable-RPC pattern the issue list's `d` uses) so the tick
/// is immediate; the daemon's `IssueUpdated` push then reconciles the row —
/// including undoing this flip if the mutation was rejected.
fn toggle_selected_criterion(state: &TaskDetailState) -> TaskDetailReduction {
    let mut criteria = acceptance_view(state.issue());
    let Some(idx) = state.acceptance_cursor else {
        return unchanged(state);
    };
    let Some(criterion) = criteria.get(idx) else {
        return unchanged(state);
    };
    let (criterion_id, checked) = (criterion.id.clone(), !criterion.checked);
    if checked {
        criteria[idx].tick(0, None);
    } else {
        criteria[idx].untick();
    }
    let mut next = state.clone();
    next.issue.acceptance_criteria = criteria.iter().map(|c| c.text.clone()).collect();
    next.issue.acceptance = criteria;
    with_intent(
        next,
        TaskDetailIntent::SetCriterionChecked {
            issue_id: state.issue.id.clone(),
            criterion_id,
            checked,
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
/// when stuck to the bottom (auto-scroll) and keeping the run card's tool count.
fn push_entry(state: &mut TaskDetailState, entry: TranscriptEntry) {
    if entry.kind == MessageKind::ToolCall {
        state.tool_calls = state.tool_calls.saturating_add(1);
    }
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
/// The EXECUTION VIEW (crisp B4 §2.3), top to bottom: the sticky run card and
/// the run's branch + PR, then the issue detail card (title, ONE meta line,
/// acceptance, properties), then the execution log of every run of this issue,
/// then the transcript of the expanded run — with the issue's activity narrative
/// in a right-hand column beside the last two.
///
/// ```text
/// ╭ 📋 HGR-5 · Ticket stats ───────────────────────────────────────╮
/// │ ◔ impl-1 is working · 7m 17s · 10 tools · $0.42       X cancel │
/// │ ainb/…N9GP8 → main · PR #8 ✓                                   │
/// │────────────────────────────────────────────────────────────────│
/// │ in progress · P2 · impl-1 · created 2026-09-02 · @boxtrack     │
/// ╰────────────────────────────────────────────────────────────────╯
/// runs ────────────────────────────────────┬ activity ─────────────
/// ▶ ◔ impl-1 · running 2m 04s              │ 7m impl-1 claimed it
///   ✗ rev-1  · failed 9m 12s               │ 6m todo → in progress
/// transcript ──────────────────────────────┤
/// ▌ Registering the route…                 │
/// ```
///
/// The right-hand metadata sidebar this screen used to carry is gone: it
/// repeated `Status` / `Assignee` from the card, added the workspace's raw ULID
/// under `Project:` and a mid-word-truncated `Notes:`, and it cost the
/// transcript a quarter of the width to do it. The activity pane took the
/// column, and it says what HAPPENED rather than restating what is on screen.
///
/// `now_ms` is the render clock the elapsed durations tick against — injected,
/// not read, so a snapshot test pins an exact duration.
///
/// The rendering is linear (no virtualisation): the visible view
/// ([`TaskDetailState::visible_entries`]) is painted top-down. The REFACTOR note
/// in `P4.md` defers virtualisation to >500-message buffers.
pub fn render_task_detail(
    buf: &mut WireBuffer,
    area_w: u16,
    top: u16,
    bottom: u16,
    state: &TaskDetailState,
    now_ms: i64,
) {
    // 63d: the issue DETAIL CARD spans the top of the area, so even a never-run
    // issue reads as a real card. It carries the run card at its head; when the
    // viewport is too short for a card at all, the run head still paints as bare
    // rows — the run is the one thing this screen must never drop.
    let mut body_top = render_detail_card(buf, area_w, top, bottom, state, now_ms);
    if body_top == top {
        body_top = render_run_head(buf, area_w, top, bottom, state, now_ms, false);
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

    // The activity pane takes a right-hand column beside the feed, and collapses
    // away when the pane is too narrow to leave the transcript a usable column
    // or when the issue has no narrative yet.
    let activity_w: u16 = if area_w >= 60 && !state.activity.is_empty() {
        (area_w / 3).clamp(ACTIVITY_MIN_W, ACTIVITY_MAX_W)
    } else {
        0
    };
    let feed_w = area_w.saturating_sub(activity_w);

    // The execution log, then the transcript of whichever run is expanded — or
    // one muted line saying why there is nothing to paint.
    let feed_top = render_execution_log(buf, feed_w, body_top, body_bottom, state, now_ms);
    match empty_transcript_note(state) {
        Some(note) if feed_top < body_bottom => {
            put_clipped(buf, 0, feed_top, note, HINT_MUTED, feed_w);
        }
        Some(_) => {}
        None => render_transcript(buf, feed_w, feed_top, body_bottom, &state.visible_entries()),
    }

    if activity_w > 0 {
        render_activity_pane(
            buf,
            feed_w,
            body_top,
            body_bottom,
            activity_w,
            state,
            now_ms,
        );
    }
}

/// The one muted line the transcript pane paints INSTEAD of a transcript, or
/// `None` when there are lines to paint.
///
/// The case this exists for: a RUNNING run whose transcript is empty. The run
/// card's elapsed reads the render clock, so it ticks whether or not anything is
/// streaming — an advancing number over a blank pane reads as evidence that
/// something is happening, and it is the one state on this screen that can
/// actively mislead. Every arm describes the STATE, never the cause, so none of
/// them goes stale when a producer that is missing today starts streaming.
fn empty_transcript_note(state: &TaskDetailState) -> Option<&'static str> {
    if !state.transcript_is_for_expanded_run() {
        return Some(OTHER_RUN_NOTE);
    }
    if !state.transcript.is_empty() {
        return None;
    }
    Some(match state.expanded_run()?.state {
        crate::vocab::RunState::Queued | crate::vocab::RunState::Running => WAITING_NOTE,
        _ => NO_TRANSCRIPT_NOTE,
    })
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

/// Paint the sticky live RUN CARD and the run's branch + PR beneath it
/// (crisp B4 §2.3), starting at `top`. Returns the first row below whatever it
/// painted — `top` itself when the issue has no run and no PR.
///
/// ```text
/// ◔ impl-1 is working · 7m 17s · 10 tools · $0.42            X cancel
/// ainb/…N9GP8 → main · PR #8 ✓
/// ```
///
/// The card describes the EXPANDED run (the one `enter` walks to), so the two
/// rows and the transcript below them are always the same run. Every segment is
/// dropped when unknown: a run with no cost recorded prints no `$`, a run with
/// no tool calls prints no `tools`, a run with no PR prints no chip. `X cancel`
/// is offered only while the run can actually be cancelled.
///
/// `edges` paints the card's `│ … │` borders around the rows; `false` renders
/// them bare, for the short viewport where the detail card yielded entirely.
fn render_run_head(
    buf: &mut WireBuffer,
    card_w: u16,
    top: u16,
    bottom: u16,
    state: &TaskDetailState,
    now_ms: i64,
    edges: bool,
) -> u16 {
    let mut row = top;
    let run = state.expanded_run();

    if let Some(run) = run {
        if row >= bottom {
            return row;
        }
        let head = format!(
            "{} {} {}",
            run.state.glyph(),
            run.agent,
            state_and_duration(run.state.phrase(), run, now_ms)
        );
        let mut parts: Vec<String> = Vec::new();
        // The tool count is the transcript's, so it only speaks for the run the
        // transcript belongs to.
        if state.transcript_is_for_expanded_run() && state.tool_calls > 0 {
            let plural = if state.tool_calls == 1 { "" } else { "s" };
            parts.push(format!("{} tool{plural}", state.tool_calls));
        }
        if let Some(cents) = run.cost_cents {
            parts.push(crate::vocab::cost_word(cents));
        }
        let line = if parts.is_empty() {
            head
        } else {
            format!("{head} · {}", parts.join(" · "))
        };
        let cells: Vec<(&str, Color)> = vec![(&line, run_color(run.state))];
        paint_head_row(buf, card_w, row, &cells, edges);
        // `X cancel` right-aligned, and only while there is a run to cancel.
        if run.state == crate::vocab::RunState::Running {
            let x = card_w.saturating_sub(CANCEL_HINT.chars().count() as u16 + 2);
            put_clipped(
                buf,
                x,
                row,
                CANCEL_HINT,
                HINT_MUTED,
                card_w.saturating_sub(1),
            );
        }
        row = row.saturating_add(1);
    }

    // The branch → target and the PR, on one line.
    let (branch, pr_url) = head_artifacts(state);
    if branch.is_none() && pr_url.is_none() {
        return row;
    }
    if row >= bottom {
        return row;
    }
    let mut cells: Vec<(&str, Color)> = Vec::new();
    let branch_text = branch.map(|b| match state.issue.target_branch.as_deref() {
        Some(target) => format!("{} → {target}", elide_branch(b)),
        None => elide_branch(b),
    });
    if let Some(text) = branch_text.as_deref() {
        cells.push((text, BRANCH_COLOR));
    }
    let status = pr_status_for(state, run);
    let chip = pr_url.map(|url| pr_chip(url, status));
    if let Some(chip) = chip.as_deref() {
        if !cells.is_empty() {
            cells.push((" · ", CARD_LABEL));
        }
        cells.push((chip, BADGE_GOLD));
        let (glyph, color) = ci_glyph(status.ci);
        cells.push((glyph, color));
        if let Some((label, color)) = conflict_segment(status.mergeable) {
            cells.push((label, color));
        }
    }
    paint_head_row(buf, card_w, row, &cells, edges);
    row.saturating_add(1)
}

/// A run's state word (or card phrase) plus how long it took, as ONE fragment:
/// `is working · 7m 17s`, `failed in 5m 00s`, `failed`.
///
/// The preposition is the point, and it is shared so the two surfaces cannot
/// drift: without `in`, a terminal row reads `failed 5m 00s`, which everybody
/// parses as "failed five minutes AGO" rather than "took five minutes". The run
/// card and the execution log differ only in the `lead` they pass — the card's
/// sentence phrase versus the log's compact word.
///
/// A terminal run with NO recorded end drops the duration entirely rather than
/// ticking: `finished_at` is joined from the row-capped `run_history`, so a real
/// finished run can arrive without one, and an elapsed measured from its START
/// would grow on every repaint. `failed` alone is true; `failed in 3d 4h` is not.
fn state_and_duration(lead: &str, run: &RunRow, now_ms: i64) -> String {
    use crate::vocab::RunState;
    let elapsed = || crate::vocab::elapsed_word(run.elapsed_ms(now_ms));
    match (run.finished_at, run.state) {
        (Some(_), _) => format!("{lead} in {}", elapsed()),
        (None, RunState::Queued | RunState::Running) => format!("{lead} · {}", elapsed()),
        (None, _) => lead.to_string(),
    }
}

/// Whether `run` is the run whose transcript the screen is bound to.
fn is_bound_run(state: &TaskDetailState, run: Option<&RunRow>) -> bool {
    run.is_none_or(|r| r.task_id == state.task_id.as_str())
}

/// The PR status to paint for `run`: the `pr_status_refresh` reply once one has
/// landed for the BOUND run, else whatever the tasks snapshot recorded for the
/// run itself, else all-unknown.
///
/// Order matters, and getting it backwards is visible: the screen-level status
/// starts all-`Unknown`, so preferring it unconditionally made the run card show
/// a pending `…` beside an execution-log row already reading `✓` for the same PR.
fn pr_status_for(state: &TaskDetailState, run: Option<&RunRow>) -> PrStatus {
    if is_bound_run(state, run) {
        if state.pr_status.ci != CiRollup::Unknown {
            return state.pr_status;
        }
        return run.and_then(|r| r.pr_status).unwrap_or(state.pr_status);
    }
    run.and_then(|r| r.pr_status).unwrap_or(UNKNOWN_PR_STATUS)
}

/// The branch + PR the run head's second row names: the EXPANDED run's own, and
/// the issue-level pair only when that run is the one the screen is bound to.
///
/// The fallback exists because an issue can carry a branch and a PR from a run
/// the tasks snapshot no longer lists. Letting it apply to any expanded run
/// would print the bound run's branch under a different run's card.
fn head_artifacts(state: &TaskDetailState) -> (Option<&str>, Option<&str>) {
    let run = state.expanded_run();
    if is_bound_run(state, run) {
        return (
            run.and_then(|r| r.branch.as_deref()).or_else(|| state.branch()),
            run.and_then(|r| r.pr_url.as_deref()).or_else(|| state.pr_url()),
        );
    }
    (
        run.and_then(|r| r.branch.as_deref()),
        run.and_then(|r| r.pr_url.as_deref()),
    )
}

/// How many rows [`render_run_head`] will paint (0, 1 or 2) — the layout asks
/// BEFORE painting so the card can budget for them.
fn run_head_rows(state: &TaskDetailState) -> u16 {
    let (branch, pr_url) = head_artifacts(state);
    u16::from(state.expanded_run().is_some()) + u16::from(branch.is_some() || pr_url.is_some())
}

/// Paint one run-head row, with or without the detail card's side borders.
fn paint_head_row(
    buf: &mut WireBuffer,
    card_w: u16,
    row: u16,
    cells: &[(&str, Color)],
    edges: bool,
) {
    if edges {
        card_field_row(buf, card_w, row, cells);
        return;
    }
    let mut cx = 0u16;
    for (text, color) in cells {
        cx = put_clipped(buf, cx, row, text, *color, card_w);
    }
}

/// The compact PR chip the run head paints: `PR #8` when the URL ends in a
/// number, else a bare `PR`.
///
/// The URL itself is gone from this screen (crisp B4 §2.3): it cost 45 cells to
/// say what `#8` says, and `o` opens it. The number is parsed from the URL tail
/// rather than synthesised from anything else — a PR the daemon never captured
/// renders no chip at all (track B "do not attempt" #8).
fn pr_chip(url: &str, _status: PrStatus) -> String {
    match url
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .filter(|s| s.chars().all(|c| c.is_ascii_digit()) && !s.is_empty())
    {
        Some(number) => format!("PR #{number}"),
        None => "PR".to_string(),
    }
}

/// The CI rollup glyph + its colour (e38.34): `✓` green pass, `✗` red fail, `…`
/// amber pending / muted unknown, so a glance distinguishes a green build from a
/// broken one and neither from "still fetching".
const fn ci_glyph(ci: CiRollup) -> (&'static str, Color) {
    match ci {
        CiRollup::Pass => (" ✓", STATUS_GREEN),
        CiRollup::Fail => (" ✗", STATUS_RED),
        CiRollup::Pending => (" …", STATUS_AMBER),
        CiRollup::Unknown => (" …", HINT_MUTED),
    }
}

/// The merge-conflict segment: ` ✗ CONFLICT` in red, or `None`.
///
/// Only a CONFLICT speaks (crisp B4 §2.3). A clean mergeable PR used to paint
/// ` ✓ mergeable` beside an already-green ` CI ✓` — two ticks for one good
/// state, on the one line that now has to carry the branch as well. An `Unknown`
/// mergeable (GitHub still computing) has always painted nothing, and still does.
const fn conflict_segment(m: Mergeable) -> Option<(&'static str, Color)> {
    match m {
        Mergeable::Conflicting => Some((" ✗ CONFLICT", STATUS_RED)),
        Mergeable::Mergeable | Mergeable::Unknown => None,
    }
}

/// `ainb/01M1FKF4BDNQ3JK5CQSS3N9GP8` → `ainb/…3N9GP8`, the SAME elide the Kanban
/// card applies (crisp B1, Q14), so one branch reads identically on both.
fn elide_branch(branch: &str) -> String {
    crate::screen::kanban::elide_branch(branch)
}

/// The colour a run's state paints in: the transcript taxonomy's error red for a
/// failure, the PR green for a clean finish, the branch blue while it runs.
const fn run_color(state: crate::vocab::RunState) -> Color {
    use crate::vocab::RunState;
    match state {
        RunState::Running => BRANCH_COLOR,
        RunState::Done => STATUS_GREEN,
        RunState::Failed => STATUS_RED,
        RunState::Queued | RunState::Cancelled => HINT_MUTED,
    }
}

/// Paint the EXECUTION LOG (crisp B4 §2.3): one row per run of this issue,
/// running on top and failed first, the expanded one marked `▶`. Returns the
/// first row below it (`top` when the issue has no runs — no header, no
/// placeholder).
///
/// At most [`RUNS_VISIBLE`] rows paint, windowed so the expanded run is always
/// one of them: the log is context for the transcript below it, and an issue
/// retried nine times must not push the transcript off the screen.
fn render_execution_log(
    buf: &mut WireBuffer,
    area_w: u16,
    top: u16,
    bottom: u16,
    state: &TaskDetailState,
    now_ms: i64,
) -> u16 {
    if top >= bottom {
        return top;
    }
    // The rules exist to SEPARATE the log from the transcript. With no log there
    // is nothing to separate, so an issue with no runs spends no row on chrome —
    // it is also the case with the least room (the 8-row panes).
    if state.runs.is_empty() {
        return top;
    }
    let mut row = render_pane_rule(buf, area_w, top, "runs");
    let window = RUNS_VISIBLE.min(state.runs.len());
    let start = state
        .run_cursor
        .saturating_sub(window.saturating_sub(1))
        .min(state.runs.len().saturating_sub(window));
    for (idx, run) in state.runs.iter().enumerate().skip(start).take(window) {
        if row >= bottom {
            return row;
        }
        let expanded = idx == state.run_cursor;
        let marker = if expanded { "▶ " } else { "  " };
        let line = format!(
            "{} {} · {}",
            run.state.glyph(),
            run.agent,
            state_and_duration(run.state.word(), run, now_ms)
        );
        let mut cx = put_clipped(buf, 0, row, marker, SELECTION_GREEN, area_w);
        cx = put_clipped(buf, cx, row, &line, run_color(run.state), area_w);
        if let Some(url) = run.pr_url.as_deref() {
            cx = put_clipped(buf, cx, row, "  ", CARD_LABEL, area_w);
            let status = pr_status_for(state, Some(run));
            cx = put_clipped(buf, cx, row, &pr_chip(url, status), BADGE_GOLD, area_w);
            let (glyph, color) = ci_glyph(status.ci);
            cx = put_clipped(buf, cx, row, glyph, color, area_w);
        }
        if let Some(cents) = run.cost_cents {
            cx = put_clipped(buf, cx, row, "  ", CARD_LABEL, area_w);
            cx = put_clipped(
                buf,
                cx,
                row,
                &crate::vocab::cost_word(cents),
                HINT_MUTED,
                area_w,
            );
        }
        let _ = cx;
        row = row.saturating_add(1);
    }
    if row < bottom {
        row = render_pane_rule(buf, area_w, row, "transcript");
    }
    row
}

/// Paint the issue's ACTIVITY narrative in the right-hand column (crisp B4
/// §2.3): `7m  impl-1 claimed the issue`, newest first, behind a `│` divider
/// that separates it from the feed.
///
/// Text the agents wrote (a comment body reaches this pane through
/// `issue_timeline`) goes through [`crate::screen::display_char`], the one
/// sanitiser, exactly as the Inbox and Control Center route theirs.
fn render_activity_pane(
    buf: &mut WireBuffer,
    x: u16,
    top: u16,
    bottom: u16,
    width: u16,
    state: &TaskDetailState,
    now_ms: i64,
) {
    let right = x.saturating_add(width);
    let text_x = x.saturating_add(2);
    for row in top..bottom {
        put_clipped(buf, x, row, "│", CARD_BORDER, right);
    }
    let mut row = render_pane_rule_at(buf, text_x, right, top, "activity");
    for entry in &state.activity {
        if row >= bottom {
            return;
        }
        let age = crate::vocab::age_word(now_ms.saturating_sub(entry.at_ms));
        let cx = put_clipped(buf, text_x, row, &format!("{age:>3} "), HINT_MUTED, right);
        put_sanitised(buf, cx, row, &entry.text, CARD_VALUE, right);
        row = row.saturating_add(1);
    }
}

/// Paint a pane header rule at `(0, row)`: `runs ────────`, the title in gold
/// over a muted rule to the right edge. Returns the row below it.
fn render_pane_rule(buf: &mut WireBuffer, area_w: u16, row: u16, title: &str) -> u16 {
    render_pane_rule_at(buf, 0, area_w, row, title)
}

/// [`render_pane_rule`] in a column starting at `x` and clipped at `right`.
fn render_pane_rule_at(buf: &mut WireBuffer, x: u16, right: u16, row: u16, title: &str) -> u16 {
    let cx = put_clipped(buf, x, row, title, CARD_TITLE, right);
    let mut rule = String::from(" ");
    for _ in cx.saturating_add(1)..right {
        rule.push('─');
    }
    put_clipped(buf, cx, row, &rule, CARD_BORDER, right);
    row.saturating_add(1)
}

/// [`put_clipped`] for text an AGENT authored: every char goes through
/// [`crate::screen::display_char`] first, so a control or bidi-override
/// character renders as a visible middot instead of acting on the terminal.
fn put_sanitised(buf: &mut WireBuffer, x: u16, row: u16, s: &str, color: Color, right: u16) -> u16 {
    let mut cx = x;
    for ch in s.chars() {
        if cx >= right {
            break;
        }
        let mut cell = Cell::new(crate::screen::display_char(ch).to_string());
        cell.fg = Some(color);
        buf.push(Coord::new(cx, row), cell);
        cx = cx.saturating_add(1);
    }
    cx
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
/// cornflower-bordered box carrying the sticky run card, the run's branch + PR,
/// the ONE meta line, and the issue's acceptance / properties / description, so
/// a never-run issue reads as a real card instead of an almost-empty page.
/// Returns the first row BELOW the card (+ its optional `Runs:` history line) —
/// the caller starts the execution log / transcript there.
///
/// Width-aware: every value is clipped by **chars** (never bytes — the utf8
/// truncate trap this file documents), and the description wraps to the inner
/// width, capped so the card always leaves room for the transcript below.
///
/// HEIGHT CONTRACT: the card budgets itself to `available - RESERVED_BELOW`
/// (a minimum legible feed region) and paints nothing (returns `top` unchanged)
/// when that budget cannot fit a legible card — a short viewport (e.g. the 8-row
/// snapshot panes) drops to the run head + transcript alone, which is the pair
/// that carries the run.
fn render_detail_card(
    buf: &mut WireBuffer,
    area_w: u16,
    top: u16,
    bottom: u16,
    state: &TaskDetailState,
    now_ms: i64,
) -> u16 {
    let issue = state.issue();
    let card_w = area_w;
    let available = bottom.saturating_sub(top);

    // The rows the card must LEAVE BELOW itself: the execution log's rule + one
    // run row, the transcript's rule, and a minimum legible transcript region.
    // The card's whole budget is what remains after this reservation — the
    // transcript feed is the screen's job, the card is context, so the card
    // yields, never the feed.
    const RESERVED_BELOW: u16 = 6;
    // The card's fixed chrome: top border + the ONE meta row + divider + bottom
    // border = 4 rows; the description adds ≥1 more, a run history line 1 more.
    const CARD_FIXED_ROWS: u16 = 4;
    /// The description never exceeds this many wrapped lines, so even a tall
    /// viewport keeps the card compact (≈40% of a 30-row pane at worst).
    const DESC_MAX_LINES: u16 = 4;

    // The `Runs:` history line is the FALLBACK for the execution log: it says
    // how many times the issue ran when the tasks snapshot has not landed (or
    // holds no row for it), and stays quiet once the log itself can say more.
    let runs_line = issue.run_count > 0 && state.runs.is_empty();
    let runs_rows = u16::from(runs_line);
    // The run head takes up to two rows inside the card, plus its divider.
    let head_rows = run_head_rows(state);
    let head_block = head_rows + u16::from(head_rows > 0);
    let budget = available.saturating_sub(RESERVED_BELOW);
    let min_needed = CARD_FIXED_ROWS + head_block + 1 + runs_rows;
    // Too narrow, or the viewport can't fit a legible card AND the reserved
    // feed region — skip the card entirely, never squeeze the transcript out.
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
        .saturating_sub(head_block)
        .saturating_sub(runs_rows)
        .clamp(1, DESC_MAX_LINES) as usize;
    let desc_lines = wrap_chars(desc_text, inner_w, desc_budget);
    let mut row = top;

    // --- top border with the gold title overlaid ---
    draw_card_hline(buf, row, card_w, '╭', '╮');
    // The trailing space keeps the title off the border rule it is painted over.
    let title = match &issue.display_id {
        Some(d) => format!(" 📋 {} · {} ", d, issue.title),
        None => format!(" 📋 {} ", issue.title),
    };
    put_clipped(buf, 2, row, &title, CARD_TITLE, inner_right);
    row = row.saturating_add(1);

    // --- the sticky live RUN CARD + the run's branch / PR, then a divider, so
    //     the run reads FIRST and the issue's metadata reads under it ---
    if head_rows > 0 {
        row = render_run_head(buf, card_w, row, bottom, state, now_ms, true);
        draw_card_divider(buf, row, card_w);
        row = row.saturating_add(1);
    }

    // --- The ONE meta line (crisp B4 §2.3), replacing the four key/value rows
    //     that carried the same six values under labels reading `, ` or
    //     `unassigned` on most cards. The RUN is the headline here, not the
    //     card's metadata, so the metadata gets one line and the run gets the
    //     sticky card above it. ---
    card_field_row(
        buf,
        card_w,
        row,
        &[(
            &meta_line(issue, state.assignee_name.as_deref()),
            CARD_VALUE,
        )],
    );
    row = row.saturating_add(1);

    // --- Labels / Due: progressive disclosure, unlike the row they came from.
    //     The old row printed `Labels: —   Due: —` on every untriaged card, the
    //     same zero-information placeholder as the `◇ None` chip B2 deleted. ---
    if !issue.labels.is_empty() || issue.due_date.is_some() {
        let labels = issue.labels.iter().map(|l| format!("[{l}]")).collect::<Vec<_>>().join(" ");
        let due = issue.due_date.map(fmt_card_date);
        let mut cells: Vec<(&str, Color)> = Vec::new();
        if !labels.is_empty() {
            cells.push(("Labels: ", CARD_LABEL));
            cells.push((&labels, CARD_VALUE));
        }
        let due_text = due.unwrap_or_default();
        if !due_text.is_empty() {
            cells.push(("   Due: ", CARD_LABEL));
            cells.push((&due_text, CARD_VALUE));
        }
        card_field_row(buf, card_w, row, &cells);
        row = row.saturating_add(1);
    }

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

    // --- Origin provenance (0056, multica parity #21): the badge is shown only
    //     for a PLATFORM-created card. A human-authored card ('manual') and a
    //     pre-0056 card (no origin_type) need no badge, so they read unchanged. ---
    if let Some(badge) = origin_badge(issue.origin_type.as_deref()) {
        card_field_row(
            buf,
            card_w,
            row,
            &[("Origin: ", CARD_LABEL), (badge, CARD_VALUE)],
        );
        row = row.saturating_add(1);
    }

    // --- Why this card is NOT running (multica parity #12, migration 0058):
    //     the newest DECLINED dispatch attempt, rendered only when there is one.
    //     A card that ran fine reads exactly as before — the whole point is that
    //     "queued forever with no explanation" becomes a stated cause. Amber,
    //     matching the `unstable` presence band: it is a warning, not a failure. ---
    if let Some(line) = dispatch_decline_line(
        issue.last_dispatch_reason.as_deref(),
        issue.last_dispatch_detail.as_deref(),
        state.blocking_run.as_deref(),
    ) {
        card_field_row(
            buf,
            card_w,
            row,
            &[("⚠ Not dispatched: ", CARD_LABEL), (&line, STATUS_AMBER)],
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

    // --- Typed links (multica parity #20): one line per link, glyphed by kind so
    //     the gating ones read differently from the associations —
    //     🔒 an UNFINISHED blocker, ✓ a satisfied one, → what this card blocks,
    //     ~ a related card. Rendered ONLY when non-empty, so an old daemon (which
    //     sends no `dependencies`) leaves the card byte-identical. ---
    if !issue.dependencies.is_empty() {
        card_field_row(buf, card_w, row, &[("Links:", CARD_LABEL)]);
        row = row.saturating_add(1);
        for link in &issue.dependencies {
            let (glyph, kind_label) = link_glyph_and_label(link);
            let reference = link.display_id.clone().unwrap_or_else(|| link.issue_id.clone());
            let head = format!("  {glyph} {kind_label:<10} {reference}  ");
            card_field_row(
                buf,
                card_w,
                row,
                &[(&head, CARD_LABEL), (&link.title, CARD_VALUE)],
            );
            row = row.saturating_add(1);
        }
    }

    // --- Subscribers + reactions (multica parity #22). Both are DETAIL-ONLY
    //     wire fields, so a list snapshot (and any pre-#22 daemon) leaves them
    //     at their default and the card renders byte-identically to today. ---
    if issue.subscriber_count > 0 {
        let count = format!("{}", issue.subscriber_count);
        let mut cells: Vec<(&str, Color)> = vec![("Subs:  ", CARD_LABEL), (&count, CARD_VALUE)];
        if issue.subscribed {
            cells.push(("  ✓ you", SELECTION_GREEN));
        }
        card_field_row(buf, card_w, row, &cells);
        row = row.saturating_add(1);
    }
    if !issue.reactions.is_empty() {
        let buckets: Vec<String> =
            issue.reactions.iter().map(|r| format!("{} {}  ", r.emoji, r.count)).collect();
        let mut cells: Vec<(&str, Color)> = vec![("React: ", CARD_LABEL)];
        for (bucket, reaction) in buckets.iter().zip(&issue.reactions) {
            // A bucket the local human is in gets the same accent the acceptance
            // markers use, so "mine" reads at a glance.
            cells.push((
                bucket.as_str(),
                if reaction.mine {
                    SELECTION_GREEN
                } else {
                    CARD_VALUE
                },
            ));
        }
        card_field_row(buf, card_w, row, &cells);
        row = row.saturating_add(1);
    }

    // --- Custom properties (multica parity #17). DETAIL-ONLY wire field, so a
    //     list snapshot (and any pre-#17 daemon) leaves it empty and the card
    //     renders byte-identically to today. ---
    if !issue.properties.is_empty() {
        card_field_row(buf, card_w, row, &[("Props:", CARD_LABEL)]);
        row = row.saturating_add(1);
        for prop in &issue.properties {
            let head = format!("  ◆ {}: ", prop.name);
            card_field_row(
                buf,
                card_w,
                row,
                &[(&head, CARD_LABEL), (&prop.value, CARD_VALUE)],
            );
            row = row.saturating_add(1);
        }
    }
    // --- Agent metadata scratch (multica parity #17). Read-only, and HIDDEN
    //     when empty — the reference's own UI rule, so it stays quiet in the
    //     common case. ---
    if !issue.metadata.is_empty() {
        card_field_row(buf, card_w, row, &[("Meta:", CARD_LABEL)]);
        row = row.saturating_add(1);
        for entry in &issue.metadata {
            let head = format!("  · {} = ", entry.key);
            card_field_row(
                buf,
                card_w,
                row,
                &[(&head, CARD_LABEL), (&entry.value, CARD_VALUE)],
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

/// The ONE meta line (crisp B4 §2.3):
/// `in progress · P2 · impl-1 · created 2026-09-02 · @boxtrack · main → main`.
///
/// Every segment is a value, never a label: the four rows this replaced spent
/// half their width on `Status: ` / `Assignee: ` / `Repo: ` / `Source: ` and
/// then printed `—` or `unassigned` into most of them. A segment with nothing to
/// say is DROPPED, so the line is as long as the issue is real.
///
/// The status word comes from [`crate::vocab::issue_word`] (never the raw wire
/// token) and the repo from [`repo_label`] (never the absolute path).
fn meta_line(issue: &IssueRow, assignee_name: Option<&str>) -> String {
    use ainb_hangar_proto::lifecycle::IssueLifecycle;

    let mut parts: Vec<String> = vec![
        crate::vocab::issue_word(IssueLifecycle::for_state(&issue.state)).to_string(),
        priority_p_label(issue.priority),
        crate::screen::assignee_label(assignee_name, issue.assignee.as_deref())
            .unwrap_or_else(|| "unassigned".to_string()),
        format!("created {}", fmt_card_date(issue.created_at)),
    ];
    if let Some(repo) = issue.repo_ref.as_deref() {
        parts.push(repo_label(repo));
    }
    match (
        issue.source_branch.as_deref(),
        issue.target_branch.as_deref(),
    ) {
        (Some(source), Some(target)) => parts.push(format!("{source} → {target}")),
        (Some(one), None) | (None, Some(one)) => parts.push(one.to_string()),
        (None, None) => {}
    }
    parts.join(" · ")
}

/// The repo as the meta line names it: `@boxtrack`, not
/// `/home/claude/ainb-e2e-home/projects/boxtrack` (crisp B4 §2.3).
///
/// A path is 40+ cells of which only the last segment identifies anything, and
/// the detail screen has ONE line for six values. A ref that is not a path
/// (a registered repo label) is already the answer and passes through with the
/// same `@` marker.
fn repo_label(repo_ref: &str) -> String {
    let trimmed = repo_ref.trim_end_matches('/');
    let name = trimmed.rsplit('/').find(|s| !s.is_empty()).unwrap_or(trimmed);
    format!("@{name}")
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
/// The glyph + rendered kind label for one typed link (multica parity #20).
///
/// `blocked_by` is the only kind that can gate, so it is the only one whose glyph
/// varies: 🔒 while the blocker is unfinished, ✓ once it is satisfied. An
/// unrecognised kind token (a newer daemon) falls back to the neutral association
/// glyph rather than being dropped.
fn link_glyph_and_label(
    link: &ainb_hangar_proto::events::IssueLinkRow,
) -> (&'static str, &'static str) {
    match link.kind.as_str() {
        "blocked_by" if link.satisfied => ("✓", "blocked-by"),
        "blocked_by" => ("🔒", "blocked-by"),
        "blocks" => ("→", "blocks"),
        _ => ("~", "related"),
    }
}

/// The `Origin:` badge text for a wire `origin_type`, or `None` when no badge
/// belongs on the card (migration 0056, multica parity #21).
///
/// A badge marks a card the PLATFORM created, so it is deliberately suppressed
/// for `manual` (a human authored it — the unremarkable case) and for a
/// pre-0056 card whose provenance is simply unknown. An unrecognised value is
/// treated like `manual` and shows nothing, mirroring the lenient read side.
fn origin_badge(origin_type: Option<&str>) -> Option<&'static str> {
    match origin_type?.trim() {
        "autopilot" => Some("⚙ autopilot"),
        "comment_mention" => Some("💬 comment mention"),
        _ => None,
    }
}

/// The human phrase for a declined dispatch (multica parity #12): the code's
/// [`DispatchReason::label`] plus the free-text detail, e.g.
/// `runtime offline — task 01J… queued; runtime rt-1 is offline`.
///
/// Returns `None` when there is no code, so a healthy card renders no line at
/// all. A code this build does not know falls back to the RAW token rather than
/// hiding the line — an older plugin against a newer daemon must still tell the
/// user something is wrong, and the detail carries the specifics regardless.
///
/// An `already_active` refusal names the blocking row when the tasks snapshot
/// knows it (`a run is already active: #1J7MR7 impl-1 (running)`, crisp B1,
/// defect 5). A detail that merely restates the label prints once, never
/// `a run is already active, a run is already active (queued)`.
fn dispatch_decline_line(
    reason: Option<&str>,
    detail: Option<&str>,
    blocking_run: Option<&str>,
) -> Option<String> {
    use ainb_hangar_core::dispatch_reason::DispatchReason;

    let raw = reason?.trim();
    if raw.is_empty() {
        return None;
    }
    let code = DispatchReason::parse(raw);
    let label: &str = match code {
        Some(code) => code.label(),
        None => raw,
    };
    if code == Some(DispatchReason::AlreadyActive) {
        if let Some(run) = blocking_run {
            return Some(format!("{label}: {run}"));
        }
    }
    Some(match detail.map(str::trim).filter(|d| !d.is_empty()) {
        Some(d) if d.starts_with(label) => d.to_string(),
        Some(d) => format!("{label} — {d}"),
        None => label.to_string(),
    })
}

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
    use ainb_hangar_proto::events::{
        IssueLinkRow, IssueMetadataRow, IssuePropertyRow, ReactionRow,
    };

    use super::*;
    use ainb_hangar_core::ids::{IssueId, TaskId};
    use ainb_plugin_sdk::WireBuffer;

    use crate::test_support::painted_text;

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
            subscriber_count: 0,
            subscribed: false,
            reactions: Vec::new(),
            properties: Vec::new(),
            metadata: Vec::new(),
            last_dispatch_reason: None,
            last_dispatch_detail: None,
            last_dispatch_at: None,
            origin_type: None,
            origin_id: None,
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
            dependencies: Vec::new(),
        }
    }

    fn state_for(issue: IssueRow) -> TaskDetailState {
        TaskDetailState::new(TaskId::from_str("task-1").unwrap(), issue)
    }

    /// One execution-log row started at epoch 0, so an elapsed assertion reads
    /// straight off the render clock the test passes in.
    fn run_row(task_id: &str, agent: &str, state: crate::vocab::RunState) -> RunRow {
        RunRow {
            task_id: task_id.into(),
            short_id: task_id.into(),
            agent: agent.into(),
            state,
            started_at: 0,
            finished_at: None,
            cost_cents: None,
            branch: None,
            pr_url: None,
            pr_status: None,
        }
    }

    /// The card's six metadata values ride ONE line (crisp B4 §2.3), in order,
    /// with no `Status: ` / `Assignee: ` / `Repo: ` labels spending the width —
    /// and the repo reads `@widget`, never the absolute path.
    #[test]
    fn detail_card_renders_one_meta_line_for_a_never_run_issue() {
        let s = state_for(full_issue());
        let mut buf = WireBuffer::new(80, 30);
        render_task_detail(&mut buf, 80, 0, 29, &s, 0);
        let rows = painted_rows(&buf);
        let text = painted_text(&buf);

        assert!(text.contains("📋 HGR-1 · Fix the widget"), "title: {text}");
        let meta = rows
            .iter()
            .find(|l| l.contains("todo"))
            .unwrap_or_else(|| panic!("no meta line:\n{}", rows.join("\n")));
        for want in [
            "todo",
            "P2",
            "agent:alice",
            "created 2023-11-14",
            "@widget",
            "main → release",
        ] {
            assert!(meta.contains(want), "{want:?} on the meta line: {meta}");
        }
        assert!(
            !text.contains("/repos/widget"),
            "the absolute repo path is gone: {text}"
        );
        for gone in ["Status: ", "Assignee: ", "Repo: ", "Source: ", "Target: "] {
            assert!(!text.contains(gone), "{gone:?} label survived: {text}");
        }
        assert!(text.contains("[bug]"), "label chip bug");
        assert!(text.contains("[p0]"), "label chip p0");
        assert!(text.contains("The widget breaks on resize."), "description");
        assert!(
            !text.contains("Runs:"),
            "a never-run issue has no runs line"
        );
    }

    /// An issue with unset fields drops those SEGMENTS from the meta line rather
    /// than printing an em-dash into them (crisp B4 §2.3): the four rows this
    /// replaced printed `Repo: —   Source: — → Target: —` on every fresh issue,
    /// the same zero-information placeholder B2 deleted from the card footer.
    #[test]
    fn meta_line_drops_unset_segments_instead_of_placeholders() {
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
        render_task_detail(&mut buf, 80, 0, 29, &s, 0);
        let rows = painted_rows(&buf);
        let text = painted_text(&buf);

        let meta = rows
            .iter()
            .find(|l| l.contains("unassigned"))
            .unwrap_or_else(|| panic!("no meta line:\n{}", rows.join("\n")));
        assert_eq!(
            meta.matches('·').count(),
            3,
            "four segments, three separators: {meta}"
        );
        assert!(!meta.contains('—'), "no em-dash placeholder: {meta}");
        assert!(!meta.contains('@'), "no repo segment: {meta}");
        assert!(!text.contains("Labels: "), "no labels row: {text}");
        assert!(!text.contains("Due: "), "no due row: {text}");
        assert!(text.contains("no description"), "unset description");
    }

    /// `/home/claude/ainb-e2e-home/projects/boxtrack` → `@boxtrack` (§2.3): the
    /// last segment is the only part that names anything, and a ref that is
    /// already a label passes through with the same marker.
    #[test]
    fn repo_label_names_the_repo_not_the_path() {
        assert_eq!(repo_label("/home/claude/projects/boxtrack"), "@boxtrack");
        assert_eq!(repo_label("/home/claude/projects/boxtrack/"), "@boxtrack");
        assert_eq!(repo_label("boxtrack"), "@boxtrack");
        assert_eq!(repo_label("/"), "@", "a pathological ref still renders");
    }

    /// Crisp B1 review: BEFORE the roster resolves the name, the header degrades
    /// to the ref's short id, never the raw 26-char ULID it used to paint. The
    /// actor kind stays: this row is wide enough, and it says agent or human.
    #[test]
    fn an_unresolved_ulid_assignee_degrades_to_a_short_id() {
        let mut issue = full_issue();
        issue.assignee = Some("agent:01M1FHM2YSRSXZQFR29ZAYF56V".into());
        let s = state_for(issue);
        let mut buf = WireBuffer::new(80, 30);
        render_task_detail(&mut buf, 80, 0, 29, &s, 0);
        let text = painted_text(&buf);

        assert!(text.contains("agent:AYF56V"), "short id: {text}");
        assert!(!text.contains("01M1FHM2"), "raw ULID gone: {text}");
    }

    /// Parity 28: the deadline renders next to the labels when set. Without this
    /// the wizard could author a due date the user could never see. Crisp B4
    /// makes the row conditional: a deadline-less issue paints no `Due:` at all
    /// rather than an em-dash.
    #[test]
    fn detail_card_renders_the_due_date_only_when_set() {
        // Set: the calendar day appears under a `Due:` label.
        let mut issue = full_issue();
        issue.due_date = Some(1_785_542_400_000); // 2026-08-01 UTC midnight
        let s = state_for(issue);
        let mut buf = WireBuffer::new(80, 30);
        render_task_detail(&mut buf, 80, 0, 29, &s, 0);
        let text = painted_text(&buf);
        assert!(text.contains("Due: "), "due label: {text}");
        assert!(text.contains("2026-08-01"), "due value: {text}");

        // Unset: no `Due:` label and no stale date.
        let s = state_for(full_issue());
        let mut buf = WireBuffer::new(80, 30);
        render_task_detail(&mut buf, 80, 0, 29, &s, 0);
        let text = painted_text(&buf);
        assert!(!text.contains("Due: "), "no due label when unset: {text}");
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
        render_task_detail(&mut buf, 80, 0, 29, &s, 0);
        let text = painted_text(&buf);
        assert!(text.contains("Linked: "), "linked label: {text}");
        assert!(text.contains('⧉'), "link glyph");
        assert!(text.contains("acme/api#42"), "linked ref value");

        // Unlinked: no Linked line at all.
        let s = state_for(full_issue());
        let mut buf = WireBuffer::new(80, 30);
        render_task_detail(&mut buf, 80, 0, 29, &s, 0);
        assert!(
            !painted_text(&buf).contains("Linked: "),
            "an unlinked issue shows no Linked line"
        );
    }

    /// 0056 / multica parity #21: a PLATFORM-created card wears an `Origin:`
    /// badge naming the provenance kind; a human-authored (`manual`) or
    /// provenance-less card wears none.
    #[test]
    fn detail_card_renders_origin_badge_only_for_platform_created_cards() {
        for (kind, expected) in [
            ("autopilot", "autopilot"),
            ("comment_mention", "comment mention"),
        ] {
            let mut issue = full_issue();
            issue.origin_type = Some(kind.into());
            issue.origin_id = Some("prov-1".into());
            let s = state_for(issue);
            let mut buf = WireBuffer::new(80, 30);
            render_task_detail(&mut buf, 80, 0, 29, &s, 0);
            let text = painted_text(&buf);
            assert!(text.contains("Origin: "), "origin label for {kind}: {text}");
            assert!(text.contains(expected), "origin value for {kind}: {text}");
        }

        for manual in [Some("manual".to_string()), None] {
            let mut issue = full_issue();
            issue.origin_type = manual.clone();
            let s = state_for(issue);
            let mut buf = WireBuffer::new(80, 30);
            render_task_detail(&mut buf, 80, 0, 29, &s, 0);
            assert!(
                !painted_text(&buf).contains("Origin: "),
                "no badge for {manual:?}"
            );
        }
    }

    /// multica parity #12: a card whose newest dispatch attempt was DECLINED
    /// renders `⚠ Not dispatched: <human label> — <detail>`, so "queued forever
    /// with no explanation" becomes a stated cause on the card the user opens.
    #[test]
    fn detail_card_renders_the_not_dispatched_line() {
        let mut issue = full_issue();
        issue.last_dispatch_reason = Some("runtime_offline".into());
        issue.last_dispatch_detail = Some("task 01J9 queued; runtime rt-1 is offline".into());
        issue.last_dispatch_at = Some(1_700_000_000_000);
        let s = state_for(issue);
        let mut buf = WireBuffer::new(100, 40);
        render_task_detail(&mut buf, 100, 0, 39, &s, 0);
        let text = painted_text(&buf);
        assert!(text.contains("Not dispatched: "), "label: {text}");
        assert!(
            text.contains("runtime offline"),
            "the HUMAN label, not the raw token: {text}"
        );
        assert!(text.contains("runtime rt-1 is offline"), "detail: {text}");
    }

    /// The negative twin (mirroring `detail_card_renders_linked_line_only_when_linked`):
    /// a healthy card paints NO such line, so the card reads exactly as it did
    /// before parity #12.
    #[test]
    fn detail_card_omits_the_not_dispatched_line_when_healthy() {
        let s = state_for(full_issue());
        let mut buf = WireBuffer::new(100, 40);
        render_task_detail(&mut buf, 100, 0, 39, &s, 0);
        assert!(
            !painted_text(&buf).contains("Not dispatched"),
            "a healthy card shows no dispatch warning"
        );
    }

    /// A code this build does not know renders the RAW token rather than hiding
    /// the line or panicking — an older plugin against a newer daemon must still
    /// tell the user something is wrong.
    #[test]
    fn detail_card_renders_an_unknown_dispatch_code_verbatim() {
        let mut issue = full_issue();
        issue.last_dispatch_reason = Some("nonsense_code".into());
        issue.last_dispatch_detail = Some("something new happened".into());
        let s = state_for(issue);
        let mut buf = WireBuffer::new(100, 40);
        render_task_detail(&mut buf, 100, 0, 39, &s, 0);
        let text = painted_text(&buf);
        assert!(
            text.contains("Not dispatched: "),
            "line still painted: {text}"
        );
        assert!(text.contains("nonsense_code"), "raw token kept: {text}");
    }

    /// The line composer itself, unit-level: known code → human label, unknown
    /// code → raw token, empty / absent code → no line at all.
    #[test]
    fn dispatch_decline_line_maps_codes_and_details() {
        assert_eq!(
            dispatch_decline_line(Some("target_unavailable"), Some("no agent"), None),
            Some("no dispatch target — no agent".to_string())
        );
        assert_eq!(
            dispatch_decline_line(Some("deferred"), None, None),
            Some("waiting on blockers".to_string())
        );
        assert_eq!(
            dispatch_decline_line(Some("future_code"), None, None),
            Some("future_code".to_string())
        );
        assert_eq!(
            dispatch_decline_line(None, Some("orphan detail"), None),
            None
        );
        assert_eq!(dispatch_decline_line(Some("   "), None, None), None);
    }

    /// Crisp B1 (defect 5): the daemon's `already_active` detail restates the
    /// label, so the line prints it ONCE; and when the tasks snapshot knows the
    /// active row, the line names it instead of the bare status.
    #[test]
    fn dispatch_decline_line_names_the_blocking_run_once() {
        let detail = Some("a run is already active (queued)");
        assert_eq!(
            dispatch_decline_line(Some("already_active"), detail, None),
            Some("a run is already active (queued)".to_string()),
            "a detail that restates the label is not doubled"
        );
        assert_eq!(
            dispatch_decline_line(
                Some("already_active"),
                detail,
                Some("#1J7MR7 impl-1 (running)")
            ),
            Some("a run is already active: #1J7MR7 impl-1 (running)".to_string()),
            "the blocking row is named when known"
        );
        // A blocking run is only relevant to `already_active`.
        assert_eq!(
            dispatch_decline_line(Some("deferred"), None, Some("#1J7MR7 impl-1 (running)")),
            Some("waiting on blockers".to_string())
        );
    }

    /// Crisp B1 (defect 8): once the glue resolves the roster names, the meta
    /// line paints `alice` over the raw actor ref; the raw ref remains the
    /// fallback until then. The EXECUTING agent moved to the run card (B4 §2.3),
    /// where it names the run rather than repeating the issue's provider token.
    #[test]
    fn meta_line_paints_the_resolved_assignee_over_the_raw_ref() {
        let mut s = state_for(full_issue());
        s.set_resolved_names(Some("alice".into()), Some("impl-1".into()), None);
        let mut buf = WireBuffer::new(80, 30);
        render_task_detail(&mut buf, 80, 0, 29, &s, 0);
        let text = painted_text(&buf);
        assert!(text.contains("alice"), "resolved assignee: {text}");
        assert!(!text.contains("agent:alice"), "raw actor ref gone: {text}");
        assert!(
            !text.contains("codex"),
            "the issue's provider token is not metadata: {text}"
        );
        assert_eq!(s.assignee_name(), Some("alice"));
        assert_eq!(s.agent_name(), Some("impl-1"));
    }

    /// The badge mapper itself: only the two platform kinds earn a badge, and an
    /// unrecognised (future) kind degrades to no badge rather than painting a raw
    /// token.
    #[test]
    fn origin_badge_is_platform_kinds_only() {
        assert_eq!(origin_badge(Some("autopilot")), Some("⚙ autopilot"));
        assert_eq!(
            origin_badge(Some("comment_mention")),
            Some("💬 comment mention")
        );
        assert_eq!(origin_badge(Some("manual")), None);
        assert_eq!(origin_badge(None), None);
        assert_eq!(origin_badge(Some("from_the_future")), None);
    }

    /// One typed link row for the render tests.
    fn link_row(kind: &str, display: &str, title: &str, satisfied: bool) -> IssueLinkRow {
        IssueLinkRow {
            kind: kind.into(),
            issue_id: format!("issue-{display}"),
            display_id: Some(display.into()),
            title: title.into(),
            state: "open".into(),
            satisfied,
        }
    }

    /// multica parity #20: all three kinds render in a `Links:` block, each with
    /// its own glyph — 🔒 for an unfinished blocker, ✓ once it is satisfied,
    /// → for what this card blocks, ~ for a related card.
    #[test]
    fn detail_card_renders_a_links_block_per_kind() {
        let mut issue = full_issue();
        issue.dependencies = vec![
            link_row("blocked_by", "HGR-4", "Build the parser", false),
            link_row("blocked_by", "HGR-3", "Land the schema", true),
            link_row("blocks", "HGR-9", "Ship the CLI", false),
            link_row("related", "HGR-7", "Docs sweep", false),
        ];
        let s = state_for(issue);
        let mut buf = WireBuffer::new(80, 40);
        render_task_detail(&mut buf, 80, 0, 39, &s, 0);
        let text = painted_text(&buf);

        assert!(text.contains("Links:"), "the block header: {text}");
        for want in [
            "🔒 blocked-by HGR-4",
            "✓ blocked-by HGR-3",
            "→ blocks",
            "~ related",
        ] {
            let squashed = text.split_whitespace().collect::<Vec<_>>().join(" ");
            let want_squashed = want.split_whitespace().collect::<Vec<_>>().join(" ");
            assert!(
                squashed.contains(&want_squashed),
                "missing {want:?} in {text}"
            );
        }
        assert!(text.contains("Build the parser"), "the link title: {text}");
        assert!(text.contains("Docs sweep"), "the related title: {text}");
    }

    /// An issue with NO links renders no `Links:` line at all — an old daemon
    /// sends no `dependencies`, so the card degrades to exactly today's render.
    #[test]
    fn detail_card_omits_the_links_block_when_empty() {
        let s = state_for(full_issue());
        let mut buf = WireBuffer::new(80, 40);
        render_task_detail(&mut buf, 80, 0, 39, &s, 0);
        assert!(
            !painted_text(&buf).contains("Links:"),
            "no links ⇒ no block (an old daemon leaves the card unchanged)"
        );
    }

    /// multica parity #22: the watcher count (with the `✓ you` marker) and the
    /// aggregated reaction buckets both render on the detail card.
    #[test]
    fn detail_card_renders_a_subs_and_react_block() {
        let mut issue = full_issue();
        issue.subscriber_count = 3;
        issue.subscribed = true;
        issue.reactions = vec![
            ReactionRow {
                emoji: "👍".into(),
                count: 3,
                mine: true,
            },
            ReactionRow {
                emoji: "🎉".into(),
                count: 1,
                mine: false,
            },
        ];
        let s = state_for(issue);
        let mut buf = WireBuffer::new(80, 40);
        render_task_detail(&mut buf, 80, 0, 39, &s, 0);
        let squashed = painted_text(&buf).split_whitespace().collect::<Vec<_>>().join(" ");

        assert!(squashed.contains("Subs: 3"), "the count: {squashed}");
        assert!(squashed.contains("✓ you"), "the you-marker: {squashed}");
        assert!(
            squashed.contains("React: 👍 3"),
            "the mine bucket: {squashed}"
        );
        assert!(squashed.contains("🎉 1"), "the other bucket: {squashed}");
    }

    /// A pre-#22 daemon sends neither field, so the card renders exactly as it
    /// does today — no `Subs:` line and no `React:` line.
    #[test]
    fn detail_card_omits_subs_and_react_when_empty() {
        let s = state_for(full_issue());
        let mut buf = WireBuffer::new(80, 40);
        render_task_detail(&mut buf, 80, 0, 39, &s, 0);
        let text = painted_text(&buf);
        assert!(!text.contains("Subs:"), "no subscribers ⇒ no line: {text}");
        assert!(!text.contains("React:"), "no reactions ⇒ no line: {text}");
    }

    /// multica parity #17: an issue's resolved CUSTOM PROPERTIES render as a
    /// `Props:` header followed by one `◆ Name: value` line per definition, in
    /// the catalog order the daemon sent.
    #[test]
    fn detail_card_renders_a_props_block_in_catalog_order() {
        let mut issue = full_issue();
        issue.properties = vec![
            IssuePropertyRow {
                key: "sprint".into(),
                name: "Sprint".into(),
                kind: "select".into(),
                value: "S2".into(),
            },
            IssuePropertyRow {
                key: "owner".into(),
                name: "Owner".into(),
                kind: "text".into(),
                value: "amy".into(),
            },
        ];
        let s = state_for(issue);
        let mut buf = WireBuffer::new(80, 40);
        render_task_detail(&mut buf, 80, 0, 39, &s, 0);
        let squashed = painted_text(&buf).split_whitespace().collect::<Vec<_>>().join(" ");

        assert!(squashed.contains("Props:"), "the header: {squashed}");
        let sprint = squashed
            .find("◆ Sprint: S2")
            .unwrap_or_else(|| panic!("the sprint line: {squashed}"));
        let owner = squashed
            .find("◆ Owner: amy")
            .unwrap_or_else(|| panic!("the owner line: {squashed}"));
        assert!(sprint < owner, "catalog order is preserved: {squashed}");
    }

    /// multica parity #17: the AGENT METADATA scratch bag renders as a `Meta:`
    /// header followed by one `· key = value` line per entry.
    #[test]
    fn detail_card_renders_a_meta_block() {
        let mut issue = full_issue();
        issue.metadata = vec![IssueMetadataRow {
            key: "pr_number".into(),
            value_json: "42".into(),
            value: "42".into(),
        }];
        let s = state_for(issue);
        let mut buf = WireBuffer::new(80, 40);
        render_task_detail(&mut buf, 80, 0, 39, &s, 0);
        let squashed = painted_text(&buf).split_whitespace().collect::<Vec<_>>().join(" ");

        assert!(squashed.contains("Meta:"), "the header: {squashed}");
        assert!(
            squashed.contains("· pr_number = 42"),
            "the entry: {squashed}"
        );
    }

    /// A pre-#17 daemon sends neither field, so the card renders exactly as it
    /// does today — no `Props:` line and no `Meta:` line.
    #[test]
    fn detail_card_omits_props_and_meta_when_empty() {
        let s = state_for(full_issue());
        let mut buf = WireBuffer::new(80, 40);
        render_task_detail(&mut buf, 80, 0, 39, &s, 0);
        let text = painted_text(&buf);
        assert!(!text.contains("Props:"), "no properties ⇒ no line: {text}");
        assert!(!text.contains("Meta:"), "no metadata ⇒ no line: {text}");
    }

    /// DECOY: a property whose RENDERED value is empty still shows its name and
    /// never paints a bare `◆ :` with a dangling colon.
    #[test]
    fn detail_card_keeps_the_name_when_a_property_value_is_empty() {
        let mut issue = full_issue();
        issue.properties = vec![IssuePropertyRow {
            key: "risk".into(),
            name: "Risk".into(),
            kind: "text".into(),
            value: String::new(),
        }];
        let s = state_for(issue);
        let mut buf = WireBuffer::new(80, 40);
        render_task_detail(&mut buf, 80, 0, 39, &s, 0);
        let squashed = painted_text(&buf).split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(
            squashed.contains("◆ Risk:"),
            "the name survives: {squashed}"
        );
        assert!(
            !squashed.contains("◆ :"),
            "no dangling bare marker: {squashed}"
        );
    }

    /// A subscriber count with the LOCAL HUMAN absent renders the count but no
    /// `✓ you` marker.
    #[test]
    fn detail_card_omits_the_you_marker_when_not_subscribed() {
        let mut issue = full_issue();
        issue.subscriber_count = 2;
        issue.subscribed = false;
        let s = state_for(issue);
        let mut buf = WireBuffer::new(80, 40);
        render_task_detail(&mut buf, 80, 0, 39, &s, 0);
        let squashed = painted_text(&buf).split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(squashed.contains("Subs: 2"), "the count: {squashed}");
        assert!(!squashed.contains("✓ you"), "not subscribed: {squashed}");
    }

    /// A fresh unchecked criterion with a deterministic id.
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
        render_task_detail(&mut buf, 80, 0, 29, &s, 0);
        let text = painted_text(&buf);
        assert!(text.contains("Acceptance:"), "acceptance header: {text}");
        assert!(text.contains("builds"), "first criterion");
        assert!(text.contains("tests green"), "second criterion");

        // Empty list: no header at all.
        let s = state_for(full_issue());
        let mut buf = WireBuffer::new(80, 30);
        render_task_detail(&mut buf, 80, 0, 29, &s, 0);
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
        render_task_detail(&mut buf, 80, 0, 29, &s, 0);
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
        render_task_detail(&mut buf, 80, 0, 29, &s, 0);
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
        render_task_detail(&mut buf, 80, 0, 29, &s, 0);
        let text = painted_text(&buf);
        assert!(text.contains("Acceptance: 0/2"), "header: {text}");
        assert!(
            text.contains("builds") && text.contains("tests green"),
            "{text}"
        );
        assert!(
            !text.contains('☑'),
            "an old daemon's rows are unchecked: {text}"
        );
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
        render_task_detail(&mut buf, 80, 0, 29, &selected, 0);
        let rows = painted_rows(&buf);
        let line = rows.iter().find(|l| l.contains("builds")).expect("criterion line");
        assert!(line.contains('▶'), "marker on the selected line: {line}");
        let other = rows.iter().find(|l| l.contains("tests green")).expect("criterion line");
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
        render_task_detail(&mut buf, 80, 0, 29, &s, 0);
        let text = painted_text(&buf);
        assert!(text.contains("Context:"), "context header: {text}");
        assert!(text.contains("acme/api#42"), "first reference");
        assert!(text.contains("https://docs"), "second reference");

        // Empty list: no header at all.
        let s = state_for(full_issue());
        let mut buf = WireBuffer::new(80, 30);
        render_task_detail(&mut buf, 80, 0, 29, &s, 0);
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
        render_task_detail(&mut buf, 80, 0, 29, &s, 0);
        let text = painted_text(&buf);

        assert!(text.contains("Runs: 3"), "run count: {text}");
        assert!(text.contains("last: running"), "latest run status");
        assert!(text.contains("2023-11-14"), "latest run date");
    }

    /// HEIGHT CONTRACT regression (the coordinator's Finding 1): at a short
    /// viewport (the 8-row snapshot panes) the card is skipped entirely and the
    /// RUN HEAD paints bare on row 0 — the card must never squeeze the feed out,
    /// and the run must never be the thing that goes missing when it does yield.
    #[test]
    fn detail_card_yields_to_the_run_head_and_transcript_at_short_viewports() {
        let mut issue = full_issue();
        issue.pr_url = Some("https://github.com/o/r/pull/7".into());
        let mut s = state_for(issue);
        s.set_runs(vec![run_row(
            "task-1",
            "impl-1",
            crate::vocab::RunState::Running,
        )]);
        let mut buf = WireBuffer::new(100, 8);
        render_task_detail(&mut buf, 100, 0, 8, &s, 60_000);
        let rows = painted_rows(&buf);
        let text = rows.join("\n");

        assert!(!text.contains('╭'), "no card border at 8 rows: {text}");
        assert!(
            rows[0].contains("◔ impl-1 is working"),
            "the run card is pinned to row 0: {}",
            rows[0]
        );
        assert!(rows[1].contains("PR #7"), "the PR rides row 1: {}", rows[1]);
    }

    /// When the delete-confirm modal is open the bottom row paints the red
    /// delete prompt naming the bound issue plus the confirm/cancel hint (63l.5).
    #[test]
    fn delete_modal_paints_confirm_bar_with_target() {
        let mut s = state_for(full_issue());
        s = reduce_task_detail(&s, TaskDetailEvent::Key('x')).state;
        assert!(s.delete_modal_open(), "x opened the delete modal");
        let mut buf = WireBuffer::new(80, 30);
        render_task_detail(&mut buf, 80, 0, 29, &s, 0);
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

    /// Every wire status the store can persist maps onto a lifecycle, and the
    /// terminal ones gate `R` on. An unknown status maps to `None` (caller keeps
    /// its current value) — a NEW TaskStatus variant must be added here or the
    /// seeded screen silently regresses to a dead `R` again.
    #[test]
    fn lifecycle_seeds_from_every_wire_status() {
        use ainb_hangar_core::task_status::TaskStatus;
        // Driven off the enum's own roster so a new variant reaches this loop.
        for status in TaskStatus::ALL {
            let got = TaskLifecycle::from_wire_status(status.as_str())
                .unwrap_or_else(|| panic!("status `{}` unmapped", status.as_str()));
            assert_eq!(
                got.is_terminal(),
                status.is_terminal(),
                "terminal gate for `{}` must agree with the store",
                status.as_str()
            );
        }
        assert_eq!(
            TaskLifecycle::from_wire_status("queued"),
            Some(TaskLifecycle::Queued)
        );
        assert_eq!(
            TaskLifecycle::from_wire_status("dispatched"),
            Some(TaskLifecycle::Queued)
        );
        assert_eq!(
            TaskLifecycle::from_wire_status("running"),
            Some(TaskLifecycle::Running)
        );
        assert_eq!(
            TaskLifecycle::from_wire_status("done"),
            Some(TaskLifecycle::Succeeded)
        );
        assert_eq!(
            TaskLifecycle::from_wire_status("failed"),
            Some(TaskLifecycle::Failed)
        );
        assert_eq!(
            TaskLifecycle::from_wire_status("cancelled"),
            Some(TaskLifecycle::Cancelled)
        );
        assert_eq!(TaskLifecycle::from_wire_status("nonsense"), None);
    }
}
