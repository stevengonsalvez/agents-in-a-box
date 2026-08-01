//! The board auto-move dispatch hook (P4 / D8).
//!
//! When a claimed task walks its FSM (`dispatched → running → done | failed`),
//! the claim loop calls [`auto_move_after_transition`] with the new status. For
//! the task's issue, every board carrying it as a card moves the card to the
//! column whose `fsm_state` matches — but only when both the board's master
//! `auto_move` toggle and the target column's `auto_move` flag are on
//! ([`BoardRepo::auto_move_on_state`]). This is the D8 "column↔FSM-state mapping +
//! per-board auto-move toggle" made real: a card slides to `Done` (green) the
//! moment its work succeeds, without the user dragging anything.
//!
//! The TUI reflects the move on its next `hangar/boards_list` pull (which it
//! triggers off the already-emitted `TaskStarted` / `TaskFinished` events), so no
//! new event type is needed here — the hook's whole job is the durable card move.
//!
//! # Best-effort
//!
//! Every fault (a chat task with no issue, a malformed workspace id, a store
//! error) is logged and swallowed: the task's terminal FSM state has already
//! committed, and an auto-move failure must never down the claim loop. A task
//! with no matching auto-move column is a silent no-op.

use ainb_hangar_core::activity::{ActivityAction, ActivityActor};
use ainb_hangar_core::actor::{ActorKind, ActorRef};
use ainb_hangar_core::clock::SystemClock;
use ainb_hangar_core::idgen::SystemIdGen;
use ainb_hangar_core::ids::WorkspaceId;
use ainb_hangar_proto::lifecycle::IssueLifecycle;
use ainb_hangar_store::repo::board::BoardRepo;
use ainb_hangar_store::repo::card_dependency::CardDependencyRepo;
use ainb_hangar_store::repo::issue::IssueRepo;
use ainb_hangar_store::repo::task::Task;
use ainb_hangar_store::service::activity::ActivityService;
use sqlx::SqlitePool;

use crate::events::EventSink;

/// Move the task's issue card to the `new_state`-matched auto-move column of every
/// board that carries it (P4 / D8). Best-effort; see the module docs.
///
/// A task with no `issue_id` (an ad-hoc / chat task) is skipped — there is no
/// card to move. `new_state` is a task-status token (`running` / `done` /
/// `failed` / …), matched against each column's `fsm_state`.
pub async fn auto_move_after_transition(pool: &SqlitePool, task: &Task, new_state: &str) {
    let Some(issue_id) = task.issue_id.as_deref() else {
        return;
    };
    let Ok(ws) = WorkspaceId::from_str(task.workspace_id.clone()) else {
        tracing::warn!(task_id = %task.id, "board auto-move: empty workspace id; skipping");
        return;
    };
    match BoardRepo::auto_move_on_state(pool, &ws, issue_id, new_state).await {
        Ok(moved) if !moved.is_empty() => {
            for m in &moved {
                tracing::info!(
                    task_id = %task.id,
                    issue_id = %issue_id,
                    board_id = %m.board_id,
                    column_id = %m.column_id,
                    state = new_state,
                    "board card auto-moved"
                );
            }
        }
        // No board maps this state for this issue: either no card carries the
        // issue, the target column is already where the card sits, or the move
        // UPDATE matched 0 rows. Logged (not silent) so a card that FAILS to
        // auto-move on CI is diagnosable — the earlier silent no-op hid exactly
        // the linux-only "card stranded in Todo" failure this guards.
        Ok(_) => {
            tracing::info!(
                task_id = %task.id,
                issue_id = %issue_id,
                state = new_state,
                "board auto-move: no card moved (no matching auto-move column, or already there)"
            );
        }
        Err(e) => {
            tracing::warn!(error = %e, task_id = %task.id, "board auto-move failed; proceeding");
        }
    }
}

