//! Integration: ORIGIN PROVENANCE end to end over a real framed `UnixStream`
//! (migration 0056, multica parity #21).
//!
//! **This file is acceptance leg B**: *an issue created by a comment mention
//! records its origin (sqlite)*. It drives the real chain the daemon runs —
//!
//! 1. `hangar/comment_add` with `@claude-agent` commits the comment and fans out
//!    a task, which is stamped `('comment_mention', <comment.id>)`;
//! 2. the daemon would hand that pair to the agent child as
//!    `HANGAR_ORIGIN_TYPE` / `HANGAR_ORIGIN_ID` (asserted in the `run_loop` /
//!    `runner` unit legs), and the child's `ainb hangar issue create` echoes it
//!    back over `hangar/issue_create` — replayed here by reading the pair off the
//!    spawned TASK row and passing it on the create, exactly as the CLI does;
//! 3. the created ISSUE row is read STRAIGHT FROM SQLITE and must carry the same
//!    pair.
//!
//! The negative legs enforce multica's handler contract verbatim
//! (`internal/handler/issue.go:1213-1231`): the two halves must arrive together,
//! the kind must be on the closed allow-list, and a kind that needs an id must
//! get one — each an `INVALID_PARAMS`, never a silent drop. A plain create with
//! no origin is stamped `('manual', NULL)`.

use std::time::{Duration, Instant};

use ainb_hangar_daemon::rpc::{self, DaemonHealth};
use ainb_hangar_daemon::seed::{self, WS_ID, WS_SLUG};
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

    /// Send `method`, then drain frames until the response (id-bearing) lands,
    /// ignoring any interleaved event pushes.
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

    async fn subscribe(&mut self, workspace_id: &str) {
        let resp = self
            .call(
                methods::WORKSPACE_SUBSCRIBE,
                serde_json::json!({ "workspace_id": workspace_id }),
            )
            .await;
        assert!(resp["error"].is_null(), "subscribe must ack: {resp}");
    }

    async fn comment(&mut self, issue_id: &str, body: &str) -> serde_json::Value {
        let resp = self
            .call(
                methods::HANGAR_COMMENT_ADD,
                serde_json::json!({
                    "workspace_id": WS_SLUG,
                    "issue_id": issue_id,
                    "author": "member:user-1",
                    "body": body,
                }),
            )
            .await;
        assert!(resp["error"].is_null(), "comment_add must ack: {resp}");
        resp
    }

    /// The create an agent child performs: `ainb hangar issue create` forwards
    /// the origin pair it read from its env.
    async fn create_issue(
        &mut self,
        title: &str,
        origin: Option<(&str, Option<&str>)>,
    ) -> serde_json::Value {
        let mut params = serde_json::json!({
            "workspace_id": WS_SLUG,
            "title": title,
            "creator": "agent:agent-1",
        });
        if let Some((kind, id)) = origin {
            params["origin_type"] = serde_json::Value::String(kind.to_string());
            if let Some(id) = id {
                params["origin_id"] = serde_json::Value::String(id.to_string());
            }
        }
        self.call(methods::HANGAR_ISSUE_CREATE, params).await
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

/// The stored `(origin_type, origin_id)` of one issue, straight from sqlite.
async fn issue_origin(store: &Store, issue_id: &str) -> (Option<String>, Option<String>) {
    sqlx::query_as("SELECT origin_type, origin_id FROM issue WHERE id = ?")
        .bind(issue_id)
        .fetch_one(store.pool())
        .await
        .unwrap()
}

/// The stored `(origin_type, origin_id)` of the single task on `issue_id`.
async fn task_origin(store: &Store, issue_id: &str) -> (Option<String>, Option<String>) {
    sqlx::query_as(
        "SELECT origin_type, origin_id FROM agent_task_queue \
         WHERE workspace_id = ?1 AND issue_id = ?2",
    )
    .bind(WS_ID)
    .bind(issue_id)
    .fetch_one(store.pool())
    .await
    .unwrap()
}

/// **ACCEPTANCE LEG B.** The whole comment-mention provenance chain, ending in
/// sqlite: a mention stamps the spawned task with the COMMENT's id, and an issue
/// the mention-spawned agent then creates carries the identical pair.
#[tokio::test]
async fn an_issue_created_through_the_comment_mention_chain_records_its_origin() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, store) = start_server(dir.path()).await;

    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;
    c.subscribe(WS_SLUG).await;

    // 1. The mention. `issue-3` carries no seeded task, so the spawn is
    //    unambiguously this comment's doing.
    let resp = c.comment("issue-3", "@claude-agent please split this out").await;
    let comment_id = resp["result"]["id"].as_str().expect("comment id").to_string();
    assert!(!comment_id.is_empty());

    // 2. The spawned TASK carries ('comment_mention', <comment.id>).
    let (kind, id) = task_origin(&store, "issue-3").await;
    assert_eq!(kind.as_deref(), Some("comment_mention"));
    assert_eq!(
        id.as_deref(),
        Some(comment_id.as_str()),
        "the task's provenance id is the COMMENT that asked for it"
    );

    // 3. Replay what the agent child does: `ainb hangar issue create` forwards
    //    the pair the daemon put in its env (read here off the task row).
    let resp = c
        .create_issue(
            "Split out the parser",
            Some(("comment_mention", Some(&comment_id))),
        )
        .await;
    assert!(resp["error"].is_null(), "issue_create must ack: {resp}");
    let new_issue_id = resp["result"]["id"].as_str().expect("issue id").to_string();

    // The response row carries it too, so the pushed IssueCreated event and a
    // later list snapshot agree.
    assert_eq!(resp["result"]["origin_type"], "comment_mention");
    assert_eq!(resp["result"]["origin_id"], comment_id.as_str());

    // 4. THE ACCEPTANCE READ: sqlite.
    let (kind, id) = issue_origin(&store, &new_issue_id).await;
    assert_eq!(kind.as_deref(), Some("comment_mention"));
    assert_eq!(id.as_deref(), Some(comment_id.as_str()));
}

