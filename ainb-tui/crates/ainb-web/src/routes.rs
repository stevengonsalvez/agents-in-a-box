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

use axum::body::Bytes;
use axum::extract::State;
use axum::http::StatusCode;
use axum::middleware;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::json;
use tokio::sync::watch;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::WatchStream;

use crate::auth;
use crate::config::WebConfig;
use crate::daemon::{Answerer, DaemonAnswerer};
use crate::data::{DataSource, FleetSnapshot};

/// Number of [`POLL_INTERVAL`] ticks between cost re-fetches. At least 1, so the
/// poller always fetches cost at startup and at least every `COST_POLL_INTERVAL`.
const COST_TICK_STRIDE: u64 = {
    let stride = COST_POLL_INTERVAL.as_secs() / POLL_INTERVAL.as_secs();
    if stride == 0 { 1 } else { stride }
};

/// How often the background poller refreshes the snapshot. The SSE stream only
/// emits when the fingerprint changes, so this is a safety-net cadence, not a
/// per-client cost.
const POLL_INTERVAL: Duration = Duration::from_secs(2);

/// How often the poller re-fetches `ainb fleet cost`. Cost cold-boots the
/// burndown plugin runtime per call and rolls up slowly, so it polls far less
/// often than sessions/needs. Between cost fetches the poller reuses the last
/// cost value held in the cached snapshot. Must be a multiple of
/// [`POLL_INTERVAL`]; the poller derives the tick stride from the two.
const COST_POLL_INTERVAL: Duration = Duration::from_secs(30);

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
    /// Web-push state, when push is configured. `None` disables every
    /// `/api/push/*` route (they answer `503 PUSH_NOT_CONFIGURED`) and the
    /// delivery loop. Shared so handlers and the delivery task see one store.
    pub push: Option<crate::push::PushState>,
    /// The `attention/answer` seam (D18): `POST /api/answer` routes an ASK-card
    /// answer through the daemon's ONE verified send path. A `dyn Answerer` so
    /// route tests inject a deterministic fake instead of dialling a socket.
    pub answer: Arc<dyn Answerer>,
    /// A receiver kept alive for the whole server lifetime so the channel never
    /// reports zero receivers. Without this, `watch::Sender::is_closed()` would
    /// be `true` at startup (the SSE handler creates the only other receiver
    /// lazily), and the background poller's shutdown guard would break on its
    /// very first tick — freezing the cache as stale forever.
    _cache_rx: watch::Receiver<CachedSnapshot>,
}

impl AppState {
    /// Build app state (no push) and spawn the background snapshot poller that
    /// maintains the cached snapshot every [`POLL_INTERVAL`].
    pub fn new(config: WebConfig, data: Arc<dyn DataSource>) -> Self {
        Self::with_push(config, data, None)
    }

    /// Build app state with an optional web-push backend, then spawn the
    /// background snapshot poller. Uses the production [`DaemonAnswerer`] for
    /// `POST /api/answer`.
    pub fn with_push(
        config: WebConfig,
        data: Arc<dyn DataSource>,
        push: Option<crate::push::PushState>,
    ) -> Self {
        Self::build(config, data, push, Arc::new(DaemonAnswerer))
    }

    /// Build app state with an explicit [`Answerer`] (the test seam), then spawn
    /// the background snapshot poller.
    pub fn with_answerer(
        config: WebConfig,
        data: Arc<dyn DataSource>,
        answer: Arc<dyn Answerer>,
    ) -> Self {
        Self::build(config, data, None, answer)
    }

