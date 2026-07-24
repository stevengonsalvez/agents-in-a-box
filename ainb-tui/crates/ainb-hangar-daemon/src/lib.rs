//! Library half of the `ainb-hangar-daemon` binary.
//!
//! P0 ships only the boot path: open the [`Store`] (which applies every
//! migration), log a `ready` line, then idle. The idle loop is intentionally a
//! no-op stub — later phases replace [`run_idle`] with the task FSM and the
//! JSON-RPC handlers without touching `main.rs`.
//!
//! Keeping this logic in a library (rather than inline in `main`) means the
//! daemon's behaviour is unit-testable and the future FSM swap is a one-function
//! change behind a stable signature.

use ainb_hangar_store::Store;

use crate::run_loop::{DaemonConfig, run};

/// Beads CLI adapter — shells out to `bd` and parses `--json` (P2.2).
///
/// The answer router (spec P2): deliver one attention answer from any surface,
/// exactly once (first-answer-wins), into the right session (C1 misroute guard),
/// via the one verified send path. Backs the `attention/answer` RPC.
pub mod answer;
/// ATC on the daemon (D12, spec P9 §4.7): the instance registry, the heartbeat
/// cron (the launchd/systemd timer's daemon-native replacement — reusing the
/// autopilot scheduler's DB-durable tick loop), the store-backed retry cap, and
/// [`atc::raise_escalation`] — the path that turns a stuck session into an
/// `escalation` attention row so it reaches the phone/web push instead of
/// dead-ending in `task-log.md`.
pub mod atc;
/// The attention ingest producer (spec P2, D10): the daemon's own tail of the
/// shared hook `events.jsonl` into the `attention` table — classifies every
/// qualifying session event and raises an answerable row + an `AttentionRaised`
/// nudge. The producer half of the answerable-inbox pipeline.
pub mod attention_ingest;
/// [`beads_adapter::BdClient`] is the sync layer's gateway to Stevie's existing
/// issue tracker: `create` / `close` / `list` / `show`, each passing `BEADS_DIR`
/// explicitly and serialised by an O_EXCL pidfile lock.
pub mod beads_adapter;
/// Hangar ↔ Beads sync (P2.3): the outbound mirror of Hangar issue lifecycle
/// changes into `bd`, recorded in `beads_mapping`.
///
/// [`beads_sync::OutboundSync`] is source-gated (swarm issues skip), idempotent
/// (replays short-circuit via the mapping repo), and non-fatal (a `bd` failure
/// surfaces a [`beads_sync::SyncError`] without corrupting Hangar state).
pub mod beads_sync;
/// The board auto-move dispatch hook (P4 / D8): on every task FSM transition the
/// claim loop moves the task's issue card to the `fsm_state`-matched auto-move
/// column of every board carrying it (best-effort, never blocks the FSM).
pub mod board;
/// The in-flight run kill registry (tcp T3 / F6): the process-global seam a
/// cancel RPC uses to signal the claim loop to stop a live run (headless process
/// group / interactive tmux session). See [`cancel::registry`].
pub mod cancel;
/// Daemon-level `claude` credential resolution (Keychain/env -> child env).
///
/// The confined child can reach neither the Keychain nor the operator's
/// `~/.claude`, so the unsandboxed daemon resolves the token and injects it as
/// ONE env var, for the `claude` backend only.
pub mod claude_cred;
/// Fresh-home boot seed: lay down the default workspace + runtime + one starter
/// agent so an empty `hangar.db` "just works" (a runtime shows in the Daemon
/// pane and the Squad create gate is already cleared). Idempotent + non-clobbering.
pub mod default_home;
/// Env allowlist config + task-env builder (P5.3).
///
/// Loads/saves `~/.agents-in-a-box/hangar/env.allow.toml` (foreign sections preserved,
/// atomic write) and exposes [`dispatch::build_task_env`] — the env-build seam
/// the claim loop uses before spawning a provider: ambient env is filtered by
/// [`ainb_hangar_core::env_policy`] then keychain keys are layered on top.
pub mod dispatch;
/// The durable event outbox drain (T1 / architecture §4.1–§4.2).
///
/// [`event_outbox::spawn`] drains the [`events::EventBroker`]'s lossless outbox
/// channel and appends every emitted
/// [`ainb_hangar_proto::events::HangarEvent`] to the `event_log` table
/// (migration 0024) with a monotonic `seq`. This is the durability that lets a
/// reconnecting or late-joining subscriber resume the bus from its last cursor —
/// the raw, replayable log beneath the read/unread inbox digest.
pub mod event_outbox;
/// The daemon's in-process event broker (e38.2).
///
/// [`events::EventBroker`] fans typed [`ainb_hangar_proto::events::HangarEvent`]s
/// from the daemon's mutation paths (claim-loop FSM finalize, RPC
/// `task_transition`, autopilot scheduler / fire-now) out to the RPC server's
/// per-connection, workspace-scoped subscribers — the producer half of the
/// dual-channel design the plugin's `StreamClient` has decoded since P3.
pub mod events;
/// Per-task execution-environment layout: workdir/output/logs + `.gc_meta.json`
/// (P1.6).
pub mod execenv;
/// Authoritative Fleet reducer fed by hooks, provider events, and tmux discovery.
pub mod fleet;
/// Claude and Codex provider transports for authoritative Fleet control.
pub mod fleet_provider;
/// The task-lifecycle state machine (T8): `statig` typed compile-time
/// transitions.
///
/// [`fsm::LifecycleGuard`] types the in-process transition ordering the claim
/// loop drives (`dispatched -> running -> done|failed`); the migration-0012 SQL
/// claim guard and the store-service idempotent finalize remain the DB-level
/// enforcers. Crate-internal: the generated `State` enum is an implementation
/// detail of the typed FSM, not part of the daemon's public surface.
mod fsm;
/// In-memory daemon health stats for the daemon-health pane (P8.5).
///
/// The rolling task-throughput ring buffer + the bounded claim-slot cache figure.
pub mod health_stats;
/// The inbox aggregator: the writer that turns the live event stream into the
/// durable notification inbox (e38.14).
///
/// [`inbox_aggregator::spawn`] subscribes to the [`events::EventBroker`] like an
/// RPC connection does, maps each issue / comment / task
/// [`ainb_hangar_proto::events::HangarEvent`] to one inbox row, and writes it to
/// the `inbox_entry` table (migration 0021). This is the take-effect seam that
/// makes the inbox a real aggregate instead of a schema with no writer.
pub mod inbox_aggregator;
/// y0f: periodic cap of the daemon's own parent completion inbox
/// (`inbox/hangar-daemon.jsonl`) — pure exhaust the daemon never drains, bounded
/// to its most-recent-N records on the sweeper tick. See [`inbox_sweep`].
pub(crate) mod inbox_sweep;
/// Interactive-mode launch: a real, attachable tmux session per task (ccc / D6).
///
/// [`interactive::spawn`] runs the provider inside a detached tmux session
/// (`tmux_hangar-<task_id>`) the way `ainb run` sessions look, and
/// [`interactive::TmuxRun::wait`] maps its recorded exit code onto the same
/// [`runner::RunOutcome`] the headless path returns, so the finalize seam is
/// shared across both modes.
pub mod interactive;
/// Dispatch-time materialisation of an agent's skills into its per-task env
/// (P6.4).
///
/// After the worktree exists and before the provider spawns,
/// [`materialise::materialise_for_agent`] copies each attached skill bundle into
/// the provider's expected layout ([`materialise::ProviderSkillLayout`]):
/// Claude/Codex/Cursor root under the task root (sibling of `workdir`, so the
/// git worktree stays clean) and are pointed there via a `*_HOME` env var;
/// Gemini/Default/Copilot root inside `workdir`. Files are copied, never
/// symlinked; `scripts/` files get the unix executable bit.
pub mod materialise;
/// `@handle` mention parsing over a comment body (e38.7).
///
/// [`mentions::parse_mentions`] extracts the distinct `@agent` handles from a
/// comment body; the `comment_add` handler resolves each against the workspace's
/// agents and enqueues a task for every match, so a user `@`-mentioning an agent
/// in a comment spawns that agent's task.
pub mod mentions;
/// Raise-time notification-channel resolution (tcp T5): read the notify rules for
/// a `(kind, workspace)` and return the [`ChannelSet`](ainb_hangar_core::channel::ChannelSet)
/// the daemon stamps onto the row + event, computed once at emit.
pub mod notify;
/// Daemon observability bootstrap (P8.1).
///
/// Installs the `tracing` subscriber with the rolling JSONL sink under
/// `<hangar_home>/hangar/logs` and an `RUST_LOG` env filter.
/// [`observability::install`] returns a `WorkerGuard` the daemon `main` holds
/// for the process lifetime, and exposes an `otlp` seam for P8.2.
pub mod observability;
/// `gh`-backed PR status fetch behind an injectable seam (e38.34).
///
/// Fetches a captured PR's CI rollup + mergeability + merge state by shelling out
/// to `gh pr view --json statusCheckRollup,mergeable,state` behind the
/// [`pr_status::PrStatusProvider`] trait, so the task-detail badge can surface
/// real check status and the refresh path can auto-move a merged PR's issue to
/// Done. Every failure (absent / unauthenticated `gh`, no checks) degrades to an
/// all-`Unknown` status — never a panic.
pub mod pr_status;
/// P5 agent profiles: the on-disk master store, the DB-index reconciler +
/// fs-watch, and compile-on-dispatch of the tool-native files (D14-D16, T6).
///
/// The pure format + the Claude/Codex down-compilers live in
/// [`ainb_hangar_core::profile`]; this module is the IO around them — read/write
/// masters under `{hangar_home}/profiles/`, reconcile the
/// [`ainb_hangar_store::repo::profile`] index against disk, and materialise the
/// resolved artifacts into a task's execution env at dispatch.
pub mod profile;
/// Durable issue-comment emission at run-loop lifecycle checkpoints (e38.6).
///
/// At each FSM checkpoint the claim loop reaches — the task starts running, and
/// the run finishes (success / failure / timeout) —
/// [`progress_comment::emit_checkpoint`] writes one **agent-authored** comment to
/// the task's issue so the agent's activity survives beyond the bounded
/// transcript buffer. Scoped to tasks bound to an issue (a `NULL`-issue chat task
/// is skipped); best-effort (a write fault is logged, never blocks the task FSM).
pub mod progress_comment;
/// The daemon's `UnixListener` JSON-RPC server (P4.10).
///
/// Binds `{hangar_home}/hangar.sock`, serves `workspace/subscribe`, `ping`, and
/// the four `hangar/*` snapshot RPCs ([`rpc::snapshots`]) backed by the store
/// repos. The plugin dials this socket through the host `unix_socket_dial` cap
/// to populate its screens with live data.
pub mod rpc;
/// The daemon's claim loop + sweeper scheduler (P1.7).
///
/// Polls [`ainb_hangar_store::service::claim`] for the oldest queued task bound
/// to this daemon's runtime and walks it through the FSM via the provider
/// [`runner`]. Driven by [`run_loop::DaemonConfig::from_env`].
pub mod run_loop;
/// Agent CLI subprocess execution — the `claude` provider (P1.7).
///
/// Spawns the provider binary in a task's isolated [`execenv::ExecEnv`] with a
/// deny-by-default env, tees its JSONL stdout to `logs/claude.jsonl`, pins the
/// first `session_id`, and enforces a runtime deadline. Returns a
/// [`runner::RunOutcome`] the claim loop maps onto the FSM.
pub mod runner;
/// Self-registration of the daemon's own runtime on boot (e38.20).
///
/// [`runtime_register::register_runtime`] idempotently upserts an
/// `agent_runtime` row keyed on `HANGAR_DAEMON_RUNTIME_ID` so a real (non-test)
/// daemon advertises a runtime the moment it boots — closing the gap where every
/// `agent_runtime` insert lived only in test fixtures.
pub mod runtime_register;
/// The autopilot scheduler thread + cron tick loop (P7.3).
///
/// [`scheduler::AutopilotScheduler`] is a daemon-global tokio task that wakes at
/// the earliest enabled autopilot's `next_tick_at`, fires it through P7.4's
/// [`ainb_hangar_store::repo::autopilot_run::fire_autopilot_tick`] (or skips when
/// the autopilot is at its `max_concurrent_runs` limit, emitting
/// `autopilot.tick_skipped`), then recomputes the next tick from the fired slot
/// to avoid drift. A [`tokio_util::sync::CancellationToken`] exits it cleanly.
pub mod scheduler;
/// Deterministic P4 seed fixture for the e2e tripwires (`test-support` only).
///
/// Writes a `default` workspace with issues/agents/skills + a running task into
/// a fresh store so the tmux tripwires can launch `ainb tui` against a seeded
/// `hangar.db` and assert live rows render.
#[cfg(any(test, feature = "test-support"))]
pub mod seed;
/// Toolkit-directory skill importer behind `ainb hangar skills sync` (P6.2).
///
/// Walks a `ainb-toolkit/skills/`-shaped tree (`<name>/SKILL.md` + nested
/// assets), parses each skill's YAML frontmatter, validates the whole batch
/// (uniqueness + parse) before any write, then upserts every skill
/// workspace-scoped via
/// [`ainb_hangar_store::repo::skill::SkillRepo::upsert_by_name`] — idempotent
/// and all-or-nothing.
pub mod skills_sync;
/// Claim-time squad-leader briefing builder (multica `squad_briefing.go` parity,
/// gap #7).
///
/// [`squad_briefing::build_squad_leader_briefing`] composes the Operating
/// Protocol + Squad Roster appended to a leader agent's run `CLAUDE.md` at claim
/// time, so a squad-leader task runs with the coordinator role + the roster of
/// members it can delegate to. Member tasks and non-squad tasks get no briefing.
pub mod squad_briefing;
/// Auto-standup watcher (D13; spec P9 §4.8).
///
/// [`standup::StandupWatcher`] is a daemon-global periodic scan that WRITES
/// `/standup` into a stagnant, idle-at-prompt session via the one verified send
/// path — behind every guardrail: a global toggle (default OFF, opt-in), a per-session
/// opt-out, a 60-minute per-session cooldown, and a max-one-concurrent cap. The
/// pure [`standup::decide_standup`] gate is the exhaustively-tested heart; a busy
/// / mid-turn session is NEVER written to (hook status, never a pane heuristic).
pub mod standup;
/// TTL sweepers + stale-dispatch reclaim (P1.4).
///
/// The daemon's tokio runtime registers these as periodic tasks; they are also
/// callable directly (with an injected clock) for deterministic testing.
pub mod sweeper;
/// `ainb hangar templates use <name>` transactional materialisation (P6.3).
///
/// Turns an embedded curated [`ainb_hangar_core::template::AgentTemplate`] into a
/// live `agent` row plus its `agent_skill` attachments in one transaction.
/// Referenced skills must be pre-imported (P6.2 `skills sync`); a missing one is
/// a hard [`templates::TemplateUseError::SkillNotImported`] with a sync hint and
/// nothing is written. Idempotent by agent name within the workspace.
pub mod templates;
/// Danger-full-access warning emission at provider invocation (P5.6).
pub mod warnings;
/// The local HTTP webhook ingress for webhook-triggered autopilots (e38.18).
///
/// A hand-rolled HTTP/1.1 handler over a `tokio` `TcpListener` bound to
/// `127.0.0.1` only. Serves `POST /hangar/webhook/<autopilot_id>`,
/// constant-time-verifies the request's HMAC-SHA256 body signature against the
/// autopilot's secret, applies the optional event filter, and fires the
/// autopilot through the existing P7.4 enqueue path on success. An unsigned /
/// wrong-signature / disabled / unknown request fires nothing (401/403/404) and
/// is recorded in the delivery audit log.
pub mod webhook_ingress;
/// Per-run working-dir provisioning from a card's `repo_ref` (spec F5): a
/// volatile `ainb/<slug>` worktree for a real repo, an in-place git-inited
/// scratch repo, or the fallback execenv workdir for a chat task.
pub mod workdir_provision;
/// Git-worktree integration for per-task working dirs (P1.6).
pub mod worktree;

