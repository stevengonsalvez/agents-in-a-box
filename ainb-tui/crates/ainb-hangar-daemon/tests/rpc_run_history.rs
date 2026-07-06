//! Integration: the run-history timeline over a real framed `UnixStream`
//! (P10 / D19).
//!
//! The user-visible proof leg for the observability history RPC. It drives the
//! WHOLE query path the way `boot()` wires it:
//!
//! 1. start the real RPC server over a seeded store;
//! 2. append `run_history` rows directly (the durable timeline the daemon's run
//!    loop writes at each finalize seam);
//! 3. assert over the socket that `hangar/run_history` returns the rows
//!    newest-finished-first, carrying provider / outcome / token-cost;
//! 4. assert the `limit` param caps the returned rows (newest kept);
//! 5. assert an unknown workspace yields an empty timeline.
//!
//! The timeline is therefore proven to TAKE EFFECT: it reads real persisted runs,
//! and the ordering + cap are enforced by the daemon, not the client. Mutating the
//! query into a constant breaks `run_history_orders_newest_first`.

use std::time::{Duration, Instant};

use ainb_hangar_daemon::rpc::{self, DaemonHealth};
use ainb_hangar_daemon::seed::{self, WS_ID, WS_SLUG};
use ainb_hangar_proto::{RpcId, RpcRequest, methods};
use ainb_hangar_store::Store;
use ainb_hangar_store::repo::run_history::{NewRunHistory, RunHistoryRepo};
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

    /// Call `hangar/run_history` for `ws` (with an optional `limit`), returning the
    /// `runs` array.
    async fn run_history(&mut self, ws: &str, limit: Option<i64>) -> Vec<serde_json::Value> {
        let mut params = serde_json::json!({ "workspace_id": ws });
        if let Some(l) = limit {
            params["limit"] = serde_json::json!(l);
        }
        let resp = self.call(methods::HANGAR_RUN_HISTORY, params).await;
        assert!(resp["error"].is_null(), "run_history must ack: {resp}");
        resp["result"]["runs"].as_array().cloned().unwrap_or_default()
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

/// A task-less run-history row (NULL task_id) for `ws`, finishing at `finished`.
fn run(run_id: &str, provider: &str, finished: i64, outcome: &str, cost: f64) -> NewRunHistory {
    NewRunHistory {
        run_id: run_id.into(),
        task_id: None,
        workspace_id: WS_ID.into(),
        session_id: Some(format!("sess-{run_id}")),
        provider: provider.into(),
        profile: None,
        started_at: Some(finished - 1500),
        finished_at: finished,
        outcome: outcome.into(),
        input_tokens: 100,
        output_tokens: 40,
        cost_usd: cost,
        diff_add: 0,
        diff_del: 0,
    }
}

/// `hangar/run_history` returns the seeded runs newest-finished-first, carrying
/// provider / outcome / token-cost.
#[tokio::test]
async fn run_history_orders_newest_first() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, store) = start_server(dir.path()).await;

    RunHistoryRepo::record(store.pool(), &run("r1", "claude", 1000, "failed", 0.001))
        .await
        .unwrap();
    RunHistoryRepo::record(store.pool(), &run("r2", "codex", 3000, "success", 0.02))
        .await
        .unwrap();
    RunHistoryRepo::record(store.pool(), &run("r3", "claude", 2000, "success", 0.01))
        .await
        .unwrap();

    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;

    // The plugin subscribes by slug; the daemon resolves it to WS_ID.
    let runs = c.run_history(WS_SLUG, None).await;
    assert_eq!(runs.len(), 3, "all three runs on the timeline: {runs:?}");
    // Newest finished first: r2 (3000) -> r3 (2000) -> r1 (1000).
    assert_eq!(runs[0]["run_id"].as_str(), Some("r2"));
    assert_eq!(runs[0]["provider"].as_str(), Some("codex"));
    assert_eq!(runs[0]["outcome"].as_str(), Some("success"));
    assert!((runs[0]["cost_usd"].as_f64().unwrap() - 0.02).abs() < 1e-9);
    assert_eq!(runs[1]["run_id"].as_str(), Some("r3"));
    assert_eq!(runs[2]["run_id"].as_str(), Some("r1"));
    assert_eq!(runs[2]["outcome"].as_str(), Some("failed"));
}

/// The `limit` param caps the returned rows (newest kept) and the daemon clamps
/// it; an unknown workspace yields an empty timeline.
#[tokio::test]
async fn run_history_respects_limit_and_unknown_workspace() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, store) = start_server(dir.path()).await;

    for i in 0..5 {
        RunHistoryRepo::record(
            store.pool(),
            &run(&format!("r{i}"), "claude", 1000 + i, "success", 0.0),
        )
        .await
        .unwrap();
    }

    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;

    // Limit caps to the two newest.
    let capped = c.run_history(WS_SLUG, Some(2)).await;
    assert_eq!(capped.len(), 2, "limit=2 returns two rows");
    assert_eq!(
        capped[0]["run_id"].as_str(),
        Some("r4"),
        "newest first under the cap"
    );

    // Unknown workspace: empty timeline, never an error.
    let none = c.run_history("ws-does-not-exist", None).await;
    assert!(
        none.is_empty(),
        "unknown workspace yields an empty timeline"
    );
}
