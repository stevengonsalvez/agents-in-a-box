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

/// The daemon-owned ACP agent pool (plan Phase 5): one adapter process per
/// PROVIDER hosting many sessions, demultiplexed by ACP `sessionId`, plus the
/// shared [`acp_pool::converge_dirty_session`] routine the boot scan
/// ([`acp_pool::converge_dirty_sessions_at_boot`], run from [`boot`]), the
/// process-exit path and the turn-deadline sweep all fan out to.
pub mod acp_pool;
/// One ACP session on the chat bus, from either door: [`acp_session::ensure`]
/// mints the `fleet_session` + `fleet_acp_session` pair for a scope and
/// [`acp_session::enqueue`] puts a prompt on the bus with its PENDING leg.
/// `fleet/acp_session_create` and a task caller share these two transactions.
pub mod acp_session;
/// The ACP task executor (move 1, step A5): the third arm of `execute_claimed`'s
/// exec-path branch, selected by `HANGAR_TASK_EXECUTOR=acp`. Runs a task's brief
/// as one ACP turn against a PER-TASK adapter process and maps the delivery leg
/// onto the same [`runner::RunOutcome`] the process executor returns.
pub mod acp_task;
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
/// The fleet copilot's guardrail gate and its confirm cards (buzz-port part 2).
///
/// The classifier and the argument projection are `ainb-fleet-tools`'; the
/// parking, the expiry, the activity feed and the copilot's authorship live
/// here, because only the daemon owns the store and the event broker.
pub mod copilot;
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
/// Hourly `fleet_provider_event` retention: raw-payload eviction on rows a
/// reducer has already consumed.
pub mod fleet_provider_retention;
/// Bounded live provider-quota projection for the public Fleet RPC.
pub mod fleet_quota;
/// Hourly `fleet_event` retention: payload eviction, row delete, byte ceiling.
pub mod fleet_retention;
/// Bounded canonical Usage projection for the public Fleet RPC.
pub mod fleet_usage;
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
/// The daemon's shutdown seam: which signal stopped it, and how far to tear
/// down.
///
/// [`shutdown::install`] registers the SIGINT and SIGTERM handlers as part of
/// taking the home — before migrations, the socket bind and the tmux reconcile —
/// and hands out [`shutdown::Handle`]s so the boot phase and the run loop wait on
/// the same first cause. The hangar daemon was this workspace's only daemon that
/// ignored SIGTERM, so the supported `daemon stop` killed it on the OS default
/// disposition and none of its teardown ran.
pub mod shutdown;
/// One daemon per hangar home.
///
/// [`single_instance::acquire`] is the first thing [`boot`] does: it takes a
/// cross-process lock on `<hangar_home>/hangar/daemon.lock` (fail-fast) so a
/// second daemon declines instead of racing the incumbent's database and
/// unlinking its socket. Holders are judged by pid liveness AND process
/// identity, so a recycled pid cannot wedge every future boot.
pub mod single_instance;
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

/// Resolve the daemon's pid file: `<hangar_home>/hangar/daemon.pid`.
///
/// The one path both the daemon (which self-registers into it at boot) and the
/// `ainb hangar daemon status/stop/start` verbs read.
#[must_use]
pub fn pid_path_in(dir: &std::path::Path) -> std::path::PathBuf {
    dir.join("hangar").join("daemon.pid")
}

/// Settings in the user's own `~/.codex/config.toml` that govern attached
/// sessions, and would surprise someone who assumed Ainb ran them in isolation.
///
/// In attached mode Ainb's sessions run on the user's real `CODEX_HOME`, so
/// their model, approval policy, hooks, MCP servers and telemetry all apply.
/// That is the point of attaching -- the phone sees the same sessions -- but it
/// is invisible unless we say so, and `--disable apps` on the client does not
/// isolate any of it.
const CODEX_HOME_SETTINGS_WORTH_NAMING: [&str; 8] = [
    "model",
    "model_reasoning_effort",
    "approval_policy",
    "sandbox_mode",
    "shell_environment_policy",
    "notify",
    "mcp_servers",
    "otel",
];

/// Log which of [`CODEX_HOME_SETTINGS_WORTH_NAMING`] the user actually has set,
/// plus any enabled `features.*`, so attaching never silently changes how a
/// session behaves.
///
/// Read-only by design. Ainb must never WRITE `~/.codex/config.toml`: it is the
/// user's file, shared with Codex Desktop and the CLI, and editing it would make
/// Ainb the owner of their model choice, approval policy and plugin surface.
fn warn_about_effective_codex_home_settings(home: Option<&std::path::Path>) {
    let Some(path) = home.map(|home| home.join(".codex/config.toml")) else {
        return;
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return;
    };
    let Ok(parsed) = text.parse::<toml::Value>() else {
        return;
    };
    let mut effective: Vec<String> = CODEX_HOME_SETTINGS_WORTH_NAMING
        .into_iter()
        .filter(|key| parsed.get(key).is_some())
        .map(str::to_string)
        .collect();
    if let Some(features) = parsed.get("features").and_then(toml::Value::as_table) {
        for (name, value) in features {
            if value.as_bool() == Some(true) {
                effective.push(format!("features.{name}"));
            }
        }
    }
    if effective.is_empty() {
        return;
    }
    tracing::warn!(
        config = %path.display(),
        settings = %effective.join(", "),
        "attached Codex sessions run on your own CODEX_HOME, so these settings from \
         your codex config apply to them (Ainb does not modify this file; set \
         `[codex] app_server = \"own\"` to run isolated sessions instead)"
    );
}

