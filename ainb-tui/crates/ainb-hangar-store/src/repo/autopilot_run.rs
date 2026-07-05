//! The autopilot tick → enqueue path (P7.4).
//!
//! [`fire_autopilot_tick`] is the single-transaction fire path the scheduler
//! (P7.3) and the manager screen's "run now" (P7.5) both call. In one
//! transaction it:
//!
//! 1. inserts an `autopilot_run` row (`status = 'running'`, `started_at =
//!    clock.now_ms()`),
//! 2. resolves the autopilot agent's `runtime_id` (an autopilot binds an agent,
//!    and every agent binds a runtime),
//! 3. in `create_issue` execution mode, inserts a fresh `issue` (authored by the
//!    autopilot's agent) so the run has a tracked work item; in the default
//!    `run_only` mode this step is skipped,
//! 4. inserts an `agent_task_queue` row via [`TaskRepo::insert_in_tx`] with
//!    `autopilot_run_id = <the new run>` and `issue_id` set to the new issue
//!    (`create_issue`) or `NULL` (`run_only`),
//!
//! then commits. Because every insert shares the one transaction, a failure of
//! any (most importantly the task insert's FK check on a bad `agent_id`) rolls
//! *all* back — there is never a stranded `autopilot_run` row with no task, nor a
//! task with no run, nor an orphan issue.
//!
//! # The autopilot-task discriminator
//!
//! An autopilot queue row carries `autopilot_run_id IS NOT NULL`. That link
//! column is both the "this is an autopilot task" marker and the back-reference
//! the finalize cascade follows. There is no separate `kind` column. In
//! `run_only` mode it is additionally `issue_id IS NULL`; in `create_issue` mode
//! it carries the id of the freshly created issue.
//!
//! # Run completion cascades through the finalize primitive
//!
//! When the task reaches a terminal state, the shared
//! [`crate::idempotent_finalize`] primitive stamps the parent
//! `autopilot_run.completed_at` and maps the task's terminal state onto the
//! run's status (`done -> completed`, `failed -> failed`, `cancelled ->
//! cancelled`). That fires on the *real* complete / fail / cancel path, not a
//! separate caller-driven update.

use ainb_hangar_core::clock::HangarClock;
use ainb_hangar_core::idgen::{IdGen, SystemIdGen};
use ainb_hangar_core::ids::{AutopilotRunId, IdError, TaskId};
use sqlx::{Row, SqlitePool};

use super::autopilot::{Autopilot, ExecutionMode};
use super::task::{NewTask, TaskRepo};

