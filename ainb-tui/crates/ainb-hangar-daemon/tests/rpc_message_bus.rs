//! Chat-bus RPC over a real Unix socket: `fleet/message_*` and
//! `fleet/transcript_*` on tmux-era sessions (plan Phase 3).
//!
//! Proves the invariants the bus rests on:
//!
//! * I1: `message_send` is idempotent by `request_id`; a replay with a
//!   different fingerprint is `invalid_params`, never a silent second delivery.
//! * I2: a subscriber that lags the wakeup broadcast pages to head from its
//!   own cursor: contiguous, duplicate-free, no resync frame, no exit.
//! * I3: an N-target send writes N delivery rows, each resolving exactly once.
//! * I14: the cursor is commit-ordered, so concurrent sends (and rows whose
//!   ulids were minted OUT of commit order) each reach a subscriber once.
//!
//! Every tmux leg here resolves to a terminal non-DELIVERED state because no
//! pane exists in the test environment; the DELIVERED path against a fake tmux
//! binary lives in `rpc_message_bus_tmux.rs`, which owns its own PATH.

use std::collections::HashSet;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use ainb_hangar_daemon::events::{EventBroker, EventSink};
use ainb_hangar_daemon::rpc::{self, DaemonHealth};
use ainb_hangar_proto::{RpcId, RpcRequest, methods};
use ainb_hangar_store::Store;
use ainb_hangar_store::repo::fleet_message::{FleetMessageRepo, NewFleetMessage};
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tracing::field::{Field, Visit};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::registry::LookupSpan;

// ------------------------------------------------------------- acp pool slot

/// Serialises the process-wide ACP pool slot across this binary's tests, and
/// clears it on the way out.
///
/// `acp_pool::install` publishes a PROCESS-GLOBAL handle: a test that installs
/// one while another is running routes that other test's prompts into a pool
/// whose store tempdir may already be gone. `rpc_acp.rs` holds the same
/// discipline for the same reason.
static POOL_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// The installed pool's lifetime. Uninstalling on DROP rather than at the end
/// of the test body is what makes it panic-safe: a failing assertion unwinds
/// straight past a trailing `uninstall().await` and leaves a stale pool
/// installed for whatever test runs next, which then fails somewhere unrelated.
struct InstalledPool {
    _guard: tokio::sync::MutexGuard<'static, ()>,
}

impl Drop for InstalledPool {
    fn drop(&mut self) {
        // Deliberately not asserted: this may be running under an unwinding
        // panic, and a panic in Drop aborts the whole test binary.
        let _ = ainb_hangar_daemon::acp_pool::try_uninstall();
    }
}

/// Publish a pool over `store` for the rest of this test.
async fn install_acp_pool(store: &Store) -> InstalledPool {
    let guard = POOL_LOCK.lock().await;
    let broker = EventBroker::new();
    ainb_hangar_daemon::acp_pool::install(ainb_hangar_daemon::acp_pool::AcpPool::new(
        store.clone(),
        broker.sink(),
        ainb_hangar_daemon::acp_pool::PoolConfig::default(),
    ))
    .await;
    InstalledPool { _guard: guard }
}

// ---------------------------------------------------------------- tracing tap

/// One captured span or event: its name plus every field set on it.
#[derive(Debug, Clone, Default)]
struct Captured {
    name: String,
    fields: Vec<(String, String)>,
}

impl Captured {
    fn field(&self, key: &str) -> Option<&str> {
        self.fields.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }
}

type Handle = Arc<Mutex<Captured>>;
type Log = Arc<Mutex<Vec<Handle>>>;

struct CollectLayer {
    spans: Log,
    events: Log,
}

struct FieldCollector<'a>(&'a mut Vec<(String, String)>);

impl Visit for FieldCollector<'_> {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.0.push((field.name().to_string(), value.to_string()));
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.0.push((field.name().to_string(), value.to_string()));
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.0.push((field.name().to_string(), value.to_string()));
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.0.push((field.name().to_string(), value.to_string()));
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.0.push((field.name().to_string(), format!("{value:?}")));
    }
}

impl<S> Layer<S> for CollectLayer
where
    S: tracing::Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(
        &self,
        attrs: &tracing::span::Attributes<'_>,
        id: &tracing::span::Id,
        ctx: Context<'_, S>,
    ) {
        let mut fields = Vec::new();
        attrs.record(&mut FieldCollector(&mut fields));
        let handle: Handle = Arc::new(Mutex::new(Captured {
            name: attrs.metadata().name().to_string(),
            fields,
        }));
        self.spans.lock().expect("span log").push(handle.clone());
        if let Some(span) = ctx.span(id) {
            span.extensions_mut().insert(handle);
        }
    }

    fn on_record(
        &self,
        id: &tracing::span::Id,
        values: &tracing::span::Record<'_>,
        ctx: Context<'_, S>,
    ) {
        if let Some(span) = ctx.span(id) {
            if let Some(handle) = span.extensions().get::<Handle>() {
                values.record(&mut FieldCollector(
                    &mut handle.lock().expect("span").fields,
                ));
            }
        }
    }

    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        let mut fields = Vec::new();
        event.record(&mut FieldCollector(&mut fields));
        self.events.lock().expect("event log").push(Arc::new(Mutex::new(Captured {
            name: event.metadata().name().to_string(),
            fields,
        })));
    }
}

/// Install the process-wide capture once; every test reads the same two logs
/// and filters for its own request ids.
fn tracing_tap() -> &'static (Log, Log) {
    static TAP: OnceLock<(Log, Log)> = OnceLock::new();
    TAP.get_or_init(|| {
        let spans: Log = Arc::default();
        let events: Log = Arc::default();
        let subscriber = tracing_subscriber::registry().with(CollectLayer {
            spans: spans.clone(),
            events: events.clone(),
        });
        let _ = tracing::subscriber::set_global_default(subscriber);
        (spans, events)
    })
}

fn snapshot(log: &Log) -> Vec<Captured> {
    log.lock()
        .expect("log")
        .iter()
        .map(|h| h.lock().expect("entry").clone())
        .collect()
}

// ---------------------------------------------------------------- rpc harness

struct Client {
    reader: BufReader<OwnedReadHalf>,
    writer: OwnedWriteHalf,
    next_id: i64,
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
            next_id: 1,
        }
    }

    async fn authed(dir: &std::path::Path, socket: &std::path::Path) -> Self {
        let mut client = Self::connect(socket).await;
        let token_path = ainb_hangar_proto::auth::token_file_in(dir);
        let token = std::fs::read_to_string(token_path).expect("read daemon.token");
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
                .read_frame(Duration::from_secs(10))
                .await
                .unwrap_or_else(|| panic!("no response to {method} within 10s"));
            if frame.get("id").is_some() {
                return frame;
            }
        }
    }

    /// Drain notifications of one method until none arrives within `quiet`.
    async fn drain_notifications(
        &mut self,
        method: &str,
        quiet: Duration,
    ) -> Vec<serde_json::Value> {
        let mut out = Vec::new();
        while let Some(frame) = self.read_frame(quiet).await {
            if frame.get("id").is_none() && frame["method"] == method {
                out.push(frame["params"].clone());
            }
        }
        out
    }
}