/// A plain create — no mention, no origin params — is stamped `('manual', NULL)`
/// rather than left NULL, so `origin_type IS NULL` keeps meaning exactly one
/// thing: "created before provenance existed".
#[tokio::test]
async fn a_plain_create_is_stamped_manual() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, store) = start_server(dir.path()).await;

    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;
    c.subscribe(WS_SLUG).await;

    let resp = c.create_issue("By hand", None).await;
    assert!(resp["error"].is_null(), "issue_create must ack: {resp}");
    let id = resp["result"]["id"].as_str().unwrap().to_string();

    let (kind, origin_id) = issue_origin(&store, &id).await;
    assert_eq!(kind.as_deref(), Some("manual"));
    assert_eq!(origin_id, None);
}

/// multica `handler/issue.go:1221`: a kind outside the closed allow-list is a
/// client error, so a rogue caller cannot mint an arbitrary provenance label.
#[tokio::test]
async fn an_unknown_origin_type_is_rejected_and_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, store) = start_server(dir.path()).await;

    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;
    c.subscribe(WS_SLUG).await;

    let before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM issue")
        .fetch_one(store.pool())
        .await
        .unwrap();

    let resp = c.create_issue("Nope", Some(("quick_create", Some("x")))).await;
    let message = resp["error"]["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("unsupported origin_type"),
        "expected the allow-list rejection, got: {resp}"
    );

    let after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM issue")
        .fetch_one(store.pool())
        .await
        .unwrap();
    assert_eq!(after, before, "a rejected origin must not land an issue");
}

/// multica `handler/issue.go:1215`, verbatim wording: the two halves must be
/// provided together.
#[tokio::test]
async fn an_origin_id_without_a_type_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, _store) = start_server(dir.path()).await;

    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;
    c.subscribe(WS_SLUG).await;

    let resp = c
        .call(
            methods::HANGAR_ISSUE_CREATE,
            serde_json::json!({
                "workspace_id": WS_SLUG,
                "title": "Nope",
                "creator": "agent:agent-1",
                "origin_id": "c-1",
            }),
        )
        .await;
    let message = resp["error"]["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("must be provided together"),
        "expected multica's pair-rule wording, got: {resp}"
    );
}

/// A kind that requires an id and got none is rejected — `autopilot` provenance
/// with no autopilot to point at is not provenance.
#[tokio::test]
async fn an_autopilot_origin_without_an_id_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, _store) = start_server(dir.path()).await;

    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;
    c.subscribe(WS_SLUG).await;

    let resp = c.create_issue("Nope", Some(("autopilot", None))).await;
    let message = resp["error"]["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("requires an origin_id"),
        "expected the missing-id rejection, got: {resp}"
    );
}
