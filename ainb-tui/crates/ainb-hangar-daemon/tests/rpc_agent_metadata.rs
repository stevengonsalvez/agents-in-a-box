//! Integration: the migration-0050 agent metadata surface over a real framed
//! `UnixStream` (multica gap #23).
//!
//! Drives the REAL dispatcher (not the store) and asserts the user-visible
//! contract:
//!   (a) `hangar/agent_create` with a `description` acks, and the following
//!       `agents_list` row carries that description AND a non-empty avatar,
//!   (b) a SECOND create with the same name is REFUSED with
//!       `data.reason == "duplicate_name"` and a message naming the agent — and
//!       the roster still holds exactly one agent by that name,
//!   (c) a 256-CHARACTER description is `INVALID_PARAMS` and writes nothing,
//!   (d) renaming agent B onto agent A's name is refused the same way, and B
//!       keeps its old name.

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

    /// Snapshot the active agent ids in `workspace`'s `agents_list` (agents only,
    /// not human members).
    async fn active_agent_ids(&mut self, workspace: &str) -> Vec<String> {
        let list = self
            .call(
                methods::HANGAR_AGENTS_LIST,
                serde_json::json!({ "workspace_id": workspace }),
            )
            .await;
        list["result"]["actors"]
            .as_array()
            .expect("actors array")
            .iter()
            .filter(|a| a["is_agent"] == true)
            .map(|a| a["actor_ref"].as_str().unwrap().to_string())
            .collect()
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

/// The `agents_list` row for `actor_ref`, or `None` when absent.
async fn actor_row(c: &mut Client, actor_ref: &str) -> Option<serde_json::Value> {
    let list = c
        .call(
            methods::HANGAR_AGENTS_LIST,
            serde_json::json!({ "workspace_id": WS_SLUG }),
        )
        .await;
    list["result"]["actors"]
        .as_array()
        .expect("actors array")
        .iter()
        .find(|a| a["actor_ref"] == actor_ref)
        .cloned()
}

/// How many agents in the roster carry `name` as their display name.
async fn count_named(c: &mut Client, name: &str) -> usize {
    let list = c
        .call(
            methods::HANGAR_AGENTS_LIST,
            serde_json::json!({ "workspace_id": WS_SLUG }),
        )
        .await;
    list["result"]["actors"]
        .as_array()
        .expect("actors array")
        .iter()
        .filter(|a| a["is_agent"] == true && a["display_name"] == name)
        .count()
}

/// (a) A create carrying a `description` acks, and the roster row the client
/// renders carries BOTH the description and a minted avatar.
#[tokio::test]
async fn agent_create_persists_description_and_mints_an_avatar() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, _store) = start_server(dir.path()).await;
    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;

    let resp = c
        .call(
            methods::HANGAR_AGENT_CREATE,
            serde_json::json!({
                "workspace_id": WS_SLUG,
                "name": "builder",
                "description": "ships the backend",
            }),
        )
        .await;
    assert!(resp["error"].is_null(), "create must ack: {resp}");

    let row = resp["result"]["actors"]
        .as_array()
        .expect("the create answers with the refreshed roster")
        .iter()
        .find(|a| a["display_name"] == "builder")
        .cloned()
        .expect("the new agent is in the answered roster");
    assert_eq!(
        row["description"], "ships the backend",
        "the create reply's row carries the blurb: {row}"
    );
    let avatar = row["avatar"].as_str().unwrap_or_default();
    assert!(
        avatar.starts_with("emoji:"),
        "an agent is never avatar-less, got {avatar:?}"
    );

    // And it survives into an independently-fetched snapshot, not just the reply.
    let actor_ref = row["actor_ref"].as_str().unwrap().to_string();
    let fresh = actor_row(&mut c, &actor_ref).await.expect("agent in agents_list");
    assert_eq!(fresh["description"], "ships the backend");
    assert_eq!(
        fresh["avatar"],
        serde_json::Value::String(avatar.to_string())
    );
}

