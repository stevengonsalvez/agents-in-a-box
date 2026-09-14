//! A surface's one live presence connection to the daemon (#963).
//!
//! The connection registry answers "which surfaces are live", and it can only
//! answer from sockets that stay open. A TUI (and, next, the desktop host)
//! polls and acts through short request connections that close in
//! milliseconds, so without a held connection it never shows up at all.
//!
//! ```text
//!  PresenceLease::spawn ──▶ dial + hello{surface} ──▶ hold (ping 30s) ──▶ EOF
//!        │                        ▲    │ fail                            │
//!        │                        └────┴── backoff 250ms..2s ◀───────────┘
//!        └─ close() / drop ──▶ socket closed ──▶ daemon removes the row
//! ```
//!
//! While any lease is alive in this process, every OTHER connection a
//! [`crate::DaemonClient`] opens says `transient` in its hello, whatever its
//! surface kind. The daemon serves those normally and stamps provenance from
//! them, but does not list them, so one running TUI is exactly one row no
//! matter how many polls it makes or which call sites label themselves `cli`.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use ainb_hangar_proto::connections::SurfaceInfo;
use ainb_hangar_proto::methods;
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;

use crate::{DaemonClient, DaemonError, RPC_TIMEOUT, read_frame, write_frame};

/// First retry delay after a failed dial or a lost connection.
const BACKOFF_INITIAL: Duration = Duration::from_millis(250);
/// Longest wait between dial attempts. Low enough that a TUI registers within
/// a few seconds of a daemon (re)start, high enough to cost nothing while no
/// daemon is running.
const BACKOFF_MAX: Duration = Duration::from_secs(2);
/// How often a held connection proves the daemon still answers. Also keeps it
/// far inside the daemon's request-connection idle window (600 s).
const PING_INTERVAL: Duration = Duration::from_secs(30);
/// Longest a close waits for the task to drop its socket before aborting it.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(1);

/// Live presence leases in this process. Non-zero marks every other daemon
/// connection from this process as transient.
static LEASES_HELD: AtomicUsize = AtomicUsize::new(0);

/// Whether this process currently holds a presence lease.
pub fn lease_held() -> bool {
    LEASES_HELD.load(Ordering::SeqCst) > 0
}

/// Where a lease's connection currently stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PresenceState {
    /// Dialing, or waiting out a backoff after `error`.
    Waiting {
        /// Why the last attempt did not produce a connection, if one ran.
        error: Option<String>,
    },
    /// A hello'd connection is held open; the daemon lists this surface.
    Connected,
    /// The lease was closed; nothing will reconnect.
    Closed,
}

/// Resolves a fresh client for every dial attempt.
///
/// Fresh per attempt so a daemon that first comes up after the surface, and
/// writes its token then, is picked up without restarting the surface.
pub type Dialer = Box<dyn Fn() -> Result<DaemonClient, DaemonError> + Send + Sync>;

/// Holds one surface's presence connection until closed or dropped.
///
/// Dropping the lease aborts its task, which closes the socket; prefer
/// [`Self::close`] on an orderly shutdown so the close is awaited.
pub struct PresenceLease {
    shutdown: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<()>>,
    state: watch::Receiver<PresenceState>,
    _held: HeldGuard,
}

impl PresenceLease {
    /// Hold `surface`'s presence against the daemon resolved from the
    /// environment, as [`DaemonClient::from_env`] resolves it.
    ///
    /// Must be called inside a tokio runtime.
    #[must_use]
    pub fn spawn(surface: SurfaceInfo) -> Self {
        Self::spawn_with(surface, Box::new(DaemonClient::from_env))
    }

    /// [`Self::spawn`] with an explicit client source (the test seam).
    #[must_use]
    pub fn spawn_with(surface: SurfaceInfo, dialer: Dialer) -> Self {
        let held = HeldGuard::acquire();
        let (state_tx, state) = watch::channel(PresenceState::Waiting { error: None });
        let (shutdown, shutdown_rx) = oneshot::channel();
        let task = tokio::spawn(run(surface, dialer, state_tx, shutdown_rx));
        Self {
            shutdown: Some(shutdown),
            task: Some(task),
            state,
            _held: held,
        }
    }

    /// Observe the connection state. The sender lives as long as the task, so
    /// `changed()` errors once the lease has fully closed.
    #[must_use]
    pub fn state(&self) -> watch::Receiver<PresenceState> {
        self.state.clone()
    }

