//! Axum router, API handlers, and the SSE live-update stream.
//!
//! All routes are read-only. The data layer ([`crate::data::DataSource`])
//! proxies the existing `ainb --format json` commands, so the dashboard never
//! duplicates data access. Live updates are delivered via Server-Sent Events:
//! a background poller refreshes the snapshot and pushes to subscribers only
//! when the content fingerprint changes.

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::middleware;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde_json::json;
use tokio::sync::watch;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::WatchStream;

use crate::auth;
use crate::config::WebConfig;
use crate::data::{DataSource, FleetSnapshot};

/// How often the background poller refreshes the snapshot. The SSE stream only
/// emits when the fingerprint changes, so this is a safety-net cadence, not a
/// per-client cost.
const POLL_INTERVAL: Duration = Duration::from_secs(2);

/// SSE keep-alive comment cadence (keeps proxies from dropping idle streams).
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(15);

/// The single cached snapshot the whole server reads from. `None` only before
/// the poller's first successful fetch completes.
type CachedSnapshot = Option<Arc<FleetSnapshot>>;

/// Shared, cheaply-clonable application state handed to every handler.
///
/// Every API request and SSE connection reads the *cached* snapshot maintained
/// by a single background poller — they never re-shell the `ainb` subprocesses
/// (which in turn spawn a `tmux capture-pane` per session). This coalesces all
/// load onto one [`POLL_INTERVAL`]-cadence refresh regardless of request volume
/// or how many SSE clients are connected.
#[derive(Clone)]
pub struct AppState {
    /// Immutable runtime config (bind addr, token, posture).
    pub config: Arc<WebConfig>,
    /// The data source backing the background poller. Handlers do NOT call this
    /// directly except on a cold cache (before the first poll lands).
    pub data: Arc<dyn DataSource>,
    /// Watch channel holding the latest cached snapshot. Handlers borrow it;
    /// SSE subscribers stream changes off it. Updated only by the poller.
    pub cache: watch::Sender<CachedSnapshot>,
    /// A receiver kept alive for the whole server lifetime so the channel never
    /// reports zero receivers. Without this, `watch::Sender::is_closed()` would
    /// be `true` at startup (the SSE handler creates the only other receiver
    /// lazily), and the background poller's shutdown guard would break on its
    /// very first tick — freezing the cache as stale forever.
    _cache_rx: watch::Receiver<CachedSnapshot>,
}

impl AppState {
    /// Build app state and spawn the background snapshot poller that maintains
    /// the cached snapshot every [`POLL_INTERVAL`].
    pub fn new(config: WebConfig, data: Arc<dyn DataSource>) -> Self {
        let (tx, rx) = watch::channel(None);
        let state = Self {
            config: Arc::new(config),
            data,
            cache: tx,
            _cache_rx: rx,
        };
        state.spawn_poller();
        state
    }

    /// Return the last good cached snapshot, or `None` if the poller hasn't
    /// produced one yet.
    fn cached(&self) -> CachedSnapshot {
        self.cache.borrow().clone()
    }

    /// Resolve a snapshot for a request: prefer the cache; on a cold cache
    /// (before the first poll completes) fall back to a single direct fetch so
    /// the very first request after startup still succeeds.
    async fn resolve_snapshot(&self) -> Result<Arc<FleetSnapshot>, crate::data::DataError> {
        if let Some(snap) = self.cached() {
            return Ok(snap);
        }
        let snap = Arc::new(self.data.snapshot().await?);
        // Seed the cache so concurrent cold-start requests coalesce too.
        let _ = self.cache.send_if_modified(|slot| {
            if slot.is_none() {
                *slot = Some(Arc::clone(&snap));
                true
            } else {
                false
            }
        });
        Ok(self.cached().unwrap_or(snap))
    }

    /// Background task: poll the data source every [`POLL_INTERVAL`] and update
    /// the cached snapshot when the fingerprint changes. The poller runs the
    /// only `ainb`/`tmux` subprocesses; all requests and SSE streams read the
    /// cache, so load is bounded to one refresh per interval.
    fn spawn_poller(&self) {
        let data = Arc::clone(&self.data);
        let tx = self.cache.clone();
        tokio::spawn(async move {
            let mut last_fp: Option<u64> = None;
            let mut ticker = tokio::time::interval(POLL_INTERVAL);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                // Refresh immediately on the first iteration (the interval's
                // first tick completes instantly), then every POLL_INTERVAL.
                ticker.tick().await;
                // Stop polling once nothing holds the state (server shut down).
                if tx.is_closed() {
                    break;
                }
                match data.snapshot().await {
                    Ok(snap) => {
                        if last_fp != Some(snap.fingerprint) {
                            last_fp = Some(snap.fingerprint);
                            // Replace the cached snapshot; SSE subscribers and
                            // every handler observe the new value.
                            let _ = tx.send(Some(Arc::new(snap)));
                        }
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "snapshot poll failed");
                    }
                }
            }
        });
    }
}

/// Build the full router with state, auth middleware, and static assets.
pub fn router(state: AppState) -> Router {
    let api = Router::new()
        .route("/healthz", get(healthz))
        .route("/api/snapshot", get(snapshot))
        .route("/api/sessions", get(sessions))
        .route("/api/needs", get(needs))
        .route("/api/cost", get(cost))
        .route("/api/events", get(events))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth::require_bearer,
        ));

    let assets = Router::new()
        .route("/", get(crate::assets::handler))
        .route("/static/*path", get(crate::assets::handler));

    api.merge(assets).with_state(state)
}

