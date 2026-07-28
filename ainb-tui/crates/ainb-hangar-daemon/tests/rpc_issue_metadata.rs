//! Integration: the `hangar/issue_metadata_*` RPCs (multica parity #17) over a
//! real framed `UnixStream` against a REAL sqlite file.
//!
//! The load-bearing invariant, quoted from the reference's own handler header:
//! *"All mutations are single-key atomic. `UpdateIssue` does NOT touch metadata
//! — any whole-blob overwrite would race with concurrent agent writes."* That is
//! asserted here at the RPC boundary, alongside primitive typing surviving the
//! wire and every cap / key / type rejection being `INVALID_PARAMS`.

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

    async fn next_event(&mut self, timeout: Duration) -> Option<serde_json::Value> {
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.checked_duration_since(Instant::now())?;
            let frame = self.read_frame(remaining).await?;
            if frame.get("id").is_none()
                && frame["method"] == ainb_hangar_proto::events::EVENT_METHOD
            {
                return Some(frame["params"].clone());
            }
        }
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
///
/// `seed` is skipped on a RE-serve of an existing directory, so the restart
/// assertion reads the same rows the first daemon wrote.
async fn start_server(dir: &std::path::Path, seed_fixture: bool) -> (std::path::PathBuf, Store) {
    let store = Store::open_in(dir).await.unwrap();
    if seed_fixture {
        seed::seed_p4_fixture(store.pool()).await.unwrap();
    }
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

/// Create one issue through the real RPC and return its id.
async fn create_issue(c: &mut Client, title: &str) -> String {
    let resp = c
        .call(
            methods::HANGAR_ISSUE_CREATE,
            serde_json::json!({
                "workspace_id": WS_SLUG,
                "title": title,
                "creator": "member:user-1",
            }),
        )
        .await;
    assert!(resp["error"].is_null(), "create must ack: {resp}");
    resp["result"]["id"].as_str().expect("issue id").to_string()
}

/// set → get → delete over the socket, with `IssueUpdated` pushed on each
/// mutation and integer fidelity surviving the round trip.
#[tokio::test]
async fn metadata_set_get_delete_round_trips_and_pushes_events() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, _store) = start_server(dir.path(), true).await;
    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;
    c.subscribe(WS_SLUG).await;

    let issue_id = create_issue(&mut c, "Agent scratch").await;
    // Drain the create's own push so it cannot shadow the mutation's event.
    let _ = c.next_event(Duration::from_secs(5)).await;

    let resp = c
        .call(
            methods::HANGAR_ISSUE_METADATA_SET,
            serde_json::json!({
                "workspace_id": WS_SLUG, "issue_id": issue_id,
                "key": "pr_number", "value": "471", "value_type": "number",
            }),
        )
        .await;
    assert!(resp["error"].is_null(), "set must ack: {resp}");
    let entries = resp["result"]["entries"].as_array().expect("entries");
    assert_eq!(entries.len(), 1, "{resp}");
    assert_eq!(entries[0]["key"], "pr_number");
    assert_eq!(
        entries[0]["value_json"], "471",
        "an integer stays an integer, never 471.0: {resp}"
    );
    assert_eq!(
        entries[0]["value"], "471",
        "the rendered form is unquoted: {resp}"
    );

    // The mutation announced the refreshed row, carrying the same bag.
    let event = c
        .next_event(Duration::from_secs(5))
        .await
        .expect("issue_updated after a metadata write");
    assert_eq!(event["event"], "issue_updated", "{event}");
    assert_eq!(event["id"], issue_id.as_str(), "{event}");
    assert_eq!(event["metadata"][0]["key"], "pr_number", "{event}");
    assert_eq!(event["metadata"][0]["value"], "471", "{event}");

    // A second key of a DIFFERENT primitive type coexists.
    let resp = c
        .call(
            methods::HANGAR_ISSUE_METADATA_SET,
            serde_json::json!({
                "workspace_id": WS_SLUG, "issue_id": issue_id,
                "key": "pipeline_status", "value": "running",
            }),
        )
        .await;
    assert!(resp["error"].is_null(), "second set must ack: {resp}");
    let _ = c.next_event(Duration::from_secs(5)).await;

    let resp = c
        .call(
            methods::HANGAR_ISSUE_METADATA_GET,
            serde_json::json!({ "workspace_id": WS_SLUG, "issue_id": issue_id }),
        )
        .await;
    let keys: Vec<&str> = resp["result"]["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["key"].as_str().unwrap())
        .collect();
    assert_eq!(
        keys,
        vec!["pipeline_status", "pr_number"],
        "key-sorted: {resp}"
    );

    // A `key` on GET narrows to that one entry.
    let resp = c
        .call(
            methods::HANGAR_ISSUE_METADATA_GET,
            serde_json::json!({
                "workspace_id": WS_SLUG, "issue_id": issue_id, "key": "pr_number",
            }),
        )
        .await;
    assert_eq!(
        resp["result"]["entries"].as_array().unwrap().len(),
        1,
        "{resp}"
    );
    assert_eq!(resp["result"]["entries"][0]["key"], "pr_number", "{resp}");

    let resp = c
        .call(
            methods::HANGAR_ISSUE_METADATA_DELETE,
            serde_json::json!({
                "workspace_id": WS_SLUG, "issue_id": issue_id, "key": "pr_number",
            }),
        )
        .await;
    assert!(resp["error"].is_null(), "delete must ack: {resp}");
    let keys: Vec<&str> = resp["result"]["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["key"].as_str().unwrap())
        .collect();
    assert_eq!(
        keys,
        vec!["pipeline_status"],
        "only the one key went: {resp}"
    );
}

/// The anti-race rule at the RPC boundary: `hangar/issue_update` never touches
/// the metadata bag.
#[tokio::test]
async fn an_unrelated_issue_update_leaves_the_metadata_bag_intact() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, _store) = start_server(dir.path(), true).await;
    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;

    let issue_id = create_issue(&mut c, "Ship #17").await;
    let resp = c
        .call(
            methods::HANGAR_ISSUE_METADATA_SET,
            serde_json::json!({
                "workspace_id": WS_SLUG, "issue_id": issue_id,
                "key": "pr_number", "value": "471", "value_type": "number",
            }),
        )
        .await;
    assert!(resp["error"].is_null(), "set must ack: {resp}");

    let resp = c
        .call(
            methods::HANGAR_ISSUE_UPDATE,
            serde_json::json!({
                "workspace_id": WS_SLUG, "issue_id": issue_id,
                "title": "Ship #17 (v2)", "state": "in_progress",
            }),
        )
        .await;
    assert!(resp["error"].is_null(), "update must ack: {resp}");
    assert_eq!(
        resp["result"]["title"], "Ship #17 (v2)",
        "the edit landed: {resp}"
    );
    assert_eq!(
        resp["result"]["metadata"][0]["value"], "471",
        "an unrelated UPDATE must not clobber the bag: {resp}"
    );

    let resp = c
        .call(
            methods::HANGAR_ISSUE_METADATA_GET,
            serde_json::json!({ "workspace_id": WS_SLUG, "issue_id": issue_id }),
        )
        .await;
    assert_eq!(resp["result"]["entries"][0]["value_json"], "471", "{resp}");
}

