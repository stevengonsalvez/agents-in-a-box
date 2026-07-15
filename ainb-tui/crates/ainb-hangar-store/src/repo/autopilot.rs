//! Typed repository wrapper over the `autopilot` + `autopilot_run` tables (P7.2).
//!
//! A stateless sqlx layer in the house style (`&SqlitePool` first arg, runtime
//! `sqlx::query` not the `query!` macro). It is the concrete backend the daemon's
//! [`ainb_hangar_core::autopilot::service::AutopilotService`] drives.
//!
//! # Cron validation happens before the insert
//!
//! [`AutopilotRepo::create`] parses `cron_expr` via P7.1
//! ([`ainb_hangar_core::autopilot::cron::parse_cron`]) **first** and returns
//! [`AutopilotRepoError::Cron`] with **no row written** on a malformed
//! expression. Only on a valid expression does it compute the cached
//! `next_tick_at` (epoch-ms, strictly after the injected clock's `now`) and
//! `INSERT`.
//!
//! # Workspace scoping (every by-id method)
//!
//! Foreign keys ARE enforced (sqlx enables `PRAGMA foreign_keys` by default), but
//! an FK only constrains that a parent row EXISTS — it says nothing about WHICH
//! tenant it belongs to. So tenant isolation is enforced in application SQL, not
//! by the engine. **Every by-id read and mutation filters
//! `WHERE id = ? AND workspace_id = ?`**, and [`AutopilotRepo::list_runs`] joins
//! through `autopilot` to verify the workspace — a caller holding an
//! [`AutopilotId`] minted in workspace A can never read, toggle, or list the runs
//! of workspace B's autopilot. The `(workspace_id, name)` unique index
//! (`idx_autopilot_workspace_name`, migration 0009) guarantees a name resolves to
//! exactly one row per tenant.
//!
//! # Enable recomputes from *now*
//!
//! [`AutopilotRepo::enable`] recomputes `next_tick_at` strictly after the current
//! clock instant before flipping `enabled = 1`, so a long-disabled autopilot
//! fires once going forward rather than replaying every missed tick.
//! [`AutopilotRepo::disable`] leaves `next_tick_at` untouched.

use ainb_hangar_core::autopilot::cron::{
    CronError, millis_to_utc, next_tick_after, parse_cron, utc_to_millis,
};
use ainb_hangar_core::clock::HangarClock;
use ainb_hangar_core::idgen::{IdGen, SystemIdGen};
use ainb_hangar_core::ids::{AgentId, AutopilotId, AutopilotRunId, WorkspaceId};
use sqlx::SqlitePool;

/// What the FIRE path materialises for each autopilot tick (the `execution_mode`
/// column, migration 0019).
///
/// Orthogonal to [`ConcurrencyPolicy`]: this decides what one *fired* tick
/// produces, not whether it fires under contention.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ExecutionMode {
    /// The fired tick enqueues a task with `issue_id = NULL` — a background run
    /// that produces no tracked issue. The v1 hard-coded path and the DEFAULT.
    #[default]
    RunOnly,
    /// The fired tick first creates a fresh `issue` (authored by the autopilot's
    /// agent), then enqueues the task AGAINST that issue, so the run has a
    /// tracked work item.
    CreateIssue,
}

impl ExecutionMode {
    /// The literal stored in the `execution_mode` column.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::RunOnly => "run_only",
            Self::CreateIssue => "create_issue",
        }
    }

    /// Parse a stored `execution_mode` value. An unrecognised string falls back
    /// to the [`RunOnly`](Self::RunOnly) default rather than erroring — a forward
    /// compatibility guard, since the column `CHECK` already rejects junk on
    /// write.
    #[must_use]
    pub fn from_db_str(s: &str) -> Self {
        match s {
            "create_issue" => Self::CreateIssue,
            _ => Self::RunOnly,
        }
    }
}

