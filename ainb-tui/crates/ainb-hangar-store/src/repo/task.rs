//! Typed repository wrapper over the `agent_task_queue` table.
//!
//! [`TaskRepo`] is a thin, stateless sqlx layer covering the *read + enqueue*
//! surface P0 needs: [`TaskRepo::insert`] writes the initial `queued` row,
//! [`TaskRepo::get_by_id`] reads one back, and [`TaskRepo::list_pending_for_runtime`]
//! lists the non-terminal queue for a runtime.
//!
//! # Partial-unique invariant
//!
//! At most one *pending* (`queued` or `dispatched`) task may exist per
//! `issue_id`, enforced by the partial unique index
//! `idx_one_pending_task_per_issue` (migration 0004). An [`TaskRepo::insert`] of
//! a second pending task for the same issue therefore returns a `sqlx::Error`
//! UNIQUE-constraint violation rather than silently double-queueing — this is
//! how the enqueue path coalesces duplicate fires (mirrors Multica
//! `022_task_lifecycle_guards.up.sql`). Tasks with a `NULL` `issue_id` (chat /
//! autopilot placeholders) are excluded from the index and never collide.
//!
//! # Scope
//!
//! There are deliberately **no state-transition methods** here (`claim`,
//! `start`, `complete`, `fail`, `cancel`, `reclaim_stale_dispatched`,
//! `sweep_expired_queued`). Those belong to the P1 daemon FSM, which builds on
//! these read/enqueue primitives.

use sqlx::{Row, SqlitePool};

/// Parameters for enqueueing a new task (always inserted as `queued`).
///
/// The lifecycle/retry columns (`status`, `attempt`, `max_attempts`,
/// `result`, `session_id`, `failure_reason`, `started_at`, `finished_at`,
/// `parent_task_id`) are left to their schema defaults at enqueue time and are
/// mutated by the P1 FSM, so they are not part of this struct.
#[derive(Debug, Clone)]
pub struct NewTask {
    /// Primary key (ULID string).
    pub id: String,
    /// Owning workspace (`workspace.id`).
    pub workspace_id: String,
    /// Target runtime (`agent_runtime.id`) the task will dispatch to.
    pub runtime_id: String,
    /// Agent (`agent.id`) that will execute the task.
    pub agent_id: String,
    /// Originating issue (`issue.id`), or `None` for chat / autopilot tasks that
    /// carry no issue at v1.
    pub issue_id: Option<String>,
    /// Working directory the run executes in, or `None` if unset at enqueue.
    pub work_dir: Option<String>,
    /// Creation timestamp (epoch milliseconds).
    pub created_at: i64,
    /// The autopilot firing this task belongs to (`autopilot_run.id`), or `None`
    /// for ordinary issue / chat tasks. A non-`None` value (paired with a `None`
    /// `issue_id`) is the discriminator that marks the row as an autopilot task;
    /// the finalize cascade follows it to stamp the run's `completed_at`.
    pub autopilot_run_id: Option<String>,
}

/// A fully-materialised `agent_task_queue` row read back from the database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Task {
    /// Primary key.
    pub id: String,
    /// Owning workspace.
    pub workspace_id: String,
    /// Target runtime.
    pub runtime_id: String,
    /// Executing agent.
    pub agent_id: String,
    /// Originating issue, or `None`.
    pub issue_id: Option<String>,
    /// Lifecycle status (one of the schema `CHECK` set).
    pub status: String,
    /// Structured result JSON blob, or `None` until the task completes.
    pub result: Option<String>,
    /// Provider session id the run used, or `None`.
    pub session_id: Option<String>,
    /// Working directory the run executed in, or `None`.
    pub work_dir: Option<String>,
    /// Current attempt number (1-based).
    pub attempt: i64,
    /// Maximum attempts before the task is abandoned.
    pub max_attempts: i64,
    /// The task this one is a retry of, or `None` for a first attempt.
    pub parent_task_id: Option<String>,
    /// Last failure reason, or `None` if not failed.
    pub failure_reason: Option<String>,
    /// Creation timestamp (epoch milliseconds) — the queued-at time.
    pub created_at: i64,
    /// When the task was claimed (`queued -> dispatched`), epoch milliseconds,
    /// or `None` while still `queued`. Distinct from [`Task::created_at`]
    /// (queued-at) and [`Task::started_at`] (run-start); the P1.4 sweepers key
    /// the reclaim window and dispatch TTL off it.
    pub dispatched_at: Option<i64>,
    /// When the run started (epoch milliseconds), or `None` if not started.
    pub started_at: Option<i64>,
    /// When the run finished (epoch milliseconds), or `None` if not finished.
    pub finished_at: Option<i64>,
    /// The autopilot run this task belongs to (`autopilot_run.id`), or `None`
    /// for ordinary issue / chat tasks. See [`NewTask::autopilot_run_id`].
    pub autopilot_run_id: Option<String>,
}