async fn start_server(dir: &std::path::Path) -> (std::path::PathBuf, Store, EventSink) {
    tracing_tap();
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
        stats: Arc::new(ainb_hangar_daemon::health_stats::HealthStats::default()),
    };
    tokio::spawn(rpc::serve(listener, store.pool().clone(), health, broker));
    (socket_path, store, sink)
}

/// Seed a tmux-era Fleet session that accepts `send_prompt`. `tmux_target` is
/// NULL, so the verified send fails SAFE and the leg resolves UNKNOWN, which
/// is exactly the terminal outcome the receipt contract promises.
async fn seed_session(store: &Store, session_key: &str) {
    sqlx::query(
        "INSERT INTO fleet_session \
         (session_key, provider, cwd, capabilities, discovered_at, last_observed_at, version) \
         VALUES (?, 'claude', '/work', '{\"send_prompt\":true}', 1, 1, 1)",
    )
    .bind(session_key)
    .execute(store.pool())
    .await
    .expect("seed fleet session");
}

fn message(id: &str) -> NewFleetMessage {
    NewFleetMessage {
        id: id.to_string(),
        request_id: None,
        request_fingerprint: None,
        scope_key: "session:seeded".to_string(),
        origin_message_id: None,
        sender: "operator".to_string(),
        kind: "user".to_string(),
        body: format!("body {id}"),
        created_at: 1,
    }
}

// ---------------------------------------------------------------------- tests

/// I1: the same `request_id` with the same content returns the identical
/// response and submits once; the same id with different content is rejected.
#[tokio::test]
async fn double_send_is_idempotent_and_a_mismatched_replay_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let (socket, store, _sink) = start_server(dir.path()).await;
    seed_session(&store, "claude:one").await;
    let mut client = Client::authed(dir.path(), &socket).await;

    let params = serde_json::json!({
        "targets": ["claude:one"],
        "text": "ship it",
        "request_id": "req-i1",
    });
    let first = client.call(methods::FLEET_MESSAGE_SEND, params.clone()).await;
    assert!(first["error"].is_null(), "first send must ack: {first}");
    let second = client.call(methods::FLEET_MESSAGE_SEND, params).await;
    assert_eq!(
        first["result"], second["result"],
        "a replayed send answers identically"
    );

    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM fleet_message")
        .fetch_one(store.pool())
        .await
        .unwrap();
    assert_eq!(rows, 1, "one message row for one request_id");
    let receipts: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM fleet_action_receipt WHERE action_kind = 'send_prompt'",
    )
    .fetch_one(store.pool())
    .await
    .unwrap();
    assert_eq!(receipts, 1, "exactly one submit was attempted, not two");

    let mismatched = client
        .call(
            methods::FLEET_MESSAGE_SEND,
            serde_json::json!({
                "targets": ["claude:one"],
                "text": "DIFFERENT text",
                "request_id": "req-i1",
            }),
        )
        .await;
    assert_eq!(
        mismatched["error"]["code"], -32602,
        "a reused request_id with different content is invalid_params: {mismatched}"
    );
}

/// Attribution: `sender` is what the recipient's re-prime corpus attributes the
/// message to and what every chat UI renders, so a copilot-authored send must
/// not be indistinguishable from a human one. An absent actor still means the
/// operator, which is what every human surface sends.
#[tokio::test]
async fn a_copilot_send_is_recorded_as_copilot_and_an_absent_actor_as_the_operator() {
    let dir = tempfile::tempdir().unwrap();
    let (socket, store, _sink) = start_server(dir.path()).await;
    seed_session(&store, "claude:one").await;
    let mut client = Client::authed(dir.path(), &socket).await;

    let human = client
        .call(
            methods::FLEET_MESSAGE_SEND,
            serde_json::json!({
                "targets": ["claude:one"],
                "text": "ship it",
                "request_id": "req-human",
            }),
        )
        .await;
    let copilot = client
        .call(
            methods::FLEET_MESSAGE_SEND,
            serde_json::json!({
                "targets": ["claude:one"],
                "text": "status?",
                "request_id": "req-copilot",
                "actor": "copilot",
            }),
        )
        .await;
    assert!(human["error"].is_null(), "{human}");
    assert!(copilot["error"].is_null(), "{copilot}");

    for (response, expected) in [(&human, "operator"), (&copilot, "copilot")] {
        let id = response["result"]["message_id"].as_str().expect("message id");
        let sender: String = sqlx::query_scalar("SELECT sender FROM fleet_message WHERE id = ?")
            .bind(id)
            .fetch_one(store.pool())
            .await
            .unwrap();
        assert_eq!(
            sender, expected,
            "a copilot write must never wear the operator's name: {response}"
        );
    }

    let blank = client
        .call(
            methods::FLEET_MESSAGE_SEND,
            serde_json::json!({
                "targets": ["claude:one"],
                "text": "x",
                "request_id": "req-blank",
                "actor": "   ",
            }),
        )
        .await;
    assert_eq!(
        blank["error"]["code"], -32602,
        "a blank actor would render as nobody: {blank}"
    );
}

