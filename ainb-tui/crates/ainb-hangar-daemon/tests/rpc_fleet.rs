//! Fleet control-plane RPC integration over a real Unix socket.
//!
//! Covers authoritative provider snapshots, same-cwd identity separation,
//! optimistic action guards, receipt idempotency, and ordered subscription
//! delivery from the durable revision log.

use std::time::{Duration, Instant};

use ainb_hangar_daemon::events::{EventBroker, EventSink};
use ainb_hangar_daemon::fleet::{self, HookObservation};
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
                Ok(stream) => break stream,
                Err(_) if Instant::now() < deadline => {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                Err(error) => panic!("never connected: {error}"),
            }
        };
        let (read_half, writer) = stream.into_split();
        Self {
            reader: BufReader::new(read_half),
            writer,
        }
    }

    async fn send(&mut self, method: &str, params: serde_json::Value) {
        let request = RpcRequest {
            jsonrpc: ainb_hangar_proto::jsonrpc_version(),
            id: RpcId::Number(7),
            method: method.to_string(),
            params,
        };
        let body = serde_json::to_vec(&request).unwrap();
        let mut frame = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
        frame.extend_from_slice(&body);
        self.writer.write_all(&frame).await.unwrap();
        self.writer.flush().await.unwrap();
    }

    async fn read_frame(&mut self, timeout: Duration) -> Option<serde_json::Value> {
        tokio::time::timeout(timeout, self.read_frame_inner()).await.ok()
    }

    async fn read_frame_inner(&mut self) -> serde_json::Value {
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

    async fn next_fleet_event(&mut self) -> serde_json::Value {
        loop {
            let frame = self
                .read_frame(Duration::from_secs(5))
                .await
                .expect("no Fleet notification within 5s");
            if frame.get("id").is_none() && frame["method"] == "fleet/event" {
                return frame["params"].clone();
            }
        }
    }

    async fn auth_from_file(&mut self, dir: &std::path::Path) {
        let token_path = ainb_hangar_proto::auth::token_file_in(dir);
        let token = std::fs::read_to_string(token_path).expect("read daemon.token");
        let response = self
            .call(
                methods::AUTH_HELLO,
                serde_json::json!({ "token": token.trim() }),
            )
            .await;
        assert!(
            response["error"].is_null(),
            "auth/hello must ack: {response}"
        );
    }
}

async fn start_server(dir: &std::path::Path) -> (std::path::PathBuf, Store, EventSink) {
    let store = Store::open_in(dir).await.unwrap();
    rpc::auth::ensure_socket_token(store.pool(), dir)
        .await
        .expect("ensure socket token");
    let socket_path = rpc::socket_path_in(dir);
    let listener = rpc::bind(&socket_path).expect("bind socket");
    let broker = EventBroker::new();
    let sink = broker.sink();
    let health = DaemonHealth {
        socket_path: socket_path.to_string_lossy().into_owned(),
        pid: std::process::id(),
        started_at: Instant::now(),
        version: "0.1.0".to_string(),
        stats: std::sync::Arc::new(ainb_hangar_daemon::health_stats::HealthStats::default()),
    };
    tokio::spawn(rpc::serve(listener, store.pool().clone(), health, broker));
    (socket_path, store, sink)
}