/// Map a [`crate::data::DataError`] to a 502 JSON envelope.
fn data_error_response(e: crate::data::DataError) -> Response {
    let body = Json(json!({
        "error": { "code": "UPSTREAM_FAILED", "message": e.to_string() }
    }));
    (StatusCode::BAD_GATEWAY, body).into_response()
}

/// `GET /healthz` — liveness + posture. Token detail is only disclosed when the
/// request is authorized (the auth middleware already gates this route).
async fn healthz(State(state): State<AppState>) -> Response {
    Json(json!({
        "ok": true,
        "readOnly": state.config.read_only,
        "tokenRequired": state.config.token.is_some(),
        "version": env!("CARGO_PKG_VERSION"),
    }))
    .into_response()
}

/// `GET /api/snapshot` — the full dashboard payload in one call. Served from
/// the cached snapshot maintained by the background poller.
async fn snapshot(State(state): State<AppState>) -> Response {
    match state.resolve_snapshot().await {
        Ok(snap) => Json(&*snap).into_response(),
        Err(e) => data_error_response(e),
    }
}

/// `GET /api/sessions` — just the live session list.
async fn sessions(State(state): State<AppState>) -> Response {
    project(&state, |s| &s.sessions).await
}

/// `GET /api/needs` — fleet needs (ASK/ERR/IDLE/WAIT).
async fn needs(State(state): State<AppState>) -> Response {
    project(&state, |s| &s.needs).await
}

/// `GET /api/cost` — cost rollups (`null` when the verb is absent).
async fn cost(State(state): State<AppState>) -> Response {
    project(&state, |s| &s.cost).await
}

/// Shared helper: read the cached snapshot and return one projected field as
/// JSON. Borrows the cached value rather than re-shelling per request.
async fn project(
    state: &AppState,
    pick: impl FnOnce(&FleetSnapshot) -> &serde_json::Value,
) -> Response {
    match state.resolve_snapshot().await {
        Ok(snap) => Json(pick(&snap)).into_response(),
        Err(e) => data_error_response(e),
    }
}

/// `GET /api/events` — SSE stream of `snapshot` events. Emits the current
/// *cached* snapshot immediately on connect, then pushes a fresh payload
/// whenever the poller updates the cache. A new connection costs nothing beyond
/// subscribing to the watch channel — it never re-shells `ainb`/`tmux`.
async fn events(
    State(state): State<AppState>,
) -> Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>> {
    // Ensure the cache is warm so a connection that arrives before the first
    // poll still gets an initial frame (single shared fetch on a cold cache).
    let _ = state.resolve_snapshot().await;

    // `WatchStream::new` yields the current value first, then every subsequent
    // change — so connecting clients receive the initial frame for free.
    let rx = state.cache.subscribe();
    let stream = WatchStream::new(rx).filter_map(|cached: CachedSnapshot| {
        let snap = cached?;
        let payload = serde_json::to_string(&*snap).ok()?;
        Some(Ok(Event::default().event("snapshot").data(payload)))
    });

    Sse::new(stream).keep_alive(KeepAlive::new().interval(KEEPALIVE_INTERVAL).text("keepalive"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::{DataError, FleetSnapshot, SnapshotFuture};
    use serde_json::{Value, json};
    use std::sync::atomic::{AtomicU64, Ordering};

    /// A data source whose snapshot fingerprint advances on every fetch, so we
    /// can observe whether the background poller is actually refreshing the
    /// cache over time.
    struct FakeSource {
        ticks: AtomicU64,
    }

    impl FakeSource {
        fn new() -> Self {
            Self {
                ticks: AtomicU64::new(0),
            }
        }
    }

    impl DataSource for FakeSource {
        fn snapshot(&self) -> SnapshotFuture<'_> {
            Box::pin(async move {
                let n = self.ticks.fetch_add(1, Ordering::SeqCst);
                let sessions = json!([{ "tick": n }]);
                let needs: Value = json!([]);
                let cost = Value::Null;
                let fingerprint =
                    FleetSnapshot::compute_fingerprint(&sessions, &needs, &cost);
                Ok::<_, DataError>(FleetSnapshot {
                    sessions,
                    needs,
                    cost,
                    fingerprint,
                })
            })
        }
    }

    /// Regression: the background poller must keep refreshing the cache even
    /// when no SSE client is ever connected. Previously the poller's
    /// `is_closed()` guard tripped on tick #0 (AppState held only the Sender,
    /// the seed Receiver was dropped), so the cache froze stale forever.
    #[tokio::test(start_paused = true)]
    async fn poller_refreshes_cache_without_any_sse_client() {
        let config = WebConfig {
            listen: "127.0.0.1:0".parse().unwrap(),
            token: None,
            insecure_bind: false,
            read_only: true,
        };
        let state = AppState::new(config, Arc::new(FakeSource::new()));

        // Let the poller run its first tick (fires immediately) and land an
        // initial snapshot.
        tokio::time::advance(Duration::from_millis(1)).await;
        tokio::task::yield_now().await;
        let first = state
            .cached()
            .expect("poller should have seeded the cache on its first tick");
        let first_tick = first.sessions[0]["tick"].clone();

        // Advance well past the poll interval with NO SSE subscriber alive.
        // If the `is_closed()` guard were still tripping, the cache would never
        // change here.
        tokio::time::advance(POLL_INTERVAL * 3).await;
        tokio::task::yield_now().await;
        tokio::task::yield_now().await;

        let later = state
            .cached()
            .expect("cache must still be populated after later polls");
        let later_tick = later.sessions[0]["tick"].clone();

        assert_ne!(
            first_tick, later_tick,
            "background poller must refresh the cached snapshot over time even \
             with no SSE client connected"
        );
    }
}
