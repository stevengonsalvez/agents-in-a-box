//! I8: an ACP delivery NEVER reaches the tmux send machinery.
//!
//! This file owns its own process because it rewrites `PATH`: a fake `tmux`
//! records every invocation, and the whole point is that the recorder stays
//! EMPTY. It is a single test so no sibling can observe the mutated
//! environment.
//!
//! The trap it pins lives in `handle_fleet_action`: without an explicit ACP arm
//! ahead of the `verified_tmux_send` fallthrough, an ACP `SendPrompt` falls into
//! the tmux path and fails safe only because `tmux_target` is NULL, reporting
//! the misleading detail "exact tmux process identity is unavailable" for a
//! session that has no pane BY DESIGN.
//!
//! DISCLOSURE: the adapter is `ainb-acp`'s `fake_acp_adapter` fixture and the
//! `tmux` on PATH is a shell-script recorder; neither is a real binary.

#![allow(
    clippy::too_many_lines,
    reason = "one END TO END scenario per test; splitting it hides the sequence under test"
)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ainb_hangar_daemon::acp_pool::{AcpPool, PoolConfig};
use ainb_hangar_daemon::events::EventBroker;
use ainb_hangar_daemon::rpc::{self, DaemonHealth};
use ainb_hangar_proto::{RpcId, RpcRequest, methods};
use ainb_hangar_store::Store;
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};

/// A `tmux` that records EVERY invocation, including `list-panes`. An ACP
/// delivery must not produce a single line.
fn install_recording_tmux(dir: &Path, log: &Path) {
    use std::os::unix::fs::PermissionsExt as _;

    let script = format!(
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> {log}\nexit 0\n",
        log = log.display()
    );
    let path = dir.join("tmux");
    std::fs::write(&path, script).expect("write fake tmux");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
        .expect("make fake tmux executable");
}

fn fake_adapter() -> PathBuf {
    let mut dir = std::env::current_exe().expect("test binary path");
    dir.pop();
    if dir.ends_with("deps") {
        dir.pop();
    }
    // UNCONDITIONAL: `cargo test -p ainb-hangar-daemon` never rebuilds another
    // crate's binary, so a "build only when absent" guard silently pins a stale
    // fixture on disk.
    let status = std::process::Command::new(env!("CARGO"))
        .args(["build", "-p", "ainb-acp", "--bin", "fake_acp_adapter"])
        .status()
        .expect("build the fixture adapter");
    assert!(status.success(), "fixture adapter build failed");
    let binary = dir.join("fake_acp_adapter");
    assert!(
        binary.exists(),
        "fixture adapter missing at {}",
        binary.display()
    );
    binary
}

struct Client {
    reader: BufReader<OwnedReadHalf>,
    writer: OwnedWriteHalf,
    next_id: i64,
}

