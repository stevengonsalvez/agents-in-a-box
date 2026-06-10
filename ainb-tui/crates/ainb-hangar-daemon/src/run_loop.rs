//! The daemon's claim loop + sweeper scheduler (P1.7).
//!
//! [`run`] is the daemon's steady-state body: it spawns the four periodic
//! sweepers (P1.4) and then polls [`ClaimTaskService`] for the oldest `queued`
//! task bound to this daemon's runtime. Each claimed task walks the FSM —
//! `dispatched -> running` ([`StartTaskService`]), provider exec
//! ([`Runner::run_claude`]), then `running -> done` ([`CompleteTaskService`]) or
//! `running -> failed` ([`FailTaskService`]) — inside its isolated
//! [`ExecEnv`](crate::execenv::ExecEnv).
//!
//! # Configuration
//!
//! [`DaemonConfig::from_env`] reads the daemon's identity and tunables from the
//! environment so a spawned binary (the P1.7 tripwire) is fully controllable
//! without a config file:
//!
//! | Env var | Meaning | Default |
//! |---|---|---|
//! | `HANGAR_DAEMON_RUNTIME_ID` | runtime this daemon claims for (**required** to claim) | — |
//! | `HANGAR_CLAUDE_PATH` | `claude` provider binary path | `claude` (resolved on `PATH`) |
//! | `HANGAR_CODEX_PATH` | `codex` provider binary path (e38.16) | `codex` (resolved on `PATH`) |
//! | `HANGAR_DAEMON_POLL_MS` | claim-poll interval | `1000` |
//! | `HANGAR_SWEEP_INTERVAL_MS` | sweep-pass interval | `60000` |
//! | `HANGAR_PROVIDER_MAX_RUNTIME_MS` | provider runtime deadline override (tests) | Multica running TTL (2.5h) |
//! | `HANGAR_SWEEP_DISPATCHED_TTL_MS` | dispatch TTL override (tests) | Multica default |
//! | `HANGAR_DAEMON_DISABLE_CLAIM` | skip the claim loop, run sweepers only (tests) | unset |
//! | `HANGAR_DAEMON_DISABLE_SANDBOX` | set to `1` to run providers UNCONFINED (security downgrade) | unset (sandbox ON) |
//!
//! When `HANGAR_DAEMON_RUNTIME_ID` is unset the claim loop is a no-op (the daemon
//! still sweeps) — a daemon with no runtime has nothing to claim.

// The provider runtime deadline is a "hours" quantity but `Duration::from_hours`
// is unstable; a raw second count is the clearest stable spelling (and matches
// the Multica running-TTL value). Same rationale as `sweeper.rs`.
#![allow(clippy::duration_suboptimal_units)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use ainb_hangar_core::clock::{HangarClock, SystemClock};
use ainb_hangar_core::idgen::{IdGen, SystemIdGen};
use ainb_hangar_store::repo::task::{Task, TaskRepo};
use ainb_hangar_store::service::claim::{ClaimTaskService, ClaimedTask};
use ainb_hangar_store::service::complete::{CompleteParams, CompleteTaskService};
use ainb_hangar_store::service::fail::FailTaskService;
use ainb_hangar_store::service::retry::{RetryDecision, RetryService};
use ainb_hangar_store::service::start::StartTaskService;
use sqlx::{Row, SqlitePool};

use crate::events::EventSink;
use crate::execenv::prepare_env;
use crate::health_stats::HealthStats;
use crate::runner::{Backend, ProviderInvocation, RunOutcome, Runner, RunnerConfig};
use crate::sweeper::{
    SweeperConfig, reclaim_orphaned_on_startup, sweep_expired_queued, sweep_stale_dispatched,
    sweep_stale_running,
};

/// Default claim-poll interval when `HANGAR_DAEMON_POLL_MS` is unset.
const DEFAULT_POLL_MS: u64 = 1_000;
/// Provider runtime deadline (Multica running TTL: 2.5h).
const PROVIDER_MAX_RUNTIME: Duration = Duration::from_secs(9_000);
/// Trailing stdout/stderr lines retained on each run for the audit tail.
const TAIL_LINES: usize = 200;

