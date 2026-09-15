//! Axum router, API handlers, and the SSE live-update stream.
//!
//! The read surfaces proxy the existing `ainb --format json` commands via the
//! data layer ([`crate::data::DataSource`]), so the dashboard never duplicates
//! data access. The two write surfaces — the WS terminal and `POST /api/answer`
//! (the daemon send seam) — are each gated by
//! [`crate::terminal::read_only_gate`], so `--read-only` refuses them with
//! `403 READ_ONLY` and the dashboard is viewer-only. Live updates are delivered
//! via Server-Sent Events:
//! a background poller refreshes the snapshot and pushes to subscribers only
//! when the content fingerprint changes.

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use ainb_app::wire::web::WebCost;
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

/// How often the background poller refreshes the snapshot. The SSE stream only
/// emits when the fingerprint changes, so this is a safety-net cadence, not a
/// per-client cost.
const POLL_INTERVAL: Duration = Duration::from_secs(2);

/// How often the cost task re-fetches `ainb fleet cost`. Cost cold-boots the
/// burndown plugin runtime per call and rolls up slowly, so it runs on its own
/// task and cadence and never holds up sessions or needs (#1055).
const COST_POLL_INTERVAL: Duration = Duration::from_secs(30);

/// The first bound on one cost fetch. A fetch past it is abandoned (its process
/// is killed), the last cost value is kept, and the next fetch gets twice the
/// time, up to [`COST_FETCH_TIMEOUT_CAP`]; a fetch that lands resets it. A lone
/// `fleet cost` returns in under a second, but #1055 measured 120 s under
/// contention, so the cap sits above that.
const COST_FETCH_TIMEOUT: Duration = Duration::from_secs(30);

