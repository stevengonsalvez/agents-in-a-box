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
//! `(issue_id, agent_id)` pair, enforced by the partial unique index
//! `idx_one_pending_task_per_issue_agent` (migration 0012, replacing the 0004
//! global-per-issue scope). An [`TaskRepo::insert`] of a second pending task
//! for the same issue **and the same agent** therefore returns a `sqlx::Error`
//! UNIQUE-constraint violation rather than silently double-queueing — this is
//! how the enqueue path coalesces duplicate fires, while *different* agents
//! may each queue work on one issue in parallel (the reference's per-(issue, agent)
//! model, `pkg/db/queries/agent.sql` `ClaimAgentTask`). Tasks with a `NULL`
//! `issue_id` (chat / autopilot placeholders) are excluded from the index and
//! never collide.
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
    /// Claim urgency: 0..3 mapping P3..P0 — HIGHER = MORE URGENT (migration
    /// 0013). `0` (P3) is the routine default; the claim loop drains
    /// `priority DESC, created_at, id`, so equal priorities stay FIFO.
    pub priority: i64,
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
    /// Claim urgency: 0..3 mapping P3..P0, higher = more urgent (see
    /// [`NewTask::priority`]).
    pub priority: i64,
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
    /// The launch mode (`headless` / `interactive`, migration 0031, ccc / D6).
    /// `headless` is the schema default — the unchanged `claude -p` / `codex exec`
    /// provider path; `interactive` dispatches the agent into a REAL attachable
    /// tmux session. Enqueue paths that do not opt in inherit `headless`.
    pub mode: String,
    /// The exact tmux session name an interactive run spawned
    /// (`tmux_hangar-<task_id>`, migration 0031), or `None` for a headless task or
    /// an interactive task not yet dispatched. This is the durable handle the
    /// attach-from-card affordance surfaces (`tmux attach -t <session_name>`).
    pub session_name: Option<String>,
    /// The run's repo (migration 0032): an absolute checkout path, or the literal
    /// `scratch`; `None` for a chat / autopilot task with no repo (the pre-F5
    /// in-tree fallback). Read back onto the struct (tcp 19n) so the dispatch path
    /// provisions the worktree from the claimed [`Task`] rather than re-querying,
    /// and an infra-retried child inherits it (the retry INSERT copies it) instead
    /// of falling back to the in-tree dir.
    pub repo_ref: Option<String>,
    /// The resolved provider the run dispatches through (`claude` / `codex` /
    /// `copilot`; migration 0032, `NOT NULL DEFAULT 'claude'`). Carried on the
    /// struct (tcp 19n) so a retry child copies it verbatim rather than silently
    /// resetting to the `claude` column default.
    pub agent_kind: String,
    /// The worktree branch (`ainb/<slug>`) this run produced commits on, recorded
    /// at finalize ONLY when the run left commits ahead of its base (tcp T2). The
    /// durable artifact that survives worktree teardown (`git worktree remove`
    /// keeps the branch); `None` when the run made no commits (nothing to surface).
    pub branch: Option<String>,
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
    /// constraint violation from `idx_one_pending_task_per_issue_agent` when a
    /// pending task already exists for the same `(issue_id, agent_id)`, or an
    /// FK violation on `workspace_id` / `runtime_id` / `agent_id` / `issue_id`.
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
    /// constraint violation from `idx_one_pending_task_per_issue_agent` when a
    /// pending task already exists for the same `(issue_id, agent_id)`, or an
    /// FK violation on `workspace_id` / `runtime_id` / `agent_id` / `issue_id` /
    /// `autopilot_run_id`.
    pub async fn insert_in_tx(
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        task: &NewTask,
    ) -> Result<String, sqlx::Error> {
        sqlx::query(
            "INSERT INTO agent_task_queue \
             (id, workspace_id, runtime_id, agent_id, issue_id, work_dir, priority, \
              created_at, autopilot_run_id) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&task.id)
        .bind(&task.workspace_id)
        .bind(&task.runtime_id)
        .bind(&task.agent_id)
        .bind(&task.issue_id)
        .bind(&task.work_dir)
        .bind(task.priority)
        .bind(task.created_at)
        .bind(&task.autopilot_run_id)
        .execute(&mut **tx)
        .await?;
        Ok(task.id.clone())
    }

    /// Record the tmux `session_name` an interactive run spawned onto the task
    /// row (migration 0031, ccc / D6). Written the moment the session is created
    /// so the attach-from-card affordance can reach it while the run is live.
    /// Returns `true` iff exactly one row was updated.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] on a store fault.
    pub async fn set_session_name(
        pool: &SqlitePool,
        id: &str,
        session_name: &str,
    ) -> Result<bool, sqlx::Error> {
        let res = sqlx::query("UPDATE agent_task_queue SET session_name = ? WHERE id = ?")
            .bind(session_name)
            .bind(id)
            .execute(pool)
            .await?;
        Ok(res.rows_affected() == 1)
    }

    /// Record the worktree `branch` (`ainb/<slug>`) a run produced commits on
    /// (tcp T2), written at finalize only when the run left commits ahead of its
    /// base. Returns `true` iff exactly one row was updated.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] on a store fault.
    pub async fn set_branch(
        pool: &SqlitePool,
        id: &str,
        branch: &str,
    ) -> Result<bool, sqlx::Error> {
        let res = sqlx::query("UPDATE agent_task_queue SET branch = ? WHERE id = ?")
            .bind(branch)
            .bind(id)
            .execute(pool)
            .await?;
        Ok(res.rows_affected() == 1)
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

    /// List **every** task in `workspace_id`, oldest first. Backs the Kanban
    /// board snapshot (P8.4), which buckets the six lifecycle statuses into its
    /// four columns client-side, so this returns terminal rows too (unlike
    /// [`list_pending_for_runtime`](Self::list_pending_for_runtime)).
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the query or a row decode fails.
    pub async fn list_by_workspace(
        pool: &SqlitePool,
        workspace_id: &str,
    ) -> Result<Vec<Task>, sqlx::Error> {
        let rows = sqlx::query(&format!(
            "SELECT {COLUMNS} FROM agent_task_queue \
             WHERE workspace_id = ? ORDER BY created_at, id"
        ))
        .bind(workspace_id)
        .fetch_all(pool)
        .await?;
        rows.iter().map(task_from_row).collect()
    }

    /// Move one task to `to_status`, **scoped to `workspace_id`**, stamping the
    /// lifecycle timestamps the new status implies. Backs the Kanban card-move
    /// (P8.4): `Shift+←/→` drags a card to a new column.
    ///
    /// The `WHERE id = ? AND workspace_id = ?` clause is the tenant guard — a
    /// task id from another workspace touches no row and returns `Ok(false)`,
    /// never another tenant's task. `Ok(true)` means exactly one row moved.
    ///
    /// Timestamp coherence (so the board's derived age / state stay sane):
    /// - moving to `running` stamps `started_at = clock` if not already set;
    /// - moving to a terminal status (`done`/`failed`/`cancelled`) stamps
    ///   `finished_at = clock`;
    /// - moving back to `queued` clears `dispatched_at`/`started_at`/`finished_at`.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] on a store fault. (Status-token validity is the
    /// caller's concern — the daemon parses it via [`TaskStatus`] before calling
    /// here; the DB `CHECK` constraint is the final guard.)
    ///
    /// [`TaskStatus`]: ainb_hangar_core::task_status::TaskStatus
    pub async fn transition_status(
        pool: &SqlitePool,
        workspace_id: &str,
        id: &str,
        to_status: ainb_hangar_core::task_status::TaskStatus,
        now_ms: i64,
    ) -> Result<bool, sqlx::Error> {
        use ainb_hangar_core::task_status::TaskStatus;
        // One UPDATE expresses every timestamp rule via CASE so the move is a
        // single atomic statement (the board reflects it on the next snapshot).
        let started = if to_status == TaskStatus::Running {
            // Preserve an existing start; only stamp when first entering running.
            "started_at = COALESCE(started_at, ?2)"
        } else if to_status == TaskStatus::Queued {
            "started_at = NULL"
        } else {
            "started_at = started_at"
        };
        let finished = if to_status.is_terminal() {
            "finished_at = ?2"
        } else {
            "finished_at = NULL"
        };
        let dispatched = if to_status == TaskStatus::Queued {
            "dispatched_at = NULL"
        } else {
            "dispatched_at = dispatched_at"
        };
        let sql = format!(
            "UPDATE agent_task_queue \
             SET status = ?1, {started}, {finished}, {dispatched} \
             WHERE id = ?3 AND workspace_id = ?4"
        );
        let res = sqlx::query(&sql)
            .bind(to_status.as_str())
            .bind(now_ms)
            .bind(id)
            .bind(workspace_id)
            .execute(pool)
            .await?;
        Ok(res.rows_affected() == 1)
    }
}

/// The full `agent_task_queue` column list, in the order [`task_from_row`]
/// reads them. Shared by every `SELECT` so the read shape stays in one place.
const COLUMNS: &str = "id, workspace_id, runtime_id, agent_id, issue_id, status, result, \
     session_id, work_dir, attempt, max_attempts, parent_task_id, failure_reason, \
     priority, created_at, dispatched_at, started_at, finished_at, autopilot_run_id, \
     mode, session_name, repo_ref, agent_kind, branch";

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
        priority: row.try_get("priority")?,
        created_at: row.try_get("created_at")?,
        dispatched_at: row.try_get("dispatched_at")?,
        started_at: row.try_get("started_at")?,
        finished_at: row.try_get("finished_at")?,
        autopilot_run_id: row.try_get("autopilot_run_id")?,
        mode: row.try_get("mode")?,
        session_name: row.try_get("session_name")?,
        repo_ref: row.try_get("repo_ref")?,
        agent_kind: row.try_get("agent_kind")?,
        branch: row.try_get("branch")?,
    })
}
