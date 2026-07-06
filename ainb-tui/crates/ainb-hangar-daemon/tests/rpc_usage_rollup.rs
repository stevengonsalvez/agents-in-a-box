//! Integration: the usage-dashboard rollup over a real framed `UnixStream`
//! (e38.35).
//!
//! This is the user-visible proof leg for the bead. It drives the WHOLE
//! query path the way `boot()` wires it:
//!
//! 1. start the real RPC server over a seeded store;
//! 2. record `task_usage` rows for two agents directly (the durable aggregate the
//!    daemon's run loop writes at each task's finalize seam);
//! 3. assert over the socket that `hangar/usage_rollup` returns the correct grand
//!    totals (summed tokens in/out + cost + run count) AND the per-agent
//!    breakdown (grouped by agent, heaviest cost first);
//! 4. mutate one usage row, re-call, and assert the totals + that agent's row
//!    changed — proving the rollup reads the LIVE table, not a cached snapshot;
//! 5. assert an unknown workspace yields all-zero totals + an empty rollup.
//!
//! The rollup is therefore proven to TAKE EFFECT: it reads real persisted usage,
//! and mutating a row is reflected in the next call. Mutating the query into a
//! constant (ignoring the table) breaks `rollup_reflects_a_mutated_usage_row`.

use std::time::{Duration, Instant};

use ainb_hangar_daemon::rpc::{self, DaemonHealth};
use ainb_hangar_daemon::seed::{self, WS_ID, WS_SLUG};
use ainb_hangar_proto::{RpcId, RpcRequest, methods};
use ainb_hangar_store::Store;
use ainb_hangar_store::repo::usage::{NewUsage, UsageRepo};
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

    /// Call `hangar/usage_rollup` for `ws`, returning the `result` object.
    async fn usage_rollup(&mut self, ws: &str) -> serde_json::Value {
        let resp = self
            .call(
                methods::HANGAR_USAGE_ROLLUP,
                serde_json::json!({ "workspace_id": ws }),
            )
            .await;
        assert!(resp["error"].is_null(), "usage_rollup must ack: {resp}");
        resp["result"].clone()
    }
}

/// Bind + serve the real listener over a seeded store, returning the socket path
/// and the live store (mirrors `boot()`'s wiring). The socket-auth token is
/// minted before the bind, exactly like production.
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

/// Seed a second agent + the extra `done` tasks the usage rows FK to. The fixture
/// already has `agent-1` + `task-1`; this adds `agent-2` + `task-2`/`task-3` so
/// the rollup can prove a multi-agent GROUP BY.
async fn seed_extra_agents_and_tasks(store: &Store) {
    let pool = store.pool();
    sqlx::query(
        "INSERT INTO agent (id, workspace_id, name, runtime_id, visibility, owner_id) \
         VALUES ('agent-2', ?, 'codex-agent', 'runtime-1', 'workspace', 'user-1')",
    )
    .bind(WS_ID)
    .execute(pool)
    .await
    .expect("insert agent-2");
    for (task, agent) in [("task-2", "agent-1"), ("task-3", "agent-2")] {
        sqlx::query(
            "INSERT INTO agent_task_queue \
             (id, workspace_id, runtime_id, agent_id, status, created_at) \
             VALUES (?, ?, 'runtime-1', ?, 'done', 0)",
        )
        .bind(task)
        .bind(WS_ID)
        .bind(agent)
        .execute(pool)
        .await
        .unwrap_or_else(|e| panic!("insert {task}: {e}"));
    }
}

fn usage(task: &str, agent: &str, tin: i64, tout: i64, cost: f64) -> NewUsage {
    NewUsage {
        task_id: task.into(),
        workspace_id: WS_ID.into(),
        agent_id: agent.into(),
        input_tokens: tin,
        output_tokens: tout,
        cost_usd: cost,
        created_at: 100,
    }
}

