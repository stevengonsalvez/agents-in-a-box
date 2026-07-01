//! Integration tests for the depth surfaces added on top of the read-only
//! dashboard: the WebSocket terminal (auth + read-only gating), the web-push
//! endpoints, and the PWA assets (manifest + service worker).
//!
//! These drive the real axum `Router` via `tower::ServiceExt::oneshot`, so they
//! exercise routing, the bearer middleware, and the read-only gate without
//! binding a socket or spawning tmux. The WS *upgrade* response (101 vs the
//! 401/403 refusals) is observable before any tmux work happens, which is
//! exactly the security boundary we care about.

use std::sync::Arc;

use ainb_web::data::{
    CoreFuture, CoreSnapshot, CostFuture, DataError, DataSource, FleetSnapshot, SnapshotFuture,
};
use ainb_web::{AppState, WebConfig, router};
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use serde_json::{Value, json};
use tower::ServiceExt;

/// Deterministic data source with one running session, so the WS terminal can
/// resolve a session id to a tmux name without a subprocess.
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
        let needs = json!([]);
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

fn app(token: Option<&str>, read_only: bool) -> axum::Router {
    let config = WebConfig {
        listen: "127.0.0.1:0".parse().unwrap(),
        token: token.map(str::to_string),
        insecure_bind: false,
        read_only,
    };
    // No push backend in tests — exercises the PUSH_NOT_CONFIGURED path.
    let state = AppState::new(config, Arc::new(FakeSource));
    router(state)
}

/// Build a WS-upgrade request for `/ws/session/{id}`, optionally with a query
/// token and the upgrade headers axum's `WebSocketUpgrade` requires.
fn ws_request(id: &str, query_token: Option<&str>) -> Request<Body> {
    let uri = match query_token {
        Some(t) => format!("/ws/session/{id}?token={t}"),
        None => format!("/ws/session/{id}"),
    };
    Request::builder()
        .uri(uri)
        .header(header::CONNECTION, "upgrade")
        .header(header::UPGRADE, "websocket")
        .header(header::SEC_WEBSOCKET_VERSION, "13")
        .header(header::SEC_WEBSOCKET_KEY, "dGhlIHNhbXBsZSBub25jZQ==")
        .body(Body::empty())
        .unwrap()
}

// ── WS terminal: read-only gate ──────────────────────────────────────────────

#[tokio::test]
async fn ws_terminal_refused_in_read_only_mode() {
    // Read-only, no token: the auth middleware passes, but the handler must
    // refuse the write surface with 403 before touching tmux.
    let app = app(None, true);
    let resp = app.oneshot(ws_request("abc", None)).await.unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "WS terminal must be refused with 403 in --read-only mode"
    );
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20).await.unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["error"]["code"], "READ_ONLY");
}

// ── WS terminal: auth gate ───────────────────────────────────────────────────

#[tokio::test]
async fn ws_terminal_refused_without_token() {
    // Token configured, not read-only: the upgrade must be refused (401) when
    // no token is presented — the write surface is never reachable unauthed.
    let app = app(Some("s3cret"), false);
    let resp = app.oneshot(ws_request("abc", None)).await.unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::UNAUTHORIZED,
        "WS terminal must require auth when a token is configured"
    );
}