/// Count the sessions stranded in the scoped `CODEX_HOME` when we attach.
///
/// They are not lost and nothing is migrated -- they simply live on an
/// app-server the phone cannot see, so the only honest thing is to say how many
/// there are and how to get back to them.
fn note_scoped_sessions_left_behind(hangar_home: &std::path::Path) {
    let scoped = hangar_home.join("codex-home").join("sessions");
    let Ok(entries) = std::fs::read_dir(&scoped) else {
        return;
    };
    let count = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "jsonl"))
        .count();
    if count == 0 {
        return;
    }
    tracing::warn!(
        scoped_home = %hangar_home.join("codex-home").display(),
        sessions = count,
        "these earlier Codex sessions stay in Ainb's scoped home and will not appear \
         in the Codex phone app; nothing was migrated or deleted. Set \
         `[codex] app_server = \"own\"` to go back to them"
    );
}

/// Where Ainb's Codex sessions run: which app-server the hangar daemon drives.
///
/// Resolution order, most specific first:
/// 1. `AINB_CODEX_APP_SERVER` env
/// 2. `app_server` under `[codex]` in `<hangar home>/config/config.toml`
/// 3. the default, `desktop`
///
/// Accepted values:
/// - `desktop` (DEFAULT) — the ChatGPT-managed daemon's control socket. This is
///   the app-server the Codex phone app pairs with, and it runs on the user's
///   real `~/.codex`, so sessions Ainb opens are visible and operable there.
/// - `own` — spawn our own server on a scoped `CODEX_HOME`. Isolated from Codex
///   Desktop (no shared remote-control enrollment identity), but invisible to
///   the phone.
/// - any other value — an explicit socket path.
///
/// Returns `None` for `own`, which is what
/// [`codex_external_socket`] reports as "do not attach".
fn codex_app_server_setting() -> Option<std::ffi::OsString> {
    if let Some(raw) = std::env::var_os("AINB_CODEX_APP_SERVER").filter(|v| !v.is_empty()) {
        return Some(raw);
    }
    match codex_app_server_from_config() {
        ConfigSetting::Set(raw) => Some(raw.into()),
        ConfigSetting::Absent => Some(CODEX_APP_SERVER_DEFAULT.into()),
        // Fail safe, not fail default: keep our own server so a broken config
        // cannot hand a user's sessions to an app-server they share with Codex
        // Desktop without them asking for it.
        ConfigSetting::Unreadable => {
            tracing::warn!(
                "hangar config could not be read; keeping Ainb's own codex app-server \
                 instead of applying the `{CODEX_APP_SERVER_DEFAULT}` default"
            );
            Some("own".into())
        }
    }
}

/// `desktop` is the default because the common case is a developer who wants
/// their Ainb Codex sessions on their phone. `own` remains one config line away
/// for anyone who needs isolation from Codex Desktop.
const CODEX_APP_SERVER_DEFAULT: &str = "desktop";

/// Read `[codex] app_server` from the hangar home's config, if present.
///
/// The daemon deliberately parses the one key it needs rather than depending on
/// `ainb-core`: the TUI crate already depends on this one, so reaching back
/// would be a cycle.
fn codex_app_server_from_config() -> ConfigSetting {
    let Ok(home) = hangar_dir() else {
        return ConfigSetting::Absent;
    };
    let path = home.join("config").join("config.toml");
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return ConfigSetting::Absent;
        }
        Err(error) => {
            tracing::warn!(path = %path.display(), %error, "cannot read hangar config");
            return ConfigSetting::Unreadable;
        }
    };
    let parsed: toml::Value = match text.parse() {
        Ok(parsed) => parsed,
        Err(error) => {
            tracing::warn!(path = %path.display(), %error, "hangar config is not valid TOML");
            return ConfigSetting::Unreadable;
        }
    };
    match parsed.get("codex").and_then(|codex| codex.get("app_server")) {
        None => ConfigSetting::Absent,
        Some(value) => match value.as_str() {
            Some(value) if !value.is_empty() => ConfigSetting::Set(value.to_string()),
            // Present but not a usable string: a typo we must not read as
            // "unset", or the default would silently override an explicit
            // choice.
            _ => {
                tracing::warn!(
                    path = %path.display(),
                    "[codex] app_server must be a non-empty string"
                );
                ConfigSetting::Unreadable
            }
        },
    }
}

/// What the hangar config had to say about `[codex] app_server`.
///
/// `Unreadable` is deliberately NOT folded into `Absent`. A syntax error
/// anywhere in `config.toml` would otherwise read as "nothing configured" and
/// silently apply the `desktop` default, moving a user who had explicitly
/// chosen `own` onto a shared app-server with Codex Desktop. Isolation must
/// never be lost to an unrelated typo.
enum ConfigSetting {
    Set(String),
    Absent,
    Unreadable,
}

