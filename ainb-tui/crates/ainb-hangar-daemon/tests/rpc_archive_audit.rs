//! Integration: the archive AUDIT trail over the real framed dispatcher
//! (`hangar/agent_archive`, `hangar/squad_archive` — parity #26, migration 0052).
//!
//! What this proves end-to-end, over a real `UnixStream`, that no unit test can:
//!
//! 1. an archive with NO `archived_by_user_id` is attributed to the WORKSPACE
//!    OWNER and stamped with the daemon's clock — in the database, not just the
//!    response;
//! 2. an explicit `archived_by_user_id` wins over that default;
//! 3. un-archiving CLEARS both columns and the response OMITS both keys (the
//!    append-only skip-if-unset wire contract);
//! 4. a LEGACY client payload `{workspace_id, agent_id, archived}` still parses;
//! 5. `hangar/squad_archive` round-trips: the archived squad disappears from
//!    `hangar/squads_list` and the database carries who + when.

use std::time::{Duration, Instant};

use ainb_hangar_daemon::rpc::{self, DaemonHealth};
use ainb_hangar_daemon::seed::{self, WS_SLUG};
use ainb_hangar_proto::{RpcId, RpcRequest, methods};
use ainb_hangar_store::Store;
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};

/// The seeded workspace's OWNER (`seed::seed_p4_fixture`) — the default archiver.
const SEED_OWNER: &str = "user-1";

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

/// Read the agent's stored `(archived, archived_at, archived_by)` triple — the
/// DATABASE truth, not the response echo.
async fn agent_audit(store: &Store, id: &str) -> (i64, Option<i64>, Option<String>) {
    sqlx::query_as("SELECT archived, archived_at, archived_by FROM agent WHERE id = ?")
        .bind(id)
        .fetch_one(store.pool())
        .await
        .expect("read agent audit")
}

async fn squad_audit(store: &Store, id: &str) -> (i64, Option<i64>, Option<String>) {
    sqlx::query_as("SELECT archived, archived_at, archived_by FROM squad WHERE id = ?")
        .bind(id)
        .fetch_one(store.pool())
        .await
        .expect("read squad audit")
}

/// An archive with no explicit archiver is attributed to the workspace OWNER and
/// stamped with the daemon's clock; the acked `ActorRow` carries both. Restoring
/// clears the columns AND omits the keys from the response.
#[tokio::test]
async fn agent_archive_defaults_the_archiver_to_the_workspace_owner() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, store) = start_server(dir.path()).await;
    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;

    let before_ms = now_ms();
    let resp = c
        .call(
            methods::HANGAR_AGENT_ARCHIVE,
            serde_json::json!({
                "workspace_id": WS_SLUG,
                "agent_id": "agent-1",
                "archived": true,
            }),
        )
        .await;
    assert!(resp["error"].is_null(), "archive must ack: {resp}");
    let after_ms = now_ms();

    // (a) The DATABASE carries the audit.
    let (archived, at, by) = agent_audit(&store, "agent-1").await;
    assert_eq!(archived, 1);
    assert_eq!(
        by.as_deref(),
        Some(&format!("member:{SEED_OWNER}")[..]),
        "an unattributed archive defaults to the workspace owner"
    );
    let at = at.expect("archived_at stamped");
    assert!(
        (before_ms..=after_ms).contains(&at),
        "archived_at {at} must be the daemon's clock reading, in [{before_ms}, {after_ms}]"
    );

    // (b) The acked ActorRow proves the audit on the WIRE.
    assert_eq!(resp["result"]["archived_at"], serde_json::json!(at));
    assert_eq!(
        resp["result"]["archived_by"],
        serde_json::json!(format!("member:{SEED_OWNER}"))
    );

    // (c) Restoring clears both columns AND omits both keys from the response —
    //     the skip-if-unset half of the append-only wire contract.
    let resp = c
        .call(
            methods::HANGAR_AGENT_ARCHIVE,
            serde_json::json!({
                "workspace_id": WS_SLUG,
                "agent_id": "agent-1",
                "archived": false,
            }),
        )
        .await;
    assert!(resp["error"].is_null(), "unarchive must ack: {resp}");
    assert!(
        resp["result"].get("archived_at").is_none()
            && resp["result"].get("archived_by").is_none(),
        "a restored agent's row must omit the audit keys: {}",
        resp["result"]
    );
    assert_eq!(agent_audit(&store, "agent-1").await, (0, None, None));
}

