//! Integration (atc RPC conformance, spec P9 D12): the ATC-on-the-daemon RPCs
//! over a real framed `UnixStream`.
//!
//! Drives the whole data-plane path `boot()` wires for ATC:
//!
//! 1. `atc/register` an instance and assert it lands in the store with a computed
//!    next heartbeat tick (the launchd/systemd timer's daemon-native replacement);
//! 2. `atc/register` the SAME name again with new config and assert it is
//!    idempotent (refreshed, not duplicated);
//! 3. `atc/list` and assert the registered instance snapshots back over the wire;
//! 4. `atc/escalate` and assert it raises an `escalation` attention row that
//!    `attention/list` then surfaces — the escalation reaching the one attention
//!    pipeline every surface reads (D12), not a dead-end in task-log.md;
//! 5. a blank-name `atc/register` is rejected (INVALID_PARAMS), never a silent
//!    empty row.

use std::time::{Duration, Instant};

use ainb_hangar_daemon::events::EventBroker;
use ainb_hangar_daemon::rpc::{self, DaemonHealth};
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

    /// Call an RPC and return the id-bearing response, skipping notifications.
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
}

/// Bind + serve the real listener over a fresh store (mirrors `boot()`), sharing
/// one broker so escalation nudges could be observed if a subscriber attached.
async fn start_server(dir: &std::path::Path) -> (std::path::PathBuf, Store) {
    let store = Store::open_in(dir).await.unwrap();
    rpc::auth::ensure_socket_token(store.pool(), dir).await.expect("ensure socket token");
    let socket_path = rpc::socket_path_in(dir);
    let listener = rpc::bind(&socket_path).expect("bind socket");
    let broker = EventBroker::new();
    let health = DaemonHealth {
        socket_path: socket_path.to_string_lossy().into_owned(),
        pid: std::process::id(),
        started_at: Instant::now(),
        version: "0.1.0".into(),
        stats: std::sync::Arc::new(ainb_hangar_daemon::health_stats::HealthStats::default()),
    };
    tokio::spawn(rpc::serve(listener, store.pool().clone(), health, broker));
    (socket_path, store)
}

#[tokio::test]
async fn atc_register_list_and_escalate_over_the_socket() {
    let dir = tempfile::tempdir().unwrap();
    let (socket, _store) = start_server(dir.path()).await;
    let mut client = Client::connect(&socket).await;
    client.auth_from_file(dir.path()).await;

    // 1. Register an instance with an explicit cron + cap.
    let reg = client
        .call(
            methods::ATC_REGISTER,
            serde_json::json!({
                "name": "main",
                "cwd": "/work/atc",
                "tmux_session": "atc-main",
                "heartbeat_cron": "*/2 * * * *",
                "err_retry_cap": 5
            }),
        )
        .await;
    assert!(reg["error"].is_null(), "atc/register must ack: {reg}");
    assert_eq!(reg["result"]["name"], "main");
    assert!(
        reg["result"]["next_tick_at"].as_i64().is_some(),
        "register computes a next heartbeat tick: {reg}"
    );

    // 2. Re-register the SAME name with new config (setup always re-sends the full
    //    config) — idempotent refresh, never a duplicate row.
    let reg2 = client
        .call(
            methods::ATC_REGISTER,
            serde_json::json!({ "name": "main", "heartbeat_cron": "*/5 * * * *", "err_retry_cap": 7 }),
        )
        .await;
    assert!(reg2["error"].is_null(), "re-register must ack: {reg2}");

    // 3. List → exactly one instance, with the refreshed config.
    let list = client.call(methods::ATC_LIST, serde_json::json!({})).await;
    assert!(list["error"].is_null(), "atc/list must ack: {list}");
    let instances = list["result"]["instances"].as_array().unwrap();
    assert_eq!(instances.len(), 1, "re-register did not duplicate: {list}");
    assert_eq!(instances[0]["name"], "main");
    assert_eq!(instances[0]["heartbeat_cron"], "*/5 * * * *", "cron refreshed");
    assert_eq!(instances[0]["err_retry_cap"], 7, "cap refreshed");
    assert_eq!(instances[0]["enabled"], true);

    // 4. Escalate a stuck session → an `escalation` attention row exists.
    let esc = client
        .call(
            methods::ATC_ESCALATE,
            serde_json::json!({
                "instance_name": "main",
                "session_id": "sess-99",
                "cwd": "/work/x",
                "reason": "retry cap reached: overloaded_error"
            }),
        )
        .await;
    assert!(esc["error"].is_null(), "atc/escalate must ack: {esc}");
    let attention_id = esc["result"]["attention_id"].as_str().unwrap();
    assert!(attention_id.starts_with("escalation:main:sess-99"), "id shape: {esc}");

    // The escalation surfaces on the fleet-wide attention feed as kind=escalation.
    let att = client
        .call(methods::ATTENTION_LIST, serde_json::json!({ "fleet": true }))
        .await;
    assert!(att["error"].is_null(), "attention/list must ack: {att}");
    let rows = att["result"]["attention"].as_array().unwrap();
    let escalation = rows
        .iter()
        .find(|r| r["id"] == attention_id)
        .unwrap_or_else(|| panic!("escalation row not in the attention feed: {att}"));
    assert_eq!(escalation["kind"], "escalation");
    assert_eq!(escalation["session_id"], "sess-99");
}

#[tokio::test]
async fn atc_register_rejects_a_blank_name() {
    let dir = tempfile::tempdir().unwrap();
    let (socket, _store) = start_server(dir.path()).await;
    let mut client = Client::connect(&socket).await;
    client.auth_from_file(dir.path()).await;

    let resp = client
        .call(methods::ATC_REGISTER, serde_json::json!({ "name": "   " }))
        .await;
    assert!(
        resp["error"].is_object(),
        "a blank instance name is a client error, not a silent empty row: {resp}"
    );
    // And nothing was registered.
    let list = client.call(methods::ATC_LIST, serde_json::json!({})).await;
    assert_eq!(list["result"]["instances"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn atc_register_rejects_an_invalid_cron() {
    let dir = tempfile::tempdir().unwrap();
    let (socket, _store) = start_server(dir.path()).await;
    let mut client = Client::connect(&socket).await;
    client.auth_from_file(dir.path()).await;

    let resp = client
        .call(
            methods::ATC_REGISTER,
            serde_json::json!({ "name": "bad", "heartbeat_cron": "not a cron" }),
        )
        .await;
    assert!(
        resp["error"].is_object(),
        "an unparseable heartbeat cron is a client error: {resp}"
    );
}