/// Forward-advance the task's ISSUE lifecycle `state` to match a task-status
/// token — the durable-issue twin of [`auto_move_after_transition`].
///
/// The default hangar issue board buckets cards by `issue.state` alone
/// (`hangar/issues_list` serves it verbatim), while the board auto-move only
/// touches durable `board_card` rows — which the default screen never
/// materialises. So a plain dispatched task walked its FSM (`running` → `done`)
/// with its issue frozen at `todo`/`open`, stranding the card in Todo through the
/// whole run and after it finished. This seam makes `issue.state` authoritative:
/// the running transition promotes the issue to `in_progress` and terminal
/// success to `done`, so the next snapshot re-pull renders the correct column.
///
/// `new_state` is a task-status token (`running` / `done` / …). Only the two that
/// have an issue-lifecycle meaning advance the issue:
/// * `running` → [`IssueLifecycle::InProgress`]
/// * `done`    → [`IssueLifecycle::Done`]
///
/// Every other token (`failed` / `cancelled` / an unknown) is a deliberate no-op:
/// a failed run must NOT mark its issue `done`, and the issue simply stays where
/// it is.
///
/// # Advance-only
///
/// The write is guarded to only ever move the issue FORWARD along the WORKFLOW
/// PROGRESS rank ([`IssueLifecycle::advance_rank`]), and never to revive a
/// terminal issue ([`IssueLifecycle::is_terminal`] — `done` / `cancelled`). This
/// mirrors the `if issue.state == …` guard the PR-merge snapshot uses
/// (snapshots.rs) so a manually-set or PR-merged terminal state is never
/// regressed — e.g. a re-run's `running` transition can never drag an
/// already-`done` issue back to `in_progress`.
///
/// The rank is deliberately NOT [`IssueLifecycle::order`]: `blocked` is APPENDED
/// at column 5, to the right of `done`, so comparing column order would freeze a
/// blocked issue forever (every later transition would read as "behind" it).
/// `advance_rank` ranks `blocked` with `todo`, so unblocking and running an issue
/// still promotes it.
///
/// # Best-effort
///
/// A NULL-issue chat task, a missing issue row, or a store fault is logged and
/// swallowed exactly like the board auto-move: the task's FSM state has already
/// committed and a lifecycle write must never down the claim loop.
pub async fn advance_issue_lifecycle_after_transition(
    pool: &SqlitePool,
    task: &Task,
    new_state: &str,
) {
    let Some(issue_id) = task.issue_id.as_deref() else {
        return;
    };
    let target = match new_state {
        "running" => IssueLifecycle::InProgress,
        "done" => IssueLifecycle::Done,
        // `failed` / `cancelled` / unknown carry no forward issue-lifecycle
        // meaning — leave the issue where it is.
        //
        // DELIBERATELY NOT WIRED: a `cancelled` TASK must not set its issue to
        // the `cancelled` STATE. A cancelled run is normally followed by a
        // re-run; whether the ISSUE is abandoned is a human/agent decision, not
        // a consequence of one run being killed.
        _ => return,
    };
    let current = match IssueRepo::get_by_id(pool, issue_id).await {
        Ok(Some(issue)) => issue,
        // The issue vanished (or never existed) — nothing to advance.
        Ok(None) => return,
        Err(e) => {
            tracing::warn!(error = %e, task_id = %task.id, "issue lifecycle advance: reading issue failed; proceeding");
            return;
        }
    };
    let current_status = IssueLifecycle::for_state(&current.state);
    // A TERMINAL issue (done / cancelled) is never revived by a run transition:
    // a re-run's `running` must not drag a merged issue back to `in_progress`,
    // and an abandoned issue stays abandoned until a human moves it.
    if current_status.is_terminal() {
        return;
    }
    // Advance-only, by WORKFLOW PROGRESS rather than board column order:
    // `blocked` paints at column 5 (right of `done`) but ranks with `todo`, so
    // comparing `order()` here would freeze a blocked issue forever. Same-rank
    // targets are an idempotent no-op.
    if target.advance_rank() <= current_status.advance_rank() {
        return;
    }
    match IssueRepo::update_state(pool, issue_id, target.as_str()).await {
        Ok(()) => {
            // multica parity #13: a daemon-driven move is a `system` activity row
            // carrying `via:"board"`, so the narrative distinguishes it from an
            // owner edit. Best-effort — never fails the advance.
            ActivityService::record(
                pool,
                &SystemIdGen,
                &SystemClock,
                &current.workspace_id,
                issue_id,
                &ActivityActor::System,
                ActivityAction::StatusChanged,
                serde_json::json!({
                    "from": current.state,
                    "to": target.as_str(),
                    "via": "board",
                }),
            )
            .await;
            tracing::info!(
                task_id = %task.id,
                issue_id = %issue_id,
                from = %current.state,
                to = target.as_str(),
                "issue lifecycle advanced"
            );
        }
        Err(e) => {
            tracing::warn!(error = %e, task_id = %task.id, "issue lifecycle advance failed; proceeding");
        }
    }
}