/// (b) A SECOND create with the same name is refused with the machine-readable
/// `duplicate_name` marker, and the roster still holds exactly one such agent.
#[tokio::test]
async fn duplicate_agent_create_is_refused_with_a_reason_marker() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, _store) = start_server(dir.path()).await;
    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;

    let create = |name: &str| serde_json::json!({ "workspace_id": WS_SLUG, "name": name });
    let first = c.call(methods::HANGAR_AGENT_CREATE, create("builder")).await;
    assert!(first["error"].is_null(), "the first create acks: {first}");

    let second = c.call(methods::HANGAR_AGENT_CREATE, create("builder")).await;
    let err = &second["error"];
    assert!(!err.is_null(), "a duplicate name must be refused: {second}");
    assert_eq!(
        err["data"]["reason"], "duplicate_name",
        "the refusal carries the machine-readable marker: {err}"
    );
    let msg = err["message"].as_str().unwrap_or_default();
    assert!(
        msg.contains("builder") && msg.contains("already exists"),
        "the message names the agent: {msg:?}"
    );

    assert_eq!(
        count_named(&mut c, "builder").await,
        1,
        "the refused create wrote no second row"
    );
}

/// (c) An over-long description is `INVALID_PARAMS` and writes nothing; the
/// boundary (exactly 255 characters) is accepted.
#[tokio::test]
async fn over_long_description_is_rejected_at_the_boundary() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, _store) = start_server(dir.path()).await;
    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;

    let too_long = "x".repeat(256);
    let resp = c
        .call(
            methods::HANGAR_AGENT_CREATE,
            serde_json::json!({
                "workspace_id": WS_SLUG,
                "name": "toolong",
                "description": too_long,
            }),
        )
        .await;
    let err = &resp["error"];
    assert!(
        !err.is_null(),
        "a 256-char description must be refused: {resp}"
    );
    assert_eq!(err["code"], -32602, "the refusal is INVALID_PARAMS: {err}");
    assert!(
        err["message"].as_str().unwrap_or_default().contains("255 characters or fewer"),
        "the message states the cap: {err}"
    );
    assert_eq!(
        count_named(&mut c, "toolong").await,
        0,
        "the rejected create wrote no agent"
    );

    // Exactly 255 is fine — the cap is inclusive.
    let ok = "y".repeat(255);
    let resp = c
        .call(
            methods::HANGAR_AGENT_CREATE,
            serde_json::json!({
                "workspace_id": WS_SLUG,
                "name": "justright",
                "description": ok,
            }),
        )
        .await;
    assert!(
        resp["error"].is_null(),
        "255 characters is accepted: {resp}"
    );
}

/// (d) Renaming agent B onto agent A's name is refused the same way, and B
/// keeps its old name (no partial write).
#[tokio::test]
async fn renaming_onto_a_taken_name_is_refused_and_leaves_the_row() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, _store) = start_server(dir.path()).await;
    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;

    // The seed lays down `claude-agent` (agent-1); add a second agent `beta`.
    let created = c
        .call(
            methods::HANGAR_AGENT_CREATE,
            serde_json::json!({ "workspace_id": WS_SLUG, "name": "beta" }),
        )
        .await;
    assert!(created["error"].is_null(), "create beta acks: {created}");
    let beta_ref = created["result"]["actors"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["display_name"] == "beta")
        .expect("beta in the roster")["actor_ref"]
        .as_str()
        .unwrap()
        .to_string();
    let beta_id = beta_ref.strip_prefix("agent:").unwrap().to_string();

    let resp = c
        .call(
            methods::HANGAR_AGENT_UPDATE,
            serde_json::json!({
                "workspace_id": WS_SLUG,
                "agent_id": beta_id,
                "name": "claude-agent",
            }),
        )
        .await;
    let err = &resp["error"];
    assert!(!err.is_null(), "a colliding rename must be refused: {resp}");
    assert_eq!(err["data"]["reason"], "duplicate_name", "{err}");

    let row = actor_row(&mut c, &beta_ref).await.expect("beta still exists");
    assert_eq!(
        row["display_name"], "beta",
        "the refused rename left the row untouched"
    );
}

/// An `agent_update` may edit the description alone, and the refreshed row the
/// mutation answers with carries the new blurb (not just a later list).
#[tokio::test]
async fn agent_update_edits_the_description() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, _store) = start_server(dir.path()).await;
    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;

    let resp = c
        .call(
            methods::HANGAR_AGENT_UPDATE,
            serde_json::json!({
                "workspace_id": WS_SLUG,
                "agent_id": "agent-1",
                "description": "reviews every PR",
            }),
        )
        .await;
    assert!(
        resp["error"].is_null(),
        "a description-only edit acks: {resp}"
    );
    assert_eq!(resp["result"]["description"], "reviews every PR");

    let row = actor_row(&mut c, "agent:agent-1").await.expect("agent-1 present");
    assert_eq!(row["description"], "reviews every PR", "the edit persisted");
}
