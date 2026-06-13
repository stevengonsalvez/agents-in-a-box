//! Soak / backpressure integration tripwire (e38.32).
//!
//! No prior test drove the daemon's event broadcast path under a *sustained*
//! stream — the transcript buffer, the [`broadcast`] event channel, and the
//! per-connection forwarder were only ever sampled across a handful of events.
//! A real leak or a drop/reorder under backpressure would slip through.
//!
//! This drives a long burst of mutating RPCs (default 500, env-tunable via
//! `HANGAR_SOAK_EVENTS`) through the REAL daemon socket path: each
//! `hangar/issue_create` commits a row and emits one `issue_created`
//! [`HangarEvent`] onto the broker, which the subscribed framed-socket
//! connection receives as a pushed `hangar/event` notification. Three
//! invariants are asserted:
//!
//! 1. **No dropped / out-of-order events** — every create carries a strictly
//!    increasing title (`soak-000001`, `soak-000002`, …); the subscriber must
//!    receive exactly that title sequence, once each, in order. There is no
//!    monotonic seq id on the wire ([`HangarEvent`] carries none), so drop /
//!    reorder is detected from the event payload's own ordering: the issue
//!    title is the per-event ordinal. This is the deterministic core.
//! 2. **Bounded memory** — the test process's RSS (the daemon runs in-process
//!    here) is sampled before and after the burst; growth must stay under a
//!    generous fixed ceiling so a real leak trips it but allocator noise does
//!    not. RSS is read portably via `ps -o rss=` (KB); if it is unreadable the
//!    bound is skipped with a surfaced reason rather than flaking.
//! 3. **Still responsive** — after the burst, a follow-up `hangar/issues_list`
//!    snapshot pull (the nav-equivalent RPC every screen issues) must still
//!    return promptly: the connection / daemon is not wedged by backpressure.
//!
//! ## CI reliability
//!
//! The invariants scale with *count*, never wall-clock: there is no fixed
//! sleep. Events are driven as fast as the socket accepts and the subscriber
//! drains them inline, so a weak runner just runs slower — it never flakes the
//! ordering check. To stay inside the broker's bounded `broadcast` capacity
//! (256) the driver paces itself: it never lets the subscriber fall more than a
//! safety window behind, so the at-most-once broadcast channel never sheds the
//! oldest event under our own backpressure (a shed event would be a spurious
//! "drop"). The default count (500) is CI-sane; a dev box sets
//! `HANGAR_SOAK_EVENTS` higher for a full soak. The RSS ceiling is generous
//! (32 MB over baseline) and skip-guarded.

use std::time::{Duration, Instant};

use ainb_hangar_daemon::rpc::{self, DaemonHealth};
use ainb_hangar_daemon::seed::{self, WS_ID};
use ainb_hangar_proto::events::EVENT_METHOD;
use ainb_hangar_proto::{RpcId, RpcRequest, methods};
use ainb_hangar_store::Store;
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};

/// Default sustained-burst size; `HANGAR_SOAK_EVENTS` overrides it.
const DEFAULT_SOAK_EVENTS: usize = 500;

/// The broker's `broadcast` capacity (mirrors `events::CHANNEL_CAPACITY`). The
/// driver keeps the subscriber within this window so the at-most-once channel
/// never sheds an event under our own load — a shed event would read as a drop.
const BROKER_CAPACITY: usize = 256;

/// Generous RSS-growth ceiling (bytes) over the pre-burst baseline. A genuine
/// per-event leak across hundreds of events blows past this; normal allocator
/// noise stays well under it. Deliberately loose so a weak runner never flakes.
const RSS_GROWTH_CEILING_BYTES: u64 = 32 * 1024 * 1024;

