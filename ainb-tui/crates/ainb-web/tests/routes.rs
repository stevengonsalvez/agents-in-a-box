//! Route + auth integration tests for the dashboard router.
//!
//! These drive the real axum `Router` (with the bearer-auth middleware and a
//! fake data source) via `tower::ServiceExt::oneshot`, so they exercise the
//! full request path — routing, auth, JSON serialization — without binding a
//! socket or spawning the `ainb` binary.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use ainb_hangar_proto::snapshots::{AnswerParams, AnswerResult};
use ainb_web::daemon::{Answerer, DaemonError};
use ainb_web::data::{
    CoreFuture, CoreSnapshot, CostFuture, DataError, DataSource, FleetSnapshot, SnapshotFuture,
};
use ainb_web::{AppState, WebConfig, router};
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use serde_json::{Value, json};
use tower::ServiceExt;

/// Deterministic data source returning a fixed snapshot — no subprocess.
struct FakeSource;

impl FakeSource {
    fn fixed_core() -> CoreSnapshot {
        let sessions = json!([
            {
                "session_id": "abc",
                "tmux_session_name": "tmux_demo",
                "workspace_name": "demo",
                "worktree_path": "/tmp/demo",
                "created_at": "2026-06-14T00:00:00Z",
                "is_running": true,
                "claude_active": true
            }
        ]);
        let needs = json!([
            { "kind": "ASK", "context": { "question": "pick option 2" }, "session": { "cwd": "/tmp/demo" } }
        ]);
        CoreSnapshot { sessions, needs }
    }
}

impl DataSource for FakeSource {
    fn snapshot(&self) -> SnapshotFuture<'_> {
        Box::pin(async {
            Ok::<_, DataError>(FleetSnapshot::from_parts(Self::fixed_core(), Value::Null))
        })
    }

    fn core(&self) -> CoreFuture<'_> {
        Box::pin(async { Ok::<_, DataError>(Self::fixed_core()) })
    }

    fn cost(&self) -> CostFuture<'_> {
        Box::pin(async { Value::Null })
    }
}

fn app(token: Option<&str>) -> axum::Router {
    let config = WebConfig {
        listen: "127.0.0.1:0".parse().unwrap(),
        token: token.map(str::to_string),
        insecure_bind: false,
        read_only: true,
    };
    let state = AppState::new(config, Arc::new(FakeSource));
    router(state)
}

/// A deterministic [`Answerer`] that records the params it received and returns
/// a canned outcome — the seam that lets a route test drive `POST /api/answer`
/// without a live daemon socket.
struct FakeAnswerer {
    last: Arc<Mutex<Option<AnswerParams>>>,
    outcome: AnswerResult,
}

impl FakeAnswerer {
    fn new(outcome: AnswerResult) -> (Arc<Self>, Arc<Mutex<Option<AnswerParams>>>) {
        let last = Arc::new(Mutex::new(None));
        (
            Arc::new(Self {
                last: Arc::clone(&last),
                outcome,
            }),
            last,
        )
    }
}

impl Answerer for FakeAnswerer {
    fn answer(
        &self,
        params: AnswerParams,
    ) -> Pin<Box<dyn Future<Output = Result<AnswerResult, DaemonError>> + Send + '_>> {
        *self.last.lock().unwrap() = Some(params);
        let outcome = self.outcome.clone();
        Box::pin(async move { Ok(outcome) })
    }
}

fn app_with_answerer(
    token: Option<&str>,
    answerer: Arc<dyn Answerer>,
    read_only: bool,
) -> axum::Router {
    let config = WebConfig {
        listen: "127.0.0.1:0".parse().unwrap(),
        token: token.map(str::to_string),
        insecure_bind: false,
        read_only,
    };
    let state = AppState::with_answerer(config, Arc::new(FakeSource), answerer);
    router(state)
}