/// Socket of an externally managed Codex app-server to attach to.
///
/// `None` means run our own server. See [`codex_app_server_setting`] for the
/// resolution order and accepted values.
///
/// A `desktop` setting whose socket does not exist resolves to `None` as well:
/// Codex Desktop is simply not running, and refusing to start any Codex
/// transport would be worse than quietly using our own server. The caller logs
/// the downgrade.
pub fn codex_external_socket() -> Option<std::path::PathBuf> {
    let resolved =
        codex_external_socket_from(codex_app_server_setting(), dirs::home_dir().as_deref());
    match resolved {
        Some(socket) if !socket.exists() => {
            tracing::warn!(
                socket = %socket.display(),
                "configured Codex app-server socket is absent (is Codex Desktop running?); \
                 falling back to Ainb's own app-server, so these sessions will not appear \
                 in the Codex phone app"
            );
            None
        }
        other => other,
    }
}

/// [`codex_external_socket`]'s pure half: setting plus home in, socket out.
///
/// Split out so the resolution rules are testable without touching process env
/// or the filesystem (env mutation races under parallel tests).
fn codex_external_socket_from(
    raw: Option<std::ffi::OsString>,
    home: Option<&std::path::Path>,
) -> Option<std::path::PathBuf> {
    let raw = raw?;
    if raw.is_empty() || raw == *"own" {
        return None;
    }
    if raw == *"desktop" {
        return home.map(|home| home.join(".codex/app-server-control/app-server-control.sock"));
    }
    Some(std::path::PathBuf::from(raw))
}

/// This daemon's registration in `<hangar_home>/hangar/daemon.pid`, removed on
/// drop (clean shutdown) so a later `ensure_hangar_daemon` does not read a dead
/// pid.
///
/// A hard kill leaves the file behind; that is already handled — the readers
/// probe liveness with `kill(pid, 0)` and drop a stale file before respawning.
#[derive(Debug)]
pub struct PidFile(Option<std::path::PathBuf>);

impl PidFile {
    /// Write `std::process::id()` into `<dir>/hangar/daemon.pid`.
    ///
    /// Best-effort: an unwritable hangar home yields an unregistered (no-op)
    /// handle and a warning — pid bookkeeping must never stop a daemon booting.
    #[must_use]
    pub fn register(dir: &std::path::Path) -> Self {
        let path = pid_path_in(dir);
        if let Some(parent) = path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                tracing::warn!(error = %e, path = %path.display(), "daemon pid dir create failed");
                return Self(None);
            }
        }
        match std::fs::write(&path, std::process::id().to_string()) {
            Ok(()) => Self(Some(path)),
            Err(e) => {
                tracing::warn!(error = %e, path = %path.display(), "daemon pid file write failed");
                Self(None)
            }
        }
    }
}

impl Drop for PidFile {
    /// Compare-and-delete: remove the file only while it still names US.
    ///
    /// A blind unlink here deletes whoever's registration is CURRENT, which is
    /// not necessarily ours — `register` is a last-write-wins `fs::write`, so any
    /// other daemon that started later owns the file's contents. An exiting
    /// daemon then wiped the LIVE daemon's registration, the next
    /// `ensure_hangar_daemon` read "nothing running", and spawned another. That
    /// ratchet is what turned a one-shot race into a pile of 69.
    ///
    /// [`crate::single_instance`] now stops the duplicates at the source; this
    /// keeps the pid file honest for the readers that still consult it, and
    /// covers the upgrade window where a pre-lock daemon is still running.
    fn drop(&mut self) {
        let Some(path) = self.0.take() else {
            return;
        };
        let ours = std::process::id().to_string();
        if std::fs::read_to_string(&path).is_ok_and(|recorded| recorded.trim() == ours) {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Env var naming the process whose death this daemon must not outlive.
///
/// Set by the tripwire harness, and by `start_daemon_if_stopped` whenever the
/// resolved hangar home is ephemeral (issue #784).
pub const PARENT_PID_ENV: &str = "AINB_HANGAR_PARENT_PID";

/// Former name of [`PARENT_PID_ENV`], still honoured so a spawner and a daemon
/// binary from different versions agree (the installed daemon can be older than
/// the `ainb` that launches it).
///
/// Compatibility shim: remove once no released `ainb` older than the rename is
/// still spawning daemons, and update the tripwire harnesses that set it.
pub const LEGACY_PARENT_PID_ENV: &str = "HANGAR_TEST_PARENT_PID";

/// Parent-death backstop. The caller gives this daemon its own parent PID: the
/// tripwire harness always, and the TUI autostart when the hangar home is
/// ephemeral, because every other guard (`single_instance`, `daemon stop`, the
/// pid file) is scoped to a home that dies with its creator.
///
/// When the parent is SIGKILLed or OOM-reaped, macOS reparents the daemon to
/// launchd, so normal Rust drop cleanup cannot run. Signal this process through
/// its existing Ctrl-C shutdown path instead.
fn spawn_parent_watchdog() {
    // `filter` before the fallback: a set-but-EMPTY new name would otherwise
    // read as a declaration, hide the legacy name, and arm nothing. The spawner
    // treats empty as undeclared too.
    let Some(parent_pid) = std::env::var(PARENT_PID_ENV)
        .ok()
        .filter(|value| !value.is_empty())
        .or_else(|| std::env::var(LEGACY_PARENT_PID_ENV).ok())
        .filter(|value| !value.is_empty())
        .and_then(|value| value.parse::<i32>().ok())
        .filter(|pid| *pid > 1)
    else {
        return;
    };

    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(1));
        loop {
            tick.tick().await;
            match nix::sys::signal::kill(nix::unistd::Pid::from_raw(parent_pid), None) {
                Ok(()) | Err(nix::errno::Errno::EPERM) => {}
                Err(nix::errno::Errno::ESRCH) => {
                    tracing::warn!(parent_pid, "watched parent exited, stopping daemon");
                    let _ = nix::sys::signal::kill(
                        nix::unistd::Pid::from_raw(std::process::id() as i32),
                        nix::sys::signal::Signal::SIGINT,
                    );
                    return;
                }
                Err(error) => {
                    tracing::warn!(parent_pid, %error, "could not check watched parent");
                }
            }
        }
    });
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
    spawn_parent_watchdog();
    let dir = hangar_dir()?;

