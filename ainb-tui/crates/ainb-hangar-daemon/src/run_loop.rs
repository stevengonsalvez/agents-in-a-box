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
//! | `HANGAR_SPAWN_SETUP_TIMEOUT_MS` | running→spawn setup-phase umbrella override (tests) | `60000` |
//! | `HANGAR_SWEEP_DISPATCHED_TTL_MS` | dispatch TTL override (tests) | reference default |
//! | `HANGAR_DAEMON_DISABLE_CLAIM` | skip the claim loop, run sweepers only (tests) | unset |
//! | `HANGAR_DAEMON_DISABLE_SANDBOX` | `1` forces providers UNCONFINED (security downgrade); `0` forces the OS sandbox ON | unset (platform default: ON on Linux, OFF on macOS) |
//!
//! When `HANGAR_DAEMON_RUNTIME_ID` is unset the claim loop is a no-op (the
//! daemon still sweeps) — a daemon with no runtime has nothing to claim.

// The provider runtime deadline is a "hours" quantity but `Duration::from_hours`
// is unstable; a raw second count is the clearest stable spelling (and matches
// the reference running-TTL value). Same rationale as `sweeper.rs`.
#![allow(clippy::duration_suboptimal_units)]

use std::collections::HashSet;
use std::path::{Path, PathBuf};
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
use crate::runner::{Backend, Mode, ProviderInvocation, RunOutcome, Runner, RunnerConfig};
use crate::sweeper::{
    SweeperConfig, reclaim_orphaned_on_startup, sweep_expired_queued, sweep_stale_dispatched,
    sweep_stale_running,
};

/// Default claim-poll interval when `HANGAR_DAEMON_POLL_MS` is unset.
const DEFAULT_POLL_MS: u64 = 1_000;
/// How long the daemon waits for the claude credential read before giving up.
///
/// The legacy macOS `SecKeychain` read presents a BLOCKING GUI auth prompt when
/// the calling binary is absent from the keychain item's ACL trusted-app list
/// (e.g. a rebuilt debug binary whose signature invalidated the ACL). On a
/// headless daemon that prompt is never answered, so a synchronous read wedges
/// the async worker forever and the task freezes at `running`. Bounding the read
/// converts that indefinite hang into a clean "dispatch without a token" fallback.
const CRED_READ_TIMEOUT: Duration = Duration::from_secs(5);
/// Umbrella bound on the ENTIRE `running -> provider spawn` setup phase (doctrine
/// hardening, D-e2e-3).
///
/// [`CRED_READ_TIMEOUT`] bounds only the keychain read; this bounds EVERY await
/// between the `dispatched -> running` commit and the provider spawn as ONE unit:
/// the `running` board auto-move + the "started" progress comment (both DB
/// writes) and [`prepare_spawn_inputs`] (env build, cred read, skills/profile
/// materialise). No await in that span — a wedged DB write, a future blocking
/// call added here, a pool deadlock, a materialise hang — can freeze a `running`
/// row forever: on expiry the run is terminalised `running -> failed`
/// ([`FailureReason::SpawnTimeout`]) with the real cause logged, rather than left
/// to the multi-hour running-TTL sweep. Defense-in-depth behind the per-step
/// bounds: the sanctioned slow step is the 5s cred read, so 60s means "genuinely
/// wedged", never "merely slow". Overridable via [`spawn_setup_timeout`].
const SPAWN_SETUP_TIMEOUT: Duration = Duration::from_secs(60);

/// The active setup-phase umbrella bound, honouring the test-only
/// `HANGAR_SPAWN_SETUP_TIMEOUT_MS` override (mirrors `HANGAR_PROVIDER_MAX_RUNTIME_MS`)
/// so an e2e / unit test can drive the wedge terminalise within a bounded budget.
/// Defaults to [`SPAWN_SETUP_TIMEOUT`].
fn spawn_setup_timeout() -> Duration {
    env_u64_opt("HANGAR_SPAWN_SETUP_TIMEOUT_MS").map_or(SPAWN_SETUP_TIMEOUT, Duration::from_millis)
}
/// Provider runtime deadline (reference running TTL: 2.5h).
const PROVIDER_MAX_RUNTIME: Duration = Duration::from_secs(9_000);
/// Trailing stdout/stderr lines retained on each run for the audit tail.
/// The brief used when a task has neither an issue nor agent instructions.
///
/// Never empty: a provider spawned with no prompt starts an interactive session
/// against the daemon's null stdin and exits non-zero without working.
const FALLBACK_PROMPT: &str = "Review the repository in your working directory and \
     continue the work described in its context files (e.g. CLAUDE.md).";
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
    /// Path to the `copilot` provider binary (GitHub Copilot CLI).
    pub copilot_path: PathBuf,
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
    ///
    /// The env-unset default is **platform-specific** (see [`Self::default_sandbox`]):
    /// ON where the sandbox primitive can actually boot a Node-based provider
    /// CLI (Linux/Landlock), OFF on macOS — where the default-on Seatbelt profile
    /// kills every headless `claude` task before it writes a transcript (exit 65
    /// in ~825ms even with the credential injected), making headless dispatch
    /// non-functional. That matches the interactive path, which is already
    /// deliberately unsandboxed. `HANGAR_DAEMON_DISABLE_SANDBOX` is the explicit
    /// override in both directions: `=1` forces it OFF (security downgrade),
    /// `=0` forces it ON (used to exercise the profile on macOS). The resolved
    /// posture is logged at daemon startup (see [`log_sandbox_posture`]).
    pub sandbox: bool,
}

impl DaemonConfig {
    /// Build the config from the process environment (see the module table).
    #[must_use]
    pub fn from_env() -> Self {
        // A fresh home now claims for the stable default runtime even with no
        // env override: the boot seed registers that runtime + a starter agent
        // bound to it, so the claim loop (run_loop.rs, skipped when `None`) is
        // enabled out of the box. `HANGAR_DAEMON_RUNTIME_ID` still overrides.
        let runtime_id = Some(ainb_hangar_store::bootstrap::default_runtime_id());
        // Resolve each provider path to an absolute, symlink-canonicalized
        // binary ONCE here, before it ever reaches `RunnerConfig` and the
        // sandbox profile generator. A bare default (`claude`/`codex`/`copilot`)
        // is otherwise emitted into the Seatbelt profile as a meaningless
        // `(literal "claude")` rule that the kernel never matches, so the OS
        // sandbox denies exec of the real PATH-resolved binary (e.g. a
        // `~/.local/bin/claude` symlink outside every system read root) and the
        // task finalizes `failed` in milliseconds with an empty transcript.
        let claude_path = resolve_provider_path(
            std::env::var_os("HANGAR_CLAUDE_PATH")
                .map_or_else(|| PathBuf::from("claude"), PathBuf::from),
            "claude",
        );
        let codex_path = resolve_provider_path(
            std::env::var_os("HANGAR_CODEX_PATH")
                .map_or_else(|| PathBuf::from("codex"), PathBuf::from),
            "codex",
        );
        let copilot_path = resolve_provider_path(
            std::env::var_os("HANGAR_COPILOT_PATH")
                .map_or_else(|| PathBuf::from("copilot"), PathBuf::from),
            "copilot",
        );
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
        // e38.23 / hangar-e2e-4: the headless OS sandbox posture is the platform
        // default (ON on Linux, OFF on macOS) unless the env var overrides it.
        let sandbox =
            Self::resolve_sandbox(std::env::var_os("HANGAR_DAEMON_DISABLE_SANDBOX").as_deref());

        Self {
            runtime_id,
            claude_path,
            codex_path,
            copilot_path,
            poll_interval,
            provider_max_runtime,
            sweeper,
            disable_claim,
            sandbox,
        }
    }

    /// Resolve the headless OS FS sandbox posture from the explicit
    /// `HANGAR_DAEMON_DISABLE_SANDBOX` override value (`None` when unset).
    ///
    /// `Some("1")` forces the sandbox OFF (the documented security opt-out);
    /// `Some("0")` forces it ON (needed to exercise the profile on macOS, where
    /// it is otherwise off by default); any other value — or unset — falls back
    /// to [`Self::default_sandbox`]. Split out as a pure function so the
    /// override precedence is testable without mutating process env.
    fn resolve_sandbox(override_val: Option<&std::ffi::OsStr>) -> bool {
        match override_val {
            Some(v) if v == "1" => false,
            Some(v) if v == "0" => true,
            _ => Self::default_sandbox(),
        }
    }

    /// The env-unset default headless sandbox posture for this platform.
    ///
    /// OFF on macOS: the default-on Seatbelt profile ([`ainb_hangar_sandbox`])
    /// cannot boot a Node-based provider CLI — every headless `claude` task dies
    /// exit 65 in ~825ms before writing a transcript, even with the credential
    /// injected — so leaving it on makes headless dispatch non-functional. The
    /// interactive path is already deliberately unsandboxed. ON everywhere else
    /// (Linux/Landlock), where the profile runs the CLI fine.
    #[cfg(target_os = "macos")]
    const fn default_sandbox() -> bool {
        false
    }

    /// The env-unset default headless sandbox posture: ON on Linux/Landlock,
    /// which can run a Node-based provider CLI under confinement. See the
    /// macOS variant for why that platform defaults OFF.
    #[cfg(not(target_os = "macos"))]
    const fn default_sandbox() -> bool {
        true
    }
}