/// I3: an N-target send writes one delivery row per recipient, each resolving
/// to exactly one terminal state with an enumerated detail, and the receipts
/// are queryable per (message, recipient).
#[tokio::test]
async fn n_target_send_resolves_one_terminal_delivery_per_recipient() {
    let dir = tempfile::tempdir().unwrap();
    let (socket, store, _sink) = start_server(dir.path()).await;
    seed_session(&store, "claude:a").await;
    seed_session(&store, "claude:b").await;
    let mut client = Client::authed(dir.path(), &socket).await;

    let response = client
        .call(
            methods::FLEET_MESSAGE_SEND,
            serde_json::json!({
                "targets": ["claude:a", "claude:b", "claude:ghost"],
                "text": "status?",
                "request_id": "req-i3",
            }),
        )
        .await;
    assert!(response["error"].is_null(), "send must ack: {response}");
    let message_id = response["result"]["message_id"].as_str().unwrap().to_string();
    let deliveries = response["result"]["deliveries"].as_array().unwrap();
    assert_eq!(deliveries.len(), 3, "one leg per requested recipient");
    assert_eq!(
        deliveries[2]["state"], "REJECTED",
        "an unknown target is REJECTED"
    );

    let rows: Vec<(String, String, Option<String>, Option<i64>)> = sqlx::query_as(
        "SELECT session_key, state, detail, resolved_at FROM fleet_message_delivery \
         WHERE message_id = ? ORDER BY session_key",
    )
    .bind(&message_id)
    .fetch_all(store.pool())
    .await
    .unwrap();
    assert_eq!(rows.len(), 3, "three durable delivery rows");
    for (session_key, state, detail, resolved_at) in &rows {
        assert_ne!(state, "PENDING", "{session_key} left PENDING");
        assert!(
            resolved_at.is_some(),
            "{session_key} has no resolution stamp"
        );
        let detail = detail.as_deref().unwrap_or_default();
        assert!(
            detail.split(';').next().is_some_and(|token| !token.contains(' ')),
            "{session_key} detail must LEAD with an enumerated token: {detail}"
        );
    }
    assert_eq!(
        rows.iter()
            .find(|(key, ..)| key == "claude:ghost")
            .map(|(_, _, d, _)| d.as_deref()),
        Some(Some("target_unknown")),
        "the unknown target's reason is greppable"
    );
    // The taxonomy is only useful if each cause maps to ITS token. A seeded
    // session has a NULL tmux_target, so its leg must be tmux_identity_unknown
    // and never the send_* catch-all.
    for key in ["claude:a", "claude:b"] {
        let detail = rows
            .iter()
            .find(|(session_key, ..)| session_key == key)
            .and_then(|(_, _, detail, _)| detail.as_deref())
            .unwrap_or_default();
        assert_eq!(
            detail.split(';').next(),
            Some("tmux_identity_unknown"),
            "{key} must carry the identity token, not a catch-all: {detail}"
        );
    }

    // The broadcast scope is minted, and the recipients' legs hang off it.
    let scope: String = sqlx::query_scalar("SELECT scope_key FROM fleet_message WHERE id = ?")
        .bind(&message_id)
        .fetch_one(store.pool())
        .await
        .unwrap();
    assert!(
        scope.starts_with("broadcast:"),
        "multi-target mints a broadcast scope"
    );
}

/// The `fleet.message.send` span carries the request-scoped fields an operator
/// needs to answer "why did this message not deliver?", and each leg's outcome
/// is logged inside it.
#[tokio::test]
async fn message_send_span_and_leg_events_are_populated() {
    let dir = tempfile::tempdir().unwrap();
    let (socket, store, _sink) = start_server(dir.path()).await;
    seed_session(&store, "claude:spanned").await;
    let mut client = Client::authed(dir.path(), &socket).await;
    client
        .call(
            methods::FLEET_MESSAGE_SEND,
            serde_json::json!({
                "targets": ["claude:spanned"],
                "text": "observe me",
                "request_id": "req-span",
            }),
        )
        .await;

    let (spans, events) = tracing_tap();
    let span = snapshot(spans)
        .into_iter()
        .find(|span| {
            span.name == "fleet.message.send" && span.field("request_id") == Some("req-span")
        })
        .expect("the send span was recorded");
    assert_eq!(span.field("target_count"), Some("1"));
    assert_eq!(
        span.field("scope_key"),
        Some("session:claude:spanned"),
        "a single-target send answers in the recipient's own scope"
    );
    assert!(span.field("message_id").is_some_and(|id| !id.is_empty()));
    assert_eq!(span.field("replay"), Some("false"));

    let leg = snapshot(events)
        .into_iter()
        .find(|event| event.field("session_key") == Some("claude:spanned"))
        .expect("the per-leg outcome was logged");
    assert!(
        leg.field("state").is_some(),
        "the leg logs its terminal state"
    );
    assert!(
        leg.field("detail").is_some(),
        "the leg logs its enumerated detail"
    );
}

/// I2: a subscriber whose wakeup receiver lags pages to head from its own
/// cursor. No gap, no duplicate, no resync frame, and the forwarder survives.
#[tokio::test]
async fn lagged_subscriber_pages_to_head_without_gaps_or_duplicates() {
    let dir = tempfile::tempdir().unwrap();
    let (socket, store, sink) = start_server(dir.path()).await;
    let mut subscriber = Client::authed(dir.path(), &socket).await;
    let ack = subscriber.call(methods::FLEET_MESSAGE_SUBSCRIBE, serde_json::json!({})).await;
    assert!(
        ack["result"]["head_id"].is_null(),
        "an empty log has no head"
    );

    // Rows first, wakeups after: the forwarder is parked on its receiver.
    for index in 0..60 {
        FleetMessageRepo::insert_message(store.pool(), &message(&format!("row-{index:03}")))
            .await
            .unwrap();
    }
    // Overrun the wakeup buffer while the forwarder is busy draining rows.
    for seq in 0..4_000 {
        sink.emit_message_seq(seq);
    }
    for index in 60..65 {
        FleetMessageRepo::insert_message(store.pool(), &message(&format!("row-{index:03}")))
            .await
            .unwrap();
    }
    sink.emit_message_seq(65);

    let events = subscriber
        .drain_notifications("fleet/message_event", Duration::from_millis(600))
        .await;
    let ids: Vec<String> = events
        .iter()
        .map(|params| params["message"]["id"].as_str().unwrap().to_string())
        .collect();
    let expected: Vec<String> = (0..65).map(|index| format!("row-{index:03}")).collect();
    assert_eq!(ids, expected, "contiguous, in order, and nothing repeated");

    let lagged = snapshot(&tracing_tap().1).into_iter().any(|event| {
        event
            .field("message")
            .is_some_and(|text| text.contains("chat message wakeups lagged"))
    });
    assert!(
        lagged,
        "the test must actually force the lag it claims to cover"
    );
}

/// I14: K concurrent sends against one pool deliver every committed row exactly
/// once to an attached subscriber.
#[tokio::test]
async fn concurrent_sends_reach_a_subscriber_exactly_once() {
    const K: usize = 12;

    let dir = tempfile::tempdir().unwrap();
    let (socket, store, _sink) = start_server(dir.path()).await;
    seed_session(&store, "claude:k").await;
    let mut subscriber = Client::authed(dir.path(), &socket).await;
    subscriber.call(methods::FLEET_MESSAGE_SUBSCRIBE, serde_json::json!({})).await;

    let mut senders = Vec::new();
    for index in 0..K {
        let dir = dir.path().to_path_buf();
        let socket = socket.clone();
        senders.push(tokio::spawn(async move {
            let mut client = Client::authed(&dir, &socket).await;
            client
                .call(
                    methods::FLEET_MESSAGE_SEND,
                    serde_json::json!({
                        "targets": ["claude:k"],
                        "text": format!("concurrent {index}"),
                        "request_id": format!("req-k-{index}"),
                    }),
                )
                .await
        }));
    }
    let mut sent = HashSet::new();
    for sender in senders {
        let response = sender.await.unwrap();
        assert!(
            response["error"].is_null(),
            "concurrent send failed: {response}"
        );
        sent.insert(response["result"]["message_id"].as_str().unwrap().to_string());
    }
    assert_eq!(sent.len(), K, "each request_id committed its own row");

    let events = subscriber
        .drain_notifications("fleet/message_event", Duration::from_millis(600))
        .await;
    let ids: Vec<String> = events
        .iter()
        .map(|params| params["message"]["id"].as_str().unwrap().to_string())
        .collect();
    let unique: HashSet<String> = ids.iter().cloned().collect();
    assert_eq!(ids.len(), unique.len(), "no row was delivered twice");
    assert_eq!(unique, sent, "the delivered set IS the committed set");
}

