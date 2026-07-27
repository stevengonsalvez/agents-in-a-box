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
use ainb_hangar_proto::{RpcId, RpcRequest, RpcResponse, methods};
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
    let (socket_path, _store) = start_server(dir.path()).await;
    let mut conn = connect(&socket_path).await;

    // First frame: authenticate exactly as a real client does — read the
    // plaintext the daemon wrote at boot and present it on auth/hello.
    auth_from_file(&mut conn, dir.path()).await;

    // workspace/subscribe acks.
    let sub = call(
        &mut conn,
        methods::WORKSPACE_SUBSCRIBE,
        serde_json::json!({"workspace_id": WS_ID}),
    )
    .await;
    assert!(sub.error.is_none(), "subscribe rejected: {sub:?}");

    // issues_list returns the seeded board (incl. Refactor API).
    let issues = call(
        &mut conn,
        methods::HANGAR_ISSUES_LIST,
        serde_json::json!({"workspace_id": WS_ID}),
    )
    .await;
    let arr = issues.result.unwrap()["issues"].as_array().unwrap().clone();
    assert_eq!(arr.len(), 3, "three seeded issues");
    assert!(
        arr.iter().any(|i| i["title"] == "Refactor API"),
        "Refactor API missing"
    );

    // agents_list lists claude-agent + the member.
    let agents = call(
        &mut conn,
        methods::HANGAR_AGENTS_LIST,
        serde_json::json!({"workspace_id": WS_ID}),
    )
    .await;
    let actors = agents.result.unwrap()["actors"].as_array().unwrap().clone();
    assert!(
        actors
            .iter()
            .any(|a| a["display_name"] == "claude-agent" && a["is_agent"] == true)
    );

    // skills_list lists commit (used).
    let skills = call(
        &mut conn,
        methods::HANGAR_SKILLS_LIST,
        serde_json::json!({"workspace_id": WS_ID}),
    )
    .await;
    let srows = skills.result.unwrap()["skills"].as_array().unwrap().clone();
    assert!(srows.iter().any(|s| s["name"] == "commit" && s["used"] == true));

    // health reports the bound socket + connected.
    let health = call(&mut conn, methods::HANGAR_HEALTH, serde_json::json!({})).await;
    let h = health.result.unwrap();
    assert_eq!(h["connected"], true);
    assert!(h["socket_path"].as_str().unwrap().ends_with("hangar.sock"));
}

/// Read one Content-Length response frame, or `None` on EOF (connection
/// closed by the daemon).
async fn read_resp_opt<R: tokio::io::AsyncBufRead + Unpin>(r: &mut R) -> Option<RpcResponse> {
    use tokio::io::AsyncBufReadExt;
    let mut len: Option<usize> = None;
    loop {
        let mut line = String::new();
        let n = r.read_line(&mut line).await.ok()?;
        if n == 0 {
            return None;
        }
        let t = line.trim_end_matches("\r\n");
        if t.is_empty() {
            let mut body = vec![0u8; len?];
            r.read_exact(&mut body).await.ok()?;
            return serde_json::from_slice(&body).ok();
        }
        if let Some((name, v)) = t.split_once(':') {
            if name.trim().eq_ignore_ascii_case("Content-Length") {
                len = v.trim().parse().ok();
            }
        }
    }
}

/// Bind + serve the real listener over a seeded store, returning the socket
/// path (the daemon side of every auth test below). Mirrors `boot()`: the
/// socket-auth token is ensured (minted + plaintext written to
/// `{dir}/hangar/daemon.token`) BEFORE the bind, exactly like production.
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

/// Poll-connect until the accept loop is up.
async fn connect(socket_path: &std::path::Path) -> UnixStream {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match UnixStream::connect(socket_path).await {
            Ok(c) => return c,
            Err(_) if Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            Err(e) => panic!("never connected: {e}"),
        }
    }
}

/// Authenticate `conn` the way every real client does: read the plaintext from
/// the token file the daemon wrote (`{dir}/hangar/daemon.token`) and send it
/// as the first `auth/hello` frame. Panics if the daemon rejects it.
async fn auth_from_file(conn: &mut UnixStream, dir: &std::path::Path) {
    let token_path = ainb_hangar_proto::auth::token_file_in(dir);
    let token = std::fs::read_to_string(&token_path).expect("read daemon.token");
    let resp = call(
        conn,
        ainb_hangar_proto::methods::AUTH_HELLO,
        serde_json::json!({ "token": token.trim() }),
    )
    .await;
    assert!(resp.error.is_none(), "auth/hello must ack: {resp:?}");
}

