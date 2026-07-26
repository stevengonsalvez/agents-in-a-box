//! Integration: the `hangar/agent_update` + `hangar/agent_archive` RPCs edit an
//! agent over a real framed `UnixStream`, are workspace-scoped, and the change is
//! observable in a follow-up `hangar/agents_list` snapshot (e38.15).
//!
//! The seed fixture lays down one agent (`agent-1`, `claude-agent`) in the seeded
//! workspace; here a connection authenticates, drives a config edit + an archive
//! through the dispatcher, then asserts:
//!   (a) `agent_update` acks with the refreshed actor row and the rename
//!       persists into `agents_list`,
//!   (b) `agent_archive` acks and removes the agent from the active `agents_list`
//!       (the archived agent is hidden from the picker),
//!   (c) un-archiving restores it,
//!   (d) both mutations aimed at a *foreign* workspace are rejected with an error
//!       and touch no row (workspace isolation).

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

/// An authenticated connection edits a seeded agent's config: the response
/// carries the refreshed actor row, and a follow-up snapshot shows the rename.
#[tokio::test]
async fn agent_update_persists_config_change() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, _store) = start_server(dir.path()).await;

    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;

    // Rename agent-1 and set model + a CLI arg + an env var.
    let resp = c
        .call(
            methods::HANGAR_AGENT_UPDATE,
            serde_json::json!({
                "workspace_id": WS_SLUG,
                "agent_id": "agent-1",
                "name": "claude-pro",
                "model": "claude-opus-4",
                "cli_args": ["--verbose"],
                "agent_env": [["FOO", "bar"]],
            }),
        )
        .await;
    assert!(resp["error"].is_null(), "update must ack: {resp}");
    // The response carries the refreshed actor row (the rename is visible).
    let row = &resp["result"];
    assert_eq!(row["actor_ref"], "agent:agent-1");
    assert_eq!(row["display_name"], "claude-pro");
    assert_eq!(row["is_agent"], true);

    // The rename persisted: a fresh agents_list snapshot shows the new name.
    let list = c
        .call(
            methods::HANGAR_AGENTS_LIST,
            serde_json::json!({ "workspace_id": WS_SLUG }),
        )
        .await;
    let agent = list["result"]["actors"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["actor_ref"] == "agent:agent-1")
        .expect("agent-1 present");
    assert_eq!(agent["display_name"], "claude-pro", "rename persisted");
}

/// Archiving an agent removes it from the active `agents_list`; un-archiving
/// restores it — the user-visible "hide from picker" behaviour.
#[tokio::test]
async fn agent_archive_hides_then_restores_in_active_list() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, _store) = start_server(dir.path()).await;

    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;

    // The agent is in the active list before archiving.
    let before = c.active_agent_ids(WS_SLUG).await;
    assert!(
        before.contains(&"agent:agent-1".to_string()),
        "active list shows agent-1 before archive: {before:?}"
    );

    // Archive it.
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

    // The active list no longer shows the archived agent.
    let after = c.active_agent_ids(WS_SLUG).await;
    assert!(
        !after.contains(&"agent:agent-1".to_string()),
        "archived agent must be hidden from the active list: {after:?}"
    );

    // Un-archive restores it to the active list.
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
    assert!(resp["error"].is_null(), "un-archive must ack: {resp}");
    let restored = c.active_agent_ids(WS_SLUG).await;
    assert!(
        restored.contains(&"agent:agent-1".to_string()),
        "un-archived agent returns to the active list: {restored:?}"
    );
}

