//! Integration: the `hangar/comment_add` RPC appends a comment to an issue over
//! a real framed `UnixStream`, persists it, is workspace-scoped, and pushes the
//! matching `hangar/event` (`comment_added`) to subscribed connections (e38.5).
//!
//! The seed fixture lays down three open issues in `WS_ID`; here a connection
//! authenticates, subscribes the workspace, posts a comment through the
//! dispatcher, then asserts (a) the response acked with the persisted row, (b)
//! the subscribed connection received a `comment_added` event carrying the typed
//! body, and (c) the same comment aimed at a *foreign* workspace is rejected with
//! an error and persists nothing (a foreign workspace can't comment on another
//! tenant's issue).
//!
//! This is the framed-socket daemon proof leg for the bead: the `comment_add`
//! RPC persists + is workspace-scoped + pushes `CommentAdded`.

use std::time::{Duration, Instant};

use ainb_hangar_daemon::rpc::{self, DaemonHealth};
use ainb_hangar_daemon::seed::{self, WS_SLUG};
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
    /// Event pushes seen while draining frames for an RPC RESPONSE.
    ///
    /// The daemon emits `comment_added` BEFORE it writes the `comment_add`
    /// reply, so whether the push or the reply reaches this socket first is a
    /// race on how much work the handler does after the emit. Buffering the
    /// pushes `call` walks past makes `next_event` deterministic either way —
    /// without it, a handler that grows slower silently starts eating the very
    /// event the test is asserting on.
    pending_events: std::collections::VecDeque<serde_json::Value>,
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
            pending_events: std::collections::VecDeque::new(),
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
            if frame["method"] == EVENT_METHOD {
                self.pending_events.push_back(frame["params"].clone());
            }
        }
    }

    async fn next_event(&mut self, timeout: Duration) -> Option<serde_json::Value> {
        if let Some(buffered) = self.pending_events.pop_front() {
            return Some(buffered);
        }
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

/// An authenticated, subscribed connection comments on a seeded issue: the
/// response carries the persisted row, and the connection receives the
/// `comment_added` push with the typed body.
#[tokio::test]
async fn comment_add_persists_and_pushes_event() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, _store) = start_server(dir.path()).await;

    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;
    c.subscribe(WS_SLUG).await;

    let resp = c
        .call(
            methods::HANGAR_COMMENT_ADD,
            serde_json::json!({
                "workspace_id": WS_SLUG,
                "issue_id": "issue-1",
                "author": "member:user-1",
                "body": "needs a second look",
            }),
        )
        .await;
    assert!(resp["error"].is_null(), "comment_add must ack: {resp}");
    // The response carries the persisted CommentRow.
    let row = &resp["result"];
    assert_eq!(row["issue_id"], "issue-1");
    assert_eq!(row["author"], "member:user-1");
    assert_eq!(row["body"], "needs a second look");
    assert!(row["id"].is_string() && !row["id"].as_str().unwrap().is_empty());

    // The subscribed connection received the comment_added push carrying the body.
    let event = c
        .next_event(Duration::from_secs(5))
        .await
        .expect("a committed comment must push a comment_added event");
    assert_eq!(event["event"], "comment_added", "wrong event: {event}");
    assert_eq!(event["issue_id"], "issue-1");
    assert_eq!(event["body"], "needs a second look");
    assert_eq!(event["author"], "member:user-1");
}

/// A mistyped / foreign workspace is rejected with an error — never a silent
/// no-op — and no comment is persisted (mirrors `handle_issue_update`).
#[tokio::test]
async fn comment_add_rejects_unknown_workspace_and_persists_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, store) = start_server(dir.path()).await;

    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;

    let resp = c
        .call(
            methods::HANGAR_COMMENT_ADD,
            serde_json::json!({
                "workspace_id": "nope-not-a-workspace",
                "issue_id": "issue-1",
                "author": "member:user-1",
                "body": "ghost comment",
            }),
        )
        .await;
    assert!(
        !resp["error"].is_null(),
        "an unknown workspace must be rejected, not a silent no-op: {resp}"
    );
    // No comment landed on issue-1.
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM comment WHERE issue_id = 'issue-1'")
        .fetch_one(store.pool())
        .await
        .unwrap();
    assert_eq!(count, 0, "a rejected comment must persist nothing");
}

/// An issue id that exists in another tenant cannot be commented on through this
/// workspace: the insert lands no row, and the issue has no comment (workspace
/// isolation — the bead's "no cross-tenant" requirement).
#[tokio::test]
async fn comment_add_is_workspace_scoped_no_cross_tenant_write() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, store) = start_server(dir.path()).await;

    // A second tenant workspace, real (so it resolves) but NOT owning issue-1.
    sqlx::query("INSERT INTO workspace (id, slug, name, created_at) VALUES (?, ?, ?, ?)")
        .bind("01HANGARFIXTUREWSB00000000")
        .bind("other")
        .bind("Other")
        .bind(1_700_000_000_000_i64)
        .execute(store.pool())
        .await
        .unwrap();

    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;

    let resp = c
        .call(
            methods::HANGAR_COMMENT_ADD,
            serde_json::json!({
                "workspace_id": "other",
                "issue_id": "issue-1",
                "author": "member:user-1",
                "body": "cross-tenant comment",
            }),
        )
        .await;
    // The workspace resolves, but the (issue, workspace) pair matches no row: the
    // handler reports a not-found error rather than commenting another tenant's
    // issue.
    assert!(
        !resp["error"].is_null(),
        "a cross-tenant issue id must not get a comment: {resp}"
    );
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM comment WHERE issue_id = 'issue-1'")
        .fetch_one(store.pool())
        .await
        .unwrap();
    assert_eq!(count, 0, "cross-tenant comment must persist nothing");
}