/// A connection whose FIRST frame is not `auth/hello` is answered with an
/// `UNAUTHORIZED` error and closed — no `hangar/*` method is reachable
/// without the minted daemon token (P0 bead e38.1, anchor (a)).
#[tokio::test]
async fn unauthenticated_first_frame_is_rejected_and_closed() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, _store) = start_server(dir.path()).await;
    let mut conn = connect(&socket_path).await;

    // Skip auth entirely: open with workspace/subscribe.
    let resp = call(
        &mut conn,
        methods::WORKSPACE_SUBSCRIBE,
        serde_json::json!({"workspace_id": WS_ID}),
    )
    .await;
    let err = resp.error.expect("unauthenticated subscribe must be rejected");
    assert_eq!(
        err.code,
        ainb_hangar_proto::auth::UNAUTHORIZED,
        "rejection must carry the shared UNAUTHORIZED code: {err:?}"
    );

    // The daemon closes the connection: a follow-up hangar/* call gets EOF,
    // never a snapshot.
    let req = RpcRequest {
        jsonrpc: ainb_hangar_proto::jsonrpc_version(),
        id: RpcId::Number(8),
        method: methods::HANGAR_ISSUES_LIST.into(),
        params: serde_json::json!({"workspace_id": WS_ID}),
    };
    // The write may succeed into the OS buffer; the READ must see EOF.
    let _ = conn.write_all(&frame(&req)).await;
    let _ = conn.flush().await;
    let mut reader = BufReader::new(&mut conn);
    assert!(
        read_resp_opt(&mut reader).await.is_none(),
        "connection must be closed after the auth rejection"
    );
}

/// A first frame that IS `auth/hello` but carries a wrong token is rejected
/// with `UNAUTHORIZED` and the connection closed.
#[tokio::test]
async fn wrong_token_is_rejected_and_closed() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, _store) = start_server(dir.path()).await;
    let mut conn = connect(&socket_path).await;

    let resp = call(
        &mut conn,
        ainb_hangar_proto::methods::AUTH_HELLO,
        serde_json::json!({"token": "mdt_DEFINITELYWRONG"}),
    )
    .await;
    let err = resp.error.expect("wrong token must be rejected");
    assert_eq!(err.code, ainb_hangar_proto::auth::UNAUTHORIZED, "{err:?}");

    let mut reader = BufReader::new(&mut conn);
    assert!(
        read_resp_opt(&mut reader).await.is_none(),
        "connection must be closed after a wrong-token hello"
    );
}

#[tokio::test]
async fn invalid_frame_headers_close_authenticated_connection() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, _store) = start_server(dir.path()).await;
    let cases = [
        ("missing", b"\r\n".as_slice()),
        (
            "duplicate",
            b"Content-Length: 2\r\nContent-Length: 2\r\n\r\n{}".as_slice(),
        ),
        ("malformed", b"Content-Length 2\r\n\r\n".as_slice()),
        (
            "unsupported",
            b"Content-Type: application/json\r\nContent-Length: 2\r\n\r\n{}".as_slice(),
        ),
        ("oversized", b"Content-Length: 16777217\r\n\r\n".as_slice()),
    ];
    for (name, frame) in cases {
        let mut conn = connect(&socket_path).await;
        auth_from_file(&mut conn, dir.path()).await;
        conn.write_all(frame).await.unwrap();
        conn.flush().await.unwrap();
        let mut reader = BufReader::new(&mut conn);
        assert!(
            read_resp_opt(&mut reader).await.is_none(),
            "daemon must close on {name} frame header"
        );
    }
}

/// Anchor (b) + (c): a same-uid client (any in-test connection IS same-uid)
/// that reads the token file and authenticates gets full RPC access — the
/// peer-cred gate passes it and the snapshot round-trips.
#[tokio::test]
async fn same_uid_client_with_file_token_gets_rpc_access() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, _store) = start_server(dir.path()).await;
    let mut conn = connect(&socket_path).await;

    auth_from_file(&mut conn, dir.path()).await;

    let ping = call(&mut conn, methods::PING, serde_json::json!({})).await;
    assert!(
        ping.error.is_none(),
        "authenticated ping must ack: {ping:?}"
    );

    let issues = call(
        &mut conn,
        methods::HANGAR_ISSUES_LIST,
        serde_json::json!({"workspace_id": WS_ID}),
    )
    .await;
    let arr = issues.result.expect("issues result")["issues"].as_array().unwrap().clone();
    assert_eq!(arr.len(), 3, "the authenticated snapshot must round-trip");
}

/// The bound socket file is `0600` (owner-only) — no other local user can
/// even connect to the control plane.
#[tokio::test]
async fn socket_file_is_owner_only() {
    use std::os::unix::fs::PermissionsExt as _;

    let dir = tempfile::tempdir().unwrap();
    let (socket_path, _store) = start_server(dir.path()).await;

    let mode = std::fs::metadata(&socket_path).expect("stat socket").permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "socket must be chmod 0600, got {mode:o}");
}