/// The most time one cost fetch is ever given.
const COST_FETCH_TIMEOUT_CAP: Duration = Duration::from_secs(240);

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
    /// (before the first poll completes) fall back to a single direct fetch of
    /// sessions and needs so the very first request after startup still
    /// succeeds. Cost is not fetched here: a second, concurrent `fleet cost`
    /// beside the cost task's is what stalled the first snapshot for 120 s
    /// (#1055). The cold snapshot carries no cost until the cost task lands.
    pub(crate) async fn resolve_snapshot(
        &self,
    ) -> Result<Arc<FleetSnapshot>, crate::data::DataError> {
        if let Some(snap) = self.cached() {
            return Ok(snap);
        }
        let core = self.data.core().await?;
        let snap = Arc::new(FleetSnapshot::from_parts(core, None));
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

    /// Background tasks: poll the data source and update the cached snapshot
    /// when the fingerprint changes.
    ///
    /// Sessions and needs refresh every [`POLL_INTERVAL`] (about 2 s) on one
    /// task, and are published as soon as they are read. Cost refreshes every
    /// [`COST_POLL_INTERVAL`] on a second task, bounded by
    /// [`COST_FETCH_TIMEOUT`]: `ainb fleet cost` cold-boots the burndown plugin
    /// runtime per call and can take minutes under contention, so it must never
    /// gate the first snapshot or a new session or card (#1055). When cost lands
    /// it is folded into the cached snapshot at once; the fast task reads the
    /// latest cost each tick. The tasks run the only `ainb`/`tmux` subprocesses;
    /// requests and SSE streams read the cache.
    fn spawn_poller(&self) {
        let (cost_tx, cost_rx) = watch::channel(None);
        self.spawn_cost_task(cost_tx);
        let data = Arc::clone(&self.data);
        let tx = self.cache.clone();
        tokio::spawn(async move {
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
                let core = match data.core().await {
                    Ok(core) => core,
                    Err(e) => {
                        tracing::warn!(error = %e, "core snapshot poll failed");
                        continue;
                    }
                };
                let snap = FleetSnapshot::from_parts(core, cost_rx.borrow().clone());
                // Compare with what the cache holds, not a local copy: the cost
                // task also writes the cache, and a stale local fingerprint
                // would republish the same snapshot to every SSE stream.
                let _ = tx.send_if_modified(|slot| {
                    if slot.as_ref().map(|held| held.fingerprint) == Some(snap.fingerprint) {
                        return false;
                    }
                    *slot = Some(Arc::new(snap));
                    true
                });
            }
        });
    }

    /// The cost task: fetch cost now and every [`COST_POLL_INTERVAL`], one fetch
    /// at a time, each bounded by [`COST_FETCH_TIMEOUT`]. A fresh value goes to
    /// the fast task through `cost_tx` and straight into the cached snapshot.
    fn spawn_cost_task(&self, cost_tx: watch::Sender<Option<WebCost>>) {
        let data = Arc::clone(&self.data);
        let tx = self.cache.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(COST_POLL_INTERVAL);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let mut bound = COST_FETCH_TIMEOUT;
            loop {
                ticker.tick().await;
                if tx.is_closed() {
                    break;
                }
                let Ok(cost) = tokio::time::timeout(bound, data.cost()).await else {
                    tracing::warn!(
                        timeout_s = bound.as_secs(),
                        "fleet cost fetch timed out; keeping the last cost"
                    );
                    bound = (bound * 2).min(COST_FETCH_TIMEOUT_CAP);
                    continue;
                };
                bound = COST_FETCH_TIMEOUT;
                if *cost_tx.borrow() == cost {
                    continue;
                }
                cost_tx.send_replace(cost.clone());
                tx.send_if_modified(|slot| {
                    let Some(snap) = slot.as_ref() else {
                        return false;
                    };
                    let core = crate::data::CoreSnapshot {
                        sessions: snap.sessions.clone(),
                        needs: snap.needs.clone(),
                    };
                    *slot = Some(Arc::new(FleetSnapshot::from_parts(core, cost.clone())));
                    true
                });
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

    // `POST /api/answer` is the *second* fleet-state write surface: it drives the
    // daemon's verified last-mile send into a live session (approvals, ASK
    // answers, free-text into the picker). It must carry the SAME posture gate as
    // the WS terminal — `read_only_gate` refuses it with `403 READ_ONLY` in
    // `--read-only` mode — so the read-only dashboard truly never mutates fleet
    // state and the `--insecure-bind + --read-only` bind exemption
    // ([`WebConfig::check_bind_security`]) stays sound (it exists only because no
    // write surface is reachable in that posture). Both write surfaces sit under
    // the shared bearer auth below, so the refusal order matches the terminal:
    // auth first (401), then read-only (403).
    let answer_route =
        Router::new()
            .route("/api/answer", post(answer))
            .layer(middleware::from_fn_with_state(
                state.clone(),
                crate::terminal::read_only_gate,
            ));

    let api = Router::new()
        .route("/healthz", get(healthz))
        .route("/api/snapshot", get(snapshot))
        .route("/api/sessions", get(sessions))
        .route("/api/needs", get(needs))
        .route("/api/cost", get(cost))
        .route("/api/events", get(events))
        .merge(answer_route)
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
        mutation: ainb_hangar_proto::mutation::MutationEnvelope::default(),
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

/// `GET /api/cost` — the projected cost panel (`null` when the verb is absent).
async fn cost(State(state): State<AppState>) -> Response {
    project(&state, |s| &s.cost).await
}

/// Shared helper: read the cached snapshot and return one projected field as
/// JSON. Borrows the cached value rather than re-shelling per request.
async fn project<T: serde::Serialize>(
    state: &AppState,
    pick: impl FnOnce(&FleetSnapshot) -> &T,
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
    use crate::data::{CoreFuture, CoreSnapshot, CostFuture, DataError};
    use serde_json::json;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// A data source whose `core` and `cost` fetch counters advance
    /// independently, so a test can observe the two poll cadences separately.
    /// The `core` payload embeds the fetch count as `tick` so cache changes are
    /// detectable; the `cost` panel embeds its own fetch count as its call count.
    struct FakeSource {
        core_fetches: AtomicU64,
        cost_fetches: AtomicU64,
        /// Cost never resolves: the stalled `fleet cost` of #1055.
        cost_hangs: bool,
    }

    impl FakeSource {
        fn new() -> Self {
            Self {
                core_fetches: AtomicU64::new(0),
                cost_fetches: AtomicU64::new(0),
                cost_hangs: false,
            }
        }

        fn with_hanging_cost() -> Self {
            Self {
                cost_hangs: true,
                ..Self::new()
            }
        }

        /// Number of times [`DataSource::cost`] has been called.
        fn cost_fetch_count(&self) -> u64 {
            self.cost_fetches.load(Ordering::SeqCst)
        }
    }

    impl DataSource for FakeSource {
        fn core(&self) -> CoreFuture<'_> {
            Box::pin(async move {
                let n = self.core_fetches.fetch_add(1, Ordering::SeqCst);
                Ok::<_, DataError>(CoreSnapshot {
                    sessions: json!([{ "tick": n }]),
                    needs: Vec::new(),
                })
            })
        }

        fn cost(&self) -> CostFuture<'_> {
            Box::pin(async move {
                let n = self.cost_fetches.fetch_add(1, Ordering::SeqCst);
                if self.cost_hangs {
                    std::future::pending::<()>().await;
                }
                Some(fetched_cost(n))
            })
        }
    }

    /// A cost panel that records which fetch produced it, as its call count.
    fn fetched_cost(n: u64) -> WebCost {
        let mut cost = WebCost::default();
        cost.totals.bucket.call_count = n;
        cost
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

    fn test_config() -> WebConfig {
        WebConfig {
            listen: "127.0.0.1:0".parse().unwrap(),
            token: None,
            insecure_bind: false,
            read_only: true,
        }
    }

    async fn settle() {
        for _ in 0..4 {
            tokio::task::yield_now().await;
        }
    }

    /// Cost polls on the slow [`COST_POLL_INTERVAL`] cadence on its own task,
    /// sessions and needs on the fast [`POLL_INTERVAL`] one, and the cached
    /// snapshot carries the latest cost as soon as it lands.
    #[tokio::test(start_paused = true)]
    async fn cost_polls_slower_than_sessions_and_needs() {
        let source = Arc::new(FakeSource::new());
        let state = AppState::new(test_config(), Arc::clone(&source) as Arc<dyn DataSource>);

        tokio::time::advance(Duration::from_millis(1)).await;
        settle().await;
        assert_eq!(source.cost_fetch_count(), 1, "cost is fetched at startup");
        assert_eq!(
            state.cached().expect("cache seeded").cost,
            Some(fetched_cost(0)),
            "the first cost lands in the cached snapshot"
        );

        let fast_ticks = COST_POLL_INTERVAL.as_secs() / POLL_INTERVAL.as_secs();
        for _ in 1..fast_ticks {
            tokio::time::advance(POLL_INTERVAL).await;
            settle().await;
        }
        assert_eq!(
            source.cost_fetch_count(),
            1,
            "cost is not re-fetched within one cost interval"
        );
        assert_eq!(
            state.cached().expect("cache present").cost,
            Some(fetched_cost(0))
        );

        tokio::time::advance(POLL_INTERVAL).await;
        settle().await;
        assert_eq!(
            source.cost_fetch_count(),
            2,
            "cost is re-fetched once a full cost interval elapses"
        );
        assert_eq!(
            state.cached().expect("cache present").cost,
            Some(fetched_cost(1))
        );
        assert!(
            source.core_fetches.load(Ordering::SeqCst) >= fast_ticks,
            "core is fetched on every fast tick"
        );
    }

    /// #1055: a cost fetch that never returns must not hold up sessions and
    /// needs. The first snapshot publishes on the first tick, later ticks keep
    /// refreshing, and the stalled fetch is abandoned at the timeout.
    #[tokio::test(start_paused = true)]
    async fn a_stalled_cost_fetch_never_gates_sessions_and_needs() {
        let source = Arc::new(FakeSource::with_hanging_cost());
        let state = AppState::new(test_config(), Arc::clone(&source) as Arc<dyn DataSource>);

        tokio::time::advance(Duration::from_millis(1)).await;
        settle().await;
        let first = state.cached().expect("sessions publish without waiting on cost");
        assert_eq!(first.cost, None, "no cost yet");

        tokio::time::advance(POLL_INTERVAL).await;
        settle().await;
        let later = state.cached().expect("cache present");
        assert_ne!(
            first.sessions, later.sessions,
            "the fast task keeps refreshing"
        );

        tokio::time::advance(COST_FETCH_TIMEOUT + COST_POLL_INTERVAL).await;
        settle().await;
        assert_eq!(
            source.cost_fetch_count(),
            2,
            "the stalled fetch was abandoned and the next one started"
        );
    }

    /// A cost fetch that times out gives the next one twice the time, up to the
    /// cap, so a slow but finite `fleet cost` eventually lands.
    #[tokio::test(start_paused = true)]
    async fn each_cost_timeout_doubles_the_next_bound_up_to_the_cap() {
        let source = Arc::new(FakeSource::with_hanging_cost());
        let _state = AppState::new(test_config(), Arc::clone(&source) as Arc<dyn DataSource>);
        tokio::time::advance(Duration::from_millis(1)).await;
        settle().await;
        assert_eq!(source.cost_fetch_count(), 1);

        // First bound 30 s: the second fetch starts right after it.
        tokio::time::advance(COST_FETCH_TIMEOUT).await;
        settle().await;
        assert_eq!(source.cost_fetch_count(), 2);

        // Second bound 60 s: nothing new until it expires.
        tokio::time::advance(COST_FETCH_TIMEOUT * 2 - Duration::from_secs(1)).await;
        settle().await;
        assert_eq!(source.cost_fetch_count(), 2, "the second fetch has 60 s");
        tokio::time::advance(Duration::from_secs(1)).await;
        settle().await;
        assert_eq!(source.cost_fetch_count(), 3);

        // The bound never passes the cap.
        assert!(
            COST_FETCH_TIMEOUT_CAP > Duration::from_secs(120),
            "above the measured 120 s"
        );
    }

    /// #1055: a cold request does not start a second `fleet cost` beside the
    /// cost task's.
    #[tokio::test(start_paused = true)]
    async fn a_cold_request_reads_sessions_and_needs_without_cost() {
        let source = Arc::new(FakeSource::with_hanging_cost());
        let state = AppState::new(test_config(), Arc::clone(&source) as Arc<dyn DataSource>);
        let snap = state.resolve_snapshot().await.expect("cold snapshot");
        assert!(snap.sessions.is_array());
        assert!(
            source.cost_fetch_count() <= 1,
            "only the cost task fetches cost"
        );
    }
}