/// `hangar/usage_rollup` returns the correct grand totals + per-agent breakdown
/// for the seeded usage rows (sum + group-by, heaviest cost first).
#[tokio::test]
async fn usage_rollup_returns_totals_and_per_agent_breakdown() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, store) = start_server(dir.path()).await;
    seed_extra_agents_and_tasks(&store).await;

    // agent-1: two runs (task-1 + task-2); agent-2: one run (task-3).
    UsageRepo::record(store.pool(), &usage("task-1", "agent-1", 1000, 200, 0.01))
        .await
        .unwrap();
    UsageRepo::record(store.pool(), &usage("task-2", "agent-1", 500, 100, 0.005))
        .await
        .unwrap();
    UsageRepo::record(store.pool(), &usage("task-3", "agent-2", 2000, 800, 0.04))
        .await
        .unwrap();

    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;

    // The plugin subscribes by slug; the daemon resolves it to WS_ID.
    let result = c.usage_rollup(WS_SLUG).await;

    // Grand totals sum every run.
    assert_eq!(
        result["total_input_tokens"].as_i64(),
        Some(3500),
        "1000+500+2000"
    );
    assert_eq!(
        result["total_output_tokens"].as_i64(),
        Some(1100),
        "200+100+800"
    );
    assert!((result["total_cost_usd"].as_f64().unwrap() - 0.055).abs() < 1e-9);
    assert_eq!(result["total_runs"].as_i64(), Some(3));

    // Per-agent breakdown, heaviest cost first: agent-2 (0.04) before agent-1 (0.015).
    let agents = result["agents"].as_array().expect("agents array");
    assert_eq!(agents.len(), 2, "two agents in the rollup: {agents:?}");
    assert_eq!(agents[0]["agent_id"].as_str(), Some("agent-2"));
    assert_eq!(agents[0]["input_tokens"].as_i64(), Some(2000));
    assert_eq!(agents[0]["runs"].as_i64(), Some(1));
    assert_eq!(agents[1]["agent_id"].as_str(), Some("agent-1"));
    assert_eq!(
        agents[1]["input_tokens"].as_i64(),
        Some(1500),
        "1000+500 grouped"
    );
    assert_eq!(
        agents[1]["output_tokens"].as_i64(),
        Some(300),
        "200+100 grouped"
    );
    assert_eq!(agents[1]["runs"].as_i64(), Some(2));
}

/// Mutating a persisted usage row is reflected in the next rollup call — proving
/// the RPC reads the LIVE table, not a constant or a cached snapshot.
#[tokio::test]
async fn rollup_reflects_a_mutated_usage_row() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, store) = start_server(dir.path()).await;
    seed_extra_agents_and_tasks(&store).await;

    UsageRepo::record(store.pool(), &usage("task-1", "agent-1", 1000, 200, 0.01))
        .await
        .unwrap();

    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;

    let before = c.usage_rollup(WS_SLUG).await;
    assert_eq!(before["total_input_tokens"].as_i64(), Some(1000));
    assert!((before["total_cost_usd"].as_f64().unwrap() - 0.01).abs() < 1e-9);

    // Re-record the SAME task with bigger usage (a retry that spent more). The
    // upsert replaces the row in place; the rollup must see the new figure.
    UsageRepo::record(store.pool(), &usage("task-1", "agent-1", 9000, 1500, 0.5))
        .await
        .unwrap();

    let after = c.usage_rollup(WS_SLUG).await;
    assert_eq!(
        after["total_input_tokens"].as_i64(),
        Some(9000),
        "the mutated row's new tokens are rolled up"
    );
    assert!((after["total_cost_usd"].as_f64().unwrap() - 0.5).abs() < 1e-9);
    assert_eq!(
        after["total_runs"].as_i64(),
        Some(1),
        "still one run (upsert, not a second row)"
    );
}

/// An unknown workspace yields all-zero totals + an empty rollup (a read — never
/// an error, mirroring the other snapshot RPCs).
#[tokio::test]
async fn usage_rollup_unknown_workspace_is_empty() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, _store) = start_server(dir.path()).await;

    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;

    let result = c.usage_rollup("ws-does-not-exist").await;
    assert_eq!(result["total_input_tokens"].as_i64(), Some(0));
    assert_eq!(result["total_output_tokens"].as_i64(), Some(0));
    assert_eq!(result["total_cost_usd"].as_f64(), Some(0.0));
    assert_eq!(result["total_runs"].as_i64(), Some(0));
    assert_eq!(result["agents"].as_array().map(Vec::len), Some(0));
}