/// One persistent test connection: a buffered reader half kept alive for the
/// whole connection (a fresh `BufReader` per call would swallow pushed frames
/// that land between request/response pairs) plus the writer half.
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
    /// response are discarded (the subscriber connection is the one that
    /// asserts on events, never the driver).
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

    /// Read frames until a `hangar/event` notification arrives (skipping any
    /// responses), or `None` when `timeout` elapses first. Returns the event's
    /// `params` object.
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

    /// Authenticate exactly as a real client does: present the plaintext token
    /// the daemon minted at boot on the first `auth/hello` frame.
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

    /// Subscribe a workspace (by slug — the daemon resolves it to the row id,
    /// like the plugin does) and assert the ack.
    async fn subscribe(&mut self, workspace: &str) {
        let resp = self
            .call(
                methods::WORKSPACE_SUBSCRIBE,
                serde_json::json!({ "workspace_id": workspace }),
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

/// Read this process's resident set size in bytes, portably and WITHOUT
/// `unsafe` (the crate forbids it). `ps -o rss=` prints RSS in kibibytes on
/// macOS and Linux alike. `None` when `ps` is missing or its output cannot be
/// parsed — the caller then skips the RSS bound with a surfaced reason instead
/// of flaking on an unreadable sample.
fn read_self_rss_bytes() -> Option<u64> {
    let pid = std::process::id();
    let out = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let kib: u64 = String::from_utf8_lossy(&out.stdout).trim().parse().ok()?;
    Some(kib * 1024)
}

/// How many events to drive: `HANGAR_SOAK_EVENTS` else [`DEFAULT_SOAK_EVENTS`].
fn soak_event_count() -> usize {
    std::env::var("HANGAR_SOAK_EVENTS")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(DEFAULT_SOAK_EVENTS)
}

/// The soak: a sustained event stream over the real daemon path must deliver
/// every event exactly once, in order, with bounded memory growth, leaving the
/// connection responsive.
#[tokio::test]
async fn soak_stream_delivers_in_order_bounded_and_responsive() {
    let total = soak_event_count();
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, _store) = start_server(dir.path()).await;

    // The subscriber: authenticated + subscribed to the seeded workspace.
    let mut sub = Client::connect(&socket_path).await;
    sub.auth_from_file(dir.path()).await;
    sub.subscribe(seed::WS_SLUG).await;

    // The driver: authenticated, drives the create burst (it never needs a
    // subscription — events flow from the broker to the subscriber connection).
    let mut driver = Client::connect(&socket_path).await;
    driver.auth_from_file(dir.path()).await;

    // Baseline RSS before the burst (may be unreadable -> skip the bound later).
    let rss_before = read_self_rss_bytes();

    // The window we allow the subscriber to lag the driver. Kept well inside
    // the broker's broadcast capacity so the at-most-once channel never sheds
    // an event under our OWN load (which would read as a spurious drop). We
    // drive in batches of this size, then drain that many events before the
    // next batch.
    let lag_window = (BROKER_CAPACITY / 2).max(1);

    let mut next_expected: usize = 1; // titles are 1-based ordinals
    let mut driven: usize = 0;

    while driven < total {
        // Drive up to `lag_window` more creates, each with a strictly
        // increasing title that doubles as the per-event ordinal.
        let batch_end = (driven + lag_window).min(total);
        for ordinal in (driven + 1)..=batch_end {
            let title = format!("soak-{ordinal:06}");
            let resp = driver
                .call(
                    methods::HANGAR_ISSUE_CREATE,
                    serde_json::json!({
                        "workspace_id": WS_ID,
                        "title": title,
                        "creator": "member:user-1",
                    }),
                )
                .await;
            assert!(resp["error"].is_null(), "issue_create must ack: {resp}");
        }
        driven = batch_end;

        // Drain exactly the events this batch produced, asserting strict order.
        while next_expected <= driven {
            let event = sub.next_event(Duration::from_secs(10)).await.unwrap_or_else(|| {
                panic!(
                    "event {next_expected}/{total} never arrived — a dropped \
                         or lost event under backpressure"
                )
            });
            assert_eq!(
                event["event"], "issue_created",
                "wrong event kind at ordinal {next_expected}: {event}"
            );
            let expected_title = format!("soak-{next_expected:06}");
            assert_eq!(
                event["title"], expected_title,
                "out-of-order or dropped event: expected {expected_title} at \
                 position {next_expected}, got {event}"
            );
            next_expected += 1;
        }
    }

    // Every emitted event was received exactly once, in order.
    assert_eq!(
        next_expected - 1,
        total,
        "received {} events but drove {total}",
        next_expected - 1
    );

    // (2) Bounded memory: RSS growth under a generous ceiling, or SKIP-with-
    // reason when RSS is unreadable on this platform — never a flake.
    match (rss_before, read_self_rss_bytes()) {
        (Some(before), Some(after)) => {
            let growth = after.saturating_sub(before);
            assert!(
                growth < RSS_GROWTH_CEILING_BYTES,
                "RSS grew {growth} bytes over {total} events (ceiling \
                 {RSS_GROWTH_CEILING_BYTES}) — a likely leak in the event path",
            );
        }
        _ => {
            eprintln!(
                "SKIP: RSS unreadable via `ps` on this platform; bounded-memory \
                 assertion skipped (ordering + responsiveness still asserted)"
            );
        }
    }

    // (3) Still responsive: a follow-up snapshot pull (the nav-equivalent RPC)
    // must return promptly — the connection is not wedged by the burst.
    let start = Instant::now();
    let resp = sub
        .call(
            methods::HANGAR_ISSUES_LIST,
            serde_json::json!({ "workspace_id": seed::WS_SLUG }),
        )
        .await;
    let elapsed = start.elapsed();
    assert!(
        resp["error"].is_null(),
        "post-burst issues_list must ack: {resp}"
    );
    assert!(
        resp["result"]["issues"].is_array(),
        "post-burst issues_list must return rows: {resp}"
    );
    assert!(
        elapsed < Duration::from_secs(5),
        "post-burst snapshot pull took {elapsed:?} — daemon wedged by backpressure"
    );
}
