//! Reconnecting daemon client wrapper and retained subscription (#P6).
//!
//! Provides automatic reconnection with 1s/4s/16s backoff, `auth/hello`
//! resynchronization, and seamless resumption of retained subscriptions
//! (such as Fleet subscriptions via `after_revision`).

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use ainb_hangar_proto::fleet::FleetReplayState;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;

use crate::presence::Dialer;
use crate::{DaemonClient, DaemonError, FleetStreamEvent};

/// Spec-mandated 1st backoff delay: 1 second.
pub const BACKOFF_1S: Duration = Duration::from_secs(1);
/// Spec-mandated 2nd backoff delay: 4 seconds.
pub const BACKOFF_4S: Duration = Duration::from_secs(4);
/// Spec-mandated 3rd+ backoff delay: 16 seconds.
pub const BACKOFF_16S: Duration = Duration::from_secs(16);

/// The standard reconnect backoff sequence: 1s, 4s, 16s.
pub const RECONNECT_SCHEDULE: [Duration; 3] = [BACKOFF_1S, BACKOFF_4S, BACKOFF_16S];

/// Reconnect timing configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timing {
    /// 1st retry delay (default 1s).
    pub backoff_1s: Duration,
    /// 2nd retry delay (default 4s).
    pub backoff_4s: Duration,
    /// 3rd+ retry delay (default 16s).
    pub backoff_16s: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            backoff_1s: BACKOFF_1S,
            backoff_4s: BACKOFF_4S,
            backoff_16s: BACKOFF_16S,
        }
    }
}

impl Timing {
    /// Calculate delay for attempt (1-based index).
    #[must_use]
    pub const fn delay_for_attempt(&self, attempt: u32) -> Duration {
        match attempt {
            1 => self.backoff_1s,
            2 => self.backoff_4s,
            _ => self.backoff_16s,
        }
    }
}

/// Renderer view of connection state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RendererConnectionView {
    /// Banner text ("reconnecting" when reconnecting, None when connected).
    pub banner: Option<&'static str>,
    /// Whether sections are marked with a stale badge.
    pub stale_badge: bool,
    /// Whether sections are frozen in place rather than emptied.
    pub frozen: bool,
}

/// Connection state published on a `tokio::sync::watch` channel.
///
/// Built in the shape of `PresenceState` from `presence.rs`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionState {
    /// Connected to the daemon; active subscription open.
    Connected,
    /// Disconnected or waiting out backoff before redialing.
    Reconnecting {
        /// Scheduled delay before next attempt.
        delay: Duration,
        /// 1-based attempt count in the current reconnect sequence.
        attempt: u32,
        /// Reason the last attempt failed, if known.
        error: Option<String>,
        /// When the client published this attempt and began its `delay`.
        ///
        /// The client's own clock, so the time between two attempts can be
        /// read without the observer's wake-up latency in it. On tokio's clock,
        /// the one the backoff sleeps on.
        scheduled_at: tokio::time::Instant,
    },
    /// Closed explicitly; will not reconnect.
    Closed,
}

impl ConnectionState {
    /// Whether currently connected to the daemon.
    #[must_use]
    pub const fn is_connected(&self) -> bool {
        matches!(self, Self::Connected)
    }

    /// Whether redialing / waiting between reconnect attempts.
    #[must_use]
    pub const fn is_reconnecting(&self) -> bool {
        matches!(self, Self::Reconnecting { .. })
    }

    /// Whether closed explicitly.
    #[must_use]
    pub const fn is_closed(&self) -> bool {
        matches!(self, Self::Closed)
    }

    /// Whether sections should be frozen with a stale badge in renderers.
    #[must_use]
    pub const fn sections_stale_and_frozen(&self) -> bool {
        matches!(self, Self::Reconnecting { .. })
    }

    /// Complete view projection for renderers.
    #[must_use]
    pub const fn renderer_view(&self) -> RendererConnectionView {
        match self {
            Self::Connected => RendererConnectionView {
                banner: None,
                stale_badge: false,
                frozen: false,
            },
            Self::Reconnecting { .. } => RendererConnectionView {
                banner: Some("reconnecting"),
                stale_badge: true,
                frozen: true,
            },
            Self::Closed => RendererConnectionView {
                banner: Some("disconnected"),
                stale_badge: true,
                frozen: true,
            },
        }
    }
}

