//! Integration: the daemon pushes `hangar/event` notifications to subscribed
//! connections (e38.2 — the producer half of the dual-channel design).
//!
//! The plugin's `StreamClient` has decoded [`ainb_hangar_proto::events`] frames
//! since P3; this proves the daemon actually *emits* them. Over a real
//! `UnixStream`: an authenticated connection subscribes a workspace, a second
//! connection drives a task transition through the daemon's dispatcher, and the
//! subscribed connection receives the matching `hangar/event` notification
//! (a JSON-RPC frame with **no** `id`). Workspace isolation is asserted
//! negatively: a connection subscribed to a *different* workspace never sees
//! the frame.

use std::time::{Duration, Instant};

use ainb_hangar_daemon::rpc::{self, DaemonHealth};
use ainb_hangar_daemon::seed::{self, WS_ID};
use ainb_hangar_proto::events::EVENT_METHOD;
use ainb_hangar_proto::{RpcId, RpcRequest, methods};
use ainb_hangar_store::Store;
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};

/// A second seeded workspace (foreign tenant) for the isolation negative.
const WS_B_ID: &str = "01HANGARFIXTUREWSB00000000";
/// The foreign workspace's slug.
const WS_B_SLUG: &str = "other";

/// One test client connection: a persistent buffered reader half + writer half.
///
/// The reader half MUST live for the whole connection: wrapping a fresh
/// `BufReader` per call would buffer-and-drop pushed notification frames that
/// arrive between request/response pairs.
struct Client {
    reader: BufReader<OwnedReadHalf>,
    writer: OwnedWriteHalf,
}

impl Client {
    /// Poll-connect until the accept loop is up, then split the stream.
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

    /// Send one framed request.
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

    /// Read one Content-Length frame body as raw JSON (response OR
    /// notification), with `timeout`. `None` on timeout.
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

    /// Send `method` and read frames until its *response* (a frame with an
    /// `id`) arrives, returning it. Pushed notifications interleaved before the
    /// response are discarded (callers asserting on events use
    /// [`Self::next_event`]).
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

    /// Read frames until a `hangar/event` notification arrives (skipping
    /// responses), or `None` when `timeout` elapses first.
    async fn next_event(&mut self, timeout: Duration) -> Option<serde_json::Value> {
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.checked_duration_since(Instant::now())?;
            let frame = self.read_frame(remaining).await?;
            if frame.get("id").is_none() && frame["method"] == EVENT_METHOD {
                return Some(frame["params"].clone());
            }
        }
    }

    /// Authenticate exactly as a real client does: read the plaintext the
    /// daemon minted at boot and present it on the first `auth/hello` frame.
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

    /// Subscribe a workspace and assert the ack.
    async fn subscribe(&mut self, workspace_id: &str) {
        let resp = self
            .call(
                methods::WORKSPACE_SUBSCRIBE,
                serde_json::json!({ "workspace_id": workspace_id }),
            )
            .await;
        assert!(resp["error"].is_null(), "subscribe must ack: {resp}");
    }
}

/// Bind + serve the real listener over a seeded store (mirrors `boot()`):
/// token ensured before the bind, then `rpc::serve` on a background task.
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

/// Seed a second (foreign) workspace so isolation has a real tenant to subscribe.
async fn seed_foreign_workspace(store: &Store) {
    sqlx::query("INSERT INTO workspace (id, slug, name, created_at) VALUES (?, ?, ?, ?)")
        .bind(WS_B_ID)
        .bind(WS_B_SLUG)
        .bind("Other")
        .bind(1_700_000_000_000_i64)
        .execute(store.pool())
        .await
        .unwrap();
}

