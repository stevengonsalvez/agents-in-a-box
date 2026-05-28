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

/// Boot the daemon: open the persistence layer and run the idle loop.
///
/// Resolves the database directory the same way every Hangar consumer does
/// (`$AINB_HANGAR_HOME` override, else `~/.ainb`), opens (creating if absent)
/// `hangar.db`, applies all embedded migrations, logs a `ready` line, then
/// hands off to [`run_idle`].
///
/// When `once` is `true` the function returns as soon as the daemon is ready
/// (one-shot mode used by the boot tripwire). Otherwise it blocks in
/// [`run_idle`] until interrupted.
///
/// # Errors
///
/// Returns an error if the store cannot be opened (directory not writable, a
/// migration fails) or if waiting for the shutdown signal fails.
pub async fn boot(once: bool) -> anyhow::Result<()> {
    let _store: Store = Store::open_default().await?;
    tracing::info!(idle = true, "ainb-hangar-daemon ready idle=true");
    if once {
        return Ok(());
    }
    run_idle().await
}

/// Idle until a shutdown signal arrives.
///
/// P0 placeholder: the daemon does no work, it simply waits for `Ctrl-C`
/// (`SIGINT`). Later phases replace the body with the task-dispatch FSM loop;
/// the signature stays stable so `main` and the boot path are unaffected.
///
/// # Errors
///
/// Returns an error if installing or awaiting the signal handler fails.
pub async fn run_idle() -> anyhow::Result<()> {
    tokio::signal::ctrl_c().await?;
    tracing::info!("ainb-hangar-daemon shutting down");
    Ok(())
}