/// I14 variant: rows whose ulids were minted in DESCENDING order still stream
/// exactly once and in commit order. This is the case an id-as-cursor design
/// loses silently: the forwarder would page past the lower id and never
/// re-read it.
#[tokio::test]
async fn out_of_order_ulid_minting_still_streams_every_row_once() {
    let dir = tempfile::tempdir().unwrap();
    let (socket, store, sink) = start_server(dir.path()).await;
    let mut subscriber = Client::authed(dir.path(), &socket).await;
    subscriber.call(methods::FLEET_MESSAGE_SUBSCRIBE, serde_json::json!({})).await;

    // Commit order 0,1,2,… against ids z,y,x,…: seq rises while the id falls.
    let ids: Vec<String> = (0..8).map(|index| format!("zz-{:03}", 900 - index)).collect();
    for id in &ids {
        let row = FleetMessageRepo::insert_message(store.pool(), &message(id)).await.unwrap();
        sink.emit_message_seq(row.seq);
    }

    let events = subscriber
        .drain_notifications("fleet/message_event", Duration::from_millis(600))
        .await;
    let delivered: Vec<String> = events
        .iter()
        .map(|params| params["message"]["id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        delivered, ids,
        "the cursor follows commit order, never the minted id"
    );
}

/// A subscriber resuming from an explicit `after_id` receives only what came
/// after it.
#[tokio::test]
async fn subscribe_after_id_resumes_from_that_row() {
    let dir = tempfile::tempdir().unwrap();
    let (socket, store, sink) = start_server(dir.path()).await;
    for index in 0..3 {
        FleetMessageRepo::insert_message(store.pool(), &message(&format!("old-{index}")))
            .await
            .unwrap();
    }
    let mut subscriber = Client::authed(dir.path(), &socket).await;
    let ack = subscriber
        .call(
            methods::FLEET_MESSAGE_SUBSCRIBE,
            serde_json::json!({ "after_id": "old-0" }),
        )
        .await;
    assert_eq!(
        ack["result"]["head_id"], "old-2",
        "the ack carries the true head"
    );

    let row = FleetMessageRepo::insert_message(store.pool(), &message("new-1")).await.unwrap();
    sink.emit_message_seq(row.seq);

    let events = subscriber
        .drain_notifications("fleet/message_event", Duration::from_millis(600))
        .await;
    let ids: Vec<&str> =
        events.iter().map(|params| params["message"]["id"].as_str().unwrap()).collect();
    assert_eq!(ids, vec!["old-1", "old-2", "new-1"]);
}

/// An `after_id` that resolves to no row is `invalid_params` on BOTH readers,
/// never silently treated as start-of-log.
#[tokio::test]
async fn unresolvable_after_id_is_invalid_params_on_list_and_subscribe() {
    let dir = tempfile::tempdir().unwrap();
    let (socket, store, _sink) = start_server(dir.path()).await;
    FleetMessageRepo::insert_message(store.pool(), &message("only-row"))
        .await
        .unwrap();
    let mut client = Client::authed(dir.path(), &socket).await;

    let listed = client
        .call(
            methods::FLEET_MESSAGE_LIST,
            serde_json::json!({ "after_id": "not-a-message", "limit": 10 }),
        )
        .await;
    assert_eq!(
        listed["error"]["code"], -32602,
        "list must refuse: {listed}"
    );

    let subscribed = client
        .call(
            methods::FLEET_MESSAGE_SUBSCRIBE,
            serde_json::json!({ "after_id": "not-a-message" }),
        )
        .await;
    assert_eq!(
        subscribed["error"]["code"], -32602,
        "subscribe must refuse: {subscribed}"
    );
}

/// An oversized `limit` is clamped to the named page maximum, not honoured.
/// A supplied scope must name a recipient of the SAME send. Codex review,
/// 2026-08-08: `--target session:B --scope session:A` prompted B while filing
/// the message in A's timeline, so a caller could write into any session's
/// history with input nothing validated.
#[tokio::test]
async fn a_scope_that_does_not_name_a_recipient_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let (socket, store, _sink) = start_server(dir.path()).await;
    seed_session(&store, "claude:a").await;
    seed_session(&store, "claude:b").await;
    let mut client = Client::authed(dir.path(), &socket).await;

    let stolen = client
        .call(
            methods::FLEET_MESSAGE_SEND,
            serde_json::json!({
                "targets": ["claude:b"],
                "scope_key": "session:claude:a",
                "text": "file this in someone else's timeline",
                "request_id": "req-scope-steal",
            }),
        )
        .await;
    assert_eq!(stolen["error"]["code"], -32602, "{stolen}");
    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM fleet_message")
        .fetch_one(store.pool())
        .await
        .unwrap();
    assert_eq!(rows, 0, "a refused send must persist nothing");

    // A session scope cannot carry a fan-out, and an unknown prefix is refused
    // rather than silently minted into the timeline.
    for (scope, targets) in [
        ("session:claude:a", vec!["claude:a", "claude:b"]),
        ("channel:not-in-part-1", vec!["claude:a"]),
    ] {
        let refused = client
            .call(
                methods::FLEET_MESSAGE_SEND,
                serde_json::json!({
                    "targets": targets,
                    "scope_key": scope,
                    "text": "nope",
                    "request_id": format!("req-{scope}"),
                }),
            )
            .await;
        assert_eq!(refused["error"]["code"], -32602, "scope {scope}: {refused}");
    }

    // The honest form still works: the recipient's own scope.
    let ok = client
        .call(
            methods::FLEET_MESSAGE_SEND,
            serde_json::json!({
                "targets": ["claude:b"],
                "scope_key": "session:claude:b",
                "text": "addressed to the session it names",
                "request_id": "req-scope-ok",
            }),
        )
        .await;
    assert!(ok["error"].is_null(), "{ok}");
}

#[tokio::test]
async fn oversized_limit_is_clamped() {
    let dir = tempfile::tempdir().unwrap();
    let (socket, store, _sink) = start_server(dir.path()).await;
    for index in 0..(ainb_hangar_proto::fleet::FLEET_MESSAGE_LIST_MAX + 20) {
        FleetMessageRepo::insert_message(store.pool(), &message(&format!("m-{index:04}")))
            .await
            .unwrap();
    }
    let mut client = Client::authed(dir.path(), &socket).await;
    let response = client
        .call(
            methods::FLEET_MESSAGE_LIST,
            serde_json::json!({ "limit": 100_000 }),
        )
        .await;
    let messages = response["result"]["messages"].as_array().unwrap();
    assert_eq!(
        u32::try_from(messages.len()).unwrap(),
        ainb_hangar_proto::fleet::FLEET_MESSAGE_LIST_MAX,
        "an unbounded page is a self-inflicted memory incident"
    );
    assert_eq!(
        response["result"]["next_after_id"], "m-0099",
        "the page hands back its own tail cursor"
    );
}

