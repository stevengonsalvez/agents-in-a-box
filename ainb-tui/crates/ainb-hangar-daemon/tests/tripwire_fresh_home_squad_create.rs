//! The money test: an EMPTY hangar home works end-to-end.
//!
//! Drives the whole fresh-home story over a real framed `UnixStream`, with NO
//! seed fixture:
//!   1. `ensure_default_home` on an empty Store lays down the default workspace,
//!      the host runtime, and exactly one starter agent.
//!   2. The seeded runtime id equals the daemon's default claim runtime id — so a
//!      seeded/created agent binds the runtime the daemon actually claims for.
//!   3. `hangar/agent_create` (no ids supplied) inserts a SECOND agent with every
//!      FK filled.
//!   4. `hangar/squad_create` — which on an empty home used to fail "no agent
//!      available to lead a squad" — now SUCCEEDS with a seeded agent as leader,
//!      and `hangar/squads_list` shows the squad (the gate at plugin.rs:2003 is
//!      cleared).

use std::time::{Duration, Instant};

use ainb_hangar_daemon::rpc::{self, DaemonHealth};
use ainb_hangar_proto::{RpcId, RpcRequest, methods};
use ainb_hangar_store::Store;
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};

const WS_SLUG: &str = "default";

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
            .call(methods::AUTH_HELLO, serde_json::json!({ "token": token.trim() }))
            .await;
        assert!(resp["error"].is_null(), "auth/hello must ack: {resp}");
    }

    /// The active AGENT actor-refs (`agent:<id>`) in `workspace`'s agents_list.
    async fn agent_refs(&mut self, workspace: &str) -> Vec<String> {
        let list = self
            .call(methods::HANGAR_AGENTS_LIST, serde_json::json!({ "workspace_id": workspace }))
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

/// Bind + serve the real listener over a store seeded ONLY by `ensure_default_home`
/// (no fixture) — the fresh-home path.
async fn start_fresh_server(dir: &std::path::Path) -> (std::path::PathBuf, Store) {
    let store = Store::open_in(dir).await.unwrap();
    // The whole point: seed a brand-new home with the boot seed, nothing else.
    ainb_hangar_daemon::default_home::ensure_default_home(store.pool())
        .await
        .expect("fresh-home seed");
    rpc::auth::ensure_socket_token(store.pool(), dir).await.expect("ensure socket token");
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

/// Bind + serve over a BARE store — no seed at all (not even the boot seed) — to
/// prove `agent_create` ensures-then-resolves the default workspace itself.
async fn start_bare_server(dir: &std::path::Path) -> std::path::PathBuf {
    let store = Store::open_in(dir).await.unwrap();
    rpc::auth::ensure_socket_token(store.pool(), dir).await.expect("ensure socket token");
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
    socket_path
}

/// `agent_create` with an empty name is a client error, not an empty insert.
#[tokio::test]
async fn agent_create_rejects_empty_name() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, _store) = start_fresh_server(dir.path()).await;
    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;

    let resp = c.call(methods::HANGAR_AGENT_CREATE, serde_json::json!({ "name": "   " })).await;
    assert!(!resp["error"].is_null(), "an empty name must be rejected: {resp}");
}

/// `agent_create` on a bare home (no workspace row yet) bootstraps the default
/// workspace itself rather than rejecting — the TUI create path ensure-then-resolves.
#[tokio::test]
async fn agent_create_bootstraps_default_workspace_when_absent() {
    let dir = tempfile::tempdir().unwrap();
    let socket_path = start_bare_server(dir.path()).await;
    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;

    let resp = c
        .call(methods::HANGAR_AGENT_CREATE, serde_json::json!({ "name": "solo" }))
        .await;
    assert!(
        resp["error"].is_null(),
        "agent_create must bootstrap the default workspace, not reject: {resp}"
    );
    let refs = c.agent_refs(WS_SLUG).await;
    assert_eq!(refs.len(), 1, "the created agent is in the default workspace: {refs:?}");
}

#[tokio::test]
async fn fresh_home_seeds_then_agent_create_and_squad_create_succeed() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, store) = start_fresh_server(dir.path()).await;

    // (1) The boot seed laid down workspace + runtime + exactly one starter agent.
    let ws_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM workspace")
        .fetch_one(store.pool())
        .await
        .unwrap();
    assert_eq!(ws_count, 1, "one default workspace seeded");
    let agent_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM agent").fetch_one(store.pool()).await.unwrap();
    assert_eq!(agent_count, 1, "exactly one starter agent seeded");

    // (2) The seeded agent binds the SAME runtime the daemon claims for.
    let default_rt = ainb_hangar_store::bootstrap::default_runtime_id();
    let seeded_rt: String = sqlx::query_scalar("SELECT runtime_id FROM agent LIMIT 1")
        .fetch_one(store.pool())
        .await
        .unwrap();
    assert_eq!(
        seeded_rt, default_rt,
        "the seeded agent's runtime must equal the daemon's claim runtime id"
    );
    let rt_online: String =
        sqlx::query_scalar("SELECT status FROM agent_runtime WHERE id = ?")
            .bind(&default_rt)
            .fetch_one(store.pool())
            .await
            .unwrap();
    assert_eq!(rt_online, "online", "the seeded runtime is registered online");

    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;

    // (3) agent_create with NO ids supplied inserts a second agent.
    let resp = c
        .call(
            methods::HANGAR_AGENT_CREATE,
            serde_json::json!({ "name": "reviewer", "provider": "codex" }),
        )
        .await;
    assert!(resp["error"].is_null(), "agent_create must ack: {resp}");
    let refs = c.agent_refs(WS_SLUG).await;
    assert_eq!(refs.len(), 2, "two agents after create: {refs:?}");

    // The recorded provider persisted onto the created agent's row.
    let codex_provider: Option<String> =
        sqlx::query_scalar("SELECT provider FROM agent WHERE name = ?")
            .bind("reviewer")
            .fetch_one(store.pool())
            .await
            .unwrap();
    assert_eq!(codex_provider.as_deref(), Some("codex"), "provider recorded on the row");

    // (4) squad_create with a seeded agent leader SUCCEEDS (the gate is cleared).
    let leader = refs.first().expect("at least one agent to lead").clone();
    let resp = c
        .call(
            methods::HANGAR_SQUAD_CREATE,
            serde_json::json!({ "workspace_id": WS_SLUG, "name": "alpha", "leader": leader }),
        )
        .await;
    assert!(
        resp["error"].is_null(),
        "squad_create on a fresh home must SUCCEED (the 'no agent available' gate is cleared): {resp}"
    );

    // squads_list shows the new squad.
    let squads = c
        .call(methods::HANGAR_SQUADS_LIST, serde_json::json!({ "workspace_id": WS_SLUG }))
        .await;
    let names: Vec<String> = squads["result"]["squads"]
        .as_array()
        .expect("squads array")
        .iter()
        .map(|s| s["name"].as_str().unwrap().to_string())
        .collect();
    assert!(names.contains(&"alpha".to_string()), "the created squad is listed: {names:?}");
}