    // FIRST, before the store, the broker, the sweepers and `rpc::bind`. Every
    // one of those mutates state this daemon must own alone, and `rpc::bind`
    // unlinks the socket unconditionally — so a duplicate that gets even this
    // far leaves the incumbent listening on an inode no client can reach.
    //
    // A loser exits 0, not non-zero: it did the right thing, and a supervisor
    // (launchd `KeepAlive`, systemd `Restart=on-failure`) must not restart-loop
    // a daemon that correctly declined. `--once` takes the lock too — a one-shot
    // boot against a live home would otherwise steal that daemon's socket.
    let _ownership = match single_instance::acquire(&dir)? {
        single_instance::Ownership::Acquired(guard) => guard,
        single_instance::Ownership::HeldBy(pid) => {
            tracing::info!(
                holder = pid,
                lock = %single_instance::lock_path_in(&dir).display(),
                "another hangar daemon owns this home; exiting"
            );
            return Ok(());
        }
        // Two fail-fast windows elapsed with the lock churning and no live
        // holder to name. Almost always another daemon booting; logged as a
        // warning rather than an info because, unlike `HeldBy`, we cannot point
        // at the daemon that won.
        single_instance::Ownership::Contended => {
            tracing::warn!(
                lock = %single_instance::lock_path_in(&dir).display(),
                "hangar home is contended and no holder could be named; exiting"
            );
            return Ok(());
        }
    };

    // Crash breadcrumbs start HERE, once this process owns the home — never
    // before. They live in the SHARED home: `start_breadcrumbs` deletes the
    // previous run's exit reason and begins overwriting `daemon.heartbeat` with
    // our pid. A duplicate that goes on to decline would therefore erase the
    // INCUMBENT's death record and impersonate its heartbeat, and the decline's
    // own `record_exit` would then file a clean exit for a daemon that is still
    // running. Behind the lock, a decliner installs nothing and `record_exit`
    // is a no-op for it.
    crate::observability::note_phase("boot");
    crate::observability::start_breadcrumbs(&dir);

    // The ownership watchdog: the one layer that survives every exit path not
    // running. If this daemon ever stops owning its home — an operator deleting
    // the lock, a home restored from a backup, or the home itself being deleted
    // underneath a running daemon — it stands down instead of racing the daemon
    // that owns it now, or serving a store and socket that no longer exist.
    // Home-scoped by construction: it reads and writes one file and signals
    // nobody, so unlike an argv-matching reaper it can never touch a daemon
    // serving a different home.
    let (lost_tx, lost_rx) = tokio::sync::oneshot::channel();
    let watchdog_dir = dir.clone();
    tokio::spawn(async move {
        let outcome = crate::single_instance::watch_ownership(&watchdog_dir).await;
        let _ = lost_tx.send(outcome);
    });

    // Signal handlers are installed HERE, not at the end of boot. Everything
    // below — migrations, the socket bind, the tmux reconcile — takes seconds on
    // a cold home, and a SIGTERM arriving in that window used to kill the process
    // on the OS default disposition: no teardown, and the ownership lock left on
    // disk naming a dead pid. `daemon restart` and system shutdown both land
    // exactly there.
    let shutdown = crate::shutdown::install(Some(lost_rx));
    let mut during_boot = shutdown.clone();

    // Raised the instant boot hands over to the run loop, so the boot-phase race
    // below stops competing. Without it that race stays armed for the daemon's
    // WHOLE life, and a Ctrl-C could cancel the run loop mid-teardown — dropping
    // it before it reaped its interactive tmux sessions, which is precisely the
    // orphaning the shutdown path exists to prevent.
    let (running_tx, running_rx) = tokio::sync::oneshot::channel::<()>();