/// Every cap / key / type rejection is a CLIENT error, never a 500, and none of
/// them writes.
#[tokio::test]
async fn metadata_rejections_are_client_errors_and_write_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, store) = start_server(dir.path(), true).await;
    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;

    let issue_id = create_issue(&mut c, "Rejection sweep").await;
    // JSON-RPC `INVALID_PARAMS` — a CLIENT error, never a 500.
    let invalid = -32602;

    for (label, params) in [
        (
            "a key starting with a digit",
            serde_json::json!({
                "workspace_id": WS_SLUG, "issue_id": issue_id,
                "key": "9lives", "value": "x",
            }),
        ),
        (
            "a key with a space",
            serde_json::json!({
                "workspace_id": WS_SLUG, "issue_id": issue_id,
                "key": "a b", "value": "x",
            }),
        ),
        (
            "a key past 64 characters",
            serde_json::json!({
                "workspace_id": WS_SLUG, "issue_id": issue_id,
                "key": "k".repeat(65), "value": "x",
            }),
        ),
        (
            "a missing value (the reference says use DELETE)",
            serde_json::json!({
                "workspace_id": WS_SLUG, "issue_id": issue_id, "key": "ok_key",
            }),
        ),
        (
            "a non-numeric value forced to number",
            serde_json::json!({
                "workspace_id": WS_SLUG, "issue_id": issue_id,
                "key": "ok_key", "value": "soon", "value_type": "number",
            }),
        ),
        (
            "an unknown value_type",
            serde_json::json!({
                "workspace_id": WS_SLUG, "issue_id": issue_id,
                "key": "ok_key", "value": "x", "value_type": "hologram",
            }),
        ),
        (
            "an issue id from no workspace",
            serde_json::json!({
                "workspace_id": WS_SLUG, "issue_id": "issue-nowhere",
                "key": "ok_key", "value": "x",
            }),
        ),
    ] {
        let resp = c.call(methods::HANGAR_ISSUE_METADATA_SET, params).await;
        assert_eq!(
            resp["error"]["code"], invalid,
            "{label} is INVALID_PARAMS, never a 500: {resp}"
        );
    }

    // The 51st DISTINCT key is refused; overwriting an existing one at the cap
    // still succeeds, exactly as the reference.
    for i in 0..50 {
        let resp = c
            .call(
                methods::HANGAR_ISSUE_METADATA_SET,
                serde_json::json!({
                    "workspace_id": WS_SLUG, "issue_id": issue_id,
                    "key": format!("k{i}"), "value": i.to_string(),
                }),
            )
            .await;
        assert!(resp["error"].is_null(), "key {i} is within the cap: {resp}");
    }
    let resp = c
        .call(
            methods::HANGAR_ISSUE_METADATA_SET,
            serde_json::json!({
                "workspace_id": WS_SLUG, "issue_id": issue_id,
                "key": "one_too_many", "value": "x",
            }),
        )
        .await;
    assert_eq!(resp["error"]["code"], invalid, "the 51st key: {resp}");
    let resp = c
        .call(
            methods::HANGAR_ISSUE_METADATA_SET,
            serde_json::json!({
                "workspace_id": WS_SLUG, "issue_id": issue_id,
                "key": "k7", "value": "rewritten",
            }),
        )
        .await;
    assert!(
        resp["error"].is_null(),
        "an overwrite AT the cap succeeds: {resp}"
    );

    let raw: String = sqlx::query_scalar("SELECT metadata FROM issue WHERE id = ?")
        .bind(&issue_id)
        .fetch_one(store.pool())
        .await
        .expect("read the stored bag");
    let bag: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(&raw).expect("the bag is a JSON object");
    assert_eq!(bag.len(), 50, "no rejection ever wrote a key: {raw}");
    assert!(!bag.contains_key("ok_key"), "{raw}");
    assert_eq!(bag["k7"], serde_json::json!("rewritten"), "{raw}");
}