    fn build(
        config: WebConfig,
        data: Arc<dyn DataSource>,
        push: Option<crate::push::PushState>,
        answer: Arc<dyn Answerer>,
    ) -> Self {
        let (tx, rx) = watch::channel(None);
        let state = Self {
            config: Arc::new(config),
            data,
            cache: tx,
            push,
            answer,
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
    pub(crate) async fn resolve_snapshot(
        &self,
    ) -> Result<Arc<FleetSnapshot>, crate::data::DataError> {
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

    /// Background task: poll the data source and update the cached snapshot when
    /// the fingerprint changes. Sessions + needs refresh every [`POLL_INTERVAL`]
    /// (~2s); cost refreshes only every [`COST_POLL_INTERVAL`] because
    /// `ainb fleet cost` cold-boots the burndown plugin runtime per call and
    /// rolls up slowly. On ticks that skip the cost fetch, the poller reuses the
    /// last cost value held in the cache, so the cached snapshot always carries
    /// the most recent cost. The poller runs the only `ainb`/`tmux`
    /// subprocesses; all requests and SSE streams read the cache.
    fn spawn_poller(&self) {
        let data = Arc::clone(&self.data);
        let tx = self.cache.clone();
        tokio::spawn(async move {
            let mut last_fp: Option<u64> = None;
            // The cost value carried forward between cost fetches. Seeded `Null`
            // (cost-absent) until the first cost fetch lands.
            let mut last_cost: serde_json::Value = serde_json::Value::Null;
            let mut tick: u64 = 0;
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

                // Fetch the fast-cadence surfaces every tick.
                let core = match data.core().await {
                    Ok(core) => core,
                    Err(e) => {
                        tracing::warn!(error = %e, "core snapshot poll failed");
                        tick = tick.wrapping_add(1);
                        continue;
                    }
                };

                // Re-fetch cost only on the slow cadence (and on the very first
                // tick). Reuse the carried-forward value in between. Cost is
                // best-effort and never fails the poll.
                if tick % COST_TICK_STRIDE == 0 {
                    last_cost = data.cost().await;
                }

                let snap = FleetSnapshot::from_parts(core, last_cost.clone());
                if last_fp != Some(snap.fingerprint) {
                    last_fp = Some(snap.fingerprint);
                    // Replace the cached snapshot; SSE subscribers and every
                    // handler observe the new value.
                    let _ = tx.send(Some(Arc::new(snap)));
                }
                tick = tick.wrapping_add(1);
            }
        });
    }
}

/// Build the full router with state, auth middleware, and static assets.
pub fn router(state: AppState) -> Router {
    // Authenticated surface: JSON API, SSE, push endpoints, and the WS terminal
    // upgrade. All gated by the bearer middleware (the WS terminal additionally
    // gates on `--read-only` inside its handler).
    // The WS terminal carries its own posture gate (`read_only_gate`) layered
    // *under* the shared bearer auth, so the refusal order is: auth first
    // (401), then read-only (403), then the upgrade.
    let terminal = Router::new().route("/ws/session/:id", get(crate::terminal::session_ws)).layer(
        middleware::from_fn_with_state(state.clone(), crate::terminal::read_only_gate),
    );

    let api = Router::new()
        .route("/healthz", get(healthz))
        .route("/api/snapshot", get(snapshot))
        .route("/api/sessions", get(sessions))
        .route("/api/needs", get(needs))
        .route("/api/answer", post(answer))
        .route("/api/cost", get(cost))
        .route("/api/events", get(events))
        .merge(terminal)
        .merge(crate::push::router())
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth::require_bearer,
        ));