/// Forward-advance the task's ISSUE lifecycle by its issue's AGGREGATE terminal
/// outcome — the durable-issue twin of [`auto_move_after_terminal`], gated on the
/// same drain.
///
/// A squad card fans out N tasks onto one issue, so a single sibling reaching
/// `done` must not mark the whole issue `done` while others still run.
/// [`TaskRepo::issue_aggregate_terminal_state`] returns `None` until the active
/// set drains, then the aggregate token (`failed` > `cancelled` > `done`). Only
/// the aggregate `done` promotes the issue (via
/// [`advance_issue_lifecycle_after_transition`], which no-ops on `failed` /
/// `cancelled`), so a failed or still-running set never advances the issue.
///
/// Best-effort — every fault is logged and swallowed (the task's terminal state
/// has already committed).
pub async fn advance_issue_lifecycle_after_terminal(pool: &SqlitePool, task: &Task) {
    use ainb_hangar_store::repo::task::TaskRepo;

    let Some(issue_id) = task.issue_id.as_deref() else {
        return;
    };
    let Ok(ws) = WorkspaceId::from_str(task.workspace_id.clone()) else {
        tracing::warn!(task_id = %task.id, "issue lifecycle terminal advance: empty workspace id; skipping");
        return;
    };
    match TaskRepo::issue_aggregate_terminal_state(pool, ws.as_str(), issue_id).await {
        // Active set not drained — a sibling still runs; do not advance yet.
        Ok(None) => {}
        Ok(Some(state)) => advance_issue_lifecycle_after_transition(pool, task, &state).await,
        Err(e) => {
            tracing::warn!(error = %e, task_id = %task.id, "issue lifecycle terminal advance: aggregate read failed");
        }
    }
}

