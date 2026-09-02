//! The per-connection idle read bound: a request/response connection that goes
//! quiet is reclaimed after the idle window, while a connection holding a LIVE
//! subscription is a push channel and survives that same quiet window (the TUI
//! plugin subscribes once and may send nothing for an hour while the operator
//! watches a run; idle-closing it painted "daemon offline" on a healthy daemon).
//!
//! Both windows are shrunk through their test-only env overrides, so the whole
//! proof takes well under two seconds. One test drives both connections so the
//! process-wide env is set exactly once.

use std::time::{Duration, Instant};

use ainb_hangar_daemon::events::EventBroker;
use ainb_hangar_daemon::rpc::{self, DaemonHealth};
use ainb_hangar_daemon::seed;
use ainb_hangar_proto::{RpcId, RpcRequest, methods};
use ainb_hangar_store::Store;
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};

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

    /// The next frame, `Ok(None)` when the daemon closed the connection.
    async fn read_frame(&mut self) -> std::io::Result<Option<serde_json::Value>> {
        use tokio::io::AsyncBufReadExt;
        let mut len: Option<usize> = None;
        loop {
            let mut line = String::new();
            let n = self.reader.read_line(&mut line).await?;
            if n == 0 {
                return Ok(None);
            }
            let t = line.trim_end_matches("\r\n");
            if t.is_empty() {
                let mut body = vec![0u8; len.expect("Content-Length header")];
                self.reader.read_exact(&mut body).await?;
                return Ok(Some(serde_json::from_slice(&body).unwrap()));
            }
            if let Some((name, v)) = t.split_once(':') {
                if name.trim().eq_ignore_ascii_case("Content-Length") {
                    len = v.trim().parse().ok();
                }
            }
        }
    }

    /// Send `method` and read frames until its response (a frame with an `id`).
    async fn call(&mut self, method: &str, params: serde_json::Value) -> Option<serde_json::Value> {
        self.send(method, params).await;
        loop {
            let frame = tokio::time::timeout(Duration::from_secs(5), self.read_frame())
                .await
                .expect("frame within 5s")
                .expect("read")?;
            if frame.get("id").is_some() {
                return Some(frame);
            }
        }
    }

    async fn auth(&mut self, dir: &std::path::Path) {
        let token_path = ainb_hangar_proto::auth::token_file_in(dir);
        let token = std::fs::read_to_string(&token_path).expect("read daemon.token");
        let resp = self
            .call(
                methods::AUTH_HELLO,
                serde_json::json!({ "token": token.trim() }),
            )
            .await
            .expect("auth reply");
        assert!(resp["error"].is_null(), "auth/hello must ack: {resp}");
    }
}

async fn start_server(dir: &std::path::Path) -> std::path::PathBuf {
    let store = Store::open_in(dir).await.unwrap();
    seed::seed_p4_fixture(store.pool()).await.unwrap();
    rpc::auth::ensure_socket_token(store.pool(), dir)
        .await
        .expect("ensure socket token");
    let socket_path = rpc::socket_path_in(dir);
    let listener = rpc::bind(&socket_path).expect("bind socket");
    let (broker, outbox_rx) = EventBroker::with_outbox();
    ainb_hangar_daemon::event_outbox::spawn(store.pool().clone(), outbox_rx);
    let health = DaemonHealth {
        socket_path: socket_path.to_string_lossy().into_owned(),
        pid: std::process::id(),
        started_at: Instant::now(),
        version: "0.1.0".into(),
        stats: std::sync::Arc::new(ainb_hangar_daemon::health_stats::HealthStats::default()),
    };
    tokio::spawn(rpc::serve(listener, store.pool().clone(), health, broker));
    socket_path
}

#[tokio::test]
async fn a_subscribed_connection_outlives_the_idle_window_an_unsubscribed_one_does_not() {
    // 300ms request/response window, 5s subscribed window: the quiet period
    // below (1s) sits between them.
    // The only test in this binary, so no concurrent reader of these vars.
    std::env::set_var("AINB_HANGAR_RPC_IDLE_MS", "300");
    std::env::set_var("AINB_HANGAR_RPC_SUBSCRIBED_IDLE_MS", "5000");
    let dir = tempfile::tempdir().unwrap();
    let socket_path = start_server(dir.path()).await;

    // A subscribed client (the TUI plugin's shape): auth, workspace/subscribe,
    // then silence.
    let mut subscribed = Client::connect(&socket_path).await;
    subscribed.auth(dir.path()).await;
    let ack = subscribed
        .call(
            methods::WORKSPACE_SUBSCRIBE,
            serde_json::json!({ "workspace_id": seed::WS_SLUG }),
        )
        .await
        .expect("subscribe reply");
    assert!(ack["error"].is_null(), "subscribe must ack: {ack}");

    // A request/response client: auth only, then silence.
    let mut plain = Client::connect(&socket_path).await;
    plain.auth(dir.path()).await;

    tokio::time::sleep(Duration::from_millis(1000)).await;

    // The plain connection was reclaimed: the daemon closed it (EOF), or the
    // next write is refused.
    let plain_alive = match tokio::time::timeout(Duration::from_secs(2), plain.read_frame()).await {
        Ok(Ok(None)) | Ok(Err(_)) => false,
        Ok(Ok(Some(_))) => true,
        Err(_) => {
            // No EOF surfaced yet: a round trip decides.
            plain.call(methods::HANGAR_HEALTH, serde_json::json!({})).await.is_some()
        }
    };
    assert!(
        !plain_alive,
        "an idle request/response connection must be reclaimed"
    );

    // The subscribed connection still answers after the same quiet period.
    let health = subscribed
        .call(methods::HANGAR_HEALTH, serde_json::json!({}))
        .await
        .expect("subscribed connection must still be served after the idle window");
    assert!(
        health["error"].is_null(),
        "health must ack over the surviving link: {health}"
    );
}