/// Both mutations aimed at a foreign / unknown workspace are rejected with an
/// error — never a silent no-op — and the agent is untouched (workspace isolation).
#[tokio::test]
async fn agent_mutations_reject_unknown_workspace_and_touch_no_row() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, _store) = start_server(dir.path()).await;

    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;

    // A foreign workspace must reject the config edit.
    let resp = c
        .call(
            methods::HANGAR_AGENT_UPDATE,
            serde_json::json!({
                "workspace_id": "nope-not-a-workspace",
                "agent_id": "agent-1",
                "name": "hijacked",
            }),
        )
        .await;
    assert!(
        !resp["error"].is_null(),
        "an unknown workspace must reject the update, not silently no-op: {resp}"
    );

    // A foreign workspace must reject the archive.
    let resp = c
        .call(
            methods::HANGAR_AGENT_ARCHIVE,
            serde_json::json!({
                "workspace_id": "nope-not-a-workspace",
                "agent_id": "agent-1",
                "archived": true,
            }),
        )
        .await;
    assert!(
        !resp["error"].is_null(),
        "an unknown workspace must reject the archive: {resp}"
    );

    // agent-1 is untouched: still active under its real workspace with its name.
    let list = c
        .call(
            methods::HANGAR_AGENTS_LIST,
            serde_json::json!({ "workspace_id": WS_SLUG }),
        )
        .await;
    let agent = list["result"]["actors"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["actor_ref"] == "agent:agent-1")
        .expect("agent-1 still active");
    assert_eq!(
        agent["display_name"], "claude-agent",
        "rejected edit must not rename the agent"
    );
}

/// An agent edit with no field flags is a client error, not an empty write.
#[tokio::test]
async fn agent_update_with_no_fields_is_rejected() {
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
            }),
        )
        .await;
    assert!(
        !resp["error"].is_null(),
        "an empty edit must be rejected: {resp}"
    );
}

/// Parity #30 — the acceptance the RPC layer owns: a per-agent env VALUE never
/// reaches the wire, in either the write response or a later list snapshot.
///
/// The response instead carries the multica-shaped metadata (key names, a count,
/// and the `redacted` flag), which is everything a UI needs and nothing an
/// attacker does. This test FAILS on `main` — before the change the response
/// simply had no `agent_env_*` keys and the daemon had no redaction concept.
#[tokio::test]
async fn agent_env_value_never_reaches_the_wire() {
    const SECRET: &str = "sk-live-DEADBEEF01";

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
                "agent_env": [["SECRET_TOKEN", SECRET]],
            }),
        )
        .await;
    assert!(resp["error"].is_null(), "update must ack: {resp}");

    let body = resp.to_string();
    assert!(!body.contains("sk-live-"), "the update response leaked the env value: {body}");
    let row = &resp["result"];
    assert_eq!(row["agent_env_key_count"], 1, "{resp}");
    assert_eq!(row["agent_env_keys"], serde_json::json!(["SECRET_TOKEN"]), "{resp}");
    assert_eq!(row["agent_env_redacted"], true, "{resp}");

    // Same contract on the list snapshot — the surface every TUI actually reads.
    let list = c
        .call(
            methods::HANGAR_AGENTS_LIST,
            serde_json::json!({ "workspace_id": WS_SLUG }),
        )
        .await;
    let body = list.to_string();
    assert!(!body.contains("sk-live-"), "agents_list leaked the env value: {body}");
    let agent = list["result"]["actors"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["actor_ref"] == "agent:agent-1")
        .expect("agent-1 present");
    assert_eq!(agent["agent_env_key_count"], 1, "{list}");
    assert_eq!(agent["agent_env_keys"], serde_json::json!(["SECRET_TOKEN"]), "{list}");
    assert_eq!(agent["agent_env_redacted"], true, "{list}");
}

/// Parity #30 — a malformed `agent_env` is rejected with a CONTENT-FREE message.
///
/// `serde_json` quotes the offending scalar (`invalid type: integer `31337`,
/// expected a string`), which for this field IS the secret, and the error text
/// lands in the caller's logs. Multica drops the underlying error for exactly
/// this reason (`cmd_agent.go:757-759`).
#[tokio::test]
async fn malformed_agent_env_error_echoes_neither_key_nor_value() {
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
                "agent_env": [["SECRET_TOKEN", 31337]],
            }),
        )
        .await;
    assert!(!resp["error"].is_null(), "a non-string env value must be rejected: {resp}");
    let message = resp["error"]["message"].as_str().unwrap_or_default();
    assert!(!message.contains("31337"), "the error echoed the value: {message}");
    assert!(!message.contains("SECRET_TOKEN"), "the error echoed the key: {message}");
    assert!(message.starts_with("expected "), "the message still names the shape: {message}");
}
