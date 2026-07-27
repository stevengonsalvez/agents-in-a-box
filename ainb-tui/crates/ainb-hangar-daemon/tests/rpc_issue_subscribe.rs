//! Integration: issue subscribers + reactions over a real framed `UnixStream`
//! against a real sqlite (multica parity #22).
//!
//! Proves the five new RPCs end to end: the mutator's OWN answer carries the
//! refreshed collection (no read-after-write round trip), an omitted `actor`
//! means the LOCAL HUMAN, a target outside the workspace / a malformed token /
//! an unknown issue are all `INVALID_PARAMS` rather than silent no-ops, and a
//! blank emoji hits the reference's "emoji is required" guard.

use std::time::{Duration, Instant};

use ainb_hangar_daemon::rpc::{self, DaemonHealth};
use ainb_hangar_daemon::seed::{self, WS_SLUG};
use ainb_hangar_proto::{RpcId, RpcRequest, methods};
use ainb_hangar_store::Store;
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};

/// Seed one fresh issue into the fixture workspace, bypassing the repo so the
/// subscriber set starts EMPTY (the repo auto-subscribes the creator).
async fn seed_issue(store: &Store, id: &str) {
    sqlx::query(
        "INSERT INTO issue \
         (id, workspace_id, title, description, state, creator_type, creator_id, created_at) \
         VALUES (?, ?, 'Subscribe fixture', 'body', 'open', 'member', 'user-1', 0)",
    )
    .bind(id)
    .bind(ainb_hangar_daemon::seed::WS_ID)
    .execute(store.pool())
    .await
    .expect("seed issue");
}

struct Client {
    reader: BufReader<OwnedReadHalf>,
    writer: OwnedWriteHalf,
}

