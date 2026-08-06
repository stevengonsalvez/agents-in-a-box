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
    ] {
        assert!(
            ids.contains(&expected),
            "{expected} must be advertised: {ids:?}"
        );
    }
    assert!(
        !ids.contains(&"fleet.acp.spawn"),
        "a capability whose methods answer -32601 must stay unadvertised"
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