/// Resolved daemon configuration (identity + tunables).
#[derive(Debug, Clone)]
pub struct DaemonConfig {
    /// Runtime id this daemon claims tasks for, or `None` (claim disabled).
    pub runtime_id: Option<String>,
    /// Path to the `claude` provider binary.
    pub claude_path: PathBuf,
    /// Path to the `codex` provider binary (e38.16).
    pub codex_path: PathBuf,
    /// Interval between claim polls.
    pub poll_interval: Duration,
    /// Hard wall-clock deadline for each provider run; the subprocess is killed
    /// past it ([`FailureReason::Timeout`]). Defaults to the Multica running TTL
    /// (2.5h); overridable via `HANGAR_PROVIDER_MAX_RUNTIME_MS` so an e2e test can
    /// drive the timeout-kill path within a bounded budget.
    pub provider_max_runtime: Duration,
    /// Sweeper thresholds + cadence.
    pub sweeper: SweeperConfig,
    /// When `true`, skip the claim loop entirely (sweepers still run). Used by
    /// the stale-dispatch sweeper tripwire to seed a `dispatched` row and prove
    /// the sweeper fails it without the loop racing to start it.
    pub disable_claim: bool,
    /// e38.23: confine every provider subprocess in the OS-level FS sandbox.
    /// **Default ON**; set `HANGAR_DAEMON_DISABLE_SANDBOX=1` to opt out (the
    /// override seam — e.g. a debug build on a platform whose sandbox primitive
    /// misbehaves). Disabling it is a security downgrade and is logged.
    pub sandbox: bool,
}

impl DaemonConfig {
    /// Build the config from the process environment (see the module table).
    #[must_use]
    pub fn from_env() -> Self {
        let runtime_id = std::env::var("HANGAR_DAEMON_RUNTIME_ID").ok().filter(|s| !s.is_empty());
        let claude_path = std::env::var_os("HANGAR_CLAUDE_PATH")
            .map_or_else(|| PathBuf::from("claude"), PathBuf::from);
        let codex_path = std::env::var_os("HANGAR_CODEX_PATH")
            .map_or_else(|| PathBuf::from("codex"), PathBuf::from);
        let poll_interval =
            Duration::from_millis(env_u64("HANGAR_DAEMON_POLL_MS", DEFAULT_POLL_MS));
        let provider_max_runtime = env_u64_opt("HANGAR_PROVIDER_MAX_RUNTIME_MS")
            .map_or(PROVIDER_MAX_RUNTIME, Duration::from_millis);

        let mut sweeper = SweeperConfig::default();
        if let Some(ms) = env_u64_opt("HANGAR_SWEEP_INTERVAL_MS") {
            sweeper.sweep_interval = Duration::from_millis(ms);
        }
        if let Some(ms) = env_u64_opt("HANGAR_SWEEP_DISPATCHED_TTL_MS") {
            sweeper.dispatched_ttl = Duration::from_millis(ms);
            // Keep the reclaim window strictly below the (now tiny) TTL so a
            // stale dispatch fails rather than being perpetually reclaimed.
            sweeper.reclaim_window = sweeper.reclaim_window.min(sweeper.dispatched_ttl / 2);
        }
        let disable_claim = std::env::var_os("HANGAR_DAEMON_DISABLE_CLAIM").is_some();
        // e38.23: sandbox is ON by default; the env var is an explicit opt-out.
        let sandbox = std::env::var_os("HANGAR_DAEMON_DISABLE_SANDBOX").is_none_or(|v| v != "1");

        Self {
            runtime_id,
            claude_path,
            codex_path,
            poll_interval,
            provider_max_runtime,
            sweeper,
            disable_claim,
            sandbox,
        }
    }
}

/// Parse an env var as `u64`, falling back to `default` when unset/invalid.
fn env_u64(key: &str, default: u64) -> u64 {
    env_u64_opt(key).unwrap_or(default)
}

/// Parse an env var as `u64`, returning `None` when unset or unparseable.
fn env_u64_opt(key: &str) -> Option<u64> {
    std::env::var(key).ok().and_then(|s| s.parse().ok())
}