/// Stateless typed wrapper over the `agent_task_queue` table.
pub struct TaskRepo;

impl TaskRepo {
    /// Enqueue one task as `queued`, leaving every lifecycle/retry column at its
    /// schema default. Returns the new row's `id` on success.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the insert fails — notably a UNIQUE
    /// constraint violation from `idx_one_pending_task_per_issue` when a pending
    /// task already exists for the same `issue_id`, or an FK violation on
    /// `workspace_id` / `runtime_id` / `agent_id` / `issue_id`.
    pub async fn insert(pool: &SqlitePool, task: &NewTask) -> Result<String, sqlx::Error> {
        let mut tx = pool.begin().await?;
        let id = Self::insert_in_tx(&mut tx, task).await?;
        tx.commit().await?;
        Ok(id)
    }

    /// Enqueue one task as `queued` **within an existing transaction**, leaving
    /// every lifecycle/retry column at its schema default. Returns the new row's
    /// `id` on success; the caller owns the commit/rollback.
    ///
    /// This is the variant the P7.4 autopilot fire path uses: it inserts the
    /// `autopilot_run` row and the task row in one transaction, so a task-insert
    /// failure (e.g. a bad `agent_id` FK) rolls the run back too. [`insert`]
    /// wraps this in its own transaction for the standalone enqueue case.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the insert fails — notably a UNIQUE
    /// constraint violation from `idx_one_pending_task_per_issue` when a pending
    /// task already exists for the same `issue_id`, or an FK violation on
    /// `workspace_id` / `runtime_id` / `agent_id` / `issue_id` /
    /// `autopilot_run_id`.
    pub async fn insert_in_tx(
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        task: &NewTask,
    ) -> Result<String, sqlx::Error> {
        sqlx::query(
            "INSERT INTO agent_task_queue \
             (id, workspace_id, runtime_id, agent_id, issue_id, work_dir, created_at, \
              autopilot_run_id) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&task.id)
        .bind(&task.workspace_id)
        .bind(&task.runtime_id)
        .bind(&task.agent_id)
        .bind(&task.issue_id)
        .bind(&task.work_dir)
        .bind(task.created_at)
        .bind(&task.autopilot_run_id)
        .execute(&mut **tx)
        .await?;
        Ok(task.id.clone())
    }

    /// Fetch one task by primary key, or `None` if absent.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the query or row decode fails.
    pub async fn get_by_id(pool: &SqlitePool, id: &str) -> Result<Option<Task>, sqlx::Error> {
        let row = sqlx::query(&format!(
            "SELECT {COLUMNS} FROM agent_task_queue WHERE id = ?"
        ))
        .bind(id)
        .fetch_optional(pool)
        .await?;
        row.map(|r| task_from_row(&r)).transpose()
    }

    /// List the *pending* (`queued` or `dispatched`) tasks for a runtime,
    /// oldest first. This is the queue the P1 FSM drains.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the query or a row decode fails.
    pub async fn list_pending_for_runtime(
        pool: &SqlitePool,
        runtime_id: &str,
    ) -> Result<Vec<Task>, sqlx::Error> {
        let rows = sqlx::query(&format!(
            "SELECT {COLUMNS} FROM agent_task_queue \
             WHERE runtime_id = ? AND status IN ('queued','dispatched') ORDER BY created_at"
        ))
        .bind(runtime_id)
        .fetch_all(pool)
        .await?;
        rows.iter().map(task_from_row).collect()
    }
}

/// The full `agent_task_queue` column list, in the order [`task_from_row`]
/// reads them. Shared by every `SELECT` so the read shape stays in one place.
const COLUMNS: &str = "id, workspace_id, runtime_id, agent_id, issue_id, status, result, \
     session_id, work_dir, attempt, max_attempts, parent_task_id, failure_reason, \
     created_at, dispatched_at, started_at, finished_at, autopilot_run_id";

/// Map one raw `agent_task_queue` row into a [`Task`].
fn task_from_row(row: &sqlx::sqlite::SqliteRow) -> Result<Task, sqlx::Error> {
    Ok(Task {
        id: row.try_get("id")?,
        workspace_id: row.try_get("workspace_id")?,
        runtime_id: row.try_get("runtime_id")?,
        agent_id: row.try_get("agent_id")?,
        issue_id: row.try_get("issue_id")?,
        status: row.try_get("status")?,
        result: row.try_get("result")?,
        session_id: row.try_get("session_id")?,
        work_dir: row.try_get("work_dir")?,
        attempt: row.try_get("attempt")?,
        max_attempts: row.try_get("max_attempts")?,
        parent_task_id: row.try_get("parent_task_id")?,
        failure_reason: row.try_get("failure_reason")?,
        created_at: row.try_get("created_at")?,
        dispatched_at: row.try_get("dispatched_at")?,
        started_at: row.try_get("started_at")?,
        finished_at: row.try_get("finished_at")?,
        autopilot_run_id: row.try_get("autopilot_run_id")?,
    })
}