/// A subscribed, authenticated connection receives a `hangar/event` push when a
/// task transition is driven through the daemon — the positive half of e38.2.
/// The driving connection itself (authenticated but NOT subscribed) receives
/// no frame: subscription, not authentication alone, opts a connection in.
#[tokio::test]
async fn subscribed_connection_receives_task_finished_event() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, _store) = start_server(dir.path()).await;

    // Connection A: authenticate + subscribe the seeded workspace (by slug —
    // the daemon resolves it to the real row id, like the plugin does).
    let mut sub = Client::connect(&socket_path).await;
    sub.auth_from_file(dir.path()).await;
    sub.subscribe(seed::WS_SLUG).await;

    // Connection B: authenticated driver, no subscription.
    let mut driver = Client::connect(&socket_path).await;
    driver.auth_from_file(dir.path()).await;
    let resp = driver
        .call(
            methods::HANGAR_TASK_TRANSITION,
            serde_json::json!({
                "workspace_id": WS_ID,
                "task_id": "task-1",
                "to_status": "done",
            }),
        )
        .await;
    assert!(resp["error"].is_null(), "transition must ack: {resp}");

    // The subscribed connection receives the typed event frame.
    let event = sub
        .next_event(Duration::from_secs(5))
        .await
        .expect("subscribed connection must receive a hangar/event frame");
    assert_eq!(event["event"], "task_finished", "wrong event: {event}");
    assert_eq!(event["task_id"], "task-1", "wrong task: {event}");
    assert_eq!(event["result"], "success", "wrong result: {event}");

    // The driver (authenticated, unsubscribed) gets nothing pushed.
    assert!(
        driver.next_event(Duration::from_millis(300)).await.is_none(),
        "an unsubscribed connection must never receive event frames"
    );
}

/// Workspace isolation: an event in workspace A reaches A's subscriber and
/// never the connection subscribed to workspace B (the negative half).
#[tokio::test]
async fn event_never_crosses_workspace_boundary() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, store) = start_server(dir.path()).await;
    seed_foreign_workspace(&store).await;

    // Subscriber on the seeded workspace (positive control)...
    let mut sub_a = Client::connect(&socket_path).await;
    sub_a.auth_from_file(dir.path()).await;
    sub_a.subscribe(seed::WS_SLUG).await;

    // ...and a subscriber on the foreign workspace.
    let mut sub_b = Client::connect(&socket_path).await;
    sub_b.auth_from_file(dir.path()).await;
    sub_b.subscribe(WS_B_SLUG).await;

    // Drive a terminal transition in workspace A.
    let mut driver = Client::connect(&socket_path).await;
    driver.auth_from_file(dir.path()).await;
    let resp = driver
        .call(
            methods::HANGAR_TASK_TRANSITION,
            serde_json::json!({
                "workspace_id": WS_ID,
                "task_id": "task-1",
                "to_status": "failed",
            }),
        )
        .await;
    assert!(resp["error"].is_null(), "transition must ack: {resp}");

    // Positive marker first: A's subscriber sees the event (so the negative
    // below proves isolation, not a broken pipeline).
    let event = sub_a
        .next_event(Duration::from_secs(5))
        .await
        .expect("workspace A's subscriber must receive the event");
    assert_eq!(event["event"], "task_finished");
    assert_eq!(event["result"], "failure");

    // Negative marker: B's subscriber must see nothing.
    assert!(
        sub_b.next_event(Duration::from_millis(300)).await.is_none(),
        "an event for workspace A must never reach a workspace-B subscription"
    );
}

/// A no-op transition (foreign task id — moves no row) emits no event: the
/// push channel reports real state changes only.
#[tokio::test]
async fn noop_transition_emits_no_event() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, _store) = start_server(dir.path()).await;

    let mut sub = Client::connect(&socket_path).await;
    sub.auth_from_file(dir.path()).await;
    sub.subscribe(seed::WS_SLUG).await;

    let mut driver = Client::connect(&socket_path).await;
    driver.auth_from_file(dir.path()).await;
    let resp = driver
        .call(
            methods::HANGAR_TASK_TRANSITION,
            serde_json::json!({
                "workspace_id": WS_ID,
                "task_id": "no-such-task",
                "to_status": "done",
            }),
        )
        .await;
    assert!(resp["error"].is_null(), "a foreign task id is a no-op ack");

    assert!(
        sub.next_event(Duration::from_millis(300)).await.is_none(),
        "a transition that moved no row must not emit an event"
    );
}
