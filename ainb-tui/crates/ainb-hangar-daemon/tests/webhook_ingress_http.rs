//! HTTP-level integration + security proof for the webhook ingress (e38.18).
//!
//! This is the user-visible / security acceptance leg. It boots the REAL HTTP
//! ingress on an ephemeral `127.0.0.1` port and drives it over a raw TCP socket
//! (no HTTP client dep), asserting the full security contract:
//!
//! - a correctly-HMAC-signed `POST /hangar/webhook/<id>` returns 200 AND fires
//!   the autopilot (an `autopilot_run` row + an `agent_task_queue` task appear);
//! - an UNSIGNED request returns 401 and fires NOTHING (negative);
//! - a WRONG-signature request returns 401 and fires NOTHING (negative);
//! - an UNKNOWN autopilot id returns 404 and fires NOTHING (negative);
//! - the event filter accepts a matching event and filters a non-matching one
//!   (200, fires nothing);
//! - every processed request lands in the delivery audit log.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::time::Duration;

use ainb_hangar_core::clock::SystemClock;
use ainb_hangar_core::ids::AutopilotId;
use ainb_hangar_core::webhook::compute_signature;
use ainb_hangar_daemon::webhook_ingress::{bind, serve};
use ainb_hangar_store::Store;
use ainb_hangar_store::repo::autopilot_webhook::{
    AutopilotWebhookRepo, WebhookConfig, WebhookSecretStore,
};
use sqlx::SqlitePool;

/// Seed a workspace + user + runtime + agent + a webhook-enabled autopilot whose
/// HMAC secret is `secret` (with the matching digest persisted), plus the 0600
/// secret file under `home`. Returns the autopilot id.
async fn seed(pool: &SqlitePool, home: &std::path::Path, secret: &str, event_filter: Option<&str>) {
    sqlx::query(
        "INSERT INTO workspace (id, slug, name, created_at) VALUES ('ws-1','default','D',0)",
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO user (id, email, created_at) VALUES ('u-1','a@b.c',0)")
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO member (workspace_id, user_id, role) VALUES ('ws-1','u-1','owner')")
        .execute(pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO agent_runtime (id, workspace_id, daemon_id, provider, runtime_mode, status) \
         VALUES ('rt-1','ws-1','d','claude','local','online')",
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO agent (id, workspace_id, name, runtime_id, visibility, owner_id) \
         VALUES ('ag-1','ws-1','A','rt-1','workspace','u-1')",
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO autopilot \
         (id, workspace_id, agent_id, name, cron_expr, max_concurrent_runs, enabled, created_at) \
         VALUES ('ap-1','ws-1','ag-1','hook','0 0 * * *',1,1,0)",
    )
    .execute(pool)
    .await
    .unwrap();

    // Persist the webhook config: enabled + the secret's digest + filter.
    let digest = ainb_hangar_core::token::sha256_hex(secret);
    let ws = ainb_hangar_core::ids::WorkspaceId::from_str("ws-1").unwrap();
    let ap = AutopilotId::from_str("ap-1").unwrap();
    AutopilotWebhookRepo::set_config(
        pool,
        &ws,
        &ap,
        &WebhookConfig {
            enabled: true,
            secret_sha256: Some(digest),
            event_filter: event_filter.map(str::to_string),
        },
    )
    .await
    .unwrap();
    // Write the recoverable secret to the 0600 file the ingress reads.
    WebhookSecretStore::in_home(home).set("ap-1", secret).unwrap();
}

/// Send a raw HTTP/1.1 POST and return `(status_code, body)`.
fn post(port: u16, path: &str, headers: &[(&str, &str)], body: &[u8]) -> (u16, String) {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect");
    stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut req = format!(
        "POST {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str("\r\n");
    let mut bytes = req.into_bytes();
    bytes.extend_from_slice(body);
    stream.write_all(&bytes).expect("write request");

    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).expect("read response");
    let text = String::from_utf8_lossy(&raw).to_string();
    let status = text.split_whitespace().nth(1).and_then(|s| s.parse::<u16>().ok()).unwrap_or(0);
    (status, text)
}

async fn count(pool: &SqlitePool, table: &str) -> i64 {
    sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table}"))
        .fetch_one(pool)
        .await
        .unwrap()
}

