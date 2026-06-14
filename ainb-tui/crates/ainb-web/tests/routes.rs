//! Route + auth integration tests for the dashboard router.
//!
//! These drive the real axum `Router` (with the bearer-auth middleware and a
//! fake data source) via `tower::ServiceExt::oneshot`, so they exercise the
//! full request path — routing, auth, JSON serialization — without binding a
//! socket or spawning the `ainb` binary.

use std::sync::Arc;

use ainb_web::data::{DataError, DataSource, FleetSnapshot, SnapshotFuture};
use ainb_web::{AppState, WebConfig, router};
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use serde_json::{Value, json};
use tower::ServiceExt;

/// Deterministic data source returning a fixed snapshot — no subprocess.
struct FakeSource;

impl DataSource for FakeSource {
    fn snapshot(&self) -> SnapshotFuture<'_> {
        Box::pin(async {
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
            let cost = Value::Null;
            let fingerprint = FleetSnapshot::compute_fingerprint(&sessions, &needs, &cost);
            Ok::<_, DataError>(FleetSnapshot {
                sessions,
                needs,
                cost,
                fingerprint,
            })
        })
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
