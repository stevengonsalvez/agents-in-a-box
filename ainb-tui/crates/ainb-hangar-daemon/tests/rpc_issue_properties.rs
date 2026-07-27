//! Integration: the custom-property catalog RPCs (multica parity #17) over a
//! real framed `UnixStream` against a REAL sqlite file.
//!
//! The proof chain: `hangar/property_define` → `hangar/issue_property_set` →
//! the issue's DETAIL row carries the RESOLVED property; the value survives a
//! full daemon restart on the same db file; and every decoy is asserted ABSENT
//! (a definition in another workspace, an ARCHIVED definition, a LIST snapshot's
//! rows, and an issue with no values sending no `properties` key at all).

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

/// Re-read one issue's DETAIL row (the path that carries `properties`).
///
/// `hangar/issue_update` re-reads and answers with the DETAIL row, and setting
/// the title to what it already is doubles as the anti-race control: an
/// unrelated `UPDATE issue` must never disturb the property bag.
async fn detail_row(c: &mut Client, issue_id: &str, title: &str) -> serde_json::Value {
    let resp = c
        .call(
            methods::HANGAR_ISSUE_UPDATE,
            serde_json::json!({
                "workspace_id": WS_SLUG,
                "issue_id": issue_id,
                "title": title,
            }),
        )
        .await;
    assert!(resp["error"].is_null(), "update must ack: {resp}");
    resp["result"].clone()
}

/// The whole #17 acceptance clause over the wire: define → set → renders, and
/// the value survives a daemon restart on the same database file.
#[tokio::test]
async fn property_define_then_set_lands_on_the_detail_row_and_survives_restart() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, _store) = start_server(dir.path(), true).await;

    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;

    let resp = c
        .call(
            methods::HANGAR_PROPERTY_DEFINE,
            serde_json::json!({
                "workspace_id": WS_SLUG,
                "key": "sprint",
                "name": "Sprint",
                "kind": "select",
                "options": ["S1", "S2"],
            }),
        )
        .await;
    assert!(resp["error"].is_null(), "define must ack: {resp}");
    assert_eq!(resp["result"]["kind"], "select", "{resp}");
    assert_eq!(resp["result"]["archived"], false, "{resp}");

    let issue_id = create_issue(&mut c, "Ship #17").await;
    let resp = c
        .call(
            methods::HANGAR_ISSUE_PROPERTY_SET,
            serde_json::json!({
                "workspace_id": WS_SLUG,
                "issue_id": issue_id,
                "key": "sprint",
                "value": "S2",
            }),
        )
        .await;
    assert!(resp["error"].is_null(), "set must ack: {resp}");
    let props = resp["result"]["properties"].as_array().expect("resolved properties");
    assert_eq!(props.len(), 1, "exactly the one set property: {resp}");
    assert_eq!(props[0]["key"], "sprint");
    assert_eq!(props[0]["name"], "Sprint");
    assert_eq!(props[0]["kind"], "select");
    assert_eq!(props[0]["value"], "S2");

    // The stored bag is keyed by the DEFINITION ID, never by the name — which is
    // what makes a rename free.
    let raw: String = sqlx::query_scalar("SELECT properties FROM issue WHERE id = ?")
        .bind(&issue_id)
        .fetch_one(_store.pool())
        .await
        .expect("read the stored bag");
    let bag: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(&raw).expect("the bag is a JSON object");
    assert_eq!(bag.len(), 1, "one stored value: {raw}");
    let def_id = bag.keys().next().unwrap();
    assert_ne!(
        def_id, "sprint",
        "keyed by definition id, not by key: {raw}"
    );
    assert_ne!(
        def_id, "Sprint",
        "keyed by definition id, not by name: {raw}"
    );
    assert_eq!(bag[def_id], serde_json::json!("S2"), "{raw}");

    // A RENAME is a catalog-only write: the stored blob is byte-identical.
    let resp = c
        .call(
            methods::HANGAR_PROPERTY_DEFINE,
            serde_json::json!({ "workspace_id": WS_SLUG, "key": "sprint", "name": "Iteration" }),
        )
        .await;
    assert!(resp["error"].is_null(), "rename must ack: {resp}");
    let after: String = sqlx::query_scalar("SELECT properties FROM issue WHERE id = ?")
        .bind(&issue_id)
        .fetch_one(_store.pool())
        .await
        .expect("re-read the stored bag");
    assert_eq!(after, raw, "a rename touches ZERO issue rows");
    let row = detail_row(&mut c, &issue_id, "Ship #17").await;
    assert_eq!(row["properties"][0]["name"], "Iteration", "{row}");
    assert_eq!(row["properties"][0]["value"], "S2", "{row}");

    // PERSISTS: drop everything and re-serve the SAME database file.
    drop(c);
    _store.pool().close().await;
    let (socket_path, _store2) = start_server(dir.path(), false).await;
    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;
    let row = detail_row(&mut c, &issue_id, "Ship #17").await;
    assert_eq!(
        row["properties"][0]["value"], "S2",
        "the value survives a daemon restart: {row}"
    );
}

