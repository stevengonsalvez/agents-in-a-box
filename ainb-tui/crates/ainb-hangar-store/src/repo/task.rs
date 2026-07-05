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

    /// Fetch the issue's single ACTIVE (`queued` / `dispatched` / `running`)
    /// task, **scoped to `workspace_id`**, newest first — or `None` when the
    /// issue has no active task (its latest run is terminal, or it never ran).
    ///
    /// Backs the card-cancel path (tcp T3 / F6): a card carries only its issue
    /// id, so the daemon resolves the one live task to cancel here. The
    /// per-(issue, agent) pending-unique index (migration 0012) caps pending
    /// tasks, but a card can carry a `running` task alongside a distinct agent's
    /// pending one; `ORDER BY created_at DESC, id DESC LIMIT 1` picks the most
    /// recent active task deterministically. The `WHERE ... workspace_id = ?`
    /// clause is the tenant guard — a foreign issue id matches no row.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the query or row decode fails.
    pub async fn active_task_for_issue(
        pool: &SqlitePool,
        workspace_id: &str,
        issue_id: &str,
    ) -> Result<Option<Task>, sqlx::Error> {
        let row = sqlx::query(&format!(
            "SELECT {COLUMNS} FROM agent_task_queue \
             WHERE workspace_id = ? AND issue_id = ? \
               AND status IN ('queued','dispatched','running') \
             ORDER BY created_at DESC, id DESC LIMIT 1"
        ))
        .bind(workspace_id)
        .bind(issue_id)
        .fetch_optional(pool)
        .await?;
        row.map(|r| task_from_row(&r)).transpose()
    }

    /// Fetch the issue's ENTIRE active set — every `queued` / `dispatched` /
    /// `running` task on `issue_id`, **scoped to `workspace_id`**, newest first.
    ///
    /// A squad card fans out N tasks onto ONE issue (leader + one per member), so
    /// an issue can carry several active tasks at once. This is the set the
    /// card-cancel path (tcp T4 / FANOUT-SEMANTICS) must cancel WHOLESALE:
    /// [`active_task_for_issue`](Self::active_task_for_issue) (LIMIT 1) resolves only
    /// the newest sibling, so cancelling on it alone leaves the leader + the other
    /// members burning. The `WHERE ... workspace_id = ?` clause is the tenant guard.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the query or a row decode fails.
    pub async fn active_tasks_for_issue(
        pool: &SqlitePool,
        workspace_id: &str,
        issue_id: &str,
    ) -> Result<Vec<Task>, sqlx::Error> {
        let rows = sqlx::query(&format!(
            "SELECT {COLUMNS} FROM agent_task_queue \
             WHERE workspace_id = ? AND issue_id = ? \
               AND status IN ('queued','dispatched','running') \
             ORDER BY created_at DESC, id DESC"
        ))
        .bind(workspace_id)
        .bind(issue_id)
        .fetch_all(pool)
        .await?;
        rows.iter().map(task_from_row).collect()
    }

    /// The AGGREGATE terminal outcome of an issue's tasks — the single card-state
    /// token to auto-move a card by, but ONLY once the issue's active set has
    /// DRAINED (tcp T4 / FANOUT-SEMANTICS).
    ///
    /// Returns:
    /// - `None` when the issue still has ANY active (`queued`/`dispatched`/
    ///   `running`) task — the squad has NOT finished, so the card must not
    ///   terminal-auto-move yet (this is the drain gate);
    /// - `None` when the issue has no tasks at all (nothing to move on);
    /// - otherwise the aggregate of the terminal statuses, by precedence
    ///   **`failed` > `cancelled` > `done`**: any failed sibling fails the card;
    ///   else any cancelled sibling cancels it; else every sibling is `done`, so
    ///   the card succeeded. Precedence is deliberate — a squad where one member
    ///   failed is a card-level failure even if the rest succeeded.
    ///
    /// # Single run generation (known limitation)
    ///
    /// The fold spans EVERY task the issue has ever had, so it assumes those tasks are
    /// ONE run generation — true for a single fan-out. A card RE-RUN after a prior
    /// `failed`/`cancelled` run leaves those older terminal rows in the table, and
    /// precedence would then still report `failed`/`cancelled` even after a clean
    /// rerun. Scoping to the latest run needs a run-generation marker (follow-up); the
    /// concurrent-fan-out contract this backs is unaffected.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the query fails.
    pub async fn issue_aggregate_terminal_state(
        pool: &SqlitePool,
        workspace_id: &str,
        issue_id: &str,
    ) -> Result<Option<String>, sqlx::Error> {
        // One pass over the issue's tasks: NULL while any task is still active (the
        // drain gate), else the highest-precedence terminal token present. `SUM(bool)`
        // over zero rows is NULL, so a task-less issue also yields NULL.
        sqlx::query_scalar::<_, Option<String>>(
            "SELECT CASE \
               WHEN SUM(status IN ('queued','dispatched','running')) > 0 THEN NULL \
               WHEN SUM(status = 'failed') > 0 THEN 'failed' \
               WHEN SUM(status = 'cancelled') > 0 THEN 'cancelled' \
               WHEN SUM(status = 'done') > 0 THEN 'done' \
               ELSE NULL END \
             FROM agent_task_queue WHERE workspace_id = ? AND issue_id = ?",
        )
        .bind(workspace_id)
        .bind(issue_id)
        .fetch_one(pool)
        .await
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Store;

    const WS: &str = "ws-a";

    async fn open() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        (dir, store)
    }

    /// Seed the minimal FK chain (workspace / runtime / agent / issue) once so task
    /// rows insert cleanly.
    async fn seed_chain(pool: &SqlitePool, issue_id: &str) {
        sqlx::query("INSERT OR IGNORE INTO workspace (id, slug, name, created_at) VALUES (?, ?, ?, 0)")
            .bind(WS).bind(WS).bind(WS).execute(pool).await.unwrap();
        sqlx::query("INSERT OR IGNORE INTO user (id, email, created_at) VALUES ('u','u@e.com',0)")
            .execute(pool).await.unwrap();
        sqlx::query("INSERT OR IGNORE INTO agent_runtime (id, workspace_id, daemon_id, provider, runtime_mode, status) VALUES ('rt', ?, 'd','claude','local','online')")
            .bind(WS).execute(pool).await.unwrap();
        sqlx::query("INSERT OR IGNORE INTO agent (id, workspace_id, name, runtime_id, instructions, visibility, owner_id) VALUES ('ag', ?, 'A','rt','x','workspace','u')")
            .bind(WS).execute(pool).await.unwrap();
        sqlx::query(
            "INSERT OR IGNORE INTO issue (id, workspace_id, title, creator_type, creator_id, created_at) \
             VALUES (?, ?, ?, 'member', 'u', 0)",
        )
        .bind(issue_id).bind(WS).bind(issue_id).execute(pool).await.unwrap();
    }

    /// Insert one task row on `issue_id` with an explicit id / status / created_at,
    /// bypassing the pending-unique index (it only guards `queued`/`dispatched`), so
    /// a test can build a squad-shaped multi-task issue directly.
    async fn seed_task(pool: &SqlitePool, issue_id: &str, id: &str, status: &str, created_at: i64) {
        sqlx::query(
            "INSERT INTO agent_task_queue (id, workspace_id, runtime_id, agent_id, issue_id, status, created_at) \
             VALUES (?, ?, 'rt', 'ag', ?, ?, ?)",
        )
        .bind(id).bind(WS).bind(issue_id).bind(status).bind(created_at)
        .execute(pool).await.unwrap();
    }

    /// The active set is EVERY non-terminal task on the issue (a squad fan-out),
    /// newest first — not just the latest sibling.
    #[tokio::test]
    async fn active_set_returns_all_non_terminal_siblings() {
        let (_d, store) = open().await;
        let pool = store.pool();
        seed_chain(pool, "iss").await;
        // A squad-shaped issue: leader running + two members running + one already done.
        seed_task(pool, "iss", "leader", "running", 10).await;
        seed_task(pool, "iss", "m1", "running", 11).await;
        seed_task(pool, "iss", "m2", "dispatched", 12).await;
        seed_task(pool, "iss", "old", "done", 9).await;

        let active = TaskRepo::active_tasks_for_issue(pool, WS, "iss").await.unwrap();
        let ids: Vec<&str> = active.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, vec!["m2", "m1", "leader"], "the whole active set, newest first; the done task excluded");
    }

    /// A foreign-workspace issue id matches no active task (tenant guard).
    #[tokio::test]
    async fn active_set_is_workspace_scoped() {
        let (_d, store) = open().await;
        let pool = store.pool();
        seed_chain(pool, "iss").await;
        seed_task(pool, "iss", "t", "running", 1).await;
        assert!(TaskRepo::active_tasks_for_issue(pool, "other-ws", "iss").await.unwrap().is_empty());
    }

    /// The aggregate is `None` while ANY sibling is still active (the drain gate),
    /// then resolves to the highest-precedence terminal token once the set drains.
    #[tokio::test]
    async fn aggregate_gates_on_drain_then_folds_by_precedence() {
        let (_d, store) = open().await;
        let pool = store.pool();

        // No tasks at all → nothing to move on.
        seed_chain(pool, "empty").await;
        assert_eq!(TaskRepo::issue_aggregate_terminal_state(pool, WS, "empty").await.unwrap(), None);

        // A squad mid-run: two done, one still running → NOT drained → None. This is
        // the exact fan-out bug: the latest-done sibling must NOT move the card.
        seed_chain(pool, "mid").await;
        seed_task(pool, "mid", "a", "done", 1).await;
        seed_task(pool, "mid", "b", "done", 2).await;
        seed_task(pool, "mid", "c", "running", 3).await;
        assert_eq!(
            TaskRepo::issue_aggregate_terminal_state(pool, WS, "mid").await.unwrap(),
            None,
            "an issue with a live sibling has not drained — no terminal auto-move"
        );

        // The last sibling finishes done → all done → aggregate `done`.
        TaskRepo::transition_status(pool, WS, "c", ainb_hangar_core::task_status::TaskStatus::Done, 4)
            .await.unwrap();
        assert_eq!(
            TaskRepo::issue_aggregate_terminal_state(pool, WS, "mid").await.unwrap().as_deref(),
            Some("done"),
            "every sibling done → the card succeeded"
        );
    }

    /// Precedence: any `failed` beats a `cancelled`/`done` sibling; a `cancelled`
    /// beats a `done`; a fully-cancelled/failed set never yields `done`.
    #[tokio::test]
    async fn aggregate_precedence_failed_over_cancelled_over_done() {
        let (_d, store) = open().await;
        let pool = store.pool();

        seed_chain(pool, "mixed").await;
        seed_task(pool, "mixed", "mx-a", "done", 1).await;
        seed_task(pool, "mixed", "mx-b", "cancelled", 2).await;
        seed_task(pool, "mixed", "mx-c", "failed", 3).await;
        assert_eq!(
            TaskRepo::issue_aggregate_terminal_state(pool, WS, "mixed").await.unwrap().as_deref(),
            Some("failed"),
            "any failed sibling fails the card"
        );

        // done + cancelled (no failure) → cancelled wins over done.
        seed_chain(pool, "userstop").await;
        seed_task(pool, "userstop", "us-a", "done", 1).await;
        seed_task(pool, "userstop", "us-b", "cancelled", 2).await;
        assert_eq!(
            TaskRepo::issue_aggregate_terminal_state(pool, WS, "userstop").await.unwrap().as_deref(),
            Some("cancelled"),
            "a user cancel of a partly-done card cancels it"
        );

        // Fully cancelled → cancelled, never done.
        seed_chain(pool, "allcancel").await;
        seed_task(pool, "allcancel", "ac-a", "cancelled", 1).await;
        seed_task(pool, "allcancel", "ac-b", "cancelled", 2).await;
        assert_eq!(
            TaskRepo::issue_aggregate_terminal_state(pool, WS, "allcancel").await.unwrap().as_deref(),
            Some("cancelled")
        );
    }
}