#[tokio::test]
async fn ws_terminal_refused_with_wrong_query_token() {
    let app = app(Some("s3cret"), false);
    let resp = app.oneshot(ws_request("abc", Some("wrong"))).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn ws_terminal_upgrades_with_valid_query_token() {
    // A `oneshot` request never reaches hyper's connection layer, so it can't
    // produce a real 101. Bind a real server and drive an actual WebSocket
    // handshake: a valid query token on the WS route must complete the upgrade
    // (the query-token fallback is honored here exactly like on the SSE route,
    // because a WebSocket upgrade cannot carry an Authorization header).
    use futures_util::StreamExt;
    use tokio_tungstenite::tungstenite::Message as TMessage;

    let app = app(Some("s3cret"), false);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    // Valid token + an id that resolves to NO session → the upgrade still
    // completes (101); the server then emits a SESSION_NOT_FOUND control frame
    // and closes. Using an unknown id keeps this deterministic and tmux-free:
    // the security boundary under test is that a valid token reaches the
    // handler at all. A read timeout guards against any hang.
    let url = format!("ws://{addr}/ws/session/does-not-exist?token=s3cret");
    let (mut ws, response) = tokio_tungstenite::connect_async(&url)
        .await
        .expect("valid query token must allow the WS terminal upgrade");
    assert_eq!(
        response.status().as_u16(),
        101,
        "WS upgrade must return 101"
    );

    let mut saw_error = false;
    let read = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while let Some(Ok(msg)) = ws.next().await {
            if let TMessage::Text(txt) = msg {
                if txt.contains("\"error\"") && txt.contains("SESSION_NOT_FOUND") {
                    saw_error = true;
                }
            }
        }
    })
    .await;
    assert!(read.is_ok(), "server must close the stream, not hang");
    assert!(
        saw_error,
        "server should report the unknown session and close"
    );

    // Wrong token → handshake is rejected (the bearer middleware 401s before
    // the upgrade), so the WS client connect fails.
    let bad = format!("ws://{addr}/ws/session/abc?token=wrong");
    assert!(
        tokio_tungstenite::connect_async(&bad).await.is_err(),
        "an invalid token must be refused before the upgrade"
    );

    server.abort();
}

#[tokio::test]
async fn ws_terminal_query_token_is_scoped_off_json_routes() {
    // The widened query-token allowance must NOT leak onto JSON routes: a
    // correct token via query string on /api/snapshot is still refused.
    let app = app(Some("s3cret"), false);
    let resp = app
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
        "query token must remain refused on JSON routes after adding the WS scope"
    );
}

// ── Web-push endpoints ───────────────────────────────────────────────────────

#[tokio::test]
async fn push_config_503_when_unconfigured() {
    let app = app(None, false);
    let resp = app
        .oneshot(Request::builder().uri("/api/push/config").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20).await.unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["error"]["code"], "PUSH_NOT_CONFIGURED");
}

#[tokio::test]
async fn push_subscribe_503_when_unconfigured() {
    let app = app(None, false);
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/push/subscribe")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    r#"{"endpoint":"https://x","keys":{"p256dh":"p","auth":"a"}}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn push_routes_are_auth_gated() {
    // With a token configured, the push config route requires the bearer header
    // just like every other /api/* route.
    let app = app(Some("s3cret"), false);
    let resp = app
        .clone()
        .oneshot(Request::builder().uri("/api/push/config").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    // With the header it gets past auth (then 503, since push is unconfigured).
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/push/config")
                .header(header::AUTHORIZATION, "Bearer s3cret")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
}

// ── PWA assets ───────────────────────────────────────────────────────────────

#[tokio::test]
async fn manifest_served_without_auth() {
    let app = app(Some("s3cret"), false);
    let resp = app
        .oneshot(Request::builder().uri("/manifest.webmanifest").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let ct = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        ct.contains("manifest+json"),
        "manifest content-type, got {ct}"
    );
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20).await.unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["scope"], "/");
    assert_eq!(body["display"], "standalone");
}

#[tokio::test]
async fn service_worker_served_without_auth_with_root_scope() {
    let app = app(Some("s3cret"), false);
    let resp = app
        .oneshot(Request::builder().uri("/sw.js").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    // The SW must be allowed to control the whole origin.
    assert_eq!(
        resp.headers().get("service-worker-allowed").and_then(|v| v.to_str().ok()),
        Some("/")
    );
    let ct = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(ct.contains("javascript"), "sw content-type, got {ct}");
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20).await.unwrap();
    let js = String::from_utf8_lossy(&bytes);
    assert!(
        js.contains("showNotification"),
        "sw.js handles push notifications"
    );
}