/// A persistent, auto-reconnecting Fleet subscription.
///
/// Automatically redials on disconnect with 1s/4s/16s backoff, resets the backoff
/// on successful `auth/hello`, and reopens the subscription through `after_revision`
/// to guarantee seamless and contiguous event delivery without gaps.
pub struct ReconnectingFleetSubscription {
    events_rx: mpsc::Receiver<Result<FleetStreamEvent, DaemonError>>,
    state_rx: watch::Receiver<ConnectionState>,
    revision_tracker: Arc<AtomicI64>,
    shutdown_tx: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<()>>,
}

impl ReconnectingFleetSubscription {
    /// Spawn a reconnecting subscription using `client` as template.
    ///
    /// The dialer automatically re-reads the token file from the socket directory
    /// on each attempt if present, ensuring daemon restarts with new tokens succeed.
    #[must_use]
    pub fn spawn(client: &DaemonClient, after_revision: i64) -> Self {
        let socket = client.socket().to_path_buf();
        let fallback_token = client.token().to_string();
        let surface = client.surface().clone();

        let dialer: Dialer = Box::new(move || {
            let mut tok = fallback_token.clone();
            if let Some(parent) = socket.parent() {
                let token_path = ainb_hangar_proto::auth::token_file_in(parent);
                if let Ok(fresh) = std::fs::read_to_string(&token_path) {
                    let trimmed = fresh.trim();
                    if !trimmed.is_empty() {
                        tok = trimmed.to_string();
                    }
                }
            }
            let mut c = DaemonClient::with_parts(socket.clone(), tok);
            c.set_surface(surface.clone());
            Ok(c)
        });

        Self::spawn_with(dialer, after_revision)
    }

    /// Spawn a reconnecting subscription using an explicit `Dialer`.
    #[must_use]
    pub fn spawn_with(dialer: Dialer, after_revision: i64) -> Self {
        Self::spawn_timed(dialer, after_revision, Timing::default())
    }

    /// Spawn a reconnecting subscription with custom timing.
    #[must_use]
    pub fn spawn_timed(dialer: Dialer, after_revision: i64, timing: Timing) -> Self {
        let (events_tx, events_rx) = mpsc::channel(64);
        let (state_tx, state_rx) = watch::channel(ConnectionState::Reconnecting {
            delay: timing.backoff_1s,
            attempt: 1,
            error: None,
            scheduled_at: tokio::time::Instant::now(),
        });
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let revision_tracker = Arc::new(AtomicI64::new(after_revision));

        let task = tokio::spawn(run_reconnecting_fleet(
            dialer,
            revision_tracker.clone(),
            timing,
            events_tx,
            state_tx,
            shutdown_rx,
        ));

        Self {
            events_rx,
            state_rx,
            revision_tracker,
            shutdown_tx: Some(shutdown_tx),
            task: Some(task),
        }
    }

    /// Observe connection state changes.
    #[must_use]
    pub fn state(&self) -> watch::Receiver<ConnectionState> {
        self.state_rx.clone()
    }

    /// Wait for the next fleet event (revision or resync).
    ///
    /// Reconnects automatically on socket drops. Returns `Err(DaemonError::Io)`
    /// only when closed.
    pub async fn next_event(&mut self) -> Result<FleetStreamEvent, DaemonError> {
        self.events_rx.recv().await.unwrap_or_else(|| {
            Err(DaemonError::Io(
                "reconnecting subscription closed".to_string(),
            ))
        })
    }

    /// Update the highest folded revision.
    ///
    /// When the caller performs a joined or separate read at a higher revision,
    /// updating this ensures a subsequent reconnect will resume after that revision.
    pub fn set_after_revision(&self, revision: i64) {
        self.revision_tracker.fetch_max(revision, Ordering::SeqCst);
    }

    /// Get current covered revision.
    #[must_use]
    pub fn covered_revision(&self) -> i64 {
        self.revision_tracker.load(Ordering::SeqCst)
    }

    /// Close the subscription and wait for background task shutdown.
    pub async fn close(mut self) {
        if let Some(shutdown) = self.shutdown_tx.take() {
            let _ = shutdown.send(());
        }
        if let Some(mut task) = self.task.take() {
            if tokio::time::timeout(Duration::from_secs(1), &mut task).await.is_err() {
                task.abort();
            }
        }
    }
}

