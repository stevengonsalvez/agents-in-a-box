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
/// Per-task execution-environment layout: workdir/output/logs + `.gc_meta.json`
/// (P1.6).
pub mod execenv;
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
/// TTL sweepers + stale-dispatch reclaim (P1.4).
///
/// The daemon's tokio runtime registers these as periodic tasks; they are also
/// callable directly (with an injected clock) for deterministic testing.
pub mod sweeper;
/// Git-worktree integration for per-task working dirs (P1.6).
pub mod worktree;

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
    let store: Store = Store::open_default().await?;
    tracing::info!(idle = true, "ainb-hangar-daemon ready idle=true");
    if once {
        return Ok(());
    }
    let cfg = DaemonConfig::from_env();
    run(store.pool().clone(), cfg).await
}
