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

use crate::run_loop::{run, DaemonConfig};

/// Beads CLI adapter — shells out to `bd` and parses `--json` (P2.2).
///
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
/// Env allowlist config + task-env builder (P5.3).
///
/// Loads/saves `~/.ainb/hangar/env.allow.toml` (foreign sections preserved,
/// atomic write) and exposes [`dispatch::build_task_env`] — the env-build seam
/// the claim loop uses before spawning a provider: ambient env is filtered by
/// [`ainb_hangar_core::env_policy`] then keychain keys are layered on top.
pub mod dispatch;
/// Per-task execution-environment layout: workdir/output/logs + `.gc_meta.json`
/// (P1.6).
pub mod execenv;
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
/// The daemon's `UnixListener` JSON-RPC server (P4.10).
///
/// Binds `{hangar_home}/hangar.sock`, serves `workspace/subscribe`, `ping`, and
/// the four `hangar/*` snapshot RPCs ([`rpc::snapshots`]) backed by the store
/// repos. The plugin dials this socket through the host `unix_socket_dial` cap
/// to populate its screens with live data.
pub mod rpc;
/// Deterministic P4 seed fixture for the e2e tripwires (`test-support` only).
///
/// Writes a `default` workspace with issues/agents/skills + a running task into
/// a fresh store so the tmux tripwires can launch `ainb tui` against a seeded
/// `hangar.db` and assert live rows render.
#[cfg(any(test, feature = "test-support"))]
pub mod seed;
/// Toolkit-directory skill importer behind `ainb hangar skills sync` (P6.2).
///
/// Walks a `toolkit/packages/skills/`-shaped tree (`<name>/SKILL.md` + nested
/// assets), parses each skill's YAML frontmatter, validates the whole batch
/// (uniqueness + parse) before any write, then upserts every skill
/// workspace-scoped via
/// [`ainb_hangar_store::repo::skill::SkillRepo::upsert_by_name`] — idempotent
/// and all-or-nothing.
pub mod skills_sync;
/// `ainb hangar templates use <name>` transactional materialisation (P6.3).
///
/// Turns an embedded curated [`ainb_hangar_core::template::AgentTemplate`] into a
/// live `agent` row plus its `agent_skill` attachments in one transaction.
/// Referenced skills must be pre-imported (P6.2 `skills sync`); a missing one is
/// a hard [`templates::TemplateUseError::SkillNotImported`] with a sync hint and
/// nothing is written. Idempotent by agent name within the workspace.
pub mod templates;
/// TTL sweepers + stale-dispatch reclaim (P1.4).
///
/// The daemon's tokio runtime registers these as periodic tasks; they are also
/// callable directly (with an injected clock) for deterministic testing.
pub mod sweeper;
/// Danger-full-access warning emission at provider invocation (P5.6).
pub mod warnings;
/// Git-worktree integration for per-task working dirs (P1.6).
pub mod worktree;

/// Resolve the directory that holds `hangar.db` (and the `hangar.sock` the RPC
/// server binds). Mirrors [`ainb_hangar_store::Store`]'s resolution so the
/// database, the per-task env tree, and the socket all share one root:
/// `$AINB_HANGAR_HOME` when set and non-empty, else `~/.ainb`.
fn hangar_dir() -> anyhow::Result<std::path::PathBuf> {
    match std::env::var_os(ainb_hangar_store::Store::home_env()).filter(|p| !p.is_empty()) {
        Some(p) => Ok(std::path::PathBuf::from(p)),
        None => Ok(dirs::home_dir()
            .ok_or_else(|| anyhow::anyhow!("could not resolve home directory"))?
            .join(".ainb")),
    }
}

/// Boot the daemon: open the persistence layer and run the claim loop.
///
/// Resolves the database directory the same way every Hangar consumer does
/// (`$AINB_HANGAR_HOME` override, else `~/.ainb`), opens (creating if absent)
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
    let store: Store = Store::open_in(&dir).await?;

    // P4.10: bind the JSON-RPC socket beside the database and serve plugin
    // connections on a background task. A bind failure is non-fatal — the
    // daemon's claim loop must still run even if no plugin can reach it (and a
    // stale socket from a crashed daemon is removed in `rpc::bind`).
    let socket_path = rpc::socket_path_in(&dir);
    match rpc::bind(&socket_path) {
        Ok(listener) => {
            let health = rpc::DaemonHealth {
                socket_path: socket_path.to_string_lossy().into_owned(),
                pid: std::process::id(),
                started_at: std::time::Instant::now(),
                version: env!("CARGO_PKG_VERSION").to_string(),
            };
            tracing::info!(socket = %socket_path.display(), "hangar rpc listening");
            tokio::spawn(rpc::serve(listener, store.pool().clone(), health));
        }
        Err(e) => tracing::warn!(error = %e, socket = %socket_path.display(), "hangar rpc bind failed"),
    }

    tracing::info!(idle = true, "ainb-hangar-daemon ready idle=true");
    if once {
        return Ok(());
    }
    let cfg = DaemonConfig::from_env();
    run(store.pool().clone(), cfg).await
}