/// Run the daemon's steady state: spawn sweepers, then poll-claim-execute until
/// `Ctrl-C`.
///
/// `stats` is the shared in-memory health collector (P8.5): each task's terminal
/// outcome is recorded into its rolling throughput ring as the FSM finalises, so
/// the `hangar/daemon_health` pane sees live per-second completed / failed
/// counts. The collector is shared with the RPC server (which snapshots it).
///
/// `events` is the daemon's event sink (e38.2): each FSM step the loop drives —
/// `dispatched -> running` and the terminal finalize — publishes its typed
/// [`HangarEvent`](ainb_hangar_proto::events::HangarEvent) so subscribed
/// plugins see lifecycle changes without re-pulling snapshots.
///
/// # Errors
///
/// Returns an error if installing the shutdown handler fails. Per-task failures
/// (provider errors, FSM races) are logged and recorded on the row, never
/// propagated — one bad task must not down the daemon.
pub async fn run(
    pool: SqlitePool,
    cfg: DaemonConfig,
    stats: Arc<HealthStats>,
    events: EventSink,
) -> anyhow::Result<()> {
    spawn_sweepers(pool.clone(), cfg.sweeper);

    if cfg.disable_claim || cfg.runtime_id.is_none() {
        tracing::info!(claim = false, "claim loop disabled; sweepers only");
        tokio::signal::ctrl_c().await?;
        tracing::info!("ainb-hangar-daemon shutting down");
        return Ok(());
    }

    let runtime_id = cfg.runtime_id.clone().expect("runtime_id present");

    // e38.25 crash recovery: this daemon just booted, so any task still frozen
    // `dispatched`/`running` for its runtime is an orphan from a previous
    // (crashed/restarted) instance — the process that owned those runs is gone.
    // Reclaim them to `queued` once, up front, so the work is re-dispatched
    // immediately rather than stranded until the multi-hour running TTL. Scoped
    // to this runtime so a sibling daemon's live runs are never touched. A
    // reclaim fault is non-fatal — the time-based sweepers still backstop it.
    match reclaim_orphaned_on_startup(&pool, &runtime_id).await {
        Ok(n) if n > 0 => tracing::info!(reclaimed = n, "startup crash-recovery reclaim"),
        Ok(_) => {}
        Err(e) => tracing::error!(error = %e, "startup crash-recovery reclaim failed"),
    }

    let runner = Runner::new(RunnerConfig {
        claude_path: cfg.claude_path.clone(),
        codex_path: cfg.codex_path.clone(),
        max_runtime: cfg.provider_max_runtime,
        tail_lines: TAIL_LINES,
        // e38.23: confine every provider spawn in the OS-level FS sandbox by
        // default. Overridable via `HANGAR_DAEMON_DISABLE_SANDBOX=1` (see
        // `DaemonConfig::from_env`).
        sandbox: cfg.sandbox,
    });
    let clock = SystemClock;

    loop {
        tokio::select! {
            biased;
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("ainb-hangar-daemon shutting down");
                return Ok(());
            }
            () = tokio::time::sleep(cfg.poll_interval) => {}
        }

        match ClaimTaskService::claim_for_runtime(&pool, &runtime_id, &clock).await {
            Ok(Some(claimed)) => {
                if let Err(e) =
                    execute_claimed(&pool, &runner, &claimed, &clock, &stats, &events).await
                {
                    tracing::error!(task_id = %claimed.id, error = %e, "task execution errored");
                }
            }
            Ok(None) => {} // empty queue — poll again
            Err(e) => tracing::error!(error = %e, "claim query failed"),
        }
    }
}