    // The rest of boot, raced against that seam. Dropping this future on a
    // shutdown unwinds the partially built daemon; `_ownership` lives OUTSIDE it,
    // so the lock is released either way.
    let booted = async move {
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
        // Hook and daemon use the same resolved runtime root, including isolated
        // `$AINB_HANGAR_HOME` test and paid-runtime installs.
        {
            let events_jsonl = dir.join("events.jsonl");
            let cursor = dir.join("hangar").join("attention_ingest.offset");
            let _attention_ingest = crate::attention_ingest::AttentionIngest::new(
                store.pool().clone(),
                broker.sink(),
                events_jsonl,
                cursor,
            )
            .spawn();
        }

        // The ACP boot scan (I16), BEFORE the pool is installed: a daemon that was
        // SIGKILLed mid-turn left `open_turn_id` set, its legs PENDING and its
        // parked permissions' attention rows open, and no runtime path revisits a
        // session this process never hosted. Same shared routine the process-exit
        // and deadline paths run, so the outcomes cannot drift.
        crate::acp_pool::converge_dirty_sessions_at_boot(store.pool(), &broker.sink()).await;

        // The confirm-card TTL, swept once at boot. The park's own bound is a
        // `tokio` timer inside a copilot turn, and a timer dies with the process:
        // without this, a card left open by a SIGKILLed or upgraded daemon keeps
        // rendering as answerable on every client for as long as the row exists,
        // and approving it returns a success receipt for a destructive tool call
        // with no waiter left to run it.
        match ainb_hangar_store::repo::fleet_chat::FleetConfirmRepo::sweep_expired(
            store.pool(),
            ainb_hangar_core::clock::HangarClock::now_ms(&ainb_hangar_core::clock::SystemClock),
        )
        .await
        {
            Ok(0) => {}
            Ok(swept) => tracing::warn!(swept, "expired confirm cards left open by a prior daemon"),
            Err(error) => tracing::error!(%error, "could not sweep expired confirm cards at boot"),
        }

        // The ACP agent pool. Installed BEFORE the socket accepts a connection so
        // `fleet/acp_session_create` can never answer "no pool" on a daemon that
        // has one; nothing is spawned until the first prompt reaches it.
        let mut acp_config = crate::acp_pool::PoolConfig::from_env();
        // Under `HANGAR_TASK_EXECUTOR=acp` a TASK turn is a pool turn, and the
        // pool's deadline sweep exempts no scope, so its 30-minute default
        // would cancel every task past half an hour while the same task on the
        // process executor gets 2.5 h. Raise the pool's floor to the task budget
        // rather than leave a 5x cut invisible from the flag.
        // ponytail: the raise lands AFTER `from_env` has already coupled
        // `sweep_interval` down to the configured deadline, so
        // `AINB_ACP_TURN_DEADLINE_MS=2000` with `executor=acp` leaves a raised
        // deadline and a 1 s sweep that can never match it: harmless (the sweep
        // is idempotent and only costs one indexed read) and test-only config
        // today. Recouple the interval to the EFFECTIVE deadline with A6.
        let configured_deadline = acp_config.turn_deadline;
        acp_config.turn_deadline = crate::run_loop::reconcile_turn_deadline(
            configured_deadline,
            crate::run_loop::acp_turn_budget(),
        );
        // Say so when it actually moved. An invisible timeout change triggered
        // by a flag is the defect class this fix exists to close, and raising
        // the floor points it the safer way for tasks while opening a smaller
        // one for CHAT: a wedged chat turn now squats its session slot for the
        // longer window before the sweep reaps it. Whoever flips the flag
        // should read that here, not discover it during an incident.
        if acp_config.turn_deadline > configured_deadline {
            tracing::warn!(
                effective_secs = acp_config.turn_deadline.as_secs(),
                configured_secs = configured_deadline.as_secs(),
                "HANGAR_TASK_EXECUTOR=acp raised the acp turn deadline to the task runtime \
                 budget (HANGAR_PROVIDER_MAX_RUNTIME_MS); a wedged CHAT turn on this daemon \
                 now converges on the longer deadline too. Set AINB_ACP_TURN_DEADLINE_MS at \
                 or above the task budget to pin it."
            );
        }
        let acp_pool = crate::acp_pool::AcpPool::new(store.clone(), broker.sink(), acp_config);
        let _acp_sweeper = acp_pool.spawn_sweeper();
        crate::acp_pool::install(acp_pool).await;

        // Name the rows written before the daemon authored `display_name` at all.
        // Every existing row is NULL there, and a session nothing observes again
        // would never be named by the writers alone, so the roster's name search
        // would stay dead for exactly the sessions an operator has to search for.
        match ainb_hangar_store::repo::fleet::FleetRepo::backfill_display_names(
            store.pool(),
            crate::fleet::display_name_for_cwd,
        )
        .await
        {
            Ok(0) => {}
            Ok(named) => tracing::info!(named, "named fleet sessions left unnamed by older builds"),
            Err(error) => tracing::error!(%error, "could not backfill fleet display names at boot"),
        }

        // Tmux reconciliation keeps unhooked and standalone provider sessions in
        // the canonical Fleet roster as degraded rows with exact pane identity.
        let _fleet_tmux = crate::fleet::spawn_tmux_reconciler(store.pool().clone(), broker.sink());

        // Three slow janitors, each on its OWN clock rather than folded into the 3s
        // reconciler tick above. Measured on a real profile: 1,440 of 1,472 visible
        // sessions were dead EXITED rows that every snapshot scanned, fleet_event
        // had grown to 1.1M rows / 847 MB under no retention at all, and
        // fleet_provider_event to 372k rows / 2,207 MB under none either — the last
        // of those saturating the single writer until session spawn failed with
        // `database is locked`. All three are pure cleanup with no deadline, so none
        // belongs on a hot path. The two payload sweeps start five and seven
        // minutes in (fleet_event first, then fleet_provider_event) so their
        // FIRST passes do not land together. That stagger does not survive a
        // cold backlog: both re-arm on the 1-minute catch-up period, so their
        // drains do overlap from about t+7min until the shorter one settles.
        // Overlap is tolerable rather than prevented -- each pass is bounded,
        // yields the writer between batches, and checkpoints -- so the writer
        // is shared politely, not serialised.
        let _fleet_archiver =
            crate::fleet::spawn_session_archiver(store.pool().clone(), broker.sink());
        let _fleet_retention =
            crate::fleet_retention::spawn_retention_sweeper(store.pool().clone());
        let _fleet_provider_retention =
            crate::fleet_provider_retention::spawn_provider_retention_sweeper(store.pool().clone());

        // Managed Codex transport starts independently from daemon readiness. A
        // missing or incompatible Codex binary leaves hook and tmux observation
        // running, while the service records an honest transport downgrade.
        let _codex_manager = if !once
            && std::env::var_os("AINB_CODEX_MANAGED")
                .as_deref()
                .is_none_or(|value| value != "0")
        {
            let binary = std::env::var_os("AINB_CODEX_BIN").unwrap_or_else(|| "codex".into());
            // Opt-in: attach to the ChatGPT-managed app-server instead of running
            // our own. That daemon is the one the Codex phone app pairs with, and
            // it runs on the user's real CODEX_HOME, so sessions Ainb opens
            // through it are visible and operable from the phone. Our own server
            // uses a scoped CODEX_HOME the phone cannot see.
            //
            // Off by default: this shares one enrollment identity with Codex
            // Desktop, which is exactly what the scoped home was introduced to
            // avoid, and existing scoped sessions do not carry over.
            let external = codex_external_socket();
            let attached = external.is_some();
            // Our own server always lives here, whether or not we attach
            // elsewhere this boot. Both reapers below are scoped to THIS path
            // and signal only a pid proven to hold it, so they must keep running
            // in attached mode too: otherwise a server orphaned by an earlier
            // SIGKILL leaks forever the moment the user opts in.
            let owned_socket = dir.join("codex-app-server.sock");
            let socket = external.unwrap_or_else(|| owned_socket.clone());
            // Older Ainb daemons used `codex app-server proxy` against this same
            // path. A pre-lock daemon can survive an upgrade and keep sending
            // stdin JSON-RPC into the native WebSocket listener. Recover only
            // the exact orphaned parent-child legacy topology for this home.
            let legacy_reaped =
                crate::fleet_provider::codex_manager::reap_legacy_codex_proxy_daemons(
                    &owned_socket,
                )
                .await;
            if legacy_reaped > 0 {
                tracing::warn!(legacy_reaped, "reaped obsolete codex proxy daemon at boot");
            }
            // Reap any app-server orphaned by a SIGKILLed/OOM-reaped prior daemon (or a
            // dead plugin broker) BEFORE we spawn our own. Rust Drop never runs after
            // SIGKILL, so this boot-time sweep is the only backstop that survives it.
            let reaped =
                crate::fleet_provider::codex_manager::reap_orphaned_codex_servers(&owned_socket)
                    .await;
            if reaped > 0 {
                tracing::warn!(reaped, "reaped orphaned codex app-server processes at boot");
            }
            if attached {
                tracing::info!(
                    socket = %socket.display(),
                    "attaching to externally managed codex app-server; not spawning or reaping"
                );
                // Attaching changes WHOSE config governs these sessions and
                // strands anything already in the scoped home. Both are
                // invisible unless we say so.
                warn_about_effective_codex_home_settings(dirs::home_dir().as_deref());
                note_scoped_sessions_left_behind(&dir);
            }
            let mut manager_config =
                crate::fleet_provider::codex_manager::CodexManagerConfig::new(binary, socket);
            if attached {
                manager_config = manager_config.attached_to_external_server();
            }
            Some(crate::fleet_provider::codex_manager::spawn_service(
                manager_config,
                store.pool().clone(),
                broker.sink(),
            ))
        } else {
            None
        };

        // P5 (T6): reconcile the agent-profile index against the on-disk masters and
        // spawn an fs-watch so an edit-on-disk of `{hangar_home}/profiles/<slug>.md`
        // is picked up into the `profile` index without an RPC. Best-effort — a
        // watcher-setup fault is logged inside the helper and swallowed (the
        // `profile/upsert` RPC still refreshes the index directly). The returned
        // watcher is HELD for the process lifetime; dropping it would stop the watch,
        // so it lives in `boot`'s scope alongside the other background handles.
        let _profile_watch = crate::profile::profiles_dir()
            .and_then(|dir| crate::profile::spawn_index_watch(store.pool().clone(), dir));

        // Fleet Usage survives TUI restarts: load the daemon-owned bounded snapshot
        // now, then refresh it on a background worker before Fleet opens its socket.
        crate::fleet_usage::install(&dir).await;
        crate::fleet_quota::install(&dir).await;

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
        // Self-register this pid so EVERY running daemon is DISCOVERABLE, not just
        // one started via `ainb hangar daemon start`. Discovery only — the exclusion
        // that actually stops a duplicate is `single_instance`'s lock, taken at the
        // top of this function. This file is what `status` prints and what a
        // pre-lock CLI still reads.
        //
        // `ensure_hangar_daemon` (the TUI's autostart) decides "is a daemon already
        // up?" purely from `<hangar_home>/hangar/daemon.pid`. Only the CLI `start`
        // verb used to write that file, so a daemon launched any other way — the
        // `ainb-hangar-daemon` binary directly, systemd/launchd, a test harness —
        // was invisible: the TUI spawned a SECOND daemon, `rpc::bind` unlinked the
        // live socket out from under the first, and two claim loops + two sweepers
        // then raced one SQLite home while the TUI talked to the newcomer's empty
        // in-memory state.
        //
        // Writing the pid here made those daemons visible; it did NOT close the hole,
        // because it happens at the END of boot and nothing checked it under
        // exclusion. The lock at the top of `boot` is the actual fix — this line is
        // now bookkeeping for humans and for `status`.
        let _pid_file = PidFile::register(&dir);
        let mut cfg = DaemonConfig::from_env();
        // The claim loop MUST key off the runtime id that is actually registered (and
        // that the seeded/created agents are bound to). A runtime cannot be renamed —
        // `agent.runtime_id` is an enforced FK — so an existing row's id wins over a
        // changed `HANGAR_DAEMON_RUNTIME_ID` (which is warned about, not obeyed).
        // Resolving here keeps the registered row, the agents, and the claim loop on
        // ONE id instead of claiming for an id nothing is bound to.
        //
        // This is also where this PROCESS claims the runtime (migration 0092): the
        // registration stamps our instance id and reports whether we displaced a
        // different one, which is what tells the run loop that the rows still
        // `dispatched`/`running` for this runtime are a dead process's orphans
        // rather than our own live work.
        let now =
            ainb_hangar_core::clock::HangarClock::now_ms(&ainb_hangar_core::clock::SystemClock);
        let boot = crate::runtime_register::resolve_runtime_boot(store.pool(), now).await;
        cfg.runtime_id = Some(boot.runtime_id);
        cfg.runtime_arrival = boot.arrival;
        // Boot is done; from here the run loop owns shutdown.
        let _ = running_tx.send(());
        run(store.pool().clone(), cfg, stats, broker.sink(), shutdown).await
    };