/// Resolve a provider binary to an absolute, symlink-canonicalized path once at
/// daemon startup, before it flows into [`RunnerConfig`] and the sandbox profile
/// generator ([`ainb_hangar_sandbox`]).
///
/// A bare name (no path separator, the default `claude`/`codex`/`copilot`) is
/// located on `$PATH`; an explicit `HANGAR_*_PATH` override is honored as given.
/// Either way the result is canonicalized so the Seatbelt/Landlock profile
/// references the real binary the OS will exec (e.g. a `~/.local/bin/claude`
/// symlink into `~/.local/share/claude/versions/…`), which no system read root
/// covers. If a bare name resolves nowhere on `$PATH`, falls back to the bare
/// name and logs a warning so the otherwise-silent sandbox denial is
/// diagnosable rather than a 40ms `agent_error` with an empty transcript.
fn resolve_provider_path(raw: PathBuf, provider: &str) -> PathBuf {
    let is_bare = raw.parent() == Some(Path::new(""));
    let located = if is_bare {
        match ainb_hangar_sandbox::find_on_path(&raw) {
            Some(found) => found,
            None => {
                tracing::warn!(
                    provider,
                    name = %raw.display(),
                    "provider binary not found on PATH; the OS sandbox will likely deny \
                     exec; set the HANGAR_*_PATH override to an absolute binary path"
                );
                raw
            }
        }
    } else {
        raw
    };
    match std::fs::canonicalize(&located) {
        Ok(abs) => abs,
        Err(e) => {
            tracing::warn!(
                provider,
                path = %located.display(),
                error = %e,
                "failed to canonicalize provider path; using as given"
            );
            located
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

/// Emit the daemon's resolved headless OS-sandbox posture at INFO, once at boot.
///
/// `sandbox = true` means every headless provider subprocess is confined in the
/// OS FS sandbox (Landlock/Seatbelt); `false` means the passthrough (unconfined)
/// path. `target_os` records which platform default (or env override) produced
/// it. This is the single startup diagnostic that disambiguates an exit-65
/// dispatch failure between "confinement on, still failing" and "stale binary".
/// Factored out of [`run`] so it is unit-testable without booting the daemon.
fn log_sandbox_posture(cfg: &DaemonConfig) {
    tracing::info!(
        sandbox = cfg.sandbox,
        target_os = std::env::consts::OS,
        "headless provider sandbox posture"
    );
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
    // hangar-e2e-5: record the resolved headless OS-sandbox posture once at boot.
    // Without this line an exit-65 dispatch failure was ambiguous between "fix
    // present, still failing" and "stale binary (fix absent)"; the confinement
    // posture is the missing diagnostic. Emitted before any task runs so triage
    // (and the e2e harness) can assert `sandbox=false` up front.
    log_sandbox_posture(&cfg);
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

    let Some(runtime_id) = cfg.runtime_id.clone().filter(|_| !cfg.disable_claim) else {
        tracing::info!(claim = false, "claim loop disabled; sweepers only");
        tokio::signal::ctrl_c().await?;
        tracing::info!("ainb-hangar-daemon shutting down");
        return Ok(());
    };

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
        copilot_path: cfg.copilot_path.clone(),
        max_runtime: cfg.provider_max_runtime,
        tail_lines: TAIL_LINES,
        // e38.23: confine every provider spawn in the OS-level FS sandbox per
        // the platform default (ON on Linux, OFF on macOS). Overridable via
        // `HANGAR_DAEMON_DISABLE_SANDBOX` (see `DaemonConfig::from_env`).
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
    // The platform credential store, built ONCE for the daemon's lifetime and
    // shared across concurrent runs (one Keychain session, not one per task).
    // `execute_claimed` takes it as a trait object, so a test can inject the
    // in-memory double instead of touching the real Keychain.
    let secrets = crate::claude_cred::default_backend();

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
                let secrets = secrets.clone();
                runs.spawn(async move {
                    let clock = SystemClock;
                    if let Err(e) =
                        execute_claimed(&pool, &runner, &claimed, &clock, &stats, &events, &interactive, secrets)
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

/// Resolve the claude credential env for `backend`, bounded and off the async
/// worker.
///
/// The keychain read ([`claude_cred::keys_for_backend`] →
/// `MacKeychainBackend::get`) is a synchronous, unbounded external call that can
/// present a BLOCKING GUI auth prompt on a headless daemon (legacy `SecKeychain`
/// ACL trust invalidated by a rebuilt binary). Answered by nobody, it wedges the
/// calling async worker forever, freezing the task at `running` (the
/// zombie-dispatch defect). We run it on [`tokio::task::spawn_blocking`] (same
/// pattern as [`cap_parent_inbox`]) and race it against `timeout`.
///
/// On timeout OR a join error we log a clear warning and return an EMPTY env, so
/// the dispatch proceeds without an injected `CLAUDE_CODE_OAUTH_TOKEN`: the run
/// then either succeeds (env-override / no-ACL path) or reaches claude and fails
/// loudly + actionably (the `finalize_failure` seam turns that into a terminal
/// `FAILED`) instead of hanging at `running` indefinitely.
///
/// The `info!` on entry and exit close the ~29-min silent span that made the
/// original hang un-observable (the black hole was itself a defect).
async fn resolve_cred_env(
    backend: Backend,
    secrets: Arc<dyn ainb_hangar_secrets::SecretBackend + Send + Sync>,
    daemon_env: std::collections::HashMap<String, String>,
    timeout: Duration,
) -> Vec<(String, String)> {
    tracing::info!(backend = backend.name(), "resolving claude credential");
    let read = tokio::task::spawn_blocking(move || {
        crate::claude_cred::keys_for_backend(backend, secrets.as_ref(), &daemon_env)
    });
    match tokio::time::timeout(timeout, read).await {
        Ok(Ok(env)) => {
            tracing::info!(injected = env.len(), "claude credential resolved");
            env
        }
        Ok(Err(e)) => {
            tracing::warn!(
                error = %e,
                "keychain credential read task failed; dispatching without injected CLAUDE_CODE_OAUTH_TOKEN"
            );
            Vec::new()
        }
        Err(_) => {
            tracing::warn!(
                timeout_secs = timeout.as_secs(),
                "keychain credential read timed out; dispatching without injected CLAUDE_CODE_OAUTH_TOKEN"
            );
            Vec::new()
        }
    }
}

/// Build the provider's child env + resolved credential for a `running` task —
/// the env / cred / skills-materialise portion of the pre-spawn preamble. Its
/// caller runs it INSIDE the [`SPAWN_SETUP_TIMEOUT`] umbrella (alongside the
/// started-side DB writes) so a wedge in any of these steps terminalises the run
/// rather than freezing it; see [`execute_claimed`].
///
/// Returns `(task_env, cred_env)`: the allowlist-filtered, skills/profile-augmented
/// child env, and the daemon-resolved claude credential that rides `extra_env`
/// (claude children only). Every step is best-effort — a materialise or cred
/// fault degrades the run (no skills / no token) but never fails it. The hard
/// guarantee is the caller's timeout, which turns a WEDGED step (not a slow one)
/// into a terminal `spawn_timeout` instead of a forever-`running` row.
async fn prepare_spawn_inputs(
    pool: &SqlitePool,
    task: &Task,
    env: &crate::execenv::ExecEnv,
    backend: Backend,
    secrets: Arc<dyn ainb_hangar_secrets::SecretBackend + Send + Sync>,
    cred_timeout: Duration,
) -> (
    std::collections::HashMap<String, String>,
    Vec<(String, String)>,
) {
    // Snapshot the daemon's env into an owned map *before* any await: the
    // `std::env::Vars` iterator is `!Send`, so holding it across `.await` would
    // make this future non-`Send`. P5.3: apply the configurable env-allowlist
    // policy here (the authoritative pass), layering keychain-resident API keys
    // on top via `dispatch::build_task_env`.
    let daemon_env: std::collections::HashMap<String, String> = std::env::vars().collect();
    let policy = load_env_policy();
    let mut task_env = crate::dispatch::build_task_env(&daemon_env, std::iter::empty(), &policy);

    // The daemon-resolved claude credential rides `extra_env` (the unfiltered
    // append), NOT `task_env` — so it bypasses the deny-by-default allowlist and
    // reaches a claude child only (`keys_for_backend` is empty for codex/copilot).
    // The read is bounded + moved off the async worker (`resolve_cred_env`): a
    // legacy keychain GUI auth prompt on a headless daemon would otherwise wedge
    // this future and freeze the task at `running`. On timeout we proceed with NO
    // token so the run reaches claude and fails loudly rather than hanging.
    let cred_env = resolve_cred_env(backend, secrets, daemon_env.clone(), cred_timeout).await;

    // ccc / D11: name the hangar daemon as the child's parent session so the run
    // is a legitimate fleet member (its AskUserQuestion reaches the attention
    // pipeline). The runner allowlists this key so it survives the deny-by-default
    // filter; set AFTER `build_task_env` so the daemon's value wins over any
    // ambient one.
    task_env.insert(
        ainb_fleet_core::session_registry::PARENT_ENV.to_string(),
        HANGAR_PARENT_SESSION.to_string(),
    );

    // P6.4: materialise the agent's attached skills into the provider's layout,
    // forwarding the `*_HOME` pointer via `task_env`. Non-fatal — a task must
    // still dispatch even if a skill bundle cannot be written.
    if let Some((key, path)) = materialise_skills(pool, task, env).await {
        task_env.insert(key, path.to_string_lossy().into_owned());
    }

    // P5 (D16): compile-on-dispatch — if a profile master matches this task's
    // agent slug, materialise its resolved tool-native files and forward the same
    // `*_HOME` pointer (idempotent when both wrote one). Non-fatal.
    if let Some((key, path)) = materialise_agent_profile(pool, task, env, backend.name()).await {
        task_env.insert(key, path.to_string_lossy().into_owned());
    }

    // P5.6: warn about `danger-full-access` on the first invocation of this
    // provider in this session. Non-fatal — never block a dispatch on it.
    warn_danger_access(task, backend.name());

    (task_env, cred_env)
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
// One over the lint's 7: the secret backend joins the existing seven collaborators
// (pool, runner, task, clock, stats, events, interactive) as a thin DI handle.
// Bundling them into a context struct is a larger refactor than this credential
// change warrants; the sibling run functions here carry the same shape.
#[allow(clippy::too_many_arguments)]
/// The claim-time squad-leader briefing injection seam (migration 0045 / GAPS #1).
///
/// When a claimed task carries a `squad_id`, the daemon will inject the leader
/// briefing (Operating Protocol + Roster + Instructions) into the run's system
/// instructions here. gap #5 lands the HOOK POINT + the dispatch-log evidence;
/// the briefing BODY is gap #1 — this returns `None` (no text yet) but logs that
/// the injection point fired, keyed off the `squad_id`, so the seam is observable.
fn squad_briefing_hook(task: &Task) -> Option<String> {
    let squad_id = task.squad_id.as_deref()?;
    tracing::info!(
        task_id = %task.id,
        squad_id = %squad_id,
        "squad briefing hook: leader-briefing injection point (body: gap #1)"
    );
    None // gap #1 fills this with buildSquadLeaderBriefing()
}

async fn execute_claimed(
    pool: &SqlitePool,
    runner: &Runner,
    claimed: &ClaimedTask,
    clock: &dyn HangarClock,
    stats: &HealthStats,
    events: &EventSink,
    interactive: &InteractiveSessions,
    secrets: Arc<dyn ainb_hangar_secrets::SecretBackend + Send + Sync>,
) -> anyhow::Result<()> {
    let task: Task = TaskRepo::get_by_id(pool, &claimed.id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("claimed task {} vanished", claimed.id))?;
    // Pre-run setup faults (slug lookup / execenv prep / F5 provision below) must
    // TERMINALISE the still-`dispatched` task as failed rather than propagate: a
    // propagated setup error left the row `dispatched`, and the stale-dispatch
    // sweeper reclaimed + re-dispatched it into the SAME fault, looping invisibly
    // (the board card never leaves Todo, the detail never shows an error) until
    // the 5min dispatch TTL relabelled it `timeout` with no cause. See
    // `finalize_setup_failure`.
    let ws_slug = match workspace_slug(pool, &task.workspace_id).await {
        Ok(s) => s,
        Err(e) => return finalize_setup_failure(pool, &task, &e, clock, stats, events).await,
    };
    let home = hangar_home();
    let env = match prepare_env(&task, &ws_slug, &home, clock) {
        Ok(env) => env,
        Err(e) => return finalize_setup_failure(pool, &task, &e, clock, stats, events).await,
    };

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

    // Claim-time squad briefing injection point (migration 0045). Fires BEFORE
    // provider dispatch so the hook is exercised even when the run later fails to
    // spawn (a nonexistent provider path still exercises it).
    if let Some(briefing) = squad_briefing_hook(&task) {
        // gap #1: merge `briefing` into the run's system instructions / CLAUDE.md here.
        let _ = briefing;
    }

    // e38.16: resolve which provider exec path this task routes to (agent →
    // runtime → provider) and the per-agent config (model / cli_args / agent_env)
    // the runner threads into that provider's argv + env. A resolve fault falls
    // back to the default provider WITH the fallback prompt, so a misconfigured
    // agent still runs rather than stranding the task — the prompt is what makes
    // that true (see `ResolvedDispatch::fallback`). The fault is logged rather
    // than swallowed: silently substituting a different agent's behaviour is
    // exactly the kind of thing an operator needs to see.
    // The ONE place the task row's `mode` column becomes a `Mode`: it is resolved
    // here, carried on the `ResolvedDispatch`, and read from there by both the
    // branch that picks the exec path and the argv built for it — so the two can
    // never disagree.
    let mode = dispatch_mode(&task.mode);
    let dispatch =
        match resolve_dispatch(pool, &task.agent_id, task.issue_id.as_deref(), mode).await {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    task_id = %task.id,
                    agent_id = %task.agent_id,
                    "dispatch resolve failed; falling back to the default provider + prompt"
                );
                ResolvedDispatch::fallback(mode)
            }
        };

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
    let run_wd = match crate::workdir_provision::provision(
        task.repo_ref.as_deref(),
        &run_slug,
        &scratch_slug,
        &home,
        &env.workdir,
        task.source_branch.as_deref(),
    ) {
        Ok(wd) => wd,
        // F5 provision failed (e.g. the card's `repo_ref` could not be
        // worktree-added) while the row is still `dispatched`. Terminalise it as
        // failed with the real error instead of propagating — the propagate path
        // left it `dispatched` to be reclaimed + re-dispatched into the same fault
        // forever, invisible to the board/detail.
        Err(e) => return finalize_setup_failure(pool, &task, &e, clock, stats, events).await,
    };
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
    // next snapshot pull reconciles if no subscriber is connected). This is a
    // non-blocking channel push (not awaited), so it stays outside the umbrella.
    emit_task_started(events, &task, clock);

    // P10 / D19: the provider that executed this run, recorded on the run-history
    // row + the OTLP task->run span. Captured up front (a `&'static str`) so the
    // spawn-timeout terminalise below can attribute the failed run, and BEFORE
    // `provider_run` moves `dispatch` (the async block takes `dispatch.agent_env`
    // by value). The `cancel_guard` was registered up front (before the start).
    let provider = dispatch.backend.name();

    // Doctrine hardening (D-e2e-3): bound EVERY await between the `running` commit
    // and the provider spawn as ONE unit. `resolve_cred_env` already bounds the
    // keychain read (the known zombie-dispatch wedge), but the `running` board
    // auto-move + the "started" progress comment are ALSO post-`running` DB awaits
    // — a wedge in either (a pool deadlock, a contended writer) is the exact
    // forever-`running` black hole, merely relocated past the cred read. So the
    // umbrella opens the instant the row is `running` and closes only once the
    // spawn inputs are built: the two started-side DB writes AND
    // [`prepare_spawn_inputs`] all run inside it. On expiry we terminalise
    // `running -> failed` with the real cause logged, so a wedge ANYWHERE in the
    // span is a loud, immediate, terminal failure — never a silent forever-run.
    //
    // The two DB writes stay best-effort INSIDE the block (a write fault is
    // logged, never blocks the FSM); only an unbounded HANG is caught by the
    // timeout, which is exactly the wedge class this guards.
    let setup = tokio::time::timeout(spawn_setup_timeout(), async {
        // P4 / D8: auto-move the task's issue card into any board's `running`
        // auto-move column.
        crate::board::auto_move_after_transition(pool, &task, "running").await;
        // The default issue board buckets by `issue.state`, not the durable
        // `board_card` the auto-move touches — so also forward-advance the issue's
        // own lifecycle to `in_progress`, or a plain task's card strands in Todo
        // through its whole run. Advance-only + best-effort.
        crate::board::advance_issue_lifecycle_after_transition(pool, &task, "running").await;
        // e38.6: write a durable, agent-authored "started" comment to the task's
        // issue so the agent's activity survives beyond the bounded transcript
        // buffer. A NULL-issue chat task writes nothing.
        progress_comment::emit_checkpoint(
            pool,
            &SystemIdGen,
            clock,
            &task,
            progress_comment::Checkpoint::Started,
        )
        .await;
        prepare_spawn_inputs(
            pool,
            &task,
            &env,
            dispatch.backend,
            secrets.clone(),
            CRED_READ_TIMEOUT,
        )
        .await
    })
    .await;

    let (task_env, cred_env) = match setup {
        Ok(inputs) => inputs,
        Err(_elapsed) => {
            tracing::error!(
                task_id = %task.id,
                timeout_ms = u64::try_from(spawn_setup_timeout().as_millis()).unwrap_or(u64::MAX),
                "run setup wedged before provider spawn; failing task"
            );
            // running -> failed: type the terminal edge before the store finalize,
            // then flow through the SAME `finalize_failure` seam the provider-spawn
            // failure uses (teardown / run-history / event). `SpawnTimeout` is
            // `NoRetry`: a wedged setup will not self-heal on a re-dispatch.
            lifecycle.fire(crate::fsm::LifecycleEvent::Fail)?;
            finalize_failure(
                pool,
                &task,
                &run_wd,
                &env,
                ainb_hangar_store::service::fail::FailureReason::SpawnTimeout,
                crate::runner::RunnerResult::default(),
                provider,
                clock,
                stats,
                events,
            )
            .await?;
            return Ok(());
        }
    };

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
    // spawns a retry child). `provider` was captured up front (before the setup
    // umbrella) so both the timeout terminalise and this run attribute the same
    // backend.
    let provider_run = async {
        if mode == Mode::Interactive {
            // The interactive path is DELIBERATELY unsandboxed (see
            // `run_interactive`): the attached claude reaches the Keychain and
            // `~/.claude` natively and auto-refreshes, so it needs no injected
            // token. `cred_env` (headless-only, consumed in the `else` arm) is
            // intentionally not threaded here — a static `CLAUDE_CODE_OAUTH_TOKEN`
            // would override keychain auth and go stale mid-session.
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
                    .run_claude_in_with_env(
                        &env,
                        task_env,
                        cred_env,
                        &dispatch.invocation,
                        &location,
                    )
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
                Backend::Copilot => runner
                    .run_copilot_in(
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
            if mode == Mode::Interactive {
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
        res = provider_run => match res {
            Ok(outcome) => outcome,
            // The provider process could not be spawned / executed (the
            // configured `claude` / `codex` path does not resolve → ENOENT, or an
            // OS-level exec/wait fault). The task is already `running` here, so
            // propagating this error out of `execute_claimed` left the row frozen
            // `running` until the multi-hour running TTL sweep — a task that
            // already died sitting `running` indefinitely, well past any dispatch
            // budget. Convert it to a terminal FAILED outcome so it flows through
            // the SAME `finalize_failure` seam the agent-error path uses
            // (teardown / run-history / event / retry taxonomy), finalising the
            // row to `failed` immediately. `SpawnError` is `NoRetry`: a
            // misconfigured binary path will not self-heal on a re-dispatch. The
            // running-TTL sweeper stays as the backstop for a genuine crash that
            // never reaches this arm at all.
            Err(e) => {
                tracing::error!(task_id = %task.id, error = %e, "provider spawn/exec failed; failing task");
                RunOutcome::Failed {
                    reason: ainb_hangar_store::service::fail::FailureReason::SpawnError,
                    result: crate::runner::RunnerResult::default(),
                }
            }
        },
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
            finalize_failure(
                pool, &task, &run_wd, &env, reason, result, provider, clock, stats, events,
            )
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

/// The program + argv [`run_interactive`] spawns into the tmux pane.
///
/// Trivial by design, and extracted for exactly one reason: it is the ONLY place
/// the interactive argv is derived, and it is reachable from a test without
/// tmux, a provider binary, or a database. The mode comes off the
/// [`ResolvedDispatch`] the task row produced — this function has no mode of its
/// own to get wrong, which is the whole point.
///
/// The previous shape passed the mode at the call site inside `run_interactive`,
/// and its test re-derived the argv *beside* that call rather than through it.
/// Hardcoding `Mode::Headless` in `run_interactive` therefore kept 254 lib tests
/// and every integration test GREEN while reintroducing the exact regression this
/// module exists to fix — the only coverage was a tmux tripwire that SKIPs when
/// tmux is absent.
fn interactive_command(runner: &Runner, dispatch: &ResolvedDispatch) -> (PathBuf, Vec<String>) {
    runner.provider_command(dispatch.backend, &dispatch.invocation, dispatch.mode)
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
    //
    // That claim is only true because the argv below is built for `Mode::Interactive`
    // — a headless argv would make this a print-and-exit process wearing a tmux
    // session's clothes.
    let session_name = crate::interactive::session_name_for(&task.id);
    let (program, argv) = interactive_command(runner, dispatch);
    // Mirror the headless env composition: the codex / copilot paths layer the
    // agent's `agent_env`; the claude path layers nothing (parity with
    // `execute_claimed`).
    let extra_env = match dispatch.backend {
        Backend::Codex | Backend::Copilot => dispatch.agent_env.clone(),
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
    // Twin the durable-card move on the issue's own `state` (the default board
    // buckets by it): an aggregate-`done` set promotes the issue to `done`;
    // failed/cancelled sets leave it untouched. Advance-only + best-effort.
    crate::board::advance_and_cascade_child(pool, task, events).await;
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
/// Compose the human-readable `result`-column detail for a failed run from the
/// runner's captured output tails. Never returns nothing — every failure path
/// leaves a diagnosable `result`, closing the black hole a bare `fail` (no
/// stored detail) used to leave.
///
/// BOTH tails are folded, each under its own labelled section, when at least
/// one is present, because the two providers surface their terminal error on
/// different streams: `claude --output-format stream-json` writes the failing
/// `{"type":"result",...}` line to STDOUT, while a crashed CLI or a shell
/// wrapper writes to STDERR. Persisting only stderr (the previous behaviour)
/// therefore dropped exactly the evidence an exit-65 claude agent_error leaves
/// behind.
///
/// A zero-output death (e.g. a sandboxed CLI killed exit 65 before writing a
/// byte) leaves BOTH tails empty; that failure is synthesised from what we DO
/// know — the reason, exit code, and provider (e.g. "agent_error: provider
/// claude exit 65 with no output") — instead of storing `result = NULL` and
/// making the crash undiagnosable from the DB alone. Signal-killed runs have no
/// exit code.
fn failure_detail(
    reason: ainb_hangar_store::service::fail::FailureReason,
    provider: &str,
    exit_code: Option<i32>,
    stdout_tail: &str,
    stderr_tail: &str,
) -> String {
    let stdout = stdout_tail.trim();
    let stderr = stderr_tail.trim();
    if stdout.is_empty() && stderr.is_empty() {
        let exit = exit_code.map_or_else(
            || "no exit code (killed)".to_string(),
            |c| format!("exit {c}"),
        );
        return format!(
            "{}: provider {provider} {exit} with no output",
            reason.as_db_str()
        );
    }
    let mut detail = format!("run failed ({}):", reason.as_db_str());
    if !stdout.is_empty() {
        detail.push_str("\n\nstdout:\n");
        detail.push_str(stdout);
    }
    if !stderr.is_empty() {
        detail.push_str("\n\nstderr:\n");
        detail.push_str(stderr);
    }
    detail
}

/// hangar-e2e-6 escape hatch: when `HANGAR_KEEP_FAILED_RUNS=1` the daemon
/// PRESERVES a failed run's worktree + provider-log dir instead of tearing the
/// clean worktree down, so the e2e loop can inspect the transcript
/// (`{logs}/claude.jsonl`) of a zero-output failure. Off by default — production
/// keeps the keep-if-dirty teardown so a clean failed run never leaks disk.
fn keep_failed_runs() -> bool {
    std::env::var_os("HANGAR_KEEP_FAILED_RUNS").is_some_and(|v| v == "1")
}

async fn finalize_failure(
    pool: &SqlitePool,
    task: &Task,
    run_wd: &crate::workdir_provision::RunWorkdir,
    env: &crate::execenv::ExecEnv,
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
    // Persist a diagnostic into the `result` column (in the TaskResult
    // `{"content": ...}` shape the detail surface renders) on EVERY failure, so a
    // crash is diagnosable from stored evidence alone. The bare `fail` path left
    // `result` blank, making every crash undiagnosable from the DB. Both tails
    // are folded when present — `claude --output-format stream-json` writes its
    // terminal ERROR line to STDOUT, so persisting only stderr dropped exactly
    // the evidence an exit-65 agent_error leaves behind. A zero-output death
    // (e.g. a sandboxed CLI killed exit 65 before writing a byte) leaves BOTH
    // tails empty; `failure_detail` synthesises a diagnostic there from what we
    // DO know (reason + exit code + provider), so no failure path leaves
    // `result` NULL.
    let detail = failure_detail(
        reason,
        provider,
        result.exit_code,
        &result.stdout_tail,
        &result.stderr_tail,
    );
    let fail_outcome =
        FailTaskService::fail_with_detail(pool, &task.id, reason, &detail, clock).await;
    match fail_outcome {
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
    // Twin on `issue.state`: the aggregate is `failed`/`cancelled` here (this
    // task's own failure is in the set), so this no-ops — but it keeps the
    // lifecycle seam symmetric with the board seam. Advance-only + best-effort.
    crate::board::advance_and_cascade_child(pool, task, events).await;
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
    tracing::warn!(task_id = %task.id, reason = reason.as_db_str(), stderr_tail = %result.stderr_tail, "task failed");
    // F5: tear down the run's provisioned worktree (keep-if-dirty). A failed run
    // that left a dirty checkout keeps it (the partial work is preserved for a
    // rerun / inspection); a clean one is removed. Done BEFORE the retry spawn so
    // a retryable failure's fresh child re-provisions a clean worktree rather than
    // resuming into this run's residue.
    //
    // hangar-e2e-6: the escape hatch preserves BOTH the worktree AND the
    // provider-log dir for a clean (zero-work) failure so the e2e loop can read
    // `{logs}/claude.jsonl` — the transcript a 36ms exit-65 crash would otherwise
    // lose to keep-if-dirty teardown. The retry child is a NEW task row with a
    // NEW full-id-keyed worktree, so keeping this run's residue never collides.
    if keep_failed_runs() {
        tracing::warn!(
            task_id = %task.id,
            reason = reason.as_db_str(),
            worktree = %run_wd.path().display(),
            logs_dir = %env.logs.display(),
            "HANGAR_KEEP_FAILED_RUNS=1: preserving failed run's worktree + provider logs for inspection"
        );
    } else {
        teardown_workdir(run_wd, &task.id);
    }
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

/// Terminalise a task that faulted during PRE-RUN setup — before the
/// `dispatched -> running` start transition — as `failed`, surfacing the real
/// error instead of stranding the row.
///
/// The claim loop resolves the workspace slug, prepares the isolated execenv,
/// and provisions the run's working directory (F5) while the row is still
/// `dispatched`. A fault in that window (a `repo_ref` that will not clone, an
/// execenv that cannot be built) used to propagate out of [`execute_claimed`]
/// with the row left `dispatched`: the stale-dispatch sweeper then reclaimed it
/// past the 90s window and re-dispatched it into the SAME fault, looping
/// invisibly — the board card never left Todo and the detail never showed an
/// error — until the 5min dispatch TTL relabelled it `timeout` with no cause.
///
/// This seam finalises the row to `failed` AT ONCE, from `dispatched`, recording
/// the real error (persisted into `result` so the detail renders it) under the
/// terminal, no-retry
/// [`FailureReason::ProvisionError`](ainb_hangar_store::service::fail::FailureReason::ProvisionError) —
/// so the failure is terminal, attributed, and visible on the board + detail. The
/// side-effects (stats / event / card auto-move / comment) mirror
/// [`finalize_failure`], minus teardown + retry: there is no worktree to reclaim
/// and a deterministic setup fault does not warrant a retry.
///
/// Always returns `Ok(())` so the claim loop treats the fault as HANDLED and
/// never re-logs it as an unhandled execution error. A concurrent cancel that won
/// the row (`dispatched -> cancelled`) is honoured (failure side-effects skipped),
/// and a terminalise that itself faults (row vanished / DB error) is left to the
/// dispatch-TTL sweeper backstop rather than looping.
async fn finalize_setup_failure(
    pool: &SqlitePool,
    task: &Task,
    // `Send + Sync` so the enclosing claim-loop future stays `Send` across the
    // `.await`s below (a bare `&dyn Display` would make it un-spawnable).
    error: &(dyn std::fmt::Display + Send + Sync),
    clock: &dyn HangarClock,
    stats: &HealthStats,
    events: &EventSink,
) -> anyhow::Result<()> {
    use ainb_hangar_store::service::fail::FailureReason;
    let reason = FailureReason::ProvisionError;
    let message = format!("run setup failed before the agent started: {error}");
    tracing::error!(task_id = %task.id, error = %error, "task setup failed before run; failing task");
    match FailTaskService::fail_setup(pool, &task.id, reason, &message, clock).await {
        Ok(_) => {}
        // A human cancel won the row first (`dispatched -> cancelled`). Honour it:
        // skip the failure side-effects and do not log a benign race as an error.
        Err(FinalizeError::TerminalMismatch {
            found: TaskState::Cancelled,
            ..
        }) => {
            tracing::info!(task_id = %task.id, "setup failed but task was cancelled first; honoring cancel");
            return Ok(());
        }
        // Could not terminalise (row vanished / already moved / DB fault). The
        // dispatch-TTL sweeper is the backstop; do not loop-log as an execution error.
        Err(e) => {
            tracing::warn!(task_id = %task.id, error = %e, "could not terminalize setup failure; leaving to sweeper backstop");
            return Ok(());
        }
    }
    // The row is terminal: record the outcome, push the terminal event, and
    // auto-move the card so the board and detail surface the failure (the whole
    // point of the fix). All best-effort, never blocking the claim loop.
    stats.record_failed(clock.now_ms() / 1_000);
    emit_task_finished(
        events,
        task,
        ainb_hangar_proto::events::TaskResult::Failure,
        clock,
    );
    crate::board::auto_move_after_terminal(pool, task).await;
    // Twin on `issue.state`; no-ops on the failed aggregate, kept for symmetry
    // with the board seam. Advance-only + best-effort.
    crate::board::advance_and_cascade_child(pool, task, events).await;
    crate::board::unblock_dependents_after_terminal(pool, task).await;
    progress_comment::emit_checkpoint(
        pool,
        &SystemIdGen,
        clock,
        task,
        progress_comment::Checkpoint::Failed { reason },
    )
    .await;
    tracing::warn!(task_id = %task.id, reason = reason.as_db_str(), "task failed during setup");
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
/// Deliberately NOT [`Default`]: see [`ResolvedDispatch::fallback`]. A derived
/// default would carry an empty prompt, which is not a degraded run but a
/// guaranteed non-run.
#[derive(Debug, Clone)]
struct ResolvedDispatch {
    /// Which provider exec path [`execute_claimed`] routes to.
    backend: Backend,
    /// Which CONTRACT that exec path is spawned for, resolved once from the
    /// task row (see [`dispatch_mode`]) and carried beside `backend`.
    ///
    /// It lives here rather than being re-derived at each use so the pane a task
    /// gets and the argv spawned into it cannot disagree. They did: the
    /// interactive path built its argv with the headless call, so a `mode =
    /// "interactive"` task spawned `claude -p` ("Print response and exit") into a
    /// pane the operator was meant to attach to and drive. Re-deriving is what
    /// gave that bug somewhere to live.
    mode: Mode,
    /// The `model` + `cli_args` the provider threads onto its argv.
    invocation: ProviderInvocation,
    /// The agent's per-agent env (`agent_env`), layered onto the child env
    /// after the deny-by-default ambient allowlist.
    agent_env: Vec<(String, String)>,
}

/// The `tasks.mode` column value that asks for an attachable session (ccc / D6,
/// the board's "Run ▾" affordance). Anything else is headless.
const INTERACTIVE_TASK_MODE: &str = "interactive";

/// The provider contract a task's `mode` column is asking for.
///
/// The ONE place a task row's mode becomes a [`Mode`]. Both the branch that picks
/// the exec path and the argv built for it read it, so the pane a task gets and
/// the argv spawned into it cannot disagree — which they did: a `mode =
/// "interactive"` task was handed the print-and-exit headless argv and died in a
/// pane the operator was meant to drive.
fn dispatch_mode(task_mode: &str) -> Mode {
    if task_mode == INTERACTIVE_TASK_MODE {
        Mode::Interactive
    } else {
        Mode::Headless
    }
}

impl ResolvedDispatch {
    /// The fault fallback: the default provider, no per-agent config, and the
    /// [`FALLBACK_PROMPT`] — used when the agent/runtime resolve fails so a
    /// misconfigured agent still RUNS rather than stranding the task.
    ///
    /// The prompt is the entire point of this constructor existing instead of a
    /// derived [`Default`]. `..Default::default()` here would hand the provider an
    /// empty brief, and a promptless provider does not "run in a degraded way" —
    /// it exits non-zero immediately, which is the very bug this fault path
    /// claimed to avoid. The "never promptless" guarantee lives in
    /// [`build_prompt`], and this path does not call it, so the invariant has to
    /// be restated here.
    ///
    /// `mode` is still the task row's own — a resolve fault says nothing about
    /// which contract the operator asked for, and defaulting it would spawn a
    /// print-and-exit process into an interactive pane.
    fn fallback(mode: Mode) -> Self {
        Self {
            backend: Backend::default(),
            mode,
            invocation: ProviderInvocation {
                prompt: FALLBACK_PROMPT.to_string(),
                model: None,
                cli_args: Vec::new(),
            },
            agent_env: Vec::new(),
        }
    }
}

/// Resolve the provider routing for a task's agent (e38.16).
///
/// Reads the agent row (its migration-0015 `model`/`cli_args`/`agent_env`
/// config) and picks the backend from the agent's OWN `provider` when set
/// (migration 0041), falling back to its runtime's advertised `provider` when
/// the agent has no override. The runtime is an execution slot the claim loop
/// keys off by `runtime_id` (never by provider — see `claim.rs` `CLAIM_SQL`), so
/// a `codex` agent bound to the single default (`claude`-advertised) runtime is
/// still claimed and now dispatches the codex backend. The agent's `model` and
/// `cli_args` become the [`ProviderInvocation`]; its `agent_env` is carried
/// separately.
///
/// # Errors
///
/// Returns an error if the agent id is malformed or the agent / runtime row is
/// missing — the caller falls back to [`ResolvedDispatch::fallback`].
async fn resolve_dispatch(
    pool: &SqlitePool,
    agent_id: &str,
    issue_id: Option<&str>,
    mode: Mode,
) -> anyhow::Result<ResolvedDispatch> {
    use ainb_hangar_store::repo::agent::AgentRepo;
    use ainb_hangar_store::repo::agent_runtime::AgentRuntimeRepo;
    let agent = AgentRepo::get(pool, agent_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("agent {agent_id} not found"))?;
    let prompt = build_prompt(pool, issue_id, agent.instructions.as_deref()).await;
    let runtime = AgentRuntimeRepo::get(pool, &agent.runtime_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("runtime {} not found", agent.runtime_id))?;
    // Per-agent provider wins; an agent with no override uses the runtime default.
    let provider = agent.provider.as_deref().unwrap_or(&runtime.provider);
    Ok(ResolvedDispatch {
        backend: Backend::from_provider(provider),
        mode,
        invocation: ProviderInvocation {
            prompt,
            model: agent.model,
            cli_args: agent.cli_args,
        },
        agent_env: agent.agent_env,
    })
}

/// The brief handed to the provider non-interactively for a task.
///
/// Every agent CLI needs the ask on its command line — the workdir carries only
/// CONTEXT (`CLAUDE.md`, materialised skills), never the work itself — so a task
/// with no prompt runs nothing. Sources, in order:
///
/// 1. the task's issue (`title` + `description`) — the normal case,
/// 2. the agent's own `instructions` — a chat / autopilot task with no issue,
/// 3. [`FALLBACK_PROMPT`] — so a provider is never spawned promptless (which is
///    an immediate non-zero exit, not a no-op).
async fn build_prompt(
    pool: &SqlitePool,
    issue_id: Option<&str>,
    agent_instructions: Option<&str>,
) -> String {
    use ainb_hangar_store::repo::issue::IssueRepo;

    if let Some(issue_id) = issue_id {
        if let Ok(Some(issue)) = IssueRepo::get_by_id(pool, issue_id).await {
            let mut brief = issue.title;
            if let Some(desc) = issue.description.filter(|d| !d.trim().is_empty()) {
                brief.push_str("\n\n");
                brief.push_str(&desc);
            }
            // 0043: when the issue links an upstream GitHub/Jira issue, append it as
            // a `Linked issue:` line so the agent resolves the link itself at
            // runtime (ainb never fetches it). Appended even to a title-only brief,
            // so a linked issue with no description still hands the agent the ref.
            if let Some(ext) = issue.external_ref.filter(|e| !e.trim().is_empty()) {
                brief.push_str("\n\nLinked issue: ");
                brief.push_str(ext.trim());
            }
            if !brief.trim().is_empty() {
                return brief;
            }
        }
    }
    agent_instructions
        .map(str::trim)
        .filter(|i| !i.is_empty())
        .map_or_else(|| FALLBACK_PROMPT.to_string(), ToString::to_string)
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
            crate::runner::RunLocation {
                cwd: path.clone(),
                extra_root: Some(path.clone()),
            }
        }
        RunWorkdir::Fallback { path } => crate::runner::RunLocation {
            cwd: path.clone(),
            extra_root: None,
        },
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
pub(crate) async fn workspace_slug(
    pool: &SqlitePool,
    workspace_id: &str,
) -> anyhow::Result<String> {
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

    /// hangar-e2e-6: a failed run whose runner captured BOTH stdout and stderr
    /// tails must surface BOTH in the persisted failure detail. This is the core
    /// observability fix: `claude --output-format stream-json` writes its terminal
    /// error line to STDOUT, so the previous stderr-only detail left an exit-65
    /// `agent_error` diagnosable ONLY if the crash happened to also write stderr —
    /// which the 36ms zero-output failure did not.
    #[test]
    fn failure_detail_folds_both_stdout_and_stderr_tails() {
        use ainb_hangar_store::service::fail::FailureReason;
        let detail = failure_detail(
            FailureReason::AgentError,
            "claude",
            Some(1),
            r#"{"type":"result","subtype":"error_during_execution","error":"boom"}"#,
            "node: fatal: could not start",
        );
        assert!(
            detail.contains("agent_error"),
            "detail must name the reason: {detail}"
        );
        assert!(
            detail.contains("boom"),
            "detail must carry the STDOUT tail (claude writes its error there): {detail}"
        );
        assert!(
            detail.contains("node: fatal: could not start"),
            "detail must carry the STDERR tail: {detail}"
        );
    }

    /// The regression this fix closes: a failure with output ONLY on stdout (the
    /// exit-65 claude shape — a stream-json error line, empty stderr) must still
    /// produce a stored detail. The old code keyed solely on the stderr tail and
    /// returned the blank `fail` for exactly this case.
    #[test]
    fn failure_detail_surfaces_stdout_only_failures() {
        use ainb_hangar_store::service::fail::FailureReason;
        let detail = failure_detail(
            FailureReason::AgentError,
            "claude",
            Some(65),
            r#"{"type":"result","subtype":"error","error":"exit 65"}"#,
            "   ",
        );
        assert!(
            detail.contains("exit 65"),
            "the stdout error line must survive into the detail: {detail}"
        );
    }

    /// No captured output on EITHER stream → a SYNTHESIZED diagnostic naming the
    /// exit code, reason, and provider (never a blank `fail` / NULL `result`),
    /// and whitespace-only tails count as empty.
    #[test]
    fn failure_detail_synthesizes_diagnostic_when_no_output_captured() {
        use ainb_hangar_store::service::fail::FailureReason;
        let detail = failure_detail(FailureReason::AgentError, "claude", Some(65), "   ", "\n\t");
        assert!(
            detail.contains("65"),
            "the synthesized diagnostic must name the exit code: {detail}"
        );
        assert!(
            detail.contains("agent_error"),
            "the synthesized diagnostic must name the failure reason: {detail}"
        );
        assert!(
            detail.contains("claude"),
            "the synthesized diagnostic must name the provider: {detail}"
        );
    }

    /// hangar-e2e-4: the `HANGAR_DAEMON_DISABLE_SANDBOX` override wins in both
    /// directions regardless of platform default — `=1` forces the headless OS
    /// sandbox OFF, `=0` forces it ON (the latter is how the durable follow-up
    /// exercises the Seatbelt profile on macOS, where it is otherwise off).
    #[test]
    fn resolve_sandbox_env_override_forces_off_and_on() {
        use std::ffi::OsStr;
        assert!(
            !DaemonConfig::resolve_sandbox(Some(OsStr::new("1"))),
            "HANGAR_DAEMON_DISABLE_SANDBOX=1 must force the sandbox OFF"
        );
        assert!(
            DaemonConfig::resolve_sandbox(Some(OsStr::new("0"))),
            "HANGAR_DAEMON_DISABLE_SANDBOX=0 must force the sandbox ON"
        );
    }

    /// hangar-e2e-4: with the env var unset, the headless sandbox posture is the
    /// PLATFORM default. On macOS it must be OFF — the default-on Seatbelt
    /// profile cannot boot the Node `claude` CLI, so every headless task died
    /// exit 65 before writing a transcript; on Linux/Landlock it stays ON. This
    /// is the fix's core assertion: before it, macOS defaulted ON (dispatch
    /// broken); after, OFF (dispatch restored), matching the already-unsandboxed
    /// interactive path.
    #[test]
    fn resolve_sandbox_unset_uses_platform_default() {
        let posture = DaemonConfig::resolve_sandbox(None);
        #[cfg(target_os = "macos")]
        assert!(
            !posture,
            "macOS: headless sandbox must default OFF (Seatbelt cannot boot the claude Node CLI)"
        );
        #[cfg(not(target_os = "macos"))]
        assert!(
            posture,
            "non-macOS: headless sandbox must default ON (Landlock runs the CLI fine)"
        );
    }

    /// A bare provider name is resolved to an absolute binary via `$PATH`:
    /// `sh` stands in for `claude`, reliably present on `$PATH` on any unix host.
    /// Before this fix the bare `PathBuf::from("claude")` flowed unresolved into
    /// the sandbox profile as a `(literal "claude")` rule the kernel never
    /// matched.
    #[cfg(unix)]
    #[test]
    fn resolve_provider_path_resolves_bare_name_to_absolute() {
        let resolved = resolve_provider_path(PathBuf::from("sh"), "claude");
        assert!(
            resolved.is_absolute(),
            "bare provider name must resolve to an absolute path: {resolved:?}"
        );
        assert!(
            resolved.is_file(),
            "resolved provider path must be a real file: {resolved:?}"
        );
    }

    /// An explicit override path is canonicalized (symlink-resolved) so the
    /// profile references the real target the OS execs.
    #[cfg(unix)]
    #[test]
    fn resolve_provider_path_canonicalizes_explicit_symlink() {
        use std::os::unix::fs::{PermissionsExt, symlink};

        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real-claude");
        std::fs::write(&real, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o755)).unwrap();
        let link = dir.path().join("claude-link");
        symlink(&real, &link).unwrap();

        let resolved = resolve_provider_path(link.clone(), "claude");
        assert_eq!(
            resolved,
            std::fs::canonicalize(&real).unwrap(),
            "explicit path must be symlink-canonicalized to its real target"
        );
        assert!(resolved.is_absolute());
    }

    /// A bare name absent from `$PATH` falls back to the bare name (and warns) so
    /// the daemon still boots; the sandbox denial is diagnosable, not a panic.
    #[test]
    fn resolve_provider_path_falls_back_when_absent() {
        let bare = PathBuf::from("definitely-not-a-real-provider-xyz123");
        let resolved = resolve_provider_path(bare.clone(), "claude");
        assert_eq!(
            resolved, bare,
            "an unresolvable bare name is returned as-is"
        );
    }

    /// hangar-e2e-5: the daemon must emit its resolved OS-sandbox posture at
    /// boot. Without this INFO line an exit-65 headless dispatch failure was
    /// indistinguishable from a stale binary (fix absent); a full e2e cycle was
    /// burned on that ambiguity. Capture the event and assert both the message
    /// and that the `sandbox` field carries the configured posture verbatim.
    #[test]
    fn log_sandbox_posture_emits_posture_at_boot() {
        use std::sync::{Arc, Mutex};
        use tracing::field::{Field, Visit};
        use tracing_subscriber::Layer;
        use tracing_subscriber::layer::{Context, SubscriberExt};
        use tracing_subscriber::registry::LookupSpan;

        #[derive(Default, Clone)]
        struct Event {
            message: String,
            fields: Vec<(String, String)>,
        }
        type EventLog = Arc<Mutex<Vec<Event>>>;

        struct Collector<'a>(&'a mut Event);
        impl Visit for Collector<'_> {
            fn record_bool(&mut self, field: &Field, value: bool) {
                self.0.fields.push((field.name().to_string(), value.to_string()));
            }
            fn record_str(&mut self, field: &Field, value: &str) {
                if field.name() == "message" {
                    self.0.message = value.to_string();
                } else {
                    self.0.fields.push((field.name().to_string(), value.to_string()));
                }
            }
            fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
                let rendered = format!("{value:?}");
                if field.name() == "message" {
                    self.0.message = rendered;
                } else {
                    self.0.fields.push((field.name().to_string(), rendered));
                }
            }
        }

        struct CollectLayer {
            log: EventLog,
        }
        impl<S> Layer<S> for CollectLayer
        where
            S: tracing::Subscriber + for<'a> LookupSpan<'a>,
        {
            fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
                let mut captured = Event::default();
                event.record(&mut Collector(&mut captured));
                self.log.lock().expect("event log lock").push(captured);
            }
        }

        let log: EventLog = Arc::default();
        let subscriber = tracing_subscriber::registry().with(CollectLayer { log: log.clone() });

        // Two configs proving the field mirrors the posture, not a constant.
        let mut cfg = DaemonConfig::from_env();
        tracing::subscriber::with_default(subscriber, || {
            cfg.sandbox = false;
            log_sandbox_posture(&cfg);
            cfg.sandbox = true;
            log_sandbox_posture(&cfg);
        });

        let events: Vec<Event> = log.lock().expect("event log").clone();
        let posture: Vec<&Event> = events
            .iter()
            .filter(|e| e.message == "headless provider sandbox posture")
            .collect();
        assert_eq!(
            posture.len(),
            2,
            "expected one sandbox-posture log per boot call, got {}",
            posture.len()
        );
        let sandbox_field = |e: &Event| {
            e.fields
                .iter()
                .find(|(k, _)| k == "sandbox")
                .map(|(_, v)| v.clone())
                .expect("posture log must carry a `sandbox` field")
        };
        assert_eq!(
            sandbox_field(posture[0]),
            "false",
            "OFF posture must log sandbox=false"
        );
        assert_eq!(
            sandbox_field(posture[1]),
            "true",
            "ON posture must log sandbox=true"
        );
        assert!(
            posture[0].fields.iter().any(|(k, _)| k == "target_os"),
            "posture log must carry the resolving platform in `target_os`"
        );
    }

    /// A secret backend whose `get` blocks far longer than any test timeout.
    /// Stands in for the headless keychain GUI-prompt hang that wedged dispatch.
    struct HangingBackend;
    impl ainb_hangar_secrets::SecretBackend for HangingBackend {
        fn get(
            &self,
            _: &ainb_hangar_secrets::Scope,
            _: &str,
        ) -> ainb_hangar_secrets::Result<Option<ainb_hangar_secrets::SecretBytes>> {
            // The real hang is unbounded (a GUI prompt nobody answers); 3s is
            // 30x the sub-second test timeout ("effectively forever" for the
            // assertion), without making the leaked blocking thread stall process
            // exit for long.
            std::thread::sleep(Duration::from_secs(3));
            Ok(Some(ainb_hangar_secrets::SecretBytes::from(
                b"tok".as_slice(),
            )))
        }
        fn put(
            &self,
            _: &ainb_hangar_secrets::Scope,
            _: &str,
            _: &[u8],
        ) -> ainb_hangar_secrets::Result<()> {
            unreachable!("the hang test never writes")
        }
        fn delete(
            &self,
            _: &ainb_hangar_secrets::Scope,
            _: &str,
        ) -> ainb_hangar_secrets::Result<()> {
            unreachable!("the hang test never deletes")
        }
    }

    /// The zombie-dispatch regression: a keychain read that never returns must
    /// NOT wedge dispatch. `resolve_cred_env` bounds it, so a hung read yields an
    /// empty env within the timeout instead of blocking the async worker forever.
    #[tokio::test]
    async fn cred_read_times_out_instead_of_wedging_dispatch() {
        let started = std::time::Instant::now();
        let env = resolve_cred_env(
            Backend::Claude,
            Arc::new(HangingBackend),
            std::collections::HashMap::new(),
            Duration::from_millis(100),
        )
        .await;

        assert!(
            env.is_empty(),
            "a wedged keychain read must inject no token (got {env:?})"
        );
        // Generous bound: the point is it returned at all, near the 100ms timeout
        // rather than after the 3s sleep. Before the fix this call never returns.
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "resolve_cred_env blocked on the hung read for {:?}",
            started.elapsed()
        );
    }

    /// The happy path still works: a fast backend's token is resolved and injected
    /// as `CLAUDE_CODE_OAUTH_TOKEN` (the bounding must not drop a real credential).
    #[tokio::test]
    async fn cred_read_returns_token_when_fast() {
        use ainb_hangar_secrets::SecretBackend as _;
        let b = ainb_hangar_secrets::InMemoryBackend::new();
        b.put(
            &ainb_hangar_secrets::Scope::Global,
            crate::claude_cred::SECRET_KEY,
            b"tok",
        )
        .unwrap();

        let env = resolve_cred_env(
            Backend::Claude,
            Arc::new(b),
            std::collections::HashMap::new(),
            Duration::from_secs(5),
        )
        .await;

        assert_eq!(
            env,
            vec![(
                crate::claude_cred::CHILD_ENV_VAR.to_string(),
                "tok".to_string()
            )]
        );
    }

    /// Doctrine hardening (D-e2e-3): the WHOLE `running -> provider spawn` preamble
    /// is bounded as ONE unit, not just the keychain read. A setup step that wedges
    /// — here a keychain backend that never returns, with the INNER cred timeout
    /// set far above the umbrella so only the outer bound can cut it — must be cut
    /// off promptly and surface as a timeout, never run to the inner bound and
    /// never forever. This is what turns a wedged setup into a terminal
    /// `spawn_timeout` in `execute_claimed` instead of a forever-`running` row.
    #[tokio::test]
    async fn spawn_setup_preamble_is_bounded_as_one_unit() {
        use ainb_hangar_store::bootstrap;
        use ainb_hangar_store::repo::task::{NewTask, TaskRepo};

        let dir = tempfile::tempdir().unwrap();
        let store = ainb_hangar_store::Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        let ws = bootstrap::ensure_default_workspace(pool).await.unwrap();
        bootstrap::ensure_runtime(pool, &bootstrap::default_runtime_id(), 1)
            .await
            .unwrap();
        let agent = bootstrap::create_agent(pool, &ws, "worker", "claude", None).await.unwrap();

        // A real queued task, read back as a `Task` so the preamble's agent /
        // workspace lookups resolve against real rows.
        let task_id =
            ainb_hangar_core::idgen::IdGen::new_ulid(&ainb_hangar_core::idgen::SystemIdGen);
        TaskRepo::insert(
            pool,
            &NewTask {
                id: task_id.clone(),
                workspace_id: ws.clone(),
                runtime_id: bootstrap::default_runtime_id(),
                agent_id: agent.id.clone(),
                issue_id: None,
                work_dir: None,
                priority: 0,
                created_at: 1,
                autopilot_run_id: None,
                generation: 0,
            },
        )
        .await
        .unwrap();
        let task = TaskRepo::get_by_id(pool, &task_id).await.unwrap().unwrap();

        // A throwaway execenv rooted in the tempdir (the wedge fires in the cred
        // read, before any materialise step touches these paths).
        let root = dir.path().join("task-root");
        let env = crate::execenv::ExecEnv {
            workdir: root.join("workdir"),
            output: root.join("output"),
            logs: root.join("logs"),
            gc_meta: root.join(".gc_meta.json"),
        };

        // Inner cred timeout (30s) far above the umbrella (300ms): ONLY the outer
        // umbrella can cut off the hung keychain read.
        let started = std::time::Instant::now();
        let bounded = tokio::time::timeout(
            Duration::from_millis(300),
            prepare_spawn_inputs(
                pool,
                &task,
                &env,
                Backend::Claude,
                Arc::new(HangingBackend),
                Duration::from_secs(30),
            ),
        )
        .await;

        assert!(
            bounded.is_err(),
            "a wedged preamble must be cut off by the umbrella, not run to the inner cred bound"
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the umbrella must bound the wedge promptly (took {:?})",
            started.elapsed()
        );
    }

    /// Real-path mutation guard for the umbrella: drive the ACTUAL
    /// `execute_claimed` seam (not `prepare_spawn_inputs` in isolation) with a
    /// wedged keychain read and the umbrella tightened via
    /// `HANGAR_SPAWN_SETUP_TIMEOUT_MS`. The row MUST terminalise
    /// `running -> failed` with `spawn_timeout`. Deleting the `tokio::time::timeout`
    /// wrap in `execute_claimed` turns this RED: the bounded (5s) cred read then
    /// returns, the run reaches a provider spawn against a non-existent binary, and
    /// the row lands `failed`/`spawn_error` (or hangs) — never `spawn_timeout`.
    #[tokio::test]
    async fn execute_claimed_terminalizes_a_wedged_setup_via_the_umbrella() {
        use ainb_hangar_store::bootstrap;
        use ainb_hangar_store::repo::task::{NewTask, TaskRepo};
        use ainb_hangar_store::service::claim::ClaimTaskService;

        // Serialise with every other `$AINB_HANGAR_HOME`-mutating test, and set a
        // tiny umbrella so the 3s wedged cred read blows the bound deterministically.
        let _env = ainb_hangar_store::test_support::lock_env();
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("hangar-home");
        let prior_home = std::env::var_os(ainb_hangar_core::paths::HANGAR_HOME_ENV);
        let prior_to = std::env::var_os("HANGAR_SPAWN_SETUP_TIMEOUT_MS");
        std::env::set_var(ainb_hangar_core::paths::HANGAR_HOME_ENV, &home);
        std::env::set_var("HANGAR_SPAWN_SETUP_TIMEOUT_MS", "50");

        let store = ainb_hangar_store::Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        let ws = bootstrap::ensure_default_workspace(pool).await.unwrap();
        let rt = bootstrap::default_runtime_id();
        bootstrap::ensure_runtime(pool, &rt, 1).await.unwrap();
        let agent = bootstrap::create_agent(pool, &ws, "worker", "claude", None).await.unwrap();

        let task_id =
            ainb_hangar_core::idgen::IdGen::new_ulid(&ainb_hangar_core::idgen::SystemIdGen);
        TaskRepo::insert(
            pool,
            &NewTask {
                id: task_id.clone(),
                workspace_id: ws.clone(),
                runtime_id: rt.clone(),
                agent_id: agent.id.clone(),
                issue_id: None,
                work_dir: None,
                priority: 0,
                created_at: 1,
                autopilot_run_id: None,
                generation: 0,
            },
        )
        .await
        .unwrap();

        let clock = SystemClock;
        let claimed = ClaimTaskService::claim_for_runtime(pool, &rt, &clock)
            .await
            .unwrap()
            .expect("the queued task must claim");

        // Provider paths that do not resolve: under the mutation (no umbrella) the
        // run would reach a spawn that ENOENT-fails as `spawn_error` — a DIFFERENT
        // reason than the `spawn_timeout` asserted below.
        let runner = Runner::new(RunnerConfig {
            claude_path: "/nonexistent/hangar-test-claude".into(),
            codex_path: "/nonexistent/hangar-test-codex".into(),
            copilot_path: "/nonexistent/hangar-test-copilot".into(),
            max_runtime: Duration::from_secs(1),
            tail_lines: 1,
            sandbox: false,
        });
        let stats = HealthStats::default();
        let events = crate::events::EventBroker::new().sink();
        let interactive = InteractiveSessions::default();

        let outcome = execute_claimed(
            pool,
            &runner,
            &claimed,
            &clock,
            &stats,
            &events,
            &interactive,
            Arc::new(HangingBackend),
        )
        .await;

        // Restore env BEFORE asserting so a failed assertion never leaks it.
        match prior_home {
            Some(v) => std::env::set_var(ainb_hangar_core::paths::HANGAR_HOME_ENV, v),
            None => std::env::remove_var(ainb_hangar_core::paths::HANGAR_HOME_ENV),
        }
        match prior_to {
            Some(v) => std::env::set_var("HANGAR_SPAWN_SETUP_TIMEOUT_MS", v),
            None => std::env::remove_var("HANGAR_SPAWN_SETUP_TIMEOUT_MS"),
        }

        outcome.expect("execute_claimed handles the wedge internally (never Err)");
        let row = TaskRepo::get_by_id(pool, &task_id).await.unwrap().unwrap();
        assert_eq!(
            row.status, "failed",
            "a wedged setup must terminalise, not stay running"
        );
        assert_eq!(
            row.failure_reason.as_deref(),
            Some("spawn_timeout"),
            "the umbrella must attribute the wedge as spawn_timeout (got {:?})",
            row.failure_reason
        );
    }

    /// migration 0045 / gap #5: driving the REAL `execute_claimed` seam for a task
    /// stamped with a `squad_id` must emit the claim-time squad-briefing hook line —
    /// carrying BOTH the `task_id` and the `squad_id` — BEFORE the provider spawn.
    /// The provider paths are nonexistent so the run fails, yet the hook must have
    /// already fired (the observable seam gap #1 keys its briefing injection off).
    /// Deleting the `squad_briefing_hook(&task)` call from `execute_claimed` turns
    /// this RED: no hook event is captured though the row still terminalises.
    #[tokio::test]
    async fn execute_claimed_fires_the_squad_briefing_hook_before_spawn() {
        use ainb_hangar_store::bootstrap;
        use ainb_hangar_store::repo::task::{NewTask, TaskRepo};
        use ainb_hangar_store::service::claim::ClaimTaskService;
        use std::sync::{Arc, Mutex};
        use tracing::field::{Field, Visit};
        use tracing_subscriber::Layer;
        use tracing_subscriber::layer::{Context, SubscriberExt};
        use tracing_subscriber::registry::LookupSpan;

        #[derive(Default, Clone)]
        struct Ev {
            message: String,
            fields: Vec<(String, String)>,
        }
        type Log = Arc<Mutex<Vec<Ev>>>;
        struct Collect<'a>(&'a mut Ev);
        impl Visit for Collect<'_> {
            fn record_str(&mut self, field: &Field, value: &str) {
                if field.name() == "message" {
                    self.0.message = value.to_string();
                } else {
                    self.0.fields.push((field.name().to_string(), value.to_string()));
                }
            }
            fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
                let rendered = format!("{value:?}");
                if field.name() == "message" {
                    self.0.message = rendered;
                } else {
                    self.0.fields.push((field.name().to_string(), rendered));
                }
            }
        }
        struct CollectLayer {
            log: Log,
        }
        impl<S> Layer<S> for CollectLayer
        where
            S: tracing::Subscriber + for<'a> LookupSpan<'a>,
        {
            fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
                let mut captured = Ev::default();
                event.record(&mut Collect(&mut captured));
                self.log.lock().expect("event log lock").push(captured);
            }
        }

        // Serialise with every other `$AINB_HANGAR_HOME`-mutating test.
        let _env = ainb_hangar_store::test_support::lock_env();
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("hangar-home");
        let prior_home = std::env::var_os(ainb_hangar_core::paths::HANGAR_HOME_ENV);
        std::env::set_var(ainb_hangar_core::paths::HANGAR_HOME_ENV, &home);

        let store = ainb_hangar_store::Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        let ws = bootstrap::ensure_default_workspace(pool).await.unwrap();
        let rt = bootstrap::default_runtime_id();
        bootstrap::ensure_runtime(pool, &rt, 1).await.unwrap();
        let agent = bootstrap::create_agent(pool, &ws, "leader", "claude", None).await.unwrap();

        let task_id =
            ainb_hangar_core::idgen::IdGen::new_ulid(&ainb_hangar_core::idgen::SystemIdGen);
        TaskRepo::insert(
            pool,
            &NewTask {
                id: task_id.clone(),
                workspace_id: ws.clone(),
                runtime_id: rt.clone(),
                agent_id: agent.id.clone(),
                issue_id: None,
                work_dir: None,
                priority: 0,
                created_at: 1,
                autopilot_run_id: None,
                generation: 0,
            },
        )
        .await
        .unwrap();
        // Stamp the dispatching squad, exactly as a squad dispatch would, so the
        // claimed task carries it into `execute_claimed`.
        TaskRepo::set_squad_id(pool, &task_id, "squad-alpha").await.unwrap();

        let clock = SystemClock;
        let claimed = ClaimTaskService::claim_for_runtime(pool, &rt, &clock)
            .await
            .unwrap()
            .expect("the queued task must claim");

        // Nonexistent provider paths: the run fails to spawn, but the squad hook
        // must have fired first (it is placed BEFORE provider dispatch).
        let runner = Runner::new(RunnerConfig {
            claude_path: "/nonexistent/hangar-test-claude".into(),
            codex_path: "/nonexistent/hangar-test-codex".into(),
            copilot_path: "/nonexistent/hangar-test-copilot".into(),
            max_runtime: Duration::from_secs(1),
            tail_lines: 1,
            sandbox: false,
        });
        let stats = HealthStats::default();
        let events = crate::events::EventBroker::new().sink();
        let interactive = InteractiveSessions::default();

        let log: Log = Arc::default();
        let subscriber = tracing_subscriber::registry().with(CollectLayer { log: log.clone() });
        let outcome = {
            // Current-thread runtime: the guard holds the subscriber across every
            // `.await` in `execute_claimed`, so the inline hook event is captured.
            let _guard = tracing::subscriber::set_default(subscriber);
            execute_claimed(
                pool,
                &runner,
                &claimed,
                &clock,
                &stats,
                &events,
                &interactive,
                Arc::new(ainb_hangar_secrets::InMemoryBackend::new()),
            )
            .await
        };

        // Restore env BEFORE asserting so a failed assertion never leaks it.
        match prior_home {
            Some(v) => std::env::set_var(ainb_hangar_core::paths::HANGAR_HOME_ENV, v),
            None => std::env::remove_var(ainb_hangar_core::paths::HANGAR_HOME_ENV),
        }
        outcome.expect("execute_claimed handles the spawn failure internally (never Err)");

        let events: Vec<Ev> = log.lock().expect("event log").clone();
        let hook: Vec<&Ev> =
            events.iter().filter(|e| e.message.contains("squad briefing hook")).collect();
        assert_eq!(
            hook.len(),
            1,
            "exactly one claim-time squad-briefing hook line must fire"
        );
        let field =
            |e: &Ev, k: &str| e.fields.iter().find(|(name, _)| name == k).map(|(_, v)| v.clone());
        assert_eq!(
            field(hook[0], "squad_id").as_deref(),
            Some("squad-alpha"),
            "the hook line must carry the dispatching squad_id"
        );
        assert_eq!(
            field(hook[0], "task_id").as_deref(),
            Some(task_id.as_str()),
            "the hook line must carry the claimed task_id"
        );

        // The run still terminalised (nonexistent provider) — proving the hook
        // fired on the path to a FAILED run, before the spawn, not only on success.
        let row = TaskRepo::get_by_id(pool, &task_id).await.unwrap().unwrap();
        assert_eq!(
            row.status, "failed",
            "the nonexistent provider must terminalise the run"
        );
    }

    /// hangar-e2e-7 REGRESSION: a failure whose runner captured NO output (both
    /// tails empty — the zero-output death, e.g. a sandboxed CLI killed exit 65
    /// before writing a byte) must STILL persist a synthesized diagnostic into the
    /// `result` column, not leave it NULL. Cycle-2 #437 only closed the
    /// non-empty-stderr leg; the empty-tail leg went through the bare `fail`,
    /// storing `result = NULL` and making the crash undiagnosable from the DB
    /// alone. After the fix the synthesized `result` names the exit code + reason
    /// + provider, so any future no-output provider death is self-describing.
    #[tokio::test]
    async fn finalize_failure_with_empty_tails_persists_synthetic_diagnostic() {
        use ainb_hangar_store::bootstrap;
        use ainb_hangar_store::repo::task::{NewTask, TaskRepo};

        let dir = tempfile::tempdir().unwrap();
        let store = ainb_hangar_store::Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        let ws = bootstrap::ensure_default_workspace(pool).await.unwrap();
        let rt = bootstrap::default_runtime_id();
        bootstrap::ensure_runtime(pool, &rt, 1).await.unwrap();
        let agent = bootstrap::create_agent(pool, &ws, "worker", "claude", None).await.unwrap();

        // A `queued` task is a legal `fail` source state (same as `running`), so
        // we can drive `finalize_failure` directly without walking the full FSM.
        let task_id =
            ainb_hangar_core::idgen::IdGen::new_ulid(&ainb_hangar_core::idgen::SystemIdGen);
        TaskRepo::insert(
            pool,
            &NewTask {
                id: task_id.clone(),
                workspace_id: ws.clone(),
                runtime_id: rt.clone(),
                agent_id: agent.id.clone(),
                issue_id: None,
                work_dir: None,
                priority: 0,
                created_at: 1,
                autopilot_run_id: None,
                generation: 0,
            },
        )
        .await
        .unwrap();
        let task = TaskRepo::get_by_id(pool, &task_id).await.unwrap().unwrap();

        // The zero-output crash: a non-zero exit code, NO stdout, NO stderr — the
        // exact shape the macOS sandbox exit-65 death produces.
        let result = crate::runner::RunnerResult {
            exit_code: Some(65),
            session_id: None,
            usage: None,
            stdout_tail: String::new(),
            stderr_tail: String::new(),
        };
        // A scratch workdir: teardown + branch reclaim are no-ops for it, so the
        // finalize is exercised without provisioning a real worktree.
        let run_wd = crate::workdir_provision::RunWorkdir::Scratch {
            path: dir.path().join("scratch"),
        };
        // A throwaway execenv rooted in the tempdir; `finalize_failure` only
        // reads `env.logs` for the `HANGAR_KEEP_FAILED_RUNS` diagnostic log line
        // (unset here), so the paths need not exist.
        let root = dir.path().join("task-root");
        let env = crate::execenv::ExecEnv {
            workdir: root.join("workdir"),
            output: root.join("output"),
            logs: root.join("logs"),
            gc_meta: root.join(".gc_meta.json"),
        };
        let clock = SystemClock;
        let stats = HealthStats::default();
        let events = crate::events::EventBroker::new().sink();

        finalize_failure(
            pool,
            &task,
            &run_wd,
            &env,
            ainb_hangar_store::service::fail::FailureReason::AgentError,
            result,
            "claude",
            &clock,
            &stats,
            &events,
        )
        .await
        .expect("finalize_failure handles a zero-output crash");

        let row = TaskRepo::get_by_id(pool, &task_id).await.unwrap().unwrap();
        assert_eq!(row.status, "failed", "the row must terminalise as failed");
        // The core regression: `result` must be NON-NULL and self-describe the
        // crash from the exit code + reason (before the fix it was NULL here).
        let result_json = row
            .result
            .expect("a zero-output failure must NOT leave `result` NULL — it is the black hole this fix closes");
        assert!(
            result_json.contains("65"),
            "the synthesized diagnostic must name the exit code, got {result_json:?}"
        );
        assert!(
            result_json.contains("agent_error"),
            "the synthesized diagnostic must name the failure reason, got {result_json:?}"
        );
        assert!(
            result_json.contains("claude"),
            "the synthesized diagnostic must name the provider, got {result_json:?}"
        );
    }

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

    /// The task brief MUST reach the provider's argv. Every agent CLI starts an
    /// interactive session with no prompt and the daemon spawns them with a null
    /// stdin, so a promptless dispatch runs nothing (verified: bare `claude` exits
    /// 1 with "Input must be provided..."). Covers the issue source, the
    /// agent-instructions fallback, and the never-empty guarantee.
    #[tokio::test]
    async fn dispatch_carries_the_task_brief_to_the_provider() {
        use ainb_hangar_store::bootstrap;
        use ainb_hangar_store::repo::issue::{IssueRepo, NewIssue};

        let dir = tempfile::tempdir().unwrap();
        let store = ainb_hangar_store::Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        let ws = bootstrap::ensure_default_workspace(pool).await.unwrap();
        bootstrap::ensure_runtime(pool, &bootstrap::default_runtime_id(), 1)
            .await
            .unwrap();
        let agent = bootstrap::create_agent(pool, &ws, "worker", "claude", None).await.unwrap();

        // (1) An issue's title + description become the brief.
        let issue_id =
            ainb_hangar_core::idgen::IdGen::new_ulid(&ainb_hangar_core::idgen::SystemIdGen);
        IssueRepo::insert(
            pool,
            &NewIssue {
                id: issue_id.clone(),
                workspace_id: ws.clone(),
                title: "Fix the login bug".into(),
                description: Some("It 500s on empty password.".into()),
                state: "open".into(),
                creator: ainb_hangar_core::actor::ActorRef::new(
                    ainb_hangar_core::actor::ActorKind::Member,
                    "stevie",
                )
                .unwrap(),
                created_at: 1,
                priority: 0,
                assignee: None,
                due_date: None,
                labels: Vec::new(),
                parent_issue_id: None,
                stage: None,
            },
        )
        .await
        .unwrap();

        let disp = resolve_dispatch(pool, &agent.id, Some(&issue_id), Mode::Headless)
            .await
            .unwrap();
        assert_eq!(
            disp.invocation.prompt, "Fix the login bug\n\nIt 500s on empty password.",
            "the issue title + description must become the brief"
        );

        // …and it lands on the argv as the trailing positional (not just in the
        // struct), behind `-p` so the run is non-interactive.
        let runner = Runner::new(RunnerConfig {
            claude_path: "claude".into(),
            codex_path: "codex".into(),
            copilot_path: "copilot".into(),
            max_runtime: Duration::from_secs(1),
            tail_lines: 1,
            sandbox: false,
        });
        let (_p, argv) = runner.provider_command(disp.backend, &disp.invocation, Mode::Headless);
        assert_eq!(
            argv.first().map(String::as_str),
            Some("-p"),
            "claude must be invoked non-interactively: {argv:?}"
        );
        assert!(argv.ends_with(&[
            "--".to_string(),
            "Fix the login bug\n\nIt 500s on empty password.".to_string()
        ]));

        // (2) No issue → the agent's own instructions are the brief.
        let instructed = bootstrap::create_agent(
            pool,
            &ws,
            "guided",
            "claude",
            Some("Triage the inbox.".into()),
        )
        .await
        .unwrap();
        let disp = resolve_dispatch(pool, &instructed.id, None, Mode::Headless).await.unwrap();
        assert_eq!(disp.invocation.prompt, "Triage the inbox.");

        // (3) Neither → a non-empty fallback, never a promptless spawn.
        let disp = resolve_dispatch(pool, &agent.id, None, Mode::Headless).await.unwrap();
        assert!(
            !disp.invocation.prompt.is_empty(),
            "a prompt is never empty"
        );
        assert_eq!(disp.invocation.prompt, FALLBACK_PROMPT);
    }

    /// A linked upstream issue (0043) appends a `Linked issue:` line to the brief so
    /// the agent resolves the ref itself — appended even to a title-only brief, and
    /// absent entirely when no ref is set.
    #[tokio::test]
    async fn dispatch_appends_linked_issue_when_external_ref_set() {
        use ainb_hangar_store::bootstrap;
        use ainb_hangar_store::repo::card_parity::CardParityRepo;
        use ainb_hangar_store::repo::issue::{IssueRepo, NewIssue};

        let dir = tempfile::tempdir().unwrap();
        let store = ainb_hangar_store::Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        let ws = bootstrap::ensure_default_workspace(pool).await.unwrap();
        bootstrap::ensure_runtime(pool, &bootstrap::default_runtime_id(), 1)
            .await
            .unwrap();
        let agent = bootstrap::create_agent(pool, &ws, "worker", "claude", None).await.unwrap();

        let seed = |title: &'static str, desc: Option<&'static str>| {
            let ws = ws.clone();
            async move {
                let id =
                    ainb_hangar_core::idgen::IdGen::new_ulid(&ainb_hangar_core::idgen::SystemIdGen);
                IssueRepo::insert(
                    pool,
                    &NewIssue {
                        id: id.clone(),
                        workspace_id: ws.clone(),
                        title: title.into(),
                        description: desc.map(Into::into),
                        state: "open".into(),
                        creator: ainb_hangar_core::actor::ActorRef::new(
                            ainb_hangar_core::actor::ActorKind::Member,
                            "stevie",
                        )
                        .unwrap(),
                        created_at: 1,
                        priority: 0,
                        assignee: None,
                        due_date: None,
                        labels: Vec::new(),
                        parent_issue_id: None,
                        stage: None,
                    },
                )
                .await
                .unwrap();
                id
            }
        };

        // A brief WITH a linked ref: the ref appends as a trailing line.
        let with_ref = seed("Fix login", Some("It 500s on empty password.")).await;
        CardParityRepo::set_issue_external_ref(pool, &ws, &with_ref, Some("acme/api#42"))
            .await
            .unwrap();
        let disp = resolve_dispatch(pool, &agent.id, Some(&with_ref), Mode::Headless)
            .await
            .unwrap();
        assert_eq!(
            disp.invocation.prompt,
            "Fix login\n\nIt 500s on empty password.\n\nLinked issue: acme/api#42",
            "a set external_ref appends the Linked issue line"
        );

        // The SAME issue with no ref set: the brief is unchanged (no trailing line).
        let no_ref = seed("Fix login", Some("It 500s on empty password.")).await;
        let disp = resolve_dispatch(pool, &agent.id, Some(&no_ref), Mode::Headless).await.unwrap();
        assert_eq!(
            disp.invocation.prompt, "Fix login\n\nIt 500s on empty password.",
            "no external_ref → no Linked issue line"
        );

        // A title-only issue (no description) still gets the linked line.
        let title_only = seed("Wire the webhook", None).await;
        CardParityRepo::set_issue_external_ref(pool, &ws, &title_only, Some("https://x/y/1"))
            .await
            .unwrap();
        let disp = resolve_dispatch(pool, &agent.id, Some(&title_only), Mode::Headless)
            .await
            .unwrap();
        assert_eq!(
            disp.invocation.prompt, "Wire the webhook\n\nLinked issue: https://x/y/1",
            "a linked issue with no brief still hands the agent the ref"
        );
    }

    /// A task row whose `mode` says "interactive" must never be handed a
    /// print-and-exit argv.
    ///
    /// This is the regression that shipped: `run_interactive` built its argv with
    /// the same call the headless path used, so a D6 "Run ▾" task spawned
    /// `claude -p …` ("Print response and exit") into a tmux pane the operator was
    /// meant to attach to and drive. It escaped because the only coverage was a
    /// tmux e2e tripwire that SKIPs cleanly when tmux/the provider binaries are
    /// absent — so this asserts the row→argv contract with no tmux at all.
    ///
    /// It drives [`interactive_command`], which is the function `run_interactive`
    /// itself calls. That indirection is the point: the FIRST version of this test
    /// re-derived the argv beside `run_interactive` instead of through it, so
    /// hardcoding `Mode::Headless` inside `run_interactive` left this test — and
    /// all 254 lib tests, and every integration test — GREEN. A test that
    /// reimplements the code under test asserts nothing about that code.
    #[test]
    fn interactive_task_mode_never_produces_a_print_and_exit_argv() {
        assert_eq!(dispatch_mode("interactive"), Mode::Interactive);
        // Everything else is headless — including the empty/unknown values a row
        // can carry.
        assert_eq!(dispatch_mode("headless"), Mode::Headless);
        assert_eq!(dispatch_mode(""), Mode::Headless);
        assert_eq!(dispatch_mode("Interactive"), Mode::Headless);

        let runner = Runner::new(RunnerConfig {
            claude_path: "claude".into(),
            codex_path: "codex".into(),
            copilot_path: "copilot".into(),
            max_runtime: Duration::from_secs(1),
            tail_lines: 1,
            sandbox: false,
        });
        let inv = ProviderInvocation {
            prompt: "do the thing".to_string(),
            model: None,
            cli_args: Vec::new(),
        };
        // A dispatch resolved from a row whose `mode` column says "interactive",
        // exactly as `execute_claimed` resolves it.
        let interactive_dispatch = |backend| ResolvedDispatch {
            backend,
            mode: dispatch_mode("interactive"),
            invocation: inv.clone(),
            agent_env: Vec::new(),
        };

        // The argv an INTERACTIVE task row actually gets, through the same
        // function `run_interactive` calls.
        let (_p, argv) = interactive_command(&runner, &interactive_dispatch(Backend::Claude));
        assert!(
            !argv.contains(&"-p".to_string()),
            "an interactive task must not spawn claude in print-and-exit mode: {argv:?}"
        );
        assert!(
            argv.ends_with(&["--".to_string(), "do the thing".to_string()]),
            "an interactive task must still be seeded with its brief: {argv:?}"
        );
        let (_p, argv) = interactive_command(&runner, &interactive_dispatch(Backend::Copilot));
        assert!(
            !argv.contains(&"-p".to_string()),
            "an interactive task must not spawn copilot in exits-after-completion mode: {argv:?}"
        );
        let (_p, argv) = interactive_command(&runner, &interactive_dispatch(Backend::Codex));
        assert!(
            !argv.contains(&"exec".to_string()),
            "an interactive task must not spawn codex's non-interactive exec: {argv:?}"
        );

        // …and the headless row still gets the headless shape (the fix must not
        // simply disable non-interactive execution).
        let (_p, argv) = runner.provider_command(Backend::Claude, &inv, dispatch_mode("headless"));
        assert!(
            argv.contains(&"-p".to_string()),
            "a headless task MUST print-and-exit: {argv:?}"
        );
    }

    /// A HEADLESS codex row must carry the `-s danger-full-access` sandbox
    /// policy, and an INTERACTIVE one must not.
    ///
    /// This locks the dispatch-path fix for the class of bug the codex live
    /// tripwire surfaced: `codex exec` defaults to a read-only sandbox, so a
    /// headless run with no `-s` flag lets the model invoke a shell tool yet
    /// silently drops its write — the task exits 0 having produced no artifact.
    /// The headless argv MUST therefore pin `danger-full-access` (the daemon's
    /// own FS sandbox is the confinement boundary); the interactive TUI, which
    /// has a human attached to answer trust prompts and no FS sandbox, must keep
    /// codex's own default confinement.
    #[test]
    fn headless_codex_argv_pins_danger_full_access_sandbox() {
        let runner = Runner::new(RunnerConfig {
            claude_path: "claude".into(),
            codex_path: "codex".into(),
            copilot_path: "copilot".into(),
            max_runtime: Duration::from_secs(1),
            tail_lines: 1,
            sandbox: false,
        });
        let inv = ProviderInvocation {
            prompt: "write the nonce".to_string(),
            model: None,
            cli_args: Vec::new(),
        };

        // Headless: exec + skip-git-repo-check + `-s danger-full-access`, in order.
        let (_p, argv) = runner.provider_command(Backend::Codex, &inv, Mode::Headless);
        let sandbox_at = argv.iter().position(|a| a == "-s").unwrap_or_else(|| {
            panic!("headless codex argv must carry a `-s` sandbox flag: {argv:?}")
        });
        assert_eq!(
            argv.get(sandbox_at + 1).map(String::as_str),
            Some("danger-full-access"),
            "headless codex must run `-s danger-full-access` or its writes are dropped: {argv:?}"
        );
        assert!(
            argv.contains(&"exec".to_string())
                && argv.contains(&"--skip-git-repo-check".to_string()),
            "headless codex must still lead with `exec --skip-git-repo-check`: {argv:?}"
        );

        // Interactive: no exec, no sandbox override — codex keeps its own default.
        let (_p, argv) = interactive_command(
            &runner,
            &ResolvedDispatch {
                backend: Backend::Codex,
                mode: dispatch_mode("interactive"),
                invocation: inv.clone(),
                agent_env: Vec::new(),
            },
        );
        assert!(
            !argv.contains(&"danger-full-access".to_string()),
            "an interactive codex session must not be forced to danger-full-access: {argv:?}"
        );
    }

    /// The RESOLVE-FAULT path must also be promptless-proof.
    ///
    /// `build_prompt` guarantees "never promptless", but the fault path does not
    /// go through it — a failed `resolve_dispatch` (missing/malformed agent) is
    /// caught by `execute_claimed` and substituted with `ResolvedDispatch::fallback`.
    /// That substitution used to be `unwrap_or_default()`, whose empty prompt
    /// recreated the exact bug the prompt threading fixed: the provider spawns and
    /// exits 1 instead of running. The comment claimed a misconfigured agent
    /// "still runs"; it could not.
    #[tokio::test]
    async fn dispatch_fault_still_yields_a_runnable_prompt() {
        let dir = tempfile::tempdir().unwrap();
        let store = ainb_hangar_store::Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();

        // A task whose agent does not exist: resolve MUST fail, which is what
        // drives `execute_claimed` onto the fallback.
        let err = resolve_dispatch(pool, "agent-that-does-not-exist", None, Mode::Headless).await;
        assert!(err.is_err(), "a missing agent must fail to resolve");

        let disp = ResolvedDispatch::fallback(Mode::Headless);
        assert!(
            !disp.invocation.prompt.trim().is_empty(),
            "the fault fallback must carry a real brief, or the provider exits 1 \
             without doing any work"
        );
        assert_eq!(disp.invocation.prompt, FALLBACK_PROMPT);

        // …and it survives all the way onto the argv, which is what the provider
        // actually sees.
        let runner = Runner::new(RunnerConfig {
            claude_path: "claude".into(),
            codex_path: "codex".into(),
            copilot_path: "copilot".into(),
            max_runtime: Duration::from_secs(1),
            tail_lines: 1,
            sandbox: false,
        });
        let (_p, argv) = runner.provider_command(disp.backend, &disp.invocation, Mode::Headless);
        assert!(
            argv.ends_with(&["--".to_string(), FALLBACK_PROMPT.to_string()]),
            "the fallback brief must reach the provider argv: {argv:?}"
        );
    }

    /// Provider-honoring proof: `resolve_dispatch` selects the backend from the
    /// AGENT's provider, overriding the runtime's advertised default. A `codex`
    /// agent bound to the single `claude`-advertised host runtime dispatches the
    /// codex backend, a `copilot` agent dispatches the copilot backend, a `claude`
    /// agent dispatches claude, and an agent with no override falls back to the
    /// runtime's provider.
    #[tokio::test]
    async fn resolve_dispatch_honours_the_agent_provider_over_the_runtime() {
        use ainb_hangar_store::bootstrap;
        use ainb_hangar_store::repo::agent::{Agent, AgentRepo};

        let dir = tempfile::tempdir().unwrap();
        let store = ainb_hangar_store::Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        let ws = bootstrap::ensure_default_workspace(pool).await.unwrap();
        // The single host runtime advertises claude.
        bootstrap::ensure_runtime(pool, &bootstrap::default_runtime_id(), 1)
            .await
            .unwrap();

        // A codex agent on that claude-advertised runtime → codex backend.
        let codex = bootstrap::create_agent(pool, &ws, "coder", "codex", None).await.unwrap();
        let disp = resolve_dispatch(pool, &codex.id, None, Mode::Headless).await.unwrap();
        assert_eq!(
            disp.backend,
            Backend::Codex,
            "a codex agent must dispatch the codex backend, not the runtime's claude"
        );

        // A copilot agent on that claude-advertised runtime → copilot backend
        // (no more silent claude fallback).
        let copilot = bootstrap::create_agent(pool, &ws, "helper", "copilot", None).await.unwrap();
        let disp = resolve_dispatch(pool, &copilot.id, None, Mode::Headless).await.unwrap();
        assert_eq!(
            disp.backend,
            Backend::Copilot,
            "a copilot agent must dispatch the copilot backend, not fall back to claude"
        );

        // A claude agent on the same runtime → claude backend.
        let claude = bootstrap::create_agent(pool, &ws, "writer", "claude", None).await.unwrap();
        let disp = resolve_dispatch(pool, &claude.id, None, Mode::Headless).await.unwrap();
        assert_eq!(disp.backend, Backend::Claude);

        // An agent with NO provider override falls back to the runtime's provider.
        let owner = bootstrap::default_owner_id(pool).await.unwrap().unwrap();
        let bare = Agent {
            id: "bare-agent".into(),
            workspace_id: ws.clone(),
            name: "bare".into(),
            runtime_id: bootstrap::default_runtime_id(),
            instructions: None,
            visibility: "workspace".into(),
            owner_id: owner,
            archived: false,
            model: None,
            cli_args: Vec::new(),
            mcp_config: None,
            thinking: None,
            agent_env: Vec::new(),
            provider: None,
            token_budget: None,
        };
        AgentRepo::insert(pool, &bare).await.unwrap();
        let disp = resolve_dispatch(pool, "bare-agent", None, Mode::Headless).await.unwrap();
        assert_eq!(
            disp.backend,
            Backend::Claude,
            "no per-agent override falls back to the runtime's advertised provider"
        );
    }
}