/// Spawn the three lifecycle sweepers as periodic background tasks.
///
/// The dispatched sweep runs at twice the configured interval (reclaim is
/// time-sensitive); queued + running sweep at the base interval. Each tick is
/// independent and idempotent, so a missed/overlapping tick is harmless.
fn spawn_sweepers(pool: SqlitePool, cfg: SweeperConfig) {
    let interval = cfg.sweep_interval;
    let dispatched_interval = (interval / 2).max(Duration::from_millis(1));

    {
        let pool = pool.clone();
        tokio::spawn(async move {
            let clock = SystemClock;
            let mut tick = tokio::time::interval(interval);
            loop {
                tick.tick().await;
                if let Err(e) = sweep_expired_queued(&pool, &clock, &cfg).await {
                    tracing::error!(error = %e, kind = "queued", "sweeper pass failed");
                }
                if let Err(e) = sweep_stale_running(&pool, &clock, &cfg).await {
                    tracing::error!(error = %e, kind = "running", "sweeper pass failed");
                }
            }
        });
    }
    tokio::spawn(async move {
        let clock = SystemClock;
        let mut tick = tokio::time::interval(dispatched_interval);
        loop {
            tick.tick().await;
            match sweep_stale_dispatched(&pool, &clock, &cfg).await {
                Ok(outcome) if outcome.failed > 0 => {
                    tracing::info!(failed = outcome.failed, "sweeper_stale_dispatched");
                }
                Ok(_) => {}
                Err(e) => tracing::error!(error = %e, kind = "dispatched", "sweeper pass failed"),
            }
        }
    });
}