    tokio::select! {
        result = booted => result,
        cause = async move {
            let raced = tokio::select! {
                cause = during_boot.recv() => Some(cause),
                _ = running_rx => None,
            };
            match raced {
                Some(cause) => cause,
                // The run loop took over. Park forever so this arm can never
                // resolve and cancel it.
                None => std::future::pending().await,
            }
        } => {
            tracing::info!(
                signal = cause.as_str(),
                "ainb-hangar-daemon shutting down during boot"
            );
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {

    /// The stranded-session notice counts only real session files.
    ///
    /// A wrong count here is worse than none: it is the number a user decides
    /// whether to switch back on.
    #[test]
    fn scoped_session_notice_counts_only_jsonl_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let sessions = dir.path().join("codex-home").join("sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        for name in ["a.jsonl", "b.jsonl", "notes.txt", "c.jsonl"] {
            std::fs::write(sessions.join(name), b"").unwrap();
        }
        let count = std::fs::read_dir(&sessions)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "jsonl"))
            .count();
        assert_eq!(count, 3, "only .jsonl sessions count");

        // No scoped home at all must be silent, not a panic or a zero-notice.
        let empty = tempfile::tempdir().unwrap();
        super::note_scoped_sessions_left_behind(empty.path());
    }

    /// The attached-mode warning names settings that are actually present, and
    /// stays silent on a config that sets none of them.
    #[test]
    fn effective_settings_warning_is_driven_by_what_is_set() {
        let home = tempfile::tempdir().unwrap();
        let codex = home.path().join(".codex");
        std::fs::create_dir_all(&codex).unwrap();

        // Silent cases must not panic: absent file, then a config with nothing
        // worth naming.
        super::warn_about_effective_codex_home_settings(Some(home.path()));
        std::fs::write(codex.join("config.toml"), "unrelated = 1\n").unwrap();
        super::warn_about_effective_codex_home_settings(Some(home.path()));

        // And the keys we promise to name are the ones the doc lists.
        assert!(super::CODEX_HOME_SETTINGS_WORTH_NAMING.contains(&"model"));
        assert!(super::CODEX_HOME_SETTINGS_WORTH_NAMING.contains(&"approval_policy"));
        assert!(super::CODEX_HOME_SETTINGS_WORTH_NAMING.contains(&"mcp_servers"));

        std::fs::write(
            codex.join("config.toml"),
            "model = \"x\"\n[features]\nhooks = true\njs_repl = false\n",
        )
        .unwrap();
        super::warn_about_effective_codex_home_settings(Some(home.path()));
    }