impl Drop for ReconnectingFleetSubscription {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown_tx.take() {
            let _ = shutdown.send(());
        }
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

async fn run_reconnecting_fleet(
    dialer: Dialer,
    revision_tracker: Arc<AtomicI64>,
    timing: Timing,
    events_tx: mpsc::Sender<Result<FleetStreamEvent, DaemonError>>,
    state_tx: watch::Sender<ConnectionState>,
    mut shutdown_rx: oneshot::Receiver<()>,
) {
    let socket_path = dialer().map(|c| c.socket().to_path_buf()).ok();
    let mut attempt: u32 = 1;

    loop {
        let covered = revision_tracker.load(Ordering::SeqCst);
        let dial_fut = async {
            let client = dialer()?;
            let (subscribe_result, subscription) = client.open_fleet_subscription(covered).await?;
            Ok::<_, DaemonError>((subscribe_result, subscription))
        };

        let dial_res = tokio::select! {
            _ = &mut shutdown_rx => {
                state_tx.send_replace(ConnectionState::Closed);
                return;
            }
            res = dial_fut => res,
        };

        let last_error: Option<String> = match dial_res {
            Ok((subscribe_result, mut subscription)) => {
                let connected_at = std::time::Instant::now();
                state_tx.send_replace(ConnectionState::Connected);

                // Handle replay events contiguous to head
                let mut replay_interrupted = false;
                match subscribe_result.replay_state {
                    FleetReplayState::Complete => {
                        for event in subscribe_result.replay {
                            revision_tracker.fetch_max(event.revision, Ordering::SeqCst);
                            if events_tx.send(Ok(FleetStreamEvent::Revision(event))).await.is_err()
                            {
                                replay_interrupted = true;
                                break;
                            }
                        }
                    }
                    FleetReplayState::SnapshotReset { .. } => {
                        // SnapshotReset indicates subscriber lag or bootstrap; the caller
                        // must perform a fresh fleet/snapshot read to reconcile state.
                        revision_tracker
                            .fetch_max(subscribe_result.snapshot.head_revision, Ordering::SeqCst);
                        if events_tx.send(Ok(FleetStreamEvent::ResyncRequired)).await.is_err() {
                            replay_interrupted = true;
                        }
                    }
                }

                if replay_interrupted {
                    state_tx.send_replace(ConnectionState::Closed);
                    return;
                }

                // Live event forwarder loop
                let disconnect_error: Option<String>;
                loop {
                    tokio::select! {
                        _ = &mut shutdown_rx => {
                            state_tx.send_replace(ConnectionState::Closed);
                            return;
                        }
                        event_res = subscription.next_event() => {
                            match event_res {
                                Ok(FleetStreamEvent::Revision(event)) => {
                                    revision_tracker.fetch_max(event.revision, Ordering::SeqCst);
                                    if events_tx.send(Ok(FleetStreamEvent::Revision(event))).await.is_err() {
                                        state_tx.send_replace(ConnectionState::Closed);
                                        return;
                                    }
                                }
                                Ok(FleetStreamEvent::ResyncRequired) => {
                                    if events_tx.send(Ok(FleetStreamEvent::ResyncRequired)).await.is_err() {
                                        state_tx.send_replace(ConnectionState::Closed);
                                        return;
                                    }
                                }
                                Err(err) => {
                                    if matches!(err, DaemonError::Io(_)) {
                                        if let Some(ref socket) = socket_path {
                                            crate::reset_host_id(socket);
                                        }
                                    }
                                    disconnect_error = Some(err.to_string());
                                    break;
                                }
                            }
                        }
                    }
                }

                // Reset backoff only after connection has held for at least 1s
                if connected_at.elapsed() >= Duration::from_secs(1) {
                    attempt = 1;
                }
                disconnect_error
            }
            Err(err) => Some(err.to_string()),
        };

        let delay = timing.delay_for_attempt(attempt);
        state_tx.send_replace(ConnectionState::Reconnecting {
            delay,
            attempt,
            error: last_error,
            scheduled_at: tokio::time::Instant::now(),
        });

        tokio::select! {
            _ = &mut shutdown_rx => {
                state_tx.send_replace(ConnectionState::Closed);
                return;
            }
            () = tokio::time::sleep(delay) => {}
        }
        attempt = attempt.saturating_add(1);
    }
}
