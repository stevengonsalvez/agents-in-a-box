//! The chat bus's DELIVERED leg, against a FAKE tmux binary that records every
//! invocation (I1's "one tmux submit" half, plan Phase 3).
//!
//! This file owns its own process because it rewrites `PATH`: it is a single
//! test so no sibling can observe the mutated environment. The fake `tmux`
//! answers `list-panes` with one pane whose exact target and start fingerprint
//! the seeded session claims, so the hardened send path verifies identity and
//! then submits for real, into the recorder instead of a terminal.
//!
//! DISCLOSURE: the tmux binary here is a fixture, not the real tmux. What it
//! proves is the daemon's side of the contract (verify, submit once, resolve
//! DELIVERED); the argv it records is the hardened `-l --` literal form.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ainb_hangar_daemon::events::EventBroker;
use ainb_hangar_daemon::rpc::{self, DaemonHealth};
use ainb_hangar_proto::{RpcId, RpcRequest, methods};
use ainb_hangar_store::Store;
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};

/// The pane the fake tmux reports, and the identity the seeded session claims.
const PANE_TARGET: &str = "msgbus:0.0";
const PANE_FINGERPRINT: &str = "pane=%9;pid=1;session_started=100";

/// Write a `tmux` shim into `dir` that reports one pane and records every
/// other invocation, one argv per line, into `log`.
fn install_fake_tmux(dir: &Path, log: &Path) {
    use std::os::unix::fs::PermissionsExt as _;

    let script = format!(
        "#!/bin/sh\n\
         if [ \"$1\" = \"list-panes\" ]; then\n\
         \tprintf 'msgbus\\t0\\t0\\t%%9\\t1\\t/work\\tzsh\\t\\t100\\t0\\t%s\\n' \"$(date +%%s)\"\n\
         \texit 0\n\
         fi\n\
         printf '%s\\n' \"$*\" >> {log}\n\
         exit 0\n",
        log = log.display()
    );
    let path = dir.join("tmux");
    std::fs::write(&path, script).expect("write fake tmux");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
        .expect("make fake tmux executable");
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
        let token = std::fs::read_to_string(ainb_hangar_proto::auth::token_file_in(dir))
            .expect("read daemon.token");
        let ack = client
            .call(
                methods::AUTH_HELLO,
                serde_json::json!({ "token": token.trim() }),
            )
            .await;
        assert!(ack["error"].is_null(), "auth/hello must ack: {ack}");
        client
    }

    async fn call(&mut self, method: &str, params: serde_json::Value) -> serde_json::Value {
        use tokio::io::AsyncBufReadExt;

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
            let mut content_length = None;
            let frame = loop {
                let mut line = String::new();
                let read = self.reader.read_line(&mut line).await.unwrap();
                assert!(read > 0, "connection closed while awaiting frame");
                let line = line.trim_end_matches("\r\n");
                if line.is_empty() {
                    let mut body = vec![0_u8; content_length.expect("Content-Length")];
                    self.reader.read_exact(&mut body).await.unwrap();
                    break serde_json::from_slice::<serde_json::Value>(&body).unwrap();
                }
                if let Some((name, value)) = line.split_once(':') {
                    if name.trim().eq_ignore_ascii_case("Content-Length") {
                        content_length = value.trim().parse().ok();
                    }
                }
            };
            if frame.get("id").is_some() {
                return frame;
            }
        }
    }
}

/// A message to a live tmux session is submitted through the hardened literal
/// send path, resolves DELIVERED, and a replay of the same `request_id` submits
/// NOTHING further (I1's one-submit half).
#[tokio::test]
async fn message_to_a_live_tmux_session_submits_once_and_resolves_delivered() {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let log = dir.path().join("tmux-calls.log");
    install_fake_tmux(&bin, &log);
    // Single-test binary: no sibling test can observe this mutation.
    std::env::set_var(
        "PATH",
        format!(
            "{}:{}",
            bin.display(),
            std::env::var("PATH").unwrap_or_default()
        ),
    );

    let store = Store::open_in(dir.path()).await.unwrap();
    rpc::auth::ensure_socket_token(store.pool(), dir.path()).await.unwrap();
    sqlx::query(
        "INSERT INTO fleet_session \
         (session_key, provider, cwd, tmux_target, process_start_fingerprint, capabilities, \
          discovered_at, last_observed_at, version) \
         VALUES ('claude:live', 'claude', '/work', ?, ?, '{\"send_prompt\":true}', 1, 1, 1)",
    )
    .bind(PANE_TARGET)
    .bind(PANE_FINGERPRINT)
    .execute(store.pool())
    .await
    .unwrap();

    let socket = rpc::socket_path_in(dir.path());
    let listener = rpc::bind(&socket).unwrap();
    let health = DaemonHealth {
        socket_path: socket.to_string_lossy().into_owned(),
        pid: std::process::id(),
        started_at: Instant::now(),
        version: "0.1.0".to_string(),
        stats: Arc::new(ainb_hangar_daemon::health_stats::HealthStats::default()),
    };
    tokio::spawn(rpc::serve(
        listener,
        store.pool().clone(),
        health,
        EventBroker::new(),
    ));

    let mut client = Client::authed(dir.path(), &socket).await;
    let params = serde_json::json!({
        "targets": ["claude:live"],
        "text": "ship it",
        "request_id": "req-tmux",
    });
    let first = client.call(methods::FLEET_MESSAGE_SEND, params.clone()).await;
    assert_eq!(
        first["result"]["deliveries"][0]["state"], "DELIVERED",
        "a verified submit resolves DELIVERED: {first}"
    );

    let calls = std::fs::read_to_string(&log).expect("the fake tmux was invoked");
    let sends: Vec<&str> = calls
        .lines()
        .filter(|line| line.contains("send-keys") && line.contains("-l"))
        .collect();
    assert_eq!(sends.len(), 1, "exactly one literal submit: {calls}");
    assert!(
        sends[0].contains("-l -- ship it"),
        "the hardened literal form carries the `--` terminator: {}",
        sends[0]
    );

    let second = client.call(methods::FLEET_MESSAGE_SEND, params).await;
    assert_eq!(
        first["result"], second["result"],
        "the replay answers identically"
    );
    let after_replay = std::fs::read_to_string(&log).unwrap();
    assert_eq!(
        after_replay, calls,
        "a replayed request_id must not submit a second prompt"
    );

    let state: String = sqlx::query_scalar(
        "SELECT state FROM fleet_message_delivery WHERE session_key = 'claude:live'",
    )
    .fetch_one(store.pool())
    .await
    .unwrap();
    assert_eq!(
        state, "DELIVERED",
        "the durable receipt agrees with the wire"
    );
}