/// Scope and thread filters page the same commit-ordered log.
#[tokio::test]
async fn list_filters_by_scope_and_by_thread_origin() {
    let dir = tempfile::tempdir().unwrap();
    let (socket, store, _sink) = start_server(dir.path()).await;
    let mut broadcast = message("bcast");
    broadcast.scope_key = "broadcast:b-1".to_string();
    FleetMessageRepo::insert_message(store.pool(), &broadcast).await.unwrap();
    for (id, scope) in [("reply-a", "session:a"), ("reply-b", "session:b")] {
        let mut reply = message(id);
        reply.scope_key = scope.to_string();
        reply.kind = "agent".to_string();
        reply.origin_message_id = Some("bcast".to_string());
        FleetMessageRepo::insert_message(store.pool(), &reply).await.unwrap();
    }
    let mut client = Client::authed(dir.path(), &socket).await;

    let threaded = client
        .call(
            methods::FLEET_MESSAGE_LIST,
            serde_json::json!({ "origin_id": "bcast", "limit": 10 }),
        )
        .await;
    let ids: Vec<&str> = threaded["result"]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|message| message["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, vec!["reply-a", "reply-b"], "the thread join is exact");

    let scoped = client
        .call(
            methods::FLEET_MESSAGE_LIST,
            serde_json::json!({ "scope_key": "session:a", "limit": 10 }),
        )
        .await;
    let ids: Vec<&str> = scoped["result"]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|message| message["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, vec!["reply-a"]);
}

/// A threaded send round trips: the origin is persisted, comes back on the
/// wire, and the thread read returns exactly that reply.
///
/// This is the per-session thread (part 2 Phase B): the origin and the reply
/// share one `session:<key>` scope, so `message_list {origin_id}` is the thread
/// view and `message_list {scope_key}` is the whole conversation.
#[tokio::test]
async fn a_threaded_send_round_trips_through_the_origin_join() {
    let dir = tempfile::tempdir().unwrap();
    let (socket, store, _sink) = start_server(dir.path()).await;
    seed_session(&store, "claude:one").await;
    let mut client = Client::authed(dir.path(), &socket).await;

    let root = client
        .call(
            methods::FLEET_MESSAGE_SEND,
            serde_json::json!({
                "targets": ["claude:one"],
                "text": "run the tests",
                "request_id": "req-thread-root",
            }),
        )
        .await;
    assert!(root["error"].is_null(), "{root}");
    let origin_id = root["result"]["message_id"].as_str().expect("a message id").to_string();

    let reply = client
        .call(
            methods::FLEET_MESSAGE_SEND,
            serde_json::json!({
                "targets": ["claude:one"],
                "origin_message_id": origin_id,
                "text": "and then deploy",
                "request_id": "req-thread-reply",
            }),
        )
        .await;
    assert!(reply["error"].is_null(), "{reply}");
    let reply_id = reply["result"]["message_id"].as_str().expect("a message id").to_string();

    // The wire carries the linkage back, or no client can render a thread.
    let thread = client
        .call(
            methods::FLEET_MESSAGE_LIST,
            serde_json::json!({ "origin_id": origin_id, "limit": 10 }),
        )
        .await;
    let rows = thread["result"]["messages"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "the thread join is exact: {thread}");
    assert_eq!(rows[0]["id"], reply_id.as_str());
    assert_eq!(
        rows[0]["origin_message_id"],
        origin_id.as_str(),
        "the reply does not carry its origin on the wire: {thread}"
    );

    // And the durable row agrees with the wire.
    let stored = FleetMessageRepo::get_message(store.pool(), &reply_id).await.unwrap().unwrap();
    assert_eq!(
        stored.origin_message_id.as_deref(),
        Some(origin_id.as_str())
    );
    assert_eq!(
        stored.scope_key, "session:claude:one",
        "a single-target send files the row in the recipient's own scope, which \
         is the scope a session thread reads"
    );
}

/// An origin the daemon cannot vouch for is refused, both ways it can be wrong.
///
/// Fails CLOSED like the channel-membership rule: an origin in another scope
/// would thread this row into a conversation nobody addressed, and an origin
/// naming nothing at all builds a thread no read can ever return. Neither may
/// persist a message.
#[tokio::test]
async fn an_origin_that_is_unknown_or_in_another_scope_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let (socket, store, _sink) = start_server(dir.path()).await;
    seed_session(&store, "claude:a").await;
    seed_session(&store, "claude:b").await;
    let mut client = Client::authed(dir.path(), &socket).await;

    // A real message, in A's scope.
    let root = client
        .call(
            methods::FLEET_MESSAGE_SEND,
            serde_json::json!({
                "targets": ["claude:a"],
                "text": "a's conversation",
                "request_id": "req-origin-root",
            }),
        )
        .await;
    let origin_id = root["result"]["message_id"].as_str().unwrap().to_string();
    let before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM fleet_message")
        .fetch_one(store.pool())
        .await
        .unwrap();

    // Same id, but this send is addressed to B, so it resolves to B's scope.
    let cross_scope = client
        .call(
            methods::FLEET_MESSAGE_SEND,
            serde_json::json!({
                "targets": ["claude:b"],
                "origin_message_id": origin_id,
                "text": "reply to a thread in someone else's conversation",
                "request_id": "req-origin-cross",
            }),
        )
        .await;
    assert_eq!(
        cross_scope["error"]["code"], -32602,
        "an origin in another scope must be refused: {cross_scope}"
    );

    for (request_id, origin) in [
        ("req-origin-missing", "01J0NOSUCHMESSAGE"),
        ("req-origin-blank", "   "),
    ] {
        let refused = client
            .call(
                methods::FLEET_MESSAGE_SEND,
                serde_json::json!({
                    "targets": ["claude:a"],
                    "origin_message_id": origin,
                    "text": "answering nothing",
                    "request_id": request_id,
                }),
            )
            .await;
        assert_eq!(
            refused["error"]["code"], -32602,
            "origin {origin:?} must be refused: {refused}"
        );
    }

    let after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM fleet_message")
        .fetch_one(store.pool())
        .await
        .unwrap();
    assert_eq!(
        after, before,
        "a refused threaded send must persist nothing"
    );
}

/// The thread read is ordered by the COMMIT-ordered `seq`, never by the ULID.
///
/// Ids are identities and are minted by whoever writes the row; part 1 shipped
/// an ordering bug of exactly this shape. The replies here are inserted with
/// ids that sort BACKWARDS against their commit order, so a reader that sorts
/// or pages by id returns them reversed (or skips one after a cursor).
#[tokio::test]
async fn a_thread_is_ordered_by_commit_seq_not_by_ulid() {
    let dir = tempfile::tempdir().unwrap();
    let (socket, store, _sink) = start_server(dir.path()).await;
    let mut root = message("origin-row");
    root.scope_key = "session:s-1".to_string();
    FleetMessageRepo::insert_message(store.pool(), &root).await.unwrap();
    // Committed first..last, named last..first.
    for id in ["zzz-first", "mmm-second", "aaa-third"] {
        let mut reply = message(id);
        reply.scope_key = "session:s-1".to_string();
        reply.kind = "agent".to_string();
        reply.origin_message_id = Some("origin-row".to_string());
        FleetMessageRepo::insert_message(store.pool(), &reply).await.unwrap();
    }
    let mut client = Client::authed(dir.path(), &socket).await;

    let page = client
        .call(
            methods::FLEET_MESSAGE_LIST,
            serde_json::json!({ "origin_id": "origin-row", "limit": 10 }),
        )
        .await;
    let ids: Vec<&str> = page["result"]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        vec!["zzz-first", "mmm-second", "aaa-third"],
        "the thread is in commit order, not id order: {page}"
    );

    // And the cursor is the same thing: paging after the first reply returns
    // the two committed AFTER it, which id order would get wrong in both
    // directions.
    let next = client
        .call(
            methods::FLEET_MESSAGE_LIST,
            serde_json::json!({ "origin_id": "origin-row", "after_id": "zzz-first", "limit": 10 }),
        )
        .await;
    let ids: Vec<&str> = next["result"]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, vec!["mmm-second", "aaa-third"], "{next}");
}

