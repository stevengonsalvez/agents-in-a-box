//! Integration: the `hangar/issues_search` RPC returns ranked title + desc +
//! comment matches over a real framed `UnixStream`, and is workspace-scoped
//! (e38.12).
//!
//! The seed fixture lays down three label-less issues in `WS_ID`; this test adds
//! its own issues + comments with controlled text so it can assert the exact
//! ranking, then a sibling tenant with a matching issue to prove isolation. It
//! verifies: (a) a title hit ranks above a description hit above a comment-only
//! hit, (b) a non-matching issue is excluded, (c) a sibling tenant's matching
//! issue is never returned, and (d) a blank query matches nothing.

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

/// Insert one issue directly (the create flow over RPC does not set a custom id).
async fn insert_issue(
    pool: &sqlx::SqlitePool,
    ws: &str,
    id: &str,
    title: &str,
    description: Option<&str>,
    created_at: i64,
) {
    use ainb_hangar_core::actor::{ActorKind, ActorRef};
    use ainb_hangar_store::repo::issue::{IssueRepo, NewIssue};
    IssueRepo::insert(
        pool,
        &NewIssue {
            id: id.into(),
            workspace_id: ws.into(),
            title: title.into(),
            description: description.map(ToString::to_string),
            state: "open".into(),
            assignee: None,
            creator: ActorRef::new(ActorKind::Member, "user-1").unwrap(),
            created_at,
            priority: 0,
            due_date: None,
            labels: Vec::new(),
            parent_issue_id: None,
            stage: None,
            acceptance_criteria: Vec::new(),
            context_refs: Vec::new(),
        },
    )
    .await
    .unwrap();
}

/// Insert one comment on an issue directly.
async fn insert_comment(pool: &sqlx::SqlitePool, id: &str, issue_id: &str, body: &str) {
    sqlx::query(
        "INSERT INTO comment (id, issue_id, author_type, author_id, body, created_at) \
         VALUES (?, ?, 'member', 'user-1', ?, 1700000001000)",
    )
    .bind(id)
    .bind(issue_id)
    .bind(body)
    .execute(pool)
    .await
    .unwrap();
}

/// Bind + serve the real listener over the seeded store, then add the search
/// fixture (controlled-text issues + a sibling tenant).
async fn start_server(dir: &std::path::Path) -> (std::path::PathBuf, Store) {
    let store = Store::open_in(dir).await.unwrap();
    seed::seed_p4_fixture(store.pool()).await.unwrap();

    // --- search fixture in the seeded workspace (WS_ID) ---
    // s-title: term in the title (rank 3).
    insert_issue(
        store.pool(),
        WS_ID,
        "s-title",
        "Improve telemetry pipeline",
        None,
        1_700_000_010_000,
    )
    .await;
    // s-desc: term only in the description (rank 2).
    insert_issue(
        store.pool(),
        WS_ID,
        "s-desc",
        "Routine cleanup",
        Some("the telemetry sink needs a flush"),
        1_700_000_011_000,
    )
    .await;
    // s-comment: term only in a comment body (rank 1).
    insert_issue(
        store.pool(),
        WS_ID,
        "s-comment",
        "Backlog grooming",
        Some("nothing relevant"),
        1_700_000_012_000,
    )
    .await;
    insert_comment(
        store.pool(),
        "sc1",
        "s-comment",
        "we lost telemetry yesterday",
    )
    .await;
    // s-none: term nowhere — must be excluded.
    insert_issue(
        store.pool(),
        WS_ID,
        "s-none",
        "Quiet issue",
        Some("no match here"),
        1_700_000_013_000,
    )
    .await;

    // --- a sibling tenant whose matching issue must never leak ---
    sqlx::query("INSERT INTO workspace (id, slug, name, created_at) VALUES (?, ?, ?, ?)")
        .bind("01HANGARFIXTUREWSB00000000")
        .bind("other")
        .bind("Other")
        .bind(1_700_000_000_000_i64)
        .execute(store.pool())
        .await
        .unwrap();
    insert_issue(
        store.pool(),
        "01HANGARFIXTUREWSB00000000",
        "s-foreign",
        "telemetry for the other tenant",
        None,
        1_700_000_014_000,
    )
    .await;

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

/// The ordered issue ids in a search result.
fn ids_in(resp: &serde_json::Value) -> Vec<String> {
    resp["result"]["issues"]
        .as_array()
        .unwrap_or_else(|| panic!("search result has no issues array: {resp}"))
        .iter()
        .map(|i| i["id"].as_str().unwrap().to_string())
        .collect()
}

/// A title hit ranks above a description hit above a comment-only hit; a
/// non-matching issue is excluded; a sibling tenant's matching issue never leaks.
#[tokio::test]
async fn issues_search_ranks_and_is_workspace_scoped() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, _store) = start_server(dir.path()).await;

    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;

    let resp = c
        .call(
            methods::HANGAR_ISSUES_SEARCH,
            serde_json::json!({ "workspace_id": WS_SLUG, "query": "telemetry" }),
        )
        .await;
    assert!(resp["error"].is_null(), "search must ack: {resp}");

    let ids = ids_in(&resp);
    assert_eq!(
        ids,
        ["s-title", "s-desc", "s-comment"],
        "title > description > comment ranking; non-match excluded; foreign tenant absent"
    );
    // Belt-and-braces: the foreign tenant's matching issue is never present.
    assert!(
        !ids.iter().any(|i| i == "s-foreign"),
        "a sibling tenant's matching issue must never be returned: {ids:?}"
    );
    // The seeded-fixture issues (Refactor API etc.) do not mention telemetry, so
    // they are excluded too — only the four fixture rows could match and one is
    // s-none, so exactly three hits.
    assert_eq!(ids.len(), 3, "exactly the three matching issues: {ids:?}");
}

/// A blank query matches nothing — searching for "" must not dump the board.
#[tokio::test]
async fn issues_search_blank_query_matches_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, _store) = start_server(dir.path()).await;

    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;

    let resp = c
        .call(
            methods::HANGAR_ISSUES_SEARCH,
            serde_json::json!({ "workspace_id": WS_SLUG, "query": "   " }),
        )
        .await;
    assert!(resp["error"].is_null(), "search must ack: {resp}");
    assert!(
        ids_in(&resp).is_empty(),
        "a blank query must match nothing, not dump the board"
    );
}

/// An unknown workspace yields an empty result, not an error (a read, like
/// `issues_list`).
#[tokio::test]
async fn issues_search_unknown_workspace_is_empty_not_error() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, _store) = start_server(dir.path()).await;

    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;

    let resp = c
        .call(
            methods::HANGAR_ISSUES_SEARCH,
            serde_json::json!({ "workspace_id": "nope-not-a-workspace", "query": "telemetry" }),
        )
        .await;
    assert!(
        resp["error"].is_null(),
        "an unknown workspace is an empty read, not an error: {resp}"
    );
    assert!(ids_in(&resp).is_empty(), "unknown workspace yields no rows");
}