    /// Close the presence connection and wait (bounded) for the socket to drop.
    pub async fn close(mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(mut task) = self.task.take() {
            if tokio::time::timeout(CLOSE_TIMEOUT, &mut task).await.is_err() {
                task.abort();
            }
        }
    }
}

impl Drop for PresenceLease {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

/// Counts one live lease for [`lease_held`] until dropped.
struct HeldGuard;

impl HeldGuard {
    fn acquire() -> Self {
        LEASES_HELD.fetch_add(1, Ordering::SeqCst);
        Self
    }
}

impl Drop for HeldGuard {
    fn drop(&mut self) {
        LEASES_HELD.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Why a held connection ended.
enum Held {
    Shutdown,
    Lost(String),
}

async fn run(
    surface: SurfaceInfo,
    dialer: Dialer,
    state: watch::Sender<PresenceState>,
    mut shutdown: oneshot::Receiver<()>,
) {
    let mut backoff = BACKOFF_INITIAL;
    loop {
        let attempt = async {
            let mut client = dialer()?;
            client.set_surface(surface.clone());
            tokio::time::timeout(RPC_TIMEOUT, client.dial_presence())
                .await
                .map_err(|_| DaemonError::Timeout(RPC_TIMEOUT))?
        };
        let error = tokio::select! {
            _ = &mut shutdown => break,
            dialed = attempt => match dialed {
                Ok((reader, writer)) => {
                    state.send_replace(PresenceState::Connected);
                    backoff = BACKOFF_INITIAL;
                    match hold(reader, writer, &mut shutdown).await {
                        Held::Shutdown => break,
                        Held::Lost(error) => error,
                    }
                }
                Err(error) => error.to_string(),
            },
        };
        state.send_replace(PresenceState::Waiting { error: Some(error) });
        tokio::select! {
            _ = &mut shutdown => break,
            () = tokio::time::sleep(backoff) => {}
        }
        backoff = (backoff * 2).min(BACKOFF_MAX);
    }
    state.send_replace(PresenceState::Closed);
}

/// Keep one authenticated connection open until the daemon goes away, stops
/// answering pings, or the lease shuts down. Returning drops both halves,
/// which is the close the daemon deregisters on.
async fn hold(
    mut reader: tokio::io::BufReader<tokio::net::unix::OwnedReadHalf>,
    mut writer: tokio::net::unix::OwnedWriteHalf,
    shutdown: &mut oneshot::Receiver<()>,
) -> Held {
    // `read_frame` is not cancel-safe, so frames are read on their own task and
    // handed over a channel the select below can drop a receive on safely.
    let (frames_tx, mut frames) = mpsc::channel::<Result<Value, DaemonError>>(8);
    let pump = tokio::spawn(async move {
        loop {
            let frame = read_frame(&mut reader).await;
            let failed = frame.is_err();
            if frames_tx.send(frame).await.is_err() || failed {
                break;
            }
        }
    });
    let _pump = AbortOnDrop(pump);

    let mut ping =
        tokio::time::interval_at(tokio::time::Instant::now() + PING_INTERVAL, PING_INTERVAL);
    let mut next_id: i64 = 2;
    // The id of an unanswered ping and when it is overdue.
    let mut pending: Option<(i64, tokio::time::Instant)> = None;

    loop {
        let overdue = async move {
            match pending {
                Some((_, deadline)) => tokio::time::sleep_until(deadline).await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            _ = &mut *shutdown => return Held::Shutdown,
            frame = frames.recv() => match frame {
                Some(Ok(frame)) => {
                    if let Some((id, _)) = pending {
                        if frame.get("id").and_then(Value::as_i64) == Some(id) {
                            pending = None;
                        }
                    }
                }
                Some(Err(error)) => return Held::Lost(error.to_string()),
                None => return Held::Lost("daemon connection closed".to_string()),
            },
            _ = ping.tick(), if pending.is_none() => {
                let id = next_id;
                next_id += 1;
                if let Err(error) = write_frame(&mut writer, methods::PING, json!({}), id).await {
                    return Held::Lost(error.to_string());
                }
                pending = Some((id, tokio::time::Instant::now() + RPC_TIMEOUT));
            }
            () = overdue => {
                return Held::Lost(format!("daemon did not answer ping within {RPC_TIMEOUT:?}"));
            }
        }
    }
}

struct AbortOnDrop(JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}