/// Every decoy the resolve path must NOT surface, in one run.
#[tokio::test]
async fn archived_foreign_and_listed_rows_never_carry_a_resolved_property() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, store) = start_server(dir.path(), true).await;

    // A DECOY workspace with its own same-key definition. It must never leak.
    sqlx::query("INSERT INTO workspace (id, slug, name, created_at) VALUES (?, ?, ?, 0)")
        .bind("ws-decoy")
        .bind("decoy")
        .bind("DECOY")
        .execute(store.pool())
        .await
        .expect("seed the decoy workspace");
    sqlx::query(
        "INSERT INTO issue_property \
         (id, workspace_id, key, name, kind, options, position, created_at) \
         VALUES ('prop-decoy', 'ws-decoy', 'sprint', 'DECOY SPRINT', 'text', '[]', 0, 0)",
    )
    .execute(store.pool())
    .await
    .expect("seed the decoy definition");

    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;

    for (key, name) in [("sprint", "Sprint"), ("risk", "Risk")] {
        let resp = c
            .call(
                methods::HANGAR_PROPERTY_DEFINE,
                serde_json::json!({
                    "workspace_id": WS_SLUG, "key": key, "name": name, "kind": "text",
                }),
            )
            .await;
        assert!(resp["error"].is_null(), "define {key} must ack: {resp}");
    }
    let issue_id = create_issue(&mut c, "Decoy sweep").await;
    for (key, value) in [("sprint", "S2"), ("risk", "high")] {
        let resp = c
            .call(
                methods::HANGAR_ISSUE_PROPERTY_SET,
                serde_json::json!({
                    "workspace_id": WS_SLUG, "issue_id": issue_id,
                    "key": key, "value": value,
                }),
            )
            .await;
        assert!(resp["error"].is_null(), "set {key} must ack: {resp}");
    }

    // ARCHIVE one: it stops RENDERING but the value stays on disk.
    let resp = c
        .call(
            methods::HANGAR_PROPERTY_ARCHIVE,
            serde_json::json!({ "workspace_id": WS_SLUG, "key": "risk", "archived": true }),
        )
        .await;
    assert!(resp["error"].is_null(), "archive must ack: {resp}");
    assert_eq!(resp["result"]["archived"], true, "{resp}");

    let row = detail_row(&mut c, &issue_id, "Decoy sweep").await;
    let names: Vec<&str> = row["properties"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"Sprint"), "the survivor renders: {row}");
    assert!(
        !names.contains(&"Risk"),
        "an ARCHIVED def stops rendering: {row}"
    );
    assert!(
        !names.contains(&"DECOY SPRINT"),
        "another workspace's def never leaks: {row}"
    );
    let raw: String = sqlx::query_scalar("SELECT properties FROM issue WHERE id = ?")
        .bind(&issue_id)
        .fetch_one(store.pool())
        .await
        .expect("read the stored bag");
    let bag: serde_json::Map<String, serde_json::Value> = serde_json::from_str(&raw).unwrap();
    assert_eq!(bag.len(), 2, "archive is NEVER a delete: {raw}");

    // The ACTIVE catalog hides it; `include_archived` brings it back.
    let resp = c
        .call(
            methods::HANGAR_PROPERTIES_LIST,
            serde_json::json!({ "workspace_id": WS_SLUG }),
        )
        .await;
    let active: Vec<&str> = resp["result"]["properties"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["key"].as_str().unwrap())
        .collect();
    assert_eq!(active, vec!["sprint"], "the active catalog: {resp}");
    let resp = c
        .call(
            methods::HANGAR_PROPERTIES_LIST,
            serde_json::json!({ "workspace_id": WS_SLUG, "include_archived": true }),
        )
        .await;
    assert_eq!(
        resp["result"]["properties"].as_array().unwrap().len(),
        2,
        "{resp}"
    );

    // DETAIL-ONLY: a LIST snapshot's rows never carry the key at all, so a
    // pre-#17 client sees a byte-identical row.
    let list = c
        .call(
            methods::HANGAR_ISSUES_LIST,
            serde_json::json!({ "workspace_id": WS_SLUG }),
        )
        .await;
    let listed = list["result"]["issues"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["id"] == issue_id.as_str())
        .expect("the created issue is listed")
        .clone();
    assert!(
        listed.get("properties").is_none(),
        "a list snapshot omits the key entirely: {listed}"
    );
    assert!(
        listed.get("metadata").is_none(),
        "a list snapshot omits the key entirely: {listed}"
    );
}