impl Client {
    async fn authed(dir: &Path, socket: &Path) -> Self {
        let deadline = Instant::now() + Duration::from_secs(5);
        let stream = loop {
            match UnixStream::connect(socket).await {
                Ok(stream) => break stream,
                Err(_) if Instant::now() < deadline => {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                Err(error) => panic!("never connected: {error}"),
            }
        };
        let (read_half, writer) = stream.into_split();
        let mut client = Self {
            reader: BufReader::new(read_half),
            writer,
            next_id: 1,
        };
        let token =
            std::fs::read_to_string(ainb_hangar_proto::auth::token_file_in(dir)).expect("token");
        let response = client
            .call(
                methods::AUTH_HELLO,
                serde_json::json!({ "token": token.trim() }),
            )
            .await;
        assert!(
            response["error"].is_null(),
            "auth/hello must ack: {response}"
        );
        client
    }

    async fn call(&mut self, method: &str, params: serde_json::Value) -> serde_json::Value {
        self.next_id += 1;
        let request = RpcRequest {
            jsonrpc: ainb_hangar_proto::jsonrpc_version(),
            id: RpcId::Number(self.next_id),
            method: method.to_string(),
            params,
        };
        let body = serde_json::to_vec(&request).unwrap();
        let mut frame = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
        frame.extend_from_slice(&body);
        self.writer.write_all(&frame).await.unwrap();
        self.writer.flush().await.unwrap();
        loop {
            let frame = tokio::time::timeout(Duration::from_secs(20), self.read_frame())
                .await
                .unwrap_or_else(|_| panic!("no response to {method} within 20s"));
            if frame.get("id").is_some() {
                return frame;
            }
        }
    }

    async fn read_frame(&mut self) -> serde_json::Value {
        use tokio::io::AsyncBufReadExt;

        let mut content_length = None;
        loop {
            let mut line = String::new();
            let read = self.reader.read_line(&mut line).await.unwrap();
            assert!(read > 0, "connection closed while awaiting frame");
            let line = line.trim_end_matches("\r\n");
            if line.is_empty() {
                let mut body = vec![0_u8; content_length.expect("Content-Length header")];
                self.reader.read_exact(&mut body).await.unwrap();
                return serde_json::from_slice(&body).unwrap();
            }
            if let Some((name, value)) = line.split_once(':') {
                if name.trim().eq_ignore_ascii_case("Content-Length") {
                    content_length = value.trim().parse().ok();
                }
            }
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_acp_delivery_never_invokes_tmux() {
    let dir = tempfile::tempdir().expect("tempdir");
    let bin_dir = dir.path().join("bin");
    std::fs::create_dir_all(&bin_dir).expect("bin dir");
    let tmux_log = dir.path().join("tmux-calls.log");
    install_recording_tmux(&bin_dir, &tmux_log);
    // Single-test binary: no sibling test can observe this mutation.
    std::env::set_var(
        "PATH",
        format!(
            "{}:{}",
            bin_dir.display(),
            std::env::var("PATH").unwrap_or_default()
        ),
    );

    let store = Store::open_in(dir.path()).await.expect("store");
    rpc::auth::ensure_socket_token(store.pool(), dir.path()).await.expect("token");
    let socket_path = rpc::socket_path_in(dir.path());
    let listener = rpc::bind(&socket_path).expect("bind");
    let broker = EventBroker::new();

    // The pool the RPC handlers route through, wired to the fixture adapter.
    let mut config = PoolConfig::default();
    config.adapters.insert(
        ainb_acp::config::CLAUDE_ADAPTER.to_string(),
        ainb_acp::config::AdapterConfig::new(ainb_acp::config::CLAUDE_ADAPTER, "default")
            .command(fake_adapter())
            .extra_env(vec![("FAKE_ACP_CHUNKS".to_string(), "2".to_string())]),
    );
    ainb_hangar_daemon::acp_pool::install(AcpPool::new(store.clone(), broker.sink(), config)).await;

    let health = DaemonHealth {
        socket_path: socket_path.to_string_lossy().into_owned(),
        pid: std::process::id(),
        started_at: Instant::now(),
        version: "0.1.0".to_string(),
        stats: Arc::new(ainb_hangar_daemon::health_stats::HealthStats::default()),
    };
    tokio::spawn(rpc::serve(listener, store.pool().clone(), health, broker));

    let mut client = Client::authed(dir.path(), &socket_path).await;
    let created = client
        .call(
            methods::FLEET_ACP_SESSION_CREATE,
            serde_json::json!({
                "provider": "claude-agent-acp",
                "cwd": dir.path().to_string_lossy(),
            }),
        )
        .await;
    assert!(created["error"].is_null(), "{created}");
    let session_key = created["result"]["session_key"].as_str().expect("key").to_string();

    let sent = client
        .call(
            methods::FLEET_MESSAGE_SEND,
            serde_json::json!({
                "targets": [session_key],
                "text": "hello acp",
                "request_id": "req-acp-no-tmux",
            }),
        )
        .await;
    assert!(sent["error"].is_null(), "{sent}");
    // PENDING at write-ack is the contract: an ACP leg resolves at TURN END.
    assert_eq!(
        sent["result"]["deliveries"][0]["state"], "PENDING",
        "an ACP leg is not answered by the send itself: {sent}"
    );

    // Wait for the turn to finish through the pool's own receipt path.
    let deadline = Instant::now() + Duration::from_secs(30);
    let (state, detail) = loop {
        let row: Option<(String, Option<String>)> = sqlx::query_as(
            "SELECT state, detail FROM fleet_message_delivery WHERE session_key = ?",
        )
        .bind(&session_key)
        .fetch_optional(store.pool())
        .await
        .expect("delivery query");
        if let Some((state, detail)) = row {
            if state != "PENDING" {
                break (state, detail);
            }
        }
        assert!(Instant::now() < deadline, "the ACP delivery never resolved");
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert_eq!(state, "DELIVERED", "detail: {detail:?}");

    let calls = std::fs::read_to_string(&tmux_log).unwrap_or_default();
    assert!(
        calls.trim().is_empty(),
        "an ACP delivery must not invoke tmux at all, got: {calls}"
    );
    assert_ne!(
        detail.as_deref(),
        Some("tmux_identity_changed"),
        "and must never report a tmux transport symptom"
    );

    // The OTHER entry point into the same trap: a direct `fleet/action`
    // SendPrompt. `execute_fleet_action` reaches `verified_tmux_send` for every
    // provider without an explicit arm, so this is the case the ACP arm exists
    // to intercept, and the recorder is the only thing that can tell "answered
    // by the pool" from "answered by a tmux send that failed safe".
    let version: i64 =
        sqlx::query_scalar("SELECT version FROM fleet_session WHERE session_key = ?")
            .bind(&session_key)
            .fetch_one(store.pool())
            .await
            .expect("session version");
    let acted = client
        .call(
            methods::FLEET_ACTION,
            serde_json::json!({
                "session_key": session_key,
                "expected_version": version,
                "request_id": "req-acp-action-no-tmux",
                "action": { "action": "send_prompt", "text": "direct prompt" },
            }),
        )
        .await;
    assert!(acted["error"].is_null(), "{acted}");
    let receipt = &acted["result"]["receipt"];
    assert_eq!(
        receipt["status"], "DELIVERED",
        "the ACP arm answered, not the tmux fallthrough: {acted}"
    );
    let receipt_detail = receipt["detail"].as_str().unwrap_or_default();
    assert!(
        receipt_detail.starts_with("acp_queued;"),
        "and it answered as the POOL, naming the message it queued: {acted}"
    );
    assert!(
        !receipt_detail.contains("tmux"),
        "an ACP action must never report a tmux symptom: {acted}"
    );

    let calls = std::fs::read_to_string(&tmux_log).unwrap_or_default();
    assert!(
        calls.trim().is_empty(),
        "a fleet/action SendPrompt on an ACP session must not invoke tmux either, got: {calls}"
    );

    ainb_hangar_daemon::acp_pool::uninstall().await;
}
