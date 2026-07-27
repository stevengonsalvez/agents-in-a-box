//! Integration: `hangar/issue_timeline` (multica parity #13) over a real framed
//! `UnixStream`.
//!
//! The ACCEPTANCE proof for the parity item: an issue is created, moved,
//! re-assigned and commented on through the real RPC dispatcher, and the
//! timeline read back must carry the merged activity + comment narrative in
//! ascending `created_at` order, with the exact multica details shapes.
//!
//! Also pins the tenant contract: an `issue_id` that does not resolve inside the
//! named workspace is `INVALID_PARAMS`, never a silent empty list (an empty
//! timeline and a cross-tenant probe must not look identical).

use std::time::{Duration, Instant};

use ainb_hangar_daemon::rpc::{self, DaemonHealth};
use ainb_hangar_daemon::seed::{self, WS_SLUG};
use ainb_hangar_proto::{RpcId, RpcRequest, methods};
use ainb_hangar_store::Store;
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};

/// One test client connection: a persistent buffered reader + writer half.
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
            id: RpcId::Number(7),
            method: method.into(),
            params,
        };
        let body = serde_json::to_vec(&req).unwrap();
        let mut out = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
        out.extend_from_slice(&body);
        self.writer.write_all(&out).await.unwrap();
        self.writer.flush().await.unwrap();
    }

    async fn read_frame(&mut self, timeout: Duration) -> Option<serde_json::Value> {
        tokio::time::timeout(timeout, self.read_frame_inner()).await.ok()
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
            let frame = self
                .read_frame(Duration::from_secs(5))
                .await
                .unwrap_or_else(|| panic!("no response to {method} within 5s"));
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

/// Bind + serve the real listener over the seeded store (mirrors `boot()`).
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

/// create → move → assign → comment → timeline: the merged narrative comes back
/// oldest-first with multica's exact `status_changed` details shape.
#[tokio::test]
async fn timeline_merges_activity_and_comments_oldest_first() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, _store) = start_server(dir.path()).await;

    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;

    let created = c
        .call(
            methods::HANGAR_ISSUE_CREATE,
            serde_json::json!({
                "workspace_id": WS_SLUG,
                "title": "parity 13 proof",
                "creator": "member:user-1",
            }),
        )
        .await;
    assert!(
        created["error"].is_null(),
        "issue_create must ack: {created}"
    );
    let issue_id = created["result"]["id"].as_str().expect("created issue id").to_string();

    let moved = c
        .call(
            methods::HANGAR_ISSUE_UPDATE,
            serde_json::json!({
                "workspace_id": WS_SLUG,
                "issue_id": issue_id,
                "state": "in_progress",
            }),
        )
        .await;
    assert!(moved["error"].is_null(), "issue_update state: {moved}");

    let assigned = c
        .call(
            methods::HANGAR_ISSUE_UPDATE,
            serde_json::json!({
                "workspace_id": WS_SLUG,
                "issue_id": issue_id,
                "assignee": "agent:agent-1",
            }),
        )
        .await;
    assert!(
        assigned["error"].is_null(),
        "issue_update assignee: {assigned}"
    );

    let commented = c
        .call(
            methods::HANGAR_COMMENT_ADD,
            serde_json::json!({
                "workspace_id": WS_SLUG,
                "issue_id": issue_id,
                "author": "agent:agent-1",
                "body": "picked this up, starting now",
            }),
        )
        .await;
    assert!(commented["error"].is_null(), "comment_add: {commented}");

    let resp = c
        .call(
            methods::HANGAR_ISSUE_TIMELINE,
            serde_json::json!({ "workspace_id": WS_SLUG, "issue_id": issue_id }),
        )
        .await;
    assert!(resp["error"].is_null(), "issue_timeline must ack: {resp}");
    let entries = resp["result"]["entries"].as_array().expect("entries array").clone();

    // Ascending by created_at — the flat multica contract.
    let times: Vec<i64> =
        entries.iter().map(|e| e["created_at"].as_i64().expect("created_at")).collect();
    let mut sorted = times.clone();
    sorted.sort_unstable();
    assert_eq!(times, sorted, "entries must be oldest-first: {entries:#?}");

    // The three activity rows, in order.
    let actions: Vec<&str> = entries
        .iter()
        .filter(|e| e["kind"] == "activity")
        .map(|e| e["action"].as_str().expect("action"))
        .collect();
    assert_eq!(
        actions,
        ["created", "status_changed", "assignee_changed"],
        "activity narrative: {entries:#?}"
    );

    // …with multica's exact details shape on the move.
    let status = entries
        .iter()
        .find(|e| e["action"] == "status_changed")
        .expect("a status_changed entry");
    assert_eq!(
        status["details"],
        serde_json::json!({"from": "open", "to": "in_progress"})
    );

    // The comment is MERGED from the comment table, not duplicated as activity.
    let comments: Vec<&serde_json::Value> =
        entries.iter().filter(|e| e["kind"] == "comment").collect();
    assert_eq!(comments.len(), 1, "one merged comment: {entries:#?}");
    assert_eq!(comments[0]["body"], "picked this up, starting now");
    assert_eq!(comments[0]["actor_type"], "agent");
    assert_eq!(comments[0]["actor_id"], "agent-1");
    assert!(
        comments[0]["action"].is_null(),
        "a comment entry carries no activity-only keys: {}",
        comments[0]
    );
}

/// An issue id that does not resolve inside the named workspace is rejected —
/// never an empty list, which would make a cross-tenant probe indistinguishable
/// from a card with no history yet.
#[tokio::test]
async fn timeline_rejects_an_issue_outside_the_workspace() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, _store) = start_server(dir.path()).await;

    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;

    let resp = c
        .call(
            methods::HANGAR_ISSUE_TIMELINE,
            serde_json::json!({
                "workspace_id": WS_SLUG,
                "issue_id": "definitely-not-an-issue-in-this-tenant",
            }),
        )
        .await;
    assert!(
        !resp["error"].is_null(),
        "an unresolvable issue must be rejected, not an empty timeline: {resp}"
    );
}