/// Every addressing / validation rejection is `INVALID_PARAMS`, never a 500,
/// and nothing is written.
#[tokio::test]
async fn property_rejections_are_client_errors_and_write_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, store) = start_server(dir.path(), true).await;
    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;

    let issue_id = create_issue(&mut c, "Rejection sweep").await;
    let resp = c
        .call(
            methods::HANGAR_PROPERTY_DEFINE,
            serde_json::json!({
                "workspace_id": WS_SLUG, "key": "sprint", "name": "Sprint",
                "kind": "select", "options": ["S1", "S2"],
            }),
        )
        .await;
    assert!(resp["error"].is_null(), "define must ack: {resp}");

    // JSON-RPC `INVALID_PARAMS` — a CLIENT error, never a 500.
    let invalid = -32602;
    for (label, params) in [
        (
            "an unknown key",
            serde_json::json!({
                "workspace_id": WS_SLUG, "issue_id": issue_id,
                "key": "nope", "value": "x",
            }),
        ),
        (
            "a value outside the option catalog",
            serde_json::json!({
                "workspace_id": WS_SLUG, "issue_id": issue_id,
                "key": "sprint", "value": "S9",
            }),
        ),
        (
            "an issue id from no workspace",
            serde_json::json!({
                "workspace_id": WS_SLUG, "issue_id": "issue-nowhere",
                "key": "sprint", "value": "S2",
            }),
        ),
    ] {
        let resp = c.call(methods::HANGAR_ISSUE_PROPERTY_SET, params).await;
        assert_eq!(
            resp["error"]["code"], invalid,
            "{label} is INVALID_PARAMS, never a 500: {resp}"
        );
    }
    // A `select` defined with no options is refused at DEFINE time.
    let resp = c
        .call(
            methods::HANGAR_PROPERTY_DEFINE,
            serde_json::json!({ "workspace_id": WS_SLUG, "key": "bad", "kind": "select" }),
        )
        .await;
    assert_eq!(resp["error"]["code"], invalid, "{resp}");
    // …and so is an unknown kind token.
    let resp = c
        .call(
            methods::HANGAR_PROPERTY_DEFINE,
            serde_json::json!({ "workspace_id": WS_SLUG, "key": "bad2", "kind": "hologram" }),
        )
        .await;
    assert_eq!(resp["error"]["code"], invalid, "{resp}");

    let raw: String = sqlx::query_scalar("SELECT properties FROM issue WHERE id = ?")
        .bind(&issue_id)
        .fetch_one(store.pool())
        .await
        .expect("read the stored bag");
    assert_eq!(raw, "{}", "not one rejection wrote a value: {raw}");
}