async fn post(
    app: &axum::Router,
    path: &str,
    bearer: Option<&str>,
    body: Value,
) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/json");
    if let Some(t) = bearer {
        req = req.header(header::AUTHORIZATION, format!("Bearer {t}"));
    }
    let resp = app
        .clone()
        .oneshot(req.body(Body::from(serde_json::to_vec(&body).unwrap())).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20).await.unwrap();
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value)
}

async fn get(app: &axum::Router, path: &str, bearer: Option<&str>) -> (StatusCode, Value) {
    let mut req = Request::builder().method("GET").uri(path);
    if let Some(t) = bearer {
        req = req.header(header::AUTHORIZATION, format!("Bearer {t}"));
    }
    let resp = app.clone().oneshot(req.body(Body::empty()).unwrap()).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20).await.unwrap();
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value)
}

// ── No token configured: loopback dev posture, everything allowed. ────

#[tokio::test]
async fn sessions_ok_without_token_when_none_configured() {
    let app = app(None);
    let (status, body) = get(&app, "/api/sessions", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body[0]["workspace_name"], "demo");
}

#[tokio::test]
async fn snapshot_returns_all_three_surfaces() {
    let app = app(None);
    let (status, body) = get(&app, "/api/snapshot", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["sessions"].is_array());
    assert!(body["needs"].is_array());
    assert!(body["cost"].is_null(), "cost degrades to null when absent");
}

#[tokio::test]
async fn needs_surface_classified() {
    let app = app(None);
    let (status, body) = get(&app, "/api/needs", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body[0]["kind"], "ASK");
}

#[tokio::test]
async fn healthz_reports_posture() {
    let app = app(None);
    let (status, body) = get(&app, "/healthz", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["ok"], true);
    assert_eq!(body["readOnly"], true);
    assert_eq!(body["tokenRequired"], false);
}

// ── Token configured: every /api/* route is gated. ───────────────────