/// What the SCHEDULER does when a tick comes due while the autopilot is already
/// at `max_concurrent_runs` in-flight runs (the `concurrency_policy` column,
/// migration 0019).
///
/// Orthogonal to [`ExecutionMode`]: this decides whether/how a tick fires under
/// contention, not what a fired tick produces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ConcurrencyPolicy {
    /// Drop the tick — no run, no task; warn + emit `tick_skipped`; reschedule to
    /// the next slot. The v1 behaviour and the DEFAULT.
    #[default]
    Skip,
    /// Fire the tick ANYWAY — create the run + task. The shared claim/dispatch
    /// queue then drains the enqueued tasks in order, so the second tick RUNS
    /// AFTER the in-flight one rather than being dropped.
    Queue,
    /// Supersede the in-flight run(s) — mark each open `autopilot_run` cancelled
    /// (stamping `completed_at`) and cancel its task — then fire a fresh run +
    /// task. The latest tick wins; stale in-flight work is abandoned.
    Replace,
}

impl ConcurrencyPolicy {
    /// The literal stored in the `concurrency_policy` column.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Skip => "skip",
            Self::Queue => "queue",
            Self::Replace => "replace",
        }
    }

    /// Parse a stored `concurrency_policy` value. An unrecognised string falls
    /// back to the [`Skip`](Self::Skip) default rather than erroring — a forward
    /// compatibility guard, since the column `CHECK` already rejects junk on
    /// write.
    #[must_use]
    pub fn from_db_str(s: &str) -> Self {
        match s {
            "queue" => Self::Queue,
            "replace" => Self::Replace,
            _ => Self::Skip,
        }
    }
}

/// A stored, cron-scheduled autopilot (one `autopilot` row).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Autopilot {
    /// Primary key (ULID string).
    pub id: String,
    /// Owning workspace (`workspace.id`).
    pub workspace_id: String,
    /// Agent to dispatch to at each tick (`agent.id`).
    pub agent_id: String,
    /// Name, unique within the workspace.
    pub name: String,
    /// Instructions handed to the agent; `None` when unset.
    pub instructions: Option<String>,
    /// Validated cron expression (UTC).
    pub cron_expr: String,
    /// Maximum simultaneous in-flight runs before the concurrency policy applies.
    pub max_concurrent_runs: i64,
    /// What the fire path materialises per tick (migration 0019).
    pub execution_mode: ExecutionMode,
    /// What the scheduler does when a tick comes due at the in-flight limit
    /// (migration 0019).
    pub concurrency_policy: ConcurrencyPolicy,
    /// Cached next-firing instant (epoch-ms); `None` when no future match.
    pub next_tick_at: Option<i64>,
    /// Whether the scheduler considers this autopilot (the `0/1` column).
    pub enabled: bool,
    /// Creation time (epoch-ms).
    pub created_at: i64,
}

/// A single firing of an [`Autopilot`] (one `autopilot_run` row).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutopilotRun {
    /// Primary key (ULID string).
    pub id: String,
    /// Owning autopilot (`autopilot.id`).
    pub autopilot_id: String,
    /// When the run was created (epoch-ms).
    pub started_at: i64,
    /// When the run finished (epoch-ms); `None` while in flight.
    pub completed_at: Option<i64>,
    /// Lifecycle status (`running` / `completed` / `failed` / `cancelled`).
    pub status: String,
}

/// The inputs to [`AutopilotRepo::create`]. `cron_expr` is validated by `create`.
#[derive(Debug, Clone)]
pub struct NewAutopilot {
    /// Owning workspace.
    pub workspace_id: WorkspaceId,
    /// Agent to dispatch to.
    pub agent_id: AgentId,
    /// Name, unique within the workspace.
    pub name: String,
    /// Optional instructions.
    pub instructions: Option<String>,
    /// Cron expression (UTC), validated on create.
    pub cron_expr: String,
    /// Maximum simultaneous in-flight runs.
    pub max_concurrent_runs: i64,
    /// What the fire path materialises per tick (migration 0019).
    pub execution_mode: ExecutionMode,
    /// What the scheduler does when a tick comes due at the in-flight limit
    /// (migration 0019).
    pub concurrency_policy: ConcurrencyPolicy,
}

/// Stateless typed wrapper over the autopilot tables.
pub struct AutopilotRepo;