/// Walk one claimed task through `dispatched -> running -> done|failed`.
///
/// Re-reads the full row (for `workspace_id` / `issue_id`), resolves the
/// workspace slug, prepares the isolated env, marks the task `running`, runs the
/// provider, and finalises the row from the [`RunOutcome`].
///
/// # Errors
///
/// Returns an error only for an unrecoverable I/O / DB fault while *setting up*
/// the run (missing row, slug lookup, env prep, start). A provider failure is a
/// normal FSM outcome (`fail`), not an error here.
async fn execute_claimed(
    pool: &SqlitePool,
    runner: &Runner,
    claimed: &ClaimedTask,
    clock: &dyn HangarClock,
    stats: &HealthStats,
    events: &EventSink,
) -> anyhow::Result<()> {
    let task: Task = TaskRepo::get_by_id(pool, &claimed.id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("claimed task {} vanished", claimed.id))?;
    let ws_slug = workspace_slug(pool, &task.workspace_id).await?;
    let home = hangar_home();
    let env = prepare_env(&task, &ws_slug, &home, clock)?;

    // e38.16: resolve which provider exec path this task routes to (agent →
    // runtime → provider) and the per-agent config (model / cli_args / agent_env)
    // the runner threads into that provider's argv + env. A resolve fault
    // defaults the dispatch to `claude` with empty config so a misconfigured
    // agent still runs rather than stranding the task.
    let dispatch = resolve_dispatch(pool, &task.agent_id).await.unwrap_or_default();

    // dispatched -> running. A lost race (another worker started it) surfaces as
    // an FSM error; bail without running a duplicate provider.
    StartTaskService::start(pool, &task.id, clock).await?;
    tracing::info!(task_id = %task.id, "task running");
    // e38.2: announce the start to subscribed plugins (best-effort push; the
    // next snapshot pull reconciles if no subscriber is connected).
    emit_task_started(events, &task, clock);

    // Snapshot the daemon's env into an owned map *before* the await: the
    // `std::env::Vars` iterator is `!Send`, so holding it across `.await` would
    // make this future non-`Send` and unusable on the multi-thread runtime.
    //
    // P5.3: apply the configurable env-allowlist policy here (the authoritative
    // pass), layering keychain-resident API keys on top via
    // `dispatch::build_task_env`. The policy is loaded from
    // `~/.ainb/hangar/env.allow.toml` (operator defaults when absent). The
    // keychain key list is empty until P5.2 wires `host/secret_store_get` into
    // dispatch; the seam is in place so adding keys is a one-line change. The
    // runner re-applies its own deny-by-default 12-var filter as
    // defense-in-depth (a strict subset of this policy).
    let daemon_env: std::collections::HashMap<String, String> = std::env::vars().collect();
    let policy = load_env_policy();
    let keychain_keys: Vec<(String, String)> = Vec::new();
    let mut task_env = crate::dispatch::build_task_env(&daemon_env, keychain_keys, &policy);

    // P6.4: materialise the agent's attached skills into the provider's layout
    // before spawning. Home-style providers (claude/codex/cursor) land their
    // skills under the *task root* (sibling of `workdir`, so the git worktree
    // stays clean) and are pointed there via a `*_HOME` env var, which is
    // folded into `task_env` here so the runner forwards it. A materialisation
    // fault is non-fatal — a task must still dispatch even if a skill bundle
    // cannot be written (the agent simply runs without its skills).
    if let Some((key, path)) = materialise_skills(pool, &task, &env).await {
        task_env.insert(key, path.to_string_lossy().into_owned());
    }

    // P5.6: warn about `danger-full-access` on the first invocation of this
    // provider in this session. The decision + ack persistence are authoritative
    // here (a task may be dispatched from the CLI with no TUI attached); a
    // re-dispatch in the same session is suppressed, a fresh session re-warns.
    // The "session" is the resumed provider session id when present, else the
    // task id (a fresh run is a fresh warning surface). A warning-IO fault is
    // non-fatal — never block a dispatch on it. e38.16: keyed on the resolved
    // backend, so a codex task warns under `codex` rather than always `claude`.
    warn_danger_access(&task, dispatch.backend.name());

    // e38.16: route to the resolved provider's exec path. `claude` takes the
    // allowlist-filtered env and no argv; `codex` runs `codex exec` with the
    // agent's model/cli_args on the argv and its `agent_env` layered onto the
    // child env. Both spawn through the same OS sandbox (e38.23).
    let outcome = match dispatch.backend {
        Backend::Claude => runner.run_claude(&env, task_env).await?,
        Backend::Codex => {
            runner
                .run_codex_with_env(&env, task_env, dispatch.agent_env, &dispatch.invocation)
                .await?
        }
    };

    match outcome {
        RunOutcome::Success(result) => {
            // P9.1: v1 gh-CLI-only PR capture. If the agent shelled out to
            // `gh pr create` inside its worktree, its printed PR-URL line lands
            // in the runner's bounded stdout tail (the per-task ring buffer,
            // cap `TAIL_LINES`, oldest-line-evicting — read concurrently in the
            // runner so it never blocks the claim loop). Scan it for the
            // canonical PR URL (last one wins on a multi-PR run) and fold it
            // into the structured result. A no-PR run yields `None`, which the
            // `TaskResult` serializer omits entirely — so the `result` JSON is
            // byte-identical to the pre-P9 shape and `pr_url` is NULL (no key),
            // never `""`.
            let pr_url = ainb_hangar_core::pr_url::parse_gh_pr_create_stdout(&result.stdout_tail);
            if let Some(url) = pr_url.as_deref() {
                tracing::info!(task_id = %task.id, pr_url = url, "captured gh pr url");
            }
            let task_result = ainb_hangar_core::result::TaskResult::new(
                result.stdout_tail,
                result.exit_code,
                pr_url,
            );
            let result_json = serde_json::to_value(&task_result)
                .unwrap_or_else(|_| serde_json::json!({"content": ""}));
            CompleteTaskService::complete(
                pool,
                &task.id,
                CompleteParams {
                    result: result_json,
                    session_id: result.session_id,
                    work_dir: env.workdir.to_str().map(str::to_string),
                },
                clock,
            )
            .await?;
            // P8.5: record the successful terminal outcome into the rolling
            // throughput ring so the daemon-health pane's sparkline sees it.
            stats.record_completed(clock.now_ms() / 1_000);
            // e38.2: push the terminal transition to subscribed plugins.
            emit_task_finished(
                events,
                &task,
                ainb_hangar_proto::events::TaskResult::Success,
                clock,
            );
            tracing::info!(task_id = %task.id, "task done");
        }
        RunOutcome::Failed { reason, result } => {
            // Persist the session id (if any) before failing so a retry can
            // resume the provider conversation.
            persist_session_id(pool, &task.id, result.session_id.as_deref()).await?;
            FailTaskService::fail(pool, &task.id, reason, clock).await?;
            // P8.5: record the failed terminal outcome (drives the sparkline's
            // red proportion for this second).
            stats.record_failed(clock.now_ms() / 1_000);
            // e38.2: push the terminal transition to subscribed plugins.
            emit_task_finished(
                events,
                &task,
                ainb_hangar_proto::events::TaskResult::Failure,
                clock,
            );
            tracing::warn!(task_id = %task.id, reason = reason.as_db_str(), "task failed");
            // F06 retry chain: a retryable (infra) failure with attempts
            // remaining spawns a fresh `queued` child carrying `parent_task_id`,
            // which the next claim pass re-dispatches. A terminal reason
            // (`agent_error` / `user_cancel` / `timeout`) or an exhausted
            // `max_attempts` is a no-op. The retry insert can collide with the
            // per-issue pending-unique index — that is a benign already-pending
            // outcome (a sibling holds the slot), logged not propagated, so one
            // failed retry never downs the claim loop.
            maybe_spawn_retry(pool, &task.id, clock).await;
        }
    }
    Ok(())
}