/// Auto-move the task's issue card by its issue's AGGREGATE terminal outcome — but
/// ONLY once the issue's active set has DRAINED (tcp T4 / FANOUT-SEMANTICS).
///
/// A squad card fans out N tasks onto one issue, so keying the terminal auto-move on
/// a single task's transition slid the card to `done`/`failed` while the leader +
/// other members were still running. This gates on the whole set:
/// [`TaskRepo::issue_aggregate_terminal_state`] returns `None` while ANY sibling is
/// still active (so nothing moves), and otherwise the aggregate token
/// (`failed` > `cancelled` > `done`) the card should land on. Only the LAST sibling
/// to finish therefore moves the card, and it moves to the aggregate column.
///
/// Idempotent: [`BoardRepo::auto_move_on_state`] skips a card already in the target
/// column, so two sibling finalizers that race to an empty active set compute the
/// same aggregate and move to the same column at most once. Best-effort — every
/// fault is logged and swallowed (the task's terminal state has already committed).
pub async fn auto_move_after_terminal(pool: &SqlitePool, task: &Task) {
    use ainb_hangar_store::repo::task::TaskRepo;

    let Some(issue_id) = task.issue_id.as_deref() else {
        return;
    };
    let Ok(ws) = WorkspaceId::from_str(task.workspace_id.clone()) else {
        tracing::warn!(task_id = %task.id, "board terminal auto-move: empty workspace id; skipping");
        return;
    };
    let state = match TaskRepo::issue_aggregate_terminal_state(pool, ws.as_str(), issue_id).await {
        // The active set has not drained yet — a sibling still runs, so the card
        // must not terminal-move. Logged (not silent) so a card that never moves
        // because the aggregate read the issue as still-active is diagnosable on
        // CI (distinguishes an un-drained gate from a failed move UPDATE).
        Ok(None) => {
            tracing::info!(
                task_id = %task.id,
                issue_id = %issue_id,
                "board terminal auto-move: issue active set not drained; card stays put"
            );
            return;
        }
        Ok(Some(s)) => s,
        Err(e) => {
            tracing::warn!(error = %e, task_id = %task.id, "board terminal auto-move: aggregate read failed");
            return;
        }
    };
    auto_move_after_transition(pool, task, &state).await;

    // PIPELINE STAGE ADVANCE. Folded in here rather than added at each of the
    // five terminal call sites, so a future terminal path cannot acquire the
    // auto-move hook and silently miss the advance, which would strand a card
    // mid-pipeline with its stage already finished.
    //
    // Only a `done` aggregate advances: a stage that FAILED or was CANCELLED
    // leaves the card in its column so the work can be retried by the same role
    // rather than being handed to the next stage half-finished.
    if state == "done" {
        advance_pipeline_stage(pool, task).await;
    }
}

/// Step the task's card ONE role-gated column to the right now its stage is done.
///
/// This is the handoff that makes a pipeline a pipeline: Implement finishes, the
/// card lands in Review, and on the next daemon tick a DIFFERENT role pulls it.
///
/// Best-effort, exactly like its sibling hooks: the task's terminal state has
/// already committed, and a board write must never down the claim loop. A card
/// that is not in a role-gated column, that is at the last stage, or whose board
/// has auto-move off is a logged no-op.
async fn advance_pipeline_stage(pool: &SqlitePool, task: &Task) {
    use ainb_hangar_store::service::pull::PullService;

    let Some(issue_id) = task.issue_id.as_deref() else {
        return;
    };
    match PullService::advance_after_stage(pool, issue_id).await {
        Ok(moved) if !moved.is_empty() => {
            for (board_id, column_id) in &moved {
                tracing::info!(
                    task_id = %task.id,
                    issue_id = %issue_id,
                    board_id = %board_id,
                    column_id = %column_id,
                    "pipeline card advanced to the next stage"
                );
            }
        }
        Ok(_) => tracing::info!(
            task_id = %task.id,
            issue_id = %issue_id,
            "pipeline advance: no card moved (not role-gated, last stage, or auto-move off)"
        ),
        Err(e) => tracing::warn!(
            error = %e, task_id = %task.id, "pipeline advance failed"
        ),
    }
}

