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
//!
//! # Every mutation mints a rule version (multica parity #14)
//!
//! Migration 0061 added the append-only `autopilot_rule_version` accountability
//! ledger. Each mutating method has an actor-carrying `_as` twin
//! ([`AutopilotRepo::create_as`], [`AutopilotRepo::update_as`],
//! [`AutopilotRepo::enable_as`], [`AutopilotRepo::disable_as`],
//! [`AutopilotRepo::set_api_trigger_enabled_as`]) that runs the mutation and the
//! ledger write in ONE transaction. The original names are one-line delegates
//! passing `actor = None`, so **the invariant has no hole**: every mutation
//! mints a version, legacy callers simply mint an unattributed one.
//!
//! [`AutopilotRepo::update_as`] is the edit surface that did not exist before —
//! prior to it an autopilot's cron, instructions, agent or policy could not be
//! changed at all without hand-editing sqlite. It is the one place where a
//! mutation may mint NO version: a rename is COSMETIC
//! ([`ainb_hangar_core::autopilot::rule_version::classify`] returns `None`), and
//! a cosmetic edit must not re-assign blame for an unattended run.

use ainb_hangar_core::actor::ActorRef;
use ainb_hangar_core::autopilot::cron::{
    CronError, millis_to_utc, next_tick_after, parse_cron, utc_to_millis,
};
use ainb_hangar_core::autopilot::rule_version::{
    AutopilotConfigSnapshot, RuleChangeKind, classify,
};
use ainb_hangar_core::clock::{HangarClock, SystemClock};
use ainb_hangar_core::idgen::{IdGen, SystemIdGen};
use ainb_hangar_core::ids::{AgentId, AutopilotId, AutopilotRunId, WorkspaceId};
use sqlx::SqlitePool;

use super::autopilot_rule_version::AutopilotRuleVersionRepo;

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

/// Who may WRITE this rule (the `access_mode` column, migration 0064, multica
/// parity #27).
///
/// Orthogonal to [`ExecutionMode`] and [`ConcurrencyPolicy`]: this decides who
/// may change the rule, not what it does. The predicate itself lives in
/// [`super::autopilot_access::can_write`] — this enum is only the stored token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AccessMode {
    /// Any actor in the workspace may write the rule. The pre-0064 behaviour,
    /// the column DEFAULT, and what every existing row upgrades to — a
    /// deny-by-default upgrade would silently lock an install out of its own
    /// automations (migration 0064 decision 4).
    #[default]
    Open,
    /// Only the rule's owner, a workspace owner/admin, or an explicit `editor`
    /// collaborator may write the rule.
    Restricted,
}

impl AccessMode {
    /// The literal stored in the `access_mode` column.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Restricted => "restricted",
        }
    }

    /// Parse a stored `access_mode` value. An unrecognised string falls back to
    /// [`Open`](Self::Open) — the same forward-compatibility guard
    /// [`ExecutionMode`] and [`ConcurrencyPolicy`] use, and the safe direction:
    /// a token this build cannot read must never become an unexplained lockout.
    /// The column `CHECK` already rejects junk on write.
    #[must_use]
    pub fn from_db_str(s: &str) -> Self {
        match s {
            "restricted" => Self::Restricted,
            _ => Self::Open,
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
    /// Whether the bare programmatic `api` trigger is armed (migration 0057).
    ///
    /// The third trigger surface after cron (0009) and webhook (0018): no cron
    /// expression, no HMAC — a caller with normal API access fires the autopilot
    /// directly. Defaults to `false`, mirroring `webhook_enabled`: a
    /// half-configured autopilot is never firable.
    pub api_trigger_enabled: bool,
    /// Who may WRITE this rule (migration 0064). `Open` for every pre-0064 row.
    pub access_mode: AccessMode,
    /// Creation time (epoch-ms).
    pub created_at: i64,
}

/// A single firing of an [`Autopilot`] (one `autopilot_run` row).
///
/// `Default` is derived so test fixtures can spread `..Default::default()` and
/// stay green when the row grows a column (as migration 0061's attribution pair
/// did) instead of drifting into a compile error per call site.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AutopilotRun {
    /// Primary key (ULID string).
    pub id: String,
    /// Owning autopilot (`autopilot.id`).
    pub autopilot_id: String,
    /// When the run was created (epoch-ms).
    pub started_at: i64,
    /// When the run finished (epoch-ms); `None` while in flight.
    pub completed_at: Option<i64>,
    /// Lifecycle status (`running` / `completed` / `failed` / `cancelled` /
    /// `skipped`). `skipped` (migration 0057) is TERMINAL — a dispatch the
    /// admission gate intentionally declined, kept distinct from `failed` so it
    /// does not pollute the failure-rate signal.
    pub status: String,
    /// Which trigger fired this run: `schedule` | `manual` | `webhook` | `api`
    /// (migration 0057). Pre-0057 rows backfilled to `schedule`.
    pub source: String,
    /// Why a `skipped` run was declined (the admission reason); `None` for every
    /// other status.
    pub failure_reason: Option<String>,
    /// The ACCOUNTABLE HUMAN for this run as a canonical actor ref (migration
    /// 0061). For an unattended fire this is the newest rule version's
    /// `published_by`; for a manual fire it is the human who clicked. `None`
    /// when neither is resolvable — an honest unknown (every pre-0061 run row
    /// reads `None`), never a fabricated actor.
    pub accountable_actor: Option<String>,
    /// HOW [`accountable_actor`](Self::accountable_actor) was resolved:
    /// `rule_owner` | `direct_human`. `None` exactly when
    /// `accountable_actor` is `None`.
    pub attribution: Option<String>,
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
    /// Whether to arm the bare programmatic `api` trigger at create time
    /// (migration 0057). `false` is the safe default.
    pub api_trigger_enabled: bool,
}