/// Resolve the directory that holds `hangar.db`.
///
/// Delegates to [`ainb_hangar_core::hangar_home`] so the database, the per-task
/// env tree, the `hangar.sock` the RPC server binds, and the log dir all share
/// one root: `$AINB_HANGAR_HOME` when set and non-empty, else
/// `~/.agents-in-a-box`.
///
/// # Errors
///
/// Returns an error if neither `$AINB_HANGAR_HOME` is set nor a home directory
/// can be resolved.
pub fn hangar_dir() -> anyhow::Result<std::path::PathBuf> {
    ainb_hangar_core::hangar_home()
        .ok_or_else(|| anyhow::anyhow!("could not resolve home directory"))
}

/// Resolve the daemon's structured-log directory: `<hangar_home>/hangar/logs`.
///
/// Defaults to `~/.agents-in-a-box/hangar/logs`. The P8.1 rolling JSONL sink writes
/// `daemon.<date>` files here; the P8.6 CLI/TUI logs-tail surfaces read them
/// back. Shares the one home resolution with [`hangar_dir`].
///
/// # Errors
///
/// Propagates [`hangar_dir`]'s error if the Hangar home cannot be resolved.
pub fn log_dir() -> anyhow::Result<std::path::PathBuf> {
    Ok(hangar_dir()?.join("hangar").join("logs"))
}