#[tokio::test]
async fn api_requires_bearer_when_token_configured() {
    let app = app(Some("s3cret"));

    // No bearer → 401.
    let (status, body) = get(&app, "/api/sessions", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"]["code"], "UNAUTHORIZED");

    // Wrong bearer → 401.
    let (status, _) = get(&app, "/api/sessions", Some("wrong")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // Correct bearer → 200.
    let (status, body) = get(&app, "/api/sessions", Some("s3cret")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body[0]["workspace_name"], "demo");
}

#[tokio::test]
async fn healthz_token_gated_too() {
    let app = app(Some("s3cret"));
    let (status, _) = get(&app, "/healthz", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, body) = get(&app, "/healthz", Some("s3cret")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["tokenRequired"], true);
}

// ── Query-string token is honored ONLY on the SSE route. ─────────────
//
// Tokens in URLs leak via proxy/access logs, browser history, and `Referer`,
// so the `?token=` fallback exists solely for `/api/events` (an `EventSource`
// cannot set request headers). Every other route requires the bearer header.

#[tokio::test]
async fn query_token_rejected_on_non_sse_route() {
    let app = app(Some("s3cret"));
    // Correct token, but via query string on a JSON route → 401 (header required).
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/snapshot?token=s3cret")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::UNAUTHORIZED,
        "query token must NOT authorize a non-SSE route"
    );

    // Same route still works with the Authorization header.
    let (status, _) = get(&app, "/api/snapshot", Some("s3cret")).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn query_token_accepted_on_sse_route() {
    let app = app(Some("s3cret"));
    // No header, token via query string on the SSE route → 200.
    let resp = app
        .clone()
        .oneshot(Request::builder().uri("/api/events?token=s3cret").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "SSE route must accept the token via query string"
    );

    // Wrong query token on the SSE route → 401.
    let resp = app
        .oneshot(Request::builder().uri("/api/events?token=wrong").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

// ── Static asset (the SPA shell) is served without auth. ─────────────

#[tokio::test]
async fn index_served_without_auth() {
    let app = app(Some("s3cret"));
    let resp = app
        .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20).await.unwrap();
    let html = String::from_utf8_lossy(&bytes);
    assert!(html.contains("ainb"), "index.html should be the SPA shell");
}

// ── POST /api/answer routes an ASK answer through the daemon seam (D18). ──

#[tokio::test]
async fn answer_delivers_through_daemon_seam() {
    let (answerer, last) = FakeAnswerer::new(AnswerResult::Delivered {
        via: "tmux (demo)".to_string(),
    });
    // Control mode (not --read-only): the write surface is live.
    let app = app_with_answerer(None, answerer, false);

    let (status, body) = post(
        &app,
        "/api/answer",
        None,
        json!({ "attentionId": "att-1", "answer": "2" }),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    // The tagged outcome is returned verbatim so the frontend renders feedback.
    assert_eq!(body["outcome"], "delivered");
    assert_eq!(body["via"], "tmux (demo)");

    // The route forwarded the exact target + defaults to the daemon.
    let params = last.lock().unwrap().clone().expect("answerer was called");
    assert_eq!(params.attention_id, "att-1");
    assert_eq!(params.answer, "2");
    assert_eq!(params.answered_by, "web", "answeredBy defaults to web");
    assert!(params.is_answer, "isAnswer defaults to true (C1 safety)");
}

#[tokio::test]
async fn answer_reports_already_answered_outcome() {
    // A second surface answering the same row loses the first-answer-wins race;
    // the daemon's tagged outcome flows straight back to the caller.
    let (answerer, _last) = FakeAnswerer::new(AnswerResult::AlreadyAnswered {
        by: "atc".to_string(),
    });
    let app = app_with_answerer(None, answerer, false);

    let (status, body) = post(
        &app,
        "/api/answer",
        None,
        json!({ "attentionId": "att-1", "answer": "1", "answeredBy": "web" }),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["outcome"], "already_answered");
    assert_eq!(body["by"], "atc");
}

#[tokio::test]
async fn answer_rejects_empty_attention_id() {
    let (answerer, last) = FakeAnswerer::new(AnswerResult::Delivered { via: "x".into() });
    let app = app_with_answerer(None, answerer, false);

    let (status, body) = post(
        &app,
        "/api/answer",
        None,
        json!({ "attentionId": "", "answer": "2" }),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "INVALID_BODY");
    assert!(
        last.lock().unwrap().is_none(),
        "a malformed request must not reach the daemon"
    );
}

#[tokio::test]
async fn answer_requires_bearer_when_token_configured() {
    let (answerer, last) = FakeAnswerer::new(AnswerResult::Delivered { via: "x".into() });
    let app = app_with_answerer(Some("s3cret"), answerer, false);

    // No bearer → 401, and the answer never reaches the daemon seam.
    let (status, body) = post(
        &app,
        "/api/answer",
        None,
        json!({ "attentionId": "att-1", "answer": "2" }),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"]["code"], "UNAUTHORIZED");
    assert!(
        last.lock().unwrap().is_none(),
        "unauth answer must not dispatch"
    );

    // Correct bearer → delivered.
    let (status, body) = post(
        &app,
        "/api/answer",
        Some("s3cret"),
        json!({ "attentionId": "att-1", "answer": "2" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["outcome"], "delivered");
}

#[tokio::test]
async fn answer_refused_in_read_only_mode() {
    // `/api/answer` is a fleet-state write surface (it drives the daemon's
    // verified last-mile send into a live session), so `--read-only` must refuse
    // it exactly like the WS terminal — `403 READ_ONLY`, and the daemon seam is
    // never touched. This is the invariant that keeps the `--insecure-bind +
    // --read-only` bind exemption sound: in read-only mode NO write surface is
    // reachable, so an unauthenticated public bind cannot inject answers.
    let (answerer, last) = FakeAnswerer::new(AnswerResult::Delivered { via: "x".into() });
    let app = app_with_answerer(None, answerer, true);

    let (status, body) = post(
        &app,
        "/api/answer",
        None,
        json!({ "attentionId": "att-1", "answer": "2" }),
    )
    .await;

    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"]["code"], "READ_ONLY");
    assert!(
        last.lock().unwrap().is_none(),
        "read-only mode must refuse the answer before it reaches the daemon"
    );
}