/// Start the ingress on an ephemeral port, returning the port. The store pool +
/// secret home back it.
async fn start_ingress(pool: SqlitePool, home: std::path::PathBuf) -> u16 {
    let listener = bind(0).await.expect("bind 127.0.0.1:0");
    let port = listener.local_addr().unwrap().port();
    let secrets = Arc::new(WebhookSecretStore::in_home(&home));
    let clock: Arc<dyn ainb_hangar_core::clock::HangarClock + Send + Sync> = Arc::new(SystemClock);
    tokio::spawn(serve(listener, pool, secrets, clock));
    // Give the listener a beat to start accepting.
    tokio::time::sleep(Duration::from_millis(50)).await;
    port
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn signed_request_fires_unsigned_and_wrong_sig_rejected_unknown_404() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(dir.path()).await.unwrap();
    const SECRET: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    seed(store.pool(), dir.path(), SECRET, None).await;

    let port = start_ingress(store.pool().clone(), dir.path().to_path_buf()).await;

    let body = br#"{"event":"push"}"#;
    let good_sig = compute_signature(SECRET.as_bytes(), body);

    // 1. A correctly-signed request → 200 AND fires.
    let (status, _) = post(
        port,
        "/hangar/webhook/ap-1",
        &[("X-Hangar-Signature", &good_sig)],
        body,
    );
    assert_eq!(status, 200, "a signed request must be accepted");
    assert_eq!(
        count(store.pool(), "autopilot_run").await,
        1,
        "signed request fired exactly one run"
    );
    assert_eq!(
        count(store.pool(), "agent_task_queue").await,
        1,
        "signed request enqueued one task"
    );

    // 2. An UNSIGNED request → 401, fires NOTHING.
    let (status, _) = post(port, "/hangar/webhook/ap-1", &[], body);
    assert_eq!(status, 401, "an unsigned request must be rejected");
    assert_eq!(
        count(store.pool(), "autopilot_run").await,
        1,
        "unsigned request fired nothing"
    );

    // 3. A WRONG-signature request → 401, fires NOTHING.
    let wrong = compute_signature(b"WRONG-SECRET", body);
    let (status, _) = post(
        port,
        "/hangar/webhook/ap-1",
        &[("X-Hangar-Signature", &wrong)],
        body,
    );
    assert_eq!(status, 401, "a wrong-signature request must be rejected");
    assert_eq!(
        count(store.pool(), "autopilot_run").await,
        1,
        "wrong-sig request fired nothing"
    );

    // 4. A signature over a DIFFERENT body (replay) → 401, fires NOTHING.
    let (status, _) = post(
        port,
        "/hangar/webhook/ap-1",
        &[("X-Hangar-Signature", &good_sig)],
        br#"{"event":"deploy"}"#,
    );
    assert_eq!(status, 401, "a body-mismatched signature must be rejected");
    assert_eq!(
        count(store.pool(), "autopilot_run").await,
        1,
        "replay fired nothing"
    );

    // 5. An UNKNOWN autopilot id → 404, fires NOTHING.
    let (status, _) = post(
        port,
        "/hangar/webhook/01JUNKNOWNUNKNOWNUNKNOWN99",
        &[("X-Hangar-Signature", &good_sig)],
        body,
    );
    assert_eq!(status, 404, "an unknown autopilot id must 404");
    assert_eq!(
        count(store.pool(), "autopilot_run").await,
        1,
        "unknown id fired nothing"
    );

    // The delivery log recorded every processed request against ap-1 (the 404
    // for an unknown id has no parent row, so it is not logged): 1 fired + 1
    // missing-sig + 1 bad-sig + 1 replay = 4 deliveries.
    assert_eq!(
        count(store.pool(), "autopilot_webhook_delivery").await,
        4,
        "every ap-1 request is audited (fired + three rejections)"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn event_filter_fires_on_match_and_filters_on_mismatch() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(dir.path()).await.unwrap();
    const SECRET: &str = "abcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcd";
    // Only `deploy` events fire.
    seed(store.pool(), dir.path(), SECRET, Some("deploy")).await;
    let port = start_ingress(store.pool().clone(), dir.path().to_path_buf()).await;

    // A `push` event (signed) → 200 but FILTERED (nothing fires).
    let push = br#"{"event":"push"}"#;
    let push_sig = compute_signature(SECRET.as_bytes(), push);
    let (status, _) = post(
        port,
        "/hangar/webhook/ap-1",
        &[("X-Hangar-Signature", &push_sig)],
        push,
    );
    assert_eq!(status, 200, "a filtered request is still accepted (200)");
    assert_eq!(
        count(store.pool(), "autopilot_run").await,
        0,
        "a non-matching event fires nothing"
    );

    // A `deploy` event (signed) → 200 AND fires.
    let deploy = br#"{"event":"deploy"}"#;
    let deploy_sig = compute_signature(SECRET.as_bytes(), deploy);
    let (status, _) = post(
        port,
        "/hangar/webhook/ap-1",
        &[("X-Hangar-Signature", &deploy_sig)],
        deploy,
    );
    assert_eq!(status, 200);
    assert_eq!(
        count(store.pool(), "autopilot_run").await,
        1,
        "the matching event fires once"
    );
}