/// Evaluate the just-failed `task_id` for an automatic retry and, if eligible,
/// spawn a `parent_task_id`-chained child row (F06).
///
/// Re-reads the failed row (so `attempt` / `max_attempts` / `failure_reason`
/// reflect the fail that just committed), mints a fresh child id via the
/// production [`SystemIdGen`], and delegates the eligibility + atomic child
/// INSERT to [`RetryService::maybe_retry_failed`]. Every fault here is
/// best-effort and swallowed (logged): a missing row, a DB read error, or the
/// per-issue pending-unique collision must never propagate out of the claim
/// loop and down the daemon — the failed parent already committed for audit.
async fn maybe_spawn_retry(pool: &SqlitePool, task_id: &str, clock: &dyn HangarClock) {
    let failed = match TaskRepo::get_by_id(pool, task_id).await {
        Ok(Some(t)) => t,
        Ok(None) => return,
        Err(e) => {
            tracing::error!(task_id = %task_id, error = %e, "retry: re-read failed task errored");
            return;
        }
    };
    let new_id = SystemIdGen.new_ulid();
    match RetryService::maybe_retry_failed(pool, &failed, &new_id, clock).await {
        Ok(RetryDecision::Spawned { new_task_id }) => {
            tracing::info!(parent_task_id = %task_id, child_task_id = %new_task_id, "task retry spawned");
        }
        Ok(RetryDecision::DoNotRetry) => {}
        Err(e) => {
            // The single atomic insert can collide with the per-(issue, agent)
            // pending-unique index when another pending task already holds the
            // slot — a benign "already pending" outcome, not a daemon fault.
            tracing::warn!(parent_task_id = %task_id, error = %e, "retry child insert failed (likely already-pending slot); skipping");
        }
    }
}

/// Publish a [`TaskStarted`](ainb_hangar_proto::events::HangarEvent::TaskStarted)
/// for `task`, scoped to its owning workspace (e38.2). Best-effort: a
/// malformed id or out-of-range clock only skips the push — the FSM write has
/// already committed and the next snapshot pull reconciles.
fn emit_task_started(events: &EventSink, task: &Task, clock: &dyn HangarClock) {
    let Ok(task_id) = ainb_hangar_core::ids::TaskId::from_str(task.id.clone()) else {
        return;
    };
    let Some(started_at) = chrono::DateTime::from_timestamp_millis(clock.now_ms()) else {
        return;
    };
    events.emit(
        &task.workspace_id,
        ainb_hangar_proto::events::HangarEvent::TaskStarted {
            task_id,
            started_at,
        },
    );
}

/// Publish a [`TaskFinished`](ainb_hangar_proto::events::HangarEvent::TaskFinished)
/// with `result` for `task`, scoped to its owning workspace (e38.2).
/// Best-effort, mirroring [`emit_task_started`].
fn emit_task_finished(
    events: &EventSink,
    task: &Task,
    result: ainb_hangar_proto::events::TaskResult,
    clock: &dyn HangarClock,
) {
    let Ok(task_id) = ainb_hangar_core::ids::TaskId::from_str(task.id.clone()) else {
        return;
    };
    let Some(ended_at) = chrono::DateTime::from_timestamp_millis(clock.now_ms()) else {
        return;
    };
    events.emit(
        &task.workspace_id,
        ainb_hangar_proto::events::HangarEvent::TaskFinished {
            task_id,
            result,
            ended_at,
        },
    );
}