/// When ANY task of a card reaches a TERMINAL state, re-evaluate every card that
/// DEPENDS on it (tcp T4 / F7 + FANOUT-SEMANTICS): a dependent whose blockers are now
/// all finished becomes RUNNABLE, and — only if its `auto_run` flag is on and it has
/// no active run — is auto-launched via the shared [`run_card`](crate::rpc::run_card)
/// core.
///
/// This fires on ANY terminal transition (`done` / `failed` / `cancelled`), not just
/// `done`, because a squad blocker only becomes FINISHED when its WHOLE active set
/// drains — and the last sibling to drain it may be a `failed`/`cancelled` one even
/// though an earlier sibling reached `done`. The decision is delegated to
/// [`CardDependencyRepo::unfinished_blockers_of`], which encodes the finished
/// contract (all-terminal ∧ any-done), so calling this on a non-`done` terminal is
/// safe: a blocker that did not actually finish (no `done`, or still draining) leaves
/// the dependent's blocker list non-empty and nothing unblocks.
///
/// The RUNNABLE visual state needs no new event: the just-finished blocker already
/// pushed a `TaskFinished` (the plugin re-pulls `boards_list` on it), and the fresh
/// snapshot renders the dependent with an empty `blocked_by` (the 🔒 clears). This
/// hook only drives the OPTIONAL auto-run so the board never has to.
///
/// # Best-effort
///
/// Every fault is logged and swallowed — the blocker's terminal state has already
/// committed, and a dependency-unblock failure must never down the claim loop. A
/// dependent that is still blocked by another card, that has no `auto_run`, or that
/// already has an active run is a silent no-op.
pub async fn unblock_dependents_after_terminal(pool: &SqlitePool, task: &Task) {
    let Some(blocker_issue_id) = task.issue_id.as_deref() else {
        return;
    };
    let Ok(ws) = WorkspaceId::from_str(task.workspace_id.clone()) else {
        return;
    };

    let dependents = match CardDependencyRepo::dependents_of(pool, blocker_issue_id).await {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!(error = %e, task_id = %task.id, "unblock: reading dependents failed");
            return;
        }
    };
    for dep in dependents {
        // Still blocked by another card? Then this completion did not unblock it.
        match CardDependencyRepo::unfinished_blockers_of(pool, &dep).await {
            Ok(remaining) if !remaining.is_empty() => continue,
            Ok(_) => {}
            Err(e) => {
                tracing::warn!(error = %e, dependent = %dep, "unblock: reading blockers failed");
                continue;
            }
        }
        tracing::info!(dependent = %dep, blocker = %blocker_issue_id, "card unblocked (last blocker done)");

        // Auto-run only when the dependent opted in (default OFF: explicit run stays
        // the default). The claim loop's concurrency caps bound any chain of
        // auto-runs — enqueue is cheap, dispatch is capped.
        match CardDependencyRepo::get_auto_run(pool, &dep).await {
            Ok(true) => auto_run_dependent(pool, &ws, &dep).await,
            Ok(false) => {}
            Err(e) => {
                tracing::warn!(error = %e, dependent = %dep, "unblock: reading auto_run failed")
            }
        }
    }
}

/// Auto-launch a now-runnable dependent card through the shared [`run_card`] core.
/// A benign refusal (still blocked, or already has an active run) is logged at
/// debug and skipped — the card is simply not launchable right now, not broken.
async fn auto_run_dependent(pool: &SqlitePool, ws: &WorkspaceId, issue_id: &str) {
    use ainb_hangar_store::repo::issue::IssueRepo;

    let issue = match IssueRepo::get_by_id(pool, issue_id).await {
        Ok(Some(i)) if i.workspace_id == ws.as_str() => i,
        Ok(_) => return,
        Err(e) => {
            tracing::warn!(error = %e, issue = %issue_id, "auto-run: loading issue failed");
            return;
        }
    };
    // Board-agnostic (the auto-run seam does not know which board); the F4 board
    // tier is skipped, the cascade still resolves the agent. Headless run.
    match crate::rpc::run_card(
        pool,
        ws,
        None,
        &issue,
        "headless",
        None,
        None,
        None,
        None,
        None,
        // multica parity #12: the dependency cascade is an AUTO_RUN trigger
        // surface. Its refusals used to be a `debug!` line and nothing else.
        ainb_hangar_core::dispatch_reason::DispatchSource::AutoRun,
    )
    .await
    {
        Ok(_) => tracing::info!(issue = %issue_id, "card auto-run launched (last blocker done)"),
        Err(crate::rpc::CardRunError::Blocked(_) | crate::rpc::CardRunError::ActiveRun(_)) => {
            tracing::debug!(issue = %issue_id, "auto-run skipped (not launchable right now)");
        }
        Err(_) => {
            tracing::warn!(issue = %issue_id, "auto-run refused (no repo/agent or store fault)")
        }
    }
}