impl AutopilotRepo {
    /// Create an autopilot.
    ///
    /// Parses `req.cron_expr` first and returns [`AutopilotRepoError::Cron`] with
    /// **no row written** on a malformed/out-of-range expression. On a valid
    /// expression, computes `next_tick_at` strictly after `clock.now_ms()` and
    /// inserts. Returns the persisted [`AutopilotId`].
    ///
    /// # Errors
    ///
    /// - [`AutopilotRepoError::Cron`] when `cron_expr` fails to parse — before
    ///   any insert.
    /// - [`AutopilotRepoError::Db`] on a SQL failure, most notably a
    ///   `(workspace_id, name)` uniqueness conflict (duplicate name) or an
    ///   `agent_id` / `workspace_id` FK violation.
    pub async fn create(
        pool: &SqlitePool,
        clock: &dyn HangarClock,
        req: &NewAutopilot,
    ) -> Result<AutopilotId, AutopilotRepoError> {
        let now_ms = clock.now_ms();
        // Validate (and reject) before touching the database.
        let next_tick_at = compute_next_tick(&req.cron_expr, now_ms)?;
        let id = SystemIdGen.new_ulid();

        sqlx::query(
            "INSERT INTO autopilot \
             (id, workspace_id, agent_id, name, instructions, cron_expr, \
              max_concurrent_runs, execution_mode, concurrency_policy, \
              next_tick_at, enabled, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 1, ?)",
        )
        .bind(&id)
        .bind(req.workspace_id.as_str())
        .bind(req.agent_id.as_str())
        .bind(&req.name)
        .bind(&req.instructions)
        .bind(&req.cron_expr)
        .bind(req.max_concurrent_runs)
        .bind(req.execution_mode.as_str())
        .bind(req.concurrency_policy.as_str())
        .bind(next_tick_at)
        .bind(now_ms)
        .execute(pool)
        .await?;

        AutopilotId::from_str(id).map_err(|_| AutopilotRepoError::EmptyId)
    }

    /// List every autopilot in a workspace, ordered by name.
    ///
    /// Workspace-scoped: an autopilot owned by another workspace can never
    /// appear.
    ///
    /// # Errors
    ///
    /// Returns [`AutopilotRepoError::Db`] on a SQL failure.
    pub async fn list(
        pool: &SqlitePool,
        workspace: &WorkspaceId,
    ) -> Result<Vec<Autopilot>, AutopilotRepoError> {
        let rows = sqlx::query_as::<_, Autopilot>(
            "SELECT id, workspace_id, agent_id, name, instructions, cron_expr, \
                    max_concurrent_runs, execution_mode, concurrency_policy, next_tick_at, enabled, created_at \
             FROM autopilot WHERE workspace_id = ? ORDER BY name",
        )
        .bind(workspace.as_str())
        .fetch_all(pool)
        .await?;
        Ok(rows)
    }

    /// Fetch one autopilot by id, scoped to `workspace` (a foreign id → `None`).
    ///
    /// # Errors
    ///
    /// Returns [`AutopilotRepoError::Db`] on a SQL failure.
    pub async fn get(
        pool: &SqlitePool,
        workspace: &WorkspaceId,
        id: &AutopilotId,
    ) -> Result<Option<Autopilot>, AutopilotRepoError> {
        let row = sqlx::query_as::<_, Autopilot>(
            "SELECT id, workspace_id, agent_id, name, instructions, cron_expr, \
                    max_concurrent_runs, execution_mode, concurrency_policy, next_tick_at, enabled, created_at \
             FROM autopilot WHERE id = ? AND workspace_id = ?",
        )
        .bind(id.as_str())
        .bind(workspace.as_str())
        .fetch_optional(pool)
        .await?;
        Ok(row)
    }

    /// Fetch one autopilot by id alone (workspace-agnostic).
    ///
    /// This is the deliberate exception to the workspace-scoping rule, used only
    /// by the webhook ingress: the webhook URL carries a ULID autopilot id (not a
    /// workspace), and the row's owning workspace is intrinsic to it. Every other
    /// by-id read MUST use [`AutopilotRepo::get`] with an explicit workspace.
    ///
    /// # Errors
    ///
    /// Returns [`AutopilotRepoError::Db`] on a SQL failure.
    pub async fn get_by_id(
        pool: &SqlitePool,
        id: &AutopilotId,
    ) -> Result<Option<Autopilot>, AutopilotRepoError> {
        let row = sqlx::query_as::<_, Autopilot>(
            "SELECT id, workspace_id, agent_id, name, instructions, cron_expr, \
                    max_concurrent_runs, execution_mode, concurrency_policy, next_tick_at, enabled, created_at \
             FROM autopilot WHERE id = ?",
        )
        .bind(id.as_str())
        .fetch_optional(pool)
        .await?;
        Ok(row)
    }

