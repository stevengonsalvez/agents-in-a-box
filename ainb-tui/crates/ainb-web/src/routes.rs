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
use tokio::sync::broadcast;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::BroadcastStream;

use crate::auth;
use crate::config::WebConfig;
use crate::data::{DataSource, FleetSnapshot};

/// How often the background poller refreshes the snapshot. The SSE stream only
/// emits when the fingerprint changes, so this is a safety-net cadence, not a
/// per-client cost.
const POLL_INTERVAL: Duration = Duration::from_secs(2);

/// SSE keep-alive comment cadence (keeps proxies from dropping idle streams).
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(15);

/// Channel capacity for the snapshot broadcast. Small — subscribers that lag
/// just receive the next fresh snapshot rather than a backlog.
const BROADCAST_CAPACITY: usize = 16;

/// Shared, cheaply-clonable application state handed to every handler.
#[derive(Clone)]
pub struct AppState {
    /// Immutable runtime config (bind addr, token, posture).
    pub config: Arc<WebConfig>,
    /// The data source backing every API route.
    pub data: Arc<dyn DataSource>,
    /// Broadcast channel carrying the latest snapshot JSON to SSE subscribers.
    pub updates: broadcast::Sender<Arc<String>>,
}

impl AppState {
    /// Build app state and spawn the background snapshot poller that feeds the
    /// SSE broadcast channel.
    pub fn new(config: WebConfig, data: Arc<dyn DataSource>) -> Self {
        let (tx, _rx) = broadcast::channel(BROADCAST_CAPACITY);
        let state = Self {
            config: Arc::new(config),
            data,
            updates: tx,
        };
        state.spawn_poller();
        state
    }

    /// Background task: poll the data source every [`POLL_INTERVAL`] and
    /// broadcast the serialized snapshot when the fingerprint changes.
    fn spawn_poller(&self) {
        let data = Arc::clone(&self.data);
        let tx = self.updates.clone();
        tokio::spawn(async move {
            let mut last_fp: Option<u64> = None;
            let mut ticker = tokio::time::interval(POLL_INTERVAL);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                ticker.tick().await;
                // No active SSE subscribers → skip the work entirely.
                if tx.receiver_count() == 0 {
                    last_fp = None;
                    continue;
                }
                match data.snapshot().await {
                    Ok(snap) => {
                        if last_fp != Some(snap.fingerprint) {
                            last_fp = Some(snap.fingerprint);
                            if let Ok(payload) = serde_json::to_string(&snap) {
                                // Ignore send errors (no receivers is fine).
                                let _ = tx.send(Arc::new(payload));
                            }
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

/// `GET /api/snapshot` — the full dashboard payload in one call.
async fn snapshot(State(state): State<AppState>) -> Response {
    match state.data.snapshot().await {
        Ok(snap) => Json(snap).into_response(),
        Err(e) => data_error_response(e),
    }
}

/// `GET /api/sessions` — just the live session list.
async fn sessions(State(state): State<AppState>) -> Response {
    project(&state, |s| s.sessions).await
}

/// `GET /api/needs` — fleet needs (ASK/ERR/IDLE/WAIT).
async fn needs(State(state): State<AppState>) -> Response {
    project(&state, |s| s.needs).await
}

/// `GET /api/cost` — cost rollups (`null` when the verb is absent).
async fn cost(State(state): State<AppState>) -> Response {
    project(&state, |s| s.cost).await
}

/// Shared helper: fetch a snapshot and return one projected field as JSON.
async fn project(
    state: &AppState,
    pick: impl FnOnce(FleetSnapshot) -> serde_json::Value,
) -> Response {
    match state.data.snapshot().await {
        Ok(snap) => Json(pick(snap)).into_response(),
        Err(e) => data_error_response(e),
    }
}

/// `GET /api/events` — SSE stream of `snapshot` events. Emits the current
/// snapshot immediately on connect, then pushes a fresh payload whenever the
/// fingerprint changes (via the background poller's broadcast channel).
async fn events(
    State(state): State<AppState>,
) -> Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>> {
    let rx = state.updates.subscribe();

    // Initial snapshot, sent eagerly so the client renders without waiting for
    // the first change.
    let initial = match state.data.snapshot().await {
        Ok(snap) => serde_json::to_string(&snap).ok(),
        Err(e) => {
            tracing::warn!(error = %e, "initial SSE snapshot failed");
            None
        }
    };
    let initial_event = initial.map(|payload| Ok(Event::default().event("snapshot").data(payload)));

    let live = BroadcastStream::new(rx).filter_map(|res| match res {
        Ok(payload) => Some(Ok(Event::default()
            .event("snapshot")
            .data((*payload).clone()))),
        // Lagged: drop the gap, the next fresh snapshot recovers state.
        Err(_) => None,
    });

    let stream = tokio_stream::iter(initial_event.into_iter()).chain(live);

    Sse::new(stream).keep_alive(KeepAlive::new().interval(KEEPALIVE_INTERVAL).text("keepalive"))
}
