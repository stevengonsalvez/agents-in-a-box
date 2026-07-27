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

use ainb_hangar_core::actor::ActorRef;
use ainb_hangar_core::clock::HangarClock;
use ainb_hangar_core::idgen::{IdGen, SystemIdGen};
use ainb_hangar_core::ids::{AutopilotRunId, IdError, TaskId};
use ainb_hangar_core::origin::IssueOrigin;
use sqlx::{Row, SqlitePool};

use super::autopilot::{Autopilot, ConcurrencyPolicy, ExecutionMode};
use super::autopilot_rule_version::AutopilotRuleVersionRepo;
use super::task::{NewTask, TaskRepo};

/// HOW a run's accountable human is resolved — multica's attribution fork
/// (migration 0061, parity #14).
///
/// *"The attribution model forks on how dispatch was invoked, not just who
/// created the rule."* multica keeps its `originator_user_id` NULL for
/// unattended fires on purpose; the accountable human is a separate question,
/// answered by the rule-version chain.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum RunAttribution {
    /// UNATTENDED (schedule / webhook / api): the accountable human is the
    /// newest rule version's `published_by`, stamped `rule_owner`. `NULL` for an
    /// unversioned (pre-0061, never-edited) rule — an honest unknown, never a
    /// fabricated actor.
    #[default]
    RuleOwner,
    /// A named human fired it by hand ("run now"), stamped `direct_human`. This
    /// is the human at the keyboard, NOT the rule's owner — the whole point of
    /// the fork.
    DirectHuman(ActorRef),
}

impl RunAttribution {
    /// The literal stored in the `attribution` column.
    #[must_use]
    pub const fn as_db_str(&self) -> &'static str {
        match self {
            Self::RuleOwner => "rule_owner",
            Self::DirectHuman(_) => "direct_human",
        }
    }

    /// Resolve `(accountable_actor, attribution)` for this run, INSIDE the
    /// caller's transaction so the pair can never disagree with the ledger the
    /// same commit observes.
    ///
    /// Both halves are `None` together: an unversioned rule fired unattended has
    /// no accountable human, and saying so honestly beats inventing one.
    async fn resolve_in_tx(
        &self,
        tx: &mut sqlx::SqliteConnection,
        autopilot_id: &str,
    ) -> Result<(Option<String>, Option<&'static str>), sqlx::Error> {
        let actor = match self {
            Self::RuleOwner => {
                AutopilotRuleVersionRepo::latest_publisher_in_tx(tx, autopilot_id).await?
            }
            Self::DirectHuman(a) => Some(a.to_string()),
        };
        Ok(match actor {
            Some(a) => (Some(a), Some(self.as_db_str())),
            None => (None, None),
        })
    }
}

/// Which trigger fired an [`Autopilot`] — the `autopilot_run.source` column
/// (migration 0057).
///
/// Multica stamps the same discriminant on every dispatch
/// (`DispatchAutopilot(ctx, autopilot, triggerID, source, payload)`,
/// `service/autopilot.go:44-51`) so a run's provenance survives into analytics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RunSource {
    /// The cron scheduler's tick loop (the pre-0057 path, and the column default).
    #[default]
    Schedule,
    /// An operator fired it by hand (`hangar/autopilot_fire_now`, the manager
    /// pane's "run now", `ainb hangar autopilot run`).
    Manual,
    /// The authenticated HMAC webhook ingress (migration 0018).
    Webhook,
    /// The bare programmatic `api` trigger (migration 0057): no cron, no HMAC —
    /// a caller with normal API access fires it directly.
    Api,
}

impl RunSource {
    /// The literal stored in the `source` column.
    #[must_use]
    pub const fn as_db_str(self) -> &'static str {
        match self {
            Self::Schedule => "schedule",
            Self::Manual => "manual",
            Self::Webhook => "webhook",
            Self::Api => "api",
        }
    }

    /// Parse a stored `source` value. An unrecognised string falls back to
    /// [`Schedule`](Self::Schedule) — the column default, and the same tolerant
    /// shape as [`ConcurrencyPolicy::from_db_str`]; the column `CHECK` already
    /// rejects junk on write.
    #[must_use]
    pub fn from_db_str(s: &str) -> Self {
        match s {
            "manual" => Self::Manual,
            "webhook" => Self::Webhook,
            "api" => Self::Api,
            _ => Self::Schedule,
        }
    }
}