    /// Disable an autopilot, scoped to `workspace`. Leaves `next_tick_at`
    /// untouched (it is ignored while disabled), preserving the cached value for
    /// re-enable inspection. A foreign id touches no row.
    ///
    /// # Errors
    ///
    /// Returns [`AutopilotRepoError::Db`] on a SQL failure.
    pub async fn disable(
        pool: &SqlitePool,
        workspace: &WorkspaceId,
        id: &AutopilotId,
    ) -> Result<(), AutopilotRepoError> {
        sqlx::query("UPDATE autopilot SET enabled = 0 WHERE id = ? AND workspace_id = ?")
            .bind(id.as_str())
            .bind(workspace.as_str())
            .execute(pool)
            .await?;
        Ok(())
    }

    /// Enable an autopilot, scoped to `workspace`, recomputing `next_tick_at`
    /// strictly after the current clock instant so a long-disabled autopilot
    /// fires once going forward rather than replaying every missed tick.
    ///
    /// Reads the stored `cron_expr` (within the workspace), recomputes, then
    /// flips `enabled = 1` and writes the fresh `next_tick_at`. A foreign /
    /// absent id is a no-op (nothing read, nothing written).
    ///
    /// # Errors
    ///
    /// - [`AutopilotRepoError::Cron`] if the stored `cron_expr` no longer parses
    ///   (a corrupt-row guard; impossible given [`create`] validates).
    /// - [`AutopilotRepoError::Db`] on a SQL failure.
    pub async fn enable(
        pool: &SqlitePool,
        clock: &dyn HangarClock,
        workspace: &WorkspaceId,
        id: &AutopilotId,
    ) -> Result<(), AutopilotRepoError> {
        // Read the stored cron, scoped to the workspace.
        let cron_expr: Option<String> =
            sqlx::query_scalar("SELECT cron_expr FROM autopilot WHERE id = ? AND workspace_id = ?")
                .bind(id.as_str())
                .bind(workspace.as_str())
                .fetch_optional(pool)
                .await?;
        let Some(cron_expr) = cron_expr else {
            // Foreign / absent id: no-op.
            return Ok(());
        };
        let next_tick_at = compute_next_tick(&cron_expr, clock.now_ms())?;
        sqlx::query(
            "UPDATE autopilot SET enabled = 1, next_tick_at = ? \
             WHERE id = ? AND workspace_id = ?",
        )
        .bind(next_tick_at)
        .bind(id.as_str())
        .bind(workspace.as_str())
        .execute(pool)
        .await?;
        Ok(())
    }

    /// List an autopilot's runs, latest-first (`ORDER BY started_at DESC`),
    /// capped at `limit`. Scoped to `workspace` by joining through `autopilot`:
    /// a foreign autopilot id yields an empty set rather than leaking another
    /// tenant's run history.
    ///
    /// # Errors
    ///
    /// Returns [`AutopilotRepoError::Db`] on a SQL failure.
    pub async fn list_runs(
        pool: &SqlitePool,
        workspace: &WorkspaceId,
        autopilot_id: &AutopilotId,
        limit: u32,
    ) -> Result<Vec<AutopilotRun>, AutopilotRepoError> {
        let rows = sqlx::query_as::<_, AutopilotRun>(
            "SELECT r.id, r.autopilot_id, r.started_at, r.completed_at, r.status \
             FROM autopilot_run r \
             JOIN autopilot a ON a.id = r.autopilot_id \
             WHERE r.autopilot_id = ? AND a.workspace_id = ? \
             ORDER BY r.started_at DESC \
             LIMIT ?",
        )
        .bind(autopilot_id.as_str())
        .bind(workspace.as_str())
        .bind(i64::from(limit))
        .fetch_all(pool)
        .await?;
        Ok(rows)
    }