/// Record a task's terminal OUTCOME on its issue's narrative (multica parity
/// #13, write sites 8 + 9).
///
/// Attributed to `agent:<agent_id>` — the run's own agent, matching multica's
/// listeners, which force the actor for `task:completed` / `task:failed`
/// regardless of who triggered the run. `details` carries the task id (additive
/// over multica's `{}`; the column is free-form).
///
/// Best-effort and no-op for a chat task with no issue: the task's terminal
/// state has already committed and an audit write must never down the run loop.
pub async fn record_task_outcome(pool: &SqlitePool, task: &Task, completed: bool) {
    let Some(issue_id) = task.issue_id.as_deref() else {
        return;
    };
    let actor = ActorRef::new(ActorKind::Agent, task.agent_id.clone())
        .map_or(ActivityActor::System, ActivityActor::Actor);
    let action = if completed {
        ActivityAction::TaskCompleted
    } else {
        ActivityAction::TaskFailed
    };
    ActivityService::record(
        pool,
        &SystemIdGen,
        &SystemClock,
        &task.workspace_id,
        issue_id,
        &actor,
        action,
        serde_json::json!({ "task_id": task.id }),
    )
    .await;
}

/// Advance the issue lifecycle, then cascade a completed sub-issue to its parent.
///
/// Fires the child-done → parent cascade (migration 0046) after the aggregate
/// terminal advance when that completion moved a **sub-issue** into a terminal
/// state — the agent-run twin of the TUI/CLI `issue_update` cascade path.
///
/// Captures the issue's `state` around [`advance_issue_lifecycle_after_terminal`]
/// so the cascade sees the real non-terminal → terminal transition (a squad
/// fan-out whose set has not drained leaves the issue non-terminal, so nothing
/// cascades until the last sibling finishes). Best-effort throughout — every fault
/// is logged and swallowed, exactly like the advance it wraps.
pub async fn advance_and_cascade_child(pool: &SqlitePool, task: &Task, events: &EventSink) {
    let prev_state = match task.issue_id.as_deref() {
        Some(id) => match IssueRepo::get_by_id(pool, id).await {
            Ok(Some(issue)) => Some(issue.state),
            _ => None,
        },
        None => None,
    };
    advance_issue_lifecycle_after_terminal(pool, task).await;
    let (Some(child_id), Some(prev)) = (task.issue_id.as_deref(), prev_state) else {
        return;
    };
    let Ok(ws) = WorkspaceId::from_str(task.workspace_id.clone()) else {
        return;
    };
    let new_state = match IssueRepo::get_by_id(pool, child_id).await {
        Ok(Some(issue)) => issue.state,
        _ => return,
    };
    maybe_cascade_child_done(pool, &ws, child_id, &prev, &new_state, events).await;
}

/// Run the store-side child-done cascade + wake the parent on a fired cascade.
///
/// For `child_id`'s `prev → new` transition, on a fired cascade this pushes the
/// parent's new comment as a live `CommentAdded` event AND (when the parent's
/// assignee is an agent) wakes it through the shared
/// [`run_card`](crate::rpc::run_card) core. Shared by the agent-run seam
/// ([`advance_and_cascade_child`]) and the TUI/CLI `issue_update` handler.
///
/// Best-effort: the store cascade (the comment write) is the durable side; a
/// failed event push or a refused parent wake leaves the comment committed. Only a
/// store fault is logged — the child's terminal state has already committed and a
/// cascade fault must never down the caller.
pub async fn maybe_cascade_child_done(
    pool: &SqlitePool,
    ws: &WorkspaceId,
    child_id: &str,
    prev_state: &str,
    new_state: &str,
    events: &EventSink,
) {
    use ainb_hangar_core::clock::{HangarClock as _, SystemClock};
    use ainb_hangar_core::idgen::{IdGen as _, SystemIdGen};
    use ainb_hangar_store::service::child_done::cascade_child_done;

    let now_ms = SystemClock.now_ms();
    let comment_id = SystemIdGen.new_ulid();
    let cascade = match cascade_child_done(
        pool,
        ws.as_str(),
        child_id,
        prev_state,
        new_state,
        now_ms,
        comment_id,
    )
    .await
    {
        Ok(Some(c)) => c,
        Ok(None) => return,
        Err(e) => {
            tracing::warn!(error = %e, child = %child_id, "child-done cascade: store fault; skipping");
            return;
        }
    };
    deliver_cascade(pool, ws, &cascade, now_ms, events).await;
}