/// What [`dispatch_with_admission`] did with a dispatch request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DispatchOutcome {
    /// The dispatch was admitted: a run + task exist.
    Fired {
        /// The new run.
        run_id: AutopilotRunId,
        /// The task enqueued against it.
        task_id: TaskId,
        /// How many in-flight runs the `replace` policy superseded first (`0`
        /// for every other policy).
        superseded: u64,
    },
    /// The admission gate declined the dispatch. A TERMINAL `skipped` run row
    /// records it; no task was enqueued.
    Skipped {
        /// The recorded `skipped` run.
        run_id: AutopilotRunId,
        /// Why it was declined (persisted as `failure_reason`).
        reason: String,
        /// The in-flight count that tripped the limit.
        in_flight: i64,
    },
}

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
pub async fn fire_autopilot_tick(
    pool: &SqlitePool,
    clock: &dyn HangarClock,
    autopilot: &Autopilot,
) -> Result<(AutopilotRunId, TaskId), FireError> {
    fire_autopilot_tick_with_source(pool, clock, autopilot, RunSource::Schedule).await
}

/// Fire one autopilot tick, stamping WHICH trigger fired it onto the run
/// (`autopilot_run.source`, migration 0057).
///
/// This holds the real fire body; [`fire_autopilot_tick`] is the legacy
/// `schedule`-sourced delegate. Every production caller should name its source
/// explicitly so the run history carries honest provenance.
///
/// Returns the new `(autopilot_run.id, agent_task_queue.id)` on commit.
///
/// # Errors
///
/// Same surface as [`fire_autopilot_tick`].
pub async fn fire_autopilot_tick_with_source(
    pool: &SqlitePool,
    clock: &dyn HangarClock,
    autopilot: &Autopilot,
    source: RunSource,
) -> Result<(AutopilotRunId, TaskId), FireError> {
    fire_autopilot_tick_with_attribution(pool, clock, autopilot, source, &RunAttribution::RuleOwner)
        .await
}