    /// Insert one `autopilot_run` row (test/seed helper for run-history reads).
    ///
    /// The scheduler's fire path (P7.4) creates runs inside its enqueue
    /// transaction; this stand-alone insert exists so the run-history read can be
    /// exercised without that machinery.
    ///
    /// # Errors
    ///
    /// Returns [`AutopilotRepoError::Db`] on a SQL failure (e.g. an
    /// `autopilot_id` FK violation).
    pub async fn insert_run(
        pool: &SqlitePool,
        autopilot_id: &AutopilotId,
        started_at: i64,
        status: &str,
    ) -> Result<AutopilotRunId, AutopilotRepoError> {
        let id = SystemIdGen.new_ulid();
        sqlx::query(
            "INSERT INTO autopilot_run (id, autopilot_id, started_at, status) \
             VALUES (?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(autopilot_id.as_str())
        .bind(started_at)
        .bind(status)
        .execute(pool)
        .await?;
        AutopilotRunId::from_str(id).map_err(|_| AutopilotRepoError::EmptyId)
    }
}

/// Parse `cron_expr` and compute the next firing instant strictly after
/// `now_ms`, as epoch milliseconds. Shared by `create` and `enable`.
///
/// Returns `Ok(None)` when the schedule is valid but has no future match.
fn compute_next_tick(cron_expr: &str, now_ms: i64) -> Result<Option<i64>, AutopilotRepoError> {
    let schedule = parse_cron(cron_expr)?;
    let Some(after) = millis_to_utc(now_ms) else {
        return Ok(None);
    };
    Ok(next_tick_after(&schedule, after).map(utc_to_millis))
}

/// Error surface for [`AutopilotRepo`].
///
/// Splits a cron-validation failure (caller passed a malformed expression,
/// rejected before any insert) from a database failure (uniqueness conflict, FK
/// violation, IO).
#[derive(Debug, thiserror::Error)]
pub enum AutopilotRepoError {
    /// The cron expression could not be parsed; no row was written.
    #[error(transparent)]
    Cron(#[from] CronError),
    /// An underlying `sqlx` failure (uniqueness conflict, FK violation, IO).
    #[error(transparent)]
    Db(#[from] sqlx::Error),
    /// A minted primary key was empty — an invariant violation that should be
    /// unreachable (PKs are 26-char ULIDs). Surfaced rather than `unwrap`'d.
    #[error("corrupt autopilot row: empty primary key")]
    EmptyId,
}

impl sqlx::FromRow<'_, sqlx::sqlite::SqliteRow> for Autopilot {
    fn from_row(row: &sqlx::sqlite::SqliteRow) -> Result<Self, sqlx::Error> {
        use sqlx::Row;
        Ok(Self {
            id: row.try_get("id")?,
            workspace_id: row.try_get("workspace_id")?,
            agent_id: row.try_get("agent_id")?,
            name: row.try_get("name")?,
            instructions: row.try_get("instructions")?,
            cron_expr: row.try_get("cron_expr")?,
            max_concurrent_runs: row.try_get("max_concurrent_runs")?,
            execution_mode: ExecutionMode::from_db_str(
                &row.try_get::<String, _>("execution_mode")?,
            ),
            concurrency_policy: ConcurrencyPolicy::from_db_str(
                &row.try_get::<String, _>("concurrency_policy")?,
            ),
            next_tick_at: row.try_get("next_tick_at")?,
            // SQLite stores the boolean as INTEGER 0/1.
            enabled: row.try_get::<i64, _>("enabled")? != 0,
            created_at: row.try_get("created_at")?,
        })
    }
}

impl sqlx::FromRow<'_, sqlx::sqlite::SqliteRow> for AutopilotRun {
    fn from_row(row: &sqlx::sqlite::SqliteRow) -> Result<Self, sqlx::Error> {
        use sqlx::Row;
        Ok(Self {
            id: row.try_get("id")?,
            autopilot_id: row.try_get("autopilot_id")?,
            started_at: row.try_get("started_at")?,
            completed_at: row.try_get("completed_at")?,
            status: row.try_get("status")?,
        })
    }
}
