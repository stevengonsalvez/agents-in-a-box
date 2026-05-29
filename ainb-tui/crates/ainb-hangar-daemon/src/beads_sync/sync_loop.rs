//! Inbound beads sync poll loop (P2.4).
//!
//! [`run_inbound_loop`] drives [`InboundSync::reconcile_once`] on a fixed
//! interval until cancelled. It is the long-lived daemon task that keeps Hangar
//! issues in step with `bd`-side status changes.
//!
//! # Lifecycle
//!
//! - Spawned as a named `JoinHandle` by the daemon's supervisor (restart-safe).
//! - Selects on a [`CancellationToken`] every tick so shutdown is honoured
//!   within one sleep (no blocking on the next reconcile).
//! - **Backs off on failure.** A failing `bd list` (binary missing, non-zero
//!   exit) is non-fatal: the loop logs, doubles its delay up to a 5-minute cap,
//!   and keeps trying. A success resets the delay to the base interval.
//!
//! The module name is `sync_loop` rather than `loop` because `loop` is a Rust
//! keyword and cannot be a module identifier.

use std::sync::Arc;
use std::time::Duration;

use tokio_util::sync::CancellationToken;

use super::inbound::InboundSync;

/// The ceiling the failure backoff doubles up to (5 minutes).
const MAX_BACKOFF: Duration = Duration::from_mins(5);

/// Poll `bd` on `interval`, reconciling each tick into Hangar, until `cancel`.
///
/// On a successful reconcile the next delay resets to `interval`. On a failure
/// the delay doubles, clamped to [`MAX_BACKOFF`], so a persistently broken `bd`
/// does not hammer the binary. The loop checks `cancel` at the head of every
/// tick, so a cancellation is honoured within at most one current delay.
///
/// `'static` lifetimes on the [`InboundSync`] let the loop own its dependencies
/// across `await` points when held behind the supervisor's `Arc`.
pub async fn run_inbound_loop(
    sync: Arc<InboundSync<'static>>,
    interval: Duration,
    cancel: CancellationToken,
) {
    let mut delay = interval;
    loop {
        tokio::select! {
            () = cancel.cancelled() => {
                tracing::info!("inbound beads sync loop cancelled; exiting");
                break;
            }
            () = tokio::time::sleep(delay) => {}
        }

        match sync.reconcile_once().await {
            Ok(stats) => {
                tracing::debug!(?stats, "inbound reconcile tick ok");
                delay = interval;
            }
            Err(e) => {
                delay = (delay * 2).min(MAX_BACKOFF);
                tracing::warn!(error = %e, next_delay_secs = delay.as_secs(), "inbound reconcile tick failed; backing off");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{MAX_BACKOFF, Duration};

    #[test]
    fn backoff_doubles_and_caps() {
        // Pure arithmetic mirror of the loop's backoff rule, pinning the cap.
        let mut delay = Duration::from_secs(30);
        for _ in 0..10 {
            delay = (delay * 2).min(MAX_BACKOFF);
        }
        assert_eq!(delay, MAX_BACKOFF);
    }
}