    /// `[codex] app_server` is read out of the hangar home's config.toml.
    ///
    /// The daemon parses this one key itself rather than depending on
    /// `ainb-core`, so the parse is worth pinning: a shape change there would
    /// otherwise silently drop everyone back to the default.
    #[test]
    fn codex_app_server_is_read_from_the_config_file() {
        let dir = tempfile::tempdir().unwrap();
        let config_dir = dir.path().join("config");
        std::fs::create_dir_all(&config_dir).unwrap();
        let path = config_dir.join("config.toml");

        let read = |text: &str| -> Option<String> {
            std::fs::write(&path, text).unwrap();
            let parsed: toml::Value = std::fs::read_to_string(&path).unwrap().parse().ok()?;
            parsed
                .get("codex")?
                .get("app_server")?
                .as_str()
                .map(str::to_string)
                .filter(|value| !value.is_empty())
        };

        assert_eq!(
            read("[codex]\napp_server = \"own\"\n"),
            Some("own".to_string())
        );
        assert_eq!(
            read("[codex]\napp_server = \"/tmp/x.sock\"\n"),
            Some("/tmp/x.sock".to_string())
        );
        assert_eq!(
            read("[codex]\napp_server = \"\"\n"),
            None,
            "empty is not a setting"
        );
        assert_eq!(
            read("[fleet]\nterminal = \"warp\"\n"),
            None,
            "absent section"
        );
        assert_eq!(read("[codex]\nother = 1\n"), None, "absent key");
    }

