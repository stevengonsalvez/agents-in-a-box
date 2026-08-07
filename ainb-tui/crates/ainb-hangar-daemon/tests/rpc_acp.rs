//! The ACP chat bus THROUGH THE RPC SURFACE: `fleet/acp_session_create`,
//! `fleet/message_send`, `fleet/message_list`, `fleet/transcript_subscribe` and
//! `fleet/action`, over a real Unix socket against the scripted fixture adapter.
//!
//! DISCLOSURE: every adapter here is `ainb-acp`'s `fake_acp_adapter` fixture,
//! never a real `claude-agent-acp` / `codex-acp`.
//!
//! The pool tests in `acp_pool.rs` drive `AcpPool` directly, which cannot catch
//! a handler that never reaches it. These drive the WIRE:
//!
//! * **R3** `acp_session_create` refuses an unknown provider, is idempotent per
//!   scope, writes BOTH rows under one key, and advertises exactly the wired
//!   action set.
//! * **I11** a broadcast to two ACP scopes threads one reply into each
//!   RECIPIENT'S own scope, and `message_list { origin_id }` returns exactly
//!   those two.
//! * **I12** a subscriber attached BEFORE the prompt receives chunk events
//!   DURING the turn: the first `acp.message` precedes `acp.turn_completed`.
//! * **R8** a permission round trips: attention row with option ids and the
//!   pending JSON-RPC id, `fleet/action` Approve, a REAL outcome on the wire,
//!   a fingerprint nothing raised refused, TWO concurrent asks both answerable,
//!   and an adapter death mid-permission closing the rows instead of leaving
//!   ghosts.
//! * **Receipts** an ACP chat leg leaves the same `fleet/receipt_get`-visible
//!   action receipt a tmux leg does, despite bypassing `execute_fleet_action`.
//!
//! The pool handle is process-wide (`acp_pool::install`), so every test here
//! holds [`POOL_LOCK`] for its whole body: two tests installing two pools
//! concurrently would route each other's prompts.

#![allow(
    clippy::too_many_lines,
    reason = "one END TO END scenario per test; splitting it hides the sequence under test"
)]
#![allow(
    clippy::significant_drop_tightening,
    reason = "the harness holds the process-wide pool lock for the whole test BY DESIGN"
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
use tokio::sync::{Mutex, MutexGuard};

/// Serialises the process-wide pool slot across this binary's tests.
static POOL_LOCK: Mutex<()> = Mutex::const_new(());

// ------------------------------------------------------------------ span tap

