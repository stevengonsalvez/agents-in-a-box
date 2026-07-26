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

use ainb_hangar_core::origin::IssueOrigin;
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
    /// The run generation this task belongs to (migration 0039, tcp 8ln). Every
    /// fresh Run / rerun / fan-out of an issue enqueues at the issue's NEXT
    /// generation ([`TaskRepo::next_generation_for_issue`]); the leader + all
    /// members of one fan-out SHARE one value (they are one run), and an infra
    /// retry child copies its parent's. `0` for the first run and for issueless
    /// chat / autopilot tasks. The card-state folds (aggregate / blocker-finished
    /// / auto-move / chip) scope to an issue's LATEST generation, so prior-run
    /// terminal rows never poison the current run.
    pub generation: i64,
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
    /// The run generation this row belongs to (migration 0039, tcp 8ln). See
    /// [`NewTask::generation`]. Carried on the struct so the infra-retry path
    /// copies it verbatim (a retry is a new attempt of the SAME run generation).
    pub generation: i64,
    /// The SOURCE branch this run branches FROM (migration 0042); `None`
    /// resolves to `main` at dispatch. Distinct from [`Self::branch`], the
    /// PRODUCED `ainb/<slug>` output recorded post-run. Written at enqueue via
    /// [`super::card_parity::CardParityRepo::set_task_source_branch_in_tx`],
    /// read back here so dispatch provisions from the claimed row.
    pub source_branch: Option<String>,
    /// The squad that dispatched this task (migration 0045), or `None` for a
    /// single-agent task. Stamped post-insert by
    /// [`SquadAssignService`](crate::service::squad_assign::SquadAssignService);
    /// read back so the daemon claim path can key a leader-briefing injection off
    /// it. An infra-retry child copies it verbatim.
    pub squad_id: Option<String>,
    /// Who/what enqueued this task (migration 0056, multica parity #21). The
    /// daemon hands it to the agent child as `HANGAR_ORIGIN_TYPE` /
    /// `HANGAR_ORIGIN_ID` at dispatch, so an issue the agent creates mid-run
    /// carries the same provenance. `None` for every pre-0056 row.
    pub origin: Option<IssueOrigin>,
}

/// Stateless typed wrapper over the `agent_task_queue` table.
pub struct TaskRepo;