/// Boot the daemon: open the persistence layer and run the claim loop.
///
/// Resolves the database directory the same way every Hangar consumer does
/// (`$AINB_HANGAR_HOME` override, else `~/.agents-in-a-box`), opens (creating if absent)
/// `hangar.db`, applies all embedded migrations, logs a `ready` line, then
/// hands off to the [`run_loop`] FSM driver (claim → execute → finalize, plus
/// the periodic sweepers).
///
/// When `once` is `true` the function returns as soon as the daemon is ready
/// (one-shot mode used by the boot tripwire). Otherwise it blocks in
/// [`run_loop::run`] until interrupted.
///
/// # Errors
///
/// Returns an error if the store cannot be opened (directory not writable, a
/// migration fails) or if the run loop's shutdown handler fails.
pub async fn boot(once: bool) -> anyhow::Result<()> {
    let dir = hangar_dir()?;

    // Observability tripwire seam (P8.1): when `$AINB_HANGAR_BOOT_TASK_ID` is set
    // the daemon emits exactly one structured boot event carrying that id. The
    // `it_subscriber_writes_jsonl` integration test sets it to prove the JSONL
    // sink installed in `main` lays down a single matching JSON line. Production
    // boots never set it, so this is a no-op in normal operation.
    if let Some(task_id) = std::env::var_os("AINB_HANGAR_BOOT_TASK_ID")
        .and_then(|v| v.into_string().ok())
        .filter(|v| !v.is_empty())
    {
        tracing::info!(task_id = %task_id, "boot");
    }

    let store: Store = Store::open_in(&dir).await?;

    // Fresh-home boot seed: lay down the default workspace + runtime + one
    // starter agent so an empty home "just works" (a runtime shows in the Daemon
    // pane, the agent picker is non-empty, and the Squad create gate is already
    // cleared). Idempotent + non-clobbering, and it self-registers the runtime
    // under the SAME default id the claim loop keys off (subsuming the old
    // e38.20 self-register). A failure is non-fatal — the daemon must still
    // sweep + serve — so it is logged and swallowed here.
    if let Err(e) = crate::default_home::ensure_default_home(store.pool()).await {
        tracing::warn!(error = %e, "fresh-home boot seed failed (daemon continues)");
    }

    // P8.5: the in-memory health stats collector — shared between the RPC server
    // (which snapshots the rolling throughput ring for the `hangar/daemon_health`
    // pane) and the run loop's FSM finalize path (which records each task's
    // terminal outcome into the ring).
    let stats = std::sync::Arc::new(crate::health_stats::HealthStats::default());

    // e38.2 + T1: the daemon-global event broker, built WITH a durable outbox.
    // Mutation paths (the claim loop's FSM steps, the RPC mutations, the
    // autopilot scheduler) publish typed `HangarEvent`s through cloned sinks;
    // the RPC server forwards them to authenticated, workspace-subscribed
    // connections (live, lossy) AND every event is queued on the lossless outbox
    // channel returned here for the outbox drain to persist. With no subscriber
    // the live broadcast is dropped silently, so the broker costs nothing when no
    // TUI is attached — but the durable log still records every event for replay.
    let (broker, outbox_rx) = crate::events::EventBroker::with_outbox();

    // T1: spawn the event-outbox drain — the writer of the durable, replayable
    // event log. It pulls every emitted event off the lossless outbox channel
    // and appends it to `event_log` (migration 0024) with a monotonic `seq`, so a
    // reconnecting or late-joining plugin resumes the bus from its last cursor.
    // Wired BEFORE the RPC server and the claim loop so the first mutation's
    // event is already persisted. The handle is dropped (process exit tears the
    // task down, mirroring the sweepers); a failed append is logged inside the
    // task, never fatal.
    let _outbox = crate::event_outbox::spawn(store.pool().clone(), outbox_rx);

    // e38.14: spawn the inbox aggregator — the writer half of the durable
    // notification inbox. It subscribes to the broker (exactly like an RPC
    // connection) and folds every issue/comment/task event into the
    // `inbox_entry` table, so live events that were once broadcast-only now land
    // durably with an unread count. Subscribed BEFORE the RPC server and the
    // claim loop come up, so the first mutation's event is already aggregated.
    // The handle is dropped (process exit tears the task down, mirroring the
    // sweepers); a failed write is logged inside the task, never fatal.
    let _inbox = crate::inbox_aggregator::spawn(store.pool().clone(), broker.subscribe());

    // Spec P2 (D10): spawn the attention ingest producer — the daemon's own tail
    // of the shared hook `events.jsonl` (`~/.agents-in-a-box/events.jsonl`, the
    // SAME file notifyd reads) into the `attention` table. It classifies every
    // qualifying session event (ASK > ERR > IDLE > WAIT) and raises an answerable
    // row + a fleet-wide `AttentionRaised` nudge. Its byte cursor lives under the
    // hangar home so the T2 store boundary holds (no cross-read of notifyd's
    // rusqlite). The handle is dropped (process exit tears the task down,
    // mirroring the inbox aggregator); a failed ingest is logged, never fatal.
    // The event-log path is home-based (matching the hook appender) independent
    // of `$AINB_HANGAR_HOME`; the cursor rides the resolved hangar dir.
    if let Some(events_jsonl) =
        dirs::home_dir().map(|h| h.join(".agents-in-a-box").join("events.jsonl"))
    {
        let cursor = dir.join("hangar").join("attention_ingest.offset");
        let _attention_ingest = crate::attention_ingest::AttentionIngest::new(
            store.pool().clone(),
            broker.sink(),
            events_jsonl,
            cursor,
        )
        .spawn();
    }

    // Tmux reconciliation keeps unhooked and standalone provider sessions in
    // the canonical Fleet roster as degraded rows with exact pane identity.
    let _fleet_tmux = crate::fleet::spawn_tmux_reconciler(store.pool().clone(), broker.sink());

    // Managed Codex transport starts independently from daemon readiness. A
    // missing or incompatible Codex binary leaves hook and tmux observation
    // running, while the service records an honest transport downgrade.
    let _codex_manager = (!once
        && std::env::var_os("AINB_CODEX_MANAGED")
            .as_deref()
            .is_none_or(|value| value != "0"))
    .then(|| {
        let binary = std::env::var_os("AINB_CODEX_BIN").unwrap_or_else(|| "codex".into());
        let socket = dir.join("codex-app-server.sock");
        crate::fleet_provider::codex_manager::spawn_service(
            crate::fleet_provider::codex_manager::CodexManagerConfig::new(binary, socket),
            store.pool().clone(),
            broker.sink(),
        )
    });

    // P5 (T6): reconcile the agent-profile index against the on-disk masters and
    // spawn an fs-watch so an edit-on-disk of `{hangar_home}/profiles/<slug>.md`
    // is picked up into the `profile` index without an RPC. Best-effort — a
    // watcher-setup fault is logged inside the helper and swallowed (the
    // `profile/upsert` RPC still refreshes the index directly). The returned
    // watcher is HELD for the process lifetime; dropping it would stop the watch,
    // so it lives in `boot`'s scope alongside the other background handles.
    let _profile_watch = crate::profile::profiles_dir()
        .and_then(|dir| crate::profile::spawn_index_watch(store.pool().clone(), dir));

    // P4.10: bind the JSON-RPC socket beside the database and serve plugin
    // connections on a background task. A bind failure is non-fatal — the
    // daemon's claim loop must still run even if no plugin can reach it (and a
    // stale socket from a crashed daemon is removed in `rpc::bind`).
    //
    // e38.1: the socket-auth credential is ensured BEFORE the bind so the
    // token file exists by the time a client can dial (clients read it and
    // present it on their first frame). A mint failure disables the listener
    // (an unauthenticated control plane must never come up) but stays
    // non-fatal to the claim loop, mirroring the bind-failure path.
    let socket_path = rpc::socket_path_in(&dir);
    match rpc::auth::ensure_socket_token(store.pool(), &dir).await {
        Ok(token_path) => match rpc::bind(&socket_path) {
            Ok(listener) => {
                let health = rpc::DaemonHealth {
                    socket_path: socket_path.to_string_lossy().into_owned(),
                    pid: std::process::id(),
                    started_at: std::time::Instant::now(),
                    version: env!("CARGO_PKG_VERSION").to_string(),
                    stats: stats.clone(),
                };
                tracing::info!(
                    socket = %socket_path.display(),
                    token_file = %token_path.display(),
                    "hangar rpc listening"
                );
                tokio::spawn(rpc::serve(
                    listener,
                    store.pool().clone(),
                    health,
                    broker.clone(),
                ));
            }
            Err(e) => {
                tracing::warn!(error = %e, socket = %socket_path.display(), "hangar rpc bind failed");
            }
        },
        Err(e) => {
            tracing::warn!(
                error = %e,
                socket = %socket_path.display(),
                "hangar rpc socket-token mint failed; rpc listener disabled"
            );
        }
    }

    // P7.3: spawn the autopilot scheduler. It is a daemon-global cron tick loop
    // over every enabled autopilot; guarded so a scheduler fault is non-fatal to
    // the claim loop (one bad autopilot must never down the daemon). The token is
    // never cancelled here (process exit tears the task down); a future
    // supervisor can cancel it for graceful shutdown.
    {
        use std::sync::Arc;

        use ainb_hangar_core::clock::SystemClock;
        use tokio_util::sync::CancellationToken;

        let scheduler = crate::scheduler::AutopilotScheduler::new(
            store.pool().clone(),
            Arc::new(SystemClock),
            CancellationToken::new(),
        )
        .with_hangar_events(broker.sink());
        tokio::spawn(scheduler.run());
        tracing::info!("autopilot scheduler spawned");
    }

    // Spec P9 (D13): spawn the auto-standup watcher — a daemon-global periodic
    // scan that WRITES `/standup` into a stagnant, idle-at-prompt session behind
    // every guardrail (global toggle default OFF/opt-in, per-session opt-out, 60-min
    // cooldown, max-one concurrent). It writes via the one verified send path
    // (INV-2) and raises a `waiting` "standup ready" attention row when a fired
    // standup's turn completes. Non-fatal like the scheduler: a discovery / send /
    // store fault is warned and degraded, never a daemon-down. The handle is
    // dropped (process exit tears the task down).
    let _standup = crate::standup::StandupWatcher::spawn(store.pool().clone(), broker.sink());
    tracing::info!("auto-standup watcher spawned");

    // Spec P9 (D12): spawn the ATC heartbeat cron — the launchd/systemd timer's
    // daemon-native replacement. It reuses the autopilot scheduler's DB-durable
    // tick loop over `atc_instance.next_tick_at`, fires each instance's heartbeat
    // on its cron (enforcing the store-backed retry cap and escalating exhausted
    // sessions through the attention pipeline), and reschedules from the fired
    // slot. Non-fatal like the scheduler; the handle is dropped (process exit
    // tears the task down).
    let _atc_heartbeat =
        crate::atc::AtcHeartbeatScheduler::spawn(store.pool().clone(), broker.sink());
    tracing::info!("ATC heartbeat cron spawned");

    // e38.18: the webhook ingress. OPT-IN — it only binds when
    // `$AINB_HANGAR_WEBHOOK_PORT` is set (an untrusted HTTP surface must not come
    // up by default). It binds 127.0.0.1 ONLY (never 0.0.0.0), so it is
    // unreachable off-host. A bind failure is non-fatal to the claim loop,
    // mirroring the RPC socket. Pass `0` for an ephemeral port.
    if let Some(port) = std::env::var_os("AINB_HANGAR_WEBHOOK_PORT")
        .and_then(|v| v.into_string().ok())
        .and_then(|v| v.trim().parse::<u16>().ok())
    {
        use std::sync::Arc;

        use ainb_hangar_core::clock::SystemClock;
        use ainb_hangar_store::repo::autopilot_webhook::WebhookSecretStore;

        match crate::webhook_ingress::bind(port).await {
            Ok(listener) => {
                let addr = listener.local_addr().ok();
                tracing::info!(
                    bind = ?addr,
                    "hangar webhook ingress listening (127.0.0.1 only)"
                );
                let secrets = Arc::new(WebhookSecretStore::in_home(&dir));
                let clock: Arc<dyn ainb_hangar_core::clock::HangarClock + Send + Sync> =
                    Arc::new(SystemClock);
                tokio::spawn(crate::webhook_ingress::serve(
                    listener,
                    store.pool().clone(),
                    secrets,
                    clock,
                ));
            }
            Err(e) => {
                tracing::warn!(error = %e, port, "hangar webhook ingress bind failed");
            }
        }
    }

    tracing::info!(idle = true, "ainb-hangar-daemon ready idle=true");
    if once {
        return Ok(());
    }
    let mut cfg = DaemonConfig::from_env();
    // The claim loop MUST key off the runtime id that is actually registered (and
    // that the seeded/created agents are bound to). A runtime cannot be renamed —
    // `agent.runtime_id` is an enforced FK — so an existing row's id wins over a
    // changed `HANGAR_DAEMON_RUNTIME_ID` (which is warned about, not obeyed).
    // Resolving here keeps the registered row, the agents, and the claim loop on
    // ONE id instead of claiming for an id nothing is bound to.
    let now = ainb_hangar_core::clock::HangarClock::now_ms(&ainb_hangar_core::clock::SystemClock);
    cfg.runtime_id = Some(crate::runtime_register::effective_runtime_id(store.pool(), now).await);
    run(store.pool().clone(), cfg, stats, broker.sink()).await
}
