//! Integration: the `hangar/issue_criterion_set` RPC ticks ONE acceptance
//! criterion on an issue over a real framed `UnixStream` against a REAL sqlite
//! file, is workspace-scoped, and pushes the matching `hangar/event`
//! (`issue_updated`) to subscribed connections (multica parity #11-rest).
//!
//! The proof chain: create an issue carrying two criteria through
//! `hangar/issue_create` (so the daemon mints the ids), tick the SECOND one, then
//! assert (a) the reply row's `acceptance[1].checked` is true and `[0]` is not,
//! (b) a subscribed connection received the `issue_updated` push, (c) a DIRECT
//! sqlx read of the daemon's own `hangar.db` shows the structured objects with
//! `"checked":true`, (d) an unknown criterion id is `INVALID_PARAMS`, and (e) a
//! mistyped workspace is an error, never a silent ack.

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


/// Open a SECOND, independent pool onto the daemon's own database file, so the
/// persistence assertion reads what actually hit disk rather than the daemon's
/// in-process cache.
async fn direct_pool(dir: &std::path::Path) -> sqlx::SqlitePool {
    let opts = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(dir.join("hangar.db"))
        .create_if_missing(false);
    sqlx::sqlite::SqlitePoolOptions::new()
        .connect_with(opts)
        .await
        .expect("open the daemon's db file directly")
}

/// Create one issue with `criteria` through the real RPC and return its id.
async fn create_issue_with_criteria(c: &mut Client, criteria: &[&str]) -> String {
    let resp = c
        .call(
            methods::HANGAR_ISSUE_CREATE,
            serde_json::json!({
                "workspace_id": WS_SLUG,
                "title": "Gap 11-rest",
                "creator": "member:user-1",
                "acceptance_criteria": criteria,
            }),
        )
        .await;
    assert!(resp["error"].is_null(), "create must ack: {resp}");
    let row = &resp["result"];
    let acceptance = row["acceptance"].as_array().expect("structured acceptance");
    assert_eq!(acceptance.len(), criteria.len(), "row: {row}");
    for (i, want) in criteria.iter().enumerate() {
        assert_eq!(acceptance[i]["text"], *want);
        assert_eq!(acceptance[i]["checked"], false, "created unchecked");
        let id = acceptance[i]["id"].as_str().expect("minted id");
        assert!(id.starts_with("ac-"), "daemon mints an ac- id, got {id}");
    }
    // The pre-#11-rest text mirror still ships for old clients.
    let texts: Vec<String> = row["acceptance_criteria"]
        .as_array()
        .expect("text mirror")
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert_eq!(texts, criteria.to_vec(), "text mirror matches: {row}");
    row["id"].as_str().expect("issue id").to_string()
}

/// **T5** — the RPC half of the #11-rest acceptance, end to end.
#[tokio::test]
async fn rpc_issue_criterion_set_persists_and_pushes_event() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, _store) = start_server(dir.path()).await;

    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;
    c.subscribe(WS_SLUG).await;

    let issue_id = create_issue_with_criteria(&mut c, &["builds green", "detail card ticks"]).await;
    // Drain the create's own push so it does not shadow the tick's event.
    let _ = c.next_event(Duration::from_secs(5)).await;

    // Read back the minted id of the SECOND criterion and tick it by id.
    let list = c
        .call(
            methods::HANGAR_ISSUES_LIST,
            serde_json::json!({ "workspace_id": WS_SLUG }),
        )
        .await;
    let row = list["result"]["issues"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["id"] == issue_id.as_str())
        .expect("the created issue is listed")
        .clone();
    let second_id = row["acceptance"][1]["id"]
        .as_str()
        .expect("second criterion id")
        .to_string();

    let resp = c
        .call(
            methods::HANGAR_ISSUE_CRITERION_SET,
            serde_json::json!({
                "workspace_id": WS_SLUG,
                "issue_id": issue_id,
                "criterion": second_id,
                "checked": true,
                "actor": "agent:builder",
            }),
        )
        .await;
    assert!(resp["error"].is_null(), "tick must ack: {resp}");
    let acceptance = resp["result"]["acceptance"]
        .as_array()
        .expect("structured acceptance on the reply");
    assert_eq!(acceptance[1]["checked"], true, "reply: {resp}");
    assert_eq!(acceptance[0]["checked"], false, "sibling untouched: {resp}");
    assert_eq!(acceptance[1]["checked_by"], "agent:builder");
    assert!(
        acceptance[1]["checked_at"].as_i64().is_some(),
        "the daemon stamped when: {resp}"
    );

    // The subscribed connection received the issue_updated push.
    let event = c
        .next_event(Duration::from_secs(5))
        .await
        .expect("a committed tick must push an issue_updated event");
    assert_eq!(event["event"], "issue_updated", "wrong event: {event}");
    assert_eq!(event["id"], issue_id.as_str());
    assert_eq!(event["acceptance"][1]["checked"], true, "event: {event}");

    // It really hit disk: read the daemon's OWN db file through a fresh pool.
    let pool = direct_pool(dir.path()).await;
    let raw: String =
        sqlx::query_scalar("SELECT acceptance_criteria FROM issue WHERE id = ?")
            .bind(&issue_id)
            .fetch_one(&pool)
            .await
            .expect("read the column");
    assert!(
        raw.starts_with("[{"),
        "the column holds structured objects, got {raw}"
    );
    assert!(
        raw.contains(r#""text":"detail card ticks","checked":true"#),
        "the ticked criterion persisted checked, got {raw}"
    );
    assert!(
        raw.contains(r#""text":"builds green","checked":false"#),
        "the sibling persisted unchecked, got {raw}"
    );
    assert!(
        raw.contains(r#""checked_by":"agent:builder""#),
        "provenance persisted, got {raw}"
    );

    // An unknown criterion id is a client error, never a silent ack.
    let resp = c
        .call(
            methods::HANGAR_ISSUE_CRITERION_SET,
            serde_json::json!({
                "workspace_id": WS_SLUG,
                "issue_id": issue_id,
                "criterion": "ac-does-not-exist",
                "checked": true,
            }),
        )
        .await;
    assert!(
        !resp["error"].is_null(),
        "an unknown criterion must be rejected: {resp}"
    );

    // A mistyped workspace is rejected outright — never a silent no-op.
    let resp = c
        .call(
            methods::HANGAR_ISSUE_CRITERION_SET,
            serde_json::json!({
                "workspace_id": "no-such-workspace",
                "issue_id": issue_id,
                "criterion": second_id,
                "checked": false,
            }),
        )
        .await;
    assert!(
        !resp["error"].is_null(),
        "a mistyped workspace must be rejected: {resp}"
    );
    // ...and it changed nothing.
    let raw_after: String =
        sqlx::query_scalar("SELECT acceptance_criteria FROM issue WHERE id = ?")
            .bind(&issue_id)
            .fetch_one(&pool)
            .await
            .expect("read the column");
    assert_eq!(raw, raw_after, "a rejected call wrote nothing");

    // Untick by 1-BASED ORDINAL reaches the same criterion and clears provenance.
    let resp = c
        .call(
            methods::HANGAR_ISSUE_CRITERION_SET,
            serde_json::json!({
                "workspace_id": WS_SLUG,
                "issue_id": issue_id,
                "criterion": "2",
                "checked": false,
            }),
        )
        .await;
    assert!(resp["error"].is_null(), "untick by ordinal must ack: {resp}");
    assert_eq!(resp["result"]["acceptance"][1]["checked"], false);
    assert!(
        resp["result"]["acceptance"][1]["checked_by"].is_null(),
        "untick cleared attribution: {resp}"
    );
}
