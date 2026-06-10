//! The `ClaimTask` service: atomic `queued -> dispatched` claim.
//!
//! [`ClaimTaskService::claim_for_runtime`] is the head of the daemon's work
//! loop: a runtime polls for the oldest `queued` task it owns, atomically flips
//! it to `dispatched`, and stamps `dispatched_at`. The whole transition is one
//! `UPDATE ... WHERE id = (SELECT ... LIMIT 1) RETURNING *` statement so two
//! daemons (or two poll iterations) can never claim the same row — `SQLite`
//! serialises the write and `RETURNING` reports exactly the row this statement
//! mutated. An empty queue is `Ok(None)` (the caller sleeps and retries), not an
//! error.
//!
//! # Per-agent concurrency cap
//!
//! The candidate `SELECT` excludes any task whose agent already has
//! `max_concurrent_tasks` rows in `running` — Multica's `CountRunningTasks`
//! guard (`task.go:761`). This keeps a single agent from being dispatched more
//! concurrent work than its runtime can handle. The count and the claim happen
//! in one statement, so the cap holds even under concurrent claims.
//!
//! Mirrors Multica's `task.go` claim path.

use ainb_hangar_core::clock::HangarClock;
use sqlx::{Row, SqlitePool};

/// The minimal projection of a freshly-claimed task the daemon needs to start a
/// run. A claimed row is always `dispatched`, so no `status` field is carried.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimedTask {
    /// The claimed task's primary key.
    pub id: String,
    /// Agent that will execute the task (`agent.id`).
    pub agent_id: String,
    /// Runtime the task was claimed for (`agent_runtime.id`).
    pub runtime_id: String,
    /// Originating issue (`issue.id`), or `None` for chat / autopilot tasks.
    pub issue_id: Option<String>,
    /// Prior provider session id to resume, or `None` for a fresh run.
    pub prior_session_id: Option<String>,
    /// Prior working directory to resume in, or `None`.
    pub prior_work_dir: Option<String>,
    /// When the claim happened (`HangarClock::now_ms`), epoch milliseconds.
    pub dispatched_at: i64,
}

/// Stateless claim service over the `agent_task_queue` table.
pub struct ClaimTaskService;

impl ClaimTaskService {
    /// Atomically claim the oldest claimable `queued` task for `runtime_id`,
    /// flipping it to `dispatched` and stamping `dispatched_at = clock.now_ms()`.
    ///
    /// A task is *claimable* when it is `queued`, bound to `runtime_id`, and its
    /// agent has fewer than `max_concurrent_tasks` rows currently `running`.
    /// Returns the claimed projection, or `Ok(None)` when nothing is claimable
    /// (empty queue, no work for this runtime, or every candidate agent is at
    /// its concurrency cap).
    ///
    /// The select-and-update is a single statement, so concurrent callers never
    /// claim the same row: `SQLite` serialises the write and `RETURNING` yields
    /// only the row this statement actually transitioned.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the statement or the row decode fails.
    #[tracing::instrument(
        name = "task.claim",
        skip(pool, clock),
        fields(runtime_id = %runtime_id, task_id = tracing::field::Empty, workspace_id = tracing::field::Empty)
    )]
    pub async fn claim_for_runtime(
        pool: &SqlitePool,
        runtime_id: &str,
        clock: &dyn HangarClock,
    ) -> Result<Option<ClaimedTask>, sqlx::Error> {
        let now = clock.now_ms();
        let row = sqlx::query(CLAIM_SQL).bind(now).bind(runtime_id).fetch_optional(pool).await?;
        // Record the claimed identity onto the span once known. `workspace_id`
        // comes back in the RETURNING projection purely for observability (it is
        // not part of the [`ClaimedTask`] the caller consumes). An empty queue
        // leaves both fields unset.
        if let Some(r) = row.as_ref() {
            let span = tracing::Span::current();
            span.record("task_id", r.try_get::<String, _>("id")?.as_str());
            span.record(
                "workspace_id",
                r.try_get::<String, _>("workspace_id")?.as_str(),
            );
        }
        row.map(|r| claimed_from_row(&r)).transpose()
    }
}

/// Atomic claim statement.
///
/// The candidate sub-select picks the oldest `queued` task for the runtime
/// whose agent is under its `max_concurrent_tasks` cap (a correlated COUNT of
/// the agent's `running` rows). The outer `UPDATE ... RETURNING` then flips
/// exactly that row and returns the projection [`claimed_from_row`] decodes.
/// `?1` = `dispatched_at` (now), `?2` = `runtime_id`.
const CLAIM_SQL: &str = "\
UPDATE agent_task_queue \
SET status = 'dispatched', dispatched_at = ?1 \
WHERE id = ( \
    SELECT q.id FROM agent_task_queue AS q \
    JOIN agent AS a ON a.id = q.agent_id \
    WHERE q.status = 'queued' AND q.runtime_id = ?2 \
      AND ( \
        SELECT COUNT(*) FROM agent_task_queue AS r \
        WHERE r.agent_id = q.agent_id AND r.status = 'running' \
      ) < a.max_concurrent_tasks \
    ORDER BY q.created_at, q.id \
    LIMIT 1 \
) \
RETURNING id, workspace_id, agent_id, runtime_id, issue_id, session_id, work_dir, dispatched_at";

/// Decode a [`ClaimedTask`] from the `RETURNING` row of [`CLAIM_SQL`].
///
/// `session_id` / `work_dir` map to the resume hints (`prior_session_id` /
/// `prior_work_dir`); `dispatched_at` is non-null here because the same
/// statement just set it.
fn claimed_from_row(row: &sqlx::sqlite::SqliteRow) -> Result<ClaimedTask, sqlx::Error> {
    Ok(ClaimedTask {
        id: row.try_get("id")?,
        agent_id: row.try_get("agent_id")?,
        runtime_id: row.try_get("runtime_id")?,
        issue_id: row.try_get("issue_id")?,
        prior_session_id: row.try_get("session_id")?,
        prior_work_dir: row.try_get("work_dir")?,
        dispatched_at: row.try_get("dispatched_at")?,
    })
}
