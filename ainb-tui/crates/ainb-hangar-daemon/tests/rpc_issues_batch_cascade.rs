//! Integration: `hangar/issues_batch_update` posts ONE aggregated child-done
//! cascade comment for a whole batch (multica parity #3-rest, MUL-4155).
//!
//! Real daemon, real framed `UnixStream`, real sqlite file. Two `stage 1`
//! sub-issues are created under one parent and completed in a SINGLE
//! `issues_batch_update` call, then the test asserts, against the daemon's own
//! database:
//!
//! - the result carries exactly ONE `BatchCascadeRow` naming BOTH children,
//! - `SELECT count(*) FROM comment WHERE issue_id = <parent>` is `1` — the
//!   literal acceptance sentence, asserted as an EXACT count so the broken
//!   two-comment behaviour cannot pass,
//! - `issue_cascade_barrier` holds exactly one claim for that parent,
//! - exactly ONE `comment_added` event was pushed, not two.

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


/// Create one issue through the RPC and return its id.
async fn create_issue(
    c: &mut Client,
    title: &str,
    parent: Option<&str>,
    stage: Option<i64>,
) -> String {
    let mut params = serde_json::json!({
        "workspace_id": WS_SLUG,
        "title": title,
        "creator": "member:user-1",
    });
    if let Some(p) = parent {
        params["parent_issue_id"] = serde_json::json!(p);
    }
    if let Some(s) = stage {
        params["stage"] = serde_json::json!(s);
    }
    let resp = c.call(methods::HANGAR_ISSUE_CREATE, params).await;
    assert!(resp["error"].is_null(), "issue_create must ack: {resp}");
    resp["result"]["id"].as_str().expect("created id").to_string()
}

async fn scalar(store: &Store, sql: &str, bind: &str) -> i64 {
    sqlx::query_scalar::<_, i64>(sql)
        .bind(bind)
        .fetch_one(store.pool())
        .await
        .expect("sqlite readout")
}

/// **THE ACCEPTANCE.** Two same-stage sub-issues completed in ONE batch produce
/// exactly one parent comment, one barrier claim, and one pushed event.
#[tokio::test]
async fn issues_batch_update_posts_one_aggregated_cascade_comment() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, store) = start_server(dir.path()).await;

    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;
    c.subscribe(WS_SLUG).await;

    let parent = create_issue(&mut c, "Parent", None, None).await;
    let a = create_issue(&mut c, "A", Some(&parent), Some(1)).await;
    let b = create_issue(&mut c, "B", Some(&parent), Some(1)).await;

    // The children were created staged — the wire actually carries `stage` now.
    assert_eq!(
        scalar(
            &store,
            "SELECT COUNT(*) FROM issue WHERE parent_issue_id = ? AND stage = 1",
            &parent,
        )
        .await,
        2,
        "both children must persist stage 1"
    );

    let resp = c
        .call(
            methods::HANGAR_ISSUES_BATCH_UPDATE,
            serde_json::json!({
                "workspace_id": WS_SLUG,
                "issue_ids": [a.clone(), b.clone()],
                "state": "done",
            }),
        )
        .await;
    assert!(resp["error"].is_null(), "batch update must ack: {resp}");

    let cascades = resp["result"]["cascades"].as_array().expect("cascades array");
    assert_eq!(cascades.len(), 1, "ONE cascade for one closed barrier: {resp}");
    assert_eq!(cascades[0]["parent_id"], serde_json::json!(parent));
    let child_ids: Vec<String> = cascades[0]["child_ids"]
        .as_array()
        .expect("child_ids")
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert_eq!(child_ids.len(), 2, "the ONE comment reports BOTH children");
    assert!(child_ids.contains(&a) && child_ids.contains(&b));
    assert_eq!(cascades[0]["children_done"], serde_json::json!(2));
    assert_eq!(cascades[0]["children_total"], serde_json::json!(2));
    assert_eq!(
        resp["result"]["updated"].as_array().expect("updated").len(),
        2,
        "both rows changed"
    );

    // The literal acceptance, read from the daemon's own database.
    assert_eq!(
        scalar(&store, "SELECT COUNT(*) FROM comment WHERE issue_id = ?", &parent).await,
        1,
        "EXACTLY one cascade comment on the parent, never two"
    );
    assert_eq!(
        scalar(
            &store,
            "SELECT COUNT(*) FROM issue_cascade_barrier WHERE parent_issue_id = ?",
            &parent,
        )
        .await,
        1,
        "one closed barrier = one claim row"
    );
    let body: String =
        sqlx::query_scalar("SELECT body FROM comment WHERE issue_id = ? LIMIT 1")
            .bind(&parent)
            .fetch_one(store.pool())
            .await
            .expect("the cascade comment");
    assert!(body.contains(&a) && body.contains(&b), "names both: {body}");
    assert!(body.contains("Closed stage 1."), "names the barrier: {body}");

    // Exactly ONE comment_added push, not one per child.
    let mut comment_events = 0;
    while let Some(ev) = c.next_event(Duration::from_millis(600)).await {
        if ev["event"] == "comment_added" {
            comment_events += 1;
        }
    }
    assert_eq!(comment_events, 1, "one aggregated comment = one push");
}

/// A stage below 1 is a client error at the boundary, never an opaque store
/// fault from the sqlite CHECK.
#[tokio::test]
async fn issue_create_rejects_a_stage_below_one() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, _store) = start_server(dir.path()).await;

    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;

    let resp = c
        .call(
            methods::HANGAR_ISSUE_CREATE,
            serde_json::json!({
                "workspace_id": WS_SLUG,
                "title": "bad stage",
                "creator": "member:user-1",
                "stage": 0,
            }),
        )
        .await;
    assert!(!resp["error"].is_null(), "stage 0 must be rejected: {resp}");
    assert!(
        resp["error"]["message"].as_str().unwrap_or_default().contains("stage"),
        "the message must name the offending field: {resp}"
    );
}

/// A mistyped state is rejected BEFORE anything is written — a batch must never
/// half-apply.
#[tokio::test]
async fn issues_batch_update_rejects_an_unknown_state_without_writing() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, store) = start_server(dir.path()).await;

    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;

    let before = scalar(&store, "SELECT COUNT(*) FROM issue WHERE state = ?", "done").await;
    let resp = c
        .call(
            methods::HANGAR_ISSUES_BATCH_UPDATE,
            serde_json::json!({
                "workspace_id": WS_SLUG,
                "issue_ids": ["issue-1", "issue-2"],
                "state": "finiwhed",
            }),
        )
        .await;
    assert!(!resp["error"].is_null(), "a typo'd state must be rejected: {resp}");
    let after = scalar(&store, "SELECT COUNT(*) FROM issue WHERE state = ?", "done").await;
    assert_eq!(before, after, "nothing may be written by a rejected batch");
}