    // Public shell: the SPA, static assets, and the PWA surface (manifest +
    // service worker). These carry no secrets and must be reachable before the
    // page can prompt for a token, so they are served without auth — exactly
    // like the existing index/static routes.
    let assets = Router::new()
        .route("/", get(crate::assets::handler))
        .route("/static/*path", get(crate::assets::handler))
        .route("/manifest.webmanifest", get(crate::assets::manifest))
        .route("/sw.js", get(crate::assets::service_worker));

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

/// `POST /api/answer` — answer one open ASK card through the daemon (D18).
///
/// Body: `{ attentionId, answer, answeredBy?, isAnswer? }`. `answeredBy`
/// defaults to `"web"` so the surface that won the race is recorded; `isAnswer`
/// defaults to `true` (a safety-critical interview answer — the daemon refuses
/// an ambiguous target rather than mis-route). The daemon runs the
/// first-answer-wins + C1 guards and performs the ONE verified last-mile send;
/// this route never touches tmux. The tagged [`AnswerResult`] is returned
/// verbatim as JSON so the frontend renders the right feedback
/// (`delivered` / `already_answered` / `ambiguous` / …).
async fn answer(State(state): State<AppState>, body: Bytes) -> Response {
    use ainb_hangar_proto::snapshots::AnswerParams;

    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct AnswerBody {
        attention_id: String,
        answer: String,
        #[serde(default)]
        answered_by: Option<String>,
        #[serde(default)]
        is_answer: Option<bool>,
    }

    let req: AnswerBody = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(e) => {
            let msg = format!("invalid answer body: {e}");
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": { "code": "INVALID_BODY", "message": msg } })),
            )
                .into_response();
        }
    };
    if req.attention_id.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": { "code": "INVALID_BODY", "message": "attentionId is required" }
            })),
        )
            .into_response();
    }

    let params = AnswerParams {
        attention_id: req.attention_id,
        answer: req.answer,
        answered_by: req.answered_by.unwrap_or_else(|| "web".to_string()),
        is_answer: req.is_answer.unwrap_or(true),
    };

    match state.answer.answer(params).await {
        Ok(result) => Json(result).into_response(),
        Err(e) => {
            let body = Json(json!({
                "error": { "code": "DAEMON_UNAVAILABLE", "message": e.to_string() }
            }));
            (StatusCode::BAD_GATEWAY, body).into_response()
        }
    }
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
        // `None` means the poller hasn't produced a snapshot yet — there is
        // genuinely nothing to send, so skip this tick (not an error).
        let snap = cached?;
        match serde_json::to_string(&*snap) {
            Ok(payload) => Some(Ok(Event::default().event("snapshot").data(payload))),
            // A serialize failure must NOT be swallowed into `None`: that would
            // leave the client connected and showing a stale "live" dashboard
            // while silently receiving nothing. Log it loudly AND push an
            // explicit `error` frame the client can surface.
            Err(e) => {
                tracing::warn!(error = %e, "failed to serialize SSE snapshot frame");
                let payload = json!({
                    "code": "SNAPSHOT_SERIALIZE_FAILED",
                    "message": "the server could not serialize the current snapshot",
                })
                .to_string();
                Some(Ok(Event::default().event("error").data(payload)))
            }
        }
    });

    Sse::new(stream).keep_alive(KeepAlive::new().interval(KEEPALIVE_INTERVAL).text("keepalive"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::{
        CoreFuture, CoreSnapshot, CostFuture, DataError, FleetSnapshot, SnapshotFuture,
    };
    use serde_json::{Value, json};
    use std::sync::atomic::{AtomicU64, Ordering};

    /// A data source whose `core` and `cost` fetch counters advance
    /// independently, so a test can observe the two poll cadences separately.
    /// The `core` payload embeds the fetch count as `tick` so cache changes are
    /// detectable; the `cost` payload embeds its own fetch count as `fetch`.
    struct FakeSource {
        core_fetches: AtomicU64,
        cost_fetches: AtomicU64,
    }

    impl FakeSource {
        fn new() -> Self {
            Self {
                core_fetches: AtomicU64::new(0),
                cost_fetches: AtomicU64::new(0),
            }
        }

        /// Number of times [`DataSource::cost`] has been called.
        fn cost_fetch_count(&self) -> u64 {
            self.cost_fetches.load(Ordering::SeqCst)
        }
    }

    impl DataSource for FakeSource {
        fn snapshot(&self) -> SnapshotFuture<'_> {
            Box::pin(async move {
                let core = self.core().await?;
                let cost = self.cost().await;
                Ok::<_, DataError>(FleetSnapshot::from_parts(core, cost))
            })
        }

        fn core(&self) -> CoreFuture<'_> {
            Box::pin(async move {
                let n = self.core_fetches.fetch_add(1, Ordering::SeqCst);
                Ok::<_, DataError>(CoreSnapshot {
                    sessions: json!([{ "tick": n }]),
                    needs: json!([]),
                })
            })
        }

        fn cost(&self) -> CostFuture<'_> {
            Box::pin(async move {
                let n = self.cost_fetches.fetch_add(1, Ordering::SeqCst);
                json!({ "fetch": n })
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
        let first = state.cached().expect("poller should have seeded the cache on its first tick");
        let first_tick = first.sessions[0]["tick"].clone();

        // Advance well past the poll interval with NO SSE subscriber alive.
        // If the `is_closed()` guard were still tripping, the cache would never
        // change here.
        tokio::time::advance(POLL_INTERVAL * 3).await;
        tokio::task::yield_now().await;
        tokio::task::yield_now().await;

        let later = state.cached().expect("cache must still be populated after later polls");
        let later_tick = later.sessions[0]["tick"].clone();

        assert_ne!(
            first_tick, later_tick,
            "background poller must refresh the cached snapshot over time even \
             with no SSE client connected"
        );
    }

    /// Cost must poll on the slow [`COST_POLL_INTERVAL`] cadence while
    /// sessions/needs poll on the fast [`POLL_INTERVAL`] one. Over a window of
    /// many fast ticks, `core` is fetched on every tick but `cost` only once per
    /// `COST_TICK_STRIDE` ticks — and the cached snapshot always carries the
    /// most recent cost value.
    #[tokio::test(start_paused = true)]
    async fn cost_polls_slower_than_sessions_and_needs() {
        let config = WebConfig {
            listen: "127.0.0.1:0".parse().unwrap(),
            token: None,
            insecure_bind: false,
            read_only: true,
        };
        let source = Arc::new(FakeSource::new());
        let state = AppState::new(config, Arc::clone(&source) as Arc<dyn DataSource>);

        // First tick fires immediately: one core fetch and one cost fetch.
        tokio::time::advance(Duration::from_millis(1)).await;
        tokio::task::yield_now().await;
        assert_eq!(
            source.cost_fetch_count(),
            1,
            "cost must be fetched on the first tick"
        );
        let after_first = state.cached().expect("cache seeded on first tick");
        assert_eq!(
            after_first.cost,
            json!({ "fetch": 0 }),
            "cached snapshot must carry the first cost value"
        );

        // Advance through (COST_TICK_STRIDE - 1) more fast ticks. Each refreshes
        // core but must NOT re-fetch cost yet — cost stays carried forward.
        for _ in 1..COST_TICK_STRIDE {
            tokio::time::advance(POLL_INTERVAL).await;
            tokio::task::yield_now().await;
        }
        assert_eq!(
            source.cost_fetch_count(),
            1,
            "cost must NOT be re-fetched within a single cost interval"
        );
        let mid = state.cached().expect("cache present mid-interval");
        assert_eq!(
            mid.cost,
            json!({ "fetch": 0 }),
            "between cost fetches the cached snapshot reuses the last cost value"
        );

        // The next fast tick crosses the cost interval boundary → cost re-fetched.
        tokio::time::advance(POLL_INTERVAL).await;
        tokio::task::yield_now().await;
        assert_eq!(
            source.cost_fetch_count(),
            2,
            "cost must be re-fetched once a full COST_POLL_INTERVAL elapses"
        );
        let after_second = state.cached().expect("cache present after second cost fetch");
        assert_eq!(
            after_second.cost,
            json!({ "fetch": 1 }),
            "cached snapshot must carry the refreshed cost value"
        );

        // Core was fetched on every tick across the whole window; cost only twice.
        assert_eq!(
            source.core_fetches.load(Ordering::SeqCst),
            COST_TICK_STRIDE + 1,
            "core must be fetched on every fast tick"
        );
    }
}
