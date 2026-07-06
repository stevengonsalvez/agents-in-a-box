//! Integration: the durable event outbox + subscribe replay (T1 — the
//! "snapshot then deltas / resume FROM seq>N" contract).
//!
//! Where `rpc_event_push` proves the daemon emits LIVE events to already-attached
//! subscribers, this proves the DURABLE half over a real `UnixStream`: events are
//! persisted to `event_log` with a monotonic `seq`, `workspace/subscribe` acks
//! with the workspace's real head cursor (not the old empty `{}`), and a client
//! that carries a `since_seq` receives exactly the events logged after that
//! cursor — the late-join / daemon-restart resume path. Workspace isolation is
//! asserted negatively: a resume on workspace B never replays workspace A's log.

use std::time::{Duration, Instant};

use ainb_hangar_daemon::events::EventBroker;
use ainb_hangar_daemon::rpc::{self, DaemonHealth};
use ainb_hangar_daemon::seed::{self, WS_ID};
use ainb_hangar_proto::events::EVENT_METHOD;
use ainb_hangar_proto::{RpcId, RpcRequest, methods};
use ainb_hangar_store::Store;
use ainb_hangar_store::repo::event_log::EventOutboxRepo;
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};

/// A second seeded workspace (foreign tenant) for the isolation negative.
const WS_B_ID: &str = "01HANGARFIXTUREWSB00000000";
/// The foreign workspace's slug.
const WS_B_SLUG: &str = "other";

/// One test client connection: a persistent buffered reader half + writer half.
///
/// The reader half MUST live for the whole connection so pushed notification
/// frames arriving between request/response pairs are not buffer-dropped.
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

    /// Send `method` and read frames until its *response* (a frame with an `id`)
    /// arrives, returning it. Interleaved notifications are discarded.
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
    /// responses), or `None` when `timeout` elapses first. Returns its `params`.
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

    /// Subscribe (optionally resuming from `since_seq`) and return the head
    /// cursor the ack's real snapshot carries.
    async fn subscribe_since(&mut self, workspace_id: &str, since_seq: Option<i64>) -> i64 {
        let mut params = serde_json::json!({ "workspace_id": workspace_id });
        if let Some(s) = since_seq {
            params["since_seq"] = serde_json::json!(s);
        }
        let resp = self.call(methods::WORKSPACE_SUBSCRIBE, params).await;
        assert!(resp["error"].is_null(), "subscribe must ack: {resp}");
        resp["result"]["snapshot"]["cursor"]
            .as_i64()
            .unwrap_or_else(|| panic!("subscribe ack must carry a real cursor: {resp}"))
    }

    /// Create an issue (drives one `IssueCreated` event through the outbox).
    async fn create_issue(&mut self, workspace_id: &str, title: &str) {
        let resp = self
            .call(
                methods::HANGAR_ISSUE_CREATE,
                serde_json::json!({
                    "workspace_id": workspace_id,
                    "title": title,
                    "creator": "member:u1",
                }),
            )
            .await;
        assert!(resp["error"].is_null(), "issue_create must ack: {resp}");
    }
}