/// Deliver a FIRED cascade: push its comment as a live event and wake the parent.
///
/// The daemon-only tail of the cascade, shared by the single-child seam
/// ([`maybe_cascade_child_done`]) and the batch handler
/// (`hangar/issues_batch_update`) so the two cannot drift — a batch that
/// aggregates N completions into ONE comment must also emit exactly ONE
/// `CommentAdded` and run exactly ONE parent wake.
///
/// Best-effort: the comment is already durable, so a malformed id only skips the
/// event and a refused wake is logged, never propagated.
pub async fn deliver_cascade(
    pool: &SqlitePool,
    ws: &WorkspaceId,
    cascade: &ainb_hangar_store::service::child_done::ParentCascade,
    now_ms: i64,
    events: &EventSink,
) {
    use ainb_hangar_core::actor::ActorKind;
    use ainb_hangar_proto::events::{CommentRow, HangarEvent};

    // Push the parent's new comment so a subscribed parent-detail view refreshes
    // live (best-effort: a malformed id only skips the event, not the wake).
    if let (Ok(cid), Ok(iid)) = (
        ainb_hangar_core::ids::CommentId::from_str(cascade.comment_id.clone()),
        ainb_hangar_core::ids::IssueId::from_str(cascade.parent_id.clone()),
    ) {
        events.emit(
            ws.as_str(),
            HangarEvent::CommentAdded(CommentRow {
                id: cid,
                issue_id: iid,
                author: format!(
                    "{}:{}",
                    cascade.comment_author.kind().as_str(),
                    cascade.comment_author.id()
                ),
                body: cascade.comment_body.clone(),
                created_at: now_ms,
                parent_id: None,
            }),
        );
    }

    // Wake the parent's assignee when it is an agent (a squad leader / single
    // agent) — the multica `dispatchParentAssigneeTrigger`. An unassigned or
    // member parent has nobody to wake (the store guard already excluded members).
    let Some(assignee) = cascade.parent_assignee.as_ref() else {
        return;
    };
    if assignee.kind() != ActorKind::Agent {
        return;
    }
    let parent = match IssueRepo::get_by_id(pool, &cascade.parent_id).await {
        Ok(Some(p)) if p.workspace_id == ws.as_str() => p,
        _ => return,
    };
    match crate::rpc::run_card(
        pool,
        ws,
        None,
        &parent,
        "headless",
        None,
        None,
        None,
        Some(assignee),
        None,
        // multica parity #12: the child-done cascade is an AUTO_RUN surface too.
        ainb_hangar_core::dispatch_reason::DispatchSource::AutoRun,
    )
    .await
    {
        Ok(_) => {
            tracing::info!(parent = %cascade.parent_id, "child-done cascade: parent agent woken");
        }
        Err(crate::rpc::CardRunError::Blocked(_) | crate::rpc::CardRunError::ActiveRun(_)) => {
            tracing::debug!(parent = %cascade.parent_id, "child-done cascade: parent wake skipped (not launchable)");
        }
        Err(_) => {
            tracing::warn!(parent = %cascade.parent_id, "child-done cascade: parent wake refused");
        }
    }
}
