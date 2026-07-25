//! Integration: the control-plane attention inbox over a real framed
//! `UnixStream` (spec P2).
//!
//! Drives the whole data-plane path `boot()` wires:
//!
//! 1. start the real RPC server over a seeded store, sharing ONE event broker
//!    with the emit sink handed back to the test;
//! 2. seed durable `attention` rows (as the ingest producer would) and assert
//!    over the socket that `attention/list` snapshots them with the right SCOPE
//!    (fleet-wide vs workspace vs no-workspace host);
//! 3. `attention/subscribe` fleet-wide, then emit an `AttentionRaised` through
//!    the sink and assert the live delta arrives as a `hangar/event`
//!    notification — the fleet-wide forwarder carries a no-workspace host session
//!    that the workspace forwarder would drop;
//! 4. `attention/answer` a row that is already answered and assert the
//!    first-answer-wins outcome (`already_answered` by the prior winner), never a
//!    second delivery.

use std::time::{Duration, Instant};

use ainb_hangar_daemon::events::EventBroker;
use ainb_hangar_daemon::rpc::{self, DaemonHealth};
use ainb_hangar_daemon::seed::{self, WS_ID, WS_SLUG};
use ainb_hangar_proto::events::HangarEvent;
use ainb_hangar_proto::{RpcId, RpcRequest, methods};
use ainb_hangar_store::Store;
use ainb_hangar_store::repo::attention::{AttentionKind, AttentionRepo, NewAttention};
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

    /// Call an RPC and return the id-bearing response, ignoring any interleaved
    /// notifications (frames with no `id`).
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

    /// Read the next NOTIFICATION frame (no `id`) — an event push.
    async fn next_notification(&mut self) -> serde_json::Value {
        loop {
            let frame = self
                .read_frame(Duration::from_secs(5))
                .await
                .expect("no notification within 5s");
            if frame.get("id").is_none() {
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
}

/// Bind + serve the real listener over the seeded store, returning the socket
/// path, the live store, and a sink to emit events through (mirrors `boot()`).
async fn start_server(
    dir: &std::path::Path,
) -> (
    std::path::PathBuf,
    Store,
    ainb_hangar_daemon::events::EventSink,
) {
    let store = Store::open_in(dir).await.unwrap();
    seed::seed_p4_fixture(store.pool()).await.unwrap();
    rpc::auth::ensure_socket_token(store.pool(), dir)
        .await
        .expect("ensure socket token");
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
    let sink = broker.sink();
    tokio::spawn(rpc::serve(listener, store.pool().clone(), health, broker));
    (socket_path, store, sink)
}

fn ask(id: &str, session: &str, ws: Option<&str>, ts: i64) -> NewAttention {
    NewAttention {
        id: id.into(),
        session_id: session.into(),
        cwd: format!("/work/{session}"),
        workspace_id: ws.map(std::string::ToString::to_string),
        kind: AttentionKind::AskUserQuestion,
        payload: format!("{{\"kind\":\"ASK\",\"id\":\"{id}\"}}"),
        degraded: false,
        created_at: ts,
        raise_transcript: None,
        channels: ainb_hangar_core::channel::ChannelSet::NONE,
    }
}

#[tokio::test]
async fn attention_list_scopes_fleet_workspace_and_host() {
    let dir = tempfile::tempdir().unwrap();
    let (socket, store, _sink) = start_server(dir.path()).await;

    // One workspace-owned row + one no-workspace host row.
    AttentionRepo::insert(store.pool(), &ask("a1", "s1", Some(WS_ID), 1000))
        .await
        .unwrap();
    AttentionRepo::insert(store.pool(), &ask("h1", "s2", None, 2000)).await.unwrap();

    let mut client = Client::connect(&socket).await;
    client.auth_from_file(dir.path()).await;

    // Fleet-wide → both rows, oldest first.
    let fleet = client
        .call(
            methods::ATTENTION_LIST,
            serde_json::json!({ "fleet": true }),
        )
        .await;
    assert!(fleet["error"].is_null(), "attention/list must ack: {fleet}");
    let ids: Vec<&str> = fleet["result"]["attention"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["a1", "h1"], "fleet feed is host-wide, oldest first");

    // Workspace-scoped (by slug — the daemon resolves it to the real id) → only a1.
    let ws = client
        .call(
            methods::ATTENTION_LIST,
            serde_json::json!({ "fleet": false, "workspace_id": WS_SLUG }),
        )
        .await;
    let ws_ids: Vec<&str> = ws["result"]["attention"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_str().unwrap())
        .collect();
    assert_eq!(ws_ids, ["a1"], "workspace scope excludes the host row");

    // No workspace → only the host row.
    let host = client
        .call(
            methods::ATTENTION_LIST,
            serde_json::json!({ "fleet": false }),
        )
        .await;
    let host_ids: Vec<&str> = host["result"]["attention"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_str().unwrap())
        .collect();
    assert_eq!(host_ids, ["h1"], "no-workspace scope is the host rows only");
}

#[tokio::test]
async fn attention_subscribe_delivers_a_fleet_wide_raised_delta() {
    let dir = tempfile::tempdir().unwrap();
    let (socket, _store, sink) = start_server(dir.path()).await;

    let mut client = Client::connect(&socket).await;
    client.auth_from_file(dir.path()).await;

    // Subscribe fleet-wide. Registration happens before the snapshot read, so
    // an event emitted immediately after the ack cannot fall into a handoff gap.
    let ack = client.call(methods::ATTENTION_SUBSCRIBE, serde_json::json!({})).await;
    assert!(
        ack["error"].is_null(),
        "attention/subscribe must ack: {ack}"
    );
    assert!(ack["result"]["attention"].as_array().unwrap().is_empty());

    // Emit immediately. A post-ack registration race would lose this nudge.
    sink.emit_attention(HangarEvent::AttentionRaised {
        attention_id: "att-9".into(),
        session_id: "sess".into(),
        workspace_id: None,
        kind: "ask_user_question".into(),
        degraded: false,
        created_at: 4242,
        channels: ainb_hangar_core::channel::ChannelSet::NONE,
    });

    let note = client.next_notification().await;
    assert_eq!(note["method"], ainb_hangar_proto::events::EVENT_METHOD);
    assert_eq!(note["params"]["event"], "attention_raised");
    assert_eq!(note["params"]["attention_id"], "att-9");
    assert!(
        note["params"].get("id").is_none(),
        "a notification carries no id"
    );
}

#[tokio::test]
async fn attention_answer_already_answered_reports_the_winner() {
    let dir = tempfile::tempdir().unwrap();
    let (socket, store, _sink) = start_server(dir.path()).await;

    // Seed an OPEN row, then let a prior surface win the answer.
    AttentionRepo::insert(store.pool(), &ask("a1", "s1", Some(WS_ID), 1000))
        .await
        .unwrap();
    AttentionRepo::mark_answered_if_open(store.pool(), "a1", "web", "first", 3000)
        .await
        .unwrap();

    let mut client = Client::connect(&socket).await;
    client.auth_from_file(dir.path()).await;

    let resp = client
        .call(
            methods::ATTENTION_ANSWER,
            serde_json::json!({
                "attention_id": "a1",
                "answer": "second",
                "answered_by": "tui",
                "is_answer": true,
            }),
        )
        .await;
    assert!(resp["error"].is_null(), "attention/answer must ack: {resp}");
    assert_eq!(
        resp["result"]["outcome"], "already_answered",
        "a second answer loses the race: {resp}"
    );
    assert_eq!(resp["result"]["by"], "web", "the prior winner is reported");
}