/// A session whose `capabilities` JSON withholds `send_prompt` is refused by
/// `action_capability` before any transport runs, and the leg must say so with
/// ITS token. This is the second half of the taxonomy contract: the tokens are
/// derived from receipt prose, so each cause needs a test that would go red if
/// that prose were reworded.
#[tokio::test]
async fn a_capability_gated_target_resolves_with_the_capability_token() {
    let dir = tempfile::tempdir().unwrap();
    let (socket, store, _sink) = start_server(dir.path()).await;
    sqlx::query(
        "INSERT INTO fleet_session \
         (session_key, provider, cwd, capabilities, discovered_at, last_observed_at, version) \
         VALUES ('claude:muted', 'claude', '/work', '{\"send_prompt\":false}', 1, 1, 1)",
    )
    .execute(store.pool())
    .await
    .expect("seed a session that refuses prompts");
    let mut client = Client::authed(dir.path(), &socket).await;

    let response = client
        .call(
            methods::FLEET_MESSAGE_SEND,
            serde_json::json!({
                "targets": ["claude:muted"],
                "text": "are you there?",
                "request_id": "req-capability",
            }),
        )
        .await;
    assert!(response["error"].is_null(), "send must ack: {response}");
    let detail: Option<String> = sqlx::query_scalar(
        "SELECT detail FROM fleet_message_delivery WHERE session_key = 'claude:muted'",
    )
    .fetch_one(store.pool())
    .await
    .unwrap();
    assert_eq!(
        detail.as_deref().and_then(|detail| detail.split(';').next()),
        Some("capability_unavailable"),
        "a gated action must be countable as such: {detail:?}"
    );
}

/// A connection that negotiates sees the three chat-bus capabilities, so a
/// client can discover the surface it is allowed to call.
#[tokio::test]
async fn negotiate_advertises_the_chat_bus_capabilities() {
    let dir = tempfile::tempdir().unwrap();
    let (socket, _store, _sink) = start_server(dir.path()).await;
    let mut client = Client::authed(dir.path(), &socket).await;
    let response = client
        .call(
            methods::FLEET_NEGOTIATE,
            serde_json::json!({
                "client_name": "test",
                "client_version": "0",
                "read_versions": { "min": 1, "max": 2 },
                "write_versions": { "min": 1, "max": 2 },
            }),
        )
        .await;
    let ids: Vec<&str> = response["result"]["capability_ids"]
        .as_array()
        .unwrap()
        .iter()
        .map(|id| id.as_str().unwrap())
        .collect();
    for expected in [
        "fleet.message.send",
        "fleet.message.read",
        "fleet.transcript.read",
        // Phase 5 landed `fleet/acp_session_create`'s dispatch arm, so its
        // capability is advertised in the SAME change. Before that arm existed
        // this assertion ran the other way round, which is the contract: a
        // capability is advertised exactly when its methods answer.
        "fleet.acp.spawn",
    ] {
        assert!(
            ids.contains(&expected),
            "{expected} must be advertised: {ids:?}"
        );
    }
}

/// `fleet/acp_session_create` refuses a provider the adapter registry does not
/// know, BEFORE any row is written.
///
/// The schema only length-checks `provider` (so the next adapter needs no
/// migration), which makes this handler the one place an unknown token can be
/// refused at all.
#[tokio::test]
async fn acp_session_create_refuses_an_unknown_provider() {
    let dir = tempfile::tempdir().unwrap();
    let (socket, store, _sink) = start_server(dir.path()).await;
    let mut client = Client::authed(dir.path(), &socket).await;

    let response = client
        .call(
            methods::FLEET_ACP_SESSION_CREATE,
            serde_json::json!({ "provider": "gemini-acp", "cwd": "/work" }),
        )
        .await;
    assert_eq!(
        response["error"]["code"], -32602,
        "an unknown adapter is invalid_params: {response}"
    );
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM fleet_acp_session")
        .fetch_one(store.pool())
        .await
        .unwrap();
    assert_eq!(rows, 0, "a refused create writes nothing");
}

