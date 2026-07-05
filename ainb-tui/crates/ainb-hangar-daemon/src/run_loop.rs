//! The daemon's claim loop + sweeper scheduler (P1.7).
//!
//! [`run`] is the daemon's steady-state body: it spawns the four periodic
//! sweepers (P1.4) and then polls [`ClaimTaskService`] for the oldest `queued`
//! task bound to this daemon's runtime. Each claimed task walks the FSM —
//! `dispatched -> running` ([`StartTaskService`]), provider exec
//! ([`Runner::run_claude`]), then `running -> done` ([`CompleteTaskService`])
//! or `running -> failed` ([`FailTaskService`]) — inside its isolated
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
//! | `HANGAR_GC_INTERVAL_MS` | workspace-GC pass interval (on-disk orphan reclaim) | `3600000` |
//! | `HANGAR_PROVIDER_MAX_RUNTIME_MS` | provider runtime deadline override (tests) | reference running TTL (2.5h) |
//! | `HANGAR_SWEEP_DISPATCHED_TTL_MS` | dispatch TTL override (tests) | reference default |
//! | `HANGAR_DAEMON_DISABLE_CLAIM` | skip the claim loop, run sweepers only (tests) | unset |
//! | `HANGAR_DAEMON_DISABLE_SANDBOX` | set to `1` to run providers UNCONFINED (security downgrade) | unset (sandbox ON) |
//!
//! When `HANGAR_DAEMON_RUNTIME_ID` is unset the claim loop is a no-op (the
//! daemon still sweeps) — a daemon with no runtime has nothing to claim.

// The provider runtime deadline is a "hours" quantity but `Duration::from_hours`
// is unstable; a raw second count is the clearest stable spelling (and matches
// the reference running-TTL value). Same rationale as `sweeper.rs`.
#![allow(clippy::duration_suboptimal_units)]

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ainb_hangar_core::clock::{HangarClock, SystemClock};
use ainb_hangar_core::idgen::{IdGen, SystemIdGen};
use ainb_hangar_core::task::state::TaskState;
use ainb_hangar_store::repo::task::{Task, TaskRepo};
use ainb_hangar_store::service::claim::{ClaimTaskService, ClaimedTask};
use ainb_hangar_store::service::complete::{CompleteParams, CompleteTaskService};
use ainb_hangar_store::service::fail::FailTaskService;
use ainb_hangar_store::service::finalize::FinalizeError;
use ainb_hangar_store::service::retry::{RetryDecision, RetryService};
use ainb_hangar_store::service::start::StartTaskService;
use sqlx::{Row, SqlitePool};
use tokio::task::JoinSet;

use crate::events::EventSink;
use crate::execenv::{prepare_env, write_context_prompt};
use crate::health_stats::HealthStats;
use crate::progress_comment;
use crate::runner::{Backend, ProviderInvocation, RunOutcome, Runner, RunnerConfig};
use crate::sweeper::{
    SweeperConfig, reclaim_orphaned_on_startup, sweep_expired_queued, sweep_stale_dispatched,
    sweep_stale_running,
};

/// Default claim-poll interval when `HANGAR_DAEMON_POLL_MS` is unset.
const DEFAULT_POLL_MS: u64 = 1_000;
/// Provider runtime deadline (reference running TTL: 2.5h).
const PROVIDER_MAX_RUNTIME: Duration = Duration::from_secs(9_000);
/// Trailing stdout/stderr lines retained on each run for the audit tail.
const TAIL_LINES: usize = 200;