impl TaskRepo {
    /// Stamp a task's ORIGIN PROVENANCE (migration 0056) WITHIN an enqueue
    /// transaction — same atomicity contract as
    /// [`CardParityRepo::set_task_source_branch_in_tx`]: the claim loop can
    /// never observe a task missing its dispatch inputs, so a task can never
    /// exist without the provenance the dispatcher hands its child.
    ///
    /// [`CardParityRepo::set_task_source_branch_in_tx`]: crate::repo::card_parity::CardParityRepo::set_task_source_branch_in_tx
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] on a store fault.
    pub async fn set_origin_in_tx(
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        task_id: &str,
        origin: &IssueOrigin,
    ) -> Result<(), sqlx::Error> {
        sqlx::query("UPDATE agent_task_queue SET origin_type = ?, origin_id = ? WHERE id = ?")
            .bind(origin.kind_db_str())
            .bind(origin.id())
            .bind(task_id)
            .execute(&mut **tx)
            .await?;
        Ok(())
    }

    /// Post-insert [`Self::set_origin_in_tx`] for callers outside a transaction.
    ///
    /// Returns `true` when exactly one row was stamped.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] on a store fault.
    pub async fn set_origin(
        pool: &SqlitePool,
        task_id: &str,
        origin: &IssueOrigin,
    ) -> Result<bool, sqlx::Error> {
        let res =
            sqlx::query("UPDATE agent_task_queue SET origin_type = ?, origin_id = ? WHERE id = ?")
                .bind(origin.kind_db_str())
                .bind(origin.id())
                .bind(task_id)
                .execute(pool)
                .await?;
        Ok(res.rows_affected() == 1)
    }

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
              created_at, autopilot_run_id, generation) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
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
        .bind(task.generation)
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

    /// Stamp the dispatching `squad_id` onto a task row (migration 0045), the
    /// non-transactional sibling of [`set_squad_id_in_tx`](Self::set_squad_id_in_tx).
    /// Used by the CLI leader-only assign path, which inserts the leader task in its
    /// own transaction and then stamps the squad ref on the committed row. Returns
    /// `true` iff exactly one row was updated.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] on a store fault.
    pub async fn set_squad_id(
        pool: &SqlitePool,
        id: &str,
        squad_id: &str,
    ) -> Result<bool, sqlx::Error> {
        let res = sqlx::query("UPDATE agent_task_queue SET squad_id = ? WHERE id = ?")
            .bind(squad_id)
            .bind(id)
            .execute(pool)
            .await?;
        Ok(res.rows_affected() == 1)
    }

    /// Stamp the dispatching squad onto a task row WITHIN a fan-out transaction
    /// (migration 0045), so the stamp commits atomically with the fanned-out task
    /// inserts. Returns `true` iff exactly one row was updated.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] on a store fault.
    pub async fn set_squad_id_in_tx(
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        id: &str,
        squad_id: &str,
    ) -> Result<bool, sqlx::Error> {
        let res = sqlx::query("UPDATE agent_task_queue SET squad_id = ? WHERE id = ?")
            .bind(squad_id)
            .bind(id)
            .execute(&mut **tx)
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
    /// # Latest run generation only (migration 0039, tcp 8ln)
    ///
    /// The fold is scoped to the issue's LATEST generation
    /// ([`next_generation_for_issue`](Self::next_generation_for_issue) mints a fresh
    /// one per Run / rerun / fan-out). A card RE-RUN after a prior
    /// `failed`/`cancelled` run leaves those older terminal rows in the table at a
    /// LOWER generation, so they are excluded here and a clean rerun reports `done`
    /// (the old-terminal-rows-poison-rerun bug). The concurrent-fan-out contract is
    /// unaffected: a fan-out's leader + members all share one generation, so the
    /// whole set is still folded together.
    ///
    /// # Retry-superseded attempts are excluded (codex F1)
    ///
    /// An infra retry spawns a child row IN the same generation whose
    /// `parent_task_id` chains to the failed attempt. The failed parent is a
    /// SUPERSEDED attempt — the child carries the attempt's real outcome — so any
    /// row another row names as its parent is excluded from the fold. Without this,
    /// a retryable failure would poison the generation `failed` forever, even after
    /// its retry child completed `done`. A capped chain's LAST failure has no child
    /// and still counts.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the query fails.
    pub async fn issue_aggregate_terminal_state(
        pool: &SqlitePool,
        workspace_id: &str,
        issue_id: &str,
    ) -> Result<Option<String>, sqlx::Error> {
        // One pass over the issue's LATEST-generation, non-superseded tasks: NULL
        // while any is still active (the drain gate), else the highest-precedence
        // terminal token present. `SUM(bool)` over zero rows is NULL, so a task-less
        // issue (the MAX subquery is NULL, matching no row) also yields NULL.
        sqlx::query_scalar::<_, Option<String>>(
            "SELECT CASE \
               WHEN SUM(status IN ('queued','dispatched','running')) > 0 THEN NULL \
               WHEN SUM(status = 'failed') > 0 THEN 'failed' \
               WHEN SUM(status = 'cancelled') > 0 THEN 'cancelled' \
               WHEN SUM(status = 'done') > 0 THEN 'done' \
               ELSE NULL END \
             FROM agent_task_queue \
             WHERE workspace_id = ?1 AND issue_id = ?2 \
               AND generation = (SELECT MAX(generation) FROM agent_task_queue \
                                 WHERE issue_id = ?2) \
               AND id NOT IN (SELECT parent_task_id FROM agent_task_queue \
                              WHERE issue_id = ?2 AND parent_task_id IS NOT NULL)",
        )
        .bind(workspace_id)
        .bind(issue_id)
        .fetch_one(pool)
        .await
    }

    /// The NEXT run generation for `issue_id` (migration 0039, tcp 8ln): one past
    /// the highest generation any task on the issue currently carries, or `0` when
    /// the issue has never run. Called ONCE per Run / rerun / fan-out so every task
    /// of that run shares the value — the fan-out leader + members are stamped with
    /// it, and the card-state folds scope to it.
    ///
    /// The caller holds the per-card launch slot + the one-active-run guard
    /// ([`run_card`](../../../ainb_hangar_daemon/rpc/fn.run_card.html)), so no two
    /// runs of one card compute the same generation concurrently.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the query fails.
    pub async fn next_generation_for_issue(
        pool: &SqlitePool,
        issue_id: &str,
    ) -> Result<i64, sqlx::Error> {
        let max: Option<i64> =
            sqlx::query_scalar("SELECT MAX(generation) FROM agent_task_queue WHERE issue_id = ?")
                .bind(issue_id)
                .fetch_one(pool)
                .await?;
        Ok(max.map_or(0, |m| m + 1))
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

    /// Live (non-terminal) task counts per agent for a whole workspace, in one
    /// O(N) pass — the batch backing for `agents_list`'s workload dimension
    /// (multica `buildPresenceMap`). Returns `agent_id -> (running, queued)`
    /// where `queued` folds Hangar's `queued` + `dispatched` (claimed-but-not-yet-
    /// running is still "waiting to work"). Terminal rows (`done`/`failed`/
    /// `cancelled`) are excluded, so history never reaches the list-level dot.
    /// An agent with zero live tasks is absent from the map (the caller defaults
    /// it to `Idle`).
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the query fails.
    pub async fn live_workload_by_workspace(
        pool: &SqlitePool,
        workspace_id: &str,
    ) -> Result<std::collections::HashMap<String, (i64, i64)>, sqlx::Error> {
        // SQLite booleans are 0/1, so `SUM(status = 'running')` counts the running
        // rows (same idiom as `issue_aggregate_terminal_state`). The predicate
        // pre-filters to live rows so the GROUP BY only spans agents with work.
        let rows = sqlx::query(
            "SELECT agent_id, \
                    COALESCE(SUM(status = 'running'), 0) AS running, \
                    COALESCE(SUM(status IN ('queued','dispatched')), 0) AS queued \
               FROM agent_task_queue \
              WHERE workspace_id = ? \
                AND status IN ('queued','dispatched','running') \
              GROUP BY agent_id",
        )
        .bind(workspace_id)
        .fetch_all(pool)
        .await?;
        rows.iter()
            .map(|row| {
                Ok((
                    row.try_get::<String, _>("agent_id")?,
                    (
                        row.try_get::<i64, _>("running")?,
                        row.try_get::<i64, _>("queued")?,
                    ),
                ))
            })
            .collect()
    }

    /// Live (non-terminal) counts for ONE agent — the single-row backing for the
    /// `agent_update` / `agent_archive` CRUD responses, so their row's workload
    /// is byte-identical to the same agent's `agents_list` row. Returns
    /// `(running, queued)`, or `(0, 0)` when the agent has no live task.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the query fails.
    pub async fn live_workload_for_agent(
        pool: &SqlitePool,
        agent_id: &str,
    ) -> Result<(i64, i64), sqlx::Error> {
        let row = sqlx::query(
            "SELECT COALESCE(SUM(status = 'running'), 0) AS running, \
                    COALESCE(SUM(status IN ('queued','dispatched')), 0) AS queued \
               FROM agent_task_queue \
              WHERE agent_id = ? \
                AND status IN ('queued','dispatched','running')",
        )
        .bind(agent_id)
        .fetch_one(pool)
        .await?;
        Ok((
            row.try_get::<i64, _>("running")?,
            row.try_get::<i64, _>("queued")?,
        ))
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
     mode, session_name, repo_ref, agent_kind, branch, generation, source_branch, squad_id, \
     origin_type, origin_id";

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
        generation: row.try_get("generation")?,
        source_branch: row.try_get("source_branch")?,
        squad_id: row.try_get("squad_id")?,
        // LENIENT (migration 0056): an unknown stored kind degrades to `manual`
        // rather than failing the row — see `IssueOrigin::from_db`.
        origin: IssueOrigin::from_db(row.try_get("origin_type")?, row.try_get("origin_id")?),
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
        sqlx::query(
            "INSERT OR IGNORE INTO workspace (id, slug, name, created_at) VALUES (?, ?, ?, 0)",
        )
        .bind(WS)
        .bind(WS)
        .bind(WS)
        .execute(pool)
        .await
        .unwrap();
        sqlx::query("INSERT OR IGNORE INTO user (id, email, created_at) VALUES ('u','u@e.com',0)")
            .execute(pool)
            .await
            .unwrap();
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

    /// Insert a task row at an explicit `generation` (migration 0039), so a test can
    /// build a multi-generation (rerun / fan-out) history directly.
    async fn seed_task_gen(
        pool: &SqlitePool,
        issue_id: &str,
        id: &str,
        status: &str,
        created_at: i64,
        generation: i64,
    ) {
        sqlx::query(
            "INSERT INTO agent_task_queue (id, workspace_id, runtime_id, agent_id, issue_id, status, created_at, generation) \
             VALUES (?, ?, 'rt', 'ag', ?, ?, ?, ?)",
        )
        .bind(id).bind(WS).bind(issue_id).bind(status).bind(created_at).bind(generation)
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
        assert_eq!(
            ids,
            vec!["m2", "m1", "leader"],
            "the whole active set, newest first; the done task excluded"
        );
    }

    /// A foreign-workspace issue id matches no active task (tenant guard).
    #[tokio::test]
    async fn active_set_is_workspace_scoped() {
        let (_d, store) = open().await;
        let pool = store.pool();
        seed_chain(pool, "iss").await;
        seed_task(pool, "iss", "t", "running", 1).await;
        assert!(
            TaskRepo::active_tasks_for_issue(pool, "other-ws", "iss")
                .await
                .unwrap()
                .is_empty()
        );
    }

    /// The aggregate is `None` while ANY sibling is still active (the drain gate),
    /// then resolves to the highest-precedence terminal token once the set drains.
    #[tokio::test]
    async fn aggregate_gates_on_drain_then_folds_by_precedence() {
        let (_d, store) = open().await;
        let pool = store.pool();

        // No tasks at all → nothing to move on.
        seed_chain(pool, "empty").await;
        assert_eq!(
            TaskRepo::issue_aggregate_terminal_state(pool, WS, "empty").await.unwrap(),
            None
        );

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
        TaskRepo::transition_status(
            pool,
            WS,
            "c",
            ainb_hangar_core::task_status::TaskStatus::Done,
            4,
        )
        .await
        .unwrap();
        assert_eq!(
            TaskRepo::issue_aggregate_terminal_state(pool, WS, "mid")
                .await
                .unwrap()
                .as_deref(),
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
            TaskRepo::issue_aggregate_terminal_state(pool, WS, "mixed")
                .await
                .unwrap()
                .as_deref(),
            Some("failed"),
            "any failed sibling fails the card"
        );

        // done + cancelled (no failure) → cancelled wins over done.
        seed_chain(pool, "userstop").await;
        seed_task(pool, "userstop", "us-a", "done", 1).await;
        seed_task(pool, "userstop", "us-b", "cancelled", 2).await;
        assert_eq!(
            TaskRepo::issue_aggregate_terminal_state(pool, WS, "userstop")
                .await
                .unwrap()
                .as_deref(),
            Some("cancelled"),
            "a user cancel of a partly-done card cancels it"
        );

        // Fully cancelled → cancelled, never done.
        seed_chain(pool, "allcancel").await;
        seed_task(pool, "allcancel", "ac-a", "cancelled", 1).await;
        seed_task(pool, "allcancel", "ac-b", "cancelled", 2).await;
        assert_eq!(
            TaskRepo::issue_aggregate_terminal_state(pool, WS, "allcancel")
                .await
                .unwrap()
                .as_deref(),
            Some("cancelled")
        );
    }

    /// A retryable failure's parent row is SUPERSEDED by its retry child (codex
    /// F1): once the child completes `done`, the generation aggregates `done` —
    /// the stale failed attempt does not poison the card. A CAPPED chain (last
    /// failure has no child) still aggregates `failed`.
    #[tokio::test]
    async fn aggregate_ignores_retry_superseded_attempts() {
        let (_d, store) = open().await;
        let pool = store.pool();
        seed_chain(pool, "retry").await;

        // Parent failed, retry child (same generation, parent_task_id set) done.
        seed_task_gen(pool, "retry", "parent", "failed", 1, 0).await;
        sqlx::query(
            "INSERT INTO agent_task_queue \
             (id, workspace_id, runtime_id, agent_id, issue_id, status, created_at, \
              generation, parent_task_id, attempt) \
             VALUES ('child', ?, 'rt', 'ag', 'retry', 'done', 2, 0, 'parent', 2)",
        )
        .bind(WS)
        .execute(pool)
        .await
        .unwrap();
        assert_eq!(
            TaskRepo::issue_aggregate_terminal_state(pool, WS, "retry")
                .await
                .unwrap()
                .as_deref(),
            Some("done"),
            "the superseded failed attempt is excluded; the retry child's done wins"
        );

        // A capped chain: the child itself fails with no further child → failed.
        seed_chain(pool, "capped").await;
        seed_task_gen(pool, "capped", "cp-parent", "failed", 1, 0).await;
        sqlx::query(
            "INSERT INTO agent_task_queue \
             (id, workspace_id, runtime_id, agent_id, issue_id, status, created_at, \
              generation, parent_task_id, attempt) \
             VALUES ('cp-child', ?, 'rt', 'ag', 'capped', 'failed', 2, 0, 'cp-parent', 2)",
        )
        .bind(WS)
        .execute(pool)
        .await
        .unwrap();
        assert_eq!(
            TaskRepo::issue_aggregate_terminal_state(pool, WS, "capped")
                .await
                .unwrap()
                .as_deref(),
            Some("failed"),
            "a capped chain's LAST failure has no child and still fails the card"
        );
    }

    /// `next_generation_for_issue` is 0 for an unseen issue and one past the highest
    /// generation present otherwise — the value a fresh Run / rerun stamps.
    #[tokio::test]
    async fn next_generation_bumps_past_the_highest() {
        let (_d, store) = open().await;
        let pool = store.pool();
        seed_chain(pool, "iss").await;
        assert_eq!(
            TaskRepo::next_generation_for_issue(pool, "iss").await.unwrap(),
            0,
            "never-run → 0"
        );
        seed_task_gen(pool, "iss", "g0", "failed", 1, 0).await;
        assert_eq!(
            TaskRepo::next_generation_for_issue(pool, "iss").await.unwrap(),
            1,
            "one past gen 0"
        );
        seed_task_gen(pool, "iss", "g1", "done", 2, 1).await;
        assert_eq!(
            TaskRepo::next_generation_for_issue(pool, "iss").await.unwrap(),
            2,
            "one past gen 1"
        );
    }

    /// The 8ln bug: a failed run (gen 0) then a clean rerun (gen 1, done) must
    /// aggregate to `done` — the old gen-0 `failed` row no longer poisons the card,
    /// so a failed-then-rerun-successful card auto-moves to the DONE column.
    #[tokio::test]
    async fn aggregate_scopes_to_latest_generation_after_a_rerun() {
        let (_d, store) = open().await;
        let pool = store.pool();
        seed_chain(pool, "rerun").await;

        // Generation 0: the card ran and FAILED.
        seed_task_gen(pool, "rerun", "g0", "failed", 1, 0).await;
        assert_eq!(
            TaskRepo::issue_aggregate_terminal_state(pool, WS, "rerun")
                .await
                .unwrap()
                .as_deref(),
            Some("failed"),
            "the only generation is the failed one"
        );

        // Generation 1: the user reruns and it SUCCEEDS. The stale gen-0 failed row
        // is excluded — the card is now `done`.
        seed_task_gen(pool, "rerun", "g1", "done", 2, 1).await;
        assert_eq!(
            TaskRepo::issue_aggregate_terminal_state(pool, WS, "rerun")
                .await
                .unwrap()
                .as_deref(),
            Some("done"),
            "the latest generation succeeded — prior failure does not poison it"
        );

        // A gen-1 squad sibling still running re-gates the drain (aggregate None),
        // proving the drain gate is also generation-scoped.
        seed_task_gen(pool, "rerun", "g1-b", "running", 3, 1).await;
        assert_eq!(
            TaskRepo::issue_aggregate_terminal_state(pool, WS, "rerun").await.unwrap(),
            None,
            "a live sibling in the latest generation means the run has not drained"
        );
    }
}