/// Create writes BOTH rows under one key, with exactly the wired capability
/// set, and is idempotent per live scope.
#[tokio::test]
async fn acp_session_create_writes_the_row_pair_and_is_idempotent_per_scope() {
    let dir = tempfile::tempdir().unwrap();
    let (socket, store, _sink) = start_server(dir.path()).await;
    let mut client = Client::authed(dir.path(), &socket).await;

    let first = client
        .call(
            methods::FLEET_ACP_SESSION_CREATE,
            serde_json::json!({
                "provider": "claude-agent-acp",
                "cwd": "/work",
                "scope_key": "session:shared",
            }),
        )
        .await;
    assert!(first["error"].is_null(), "{first}");
    let session_key = first["result"]["session_key"].as_str().expect("key").to_string();
    assert_eq!(first["result"]["scope_key"], "session:shared");

    // BOTH rows, one key: the fleet's session identity plus the ACP adjunct.
    let (provider, capabilities): (String, String) =
        sqlx::query_as("SELECT provider, capabilities FROM fleet_session WHERE session_key = ?")
            .bind(&session_key)
            .fetch_one(store.pool())
            .await
            .expect("fleet_session row");
    assert_eq!(
        provider, "acp",
        "the WIRE token, mapped to FleetProvider::Acp"
    );
    let capabilities: ainb_hangar_proto::fleet::FleetCapabilities =
        serde_json::from_str(&capabilities).expect("capability json");
    for (name, enabled) in [
        ("send_prompt", capabilities.send_prompt),
        ("approvals", capabilities.approvals),
        ("structured_answer", capabilities.structured_answer),
        ("interrupt", capabilities.interrupt),
        ("stop", capabilities.stop),
        ("kill", capabilities.kill),
    ] {
        assert!(enabled, "{name} is one of the actions Phase 5 wires");
    }
    for (name, enabled) in [
        ("tmux_attach", capabilities.tmux_attach),
        ("tmux_text", capabilities.tmux_text),
        ("verified_picker", capabilities.verified_picker),
        ("restart", capabilities.restart),
        ("archive", capabilities.archive),
    ] {
        assert!(
            !enabled,
            "{name} must stay off: an ACP session has no pane and no such path"
        );
    }
    let (acp_provider, acp_state): (String, String) =
        sqlx::query_as("SELECT provider, state FROM fleet_acp_session WHERE session_key = ?")
            .bind(&session_key)
            .fetch_one(store.pool())
            .await
            .expect("fleet_acp_session row");
    assert_eq!(acp_provider, "claude-agent-acp", "the CONCRETE adapter");
    assert_eq!(acp_state, "IDLE", "no process is spawned at create");

    // Idempotent per live scope, backed by the partial unique index. The SAME
    // session means the same adapter AND the same root: see the two refusals
    // below.
    let second = client
        .call(
            methods::FLEET_ACP_SESSION_CREATE,
            serde_json::json!({
                "provider": "claude-agent-acp",
                "cwd": "/work",
                "scope_key": "session:shared",
            }),
        )
        .await;
    assert_eq!(
        second["result"]["session_key"].as_str(),
        Some(session_key.as_str()),
        "a second create for a live scope returns the existing session: {second}"
    );
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM fleet_acp_session")
        .fetch_one(store.pool())
        .await
        .unwrap();
    assert_eq!(rows, 1, "and writes no second row");

    // A replay that names a DIFFERENT adapter, or a different working
    // directory, is not the same session. Handing back the live key would run
    // every later prompt against an agent (or a repository) the caller never
    // asked for, and nothing downstream could tell.
    for (field, params) in [
        (
            "provider",
            serde_json::json!({
                "provider": "codex-acp",
                "cwd": "/work",
                "scope_key": "session:shared",
            }),
        ),
        (
            "cwd",
            serde_json::json!({
                "provider": "claude-agent-acp",
                "cwd": "/elsewhere",
                "scope_key": "session:shared",
            }),
        ),
    ] {
        let refused = client.call(methods::FLEET_ACP_SESSION_CREATE, params).await;
        assert_eq!(
            refused["error"]["code"], -32602,
            "a {field} mismatch on a live scope is invalid_params: {refused}"
        );
        let message = refused["error"]["message"].as_str().unwrap_or_default();
        assert!(
            message.contains(field),
            "the refusal must name the mismatched field: {message}"
        );
    }
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM fleet_acp_session")
        .fetch_one(store.pool())
        .await
        .unwrap();
    assert_eq!(rows, 1, "and neither refusal wrote a row");
}

/// An operator `fleet/action SendPrompt` the pool REFUSES must not leak a
/// PENDING delivery.
///
/// The operator path writes the message row and its leg BEFORE handing the
/// prompt to the pool, exactly as the chat path does. The chat path resolves a
/// refusal; this one used to return a Rejected receipt and leave the leg
/// PENDING until some future boot scan, so the two entry points disagreed about
/// the same rejection.
///
/// DISCLOSURE: the refusal is injected by seeding the `fleet_session` row
/// WITHOUT its `fleet_acp_session` adjunct, which is what the pool reports as
/// `session_gone`; no adapter is involved.
#[tokio::test]
async fn a_refused_operator_prompt_resolves_its_delivery_leg() {
    let dir = tempfile::tempdir().unwrap();
    let (socket, store, _sink) = start_server(dir.path()).await;
    // The pool must exist for the ACP action arm to run at all; it never spawns
    // anything here because the refusal happens at the session-row read.
    let _pool = install_acp_pool(&store).await;

    sqlx::query(
        "INSERT INTO fleet_session \
         (session_key, provider, cwd, capabilities, discovered_at, last_observed_at, version) \
         VALUES ('acp:orphan', 'acp', '/work', '{\"send_prompt\":true}', 1, 1, 1)",
    )
    .execute(store.pool())
    .await
    .expect("seed an ACP fleet session with no adjunct row");

    let mut client = Client::authed(dir.path(), &socket).await;
    let response = client
        .call(
            methods::FLEET_ACTION,
            serde_json::json!({
                "session_key": "acp:orphan",
                "expected_version": 1,
                "request_id": "req-orphan-prompt",
                "action": { "action": "send_prompt", "text": "are you there?" },
            }),
        )
        .await;
    assert!(
        response["error"].is_null(),
        "the action must ack: {response}"
    );
    assert_eq!(
        response["result"]["receipt"]["status"], "REJECTED",
        "a pool refusal is a Rejected receipt: {response}"
    );

    let leg: (String, Option<String>) = sqlx::query_as(
        "SELECT state, detail FROM fleet_message_delivery WHERE session_key = 'acp:orphan'",
    )
    .fetch_one(store.pool())
    .await
    .expect("the operator prompt wrote a leg");
    assert_eq!(
        leg.0, "REJECTED",
        "the leg must be terminal, not left PENDING for a future boot scan"
    );
    assert_eq!(
        leg.1.as_deref().and_then(|detail| detail.split(';').next()),
        Some("session_gone"),
        "carrying the enumerated reason: {leg:?}"
    );
}