/// The parent-session identity the daemon stamps onto every task it spawns
/// (ccc / D11), via `AINB_PARENT_SESSION` in the provider's child env.
///
/// It names the hangar daemon as the owning orchestrator so the lifecycle hook
/// (`ainb fleet atc hook`) resolves fleet membership and forwards the session's
/// AskUserQuestion / lifecycle events into the attention pipeline — the fix for
/// a daemon-spawned run showing "0 need you" while its picker is open (INV-3 /
/// D11). A stable constant, not a per-task id: the spawner is the one daemon,
/// and the attention ingest scopes hook rows host-wide regardless, so a finer
/// key buys nothing. Completions the hook routes to `inbox/hangar-daemon.jsonl`
/// are the daemon's own (the daemon detects completion via the run outcome, not
/// the inbox), so that inbox is pure exhaust — [`crate::inbox_sweep`] caps it
/// on the sweeper tick (y0f) so it never grows without bound.
const HANGAR_PARENT_SESSION: &str = "hangar-daemon";

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
    /// past it ([`FailureReason::Timeout`]). Defaults to the reference running
    /// TTL (2.5h); overridable via `HANGAR_PROVIDER_MAX_RUNTIME_MS` so an
    /// e2e test can drive the timeout-kill path within a bounded budget.
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
        // e38.22: the on-disk workspace-GC cadence is independently tunable (it
        // is far longer than the row-sweep cadence by default); an e2e test can
        // tighten it to drive a reclaim within a bounded budget.
        if let Some(ms) = env_u64_opt("HANGAR_GC_INTERVAL_MS") {
            sweeper.gc_interval = Duration::from_millis(ms);
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

/// Tracks the tmux session names of in-flight INTERACTIVE runs so daemon
/// shutdown can reap them (a54).
///
/// An interactive run is a DETACHED tmux session ([`crate::interactive`]): it
/// survives the daemon process exiting, and aborting the in-flight `wait`
/// future (as the [`JoinSet`] does on shutdown) does NOT kill it — so a naive
/// shutdown would orphan every live interactive pane. This shared set records
/// each live session name from spawn until its `wait` returns; on `Ctrl-C` the
/// loop drains it and kills each session by its EXACT name (never a wildcard),
/// mirroring the reap the old single-inline path got "for free" by blocking
/// shutdown until the run finished.
///
/// Cheap to clone (an `Arc`) so each spawned execution shares the one set. A
/// poisoned lock is treated as empty rather than panicking a shutdown path.
#[derive(Clone, Default)]
pub(crate) struct InteractiveSessions {
    inner: Arc<Mutex<HashSet<String>>>,
}

impl InteractiveSessions {
    /// Record a now-live interactive session so shutdown can reap it.
    fn register(&self, session_name: &str) {
        if let Ok(mut set) = self.inner.lock() {
            set.insert(session_name.to_string());
        }
    }

    /// Drop a session that has already been reaped naturally (its `wait`
    /// returned) so shutdown does not try to kill an already-gone session and
    /// the set stays bounded.
    fn unregister(&self, session_name: &str) {
        if let Ok(mut set) = self.inner.lock() {
            set.remove(session_name);
        }
    }

    /// Take every still-live session name (clearing the set) for the shutdown
    /// reap. A poisoned lock yields an empty vec — shutdown must never panic.
    fn drain(&self) -> Vec<String> {
        self.inner.lock().map_or_else(|_| Vec::new(), |mut set| set.drain().collect())
    }
}

/// Kill every still-live interactive tmux session by its EXACT name so daemon
/// shutdown never orphans a detached pane (a54). Best-effort and bounded: a
/// session already gone is a harmless no-op.
async fn reap_interactive_sessions(sessions: &InteractiveSessions) {
    let names = sessions.drain();
    if names.is_empty() {
        return;
    }
    tracing::info!(
        count = names.len(),
        "reaping in-flight interactive sessions on shutdown"
    );
    for name in names {
        crate::interactive::kill_session(&name).await;
    }
}

/// Run the daemon's steady state: spawn sweepers, then poll-claim-execute until
/// `Ctrl-C`.
///
/// `stats` is the shared in-memory health collector (P8.5): each task's
/// terminal outcome is recorded into its rolling throughput ring as the FSM
/// finalises, so the `hangar/daemon_health` pane sees live per-second completed
/// / failed counts. The collector is shared with the RPC server (which
/// snapshots it).
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
    // e38.22: schedule the on-disk workspace GC alongside the row-sweepers, so
    // leaked per-task dirs (no `.gc_meta.json`, mtime past the 72h grace) and
    // build artifacts are actually reclaimed instead of accumulating forever.
    // Rooted at the same Hangar home the per-task env tree is created under. The
    // handle is dropped (process exit tears the task down, mirroring
    // `spawn_sweepers`); a future supervisor can keep it to cancel cleanly.
    let _gc = spawn_gc_sweeper(
        pool.clone(),
        hangar_home(),
        cfg.sweeper.gc_interval,
        Arc::new(SystemClock),
    );

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

    // a54: claimed executions run on this JoinSet, NOT inline, so one long-lived
    // run — most acutely an INTERACTIVE session a human is attached to — never
    // wedges the claim loop. The loop keeps polling and claiming while runs are
    // in flight. The per-agent `max_concurrent_tasks` cap needs no in-memory
    // accounting: the claim SQL itself counts the agent's live
    // `dispatched`+`running` rows (`ClaimTaskService`), and a claimed row is
    // `dispatched` from the instant of claim through to its terminal finalize —
    // so the DB's live in-flight count is the authoritative bound the next claim
    // consults, and a spawned-but-not-yet-polled run is already accounted for.
    let mut runs: JoinSet<()> = JoinSet::new();
    // a54 shutdown reap: the set of live interactive tmux sessions, so `Ctrl-C`
    // can kill each by exact name instead of orphaning a detached pane.
    let interactive = InteractiveSessions::default();

    loop {
        tokio::select! {
            biased;
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("ainb-hangar-daemon shutting down");
                // Reap every in-flight interactive tmux session by exact name so
                // no detached pane orphans (aborting its `wait` future below does
                // NOT kill a detached session).
                reap_interactive_sessions(&interactive).await;
                // Then abort + drain every in-flight run. Each headless provider
                // was spawned with `kill_on_drop(true)`, so dropping its aborted
                // future SIGKILLs the child instead of leaving it reparented to
                // init, mutating the workspace unsupervised. `shutdown()` awaits
                // each aborted task so the kills are delivered BEFORE we return —
                // it does NOT wait for a provider to finish (abort cancels the
                // wait), so shutdown stays prompt. The DB rows stay `running` and
                // are requeued by the next boot's crash-recovery reclaim (and
                // backstopped by the stale-running sweeper).
                runs.shutdown().await;
                return Ok(());
            }
            // Reap a finished run so the JoinSet never accumulates completed
            // handles. Disabled while empty (`if !runs.is_empty()`) so the arm is
            // inert on an idle daemon rather than resolving `None` in a hot spin.
            Some(joined) = runs.join_next(), if !runs.is_empty() => {
                if let Err(e) = joined {
                    // A panic in one execution must never down the loop; log it
                    // and keep claiming (a cancellation is a normal shutdown).
                    if e.is_panic() {
                        tracing::error!(error = %e, "task execution panicked");
                    }
                }
                continue;
            }
            () = tokio::time::sleep(cfg.poll_interval) => {}
        }

        match ClaimTaskService::claim_for_runtime(&pool, &runtime_id, &SystemClock).await {
            Ok(Some(claimed)) => {
                // Spawn the execution so the loop returns immediately to claiming
                // the next task rather than blocking on this run's completion.
                let pool = pool.clone();
                let runner = runner.clone();
                let stats = stats.clone();
                let events = events.clone();
                let interactive = interactive.clone();
                runs.spawn(async move {
                    let clock = SystemClock;
                    if let Err(e) =
                        execute_claimed(&pool, &runner, &claimed, &clock, &stats, &events, &interactive)
                            .await
                    {
                        tracing::error!(task_id = %claimed.id, error = %e, "task execution errored");
                    }
                });
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
                // y0f: cap the daemon's OWN parent inbox on the same tick. The
                // hook routes every daemon-spawned run's completion there, but the
                // daemon detects completion via the run outcome — so the file is
                // pure exhaust that would grow forever. The cap is blocking file
                // IO (an fs2 advisory lock + atomic rewrite), so it runs on the
                // blocking pool; a fault is logged, never fatal.
                cap_parent_inbox().await;
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

/// Cap the daemon's own parent completion inbox to its most-recent-N records
/// (y0f), on the blocking pool since it takes an `fs2` advisory lock + rewrites
/// a file. Best-effort: a missing home / file is a no-op, an IO fault or a join
/// error is logged and swallowed — inbox hygiene must never down a sweeper.
async fn cap_parent_inbox() {
    match tokio::task::spawn_blocking(|| {
        crate::inbox_sweep::sweep_parent_inbox(crate::inbox_sweep::KEEP_LAST)
    })
    .await
    {
        Ok(Ok(report)) if report.evicted() > 0 => {
            tracing::info!(
                evicted = report.evicted(),
                kept = report.kept,
                "hangar-daemon inbox capped"
            );
        }
        Ok(Ok(_)) => {}
        Ok(Err(e)) => tracing::warn!(error = %e, "hangar-daemon inbox cap failed"),
        Err(e) => tracing::warn!(error = %e, "hangar-daemon inbox cap task join failed"),
    }
}

/// Spawn the periodic workspace-GC sweeper as a background task (e38.22).
///
/// On every `interval` tick it walks the live workspace tree under `home`
/// (`{home}/.agents-in-a-box/hangar/workspaces/{ws_slug}/{shortID}/`) via
/// [`sweep_workspaces_gc`], reclaiming each orphaned per-task dir — no
/// `.gc_meta.json` marker AND mtime older than the 72h grace relative to the
/// injected clock's `now_ms()` — while leaving every live (marked) dir and
/// every young orphan in place. This is the scheduled driver the bead is about:
/// the orphan-scan code existed but nothing ticked it, so leaked dirs
/// accumulated forever.
///
/// On the same tick it also sweeps orphaned task WORKTREES under
/// `{home}/.agents-in-a-box/worktrees/` (tcp yjj) via
/// [`sweep_orphan_worktrees`](crate::workdir_provision::sweep_orphan_worktrees):
/// a Ctrl-C mid-run or a crash between finalize and teardown leaves a terminal
/// task's git worktree on disk, so this removes each whose task is terminal and
/// whose tree is clean (keeping dirty ones), pruning the git registration. This
/// pass needs the store to resolve each worktree's task status, hence the `pool`.
///
/// The `clock` is injected so the 72h grace comparison is deterministic under
/// test (production passes [`SystemClock`]). Each pass is independent and
/// idempotent (a missing tree is a no-op, an already-removed dir is success),
/// so a missed or overlapping tick is harmless. Returns the task's
/// [`JoinHandle`] so a caller (the integration test, a future supervisor) can
/// stop it; the production daemon drops it and relies on process exit to tear
/// the task down, mirroring [`spawn_sweepers`].
#[must_use]
pub fn spawn_gc_sweeper(
    pool: SqlitePool,
    home: PathBuf,
    interval: Duration,
    clock: Arc<dyn HangarClock>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(interval);
        loop {
            tick.tick().await;
            match crate::execenv::sweep_workspaces_gc(&home, clock.now_ms()) {
                Ok(report) if report.reclaimed > 0 => {
                    tracing::info!(
                        reclaimed = report.reclaimed,
                        retained = report.retained,
                        "workspace_gc_swept"
                    );
                }
                Ok(_) => {}
                Err(e) => tracing::error!(error = %e, kind = "workspace_gc", "gc pass failed"),
            }
            match crate::workdir_provision::sweep_orphan_worktrees(&pool, &home).await {
                Ok(report) if report.removed > 0 || report.kept_dirty > 0 => {
                    tracing::info!(
                        removed = report.removed,
                        kept_dirty = report.kept_dirty,
                        kept_active = report.kept_active,
                        "worktree_gc_swept"
                    );
                }
                Ok(_) => {}
                Err(e) => tracing::error!(error = %e, kind = "worktree_gc", "gc pass failed"),
            }
        }
    })
}

/// Walk one claimed task through `dispatched -> running -> done|failed`.
///
/// Re-reads the full row (for `workspace_id` / `issue_id`), resolves the
/// workspace slug, prepares the isolated env, marks the task `running`, runs
/// the provider, and finalises the row from the [`RunOutcome`].
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
    interactive: &InteractiveSessions,
) -> anyhow::Result<()> {
    let task: Task = TaskRepo::get_by_id(pool, &claimed.id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("claimed task {} vanished", claimed.id))?;
    let ws_slug = workspace_slug(pool, &task.workspace_id).await?;
    let home = hangar_home();
    let env = prepare_env(&task, &ws_slug, &home, clock)?;

    // e38.21: inject the workspace's context prompt into the task's execenv as a
    // `CLAUDE.md` so the agent run actually sees the per-workspace context (the
    // provider reads `CLAUDE.md` from its CWD). An unconfigured workspace writes
    // no file (the v1 behaviour); a config-read or write fault is non-fatal — a
    // task must still dispatch even if its context cannot be materialised.
    match workspace_context_prompt(pool, &task.workspace_id).await {
        Ok(prompt) => {
            if let Err(e) = write_context_prompt(&env, prompt.as_deref()) {
                tracing::warn!(error = %e, task_id = %task.id, "context prompt injection failed");
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, task_id = %task.id, "context prompt read failed");
        }
    }

    // e38.16: resolve which provider exec path this task routes to (agent →
    // runtime → provider) and the per-agent config (model / cli_args / agent_env)
    // the runner threads into that provider's argv + env. A resolve fault
    // defaults the dispatch to `claude` with empty config so a misconfigured
    // agent still runs rather than stranding the task.
    let dispatch = resolve_dispatch(pool, &task.agent_id).await.unwrap_or_default();

    // F5: provision the run's working directory from the card's `repo_ref`.
    //
    // A card task carries `scratch` or an absolute repo path (rpc `card_run`
    // enforces F2 — a repo is always set), so it runs in a volatile git worktree
    // (`~/.agents-in-a-box/worktrees/<shortID>` on branch `ainb/<shortID>`) or the
    // scratch repo; a chat / autopilot task has no `repo_ref` and runs in the
    // in-tree fallback workdir (the pre-F5 behaviour). The short-id slug is unique
    // per task, so N cards on ONE repo provision N distinct worktrees that never
    // collide (the F5 concurrency guarantee). A provision fault is treated like
    // any other setup fault (`prepare_env` above): it propagates so the row stays
    // `dispatched` and the stale-dispatch sweeper reclaims + re-queues it, rather
    // than dispatching a run into an unprovisioned dir. The injected `CLAUDE.md`
    // (above) + skills stay in the task tree, NOT the worktree, so teardown's
    // keep-if-dirty check sees only genuine agent work.
    //
    // tcp 19n: the repo is read straight off the claimed `Task` (which now carries
    // `repo_ref` verbatim from the row) rather than a second `card_parity` query.
    // The single source means a RuntimeOffline-retried child — whose INSERT copies
    // `repo_ref` — provisions the SAME repo's worktree instead of the in-tree
    // fallback a dropped `repo_ref` used to force.
    //
    // Two slugs: the per-run FULL task id keys the volatile worktree (unique per
    // run — the F5 no-collision guarantee, made airtight by the full id, tcp vpm),
    // while SCRATCH is keyed on the (issue, agent) pair. A rerun re-dispatches the
    // card to the SAME agent, so (issue, agent) reuses the durable scratch dir
    // across reruns (F2 intent); a SQUAD fan-out gives each member a DISTINCT agent
    // on the one issue (the 0012 (issue, agent) pending-guard scope), so members
    // that claim in parallel get DISTINCT scratch dirs and never race in one working
    // tree. Both slugs use FULL ids so no truncated prefix can cross-wire two runs.
    // A task with no issue never resolves `scratch`, so its fallback to the run slug
    // is inert.
    let run_slug = task.id.clone();
    let scratch_slug = task.issue_id.as_deref().map_or_else(
        || run_slug.clone(),
        |issue| format!("{}-{}", issue, task.agent_id),
    );
    let run_wd = crate::workdir_provision::provision(
        task.repo_ref.as_deref(),
        &run_slug,
        &scratch_slug,
        &home,
        &env.workdir,
    )?;
    let location = run_location_for(&run_wd);
    tracing::info!(task_id = %task.id, cwd = %run_wd.path().display(), "run workdir provisioned");

    // T8: the typed in-process lifecycle guard. It begins at `dispatched` (the
    // `queued -> dispatched` claim already committed via `ClaimTaskService`,
    // arbitrated by the migration-0012 SQL index) and types every remaining edge
    // the loop drives. Advancing it BEFORE each store-service write turns an
    // out-of-order finalize into a typed error rather than a silent mis-step;
    // the store-service idempotent finalize + the SQL guard stay the
    // authoritative DB-level enforcers for atomicity and cross-daemon races.
    let mut lifecycle = crate::fsm::LifecycleGuard::claimed();

    // tcp T3 / F6: register this run in the kill registry BEFORE the start
    // transition, so a cancel arriving during setup (or racing the start) finds a
    // live token to signal rather than missing it in the window between start and
    // a later registration. The guard's `Drop` deregisters on every exit path.
    let cancel_guard = crate::cancel::registry().register(&task.id);

    // dispatched -> running. A lost race (another worker started it) surfaces as
    // an FSM error; bail without running a duplicate provider. The typed guard
    // advances first: a `dispatched -> running` edge is legal, so this only fails
    // if the loop is driven out of order (a logic bug), never on a real run.
    lifecycle.fire(crate::fsm::LifecycleEvent::Start)?;
    match StartTaskService::start(pool, &task.id, clock).await {
        Ok(_) => {}
        // tcp T3 / F6: a cancel landed BEFORE the run could start (the RPC flipped
        // the row `{queued|dispatched} -> cancelled`, winning the finalize race).
        // The provider never spawned, so reclaim the just-provisioned worktree and
        // finish as cancelled without running anything — never leak the checkout.
        Err(FinalizeError::TerminalMismatch {
            found: TaskState::Cancelled,
            ..
        }) => {
            tracing::info!(task_id = %task.id, "cancelled before start; tearing down without running");
            teardown_workdir(&run_wd, &task.id);
            return Ok(());
        }
        Err(e) => return Err(e.into()),
    }
    // Surface the typed FSM position beside the DB transition so a lifecycle
    // race is greppable in the daemon log next to the store's `task.start` span.
    tracing::info!(task_id = %task.id, lifecycle = lifecycle.state().as_db_str(), "task running");
    // e38.2: announce the start to subscribed plugins (best-effort push; the
    // next snapshot pull reconciles if no subscriber is connected).
    emit_task_started(events, &task, clock);
    // P4 / D8: auto-move the task's issue card into any board's `running`
    // auto-move column (best-effort; never blocks the FSM).
    crate::board::auto_move_after_transition(pool, &task, "running").await;
    // e38.6: write a durable, agent-authored "started" comment to the task's
    // issue so the agent's activity survives beyond the bounded transcript
    // buffer. A NULL-issue chat task writes nothing; a write fault is logged,
    // never blocks the FSM (the `running` transition has already committed).
    progress_comment::emit_checkpoint(
        pool,
        &SystemIdGen,
        clock,
        &task,
        progress_comment::Checkpoint::Started,
    )
    .await;

    // Snapshot the daemon's env into an owned map *before* the await: the
    // `std::env::Vars` iterator is `!Send`, so holding it across `.await` would
    // make this future non-`Send` and unusable on the multi-thread runtime.
    //
    // P5.3: apply the configurable env-allowlist policy here (the authoritative
    // pass), layering keychain-resident API keys on top via
    // `dispatch::build_task_env`. The policy is loaded from
    // `~/.agents-in-a-box/hangar/env.allow.toml` (operator defaults when absent).
    // The keychain key list is empty until P5.2 wires `host/secret_store_get`
    // into dispatch; the seam is in place so adding keys is a one-line change.
    // The runner re-applies its own deny-by-default 12-var filter as
    // defense-in-depth (a strict subset of this policy).
    let daemon_env: std::collections::HashMap<String, String> = std::env::vars().collect();
    let policy = load_env_policy();
    let keychain_keys: Vec<(String, String)> = Vec::new();
    let mut task_env = crate::dispatch::build_task_env(&daemon_env, keychain_keys, &policy);

    // ccc / D11: mark this daemon-spawned session as a legitimate fleet member by
    // stamping AINB_PARENT_SESSION into the provider's child env. The lifecycle
    // hook gates its `events.jsonl` append on fleet membership (resolvable parent
    // | non-empty inbox | ATC cwd); a card run has none, so its AskUserQuestion
    // never reached the attention pipeline (the control centre showed "0 need you"
    // with the picker open — INV-3 / D11). Naming the hangar daemon as the parent
    // resolves membership for BOTH provider paths at once: the headless
    // (`run_claude` / `run_codex_with_env`) and interactive (`run_interactive`)
    // spawns each compose their child env from `task_env`, and the runner
    // allowlists this key so it survives the deny-by-default filter. It is set
    // AFTER `build_task_env` so the daemon's own value always wins over any
    // ambient one.
    task_env.insert(
        ainb_fleet_core::session_registry::PARENT_ENV.to_string(),
        HANGAR_PARENT_SESSION.to_string(),
    );

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

    // P5 (D16): compile-on-dispatch — if a profile master matches this task's
    // agent slug, materialise the resolved tool-native files (Claude `.md` /
    // Codex config+prompt) into the task's execution env and forward the same
    // `*_HOME` pointer the skills layout uses (idempotent when both wrote one).
    // Best-effort: a missing profile or a write fault is logged and skipped — a
    // task must still dispatch without its profile.
    if let Some((key, path)) =
        materialise_agent_profile(pool, &task, &env, dispatch.backend.name()).await
    {
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

    // ccc / D6: an `interactive` task launches the provider inside a REAL,
    // attachable tmux session (not the headless pipe-capture path). The session
    // name is recorded on the row the moment it is created so the attach-from-card
    // affordance can reach it mid-run; completion is detected by the session being
    // reaped, mapped onto the same `RunOutcome` the headless path returns.
    //
    // e38.16 (headless): route to the resolved provider's exec path. `claude`
    // takes the allowlist-filtered env and no argv; `codex` runs `codex exec` with
    // the agent's model/cli_args on the argv and its `agent_env` layered onto the
    // child env. Both spawn through the same OS sandbox (e38.23).
    // tcp T3 / F6: register this run in the daemon's kill registry so the cancel
    // RPC (which runs on the RPC server task, not this claim-loop task) can stop
    // it, then RACE the provider against that cancel signal. When the signal
    // fires, `provider_run` is DROPPED — for a headless run that SIGKILLs the
    // provider's process group via the runner's `kill_on_drop(true)` — and, for
    // interactive, the detached tmux session (which a dropped `wait` does NOT
    // kill) is torn down by its exact name below. Both settle as
    // `RunOutcome::Cancelled`, finalised through the dedicated cancelled seam
    // (never the failure path, so a cancel neither auto-moves to `failed` nor
    // spawns a retry child).
    // P10 / D19: the provider that executed this run, recorded on the run-history
    // row + the OTLP task->run span. Captured BEFORE `provider_run` moves
    // `dispatch` (the async block takes `dispatch.agent_env` by value). The
    // `cancel_guard` was registered up front (before the start transition).
    let provider = dispatch.backend.name();
    let provider_run = async {
        if task.mode == "interactive" {
            run_interactive(
                pool,
                runner,
                &task,
                &ws_slug,
                &env,
                run_wd.path(),
                task_env,
                &dispatch,
                interactive,
            )
            .await
        } else {
            // F5: run in the provisioned worktree / scratch dir (`location.cwd`),
            // widening the sandbox to it (`location.extra_root`) so the confined
            // agent can write its own checkout. The runner's `io::Result` is
            // lifted to `anyhow` so both `provider_run` arms share one error type
            // (the interactive path already returns `anyhow::Result`).
            match dispatch.backend {
                Backend::Claude => runner
                    .run_claude_in(&env, task_env, &location)
                    .await
                    .map_err(anyhow::Error::from),
                Backend::Codex => runner
                    .run_codex_in(
                        &env,
                        task_env,
                        dispatch.agent_env,
                        &dispatch.invocation,
                        &location,
                    )
                    .await
                    .map_err(anyhow::Error::from),
            }
        }
    };
    let outcome = tokio::select! {
        // Bias the cancel arm: an already-signalled cancel wins deterministically
        // over a provider future that became ready in the same poll.
        biased;
        () = cancel_guard.cancelled() => {
            if task.mode == "interactive" {
                // The detached session survives `run_interactive`'s dropped `wait`,
                // so kill it by its EXACT name and clear the shutdown-reap entry the
                // dropped future would otherwise leave registered (keeps the set
                // bounded — the reap on shutdown would no-op it anyway).
                let session = crate::interactive::session_name_for(&task.id);
                crate::interactive::kill_session(&session).await;
                interactive.unregister(&session);
            }
            // Headless: dropping `provider_run` here fires the runner's
            // `kill_on_drop(true)`, SIGKILLing the provider's process group.
            RunOutcome::Cancelled(crate::runner::RunnerResult::default())
        }
        res = provider_run => res?,
    };
    // The guard is dropped once the run is decided so the registry stays bounded
    // to genuinely-live runs (its `Drop` deregisters this task).
    drop(cancel_guard);

    match outcome {
        RunOutcome::Success(result) => {
            // running -> done: type the terminal edge before the store finalize.
            lifecycle.fire(crate::fsm::LifecycleEvent::Complete)?;
            finalize_success(pool, &task, &run_wd, result, provider, clock, stats, events).await?;
        }
        RunOutcome::Failed { reason, result } => {
            // running -> failed: type the terminal edge before the store finalize.
            lifecycle.fire(crate::fsm::LifecycleEvent::Fail)?;
            finalize_failure(pool, &task, &run_wd, reason, result, provider, clock, stats, events)
                .await?;
        }
        RunOutcome::Cancelled(result) => {
            // running -> cancelled (tcp T3 / F6): the cancel RPC is the
            // authoritative owner of the terminal DB flip, the terminal event, and
            // the card auto-move; this seam only reclaims the run's artifacts.
            // Type the edge first so an out-of-order cancel is a typed error.
            lifecycle.fire(crate::fsm::LifecycleEvent::Cancel)?;
            finalize_cancelled(pool, &task, &run_wd, result, provider, clock).await?;
        }
    }
    Ok(())
}

/// Launch a task's provider inside a REAL, attachable tmux session and await
/// its completion (ccc / D6 interactive mode).
///
/// Resolves the provider program + argv (the same the headless path would
/// exec), composes the deny-by-default child env, spawns the session under
/// `tmux_hangar-<task_id>`, and — crucially — records that exact session name
/// on the task row BEFORE awaiting, so the attach-from-card affordance can
/// surface a copyable `tmux attach -t <name>` while the agent is live. The
/// returned [`RunOutcome`] is the same shape the headless runner returns, so
/// the finalize seam ([`finalize_success`] / [`finalize_failure`]) is
/// unchanged.
///
/// # Errors
///
/// Returns an error only on an unrecoverable IO fault spawning the session (a
/// bad tmux invocation, an unwritable wrapper). A non-zero provider exit or a
/// timeout is a normal FSM outcome carried in the [`RunOutcome`], not an error.
async fn run_interactive(
    pool: &SqlitePool,
    runner: &Runner,
    task: &Task,
    ws_slug: &str,
    env: &crate::execenv::ExecEnv,
    cwd: &std::path::Path,
    task_env: std::collections::HashMap<String, String>,
    dispatch: &ResolvedDispatch,
    interactive: &InteractiveSessions,
) -> anyhow::Result<RunOutcome> {
    // ccc / D6: the interactive session is a REAL, attachable tmux terminal (YOLO)
    // — it is DELIBERATELY not wrapped in the headless OS FS sandbox (Seatbelt /
    // Landlock). Confining a live terminal the user attaches to and drives would
    // defeat the interactive feature; the human operator is present and in control,
    // which is the trust model D6 chose for interactive over headless.
    let session_name = crate::interactive::session_name_for(&task.id);
    let (program, argv) = runner.provider_command(dispatch.backend, &dispatch.invocation);
    // Mirror the headless env composition: the codex path layers the agent's
    // `agent_env`; the claude path layers nothing (parity with `execute_claimed`).
    let extra_env = match dispatch.backend {
        Backend::Codex => dispatch.agent_env.clone(),
        Backend::Claude => Vec::new(),
    };
    let child_env = crate::runner::compose_child_env(task_env, extra_env);

    // F5: the attachable tmux session runs in the provisioned worktree / scratch
    // dir (`cwd`), not the in-tree workdir; logs still stream to the task tree.
    let run = crate::interactive::spawn(
        &program,
        cwd,
        &argv,
        &child_env,
        &session_name,
        &env.logs,
        runner.max_runtime(),
    )
    .await?;

    // a54: register the now-live session for the shutdown reap IMMEDIATELY, before
    // any `.await` — a detached tmux session survives the daemon exiting, and the
    // JoinSet aborting this future does not kill it. Registering synchronously
    // right after spawn (no await point in between) closes the spawn→register
    // window: a Ctrl-C can only cancel this future at an await, so once we reach
    // the first await below the session is already tracked and shutdown reaps it.
    // Unregistered on every exit path (the abort branches below, and after `wait`).
    interactive.register(&session_name);

    // Record the session name on the row NOW (the session is live) so the card's
    // `a` attach affordance can reach it before the run finishes. This handle IS
    // the interactive feature: a run whose name cannot be recorded is
    // un-attachable-from-card, so on a persist failure (DB fault, or the row
    // vanished mid-flight) tear the just-spawned session down and fail the task
    // with a retryable reason rather than run an un-attachable agent to completion.
    match TaskRepo::set_session_name(pool, &task.id, &session_name).await {
        Ok(true) => {
            tracing::info!(task_id = %task.id, session = %session_name, "interactive session recorded");
        }
        Ok(false) => {
            tracing::warn!(task_id = %task.id, "interactive session name update matched no row; aborting");
            // `abort` kills the session; unregister so shutdown does not re-reap a
            // dead session and the tracking set stays bounded.
            interactive.unregister(&session_name);
            return Ok(run
                .abort(ainb_hangar_store::service::fail::FailureReason::RuntimeOffline)
                .await);
        }
        Err(e) => {
            tracing::warn!(task_id = %task.id, error = %e, "interactive session name persist failed; aborting");
            interactive.unregister(&session_name);
            return Ok(run
                .abort(ainb_hangar_store::service::fail::FailureReason::RuntimeOffline)
                .await);
        }
    }

    // ccc / D11: register the live tmux session in ainb's session registry
    // (`sessions.json`) so `ainb list` — and thus fleet discover / auto-standup /
    // `atc broadcast` — can SEE and TARGET it. Only the interactive mode registers:
    // it is a real, attachable tmux session; the headless path is a captured
    // subprocess with no tmux target, so its fleet visibility is the attention
    // pipeline (mechanism a) alone. Best-effort: the session is already live and
    // recorded on the row, so a registry write fault is logged and ignored rather
    // than failing the run (the external-dep / degrade rule).
    let record = ainb_fleet_core::session_registry::AinbSessionRecord::new(
        session_name.clone(),
        cwd.to_path_buf(),
        ws_slug.to_string(),
    );
    if let Err(e) = ainb_fleet_core::session_registry::register_session(&record) {
        tracing::warn!(task_id = %task.id, error = %e, "session registry write failed");
    }

    // a54: the session was registered for the shutdown reap right after spawn
    // (above). Unregister once `wait` returns — a naturally reaped session needs
    // no shutdown kill, and the set stays bounded. `wait` errors only on an
    // unexpected IO fault; unregister on that path too.
    let outcome = run.wait().await;
    interactive.unregister(&session_name);
    Ok(outcome?)
}

/// Finalise a successful run: capture any `gh pr create` URL, complete the row,
/// record the throughput tick, push the terminal event, and post the durable
/// "done" comment to the issue (e38.6).
///
/// Split out of [`execute_claimed`] so the claim-loop body stays readable; the
/// ordering (complete → stats → event → comment) is unchanged.
///
/// # Errors
///
/// Propagates a [`CompleteTaskService`] FSM/DB fault (an unrecoverable finalize
/// error). The progress comment is best-effort and never errors.
async fn finalize_success(
    pool: &SqlitePool,
    task: &Task,
    run_wd: &crate::workdir_provision::RunWorkdir,
    result: crate::runner::RunnerResult,
    provider: &str,
    clock: &dyn HangarClock,
    stats: &HealthStats,
    events: &EventSink,
) -> anyhow::Result<()> {
    // P9.1: v1 gh-CLI-only PR capture. If the agent shelled out to `gh pr
    // create` inside its worktree, its printed PR-URL line lands in the runner's
    // bounded stdout tail (the per-task ring buffer, cap `TAIL_LINES`,
    // oldest-line-evicting — read concurrently in the runner so it never blocks
    // the claim loop). Scan it for the canonical PR URL (last one wins on a
    // multi-PR run) and fold it into the structured result. A no-PR run yields
    // `None`, which the `TaskResult` serializer omits entirely — so the `result`
    // JSON is byte-identical to the pre-P9 shape and `pr_url` is NULL (no key),
    // never `""`.
    let pr_url = ainb_hangar_core::pr_url::parse_gh_pr_create_stdout(&result.stdout_tail);
    if let Some(url) = pr_url.as_deref() {
        tracing::info!(task_id = %task.id, pr_url = url, "captured gh pr url");
    }
    // e38.35: capture the run's usage before `result` is partially moved into
    // `CompleteParams` below, so the dashboard rollup sees this run's tokens/cost.
    let usage = result.usage.clone();
    // P10 / D19: the provider session id is also moved into `CompleteParams`
    // below; clone it first so the run-history row can record it too.
    let session_id = result.session_id.clone();
    let task_result =
        ainb_hangar_core::result::TaskResult::new(result.stdout_tail, result.exit_code, pr_url);
    let result_json =
        serde_json::to_value(&task_result).unwrap_or_else(|_| serde_json::json!({"content": ""}));
    match CompleteTaskService::complete(
        pool,
        &task.id,
        CompleteParams {
            result: result_json,
            session_id: result.session_id,
            // F5: the dir the run actually executed in (the provisioned worktree /
            // scratch, or the in-tree fallback), not the always-empty task workdir.
            work_dir: run_wd.path().to_str().map(str::to_string),
        },
        clock,
    )
    .await
    {
        Ok(_) => {}
        // tcp T3 / F6: a human cancel (`running -> cancelled`) landed between the
        // provider finishing and this finalize (the cancel RPC won the conditional
        // finalize race). Cancelled wins — skip the success side-effects (the
        // cancel RPC owns the terminal event + card auto-move), but STILL tear the
        // worktree down so the race never leaks a checkout, then return Ok so the
        // benign race is not logged as a task-execution error.
        Err(FinalizeError::TerminalMismatch {
            found: TaskState::Cancelled,
            ..
        }) => {
            tracing::info!(task_id = %task.id, "run completed but was cancelled first; honoring cancel");
            // Reclaim the run's artifacts as the cancelled seam would (branch +
            // run-history + teardown), so a race-lost natural finish keeps the
            // same observability a cleanly-cancelled run gets.
            persist_run_branch(pool, &task.id, run_wd).await;
            record_run_history(
                pool,
                task,
                provider,
                session_id.as_deref(),
                usage.as_ref(),
                "cancelled",
                clock,
            )
            .await;
            teardown_workdir(run_wd, &task.id);
            return Ok(());
        }
        Err(e) => return Err(e.into()),
    }
    // e38.35: record this run's token/cost usage now the task row is terminal
    // (best-effort; a run that reported no usage records nothing).
    persist_usage(pool, task, usage.as_ref(), clock).await;
    // tcp T2: record the worktree branch if the run left commits — before the
    // TaskFinished event below, so a board re-pulling `tasks_list` on that event
    // already surfaces the branch. A no-commit / scratch / in-tree run is a no-op.
    persist_run_branch(pool, &task.id, run_wd).await;
    // P10 / D19: append the durable run-history row (provider / session / outcome
    // / duration / token-cost) + emit the OTLP task->run span. Best-effort.
    record_run_history(
        pool,
        task,
        provider,
        session_id.as_deref(),
        usage.as_ref(),
        "success",
        clock,
    )
    .await;
    // P8.5: record the successful terminal outcome into the rolling throughput
    // ring so the daemon-health pane's sparkline sees it.
    stats.record_completed(clock.now_ms() / 1_000);
    // e38.2: push the terminal transition to subscribed plugins.
    emit_task_finished(
        events,
        task,
        ainb_hangar_proto::events::TaskResult::Success,
        clock,
    );
    // P4 / D8 + tcp T4 / FANOUT-SEMANTICS: auto-move the card by its issue's
    // AGGREGATE outcome, but only once the whole active set has drained — so a squad
    // card does not slide to `done` while its leader / other members still run.
    // Best-effort; never blocks.
    crate::board::auto_move_after_terminal(pool, task).await;
    // tcp T4 / F7 + FANOUT-SEMANTICS: a task of this card just went terminal, so
    // re-evaluate every card that DEPENDS on it — a dependent whose blockers are now
    // all FINISHED (their active sets drained with a `done`) becomes runnable (the 🔒
    // clears on the next board pull) and, if it opted into auto-run, is launched.
    // The finished decision is the store's, so this is safe on any terminal.
    // Best-effort; never blocks the claim loop.
    crate::board::unblock_dependents_after_terminal(pool, task).await;
    // e38.6: durable terminal comment on the issue thread (best-effort).
    progress_comment::emit_checkpoint(
        pool,
        &SystemIdGen,
        clock,
        task,
        progress_comment::Checkpoint::Succeeded,
    )
    .await;
    // F5: tear down the run's provisioned worktree (keep-if-dirty). A clean
    // checkout is removed + deregistered; a dirty one is kept so the agent's
    // uncommitted work survives; scratch + fallback are no-ops.
    teardown_workdir(run_wd, &task.id);
    tracing::info!(task_id = %task.id, "task done");
    Ok(())
}

/// Finalise a failed run: persist the session id (for a resume), fail the row,
/// record the throughput tick, push the terminal event, post the durable
/// "blocker" comment carrying the reason (e38.6), then evaluate the retry
/// chain.
///
/// Split out of [`execute_claimed`] alongside [`finalize_success`]; ordering
/// (persist → fail → stats → event → comment → retry) is unchanged.
///
/// # Errors
///
/// Propagates a [`persist_session_id`] or [`FailTaskService`] FSM/DB fault. The
/// progress comment and the retry evaluation are best-effort and never error.
async fn finalize_failure(
    pool: &SqlitePool,
    task: &Task,
    run_wd: &crate::workdir_provision::RunWorkdir,
    reason: ainb_hangar_store::service::fail::FailureReason,
    result: crate::runner::RunnerResult,
    provider: &str,
    clock: &dyn HangarClock,
    stats: &HealthStats,
    events: &EventSink,
) -> anyhow::Result<()> {
    // Persist the session id (if any) before failing so a retry can resume the
    // provider conversation.
    persist_session_id(pool, &task.id, result.session_id.as_deref()).await?;
    match FailTaskService::fail(pool, &task.id, reason, clock).await {
        Ok(_) => {}
        // tcp T3 / F6: a human cancel (`running -> cancelled`) beat this failure to
        // the conditional finalize. Cancelled wins — skip the failure side-effects
        // (event / auto-move / retry are the cancel RPC's / not wanted for a
        // cancel), but STILL tear the worktree down so the race never leaks, then
        // return Ok so the benign race is not logged as an execution error.
        Err(FinalizeError::TerminalMismatch {
            found: TaskState::Cancelled,
            ..
        }) => {
            tracing::info!(task_id = %task.id, "run failed but was cancelled first; honoring cancel");
            // Reclaim artifacts as the cancelled seam does (branch + run-history +
            // teardown) so a race-lost natural finish keeps the same observability.
            persist_run_branch(pool, &task.id, run_wd).await;
            record_run_history(
                pool,
                task,
                provider,
                result.session_id.as_deref(),
                result.usage.as_ref(),
                "cancelled",
                clock,
            )
            .await;
            teardown_workdir(run_wd, &task.id);
            return Ok(());
        }
        Err(e) => return Err(e.into()),
    }
    // e38.35: a failed/timed-out run can still report partial usage worth
    // accounting; record it now the row is terminal (best-effort).
    persist_usage(pool, task, result.usage.as_ref(), clock).await;
    // tcp T2: a failed run that still committed leaves a durable branch — record
    // it (before the TaskFinished event) so the card surfaces the partial work.
    persist_run_branch(pool, &task.id, run_wd).await;
    // P10 / D19: a failed run still appends a run-history row (outcome=failed) +
    // emits the OTLP task->run span, so the timeline records the failure and its
    // partial token-cost. Best-effort.
    record_run_history(
        pool,
        task,
        provider,
        result.session_id.as_deref(),
        result.usage.as_ref(),
        "failed",
        clock,
    )
    .await;
    // P8.5: record the failed terminal outcome (drives the sparkline's red
    // proportion for this second).
    stats.record_failed(clock.now_ms() / 1_000);
    // e38.2: push the terminal transition to subscribed plugins.
    emit_task_finished(
        events,
        task,
        ainb_hangar_proto::events::TaskResult::Failure,
        clock,
    );
    // P4 / D8 + tcp T4 / FANOUT-SEMANTICS: auto-move the card by its issue's
    // AGGREGATE outcome once the whole active set has drained. A single failed member
    // does not move a still-running squad card; the LAST sibling to drain moves it,
    // and one failed sibling lands the whole card in the `failed` column (aggregate
    // precedence). Best-effort; never blocks.
    crate::board::auto_move_after_terminal(pool, task).await;
    // tcp T4 / F7 + FANOUT-SEMANTICS: this member going terminal may have drained a
    // blocker whose set already held a `done` sibling — re-evaluate dependents so a
    // squad blocker that finished on a mixed done/failed drain still unblocks. The
    // store owns the finished decision, so a genuinely-unfinished blocker is a no-op.
    // Best-effort; never blocks the claim loop.
    crate::board::unblock_dependents_after_terminal(pool, task).await;
    // e38.6: durable blocker comment carrying the failure reason, so the issue
    // thread records WHY the run stopped (best-effort).
    progress_comment::emit_checkpoint(
        pool,
        &SystemIdGen,
        clock,
        task,
        progress_comment::Checkpoint::Failed { reason },
    )
    .await;
    tracing::warn!(task_id = %task.id, reason = reason.as_db_str(), "task failed");
    // F5: tear down the run's provisioned worktree (keep-if-dirty). A failed run
    // that left a dirty checkout keeps it (the partial work is preserved for a
    // rerun / inspection); a clean one is removed. Done BEFORE the retry spawn so
    // a retryable failure's fresh child re-provisions a clean worktree rather than
    // resuming into this run's residue.
    teardown_workdir(run_wd, &task.id);
    // F06 retry chain: a retryable (infra) failure with attempts remaining
    // spawns a fresh `queued` child carrying `parent_task_id`, which the next
    // claim pass re-dispatches. A terminal reason (`agent_error` / `user_cancel`
    // / `timeout`) or an exhausted `max_attempts` is a no-op. The retry insert
    // can collide with the per-issue pending-unique index — that is a benign
    // already-pending outcome (a sibling holds the slot), logged not propagated,
    // so one failed retry never downs the claim loop.
    maybe_spawn_retry(pool, &task.id, clock).await;
    Ok(())
}

/// Finalise a cancelled run (tcp T3 / F6): reclaim the run's artifacts after a
/// human cancel signalled it mid-flight.
///
/// The cancel RPC ([`crate::rpc`]) is the authoritative owner of the terminal
/// transition — it flips the row `running -> cancelled` via the store's
/// `CancelTaskService`, emits the `TaskFinished(Cancelled)` event, and auto-moves
/// the card BEFORE signalling this run to stop. So this seam does NOT re-write
/// the DB, re-emit the event, or re-move the card; it only reclaims what this run
/// future owns and the RPC cannot see: a durable branch a cancelled run may have
/// committed, a run-history row recording the cancel, and the provisioned
/// worktree (torn down keep-if-dirty).
///
/// Deliberately NO retry — a user cancel is terminal by intent
/// (`UserCancel -> NoRetry`) — and NO throughput tick (a cancel is neither a
/// success nor a failure the health sparkline should count).
///
/// # Errors
///
/// Never returns `Err`: every step (branch capture, run-history append, teardown)
/// is best-effort and self-contained. The `Result` matches the sibling finalize
/// seams so the `execute_claimed` match arm reads uniformly.
async fn finalize_cancelled(
    pool: &SqlitePool,
    task: &Task,
    run_wd: &crate::workdir_provision::RunWorkdir,
    result: crate::runner::RunnerResult,
    provider: &str,
    clock: &dyn HangarClock,
) -> anyhow::Result<()> {
    // A cancelled run that still committed leaves a durable branch — record it so
    // the card can surface the partial work (mirrors the failed path).
    persist_run_branch(pool, &task.id, run_wd).await;
    // P10 / D19: append a run-history row (outcome=cancelled) so the observability
    // timeline records the cancel + any partial token-cost. Best-effort.
    record_run_history(
        pool,
        task,
        provider,
        result.session_id.as_deref(),
        result.usage.as_ref(),
        "cancelled",
        clock,
    )
    .await;
    // F5: tear down the run's provisioned worktree (keep-if-dirty). A cancelled
    // run that left a dirty checkout keeps it for inspection; a clean one is
    // removed + deregistered.
    teardown_workdir(run_wd, &task.id);
    tracing::info!(task_id = %task.id, "task cancelled");
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
///
/// `pub(crate)` so the cancel RPC (tcp T3 / F6) can push the same terminal event
/// the FSM finalize seams do — it owns the `running -> cancelled` transition, so
/// it emits `TaskFinished(Cancelled)` through this one helper rather than
/// hand-rolling a divergent event.
pub(crate) fn emit_task_finished(
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
/// attached skill bundle to disk via
/// [`crate::materialise::materialise_for_agent`]. Returns the `*_HOME` env var
/// (name, path) the runner must forward so a home-style provider
/// (claude/codex/cursor) discovers the skills under the task root, or `None`
/// when there is no env pointer (no skills, or an in-workdir provider).
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

/// Compile-on-dispatch (P5, D16): if a profile master matches this task's agent
/// slug, materialise its resolved tool-native files into the task's execution
/// env and return the `*_HOME` env pointer the runner forwards. `None` when the
/// agent has no matching profile master on disk, the provider has no profile
/// target, or any read/write faults.
///
/// The profile slug is resolved as the agent's name (D16: the board-assignee
/// slug *is* the profile slug; an agent named for its profile picks that master
/// up). Best-effort by design — a missing / unparseable master, or a
/// materialise fault, is logged and swallowed so a task always dispatches.
async fn materialise_agent_profile(
    pool: &SqlitePool,
    task: &Task,
    env: &crate::execenv::ExecEnv,
    provider: &str,
) -> Option<(String, PathBuf)> {
    use ainb_hangar_store::repo::agent::AgentRepo;

    let slug = match AgentRepo::get(pool, &task.agent_id).await {
        Ok(Some(agent)) => agent.name,
        Ok(None) => return None,
        Err(e) => {
            tracing::warn!(error = %e, task_id = %task.id, "profile compile: agent resolve failed; skipping");
            return None;
        }
    };
    let dir = crate::profile::profiles_dir()?;
    let master = match crate::profile::read_master(&dir, &slug) {
        Ok(Some(m)) => m,
        // No master for this agent slug is the common case (not every agent has a
        // profile) — silently skip.
        Ok(None) => return None,
        Err(e) => {
            tracing::warn!(error = %e, task_id = %task.id, slug = %slug, "profile compile: master unreadable; skipping");
            return None;
        }
    };
    match crate::profile::materialise_profile(&master, provider, env.root()) {
        Ok(report) => {
            if report.files_written > 0 {
                tracing::info!(
                    task_id = %task.id,
                    slug = %report.slug,
                    provider = %provider,
                    files = report.files_written,
                    warnings = report.warnings.len(),
                    "profile compiled on dispatch"
                );
            }
            report.home_env
        }
        Err(e) => {
            tracing::warn!(error = %e, task_id = %task.id, slug = %slug, "profile compile failed; proceeding without profile");
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
    /// The agent's per-agent env (`agent_env`), layered onto the child env
    /// after the deny-by-default ambient allowlist.
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

/// Build the provider [`RunLocation`](crate::runner::RunLocation) for a
/// provisioned run workdir (F5).
///
/// A worktree / scratch run executes OUTSIDE the task tree, so its dir is both
/// the cwd and an extra sandbox write-root (the confinement must be widened to
/// it); the in-tree fallback needs no widening (it is already under the sandbox
/// base).
fn run_location_for(run_wd: &crate::workdir_provision::RunWorkdir) -> crate::runner::RunLocation {
    use crate::workdir_provision::RunWorkdir;
    match run_wd {
        RunWorkdir::Worktree { path, .. } | RunWorkdir::Scratch { path } => {
            crate::runner::RunLocation { cwd: path.clone(), extra_root: Some(path.clone()) }
        }
        RunWorkdir::Fallback { path } => {
            crate::runner::RunLocation { cwd: path.clone(), extra_root: None }
        }
    }
}

/// Record the run's worktree branch on the task row when the run produced
/// commits (tcp T2), so the board card + task detail surface it WITHOUT a git
/// query at render time.
///
/// Only a [`RunWorkdir::Worktree`] whose `ainb/<slug>` branch is ahead of its
/// base (the agent left commits) is recorded — that branch survives teardown
/// (`git worktree remove` keeps it), so it is the durable artifact worth
/// surfacing. A no-commit run, a scratch / in-tree run, or a git hiccup records
/// nothing, so a NULL `branch` cleanly means "no commits, nothing to show".
///
/// Called BEFORE teardown (the branch/worktree still exist) and BEFORE the
/// terminal `TaskFinished` event, so a subscribed board re-pulling `tasks_list`
/// on that event already sees the recorded branch. Best-effort: a store or git
/// fault is logged, never propagated onto a finalize that has already committed
/// the task's terminal state.
async fn persist_run_branch(
    pool: &SqlitePool,
    task_id: &str,
    run_wd: &crate::workdir_provision::RunWorkdir,
) {
    let crate::workdir_provision::RunWorkdir::Worktree { branch, .. } = run_wd else {
        return;
    };
    match crate::workdir_provision::commits_ahead(run_wd) {
        Ok(0) => {}
        Ok(n) => match TaskRepo::set_branch(pool, task_id, branch).await {
            Ok(_) => {
                tracing::info!(task_id = %task_id, branch = %branch, commits = n, "run branch recorded");
            }
            Err(e) => tracing::warn!(task_id = %task_id, error = %e, "branch record failed"),
        },
        Err(e) => {
            tracing::warn!(task_id = %task_id, error = %e, "commits-ahead check failed; branch not recorded");
        }
    }
}

/// Tear down a run's provisioned worktree after the task finalises (F5
/// keep-if-dirty): a clean worktree is removed + deregistered, a dirty one is
/// kept (uncommitted agent work preserved), scratch + fallback are no-ops.
///
/// Best-effort — a teardown fault is logged, never propagated onto a finalize
/// that has already committed the task's terminal state.
fn teardown_workdir(run_wd: &crate::workdir_provision::RunWorkdir, task_id: &str) {
    match crate::workdir_provision::teardown(run_wd) {
        Ok(outcome) => tracing::info!(task_id = %task_id, ?outcome, "run workdir torn down"),
        Err(e) => tracing::warn!(task_id = %task_id, error = %e, "run workdir teardown failed"),
    }
}

/// Look up a workspace's slug by id.
pub(crate) async fn workspace_slug(pool: &SqlitePool, workspace_id: &str) -> anyhow::Result<String> {
    let row = sqlx::query("SELECT slug FROM workspace WHERE id = ?")
        .bind(workspace_id)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| anyhow::anyhow!("workspace {workspace_id} not found"))?;
    Ok(row.try_get::<String, _>("slug")?)
}

/// Look up a workspace's configured `context_prompt` by id (e38.21).
///
/// `None` when the workspace has no prompt configured (the migration-0020 NULL
/// default) — the dispatch path then writes no `CLAUDE.md`. A missing workspace
/// resolves to `None` here (the dispatch already validated the row via
/// [`workspace_slug`]); the prompt injection is best-effort, never a dispatch
/// blocker.
async fn workspace_context_prompt(
    pool: &SqlitePool,
    workspace_id: &str,
) -> anyhow::Result<Option<String>> {
    let prompt: Option<String> =
        sqlx::query_scalar("SELECT context_prompt FROM workspace WHERE id = ?")
            .bind(workspace_id)
            .fetch_optional(pool)
            .await?
            .flatten();
    Ok(prompt)
}

/// Persist the provider `session_id` onto the task row (best-effort; only when
/// a session was actually opened).
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

/// Read the task row's `started_at` (epoch ms) from the DB — the run-start
/// stamp `StartTaskService::start` wrote, which the claim-time in-memory `Task`
/// predates (P10 / D19). `None` when the row is missing or never started, or on
/// a read fault (the run-history duration then degrades to 0 rather than
/// erroring).
async fn read_started_at(pool: &SqlitePool, task_id: &str) -> Option<i64> {
    sqlx::query_scalar::<_, Option<i64>>("SELECT started_at FROM agent_task_queue WHERE id = ?")
        .bind(task_id)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten()
        .flatten()
}

/// Record the run's token/cost usage into the `task_usage` table (e38.35), so
/// the usage dashboard can roll it up. Best-effort and only when the provider
/// actually reported usage — a run with no result-usage records nothing.
///
/// Called from both the success and failure finalize paths: a failed or
/// timed-out run can still report partial usage worth accounting. The upsert is
/// keyed by `task_id`, so a retry replaces rather than double-counts. A usage
/// write fault is logged, never propagated — it must never down a finalize that
/// has already committed the task's terminal state.
async fn persist_usage(
    pool: &SqlitePool,
    task: &Task,
    usage: Option<&crate::runner::ProviderUsage>,
    clock: &dyn HangarClock,
) {
    let Some(u) = usage else { return };
    let row = ainb_hangar_store::repo::usage::NewUsage {
        task_id: task.id.clone(),
        workspace_id: task.workspace_id.clone(),
        agent_id: task.agent_id.clone(),
        input_tokens: u.input_tokens,
        output_tokens: u.output_tokens,
        cost_usd: u.cost_usd,
        created_at: clock.now_ms(),
    };
    if let Err(e) = ainb_hangar_store::repo::usage::UsageRepo::record(pool, &row).await {
        tracing::warn!(error = %e, task_id = %task.id, "usage record failed");
    }
}

/// Append a durable `run_history` row for a finished run (P10 / D19) AND emit
/// the OTLP `task.run` boundary span carrying the run's token / cost / duration
/// attributes.
///
/// Called from both finalize paths (`outcome` = `success` | `failed`). Unlike
/// [`persist_usage`] (a per-task upsert), this is APPEND-ONLY — a fresh
/// `run_id` is minted per run, so a retried task appends a second history row
/// rather than overwriting the first. `diff_add` / `diff_del` are 0 until the
/// runner surfaces a diff stat.
///
/// The `task.run` span is always emitted as a `tracing` span (so it lands in
/// the JSONL sink); under the `otlp` feature with a configured endpoint it is
/// ALSO exported over OTLP (T5) — the span's `tokens_in` / `tokens_out` /
/// `cost_usd` / `duration_ms` fields become OTLP span attributes. It is opened
/// + immediately closed (entered then dropped) so the batch exporter flushes
/// it. A history write fault is logged, never propagated — it must never down a
/// finalize that has already committed the task's terminal state.
async fn record_run_history(
    pool: &SqlitePool,
    task: &Task,
    provider: &str,
    session_id: Option<&str>,
    usage: Option<&crate::runner::ProviderUsage>,
    outcome: &str,
    clock: &dyn HangarClock,
) {
    let run_id = SystemIdGen.new_ulid();
    let finished_at = clock.now_ms();
    let (input_tokens, output_tokens, cost_usd) = usage.map_or((0, 0, 0.0), |u| {
        (u.input_tokens, u.output_tokens, u.cost_usd)
    });
    // The in-memory `task` was read at claim time (before `StartTaskService::start`
    // stamped `started_at`), so its `started_at` is stale `None`. Read the live
    // DB value so the run's duration is real; fall back to the (stale) struct
    // value, then to `finished_at` (0 duration) if even the row has none.
    let started_at = read_started_at(pool, &task.id).await.or(task.started_at);
    // Duration from the run's start (never the queued-at time); 0 when no start
    // was recorded (defensive — the finalize seam always has one).
    let duration_ms = started_at.map_or(0, |s| finished_at.saturating_sub(s));

    // OTLP task->run boundary span (T5 / D19). The block scopes the span guard so
    // it is entered then dropped immediately, closing the span for the batch
    // exporter. Purely additive to the JSONL sink — a no-op export when OTLP is
    // unconfigured.
    {
        let span = tracing::info_span!(
            "task.run",
            task_id = %task.id,
            run_id = %run_id,
            provider = provider,
            outcome = outcome,
            tokens_in = input_tokens,
            tokens_out = output_tokens,
            cost_usd = cost_usd,
            duration_ms = duration_ms,
        );
        let _enter = span.enter();
    }

    let row = ainb_hangar_store::repo::run_history::NewRunHistory {
        run_id,
        task_id: Some(task.id.clone()),
        workspace_id: task.workspace_id.clone(),
        session_id: session_id.map(str::to_string),
        provider: provider.to_string(),
        profile: None,
        started_at,
        finished_at,
        outcome: outcome.to_string(),
        input_tokens,
        output_tokens,
        cost_usd,
        diff_add: 0,
        diff_del: 0,
    };
    if let Err(e) = ainb_hangar_store::repo::run_history::RunHistoryRepo::record(pool, &row).await {
        tracing::warn!(error = %e, task_id = %task.id, "run history record failed");
    }
}

/// Load the env-allowlist policy from
/// `~/.agents-in-a-box/hangar/env.allow.toml`.
///
/// Falls back to the operator-default policy (the 12 built-ins + hardcoded
/// deny) if the path can't be resolved or the file is unreadable — a
/// missing/corrupt config must never down a daemon dispatch.
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

/// Resolve the BARE home root the per-task env tree is appended to.
///
/// NOTE: this intentionally does NOT use [`ainb_hangar_core::hangar_home`].
/// That helper resolves the *Hangar home* (`$AINB_HANGAR_HOME` verbatim, else
/// `~/.agents-in-a-box`) — the dir that DIRECTLY holds `hangar.db`. The env
/// tree, by contrast, is rooted at `{bare_home}/.agents-in-a-box/hangar/
/// workspaces/...` ([`crate::execenv::prepare_env`] + the
/// `tripwire_skill_import_and_dispatch` contract append the `.agents-in-a-box`
/// segment themselves). So this returns the *bare* home: `$AINB_HANGAR_HOME`
/// verbatim when set, else the user's home WITHOUT the `.agents-in-a-box`
/// segment. Routing it through the shared helper would double the segment on
/// the default path (`~/.agents-in-a-box/.agents-in-a-box/...`).
///
/// Falls back to the current directory only when the home itself cannot be
/// resolved, preserving the daemon's prior infallible contract here (the env
/// tree is best-effort; a missing home must not abort the loop).
pub(crate) fn hangar_home() -> PathBuf {
    std::env::var_os(ainb_hangar_core::paths::HANGAR_HOME_ENV)
        .filter(|p| !p.is_empty())
        .map_or_else(
            || dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")),
            PathBuf::from,
        )
}

/// Warn about `danger-full-access` on the first invocation of `provider` in
/// this task's session (P5.6). Best-effort: a `state.toml` path-resolution or
/// IO fault is logged and swallowed — a warning bookkeeping failure must never
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The shutdown-reap tracker records a live interactive session and hands
    /// it back exactly once on drain, so `Ctrl-C` kills every live session
    /// — and only live ones — by exact name.
    #[test]
    fn interactive_sessions_registers_then_drains_once() {
        let sessions = InteractiveSessions::default();
        sessions.register("tmux_hangar-a");
        sessions.register("tmux_hangar-b");

        let mut drained = sessions.drain();
        drained.sort();
        assert_eq!(drained, vec!["tmux_hangar-a", "tmux_hangar-b"]);

        // Drain is destructive: a second drain (a double shutdown signal) yields
        // nothing, so no session is killed twice.
        assert!(sessions.drain().is_empty());
    }

    /// A session whose run finished naturally is unregistered, so the shutdown
    /// reap never tries to kill an already-gone session (and the set stays
    /// bounded across a long-lived daemon).
    #[test]
    fn interactive_sessions_unregister_drops_a_finished_session() {
        let sessions = InteractiveSessions::default();
        sessions.register("tmux_hangar-live");
        sessions.register("tmux_hangar-done");
        sessions.unregister("tmux_hangar-done");

        assert_eq!(sessions.drain(), vec!["tmux_hangar-live"]);
    }

    /// The tracker is cheap to clone (an `Arc`), and clones share ONE set — a
    /// session registered through a spawned execution's clone is visible to the
    /// run loop's clone for the shutdown reap.
    #[test]
    fn interactive_sessions_clone_shares_one_set() {
        let a = InteractiveSessions::default();
        let b = a.clone();
        b.register("tmux_hangar-shared");
        // The loop's handle (`a`) sees the session the execution's handle (`b`)
        // registered.
        assert_eq!(a.drain(), vec!["tmux_hangar-shared"]);
        // And it is now gone from both views.
        assert!(b.drain().is_empty());
    }

    /// An empty tracker drains to nothing — an idle daemon's shutdown reap is a
    /// no-op (kills no sessions).
    #[test]
    fn interactive_sessions_empty_drains_to_nothing() {
        assert!(InteractiveSessions::default().drain().is_empty());
    }
}