impl Client {
    async fn connect(socket_path: &std::path::Path) -> Self {
        let deadline = Instant::now() + Duration::from_secs(5);
        let stream = loop {
            match UnixStream::connect(socket_path).await {
                Ok(c) => break c,
                Err(_) if Instant::now() < deadline => {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                Err(e) => panic!("never connected: {e}"),
            }
        };
        let (read_half, writer) = stream.into_split();
        Self {
            reader: BufReader::new(read_half),
            writer,
        }
    }

    async fn send(&mut self, method: &str, params: serde_json::Value) {
        let req = RpcRequest {
            jsonrpc: ainb_hangar_proto::jsonrpc_version(),
            id: RpcId::Number(22),
            method: method.into(),
            params,
        };
        let body = serde_json::to_vec(&req).unwrap();
        let mut out = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
        out.extend_from_slice(&body);
        self.writer.write_all(&out).await.unwrap();
        self.writer.flush().await.unwrap();
    }

    async fn read_frame_inner(&mut self) -> serde_json::Value {
        use tokio::io::AsyncBufReadExt;
        let mut len: Option<usize> = None;
        loop {
            let mut line = String::new();
            let n = self.reader.read_line(&mut line).await.unwrap();
            assert!(n > 0, "connection closed while awaiting a frame");
            let t = line.trim_end_matches("\r\n");
            if t.is_empty() {
                let mut body = vec![0u8; len.expect("Content-Length header")];
                self.reader.read_exact(&mut body).await.unwrap();
                return serde_json::from_slice(&body).unwrap();
            }
            if let Some((name, v)) = t.split_once(':') {
                if name.trim().eq_ignore_ascii_case("Content-Length") {
                    len = v.trim().parse().ok();
                }
            }
        }
    }

    async fn call(&mut self, method: &str, params: serde_json::Value) -> serde_json::Value {
        self.send(method, params).await;
        loop {
            let frame = tokio::time::timeout(Duration::from_secs(5), self.read_frame_inner())
                .await
                .unwrap_or_else(|_| panic!("no response to {method} within 5s"));
            if frame.get("id").is_some() {
                return frame;
            }
        }
    }

    async fn auth_from_file(&mut self, dir: &std::path::Path) {
        let token_path = ainb_hangar_proto::auth::token_file_in(dir);
        let token = std::fs::read_to_string(&token_path).expect("read daemon.token");
        let resp = self
            .call(
                methods::AUTH_HELLO,
                serde_json::json!({ "token": token.trim() }),
            )
            .await;
        assert!(resp["error"].is_null(), "auth/hello must ack: {resp}");
    }
}

async fn start_server(dir: &std::path::Path) -> (std::path::PathBuf, Store) {
    let store = Store::open_in(dir).await.unwrap();
    seed::seed_p4_fixture(store.pool()).await.unwrap();
    rpc::auth::ensure_socket_token(store.pool(), dir)
        .await
        .expect("ensure socket token");
    let socket_path = rpc::socket_path_in(dir);
    let listener = rpc::bind(&socket_path).expect("bind socket");
    let health = DaemonHealth {
        socket_path: socket_path.to_string_lossy().into_owned(),
        pid: std::process::id(),
        started_at: Instant::now(),
        version: "0.1.0".into(),
        stats: std::sync::Arc::new(ainb_hangar_daemon::health_stats::HealthStats::default()),
    };
    tokio::spawn(rpc::serve(
        listener,
        store.pool().clone(),
        health,
        ainb_hangar_daemon::events::EventBroker::new(),
    ));
    (socket_path, store)
}

/// `(actor, reason)` pairs out of a `IssueSubscribersResult` payload.
fn pairs(resp: &serde_json::Value) -> Vec<(String, String)> {
    resp["result"]["subscribers"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|r| {
            (
                r["actor"].as_str().unwrap_or_default().to_string(),
                r["reason"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect()
}

/// The full round trip: subscribe (defaulting to the LOCAL HUMAN), read back,
/// unsubscribe. Every mutator answers the REFRESHED set.
#[tokio::test]
async fn subscribe_then_list_then_unsubscribe_over_the_socket() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, store) = start_server(dir.path()).await;
    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;
    seed_issue(&store, "sub-1").await;

    let resp = c
        .call(
            methods::HANGAR_ISSUE_SUBSCRIBE,
            serde_json::json!({ "workspace_id": WS_SLUG, "issue_id": "sub-1" }),
        )
        .await;
    assert!(resp["error"].is_null(), "subscribe must ack: {resp}");
    assert_eq!(
        pairs(&resp),
        vec![("member:me".to_string(), "manual".to_string())],
        "an omitted actor defaults to the LOCAL HUMAN, reason=manual"
    );

    // At rest — the reply is not evidence of a write.
    let at_rest: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM issue_subscriber WHERE issue_id = 'sub-1'")
            .fetch_one(store.pool())
            .await
            .unwrap();
    assert_eq!(at_rest, 1);

    let resp = c
        .call(
            methods::HANGAR_ISSUE_SUBSCRIBERS,
            serde_json::json!({ "workspace_id": WS_SLUG, "issue_id": "sub-1" }),
        )
        .await;
    assert_eq!(pairs(&resp).len(), 1, "the read agrees: {resp}");

    let resp = c
        .call(
            methods::HANGAR_ISSUE_UNSUBSCRIBE,
            serde_json::json!({ "workspace_id": WS_SLUG, "issue_id": "sub-1" }),
        )
        .await;
    assert!(resp["error"].is_null(), "unsubscribe must ack: {resp}");
    assert!(pairs(&resp).is_empty(), "the set empties: {resp}");
}

/// Re-subscribing an existing `creator` is a no-op that STILL answers
/// "subscribed", and the original provenance survives (first reason wins).
#[tokio::test]
async fn re_subscribing_keeps_the_original_reason() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, store) = start_server(dir.path()).await;
    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;
    seed_issue(&store, "sub-2").await;
    ainb_hangar_store::repo::issue_subscriber::IssueSubscriberRepo::add(
        store.pool(),
        ainb_hangar_daemon::seed::WS_ID,
        "sub-2",
        &ainb_hangar_core::actor::local_member(),
        ainb_hangar_store::repo::issue_subscriber::SubscribeReason::Creator,
        0,
    )
    .await
    .unwrap();

    let resp = c
        .call(
            methods::HANGAR_ISSUE_SUBSCRIBE,
            serde_json::json!({ "workspace_id": WS_SLUG, "issue_id": "sub-2" }),
        )
        .await;
    assert!(
        resp["error"].is_null(),
        "a redundant subscribe still acks: {resp}"
    );
    assert_eq!(
        pairs(&resp),
        vec![("member:me".to_string(), "creator".to_string())],
        "first reason wins: {resp}"
    );
}

#[tokio::test]
async fn subscribe_rejects_an_unknown_issue_and_a_malformed_actor() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, store) = start_server(dir.path()).await;
    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;
    seed_issue(&store, "sub-3").await;

    let resp = c
        .call(
            methods::HANGAR_ISSUE_SUBSCRIBE,
            serde_json::json!({ "workspace_id": WS_SLUG, "issue_id": "no-such-issue" }),
        )
        .await;
    assert!(
        !resp["error"].is_null(),
        "an unknown issue must fail: {resp}"
    );

    let resp = c
        .call(
            methods::HANGAR_ISSUE_SUBSCRIBE,
            serde_json::json!({
                "workspace_id": WS_SLUG,
                "issue_id": "sub-3",
                "actor": "nonsense",
            }),
        )
        .await;
    assert!(
        !resp["error"].is_null(),
        "a malformed actor must fail: {resp}"
    );
}