/// Materialise the claimed task's agent skills into the provider layout, after
/// the per-task env exists and before the provider spawns (P6.4).
///
/// Resolves the provider from the task's agent → its runtime, then copies every
/// attached skill bundle to disk via [`crate::materialise::materialise_for_agent`].
/// Returns the `*_HOME` env var (name, path) the runner must forward so a
/// home-style provider (claude/codex/cursor) discovers the skills under the task
/// root, or `None` when there is no env pointer (no skills, or an in-workdir
/// provider).
///
/// Best-effort: any resolution or IO fault is logged and swallowed (`None`) — a
/// task must still dispatch even if its skills cannot be materialised.
async fn materialise_skills(
    pool: &SqlitePool,
    task: &Task,
    env: &crate::execenv::ExecEnv,
) -> Option<(String, PathBuf)> {
    // Resolve provider AND the agent's owning workspace together (both read the
    // agent row): the workspace scopes the skill materialise so a task can only
    // ever materialise its own tenant's skills.
    let (provider, workspace) = match resolve_provider_and_workspace(pool, &task.agent_id).await {
        Ok(pw) => pw,
        Err(e) => {
            tracing::warn!(error = %e, task_id = %task.id, "skill materialise: agent resolve failed; skipping");
            return None;
        }
    };
    let Ok(agent) = ainb_hangar_core::ids::AgentId::from_str(task.agent_id.clone()) else {
        return None;
    };
    let target = crate::materialise::MaterialiseTarget {
        workspace,
        task_root: env.root().to_path_buf(),
        workdir: env.workdir.clone(),
        provider,
    };
    match crate::materialise::materialise_for_agent(pool, &agent, &target).await {
        Ok(report) => {
            if report.files_written > 0 {
                tracing::info!(
                    task_id = %task.id,
                    files = report.files_written,
                    bytes = report.total_bytes,
                    skills = ?report.skill_names,
                    "skills materialised"
                );
            }
            report.home_env
        }
        Err(e) => {
            tracing::warn!(error = %e, task_id = %task.id, "skill materialise failed; proceeding without skills");
            None
        }
    }
}

/// The resolved provider routing for one task (e38.16): which exec path to take
/// plus the per-agent config to thread into it.
///
/// [`Default`] is the safe fallback (`claude`, no model/args/env) used when the
/// agent/runtime resolve fails — a misconfigured agent still dispatches to the
/// default provider rather than stranding the task.
#[derive(Debug, Clone, Default)]
struct ResolvedDispatch {
    /// Which provider exec path [`execute_claimed`] routes to.
    backend: Backend,
    /// The `model` + `cli_args` the provider threads onto its argv.
    invocation: ProviderInvocation,
    /// The agent's per-agent env (`agent_env`), layered onto the child env after
    /// the deny-by-default ambient allowlist.
    agent_env: Vec<(String, String)>,
}

/// Resolve the provider routing for a task's agent (e38.16).
///
/// Reads the agent row (its migration-0015 `model`/`cli_args`/`agent_env`
/// config) and its runtime's `provider` wire name, mapping the latter to a
/// [`Backend`]. The agent's `model` and `cli_args` become the
/// [`ProviderInvocation`]; its `agent_env` is carried separately.
///
/// # Errors
///
/// Returns an error if the agent id is malformed or the agent / runtime row is
/// missing — the caller falls back to the [`ResolvedDispatch::default`].
async fn resolve_dispatch(pool: &SqlitePool, agent_id: &str) -> anyhow::Result<ResolvedDispatch> {
    use ainb_hangar_store::repo::agent::AgentRepo;
    use ainb_hangar_store::repo::agent_runtime::AgentRuntimeRepo;
    let agent = AgentRepo::get(pool, agent_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("agent {agent_id} not found"))?;
    let runtime = AgentRuntimeRepo::get(pool, &agent.runtime_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("runtime {} not found", agent.runtime_id))?;
    Ok(ResolvedDispatch {
        backend: Backend::from_provider(&runtime.provider),
        invocation: ProviderInvocation {
            model: agent.model,
            cli_args: agent.cli_args,
        },
        agent_env: agent.agent_env,
    })
}