/// Bind + serve the real listener over a seeded store, WITH the durable event
/// outbox drain wired (mirrors `boot()`): events driven through the dispatcher
/// are persisted to `event_log`, so a later subscribe can replay them.
async fn start_server(dir: &std::path::Path) -> (std::path::PathBuf, Store) {
    let store = Store::open_in(dir).await.unwrap();
    seed::seed_p4_fixture(store.pool()).await.unwrap();
    rpc::auth::ensure_socket_token(store.pool(), dir)
        .await
        .expect("ensure socket token");
    let socket_path = rpc::socket_path_in(dir);
    let listener = rpc::bind(&socket_path).expect("bind socket");

    // The durable broker + its outbox drain: emissions land in `event_log`.
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

/// Poll the durable outbox until `workspace_id` has logged at least `want`
/// events — the drain persists asynchronously, so a replay assertion waits for
/// the write to land rather than racing it.
async fn wait_for_head(store: &Store, workspace_id: &str, want: i64) -> i64 {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let head = EventOutboxRepo::head_seq(store.pool(), workspace_id).await.unwrap();
        if head >= want {
            return head;
        }
        assert!(
            Instant::now() < deadline,
            "outbox never reached head {want} (got {head})"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// A late-joining subscriber that carries `since_seq = 0` replays the WHOLE
/// durable backlog, oldest first, before going live — the core resume contract.
#[tokio::test]
async fn subscribe_with_since_seq_replays_the_durable_backlog_in_order() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, store) = start_server(dir.path()).await;

    // A driver creates three issues while NO subscriber is attached (late join).
    let mut driver = Client::connect(&socket_path).await;
    driver.auth_from_file(dir.path()).await;
    for title in ["issue-A", "issue-B", "issue-C"] {
        driver.create_issue(seed::WS_SLUG, title).await;
    }
    // Wait until all three events durably landed.
    let head = wait_for_head(&store, WS_ID, 3).await;
    assert_eq!(head, 3, "three issue_created events logged");

    // A fresh subscriber joins LATE and resumes from the very start.
    let mut sub = Client::connect(&socket_path).await;
    sub.auth_from_file(dir.path()).await;
    let cursor = sub.subscribe_since(seed::WS_SLUG, Some(0)).await;
    assert_eq!(cursor, 3, "the ack reports the real head cursor");

    // The backlog arrives as replay deltas, oldest first.
    let mut titles = Vec::new();
    for _ in 0..3 {
        let ev = sub
            .next_event(Duration::from_secs(5))
            .await
            .expect("replay must deliver each backlog event");
        assert_eq!(ev["event"], "issue_created", "replayed a lifecycle event");
        titles.push(ev["title"].as_str().unwrap().to_string());
    }
    assert_eq!(
        titles,
        ["issue-A", "issue-B", "issue-C"],
        "replay is in seq order"
    );
}

/// Resuming from a mid-log cursor replays ONLY the events after it — the
/// daemon-restart / reconnect case where the client already saw the earlier
/// events and must not re-receive them.
#[tokio::test]
async fn resume_from_a_mid_cursor_replays_only_newer_events() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, store) = start_server(dir.path()).await;

    let mut driver = Client::connect(&socket_path).await;
    driver.auth_from_file(dir.path()).await;

    // First two issues, then capture the cursor a client would have recorded.
    driver.create_issue(seed::WS_SLUG, "issue-A").await;
    driver.create_issue(seed::WS_SLUG, "issue-B").await;
    let mid = wait_for_head(&store, WS_ID, 2).await;
    assert_eq!(mid, 2);

    // A third issue happens "after the client went away".
    driver.create_issue(seed::WS_SLUG, "issue-C").await;
    wait_for_head(&store, WS_ID, 3).await;

    // Resuming from the mid cursor yields ONLY issue-C.
    let mut sub = Client::connect(&socket_path).await;
    sub.auth_from_file(dir.path()).await;
    let cursor = sub.subscribe_since(seed::WS_SLUG, Some(mid)).await;
    assert_eq!(cursor, 3, "the ack still reports the current head");

    let ev = sub
        .next_event(Duration::from_secs(5))
        .await
        .expect("resume must deliver the one missed event");
    assert_eq!(ev["event"], "issue_created");
    assert_eq!(
        ev["title"], "issue-C",
        "only the post-cursor event is replayed"
    );

    // Nothing else is replayed (the two pre-cursor events are not re-sent).
    assert!(
        sub.next_event(Duration::from_millis(300)).await.is_none(),
        "events at or before the resume cursor are never replayed"
    );
}

/// A plain subscribe (no `since_seq`) replays NOTHING — the pre-cursor behaviour
/// stays intact; only an explicit resume cursor triggers a backlog push.
#[tokio::test]
async fn subscribe_without_since_seq_replays_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, store) = start_server(dir.path()).await;

    let mut driver = Client::connect(&socket_path).await;
    driver.auth_from_file(dir.path()).await;
    driver.create_issue(seed::WS_SLUG, "issue-A").await;
    wait_for_head(&store, WS_ID, 1).await;

    let mut sub = Client::connect(&socket_path).await;
    sub.auth_from_file(dir.path()).await;
    let cursor = sub.subscribe_since(seed::WS_SLUG, None).await;
    assert_eq!(
        cursor, 1,
        "the ack still reports the head even without a resume"
    );

    assert!(
        sub.next_event(Duration::from_millis(300)).await.is_none(),
        "a plain subscribe pushes no backlog"
    );
}

/// Workspace isolation on replay: a resume on workspace B never replays
/// workspace A's durable log, even though A's events hold the only (globally
/// earlier) sequences.
#[tokio::test]
async fn replay_never_crosses_workspace_boundary() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, store) = start_server(dir.path()).await;
    seed_foreign_workspace(&store).await;

    // All events are created in workspace A.
    let mut driver = Client::connect(&socket_path).await;
    driver.auth_from_file(dir.path()).await;
    driver.create_issue(seed::WS_SLUG, "issue-A").await;
    driver.create_issue(seed::WS_SLUG, "issue-B").await;
    wait_for_head(&store, WS_ID, 2).await;

    // A subscriber resumes workspace B from the very start.
    let mut sub_b = Client::connect(&socket_path).await;
    sub_b.auth_from_file(dir.path()).await;
    let cursor_b = sub_b.subscribe_since(WS_B_SLUG, Some(0)).await;
    assert_eq!(cursor_b, 0, "workspace B has an empty log (zero head)");
    assert!(
        sub_b.next_event(Duration::from_millis(300)).await.is_none(),
        "workspace A's events must never replay to a workspace-B subscriber"
    );

    // Positive control: A's own resume DOES replay its two events (so the
    // negative above proves isolation, not a broken replay pipeline).
    let mut sub_a = Client::connect(&socket_path).await;
    sub_a.auth_from_file(dir.path()).await;
    sub_a.subscribe_since(seed::WS_SLUG, Some(0)).await;
    let ev = sub_a
        .next_event(Duration::from_secs(5))
        .await
        .expect("workspace A resumes its own log");
    assert_eq!(ev["title"], "issue-A");
}