    /// The resolver's value semantics.
    ///
    /// `desktop` is the DEFAULT, so an absent setting still attaches; `own` is
    /// the explicit way back to Ainb's own scoped server; anything else is a
    /// socket path taken verbatim.
    #[test]
    fn codex_app_server_values_resolve_to_the_right_socket() {
        use std::ffi::OsString;
        let home = std::path::Path::new("/Users/example");
        let desktop = home.join(".codex/app-server-control/app-server-control.sock");

        assert_eq!(
            super::codex_external_socket_from(Some(OsString::from("desktop")), Some(home)),
            Some(desktop.clone())
        );
        assert_eq!(
            super::codex_external_socket_from(Some(OsString::from("own")), Some(home)),
            None,
            "`own` must mean run our own server"
        );
        assert_eq!(
            super::codex_external_socket_from(Some(OsString::new()), Some(home)),
            None,
            "an empty value must not be read as a path"
        );
        assert_eq!(
            super::codex_external_socket_from(Some(OsString::from("/tmp/custom.sock")), Some(home)),
            Some(std::path::PathBuf::from("/tmp/custom.sock"))
        );
        assert_eq!(
            super::codex_external_socket_from(None, Some(home)),
            None,
            "no setting at all is the caller's business; the default is applied upstream"
        );

        // The default itself, so a change to it fails here rather than silently
        // moving every user's sessions between app-servers.
        assert_eq!(super::CODEX_APP_SERVER_DEFAULT, "desktop");
        assert_eq!(
            super::codex_external_socket_from(
                Some(super::CODEX_APP_SERVER_DEFAULT.into()),
                Some(home)
            ),
            Some(desktop),
            "the default must attach to the phone-visible daemon"
        );
    }

    use super::{PidFile, pid_path_in};

    /// The happy path: a clean exit takes our own registration with it, so the
    /// next `ensure_hangar_daemon` does not read a dead pid.
    #[test]
    fn drop_removes_our_own_registration() {
        let home = tempfile::tempdir().expect("tmpdir");
        let path = pid_path_in(home.path());

        let pid_file = PidFile::register(home.path());
        assert_eq!(
            std::fs::read_to_string(&path).expect("registered"),
            std::process::id().to_string()
        );

        drop(pid_file);
        assert!(!path.exists(), "our own registration should be removed");
    }

    /// The ratchet regression. `register` is last-write-wins, so a daemon that
    /// started later owns the file's contents. When the EARLIER daemon exits, a
    /// blind unlink deletes the LIVE daemon's registration — the next autostart
    /// then reads "nothing running" and spawns yet another. That is how one
    /// machine reached 69 daemons instead of settling at two.
    #[test]
    fn drop_keeps_a_registration_another_daemon_overwrote() {
        let home = tempfile::tempdir().expect("tmpdir");
        let path = pid_path_in(home.path());

        let ours = PidFile::register(home.path());
        // A later daemon self-registers over the top of ours.
        std::fs::write(&path, "424242").expect("overwrite with a newer daemon's pid");

        drop(ours);

        assert!(
            path.exists(),
            "an exiting daemon must not delete the live daemon's registration"
        );
        assert_eq!(
            std::fs::read_to_string(&path).expect("still registered").trim(),
            "424242"
        );
    }

    /// An unwritable home yields an unregistered handle, and dropping it must not
    /// touch anything.
    #[test]
    fn dropping_an_unregistered_handle_is_a_no_op() {
        let home = tempfile::tempdir().expect("tmpdir");
        let path = pid_path_in(home.path());
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(&path, "424242").expect("seed someone else's registration");

        drop(PidFile(None));

        assert!(path.exists(), "a no-op handle must not remove anything");
    }
}