/// The process-wide tracing capture, so the pool's `acp.spawn` / `acp.turn`
/// spans are assertable rather than merely emitted. A span whose fields nothing
/// reads is a surface that rots silently.
#[derive(Clone, Default)]
struct SpanLog(Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for SpanLog {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("span log").extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for SpanLog {
    type Writer = Self;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// Install the capture once, BEFORE any pool work; spans close into the buffer.
fn span_log() -> &'static SpanLog {
    static LOG: std::sync::OnceLock<SpanLog> = std::sync::OnceLock::new();
    LOG.get_or_init(|| {
        let log = SpanLog::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(log.clone())
            .with_span_events(tracing_subscriber::fmt::format::FmtSpan::CLOSE)
            .with_max_level(tracing::Level::INFO)
            .with_ansi(false)
            .finish();
        let _ = tracing::subscriber::set_global_default(subscriber);
        log
    })
}

fn span_text() -> String {
    String::from_utf8_lossy(&span_log().0.lock().expect("span log")).into_owned()
}

/// Wait for `needle` to reach the span buffer, or fail with the whole buffer.
///
/// Spans are written on CLOSE, and the writer runs off the asserting task, so
/// the arrival is asynchronous. The assertion itself is unchanged: the exact
/// string must appear, this only stops a loaded runner from failing a surface
/// that is present.
async fn await_span(needle: &str, what: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let spans = span_text();
        if spans.contains(needle) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "{what}: {needle:?} never reached the span buffer: {spans}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

// ------------------------------------------------------------------ harness

fn fake_adapter() -> PathBuf {
    static BUILT: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    BUILT
        .get_or_init(|| {
            let mut dir = std::env::current_exe().expect("test binary path");
            dir.pop();
            if dir.ends_with("deps") {
                dir.pop();
            }
            // UNCONDITIONAL: `cargo test -p ainb-hangar-daemon` never rebuilds
            // another crate's binary, so a "build only when absent" guard
            // silently pins a stale fixture.
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
        })
        .clone()
}

/// One daemon on its own socket, with a pool wired to the fixture adapter and
/// installed in the process-wide slot.
struct Harness {
    _guard: MutexGuard<'static, ()>,
    _dir: tempfile::TempDir,
    dir: PathBuf,
    socket: PathBuf,
    store: Store,
    pool: Arc<AcpPool>,
}

impl Harness {
    async fn start(script: &[(&str, &str)], tune: impl FnOnce(&mut PoolConfig)) -> Self {
        let guard = POOL_LOCK.lock().await;
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open_in(dir.path()).await.expect("store");
        rpc::auth::ensure_socket_token(store.pool(), dir.path()).await.expect("token");
        let socket = rpc::socket_path_in(dir.path());
        let listener = rpc::bind(&socket).expect("bind");
        let broker = EventBroker::new();

        let mut config = PoolConfig::default();
        config.adapters.insert(
            ainb_acp::config::CLAUDE_ADAPTER.to_string(),
            ainb_acp::config::AdapterConfig::new(ainb_acp::config::CLAUDE_ADAPTER, "default")
                .command(fake_adapter())
                .extra_env(
                    script
                        .iter()
                        .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
                        .collect(),
                ),
        );
        tune(&mut config);
        let pool = AcpPool::new(store.clone(), broker.sink(), config);
        ainb_hangar_daemon::acp_pool::install(Arc::clone(&pool)).await;

        let health = DaemonHealth {
            socket_path: socket.to_string_lossy().into_owned(),
            pid: std::process::id(),
            started_at: Instant::now(),
            version: "0.1.0".to_string(),
            stats: Arc::new(ainb_hangar_daemon::health_stats::HealthStats::default()),
        };
        tokio::spawn(rpc::serve(listener, store.pool().clone(), health, broker));
        Self {
            _guard: guard,
            dir: dir.path().to_path_buf(),
            _dir: dir,
            socket,
            store,
            pool,
        }
    }

    async fn client(&self) -> Client {
        Client::authed(&self.dir, &self.socket).await
    }

    /// Create one ACP session over the wire and return its key plus scope.
    async fn create_session(&self, client: &mut Client, scope: Option<&str>) -> (String, String) {
        let mut params = serde_json::json!({
            "provider": ainb_acp::config::CLAUDE_ADAPTER,
            "cwd": self.dir.to_string_lossy(),
        });
        if let Some(scope) = scope {
            params["scope_key"] = serde_json::json!(scope);
        }
        let created = client.call(methods::FLEET_ACP_SESSION_CREATE, params).await;
        assert!(created["error"].is_null(), "{created}");
        (
            created["result"]["session_key"].as_str().expect("key").to_string(),
            created["result"]["scope_key"].as_str().expect("scope").to_string(),
        )
    }

    async fn delivery(&self, message_id: &str, session_key: &str) -> (String, Option<String>) {
        sqlx::query_as(
            "SELECT state, detail FROM fleet_message_delivery \
             WHERE message_id = ? AND session_key = ?",
        )
        .bind(message_id)
        .bind(session_key)
        .fetch_one(self.store.pool())
        .await
        .expect("delivery row")
    }

    async fn await_delivered(&self, message_id: &str, session_key: &str) -> String {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let (state, detail) = self.delivery(message_id, session_key).await;
            if state != "PENDING" {
                assert_eq!(state, "DELIVERED", "{session_key}: {detail:?}");
                return state;
            }
            assert!(
                Instant::now() < deadline,
                "{session_key} never resolved its leg"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// The session's only delivery leg, for the streaming test where the send
    /// response has deliberately not been read yet.
    async fn only_delivery(&self, session_key: &str) -> (String, Option<String>) {
        sqlx::query_as("SELECT state, detail FROM fleet_message_delivery WHERE session_key = ?")
            .bind(session_key)
            .fetch_one(self.store.pool())
            .await
            .expect("delivery row")
    }

    /// The session row's current optimistic-concurrency version, which every
    /// `fleet/action` must carry.
    async fn version(&self, session_key: &str) -> i64 {
        sqlx::query_scalar("SELECT version FROM fleet_session WHERE session_key = ?")
            .bind(session_key)
            .fetch_one(self.store.pool())
            .await
            .expect("session version")
    }

    /// Poll until the session has an OPEN attention row, and return it.
    async fn await_open_attention(&self, session_key: &str) -> (String, serde_json::Value) {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let row: Option<(String, String)> = sqlx::query_as(
                "SELECT id, payload FROM attention WHERE session_id = ? AND state = 'open'",
            )
            .bind(session_key)
            .fetch_optional(self.store.pool())
            .await
            .expect("attention query");
            if let Some((id, payload)) = row {
                return (id, serde_json::from_str(&payload).expect("payload json"));
            }
            assert!(
                Instant::now() < deadline,
                "no permission was ever raised for {session_key}"
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    async fn transcript_text(&self, session_key: &str) -> String {
        use ainb_hangar_store::repo::fleet_provider_event::FleetProviderEventRepo;

        FleetProviderEventRepo::list_by_session_after(self.store.pool(), session_key, 0, 500)
            .await
            .expect("transcript")
            .into_iter()
            .map(|row| row.raw_payload)
            .collect()
    }

    async fn finish(self) {
        ainb_hangar_daemon::acp_pool::uninstall().await;
    }
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

    async fn send(&mut self, method: &str, params: serde_json::Value) {
        self.next_id += 1;
        let request = RpcRequest {
            jsonrpc: ainb_hangar_proto::jsonrpc_version(),
            id: RpcId::Number(self.next_id),
            method: method.to_string(),
            params,
        };
        let body = serde_json::to_vec(&request).expect("encode request");
        let mut frame = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
        frame.extend_from_slice(&body);
        self.writer.write_all(&frame).await.expect("write frame");
        self.writer.flush().await.expect("flush");
    }

    async fn call(&mut self, method: &str, params: serde_json::Value) -> serde_json::Value {
        self.send(method, params).await;
        loop {
            let frame = self
                .read_frame(Duration::from_secs(30))
                .await
                .unwrap_or_else(|| panic!("no response to {method} within 30s"));
            if frame.get("id").is_some() {
                return frame;
            }
        }
    }

    async fn read_frame(&mut self, timeout: Duration) -> Option<serde_json::Value> {
        tokio::time::timeout(timeout, self.read_frame_inner()).await.ok()
    }

    async fn read_frame_inner(&mut self) -> serde_json::Value {
        use tokio::io::AsyncBufReadExt;

        let mut content_length = None;
        loop {
            let mut line = String::new();
            let read = self.reader.read_line(&mut line).await.expect("read header");
            assert!(read > 0, "connection closed while awaiting frame");
            let line = line.trim_end_matches("\r\n");
            if line.is_empty() {
                let mut body = vec![0_u8; content_length.expect("Content-Length header")];
                self.reader.read_exact(&mut body).await.expect("read body");
                return serde_json::from_slice(&body).expect("decode frame");
            }
            if let Some((name, value)) = line.split_once(':') {
                if name.trim().eq_ignore_ascii_case("Content-Length") {
                    content_length = value.trim().parse().ok();
                }
            }
        }
    }
}

// -------------------------------------------------------------------- tests

/// R3, the whole create contract in one socket session: capability advertised,
/// unknown provider refused, idempotent per scope, BOTH rows under one key,
/// and capabilities that match exactly the actions Phase 5 wired.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn acp_session_create_is_gated_idempotent_and_transactional() {
    use ainb_hangar_proto::fleet::{
        FLEET_CAPABILITY_ACP_SPAWN, FLEET_PROTOCOL_CAPABILITY_IDS, FleetCapabilities,
    };

    let harness = Harness::start(&[("FAKE_ACP_CHUNKS", "1")], |_| {}).await;
    let mut client = harness.client().await;

    // Capability-gated: the method is served only because its id is in the
    // advertised catalogue, which `fleet/negotiate` publishes.
    assert!(FLEET_PROTOCOL_CAPABILITY_IDS.contains(&FLEET_CAPABILITY_ACP_SPAWN));
    let negotiated = client
        .call(
            methods::FLEET_NEGOTIATE,
            serde_json::json!({
                "client_name": "rpc_acp test",
                "client_version": "0.0.0",
                "read_versions": { "min": 1, "max": 99 },
                "write_versions": { "min": 1, "max": 99 },
            }),
        )
        .await;
    assert!(negotiated["error"].is_null(), "{negotiated}");
    let advertised: Vec<&str> = negotiated["result"]["capability_ids"]
        .as_array()
        .expect("capability list")
        .iter()
        .filter_map(serde_json::Value::as_str)
        .collect();
    assert!(
        advertised.contains(&FLEET_CAPABILITY_ACP_SPAWN),
        "the daemon serves acp_session_create, so it must advertise the capability: {advertised:?}"
    );

    // An unknown adapter token is refused at the REGISTRY, not silently minted:
    // the store only length-checks `provider`, so this handler is the one place
    // a typo can be caught.
    let rejected = client
        .call(
            methods::FLEET_ACP_SESSION_CREATE,
            serde_json::json!({ "provider": "gpt-agent-acp", "cwd": harness.dir.to_string_lossy() }),
        )
        .await;
    assert_eq!(
        rejected["error"]["code"], -32602,
        "an unknown provider is invalid_params: {rejected}"
    );
    assert!(
        rejected["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("gpt-agent-acp"),
        "{rejected}"
    );
    let orphans: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM fleet_acp_session")
        .fetch_one(harness.store.pool())
        .await
        .expect("count");
    assert_eq!(orphans, 0, "a refused create must persist nothing");

    // Idempotent per LIVE scope: the second create returns the SAME key rather
    // than minting a second session for one chat scope.
    let (first, scope) = harness.create_session(&mut client, Some("session:acp-fixed")).await;
    let (second, scope_again) =
        harness.create_session(&mut client, Some("session:acp-fixed")).await;
    assert_eq!(
        first, second,
        "a replayed create must not mint a second key"
    );
    assert_eq!(scope, "session:acp-fixed");
    assert_eq!(scope_again, scope);
    let sessions: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM fleet_acp_session")
        .fetch_one(harness.store.pool())
        .await
        .expect("count");
    assert_eq!(sessions, 1, "one live session per scope");

    // Idempotent only for the SAME adapter. Answering a `codex-acp` create with
    // the live CLAUDE key would hand the caller a session that prompts a
    // different agent than it believes it is driving, and every later
    // `message_send` to that key would misroute in silence.
    let clash = client
        .call(
            methods::FLEET_ACP_SESSION_CREATE,
            serde_json::json!({
                "provider": ainb_acp::config::CODEX_ADAPTER,
                "cwd": harness.dir.to_string_lossy(),
                "scope_key": "session:acp-fixed",
            }),
        )
        .await;
    assert_eq!(
        clash["error"]["code"], -32602,
        "a scope held by another provider's live session is refused: {clash}"
    );
    assert!(
        clash["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains(ainb_acp::config::CLAUDE_ADAPTER),
        "the refusal names the incumbent: {clash}"
    );
    let (still, live): (i64, String) = sqlx::query_as(
        "SELECT COUNT(*), MIN(provider) FROM fleet_acp_session WHERE scope_key = 'session:acp-fixed'",
    )
    .fetch_one(harness.store.pool())
    .await
    .expect("count");
    assert_eq!(still, 1, "the refusal persisted nothing");
    assert_eq!(
        live,
        ainb_acp::config::CLAUDE_ADAPTER,
        "and left the incumbent untouched"
    );

    // BOTH rows, under ONE key, from ONE transaction.
    let (provider, cwd, state): (String, String, String) =
        sqlx::query_as("SELECT provider, cwd, state FROM fleet_acp_session WHERE session_key = ?")
            .bind(&first)
            .fetch_one(harness.store.pool())
            .await
            .expect("acp row");
    assert_eq!(provider, ainb_acp::config::CLAUDE_ADAPTER);
    assert_eq!(cwd, harness.dir.to_string_lossy());
    assert_eq!(state, "IDLE");
    let (fleet_provider, capabilities, management): (String, String, String) = sqlx::query_as(
        "SELECT provider, capabilities, management_state FROM fleet_session WHERE session_key = ?",
    )
    .bind(&first)
    .fetch_one(harness.store.pool())
    .await
    .expect("fleet row");
    assert_eq!(
        fleet_provider, "acp",
        "the WIRE token is `acp`; the concrete adapter lives on the ACP row"
    );
    assert_eq!(management, "MANAGED");

    // The capability JSON is exactly the wired action set, field for field. A
    // drift here is an operator offered a button the daemon cannot honour.
    let advertised: FleetCapabilities = serde_json::from_str(&capabilities).expect("capabilities");
    assert_eq!(
        advertised,
        FleetCapabilities {
            structured_answer: true,
            structured_dismiss: false,
            approvals: true,
            approval_session: false,
            send_prompt: true,
            continue_turn: false,
            retry: false,
            interrupt: true,
            start: false,
            stop: true,
            restart: false,
            kill: true,
            archive: false,
            tmux_attach: false,
            tmux_text: false,
            verified_picker: false,
        },
        "capabilities must match exactly the arms `execute_acp_action` implements"
    );

    harness.finish().await;
}

/// I11 over the wire: ONE `fleet/message_send` to two ACP scopes writes two
/// delivery legs, produces one reply per recipient in that RECIPIENT'S own
/// scope threaded to the broadcast id, and `message_list { origin_id }` returns
/// exactly those two.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_broadcast_to_two_acp_scopes_threads_one_reply_into_each() {
    let harness = Harness::start(
        &[("FAKE_ACP_CHUNKS", "1"), ("FAKE_ACP_ECHO_PROMPT", "1")],
        |_| {},
    )
    .await;
    let mut client = harness.client().await;
    let (one, scope_one) = harness.create_session(&mut client, None).await;
    let (two, scope_two) = harness.create_session(&mut client, None).await;
    assert_ne!(scope_one, scope_two);

    let sent = client
        .call(
            methods::FLEET_MESSAGE_SEND,
            serde_json::json!({
                "targets": [one, two],
                "text": "standup",
                "request_id": "req-acp-broadcast",
            }),
        )
        .await;
    assert!(sent["error"].is_null(), "{sent}");
    let broadcast_id = sent["result"]["message_id"].as_str().expect("message id").to_string();
    let deliveries = sent["result"]["deliveries"].as_array().expect("deliveries");
    assert_eq!(deliveries.len(), 2, "one leg per recipient: {sent}");
    for leg in deliveries {
        assert_eq!(
            leg["state"], "PENDING",
            "an ACP leg resolves at TURN END, not at write-ack: {sent}"
        );
    }

    harness.await_delivered(&broadcast_id, &one).await;
    harness.await_delivered(&broadcast_id, &two).await;

    let listed = client
        .call(
            methods::FLEET_MESSAGE_LIST,
            serde_json::json!({ "origin_id": broadcast_id, "limit": 50 }),
        )
        .await;
    assert!(listed["error"].is_null(), "{listed}");
    let replies = listed["result"]["messages"].as_array().expect("messages");
    assert_eq!(
        replies.len(),
        2,
        "the thread join returns exactly the two agent replies: {listed}"
    );
    for (session_key, scope_key) in [(&one, &scope_one), (&two, &scope_two)] {
        let reply = replies
            .iter()
            .find(|row| row["sender"] == serde_json::json!(session_key))
            .unwrap_or_else(|| panic!("no reply from {session_key}: {listed}"));
        assert_eq!(
            reply["scope_key"],
            serde_json::json!(scope_key),
            "a broadcast reply lands in the RECIPIENT's scope, never the broadcast scope"
        );
        assert_eq!(reply["origin_message_id"], serde_json::json!(broadcast_id));
        assert_eq!(reply["kind"], "agent");
        assert!(
            reply["body"].as_str().unwrap_or_default().contains("echo:standup"),
            "{reply}"
        );
    }

    harness.finish().await;
}

/// I12 over the wire: a subscriber attached BEFORE the prompt receives chunk
/// events DURING the fake adapter's turn, with the first `acp.message` strictly
/// ahead of `acp.turn_completed`.
///
/// The script crosses a KIND boundary early and paces itself: the writer
/// coalesces contiguous same-kind text until 4 KiB or a kind change, so a
/// boundary is what makes the first row commit mid-turn.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_subscriber_attached_before_the_prompt_sees_chunks_during_the_turn() {
    let script_dir = tempfile::tempdir().expect("script dir");
    let script_path = script_dir.path().join("paced.ndjson");
    let script = [
        r#"{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"early "}}"#,
        r#"{"sessionUpdate":"agent_thought_chunk","content":{"type":"text","text":"thinking "}}"#,
        r#"{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"late "}}"#,
        r#"{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"later "}}"#,
    ]
    .join("\n");
    std::fs::write(&script_path, script).expect("write script");

    let harness = Harness::start(
        &[
            ("FAKE_ACP_SCRIPT", script_path.to_str().expect("utf8 path")),
            ("FAKE_ACP_CHUNK_DELAY_MS", "150"),
        ],
        |config| config.writer.flush_interval = Duration::from_millis(25),
    )
    .await;
    let mut client = harness.client().await;
    let (session_key, _scope) = harness.create_session(&mut client, None).await;

    // BEFORE the prompt: nothing has been committed yet, so the head cursor is
    // empty and every chunk this test sees is live, not replayed.
    let subscribed = client
        .call(
            methods::FLEET_TRANSCRIPT_SUBSCRIBE,
            serde_json::json!({ "session_key": session_key }),
        )
        .await;
    assert!(subscribed["error"].is_null(), "{subscribed}");
    assert!(
        subscribed["result"]["head_order"].is_null(),
        "the subscription attached before any chunk existed: {subscribed}"
    );

    client
        .send(
            methods::FLEET_MESSAGE_SEND,
            serde_json::json!({
                "targets": [session_key],
                "text": "stream",
                "request_id": "req-acp-stream",
            }),
        )
        .await;

    // Read notifications in arrival order until the turn's completion marker,
    // then assert an agent chunk genuinely preceded it on the SAME stream.
    let mut kinds: Vec<String> = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut mid_turn_state = None;
    loop {
        let frame = client
            .read_frame(Duration::from_secs(30))
            .await
            .expect("a transcript event within the turn");
        if frame.get("id").is_some() || frame["method"] != "fleet/transcript_event" {
            continue;
        }
        let chunk = &frame["params"]["chunk"];
        assert_eq!(chunk["session_key"], serde_json::json!(session_key));
        let kind = chunk["event_type"].as_str().unwrap_or_default().to_string();
        // The first AGENT chunk: sample the delivery right now, because
        // "during the turn" means the leg has not resolved yet.
        if kind == "acp.message" && mid_turn_state.is_none() {
            mid_turn_state = Some(harness.only_delivery(&session_key).await);
        }
        let done = kind == "acp.turn_completed";
        kinds.push(kind);
        if done {
            break;
        }
        assert!(Instant::now() < deadline, "the turn never completed");
    }

    let first_chunk = kinds
        .iter()
        .position(|kind| kind == "acp.message")
        .unwrap_or_else(|| panic!("no agent chunk was ever streamed: {kinds:?}"));
    let completed = kinds
        .iter()
        .position(|kind| kind == "acp.turn_completed")
        .expect("the completion marker");
    assert!(
        first_chunk < completed,
        "the first chunk must arrive BEFORE the turn completes: {kinds:?}"
    );
    assert_eq!(
        mid_turn_state.map(|leg| leg.0),
        Some("PENDING".to_string()),
        "and the delivery was still open when it did"
    );

    harness.finish().await;
}

/// R8 over the wire: the adapter asks, an attention row carries the option ids
/// and the pending JSON-RPC id, `fleet/action` Approve reaches THAT id, and the
/// adapter reports a real outcome, not `cancelled`. A stale fingerprint is
/// refused rather than applied to whatever ask is current.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_permission_round_trips_through_fleet_action() {
    let harness = Harness::start(
        &[
            ("FAKE_ACP_PERMISSION_SESSIONS", "*"),
            ("FAKE_ACP_CHUNKS", "1"),
        ],
        |_| {},
    )
    .await;
    let mut client = harness.client().await;
    let (session_key, _scope) = harness.create_session(&mut client, None).await;

    let sent = client
        .call(
            methods::FLEET_MESSAGE_SEND,
            serde_json::json!({
                "targets": [session_key],
                "text": "rm -rf /tmp/fixture",
                "request_id": "req-acp-permission",
            }),
        )
        .await;
    assert!(sent["error"].is_null(), "{sent}");
    let message_id = sent["result"]["message_id"].as_str().expect("message id").to_string();

    // The attention row an operator actually sees: the adapter's option ids AND
    // the pending JSON-RPC id the answer has to reach.
    let (attention_id, payload) = harness.await_open_attention(&session_key).await;
    let options: Vec<&str> = payload["options"]
        .as_array()
        .expect("options")
        .iter()
        .filter_map(|option| option["optionId"].as_str())
        .collect();
    assert_eq!(
        options,
        vec!["allow-once", "reject-once"],
        "the row carries the adapter's own option ids: {payload}"
    );
    assert!(
        payload["rpcId"].as_i64().is_some_and(|id| id >= 9000),
        "the row carries the pending JSON-RPC id: {payload}"
    );
    let fingerprint = payload["requestFingerprint"].as_str().expect("fingerprint").to_string();

    // A fingerprint no ask carries is refused, so an operator answering a screen
    // that has moved on cannot approve the current ask. The refusal comes from
    // the POOL's parked map, not from the session row's single fingerprint slot:
    // an ACP session can be blocked on SEVERAL asks at once and the row can name
    // only one of them, which is why the row-equality gate is off for ACP (see
    // `two_parked_permissions_are_both_answerable_over_the_wire`).
    let stale = client
        .call(
            methods::FLEET_ACTION,
            serde_json::json!({
                "session_key": session_key,
                "expected_version": harness.version(&session_key).await,
                "request_id": "req-acp-stale-answer",
                "action": {
                    "action": "approve",
                    "request_fingerprint": "0000000000000000000000000000000000000000000000000000000000000000",
                },
            }),
        )
        .await;
    assert!(stale["error"].is_null(), "{stale}");
    assert_eq!(
        stale["result"]["receipt"]["status"], "FAILED",
        "a fingerprint nothing raised must be refused: {stale}"
    );
    assert!(
        stale["result"]["receipt"]["detail"]
            .as_str()
            .unwrap_or_default()
            .contains("no longer waiting"),
        "and say why: {stale}"
    );
    let (still_open,): (String,) = sqlx::query_as("SELECT state FROM attention WHERE id = ?")
        .bind(&attention_id)
        .fetch_one(harness.store.pool())
        .await
        .expect("attention row");
    assert_eq!(still_open, "open", "the refusal left the ask untouched");

    let approved = client
        .call(
            methods::FLEET_ACTION,
            serde_json::json!({
                "session_key": session_key,
                "expected_version": harness.version(&session_key).await,
                "request_id": "req-acp-answer",
                "action": { "action": "approve", "request_fingerprint": fingerprint },
            }),
        )
        .await;
    assert!(approved["error"].is_null(), "{approved}");
    assert_eq!(
        approved["result"]["receipt"]["status"], "DELIVERED",
        "the answer reached the adapter: {approved}"
    );
    assert!(
        approved["result"]["receipt"]["detail"]
            .as_str()
            .unwrap_or_default()
            .contains("allow-once"),
        "and the receipt names the option that was taken: {approved}"
    );

    harness.await_delivered(&message_id, &session_key).await;
    // REAL outcome on the wire: the fixture echoes what it was told, so
    // `selected:allow-once` proves the answer landed on the original request id
    // rather than the connection timing out into `cancelled`.
    let text = harness.transcript_text(&session_key).await;
    assert!(
        text.contains("permission:selected:allow-once"),
        "the adapter observed a real selection: {text}"
    );
    assert!(
        !text.contains("permission:cancelled"),
        "and never a cancellation: {text}"
    );

    // Nothing is left waiting.
    let (state, answered_by): (String, Option<String>) =
        sqlx::query_as("SELECT state, answered_by FROM attention WHERE id = ?")
            .bind(&attention_id)
            .fetch_one(harness.store.pool())
            .await
            .expect("attention row");
    assert_eq!(state, "answered");
    assert_eq!(answered_by.as_deref(), Some("operator"));
    let (attention_state, current): (String, Option<String>) = sqlx::query_as(
        "SELECT attention_state, current_request_fingerprint FROM fleet_session \
         WHERE session_key = ?",
    )
    .bind(&session_key)
    .fetch_one(harness.store.pool())
    .await
    .expect("fleet row");
    assert_eq!(attention_state, "NONE");
    assert_eq!(current, None);

    harness.finish().await;
}

/// R8 with TWO asks outstanding, over the wire: `fleet/action` answers the
/// OLDER one and the adapter is genuinely unblocked.
///
/// This is the leg the pool-level twin cannot prove. `fleet/action` used to
/// validate the answer's fingerprint against
/// `fleet_session.current_request_fingerprint`, which `raise_permission`
/// overwrites per ask. With two parked, that made every ask but the newest
/// unanswerable BEFORE the pool was ever consulted: `invalid_params` for the
/// older fingerprint, `NotWaiting` for the newer one once it was spent, and an
/// adapter blocked until the 30 minute turn deadline either way.
///
/// DISCLOSURE: the concurrency comes from the fixture's
/// `FAKE_ACP_PERMISSION_COUNT`; a real adapter gets there with parallel tool
/// calls.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_parked_permissions_are_both_answerable_over_the_wire() {
    let harness = Harness::start(
        &[
            ("FAKE_ACP_PERMISSION_SESSIONS", "*"),
            ("FAKE_ACP_PERMISSION_COUNT", "2"),
            ("FAKE_ACP_CHUNKS", "1"),
        ],
        |_| {},
    )
    .await;
    let mut client = harness.client().await;
    let (session_key, _scope) = harness.create_session(&mut client, None).await;

    let sent = client
        .call(
            methods::FLEET_MESSAGE_SEND,
            serde_json::json!({
                "targets": [session_key],
                "text": "rm -rf /tmp/fixture",
                "request_id": "req-acp-two-permissions",
            }),
        )
        .await;
    assert!(sent["error"].is_null(), "{sent}");
    let message_id = sent["result"]["message_id"].as_str().expect("message id").to_string();

    // Both asks, in raise order: the attention id is a ULID.
    let deadline = Instant::now() + Duration::from_secs(30);
    let asks: Vec<(String, String)> = loop {
        let rows: Vec<(String, String)> = sqlx::query_as(
            "SELECT id, payload FROM attention \
             WHERE session_id = ? AND state = 'open' ORDER BY id ASC",
        )
        .bind(&session_key)
        .fetch_all(harness.store.pool())
        .await
        .expect("attention query");
        if rows.len() == 2 {
            break rows
                .into_iter()
                .map(|(id, payload)| {
                    let payload: serde_json::Value =
                        serde_json::from_str(&payload).expect("payload json");
                    let fingerprint =
                        payload["requestFingerprint"].as_str().expect("fingerprint").to_string();
                    (id, fingerprint)
                })
                .collect();
        }
        assert!(
            Instant::now() < deadline,
            "the adapter never raised two concurrent permissions"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    };

    // OLDEST first: the fingerprint the session row is NOT carrying, and the
    // exact answer the old gate refused as stale.
    for (index, (attention_id, fingerprint)) in asks.iter().enumerate() {
        let approved = client
            .call(
                methods::FLEET_ACTION,
                serde_json::json!({
                    "session_key": session_key,
                    "expected_version": harness.version(&session_key).await,
                    "request_id": format!("req-acp-two-answer-{index}"),
                    "action": { "action": "approve", "request_fingerprint": fingerprint },
                }),
            )
            .await;
        assert!(approved["error"].is_null(), "ask {index}: {approved}");
        assert_eq!(
            approved["result"]["receipt"]["status"], "DELIVERED",
            "ask {index} must reach its adapter request: {approved}"
        );
        let (state,): (String,) = sqlx::query_as("SELECT state FROM attention WHERE id = ?")
            .bind(attention_id)
            .fetch_one(harness.store.pool())
            .await
            .expect("attention row");
        assert_eq!(state, "answered", "ask {index} did not close");
    }

    harness.await_delivered(&message_id, &session_key).await;
    let text = harness.transcript_text(&session_key).await;
    assert_eq!(
        text.matches("permission:selected:allow-once").count(),
        2,
        "both blocked adapter requests observed a real selection: {text}"
    );
    assert!(
        !text.contains("permission:cancelled"),
        "and neither timed out into a cancellation: {text}"
    );
    let (attention_state, current): (String, Option<String>) = sqlx::query_as(
        "SELECT attention_state, current_request_fingerprint FROM fleet_session \
         WHERE session_key = ?",
    )
    .bind(&session_key)
    .fetch_one(harness.store.pool())
    .await
    .expect("fleet row");
    assert_eq!(attention_state, "NONE");
    assert_eq!(current, None);

    harness.finish().await;
}

/// A chat delivery to an ACP recipient leaves the SAME action receipt a tmux
/// recipient's leg leaves, so `fleet/receipt_get` on the leg's request id
/// answers for both.
///
/// The ACP leg deliberately bypasses `execute_fleet_action` (its prompt stays
/// PENDING until turn end), and it used to bypass the receipt with it: one
/// `fleet/message_send` produced a receipt for a tmux target and nothing at all
/// for an ACP one, which is only discoverable by querying and getting `null`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_acp_chat_leg_writes_its_action_receipt() {
    let harness = Harness::start(&[("FAKE_ACP_CHUNKS", "1")], |_| {}).await;
    let mut client = harness.client().await;
    let (session_key, _scope) = harness.create_session(&mut client, None).await;

    let sent = client
        .call(
            methods::FLEET_MESSAGE_SEND,
            serde_json::json!({
                "targets": [session_key],
                "text": "hello",
                "request_id": "req-acp-leg-receipt",
            }),
        )
        .await;
    assert!(sent["error"].is_null(), "{sent}");
    let message_id = sent["result"]["message_id"].as_str().expect("message id").to_string();
    harness.await_delivered(&message_id, &session_key).await;

    // The leg's request id is minted inside the daemon, so read it back rather
    // than reimplementing the fingerprint here.
    let (request_id, status, detail): (String, String, Option<String>) = sqlx::query_as(
        "SELECT request_id, status, detail FROM fleet_action_receipt WHERE session_key = ?",
    )
    .bind(&session_key)
    .fetch_one(harness.store.pool())
    .await
    .expect("the acp leg wrote exactly one action receipt");
    assert!(
        request_id.starts_with("message:"),
        "the receipt belongs to the delivery leg: {request_id}"
    );
    assert_eq!(
        status, "DELIVERED",
        "TERMINAL, like the operator SendPrompt arm: nothing reopens an action \
         receipt, so a PENDING one would be read as UNKNOWN mid-turn"
    );
    assert_eq!(
        detail.as_deref(),
        Some(format!("acp_queued; message {message_id}").as_str()),
        "and it names where the real outcome lives"
    );

    let fetched = client
        .call(
            methods::FLEET_RECEIPT_GET,
            serde_json::json!({ "request_id": request_id }),
        )
        .await;
    assert!(fetched["error"].is_null(), "{fetched}");
    assert_eq!(
        fetched["result"]["receipt"]["status"], "DELIVERED",
        "fleet/receipt_get answers for an ACP leg, not just a tmux one: {fetched}"
    );

    harness.finish().await;
}

/// R8's failure leg: the adapter DIES while a permission is parked. Convergence
/// closes the ask, so no ghost row survives for an operator to click forever,
/// and the delivery gets its enumerated terminal outcome.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_adapter_death_mid_permission_leaves_no_ghost_row() {
    let harness = Harness::start(
        &[
            ("FAKE_ACP_PERMISSION_SESSIONS", "*"),
            ("FAKE_ACP_CHUNKS", "1"),
        ],
        |_| {},
    )
    .await;
    let mut client = harness.client().await;
    let (session_key, _scope) = harness.create_session(&mut client, None).await;

    let sent = client
        .call(
            methods::FLEET_MESSAGE_SEND,
            serde_json::json!({
                "targets": [session_key],
                "text": "rm -rf /tmp/fixture",
                "request_id": "req-acp-permission-death",
            }),
        )
        .await;
    assert!(sent["error"].is_null(), "{sent}");
    let message_id = sent["result"]["message_id"].as_str().expect("message id").to_string();
    let (attention_id, _payload) = harness.await_open_attention(&session_key).await;

    assert!(
        harness.pool.kill_provider(ainb_acp::config::CLAUDE_ADAPTER).await,
        "the fixture process was live"
    );

    // The ask closes on its own, with convergence named as the answerer.
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let (state, answered_by): (String, Option<String>) =
            sqlx::query_as("SELECT state, answered_by FROM attention WHERE id = ?")
                .bind(&attention_id)
                .fetch_one(harness.store.pool())
                .await
                .expect("attention row");
        if state != "open" {
            assert_eq!(state, "answered", "a ghost ask must be closed, not deleted");
            assert_eq!(answered_by.as_deref(), Some("hangar-converge"));
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the permission stayed open after its adapter died"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let (state, detail) = loop {
        let (state, detail) = harness.delivery(&message_id, &session_key).await;
        if state != "PENDING" {
            break (state, detail);
        }
        assert!(Instant::now() < deadline, "the leg never resolved");
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert!(
        state == "UNKNOWN" || state == "FAILED",
        "{state} ({detail:?})"
    );
    assert!(
        detail.as_deref().is_some_and(|detail| detail.starts_with("adapter_exit")),
        "{detail:?}"
    );
    let (attention_state, current): (String, Option<String>) = sqlx::query_as(
        "SELECT attention_state, current_request_fingerprint FROM fleet_session \
         WHERE session_key = ?",
    )
    .bind(&session_key)
    .fetch_one(harness.store.pool())
    .await
    .expect("fleet row");
    assert_eq!(
        attention_state, "NONE",
        "the snapshot must stop showing an approval nobody can answer"
    );
    assert_eq!(current, None, "and its fingerprint must be cleared");

    harness.finish().await;
}

/// The Observability row for Phase 5, over the socket: `hangar/daemon_health`
/// carries the pool's per-scope queue depth, open-turn age, transcript growth
/// and per-provider breaker state, and the `acp.spawn` / `acp.turn` spans carry
/// the fields the runbook reads.
///
/// The plan's hard rule is that every field on this surface is exercised by a
/// test asserting it is POPULATED. Without this, `acp_pool: None` (or a
/// permanently zero `queue_depth`) keeps every suite green while the pane that
/// answers "why is the copilot stuck" renders empty in production.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn daemon_health_reports_the_acp_pool_and_the_spans_carry_their_fields() {
    // Installed BEFORE any pool work, so the spawn and turn spans land in it.
    span_log();
    let harness = Harness::start(
        &[("FAKE_ACP_CHUNKS", "2"), ("FAKE_ACP_HANG_PROMPTS", "hang")],
        |_| {},
    )
    .await;
    let mut client = harness.client().await;
    let (session_key, scope_key) = harness.create_session(&mut client, None).await;

    // One turn that COMPLETES, so the pane has transcript bytes to report and
    // the turn span has an outcome to record.
    let sent = client
        .call(
            methods::FLEET_MESSAGE_SEND,
            serde_json::json!({
                "targets": [&session_key],
                "text": "hello",
                "request_id": "req-health-warmup",
            }),
        )
        .await;
    assert!(sent["error"].is_null(), "{sent}");
    let first = sent["result"]["message_id"].as_str().expect("message id").to_string();
    harness.await_delivered(&first, &session_key).await;

    // Then a turn that hangs, with a second prompt QUEUED behind it: exactly
    // the stuck-copilot shape the pane exists for.
    for text in ["hang", "queued-behind"] {
        let sent = client
            .call(
                methods::FLEET_MESSAGE_SEND,
                serde_json::json!({
                    "targets": [&session_key],
                    "text": text,
                    "request_id": format!("req-health-{text}"),
                }),
            )
            .await;
        assert!(sent["error"].is_null(), "{sent}");
    }

    let deadline = Instant::now() + Duration::from_secs(30);
    let (processes, session) = loop {
        let health = client
            .call(
                methods::HANGAR_DAEMON_HEALTH,
                serde_json::json!({ "workspace_id": "default" }),
            )
            .await;
        assert!(health["error"].is_null(), "{health}");
        let acp = &health["result"]["acp_pool"];
        assert!(
            !acp.is_null(),
            "a daemon with a pool installed must report it: {health}"
        );
        let row = acp["sessions"]
            .as_array()
            .and_then(|rows| rows.iter().find(|row| row["session_key"] == *session_key));
        if let Some(row) = row {
            if row["turn_open"] == serde_json::json!(true)
                && row["queue_depth"].as_u64().unwrap_or(0) >= 1
            {
                break (acp["processes"].clone(), row.clone());
            }
        }
        assert!(
            Instant::now() < deadline,
            "the pool never reported an open turn with a queued prompt: {health}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };

    // The session row, field by field.
    assert_eq!(session["scope_key"], serde_json::json!(scope_key));
    assert_eq!(
        session["provider"],
        serde_json::json!(ainb_acp::config::CLAUDE_ADAPTER)
    );
    assert_eq!(session["state"], "ACTIVE");
    assert_eq!(session["queue_capacity"], serde_json::json!(32));
    assert!(
        session["turn_age_ms"].as_i64().is_some(),
        "an open turn must report its age, which is how a wedged turn is spotted: {session}"
    );
    assert!(
        session["transcript_bytes"].as_u64().unwrap_or(0) > 0,
        "committed transcript bytes are the growth signal that stands in for backpressure: {session}"
    );

    // The process row, including the breaker that is still closed.
    let processes = processes.as_array().expect("process rows").clone();
    assert_eq!(
        processes.len(),
        1,
        "one process per PROVIDER: {processes:?}"
    );
    let process = &processes[0];
    assert_eq!(
        process["provider"],
        serde_json::json!(ainb_acp::config::CLAUDE_ADAPTER)
    );
    assert_eq!(process["state"], "running");
    assert_eq!(process["sessions"], serde_json::json!(1));
    assert_eq!(process["session_cap"], serde_json::json!(16));
    assert_eq!(process["in_flight"], serde_json::json!(1));
    assert_eq!(process["in_flight_cap"], serde_json::json!(4));
    assert_eq!(process["breaker_open"], serde_json::json!(false));
    assert_eq!(process["breaker_failures"], serde_json::json!(0));
    assert_eq!(process["provider_version"], "0.0.0-fixture");

    // ... and the breaker still reports once the process it belongs to is gone,
    // which is the ONLY moment its state explains anything.
    assert!(harness.pool.kill_provider(ainb_acp::config::CLAUDE_ADAPTER).await);
    loop {
        let health = client
            .call(
                methods::HANGAR_DAEMON_HEALTH,
                serde_json::json!({ "workspace_id": "default" }),
            )
            .await;
        let row = health["result"]["acp_pool"]["processes"].as_array().and_then(|rows| {
            rows.iter()
                .find(|row| row["provider"] == ainb_acp::config::CLAUDE_ADAPTER)
                .cloned()
        });
        if let Some(row) = row {
            if row["breaker_failures"].as_u64().unwrap_or(0) >= 1 {
                assert_eq!(row["state"], "exited", "{row}");
                assert_eq!(
                    row["breaker_open"],
                    serde_json::json!(false),
                    "one crash is under the threshold of 3: {row}"
                );
                break;
            }
        }
        assert!(
            Instant::now() < deadline,
            "the crash never reached the breaker the pane reports: {health}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // The spans the runbook's log half reads. A span only reaches the buffer on
    // CLOSE, and a span held by a task outlives the call that opened it, so poll
    // to a deadline rather than sampling once: reading the buffer immediately
    // passes on a fast machine and fails on a loaded CI runner for no reason
    // that has anything to do with the surface under test.
    await_span("acp.spawn", "the spawn path is traced").await;
    await_span(
        "0.0.0-fixture",
        "the spawn span records the provider version it observed",
    )
    .await;
    await_span(
        r#"outcome="DELIVERED""#,
        "the turn span records its own outcome (Span::current() in finish_turn recorded nothing)",
    )
    .await;

    harness.finish().await;
}

// ------------------------------------------- Phase 6: retention over the wire

/// `fleet/transcript_prune` is export-THEN-delete, in that order, and it
/// refuses to delete at all without an export unless `no_export` is explicit.
/// This is the one destructive verb on the chat-bus surface and the daemon has
/// no undo, so the refusal is the contract, not friction.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn transcript_prune_exports_before_it_deletes_and_refuses_a_bare_delete() {
    let harness = Harness::start(&[("FAKE_ACP_CHUNKS", "4")], |_| {}).await;
    let mut client = harness.client().await;
    let (session_key, _scope) = harness.create_session(&mut client, None).await;

    let sent = client
        .call(
            methods::FLEET_MESSAGE_SEND,
            serde_json::json!({
                "targets": [session_key],
                "text": "hello",
                "request_id": "req-prune",
            }),
        )
        .await;
    let message_id = sent["result"]["message_id"].as_str().expect("message id").to_string();
    harness.await_delivered(&message_id, &session_key).await;

    let before = client
        .call(
            methods::FLEET_TRANSCRIPT_LIST,
            serde_json::json!({ "session_key": session_key, "limit": 100 }),
        )
        .await;
    let chunks = before["result"]["chunks"].as_array().expect("chunks").clone();
    assert!(
        chunks.len() > 2,
        "a real transcript to prune: {}",
        chunks.len()
    );
    // Keep the tail: prune everything below the LAST chunk's order.
    let watermark = chunks.last().expect("a chunk")["ingest_order"].as_i64().expect("ingest_order");

    // No export path and no explicit no_export: refused, and nothing deleted.
    let refused = client
        .call(
            methods::FLEET_TRANSCRIPT_PRUNE,
            serde_json::json!({ "session_key": session_key, "before_order": watermark }),
        )
        .await;
    assert_eq!(refused["error"]["code"], -32602, "{refused}");
    assert!(
        refused["error"]["message"].as_str().expect("message").contains("no_export"),
        "the refusal names the flag that overrides it: {refused}"
    );
    assert_eq!(
        client
            .call(
                methods::FLEET_TRANSCRIPT_LIST,
                serde_json::json!({ "session_key": session_key, "limit": 100 }),
            )
            .await["result"]["chunks"]
            .as_array()
            .expect("chunks")
            .len(),
        chunks.len(),
        "a refused prune deletes nothing"
    );

    let export = harness.dir.join("transcript.jsonl");
    let pruned = client
        .call(
            methods::FLEET_TRANSCRIPT_PRUNE,
            serde_json::json!({
                "session_key": session_key,
                "before_order": watermark,
                "export_path": export.to_string_lossy(),
            }),
        )
        .await;
    assert!(pruned["error"].is_null(), "{pruned}");
    let exported = pruned["result"]["exported"].as_u64().expect("exported");
    let deleted = pruned["result"]["deleted"].as_u64().expect("deleted");
    assert_eq!(
        exported, deleted,
        "every deleted row is in the export, and no row is exported that is not deleted"
    );
    assert_eq!(deleted as usize, chunks.len() - 1, "the tail row survives");

    // The export is REAL and re-readable, one JSON chunk per line.
    let jsonl = std::fs::read_to_string(&export).expect("the export file exists");
    let lines: Vec<&str> = jsonl.lines().collect();
    assert_eq!(lines.len(), exported as usize);
    let first: serde_json::Value = serde_json::from_str(lines[0]).expect("one chunk per line");
    assert_eq!(first["session_key"], session_key);
    assert_eq!(first["ingest_order"], chunks[0]["ingest_order"]);

    let after = client
        .call(
            methods::FLEET_TRANSCRIPT_LIST,
            serde_json::json!({ "session_key": session_key, "limit": 100 }),
        )
        .await;
    let remaining = after["result"]["chunks"].as_array().expect("chunks");
    assert_eq!(remaining.len(), 1, "only the tail is left: {after}");
    assert_eq!(remaining[0]["ingest_order"], watermark);

    harness.finish().await;
}

/// `no_export` is the deliberate way to say "delete unexported", and it is
/// mutually exclusive with an export path rather than quietly winning.
///
/// It also carries the prune's two BLAST RADIUS claims, which otherwise rest
/// entirely on reading the repo's SQL: a `source <> 'acp'` row under the SAME
/// session key survives (so the projection sources' `projection_revision IS
/// NULL` recovery contract is genuinely untouchable from here), and an `acp`
/// row under a DIFFERENT session key survives. Both sit below the watermark, so
/// only the predicates keep them alive.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn transcript_prune_honours_an_explicit_no_export_and_refuses_both_at_once() {
    use ainb_hangar_store::repo::fleet_provider_event::{
        FleetProviderEventRepo, NewFleetProviderEvent,
    };

    let harness = Harness::start(&[("FAKE_ACP_CHUNKS", "2")], |_| {}).await;
    let mut client = harness.client().await;
    let (session_key, _scope) = harness.create_session(&mut client, None).await;
    let sent = client
        .call(
            methods::FLEET_MESSAGE_SEND,
            serde_json::json!({
                "targets": [session_key],
                "text": "hello",
                "request_id": "req-prune-2",
            }),
        )
        .await;
    let message_id = sent["result"]["message_id"].as_str().expect("message id").to_string();
    harness.await_delivered(&message_id, &session_key).await;

    // Two rows the prune must NOT reach, both below the watermark.
    for (event_id, source, owner) in [
        ("guard-hook", "claude_hook", session_key.as_str()),
        ("guard-acp-elsewhere", "acp", "acp:someone-else"),
    ] {
        FleetProviderEventRepo::append(
            harness.store.pool(),
            &NewFleetProviderEvent {
                event_id: event_id.to_string(),
                provider: "claude".to_string(),
                source: source.to_string(),
                session_key: Some(owner.to_string()),
                provider_session_id: None,
                observed_at: 1,
                received_at: 1,
                event_type: "guard".to_string(),
                raw_payload: format!("{{\"guard\":\"{event_id}\"}}"),
            },
        )
        .await
        .expect("seed a row the prune must not touch");
    }

    // A relative export path is refused by the DAEMON, not just by the CLI:
    // any other socket client would otherwise get its export written relative
    // to the daemon's cwd while the delete proceeded normally.
    let relative = client
        .call(
            methods::FLEET_TRANSCRIPT_PRUNE,
            serde_json::json!({
                "session_key": session_key,
                "before_order": 1_000_000,
                "export_path": "out.jsonl",
            }),
        )
        .await;
    assert_eq!(relative["error"]["code"], -32602, "{relative}");
    assert!(
        relative["error"]["message"].as_str().expect("message").contains("absolute"),
        "the refusal names the rule: {relative}"
    );

    // An export path that already EXISTS is refused, and nothing is deleted:
    // `--export ~/.agents-in-a-box/hangar.db` must not truncate the store and
    // then prune anyway.
    let occupied = harness.dir.join("occupied.jsonl");
    std::fs::write(&occupied, "do not clobber me\n").expect("seed an existing file");
    let clobber = client
        .call(
            methods::FLEET_TRANSCRIPT_PRUNE,
            serde_json::json!({
                "session_key": session_key,
                "before_order": 1_000_000,
                "export_path": occupied.to_string_lossy(),
            }),
        )
        .await;
    assert_eq!(clobber["error"]["code"], -32602, "{clobber}");
    assert_eq!(
        std::fs::read_to_string(&occupied).expect("the file survives"),
        "do not clobber me\n",
        "a refused export never touched the operator's file"
    );
    assert!(
        !client
            .call(
                methods::FLEET_TRANSCRIPT_LIST,
                serde_json::json!({ "session_key": session_key, "limit": 100 }),
            )
            .await["result"]["chunks"]
            .as_array()
            .expect("chunks")
            .is_empty(),
        "and it deleted nothing: {clobber}"
    );

    let both = client
        .call(
            methods::FLEET_TRANSCRIPT_PRUNE,
            serde_json::json!({
                "session_key": session_key,
                "before_order": 1_000_000,
                "export_path": harness.dir.join("x.jsonl").to_string_lossy(),
                "no_export": true,
            }),
        )
        .await;
    assert_eq!(both["error"]["code"], -32602, "{both}");

    let pruned = client
        .call(
            methods::FLEET_TRANSCRIPT_PRUNE,
            serde_json::json!({
                "session_key": session_key,
                "before_order": 1_000_000,
                "no_export": true,
            }),
        )
        .await;
    assert!(pruned["error"].is_null(), "{pruned}");
    assert_eq!(pruned["result"]["exported"], 0);
    assert!(pruned["result"]["deleted"].as_u64().expect("deleted") > 0);
    assert!(
        pruned["result"]["export_path"].is_null(),
        "nothing was written: {pruned}"
    );
    let after = client
        .call(
            methods::FLEET_TRANSCRIPT_LIST,
            serde_json::json!({ "session_key": session_key, "limit": 100 }),
        )
        .await;
    let remaining = after["result"]["chunks"].as_array().expect("chunks");
    assert_eq!(
        remaining.iter().map(|chunk| &chunk["event_id"]).collect::<Vec<_>>(),
        vec!["guard-hook"],
        "every ACP row is gone as asked, and the non-acp row under the SAME \
         session key is all that is left: {after}"
    );

    for event_id in ["guard-hook", "guard-acp-elsewhere"] {
        assert!(
            FleetProviderEventRepo::get(harness.store.pool(), event_id)
                .await
                .expect("query")
                .is_some(),
            "{event_id} is outside the prune's blast radius and must survive"
        );
    }

    harness.finish().await;
}