/// The transcript readers answer on an empty transcript (rows arrive with the
/// ACP leg in a later phase), and the forwarder filters foreign sessions
/// BEFORE it queries: another session's wakeup delivers nothing here.
#[tokio::test]
async fn transcript_readers_answer_empty_and_the_forwarder_filters_its_session() {
    let dir = tempfile::tempdir().unwrap();
    let (socket, store, sink) = start_server(dir.path()).await;
    let mut client = Client::authed(dir.path(), &socket).await;

    let listed = client
        .call(
            methods::FLEET_TRANSCRIPT_LIST,
            serde_json::json!({ "session_key": "acp:mine", "limit": 10 }),
        )
        .await;
    assert!(listed["result"]["chunks"].as_array().unwrap().is_empty());
    assert!(listed["result"]["next_after_order"].is_null());

    let ack = client
        .call(
            methods::FLEET_TRANSCRIPT_SUBSCRIBE,
            serde_json::json!({ "session_key": "acp:mine" }),
        )
        .await;
    assert!(
        ack["result"]["head_order"].is_null(),
        "empty transcript, no head"
    );

    // A row for ANOTHER session plus its wakeup must not reach this subscriber.
    seed_transcript_row(&store, "acp:other", "evt-other").await;
    sink.emit_transcript_order("acp:other", 1);
    assert!(
        client
            .drain_notifications("fleet/transcript_event", Duration::from_millis(300))
            .await
            .is_empty(),
        "a foreign session's chunk costs a wakeup and nothing more"
    );

    // This session's row does arrive.
    seed_transcript_row(&store, "acp:mine", "evt-mine").await;
    sink.emit_transcript_order("acp:mine", 2);
    let chunks = client
        .drain_notifications("fleet/transcript_event", Duration::from_millis(600))
        .await;
    assert_eq!(chunks.len(), 1, "exactly this session's chunk");
    assert_eq!(chunks[0]["chunk"]["event_id"], "evt-mine");
    assert_eq!(chunks[0]["chunk"]["session_key"], "acp:mine");
}

/// An unbounded `targets` list is one request that writes an unbounded leg set
/// and holds the daemon across a verified transport submit per recipient.
#[tokio::test]
async fn a_send_past_the_target_ceiling_is_invalid_params() {
    use ainb_hangar_proto::fleet::FLEET_MESSAGE_TARGETS_MAX;

    let dir = tempfile::tempdir().unwrap();
    let (socket, store, _sink) = start_server(dir.path()).await;
    seed_session(&store, "claude:one").await;
    let mut client = Client::authed(dir.path(), &socket).await;

    let targets: Vec<String> =
        (0..=FLEET_MESSAGE_TARGETS_MAX).map(|index| format!("claude:{index}")).collect();
    let response = client
        .call(
            methods::FLEET_MESSAGE_SEND,
            serde_json::json!({ "targets": targets, "text": "hi", "request_id": "req-fanout" }),
        )
        .await;

    assert_eq!(response["error"]["code"], -32602);
    assert!(
        response["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("at most")),
        "the refusal names the ceiling: {response}"
    );
    let persisted: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM fleet_message")
        .fetch_one(store.pool())
        .await
        .unwrap();
    assert_eq!(persisted, 0, "a refused send writes nothing");
}

/// The body is persisted verbatim and re-submitted once per recipient, so an
/// unbounded one is an unbounded write multiplied by the target count.
#[tokio::test]
async fn a_send_past_the_body_ceiling_is_invalid_params() {
    use ainb_hangar_proto::fleet::FLEET_MESSAGE_BODY_MAX;

    let dir = tempfile::tempdir().unwrap();
    let (socket, store, _sink) = start_server(dir.path()).await;
    seed_session(&store, "claude:one").await;
    let mut client = Client::authed(dir.path(), &socket).await;

    let response = client
        .call(
            methods::FLEET_MESSAGE_SEND,
            serde_json::json!({
                "targets": ["claude:one"],
                "text": "x".repeat(FLEET_MESSAGE_BODY_MAX + 1),
                "request_id": "req-fat",
            }),
        )
        .await;

    assert_eq!(response["error"]["code"], -32602);
    assert!(
        response["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("at most")),
        "the refusal names the ceiling: {response}"
    );

    // The byte BELOW the ceiling still goes through, so the bound is a ceiling
    // and not an off-by-one that quietly moved the usable limit.
    let accepted = client
        .call(
            methods::FLEET_MESSAGE_SEND,
            serde_json::json!({
                "targets": ["claude:one"],
                "text": "x".repeat(FLEET_MESSAGE_BODY_MAX),
                "request_id": "req-exact",
            }),
        )
        .await;
    assert!(
        accepted["result"]["message_id"].is_string(),
        "exactly at the ceiling must be accepted: {accepted}"
    );
}

/// A thread and a scope are different cuts of the same log and a reply lives in
/// its RECIPIENT's scope, so the intersection is almost always empty. Answering
/// the origin-only question would be answering a question nobody asked.
#[tokio::test]
async fn list_refuses_a_thread_and_a_scope_filter_together() {
    let dir = tempfile::tempdir().unwrap();
    let (socket, _store, _sink) = start_server(dir.path()).await;
    let mut client = Client::authed(dir.path(), &socket).await;

    let response = client
        .call(
            methods::FLEET_MESSAGE_LIST,
            serde_json::json!({ "origin_id": "bcast", "scope_key": "session:a", "limit": 10 }),
        )
        .await;

    assert_eq!(response["error"]["code"], -32602);
    let message = response["error"]["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("mutually exclusive") && message.contains("recipients' scopes"),
        "the refusal names the conflict and why: {response}"
    );
}

/// A target that EXISTS but whose session has exited gets the plan's
/// `target_not_running` token, not a transport symptom that reads like a bug in
/// the bus.
#[tokio::test]
async fn an_exited_target_resolves_with_the_not_running_token() {
    let dir = tempfile::tempdir().unwrap();
    let (socket, store, _sink) = start_server(dir.path()).await;
    sqlx::query(
        "INSERT INTO fleet_session \
         (session_key, provider, cwd, capabilities, lifecycle_state, discovered_at, \
          last_observed_at, version) \
         VALUES ('claude:gone', 'claude', '/work', '{\"send_prompt\":true}', 'EXITED', 1, 1, 1)",
    )
    .execute(store.pool())
    .await
    .expect("seed an exited session");
    let mut client = Client::authed(dir.path(), &socket).await;

    let response = client
        .call(
            methods::FLEET_MESSAGE_SEND,
            serde_json::json!({
                "targets": ["claude:gone"],
                "text": "anyone home",
                "request_id": "req-gone",
            }),
        )
        .await;

    assert_eq!(response["result"]["deliveries"][0]["state"], "REJECTED");
    let detail: String = sqlx::query_scalar(
        "SELECT detail FROM fleet_message_delivery WHERE session_key = 'claude:gone'",
    )
    .fetch_one(store.pool())
    .await
    .expect("read the durable leg");
    assert_eq!(
        detail, "target_not_running",
        "the durable receipt carries the enumerated reason, not free text"
    );
}

async fn seed_transcript_row(store: &Store, session_key: &str, event_id: &str) {
    use ainb_hangar_store::repo::fleet_provider_event::{
        FleetProviderEventRepo, NewFleetProviderEvent,
    };

    FleetProviderEventRepo::append(
        store.pool(),
        &NewFleetProviderEvent {
            event_id: event_id.to_string(),
            provider: "claude-agent-acp".to_string(),
            source: "acp".to_string(),
            session_key: Some(session_key.to_string()),
            provider_session_id: None,
            observed_at: 10,
            received_at: 11,
            event_type: "acp.message".to_string(),
            raw_payload: serde_json::json!({ "text": "hi" }).to_string(),
        },
    )
    .await
    .expect("seed transcript row");
}