/// Resolve the provider wire name and owning workspace for a task's agent
/// (agent → `workspace_id` + agent → runtime → `provider`).
async fn resolve_provider_and_workspace(
    pool: &SqlitePool,
    agent_id: &str,
) -> anyhow::Result<(String, ainb_hangar_core::ids::WorkspaceId)> {
    use ainb_hangar_store::repo::agent::AgentRepo;
    use ainb_hangar_store::repo::agent_runtime::AgentRuntimeRepo;
    let agent = AgentRepo::get(pool, agent_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("agent {agent_id} not found"))?;
    let workspace = ainb_hangar_core::ids::WorkspaceId::from_str(agent.workspace_id.clone())
        .map_err(|_| anyhow::anyhow!("agent {agent_id} has empty workspace_id"))?;
    let runtime = AgentRuntimeRepo::get(pool, &agent.runtime_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("runtime {} not found", agent.runtime_id))?;
    Ok((runtime.provider, workspace))
}

/// Look up a workspace's slug by id.
async fn workspace_slug(pool: &SqlitePool, workspace_id: &str) -> anyhow::Result<String> {
    let row = sqlx::query("SELECT slug FROM workspace WHERE id = ?")
        .bind(workspace_id)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| anyhow::anyhow!("workspace {workspace_id} not found"))?;
    Ok(row.try_get::<String, _>("slug")?)
}

/// Persist the provider `session_id` onto the task row (best-effort; only when a
/// session was actually opened).
async fn persist_session_id(
    pool: &SqlitePool,
    task_id: &str,
    session_id: Option<&str>,
) -> anyhow::Result<()> {
    if let Some(sid) = session_id {
        sqlx::query("UPDATE agent_task_queue SET session_id = ? WHERE id = ?")
            .bind(sid)
            .bind(task_id)
            .execute(pool)
            .await?;
    }
    Ok(())
}

/// Load the env-allowlist policy from `~/.ainb/hangar/env.allow.toml`.
///
/// Falls back to the operator-default policy (the 12 built-ins + hardcoded deny)
/// if the path can't be resolved or the file is unreadable — a missing/corrupt
/// config must never down a daemon dispatch.
fn load_env_policy() -> ainb_hangar_core::env_policy::EnvPolicy {
    crate::dispatch::default_allow_path()
        .and_then(|p| crate::dispatch::load_allow_at(&p))
        .map_or_else(
            |e| {
                tracing::warn!(error = %e, "env allow config load failed; using defaults");
                ainb_hangar_core::env_policy::EnvPolicy::default()
            },
            crate::dispatch::EnvAllowConfig::into_policy,
        )
}

/// Resolve the Hangar home directory the per-task env tree is rooted under.
///
/// `$AINB_HANGAR_HOME` (when set and non-empty), else the user's home. The
/// per-task layout then lives at `{home}/.ainb/hangar/workspaces/...` (see
/// [`crate::execenv::prepare_env`]). Mirrors [`ainb_hangar_store::Store`]'s home
/// resolution so the daemon's env dirs and its database share a root.
fn hangar_home() -> PathBuf {
    std::env::var_os("AINB_HANGAR_HOME").filter(|p| !p.is_empty()).map_or_else(
        || dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")),
        PathBuf::from,
    )
}

/// Warn about `danger-full-access` on the first invocation of `provider` in this
/// task's session (P5.6). Best-effort: a `state.toml` path-resolution or IO
/// fault is logged and swallowed — a warning bookkeeping failure must never
/// block a dispatch. The session id is the task's resumed provider session when
/// present, else the task id (a fresh run is a fresh warning surface).
fn warn_danger_access(task: &Task, provider: &str) {
    let session = task.session_id.as_deref().unwrap_or(task.id.as_str());
    let outcome = crate::warnings::default_state_path()
        .and_then(|p| crate::warnings::maybe_warn_provider(&p, provider, session));
    if let Err(e) = outcome {
        tracing::warn!(error = %e, "danger-access warning bookkeeping failed; proceeding");
    }
}
