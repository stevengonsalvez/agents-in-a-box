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
//! 3. inserts an `agent_task_queue` row via [`TaskRepo::insert_in_tx`] with
//!    `issue_id = NULL` and `autopilot_run_id = <the new run>`,
//!
//! then commits. Because both inserts share the one transaction, a failure of
//! either (most importantly the task insert's FK check on a bad `agent_id`)
//! rolls *both* back — there is never a stranded `autopilot_run` row with no
//! task, nor a task with no run.
//!
//! # The autopilot-task discriminator
//!
//! An autopilot queue row carries `autopilot_run_id IS NOT NULL` (paired with
//! `issue_id IS NULL`). That link column is both the "this is an autopilot task"
//! marker and the back-reference the finalize cascade follows. There is no
//! separate `kind` column.
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

use super::autopilot::Autopilot;
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

    // 3. The task row: no issue, linked to the run. A bad agent_id FK here fails
    //    the whole transaction (rolling back the run insert above).
    TaskRepo::insert_in_tx(
        &mut tx,
        &NewTask {
            id: task_id.clone(),
            workspace_id: autopilot.workspace_id.clone(),
            runtime_id,
            agent_id: autopilot.agent_id.clone(),
            issue_id: None,
            work_dir: None,
            created_at: now,
            autopilot_run_id: Some(run_id.clone()),
        },
    )
    .await?;

    tx.commit().await?;

    Ok((
        AutopilotRunId::from_str(run_id)?,
        TaskId::from_str(task_id)?,
    ))
}
