//! Integration: the `hangar/issue_delete` RPC deletes an issue over a real
//! framed `UnixStream`, pushes the matching `hangar/event` (`issue_deleted`) to
//! subscribed connections, refuses while a task is ACTIVE, and is
//! workspace-scoped (63d).
//!
//! The seed fixture lays down three open issues in `WS_ID`; here a connection
//! authenticates, subscribes the workspace, and asserts (a) a delete acks + pushes
//! `issue_deleted` + drops the row from a follow-up `issues_list`, (b) an issue
//! with a live task is refused with a "cancel the run first" error, and (c) a
//! mistyped workspace is rejected, never a silent delete.

use std::time::{Duration, Instant};

use ainb_hangar_daemon::rpc::{self, DaemonHealth};
use ainb_hangar_daemon::seed::{self, WS_ID, WS_SLUG};
use ainb_hangar_proto::events::EVENT_METHOD;
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

    async fn next_event(&mut self, timeout: Duration) -> Option<serde_json::Value> {
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.checked_duration_since(Instant::now())?;
            let frame = self.read_frame(remaining).await?;
            if frame.get("id").is_none() && frame["method"] == EVENT_METHOD {
                return Some(frame["params"].clone());
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

    async fn subscribe(&mut self, workspace_id: &str) {
        let resp = self
            .call(
                methods::WORKSPACE_SUBSCRIBE,
                serde_json::json!({ "workspace_id": workspace_id }),
            )
            .await;
        assert!(resp["error"].is_null(), "subscribe must ack: {resp}");
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

/// An authenticated, subscribed connection deletes a seeded issue: the response
/// acks, the connection receives the `issue_deleted` push, and the issue is gone
/// from a fresh snapshot.
#[tokio::test]
async fn issue_delete_removes_and_pushes_event() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, _store) = start_server(dir.path()).await;

    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;
    c.subscribe(WS_SLUG).await;

    // issue-3 has no task in the fixture (issue-1 carries the running task), so it
    // is the clean happy-path delete.
    let resp = c
        .call(
            methods::HANGAR_ISSUE_DELETE,
            serde_json::json!({ "workspace_id": WS_SLUG, "issue_id": "issue-3" }),
        )
        .await;
    assert!(resp["error"].is_null(), "delete must ack: {resp}");

    // The subscribed connection received the issue_deleted push. Drain it BEFORE
    // the next request — `call()` discards interleaved notifications.
    let event = c
        .next_event(Duration::from_secs(5))
        .await
        .expect("a committed delete must push an issue_deleted event");
    assert_eq!(event["event"], "issue_deleted", "wrong event: {event}");
    assert_eq!(event["issue_id"], "issue-3");

    // The delete persisted: a fresh issues_list no longer carries issue-3, while
    // its siblings remain.
    let list = c
        .call(
            methods::HANGAR_ISSUES_LIST,
            serde_json::json!({ "workspace_id": WS_SLUG }),
        )
        .await;
    let issues = list["result"]["issues"].as_array().unwrap();
    assert!(
        issues.iter().all(|i| i["id"] != "issue-3"),
        "issue-3 gone from the snapshot"
    );
    assert!(
        issues.iter().any(|i| i["id"] == "issue-2"),
        "sibling issue-2 untouched"
    );
}

/// An issue with a live (running) task refuses the delete with a "cancel the run
/// first" error, and the issue survives.
#[tokio::test]
async fn issue_delete_refuses_while_a_task_is_active() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, store) = start_server(dir.path()).await;

    // Enqueue a task on issue-2 and force it RUNNING (an active status).
    let (runtime_id, agent_id): (String, String) =
        sqlx::query_as("SELECT a.runtime_id, a.id FROM agent a WHERE a.workspace_id = ? LIMIT 1")
            .bind(WS_ID)
            .fetch_one(store.pool())
            .await
            .unwrap();
    sqlx::query(
        "INSERT INTO agent_task_queue \
         (id, workspace_id, runtime_id, agent_id, issue_id, status, created_at) \
         VALUES ('t-live', ?, ?, ?, 'issue-2', 'running', 0)",
    )
    .bind(WS_ID)
    .bind(&runtime_id)
    .bind(&agent_id)
    .execute(store.pool())
    .await
    .unwrap();

    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;

    let resp = c
        .call(
            methods::HANGAR_ISSUE_DELETE,
            serde_json::json!({ "workspace_id": WS_SLUG, "issue_id": "issue-2" }),
        )
        .await;
    assert!(
        !resp["error"].is_null(),
        "an active-task issue must refuse deletion: {resp}"
    );
    assert!(
        resp["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("cancel the run first"),
        "refusal must tell the caller to cancel first: {resp}"
    );

    // issue-2 survives.
    let list = c
        .call(
            methods::HANGAR_ISSUES_LIST,
            serde_json::json!({ "workspace_id": WS_ID }),
        )
        .await;
    let issues = list["result"]["issues"].as_array().unwrap();
    assert!(
        issues.iter().any(|i| i["id"] == "issue-2"),
        "refused delete left issue-2 in place"
    );
}

/// A mistyped / foreign workspace is rejected with an error — never a silent
/// delete — and the issue is untouched.
#[tokio::test]
async fn issue_delete_rejects_unknown_workspace_and_touches_no_row() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, _store) = start_server(dir.path()).await;

    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;

    let resp = c
        .call(
            methods::HANGAR_ISSUE_DELETE,
            serde_json::json!({
                "workspace_id": "nope-not-a-workspace",
                "issue_id": "issue-1",
            }),
        )
        .await;
    assert!(
        !resp["error"].is_null(),
        "an unknown workspace must be rejected, not a silent delete: {resp}"
    );

    // issue-1 is untouched under its real workspace.
    let list = c
        .call(
            methods::HANGAR_ISSUES_LIST,
            serde_json::json!({ "workspace_id": WS_ID }),
        )
        .await;
    let issues = list["result"]["issues"].as_array().unwrap();
    assert!(
        issues.iter().any(|i| i["id"] == "issue-1"),
        "rejected delete left issue-1 in place"
    );
}