/// An explicit `archived_by_user_id` overrides the owner default, and a LEGACY
/// payload without the field still parses (the serde-default proof).
#[tokio::test]
async fn agent_archive_honours_an_explicit_archiver_and_a_legacy_payload() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, store) = start_server(dir.path()).await;
    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;

    let resp = c
        .call(
            methods::HANGAR_AGENT_ARCHIVE,
            serde_json::json!({
                "workspace_id": WS_SLUG,
                "agent_id": "agent-1",
                "archived": true,
                "archived_by_user_id": "user-2",
            }),
        )
        .await;
    assert!(resp["error"].is_null(), "archive must ack: {resp}");
    let (_, _, by) = agent_audit(&store, "agent-1").await;
    assert_eq!(
        by.as_deref(),
        Some("member:user-2"),
        "an explicit archiver wins over the owner default"
    );

    // A pre-0052 client payload — no `archived_by_user_id` key at all — still
    // parses and falls back to the owner.
    let resp = c
        .call(
            methods::HANGAR_AGENT_ARCHIVE,
            serde_json::json!({
                "workspace_id": WS_SLUG,
                "agent_id": "agent-1",
                "archived": true,
            }),
        )
        .await;
    assert!(resp["error"].is_null(), "legacy payload must parse: {resp}");
    let (_, _, by) = agent_audit(&store, "agent-1").await;
    assert_eq!(
        by.as_deref(),
        Some(&format!("member:{SEED_OWNER}")[..]),
        "re-archiving re-stamps: last archiver wins"
    );
}

/// `hangar/squad_archive` round-trip: the archived squad leaves `squads_list`,
/// the database carries who + when, and restoring puts it back.
#[tokio::test]
async fn squad_archive_hides_the_squad_and_records_the_audit() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, store) = start_server(dir.path()).await;
    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;

    // Create a squad through the RPC surface, then find its id.
    let created = c
        .call(
            methods::HANGAR_SQUAD_CREATE,
            serde_json::json!({
                "workspace_id": WS_SLUG,
                "name": "qa",
                "leader": "agent:agent-1",
            }),
        )
        .await;
    assert!(created["error"].is_null(), "create must ack: {created}");
    let squad_id = created["result"]["squads"]
        .as_array()
        .expect("squads array")
        .iter()
        .find(|s| s["name"] == "qa")
        .expect("the new squad is in the refreshed list")["id"]
        .as_str()
        .expect("squad id")
        .to_string();

    let before_ms = now_ms();
    let resp = c
        .call(
            methods::HANGAR_SQUAD_ARCHIVE,
            serde_json::json!({
                "workspace_id": WS_SLUG,
                "squad_id": squad_id,
                "archived": true,
                "archived_by_user_id": SEED_OWNER,
            }),
        )
        .await;
    assert!(resp["error"].is_null(), "squad archive must ack: {resp}");
    let after_ms = now_ms();

    // The refreshed list in the RESPONSE is already active-only.
    let names: Vec<&str> = resp["result"]["squads"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["name"].as_str().unwrap())
        .collect();
    assert!(
        !names.contains(&"qa"),
        "the archived squad must leave the response list: {names:?}"
    );

    // …and a fresh squads_list snapshot agrees.
    let list = c
        .call(
            methods::HANGAR_SQUADS_LIST,
            serde_json::json!({ "workspace_id": WS_SLUG }),
        )
        .await;
    assert!(
        !list["result"]["squads"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["id"] == squad_id.as_str()),
        "the archived squad must leave squads_list: {}",
        list["result"]
    );

    // The DATABASE carries who + when.
    let (archived, at, by) = squad_audit(&store, &squad_id).await;
    assert_eq!(archived, 1);
    assert_eq!(by.as_deref(), Some(&format!("member:{SEED_OWNER}")[..]));
    let at = at.expect("archived_at stamped");
    assert!((before_ms..=after_ms).contains(&at), "stamp {at} in range");

    // Restoring returns it to the list and clears the audit pair.
    let resp = c
        .call(
            methods::HANGAR_SQUAD_ARCHIVE,
            serde_json::json!({
                "workspace_id": WS_SLUG,
                "squad_id": squad_id,
                "archived": false,
            }),
        )
        .await;
    assert!(resp["error"].is_null(), "restore must ack: {resp}");
    assert!(
        resp["result"]["squads"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["id"] == squad_id.as_str()),
        "a restored squad returns to the list"
    );
    assert_eq!(squad_audit(&store, &squad_id).await, (0, None, None));
}

/// An unknown / foreign squad id is a client error, never a silent no-op.
#[tokio::test]
async fn squad_archive_rejects_an_unknown_squad() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, _store) = start_server(dir.path()).await;
    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;

    let resp = c
        .call(
            methods::HANGAR_SQUAD_ARCHIVE,
            serde_json::json!({
                "workspace_id": WS_SLUG,
                "squad_id": "no-such-squad",
                "archived": true,
            }),
        )
        .await;
    assert!(
        !resp["error"].is_null(),
        "an unknown squad must be rejected: {resp}"
    );
    assert!(
        resp["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("no-such-squad"),
        "the error must name the squad: {resp}"
    );
}

/// Wall-clock epoch ms, used to bracket the daemon's own stamp.
fn now_ms() -> i64 {
    i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap()
}