/// Fire one autopilot tick, stamping both WHICH trigger fired it
/// (`autopilot_run.source`) and WHO is accountable for it
/// (`autopilot_run.accountable_actor` / `attribution`, migration 0061).
///
/// This holds the real fire body;
/// [`fire_autopilot_tick_with_source`] is the delegate that attributes every
/// unattended fire to [`RunAttribution::RuleOwner`], and
/// [`fire_autopilot_tick`] is the legacy `schedule`-sourced delegate on top of
/// that.
///
/// The accountable actor is resolved INSIDE the fire transaction (from the
/// newest rule version for `RuleOwner`, from the supplied ref for
/// `DirectHuman`), so a run can never disagree with the ledger its own commit
/// observed.
///
/// # Errors
///
/// Same surface as [`fire_autopilot_tick`].
#[tracing::instrument(
    name = "autopilot.tick",
    skip(pool, clock, autopilot, attribution),
    fields(
        autopilot_id = %autopilot.id,
        cron_expr = %autopilot.cron_expr,
        source = source.as_db_str(),
        attribution = attribution.as_db_str(),
    )
)]
pub async fn fire_autopilot_tick_with_attribution(
    pool: &SqlitePool,
    clock: &dyn HangarClock,
    autopilot: &Autopilot,
    source: RunSource,
    attribution: &RunAttribution,
) -> Result<(AutopilotRunId, TaskId), FireError> {
    let now = clock.now_ms();
    let run_id = SystemIdGen.new_ulid();
    let task_id = SystemIdGen.new_ulid();

    let mut tx = pool.begin().await?;

    // 0. Who is accountable for THIS run, resolved in the same transaction.
    let (accountable_actor, attribution_token) =
        attribution.resolve_in_tx(&mut tx, &autopilot.id).await?;

    // 1. The run row, in-flight.
    sqlx::query(
        "INSERT INTO autopilot_run \
         (id, autopilot_id, started_at, status, source, accountable_actor, attribution) \
         VALUES (?, ?, ?, 'running', ?, ?, ?)",
    )
    .bind(&run_id)
    .bind(&autopilot.id)
    .bind(now)
    .bind(source.as_db_str())
    .bind(&accountable_actor)
    .bind(attribution_token)
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
            // ORIGIN PROVENANCE (migration 0056, multica parity #21): stamped
            // INSIDE this INSERT — the exact analogue of multica's
            // `CreateIssueWithOrigin` (`service/autopilot.go:129-146`), which
            // binds `ap.ID` (the RULE, not the run) so the completion handler
            // and analytics can resolve the issue by provenance.
            sqlx::query(
                "INSERT INTO issue \
                 (id, workspace_id, title, description, state, \
                  creator_type, creator_id, created_at, origin_type, origin_id) \
                 VALUES (?, ?, ?, ?, 'open', 'agent', ?, ?, 'autopilot', ?)",
            )
            .bind(&issue_id)
            .bind(&autopilot.workspace_id)
            .bind(&title)
            .bind(&autopilot.instructions)
            .bind(&autopilot.agent_id)
            .bind(now)
            .bind(&autopilot.id)
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

    // 5. Stamp the task's ORIGIN PROVENANCE inside the SAME transaction, so the
    //    claim loop can never observe a fired task missing the provenance the
    //    dispatcher hands its agent child. `origin_id` is the AUTOPILOT id, not
    //    the run id (multica `service/autopilot.go:145`).
    TaskRepo::set_origin_in_tx(
        &mut tx,
        &task_id,
        &IssueOrigin::autopilot(&autopilot.id).map_err(|e| {
            FireError::Db(sqlx::Error::Protocol(format!(
                "autopilot {} has an unusable id for origin provenance: {e}",
                autopilot.id
            )))
        })?,
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

/// Count an autopilot's in-flight (not-yet-completed) runs — the
/// concurrency-policy denominator.
///
/// A `skipped` run is terminal (it stamps `completed_at`), so declined
/// dispatches never inflate this count.
///
/// # Errors
///
/// Returns [`FireError::Db`] on a SQL failure.
pub async fn count_in_flight(pool: &SqlitePool, autopilot_id: &str) -> Result<i64, FireError> {
    let n = sqlx::query_scalar::<_, i64>(
        "SELECT count(*) FROM autopilot_run \
         WHERE autopilot_id = ? AND completed_at IS NULL",
    )
    .bind(autopilot_id)
    .fetch_one(pool)
    .await?;
    Ok(n)
}

/// Record a dispatch the admission gate intentionally DECLINED, as a terminal
/// `skipped` run row (migration 0057).
///
/// The direct analogue of multica's `recordSkippedRun`
/// (`service/autopilot.go:389-435`): persist a run carrying `status='skipped'`,
/// the originating `source`, and `failure_reason = <reason>` — no task, no
/// issue — so a declined dispatch is visible to every read path instead of
/// existing only as a log line. `skipped` is deliberately NOT `failed`: reusing
/// `failed` would pollute the failure-rate signal.
///
/// `started_at` and `completed_at` are BOTH stamped with `clock.now_ms()`. That
/// is load-bearing, not cosmetic: [`count_in_flight`] counts
/// `completed_at IS NULL`, so a skip left open would permanently inflate the
/// in-flight count and wedge the autopilot at its limit forever.
///
/// # Errors
///
/// Returns [`FireError::Db`] on a SQL failure (e.g. an `autopilot_id` FK
/// violation), or [`FireError::EmptyId`] on the unreachable empty-PK invariant
/// break.
pub async fn record_skipped_run(
    pool: &SqlitePool,
    clock: &dyn HangarClock,
    autopilot: &Autopilot,
    source: RunSource,
    reason: &str,
) -> Result<AutopilotRunId, FireError> {
    record_skipped_run_with_attribution(
        pool,
        clock,
        autopilot,
        source,
        reason,
        &RunAttribution::RuleOwner,
    )
    .await
}

/// Record a declined dispatch, attributed to its accountable human
/// (migration 0061).
///
/// A **skipped** run is still an accountable event — a dispatch someone's rule
/// asked for and the gate declined — so it carries the identical
/// `accountable_actor` / `attribution` pair a fired run would.
/// [`record_skipped_run`] is the delegate that attributes to
/// [`RunAttribution::RuleOwner`].
///
/// # Errors
///
/// Same surface as [`record_skipped_run`].
pub async fn record_skipped_run_with_attribution(
    pool: &SqlitePool,
    clock: &dyn HangarClock,
    autopilot: &Autopilot,
    source: RunSource,
    reason: &str,
    attribution: &RunAttribution,
) -> Result<AutopilotRunId, FireError> {
    let now = clock.now_ms();
    let run_id = SystemIdGen.new_ulid();
    let mut tx = pool.begin().await?;
    let (accountable_actor, attribution_token) =
        attribution.resolve_in_tx(&mut tx, &autopilot.id).await?;
    sqlx::query(
        "INSERT INTO autopilot_run \
         (id, autopilot_id, started_at, completed_at, status, source, failure_reason, \
          accountable_actor, attribution) \
         VALUES (?, ?, ?, ?, 'skipped', ?, ?, ?, ?)",
    )
    .bind(&run_id)
    .bind(&autopilot.id)
    .bind(now)
    .bind(now)
    .bind(source.as_db_str())
    .bind(reason)
    .bind(&accountable_actor)
    .bind(attribution_token)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(AutopilotRunId::from_str(run_id)?)
}

/// The SHARED admission gate: apply the autopilot's concurrency policy, then
/// either fire or record a `skipped` run.
///
/// This is the deep module behind a shallow interface every trigger surface
/// (cron scheduler, manual fire-now, webhook ingress, the `api` trigger) calls,
/// so no path can drift from the tested policy. It is the exact analogue of
/// multica running its admission gate on EVERY dispatch whatever the source
/// (`service/autopilot.go:51-53`).
///
/// The policy branch is lifted verbatim from the scheduler's `fire_or_skip`,
/// minus the event emission and the reschedule (both remain the scheduler's
/// job):
///
/// - under the limit → fire (`superseded: 0`), regardless of policy;
/// - at the limit + [`ConcurrencyPolicy::Skip`] → record a terminal `skipped`
///   run and enqueue NOTHING;
/// - at the limit + [`ConcurrencyPolicy::Queue`] → fire anyway (the claim queue
///   serialises the work);
/// - at the limit + [`ConcurrencyPolicy::Replace`] → [`supersede_in_flight`]
///   first, then fire, reporting how many runs were superseded.
///
/// The only behavioural change versus the pre-0057 scheduler is that the `Skip`
/// branch now WRITES a row; it still enqueues no work.
///
/// # Errors
///
/// - [`FireError::Db`] on any SQL failure (the in-flight count, the skip insert,
///   the supersede, or the fire transaction).
/// - [`FireError::AgentNotFound`] when the fire path cannot resolve the agent.
/// - [`FireError::EmptyId`] on the unreachable empty-PK invariant break.
pub async fn dispatch_with_admission(
    pool: &SqlitePool,
    clock: &dyn HangarClock,
    autopilot: &Autopilot,
    source: RunSource,
) -> Result<DispatchOutcome, FireError> {
    dispatch_with_admission_as(pool, clock, autopilot, source, &RunAttribution::RuleOwner).await
}

/// The shared admission gate, carrying the run's accountable human
/// (migration 0061).
///
/// Holds the real body; [`dispatch_with_admission`] is the delegate that
/// attributes every dispatch to [`RunAttribution::RuleOwner`] — which is what
/// the scheduler, the webhook ingress and the `api` trigger all want, since
/// those are unattended fires. Only a human-driven "run now" passes
/// [`RunAttribution::DirectHuman`].
///
/// BOTH branches attribute identically: a `skipped` run is an accountable event
/// too, not a fire-only concern.
///
/// # Errors
///
/// Same surface as [`dispatch_with_admission`].
pub async fn dispatch_with_admission_as(
    pool: &SqlitePool,
    clock: &dyn HangarClock,
    autopilot: &Autopilot,
    source: RunSource,
    attribution: &RunAttribution,
) -> Result<DispatchOutcome, FireError> {
    let in_flight = count_in_flight(pool, &autopilot.id).await?;
    let at_limit = in_flight >= autopilot.max_concurrent_runs;

    // The concurrency policy only matters AT the limit. Under the limit, every
    // policy simply fires.
    let superseded = match (at_limit, autopilot.concurrency_policy) {
        (false, _) | (true, ConcurrencyPolicy::Queue) => 0,
        (true, ConcurrencyPolicy::Skip) => {
            let reason = format!(
                "concurrency limit: {in_flight}/{} in flight",
                autopilot.max_concurrent_runs
            );
            let run_id = record_skipped_run_with_attribution(
                pool,
                clock,
                autopilot,
                source,
                &reason,
                attribution,
            )
            .await?;
            return Ok(DispatchOutcome::Skipped {
                run_id,
                reason,
                in_flight,
            });
        }
        (true, ConcurrencyPolicy::Replace) => {
            supersede_in_flight(pool, clock, &autopilot.id).await?
        }
    };

    let (run_id, task_id) =
        fire_autopilot_tick_with_attribution(pool, clock, autopilot, source, attribution).await?;
    Ok(DispatchOutcome::Fired {
        run_id,
        task_id,
        superseded,
    })
}