/// Error surface for [`fire_autopilot_tick`].
#[derive(Debug, thiserror::Error)]
pub enum FireError {
    /// An underlying `sqlx` failure: the run insert, the agent-runtime lookup,
    /// or — most relevantly for rollback — the task insert's FK violation. The
    /// transaction is rolled back before this is returned, so no partial state
    /// persists.
    #[error(transparent)]
    Db(#[from] sqlx::Error),
    /// The autopilot's agent could not be resolved to a runtime (the `agent`
    /// row is absent). Surfaced before the task insert; nothing is persisted.
    #[error("autopilot agent {agent_id} has no agent row (cannot resolve runtime)")]
    AgentNotFound {
        /// The agent id that failed to resolve.
        agent_id: String,
    },
    /// A minted primary key was empty — an invariant violation that should be
    /// unreachable (PKs are 26-char ULIDs). Surfaced rather than `unwrap`'d.
    #[error("corrupt id: empty primary key")]
    EmptyId,
}

impl From<IdError> for FireError {
    fn from(_: IdError) -> Self {
        Self::EmptyId
    }
}

/// Fire one autopilot tick: create the run and enqueue its task atomically.
///
/// Returns the new `(autopilot_run.id, agent_task_queue.id)` on commit.
///
/// # Errors
///
/// - [`FireError::AgentNotFound`] if the autopilot's `agent_id` resolves to no
///   `agent` row (no runtime to dispatch to); nothing is persisted.
/// - [`FireError::Db`] on any SQL failure — notably a task-insert FK violation,
///   after which the run insert is rolled back too.
/// - [`FireError::EmptyId`] on the unreachable empty-PK invariant break.
#[tracing::instrument(
    name = "autopilot.tick",
    skip(pool, clock, autopilot),
    fields(autopilot_id = %autopilot.id, cron_expr = %autopilot.cron_expr)
)]
pub async fn fire_autopilot_tick(
    pool: &SqlitePool,
    clock: &dyn HangarClock,
    autopilot: &Autopilot,
) -> Result<(AutopilotRunId, TaskId), FireError> {
    let now = clock.now_ms();
    let run_id = SystemIdGen.new_ulid();
    let task_id = SystemIdGen.new_ulid();

    let mut tx = pool.begin().await?;

    // 1. The run row, in-flight.
    sqlx::query(
        "INSERT INTO autopilot_run (id, autopilot_id, started_at, status) \
         VALUES (?, ?, ?, 'running')",
    )
    .bind(&run_id)
    .bind(&autopilot.id)
    .bind(now)
    .execute(&mut *tx)
    .await?;

    // 2. Resolve the agent's runtime within the same transaction. An absent
    //    agent row means the autopilot points at a deleted agent; abort (the
    //    run insert rolls back) rather than enqueue an orphan task.
    let runtime_id: Option<String> = sqlx::query("SELECT runtime_id FROM agent WHERE id = ?")
        .bind(&autopilot.agent_id)
        .fetch_optional(&mut *tx)
        .await?
        .map(|row| row.get::<String, _>("runtime_id"));
    let Some(runtime_id) = runtime_id else {
        // Dropping `tx` rolls back the run insert.
        return Err(FireError::AgentNotFound {
            agent_id: autopilot.agent_id.clone(),
        });
    };

    // 3. In `create_issue` mode, materialise a tracked issue (authored by the
    //    autopilot's agent) inside the same transaction so the run has a work
    //    item; the task then links to it. In `run_only` mode (the default) the
    //    task is issue-less (`issue_id = NULL`) — the v1 background-run path. Any
    //    failure here rolls back the run insert above.
    let issue_id = match autopilot.execution_mode {
        ExecutionMode::RunOnly => None,
        ExecutionMode::CreateIssue => {
            let issue_id = SystemIdGen.new_ulid();
            let title = autopilot
                .instructions
                .clone()
                .unwrap_or_else(|| format!("Autopilot run: {}", autopilot.name));
            sqlx::query(
                "INSERT INTO issue \
                 (id, workspace_id, title, description, state, \
                  creator_type, creator_id, created_at) \
                 VALUES (?, ?, ?, ?, 'open', 'agent', ?, ?)",
            )
            .bind(&issue_id)
            .bind(&autopilot.workspace_id)
            .bind(&title)
            .bind(&autopilot.instructions)
            .bind(&autopilot.agent_id)
            .bind(now)
            .execute(&mut *tx)
            .await?;
            Some(issue_id)
        }
    };

    // 4. The task row: linked to the run, and (in create_issue mode) to the new
    //    issue. A bad agent_id FK here fails the whole transaction (rolling back
    //    the run + issue inserts above).
    TaskRepo::insert_in_tx(
        &mut tx,
        &NewTask {
            id: task_id.clone(),
            workspace_id: autopilot.workspace_id.clone(),
            runtime_id,
            agent_id: autopilot.agent_id.clone(),
            issue_id,
            work_dir: None,
            // Autopilot ticks are routine background work: default urgency
            // (priority 0 = P3), drained FIFO among equals at claim time.
            priority: 0,
            created_at: now,
            autopilot_run_id: Some(run_id.clone()),
            generation: 0,
        },
    )
    .await?;

    tx.commit().await?;

    Ok((
        AutopilotRunId::from_str(run_id)?,
        TaskId::from_str(task_id)?,
    ))
}

/// Supersede every in-flight run of an autopilot — the `replace` concurrency
/// policy's "cancel the stale work before firing fresh" step (e38.19).
///
/// In one transaction it:
///
/// 1. cancels each in-flight task (`autopilot_run_id IN (open runs) AND status
///    NOT terminal`): `status = 'cancelled'`, `finished_at = now`,
/// 2. cancels each open run (`completed_at IS NULL`): `status = 'cancelled'`,
///    `completed_at = now`.
///
/// The order matters only for clarity (both are correlated by `autopilot_id`).
/// Returns the number of runs superseded. A no-op (returns `0`) when nothing is
/// in flight.
///
/// This is NOT routed through the FSM finalize cascade: the cascade stamps a
/// run from its task's terminal state, but here the SCHEDULER is the authority
/// declaring the run abandoned, so it cancels the run directly. The result is
/// the same terminal shape (run `cancelled` + `completed_at` set, task
/// `cancelled` + `finished_at` set) the finalize path would have produced.
///
/// # Errors
///
/// Returns [`FireError::Db`] on any SQL failure; the transaction rolls back so a
/// partial supersede never persists.
pub async fn supersede_in_flight(
    pool: &SqlitePool,
    clock: &dyn HangarClock,
    autopilot_id: &str,
) -> Result<u64, FireError> {
    let now = clock.now_ms();
    let mut tx = pool.begin().await?;

    // 1. Cancel the tasks of the open runs (abandon the in-flight work). Scoped
    //    to the autopilot's own open runs, so a foreign autopilot's tasks are
    //    untouched.
    sqlx::query(
        "UPDATE agent_task_queue \
         SET status = 'cancelled', finished_at = ? \
         WHERE autopilot_run_id IN ( \
                 SELECT id FROM autopilot_run \
                 WHERE autopilot_id = ? AND completed_at IS NULL \
             ) \
           AND status NOT IN ('done', 'failed', 'cancelled')",
    )
    .bind(now)
    .bind(autopilot_id)
    .execute(&mut *tx)
    .await?;

    // 2. Cancel the open runs themselves.
    let runs = sqlx::query(
        "UPDATE autopilot_run \
         SET status = 'cancelled', completed_at = ? \
         WHERE autopilot_id = ? AND completed_at IS NULL",
    )
    .bind(now)
    .bind(autopilot_id)
    .execute(&mut *tx)
    .await?
    .rows_affected();

    tx.commit().await?;
    Ok(runs)
}