/// An all-optional patch over an [`Autopilot`]'s editable fields
/// ([`AutopilotRepo::update_as`], multica parity #14).
///
/// `None` means "leave alone". Before this existed there was **no edit surface
/// at all**: an autopilot's cron, instructions, agent or policy could not be
/// changed without hand-editing sqlite.
///
/// [`name`](Self::name) is the only COSMETIC field — changing it alone mints no
/// rule version (see [`classify`]).
#[derive(Debug, Clone, Default)]
pub struct AutopilotEdit {
    /// New display name. **Cosmetic on its own.**
    pub name: Option<String>,
    /// Re-target the rule at a different agent (`Target`).
    pub agent_id: Option<AgentId>,
    /// New instructions; `Some(None)` CLEARS them (`Instructions`).
    pub instructions: Option<Option<String>>,
    /// New cron expression (`Schedule`). Revalidated before any write, and
    /// `next_tick_at` is recomputed strictly after the clock's now.
    pub cron_expr: Option<String>,
    /// New in-flight ceiling (`Policy`).
    pub max_concurrent_runs: Option<i64>,
    /// New execution mode (`Policy`).
    pub execution_mode: Option<ExecutionMode>,
    /// New concurrency policy (`Policy`).
    pub concurrency_policy: Option<ConcurrencyPolicy>,
    /// New write-access mode (`Access`, migration 0064). Opening a rule up to
    /// more writers is exactly the kind of publish the accountability ledger
    /// exists to record, so this is SUBSTANTIVE, never cosmetic.
    pub access_mode: Option<AccessMode>,
}

impl AutopilotEdit {
    /// Whether the patch names at least one field.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.name.is_none()
            && self.agent_id.is_none()
            && self.instructions.is_none()
            && self.cron_expr.is_none()
            && self.max_concurrent_runs.is_none()
            && self.execution_mode.is_none()
            && self.concurrency_policy.is_none()
            && self.access_mode.is_none()
    }
}

/// What [`AutopilotRepo::update_as`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateOutcome {
    /// No such autopilot in this workspace. Nothing was touched (tenant
    /// scoping: a foreign id never leaks, and never writes).
    NotFound,
    /// The row was updated.
    Updated {
        /// The minted rule version, or `None` when the edit was **cosmetic**
        /// (a rename / no-op). `None` is the wire-visible proof of multica's
        /// "cosmetic edits don't count" rule: the name change still LANDED,
        /// it simply is not an accountability event.
        version: Option<i64>,
    },
}

