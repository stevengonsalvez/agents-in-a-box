//! Integration: the daemon's `UnixListener` JSON-RPC server (P4.10) answers the
//! P4 snapshot RPCs over real socket bytes, seeded with the P4 fixture.
//!
//! This is the in-crate proof that GAP2 (the daemon had no socket listener) is
//! closed: it binds the *real* `rpc::serve` listener, connects a client over a
//! real `UnixStream`, frames a genuine `ainb-hangar-proto` request, and asserts
//! the seeded `Refactor API` issue / `claude-agent` actor / `commit` skill come
//! back. The plugin speaks the identical wire shape through the host cap.

use std::time::{Duration, Instant};

use ainb_hangar_daemon::rpc::{self, DaemonHealth};
use ainb_hangar_daemon::seed::{self, WS_ID};
use ainb_hangar_proto::{methods, RpcId, RpcRequest, RpcResponse};
use ainb_hangar_store::Store;
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

/// Frame a request in a Content-Length envelope.
fn frame(req: &RpcRequest) -> Vec<u8> {
    let body = serde_json::to_vec(req).unwrap();
    let mut out = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    out.extend_from_slice(&body);
    out
}

/// Read one Content-Length response frame.
async fn read_resp<R: tokio::io::AsyncBufRead + Unpin>(r: &mut R) -> RpcResponse {
    use tokio::io::AsyncBufReadExt;
    let mut len: Option<usize> = None;
    loop {
        let mut line = String::new();
        r.read_line(&mut line).await.unwrap();
        let t = line.trim_end_matches("\r\n");
        if t.is_empty() {
            let mut body = vec![0u8; len.unwrap()];
            r.read_exact(&mut body).await.unwrap();
            return serde_json::from_slice(&body).unwrap();
        }
        if let Some((n, v)) = t.split_once(':') {
            if n.trim().eq_ignore_ascii_case("Content-Length") {
                len = v.trim().parse().ok();
            }
        }
    }
}

/// Send a request and read its response over the live socket.
async fn call(conn: &mut UnixStream, method: &str, params: serde_json::Value) -> RpcResponse {
    let req = RpcRequest {
        jsonrpc: ainb_hangar_proto::jsonrpc_version(),
        id: RpcId::Number(7),
        method: method.into(),
        params,
    };
    conn.write_all(&frame(&req)).await.unwrap();
    conn.flush().await.unwrap();
    let mut reader = BufReader::new(conn);
    read_resp(&mut reader).await
}

#[tokio::test]
async fn seeded_snapshots_round_trip_over_real_socket() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(dir.path()).await.unwrap();
    seed::seed_p4_fixture(store.pool()).await.unwrap();

    let socket_path = rpc::socket_path_in(dir.path());
    let listener = rpc::bind(&socket_path).expect("bind socket");
    let health = DaemonHealth {
        socket_path: socket_path.to_string_lossy().into_owned(),
        pid: std::process::id(),
        started_at: Instant::now(),
        version: "0.1.0".into(),
        stats: std::sync::Arc::new(ainb_hangar_daemon::health_stats::HealthStats::default()),
    };
    tokio::spawn(rpc::serve(listener, store.pool().clone(), health));

    // Poll-connect (the accept loop is up within a tick).
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut conn = loop {
        match UnixStream::connect(&socket_path).await {
            Ok(c) => break c,
            Err(_) if Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            Err(e) => panic!("never connected: {e}"),
        }
    };

    // workspace/subscribe acks.
    let sub = call(&mut conn, methods::WORKSPACE_SUBSCRIBE, serde_json::json!({"workspace_id": WS_ID})).await;
    assert!(sub.error.is_none(), "subscribe rejected: {sub:?}");

    // issues_list returns the seeded board (incl. Refactor API).
    let issues = call(&mut conn, methods::HANGAR_ISSUES_LIST, serde_json::json!({"workspace_id": WS_ID})).await;
    let arr = issues.result.unwrap()["issues"].as_array().unwrap().clone();
    assert_eq!(arr.len(), 3, "three seeded issues");
    assert!(arr.iter().any(|i| i["title"] == "Refactor API"), "Refactor API missing");

    // agents_list lists claude-agent + the member.
    let agents = call(&mut conn, methods::HANGAR_AGENTS_LIST, serde_json::json!({"workspace_id": WS_ID})).await;
    let actors = agents.result.unwrap()["actors"].as_array().unwrap().clone();
    assert!(actors.iter().any(|a| a["display_name"] == "claude-agent" && a["is_agent"] == true));

    // skills_list lists commit (used).
    let skills = call(&mut conn, methods::HANGAR_SKILLS_LIST, serde_json::json!({"workspace_id": WS_ID})).await;
    let srows = skills.result.unwrap()["skills"].as_array().unwrap().clone();
    assert!(srows.iter().any(|s| s["name"] == "commit" && s["used"] == true));

    // health reports the bound socket + connected.
    let health = call(&mut conn, methods::HANGAR_HEALTH, serde_json::json!({})).await;
    let h = health.result.unwrap();
    assert_eq!(h["connected"], true);
    assert!(h["socket_path"].as_str().unwrap().ends_with("hangar.sock"));
}