async fn apply_hook(
    store: &Store,
    sink: &EventSink,
    event_id: &str,
    provider: &str,
    session_id: &str,
    event_type: &str,
    payload: &serde_json::Value,
    observed_at: i64,
) {
    fleet::apply_hook(
        store.pool(),
        sink,
        HookObservation {
            event_id: event_id.to_string(),
            provider,
            provider_session_id: session_id,
            cwd: "/work/shared",
            event_type,
            payload,
            observed_at,
        },
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn snapshot_keeps_same_cwd_authoritative_provider_sessions_distinct() {
    let dir = tempfile::tempdir().unwrap();
    let (socket, store, sink) = start_server(dir.path()).await;
    let payload = serde_json::json!({ "source": "hook" });
    apply_hook(
        &store,
        &sink,
        "claude-start",
        "claude",
        "shared-id",
        "SessionStart",
        &payload,
        100,
    )
    .await;
    apply_hook(
        &store,
        &sink,
        "codex-start",
        "codex",
        "shared-id",
        "SessionStart",
        &payload,
        101,
    )
    .await;

    let mut client = Client::connect(&socket).await;
    client.auth_from_file(dir.path()).await;
    let response = client.call(methods::FLEET_SNAPSHOT, serde_json::json!({})).await;
    assert!(
        response["error"].is_null(),
        "fleet/snapshot must ack: {response}"
    );
    let sessions = response["result"]["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 2, "same cwd must not merge providers");
    assert_eq!(sessions[0]["session_key"], "claude:shared-id");
    assert_eq!(sessions[0]["provider"], "claude");
    assert_eq!(sessions[0]["provenance"], "authoritative");
    assert_eq!(sessions[1]["session_key"], "codex:shared-id");
    assert_eq!(sessions[1]["provider"], "codex");
    assert_eq!(sessions[0]["cwd"], sessions[1]["cwd"]);
    assert_eq!(response["result"]["head_revision"], 2);
}

#[tokio::test]
async fn snapshot_exposes_active_claude_subagent_work() {
    let dir = tempfile::tempdir().unwrap();
    let (socket, store, sink) = start_server(dir.path()).await;
    apply_hook(
        &store,
        &sink,
        "claude-session-start",
        "claude",
        "parent",
        "SessionStart",
        &serde_json::json!({}),
        100,
    )
    .await;
    apply_hook(
        &store,
        &sink,
        "claude-subagent-start",
        "claude",
        "parent",
        "SubagentStart",
        &serde_json::json!({ "payload": { "agent_id": "child-1" } }),
        200,
    )
    .await;

    let mut client = Client::connect(&socket).await;
    client.auth_from_file(dir.path()).await;
    let snapshot = client.call(methods::FLEET_SNAPSHOT, serde_json::json!({})).await;
    let session = &snapshot["result"]["sessions"][0];
    assert_eq!(session["session_key"], "claude:parent");
    assert_eq!(session["lifecycle"], "RUNNING");
    assert_eq!(session["active_work_count"], 1);
}

#[tokio::test]
async fn action_rejects_stale_or_wrong_request_and_replays_receipt() {
    let dir = tempfile::tempdir().unwrap();
    let (socket, store, sink) = start_server(dir.path()).await;
    let question = serde_json::json!({
        "questions": [{
            "id": "q-1",
            "question": "Deploy now?",
            "options": ["yes", "no"]
        }]
    });
    apply_hook(
        &store,
        &sink,
        "claude-question",
        "claude",
        "question-session",
        "AskUserQuestion",
        &question,
        200,
    )
    .await;

    let mut client = Client::connect(&socket).await;
    client.auth_from_file(dir.path()).await;
    let snapshot = client.call(methods::FLEET_SNAPSHOT, serde_json::json!({})).await;
    let session = &snapshot["result"]["sessions"][0];
    let version = session["version"].as_i64().unwrap();
    let fingerprint = session["current_request_fingerprint"].as_str().unwrap().to_string();
    let action = serde_json::json!({
        "session_key": "claude:question-session",
        "expected_version": version,
        "request_id": "answer-1",
        "action": {
            "action": "structured_answer",
            "request_fingerprint": fingerprint,
            "answers": [{
                "question_id": "q-1",
                "selected_options": ["yes"]
            }]
        }
    });

    let first = client.call(methods::FLEET_ACTION, action.clone()).await;
    assert!(first["error"].is_null(), "first action must ack: {first}");
    assert_eq!(first["result"]["receipt"]["request_id"], "answer-1");
    assert_eq!(first["result"]["receipt"]["status"], "FAILED");
    let replay = client.call(methods::FLEET_ACTION, action).await;
    assert_eq!(
        replay["result"]["receipt"], first["result"]["receipt"],
        "same request id and payload must return original receipt"
    );

    let stale = client
        .call(
            methods::FLEET_ACTION,
            serde_json::json!({
                "session_key": "claude:question-session",
                "expected_version": version + 1,
                "request_id": "answer-stale",
                "action": {
                    "action": "structured_answer",
                    "request_fingerprint": fingerprint,
                    "answers": [{ "question_id": "q-1", "selected_options": ["yes"] }]
                }
            }),
        )
        .await;
    assert!(
        stale["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("version")),
        "stale action must explain version rejection: {stale}"
    );

    let wrong_request = client
        .call(
            methods::FLEET_ACTION,
            serde_json::json!({
                "session_key": "claude:question-session",
                "expected_version": version,
                "request_id": "answer-wrong-request",
                "action": {
                    "action": "structured_answer",
                    "request_fingerprint": "fnv1a64:wrong",
                    "answers": [{ "question_id": "q-1", "selected_options": ["yes"] }]
                }
            }),
        )
        .await;
    assert!(
        wrong_request["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("fingerprint")),
        "wrong request must explain fingerprint rejection: {wrong_request}"
    );

    let stale_picker = client
        .call(
            methods::FLEET_ACTION,
            serde_json::json!({
                "session_key": "claude:question-session",
                "expected_version": version,
                "request_id": "picker-wrong-request",
                "action": {
                    "action": "verified_picker",
                    "request_fingerprint": "fnv1a64:wrong",
                    "key": "1"
                }
            }),
        )
        .await;
    assert!(
        stale_picker["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("fingerprint")),
        "picker must reject stale request fingerprint: {stale_picker}"
    );

    let stale_picker_version = client
        .call(
            methods::FLEET_ACTION,
            serde_json::json!({
                "session_key": "claude:question-session",
                "expected_version": version + 1,
                "request_id": "picker-stale-version",
                "action": {
                    "action": "verified_picker",
                    "request_fingerprint": fingerprint,
                    "key": "1"
                }
            }),
        )
        .await;
    assert!(
        stale_picker_version["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("version")),
        "picker must reject stale session version: {stale_picker_version}"
    );
}

#[tokio::test]
async fn claude_structured_action_delivers_exact_complete_answer_once() {
    use ainb_plugin_notifyd::broker::{
        BrokerState, StructuredResolution, client_await_structured, client_list,
        request_fingerprint,
    };
    use tokio::net::UnixListener;

    static ENV_LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    let _env_guard = ENV_LOCK.get_or_init(|| std::sync::Mutex::new(())).lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let approve_socket = dir.path().join("approve.sock");
    rpc::set_approve_socket_for_test(Some(approve_socket.clone()));
    let listener = UnixListener::bind(&approve_socket).unwrap();
    let broker_task = tokio::spawn(ainb_plugin_notifyd::broker::serve(
        listener,
        BrokerState::with_timeout(Duration::from_secs(10)),
    ));
    let (socket, store, sink) = start_server(dir.path()).await;
    let tool_input = serde_json::json!({
        "questions": [
            {
                "id": "q-1",
                "question": "Regions?",
                "multiSelect": true,
                "options": [{"label": "eu"}, {"label": "us"}]
            },
            {
                "id": "q-2",
                "question": "Mode?",
                "options": [{"label": "safe"}, {"label": "fast"}]
            }
        ]
    });
    let request_identity = serde_json::json!({
        "tool_use_id": "toolu-exact",
        "tool_input": tool_input,
    });
    let fingerprint = request_fingerprint(&request_identity);
    let hook = serde_json::json!({
        "matcher": "AskUserQuestion",
        "payload": {
            "tool_use_id": "toolu-exact",
            "tool_input": request_identity["tool_input"],
        }
    });
    apply_hook(
        &store,
        &sink,
        "claude-exact-question",
        "claude",
        "exact-session",
        "AskUserQuestion",
        &hook,
        220,
    )
    .await;

    let waiter_socket = approve_socket.clone();
    let waiter_fingerprint = fingerprint.clone();
    let questions = request_identity["tool_input"]["questions"].as_array().unwrap().clone();
    let waiter = tokio::task::spawn_blocking(move || {
        client_await_structured(
            &waiter_socket,
            "exact-session",
            &waiter_fingerprint,
            &questions,
            Duration::from_secs(5),
        )
    });
    for _ in 0..100 {
        let socket = approve_socket.clone();
        let registered = tokio::task::spawn_blocking(move || client_list(&socket))
            .await
            .unwrap()
            .unwrap_or_default()
            .iter()
            .any(|pending| pending.session_id == "exact-session");
        if registered {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let mut client = Client::connect(&socket).await;
    client.auth_from_file(dir.path()).await;
    let snapshot = client.call(methods::FLEET_SNAPSHOT, serde_json::json!({})).await;
    let version = snapshot["result"]["sessions"][0]["version"].as_i64().unwrap();
    assert_eq!(
        snapshot["result"]["sessions"][0]["current_request_fingerprint"],
        fingerprint
    );
    let action = serde_json::json!({
        "session_key": "claude:exact-session",
        "expected_version": version,
        "request_id": "answer-exact-once",
        "action": {
            "action": "structured_answer",
            "request_fingerprint": fingerprint,
            "answers": [
                {"question_id": "q-1", "selected_options": ["eu", "us"]},
                {"question_id": "q-2", "selected_options": ["safe"]}
            ]
        }
    });
    let first = client.call(methods::FLEET_ACTION, action.clone()).await;
    assert_eq!(first["result"]["receipt"]["status"], "DELIVERED");
    let replay = client.call(methods::FLEET_ACTION, action).await;
    assert_eq!(replay["result"]["receipt"], first["result"]["receipt"]);

    let resolution = waiter.await.unwrap();
    let StructuredResolution::Answered { answers } = resolution else {
        panic!("exact structured waiter was not answered");
    };
    assert_eq!(answers.len(), 2);
    assert_eq!(answers[0].question, "Regions?");
    assert_eq!(answers[0].selected_options, ["eu", "us"]);
    assert_eq!(answers[1].question, "Mode?");
    assert_eq!(answers[1].selected_options, ["safe"]);

    broker_task.abort();
    rpc::set_approve_socket_for_test(None);
}

#[tokio::test]
async fn subscribe_delivers_every_revision_after_snapshot_in_order() {
    let dir = tempfile::tempdir().unwrap();
    let (socket, store, sink) = start_server(dir.path()).await;
    let payload = serde_json::json!({ "source": "hook" });
    apply_hook(
        &store,
        &sink,
        "start-before-subscribe",
        "claude",
        "stream-session",
        "SessionStart",
        &payload,
        300,
    )
    .await;

    let mut client = Client::connect(&socket).await;
    client.auth_from_file(dir.path()).await;
    let ack = client
        .call(
            methods::FLEET_SUBSCRIBE,
            serde_json::json!({ "after_revision": 0 }),
        )
        .await;
    assert!(ack["error"].is_null(), "fleet/subscribe must ack: {ack}");
    let head = ack["result"]["snapshot"]["head_revision"].as_i64().unwrap();
    assert_eq!(head, 1);

    apply_hook(
        &store,
        &sink,
        "prompt-after-subscribe",
        "claude",
        "stream-session",
        "UserPromptSubmit",
        &payload,
        301,
    )
    .await;
    apply_hook(
        &store,
        &sink,
        "stop-after-subscribe",
        "claude",
        "stream-session",
        "Stop",
        &payload,
        302,
    )
    .await;

    let first = client.next_fleet_event().await;
    let second = client.next_fleet_event().await;
    assert_eq!(first["revision"], head + 1);
    assert_eq!(first["event_id"], "prompt-after-subscribe");
    assert_eq!(second["revision"], head + 2);
    assert_eq!(second["event_id"], "stop-after-subscribe");
}

#[tokio::test]
async fn broadcast_returns_ordered_receipt_for_every_target() {
    let dir = tempfile::tempdir().unwrap();
    let (socket, store, sink) = start_server(dir.path()).await;
    apply_hook(
        &store,
        &sink,
        "broadcast-session",
        "claude",
        "broadcast-live",
        "SessionStart",
        &serde_json::json!({}),
        400,
    )
    .await;
    let mut client = Client::connect(&socket).await;
    client.auth_from_file(dir.path()).await;
    let response = client
        .call(
            methods::FLEET_BROADCAST,
            serde_json::json!({
                "target_keys": ["claude:broadcast-live", "claude:missing"],
                "text": "status",
                "idempotency_key": "broadcast-all-receipts"
            }),
        )
        .await;
    assert!(
        response["error"].is_null(),
        "broadcast must return result: {response}"
    );
    let receipts = response["result"]["receipts"].as_array().unwrap();
    assert_eq!(receipts.len(), 2);
    assert_eq!(receipts[0]["session_key"], "claude:broadcast-live");
    assert_eq!(receipts[1]["session_key"], "claude:missing");
    assert_eq!(receipts[0]["status"], "REJECTED");
    assert_eq!(receipts[1]["status"], "REJECTED");
}