/// The reference's `403`: a target that is not in this workspace is rejected.
#[tokio::test]
async fn subscribe_rejects_an_actor_outside_the_workspace() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, store) = start_server(dir.path()).await;
    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;
    seed_issue(&store, "sub-4").await;

    let resp = c
        .call(
            methods::HANGAR_ISSUE_SUBSCRIBE,
            serde_json::json!({
                "workspace_id": WS_SLUG,
                "issue_id": "sub-4",
                "actor": "member:stranger",
            }),
        )
        .await;
    assert!(
        !resp["error"].is_null(),
        "an out-of-workspace target must be refused: {resp}"
    );
    let at_rest: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM issue_subscriber WHERE issue_id = 'sub-4'")
            .fetch_one(store.pool())
            .await
            .unwrap();
    assert_eq!(at_rest, 0, "and nothing was written");
}

/// Reactions: add / tally / `mine` for the local human / remove / blank guard.
#[tokio::test]
async fn reaction_add_remove_round_trip_and_the_emoji_guard() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, store) = start_server(dir.path()).await;
    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;
    seed_issue(&store, "sub-5").await;

    let resp = c
        .call(
            methods::HANGAR_ISSUE_REACTION_ADD,
            serde_json::json!({ "workspace_id": WS_SLUG, "issue_id": "sub-5", "emoji": "👍" }),
        )
        .await;
    assert!(resp["error"].is_null(), "react add must ack: {resp}");
    let buckets = resp["result"]["reactions"].as_array().cloned().unwrap_or_default();
    assert_eq!(buckets.len(), 1, "one bucket: {resp}");
    assert_eq!(buckets[0]["emoji"], "👍");
    assert_eq!(buckets[0]["count"], 1);
    assert_eq!(buckets[0]["mine"], true, "the local human is the reactor");

    let resp = c
        .call(
            methods::HANGAR_ISSUE_REACTION_REMOVE,
            serde_json::json!({ "workspace_id": WS_SLUG, "issue_id": "sub-5", "emoji": "👍" }),
        )
        .await;
    assert!(resp["error"].is_null(), "react remove must ack: {resp}");
    assert!(
        resp["result"]["reactions"].as_array().is_none_or(std::vec::Vec::is_empty),
        "the set empties: {resp}"
    );

    let resp = c
        .call(
            methods::HANGAR_ISSUE_REACTION_ADD,
            serde_json::json!({ "workspace_id": WS_SLUG, "issue_id": "sub-5", "emoji": "  " }),
        )
        .await;
    assert!(!resp["error"].is_null(), "a blank emoji must fail: {resp}");
    assert!(
        resp["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("emoji is required"),
        "the reference's own guard text: {resp}"
    );
}