/// The published snapshot of a stored autopilot row.
fn snapshot_of(ap: &Autopilot) -> AutopilotConfigSnapshot {
    AutopilotConfigSnapshot {
        name: ap.name.clone(),
        agent_id: ap.agent_id.clone(),
        instructions: ap.instructions.clone(),
        cron_expr: ap.cron_expr.clone(),
        max_concurrent_runs: ap.max_concurrent_runs,
        execution_mode: ap.execution_mode.as_str().to_string(),
        concurrency_policy: ap.concurrency_policy.as_str().to_string(),
        enabled: ap.enabled,
        api_trigger_enabled: ap.api_trigger_enabled,
        access_mode: ap.access_mode.as_str().to_string(),
    }
}

/// The `SELECT` column list shared by every autopilot read.
const AUTOPILOT_COLUMNS: &str = "id, workspace_id, agent_id, name, instructions, cron_expr, \
     max_concurrent_runs, execution_mode, concurrency_policy, next_tick_at, enabled, \
     api_trigger_enabled, access_mode, created_at";

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
        Self::create_as(pool, clock, req, None).await
    }

    /// Create an autopilot AND publish rule-version **v1** naming `actor` as the
    /// accountable human — in ONE transaction (multica parity #14).
    ///
    /// The transactionality is the invariant, not an optimisation: *"creation
    /// itself is a transaction that also writes rule-version v1"*, so there is
    /// **no window in which an autopilot exists with no accountable human**.
    /// A `None` actor still mints v1, with `published_by` NULL — an honest
    /// unattributed record rather than a hole in the ledger. [`create`] is the
    /// legacy delegate that passes `None`.
    ///
    /// # Errors
    ///
    /// Same surface as [`create`](Self::create): [`AutopilotRepoError::Cron`]
    /// before any write on a malformed expression, otherwise
    /// [`AutopilotRepoError::Db`].
    pub async fn create_as(
        pool: &SqlitePool,
        clock: &dyn HangarClock,
        req: &NewAutopilot,
        actor: Option<&ActorRef>,
    ) -> Result<AutopilotId, AutopilotRepoError> {
        let now_ms = clock.now_ms();
        // Validate (and reject) before touching the database.
        let next_tick_at = compute_next_tick(&req.cron_expr, now_ms)?;
        let id = SystemIdGen.new_ulid();

        let mut tx = pool.begin().await?;

        sqlx::query(
            "INSERT INTO autopilot \
             (id, workspace_id, agent_id, name, instructions, cron_expr, \
              max_concurrent_runs, execution_mode, concurrency_policy, \
              next_tick_at, enabled, api_trigger_enabled, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 1, ?, ?)",
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
        .bind(i64::from(req.api_trigger_enabled))
        .bind(now_ms)
        .execute(&mut *tx)
        .await?;

        let autopilot_id = AutopilotId::from_str(id).map_err(|_| AutopilotRepoError::EmptyId)?;
        let published = AutopilotConfigSnapshot {
            name: req.name.clone(),
            agent_id: req.agent_id.as_str().to_string(),
            instructions: req.instructions.clone(),
            cron_expr: req.cron_expr.clone(),
            max_concurrent_runs: req.max_concurrent_runs,
            execution_mode: req.execution_mode.as_str().to_string(),
            concurrency_policy: req.concurrency_policy.as_str().to_string(),
            enabled: true,
            api_trigger_enabled: req.api_trigger_enabled,
            // A rule is always born 'open' (the column default); restricting it
            // is a deliberate, separately-ledgered publish.
            access_mode: AccessMode::Open.as_str().to_string(),
        };
        AutopilotRuleVersionRepo::publish_in_tx(
            &mut tx,
            clock,
            &req.workspace_id,
            &autopilot_id,
            RuleChangeKind::Created,
            actor,
            &published.to_config_summary(None),
        )
        .await?;

        tx.commit().await?;
        Ok(autopilot_id)
    }

    /// Edit an autopilot's config, publishing a rule version when — and only
    /// when — the edit is SUBSTANTIVE (multica parity #14).
    ///
    /// The contract, in order:
    ///
    /// 1. resolve the row `WHERE id = ? AND workspace_id = ?`; a foreign /
    ///    absent id returns [`UpdateOutcome::NotFound`] and touches nothing;
    /// 2. if `cron_expr` is being changed, **parse it first** and return
    ///    [`AutopilotRepoError::Cron`] with **no row written and no version
    ///    row** — the same guarantee [`create`](Self::create) gives. On success
    ///    `next_tick_at` is recomputed strictly after `clock.now_ms()`;
    /// 3. `UPDATE` the supplied fields;
    /// 4. [`classify`] before-vs-after: `Some(kind)` publishes a version and
    ///    reports `Updated { version: Some(n) }`; `None` (name-only / no-op)
    ///    publishes NOTHING and reports `Updated { version: None }`. The name
    ///    change lands either way — cosmetic means *unversioned*, not
    ///    *rejected*;
    /// 5. commit.
    ///
    /// Everything above runs in one transaction, so a rejected step leaves the
    /// row byte-identical and the ledger untouched.
    ///
    /// # Errors
    ///
    /// - [`AutopilotRepoError::Cron`] when the new `cron_expr` fails to parse —
    ///   before any write.
    /// - [`AutopilotRepoError::Db`] on a SQL failure (a `(workspace_id, name)`
    ///   uniqueness conflict, or an `agent_id` FK violation from a re-target).
    pub async fn update_as(
        pool: &SqlitePool,
        clock: &dyn HangarClock,
        workspace: &WorkspaceId,
        id: &AutopilotId,
        edit: &AutopilotEdit,
        actor: Option<&ActorRef>,
    ) -> Result<UpdateOutcome, AutopilotRepoError> {
        let mut tx = pool.begin().await?;

        let before = sqlx::query_as::<_, Autopilot>(&format!(
            "SELECT {AUTOPILOT_COLUMNS} FROM autopilot WHERE id = ? AND workspace_id = ?"
        ))
        .bind(id.as_str())
        .bind(workspace.as_str())
        .fetch_optional(&mut *tx)
        .await?;
        let Some(before) = before else {
            // Dropping `tx` rolls back nothing (we only read) and, crucially,
            // no version row was minted for a foreign id.
            return Ok(UpdateOutcome::NotFound);
        };

        // Apply the patch to an in-memory copy so `classify` sees the real
        // before/after pair.
        let mut after = before.clone();
        if let Some(name) = &edit.name {
            after.name = name.clone();
        }
        if let Some(agent_id) = &edit.agent_id {
            after.agent_id = agent_id.as_str().to_string();
        }
        if let Some(instructions) = &edit.instructions {
            after.instructions = instructions.clone();
        }
        if let Some(cron_expr) = &edit.cron_expr {
            after.cron_expr = cron_expr.clone();
        }
        if let Some(n) = edit.max_concurrent_runs {
            after.max_concurrent_runs = n;
        }
        if let Some(mode) = edit.execution_mode {
            after.execution_mode = mode;
        }
        if let Some(policy) = edit.concurrency_policy {
            after.concurrency_policy = policy;
        }
        if let Some(mode) = edit.access_mode {
            after.access_mode = mode;
        }

        // Cron revalidation BEFORE any write: a malformed expression must leave
        // the row and the ledger untouched.
        if after.cron_expr != before.cron_expr {
            after.next_tick_at = compute_next_tick(&after.cron_expr, clock.now_ms())?;
        }

        sqlx::query(
            "UPDATE autopilot SET name = ?, agent_id = ?, instructions = ?, cron_expr = ?, \
                    max_concurrent_runs = ?, execution_mode = ?, concurrency_policy = ?, \
                    next_tick_at = ?, access_mode = ? \
             WHERE id = ? AND workspace_id = ?",
        )
        .bind(&after.name)
        .bind(&after.agent_id)
        .bind(&after.instructions)
        .bind(&after.cron_expr)
        .bind(after.max_concurrent_runs)
        .bind(after.execution_mode.as_str())
        .bind(after.concurrency_policy.as_str())
        .bind(after.next_tick_at)
        .bind(after.access_mode.as_str())
        .bind(id.as_str())
        .bind(workspace.as_str())
        .execute(&mut *tx)
        .await?;

        let before_snapshot = snapshot_of(&before);
        let after_snapshot = snapshot_of(&after);
        let version = match classify(&before_snapshot, &after_snapshot) {
            Some(kind) => Some(
                AutopilotRuleVersionRepo::publish_in_tx(
                    &mut tx,
                    clock,
                    workspace,
                    id,
                    kind,
                    actor,
                    &after_snapshot.to_config_summary(Some(&before_snapshot)),
                )
                .await?,
            ),
            // Cosmetic (a rename) or a literal no-op: the ledger is an
            // accountability record, not a change log.
            None => None,
        };

        tx.commit().await?;
        Ok(UpdateOutcome::Updated { version })
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
        let rows = sqlx::query_as::<_, Autopilot>(&format!(
            "SELECT {AUTOPILOT_COLUMNS} FROM autopilot WHERE workspace_id = ? ORDER BY name"
        ))
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
        let row = sqlx::query_as::<_, Autopilot>(&format!(
            "SELECT {AUTOPILOT_COLUMNS} FROM autopilot WHERE id = ? AND workspace_id = ?"
        ))
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
        let row = sqlx::query_as::<_, Autopilot>(&format!(
            "SELECT {AUTOPILOT_COLUMNS} FROM autopilot WHERE id = ?"
        ))
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
        Self::disable_as(pool, &SystemClock, workspace, id, None).await
    }

    /// Disable an autopilot AND publish a `paused` rule version naming `actor`,
    /// in one transaction (multica parity #14).
    ///
    /// Pausing is a SUBSTANTIVE publish: it changes whether the rule fires
    /// unattended, so it re-stamps who is accountable. A foreign / absent id
    /// touches no row and mints no version. [`disable`](Self::disable) is the
    /// legacy delegate; it passes `None` and a
    /// [`SystemClock`](ainb_hangar_core::clock::SystemClock) for the ledger's
    /// `created_at` (its signature carries no clock).
    ///
    /// # Errors
    ///
    /// Returns [`AutopilotRepoError::Db`] on a SQL failure.
    pub async fn disable_as(
        pool: &SqlitePool,
        clock: &dyn HangarClock,
        workspace: &WorkspaceId,
        id: &AutopilotId,
        actor: Option<&ActorRef>,
    ) -> Result<(), AutopilotRepoError> {
        let mut tx = pool.begin().await?;
        let Some(before) = sqlx::query_as::<_, Autopilot>(&format!(
            "SELECT {AUTOPILOT_COLUMNS} FROM autopilot WHERE id = ? AND workspace_id = ?"
        ))
        .bind(id.as_str())
        .bind(workspace.as_str())
        .fetch_optional(&mut *tx)
        .await?
        else {
            return Ok(());
        };

        sqlx::query("UPDATE autopilot SET enabled = 0 WHERE id = ? AND workspace_id = ?")
            .bind(id.as_str())
            .bind(workspace.as_str())
            .execute(&mut *tx)
            .await?;

        let before_snapshot = snapshot_of(&before);
        let mut after_snapshot = before_snapshot.clone();
        after_snapshot.enabled = false;
        AutopilotRuleVersionRepo::publish_in_tx(
            &mut tx,
            clock,
            workspace,
            id,
            RuleChangeKind::Paused,
            actor,
            &after_snapshot.to_config_summary(Some(&before_snapshot)),
        )
        .await?;

        tx.commit().await?;
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
        Self::enable_as(pool, clock, workspace, id, None).await
    }

    /// Enable an autopilot AND publish a `resumed` rule version naming `actor`,
    /// in one transaction (multica parity #14).
    ///
    /// Resuming is a SUBSTANTIVE publish in multica's model — putting a rule
    /// back into unattended service re-stamps who is accountable for its runs.
    /// [`enable`](Self::enable) is the legacy delegate that passes `None`.
    ///
    /// # Errors
    ///
    /// - [`AutopilotRepoError::Cron`] if the stored `cron_expr` no longer parses
    ///   (a corrupt-row guard); nothing is written and no version is minted.
    /// - [`AutopilotRepoError::Db`] on a SQL failure.
    pub async fn enable_as(
        pool: &SqlitePool,
        clock: &dyn HangarClock,
        workspace: &WorkspaceId,
        id: &AutopilotId,
        actor: Option<&ActorRef>,
    ) -> Result<(), AutopilotRepoError> {
        let mut tx = pool.begin().await?;
        // Read the stored row, scoped to the workspace.
        let Some(before) = sqlx::query_as::<_, Autopilot>(&format!(
            "SELECT {AUTOPILOT_COLUMNS} FROM autopilot WHERE id = ? AND workspace_id = ?"
        ))
        .bind(id.as_str())
        .bind(workspace.as_str())
        .fetch_optional(&mut *tx)
        .await?
        else {
            // Foreign / absent id: no-op.
            return Ok(());
        };

        let next_tick_at = compute_next_tick(&before.cron_expr, clock.now_ms())?;
        sqlx::query(
            "UPDATE autopilot SET enabled = 1, next_tick_at = ? \
             WHERE id = ? AND workspace_id = ?",
        )
        .bind(next_tick_at)
        .bind(id.as_str())
        .bind(workspace.as_str())
        .execute(&mut *tx)
        .await?;

        let before_snapshot = snapshot_of(&before);
        let mut after_snapshot = before_snapshot.clone();
        after_snapshot.enabled = true;
        AutopilotRuleVersionRepo::publish_in_tx(
            &mut tx,
            clock,
            workspace,
            id,
            RuleChangeKind::Resumed,
            actor,
            &after_snapshot.to_config_summary(Some(&before_snapshot)),
        )
        .await?;

        tx.commit().await?;
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
            "SELECT r.id, r.autopilot_id, r.started_at, r.completed_at, r.status, \
                    r.source, r.failure_reason, r.accountable_actor, r.attribution \
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

    /// Arm (or disarm) the bare programmatic `api` trigger, scoped to
    /// `workspace` (migration 0057). A foreign / absent id touches no row.
    ///
    /// Mirrors [`AutopilotWebhookRepo::set_config`](super::autopilot_webhook::AutopilotWebhookRepo::set_config):
    /// a trigger surface is armed by an explicit operator action, never
    /// implicitly at create time.
    ///
    /// # Errors
    ///
    /// Returns [`AutopilotRepoError::Db`] on a SQL failure.
    pub async fn set_api_trigger_enabled(
        pool: &SqlitePool,
        workspace: &WorkspaceId,
        id: &AutopilotId,
        enabled: bool,
    ) -> Result<bool, AutopilotRepoError> {
        Self::set_api_trigger_enabled_as(pool, &SystemClock, workspace, id, enabled, None).await
    }

    /// Arm / disarm the `api` trigger AND publish a `trigger` rule version
    /// naming `actor`, in one transaction (multica parity #14).
    ///
    /// hangar collapses trigger config into columns on `autopilot`, so multica's
    /// PER-TRIGGER publisher (migration 189) folds into the PER-RULE version
    /// chain: arming or disarming a trigger is a substantive publish on the
    /// rule. [`set_api_trigger_enabled`](Self::set_api_trigger_enabled) is the
    /// legacy delegate (`None` actor, [`SystemClock`](ainb_hangar_core::clock::SystemClock)).
    ///
    /// # Errors
    ///
    /// Returns [`AutopilotRepoError::Db`] on a SQL failure.
    pub async fn set_api_trigger_enabled_as(
        pool: &SqlitePool,
        clock: &dyn HangarClock,
        workspace: &WorkspaceId,
        id: &AutopilotId,
        enabled: bool,
        actor: Option<&ActorRef>,
    ) -> Result<bool, AutopilotRepoError> {
        let mut tx = pool.begin().await?;
        let Some(before) = sqlx::query_as::<_, Autopilot>(&format!(
            "SELECT {AUTOPILOT_COLUMNS} FROM autopilot WHERE id = ? AND workspace_id = ?"
        ))
        .bind(id.as_str())
        .bind(workspace.as_str())
        .fetch_optional(&mut *tx)
        .await?
        else {
            return Ok(false);
        };

        sqlx::query(
            "UPDATE autopilot SET api_trigger_enabled = ? WHERE id = ? AND workspace_id = ?",
        )
        .bind(i64::from(enabled))
        .bind(id.as_str())
        .bind(workspace.as_str())
        .execute(&mut *tx)
        .await?;

        let before_snapshot = snapshot_of(&before);
        let mut after_snapshot = before_snapshot.clone();
        after_snapshot.api_trigger_enabled = enabled;
        AutopilotRuleVersionRepo::publish_in_tx(
            &mut tx,
            clock,
            workspace,
            id,
            RuleChangeKind::Trigger,
            actor,
            &after_snapshot.to_config_summary(Some(&before_snapshot)),
        )
        .await?;

        tx.commit().await?;
        Ok(true)
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
            api_trigger_enabled: row.try_get::<i64, _>("api_trigger_enabled")? != 0,
            access_mode: AccessMode::from_db_str(&row.try_get::<String, _>("access_mode")?),
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
            source: row.try_get("source")?,
            failure_reason: row.try_get("failure_reason")?,
            accountable_actor: row.try_get("accountable_actor")?,
            attribution: row.try_get("attribution")?,
        })
    }
}
