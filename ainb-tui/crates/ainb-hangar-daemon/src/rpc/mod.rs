//! The daemon's `UnixListener` JSON-RPC server (P4.10).
//!
//! P3.7's plugin dials `~/.agents-in-a-box/hangar.sock` through the host `unix_socket_dial`
//! cap and speaks the [`ainb_hangar_proto`] JSON-RPC envelope over LSP-style
//! Content-Length framing. P1's daemon never opened that socket — its `boot()`
//! ran the claim-loop FSM only. This module is the missing listener: it binds the
//! socket, accepts plugin connections, decodes framed requests, and answers
//! `workspace/subscribe`, `ping`, and the four P4 snapshot RPCs
//! ([`crate::rpc::snapshots`]) backed by the store repos.
//!
//! ## Wire shape
//!
//! Identical framing to the plugin's [`encode_request`](super) side: each frame
//! is `Content-Length: N\r\n\r\n` followed by `N` bytes of JSON-RPC envelope.
//! [`read_frame`] reassembles one request; [`encode_frame`] frames one response.
//!
//! ## Concurrency
//!
//! Each accepted connection gets its own task; the shared [`SqlitePool`] is
//! cheaply cloned per connection (sqlx pools are internally reference-counted).
//! The dispatcher ([`dispatch`]) is `async` but holds no per-connection mutable
//! state, so two plugins (e.g. a TUI + a CLI probe) can be served in parallel
//! without coordination.
//!
//! ## Event push (e38.2)
//!
//! Responses and pushed `hangar/event` notifications share one connection, so
//! each connection runs a dedicated **writer task** fed by an mpsc channel:
//! the request loop queues response frames, and — once the (authenticated)
//! connection has issued `workspace/subscribe` for a known workspace — a
//! per-connection **forwarder task** taps the daemon-global
//! [`crate::events::EventBroker`], filters to the subscribed workspace's
//! resolved row id, and queues notification frames onto the same channel.
//! Connection close tears both tasks down, deregistering the subscription.
//! Unauthenticated connections never reach the subscribe path (the auth gate
//! closes them first), so only authenticated, subscribed connections ever
//! receive event frames.

pub mod auth;
pub mod snapshots;

use std::path::{Path, PathBuf};
use std::sync::Arc;
#[cfg(any(test, feature = "test-support"))]
use std::sync::{OnceLock, RwLock};
use std::time::Instant;

use ainb_hangar_core::clock::{HangarClock, SystemClock};
use ainb_hangar_core::ids::{AgentId, AutopilotId, SkillId, WorkspaceId};
use ainb_hangar_proto::methods;
use ainb_hangar_proto::settings::{DaemonHealthSnapshot, HealthSnapshot};
use ainb_hangar_proto::{RpcError, RpcId, RpcRequest, RpcResponse};
use futures_util::future::join_all;
use sqlx::SqlitePool;

use crate::events::{
    EventBroker, EventSink, ScopedEvent, encode_event_frame, encode_event_frame_payload,
    encode_notification_frame,
};
use crate::health_stats::HealthStats;
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{broadcast, mpsc};

/// JSON-RPC "method not found" code (spec-reserved).
const METHOD_NOT_FOUND: i32 = -32601;
/// JSON-RPC "invalid params" code (spec-reserved).
const INVALID_PARAMS: i32 = -32602;
/// JSON-RPC "internal error" code (spec-reserved) — used for store faults.
const INTERNAL_ERROR: i32 = -32603;
/// Soft cap on one request body. Snapshot requests are tiny.
const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

#[cfg(any(test, feature = "test-support"))]
static APPROVE_SOCKET_OVERRIDE: OnceLock<RwLock<Option<PathBuf>>> = OnceLock::new();

/// Override Claude broker socket for isolated integration tests.
#[cfg(any(test, feature = "test-support"))]
pub fn set_approve_socket_for_test(path: Option<PathBuf>) {
    *APPROVE_SOCKET_OVERRIDE
        .get_or_init(|| RwLock::new(None))
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = path;
}

/// Immutable daemon facts the `hangar/health` snapshot reports.
///
/// Carried alongside the pool so the dispatcher can answer `hangar/health`
/// without reaching into process globals. `started_at` anchors the uptime
/// computation; `socket_path` echoes the path the daemon actually bound.
#[derive(Debug, Clone)]
pub struct DaemonHealth {
    /// The unix socket path the daemon bound (echoed verbatim to the plugin).
    pub socket_path: String,
    /// The daemon process id.
    pub pid: u32,
    /// Instant the daemon started, for uptime.
    pub started_at: Instant,
    /// Daemon version string (crate version).
    pub version: String,
    /// Shared in-memory health stats collector (the rolling throughput ring +
    /// claim-cache figure) backing the `hangar/daemon_health` pane (P8.5). Shared
    /// with the FSM finalize path that records terminal task outcomes.
    pub stats: Arc<HealthStats>,
}

impl DaemonHealth {
    /// Build the wire [`HealthSnapshot`] for a `connected` link state.
    #[must_use]
    pub fn snapshot(&self, connected: bool) -> HealthSnapshot {
        HealthSnapshot {
            socket_path: self.socket_path.clone(),
            pid: self.pid,
            uptime_secs: self.started_at.elapsed().as_secs(),
            version: self.version.clone(),
            connected,
        }
    }
}

/// Resolve the daemon's socket path from the store directory.
///
/// The socket lives beside the database: `{store_dir}/hangar.sock`. This mirrors
/// the plugin's dial target (`~/.agents-in-a-box/hangar.sock`) when the store resolves to
/// the default `~/.agents-in-a-box`, and follows `$AINB_HANGAR_HOME` when overridden so a
/// test's isolated home gets an isolated socket.
#[must_use]
pub fn socket_path_in(store_dir: &Path) -> PathBuf {
    store_dir.join("hangar.sock")
}

/// Bind the listener at `socket_path`, removing any stale socket file first,
/// and tighten the socket file to `0600` (owner-only).
///
/// The mode is set immediately after the bind so no other local user can even
/// connect to the control plane; the per-connection peer-uid + token gates in
/// [`serve`] are defence in depth behind it.
///
/// # Errors
///
/// Returns an error if the parent directory is missing/unwritable, the bind
/// fails for a reason other than a stale socket file (which is removed and
/// retried once), or the permission tightening fails.
pub fn bind(socket_path: &Path) -> std::io::Result<UnixListener> {
    use std::os::unix::fs::PermissionsExt as _;

    // A leftover socket file from a previous (crashed) daemon would make `bind`
    // fail with AddrInUse even though nothing is listening. Remove it first —
    // this is safe because only one daemon owns a given hangar home at a time.
    if socket_path.exists() {
        let _ = std::fs::remove_file(socket_path);
    }
    let listener = UnixListener::bind(socket_path)?;
    std::fs::set_permissions(socket_path, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

/// Accept connections forever, serving each on its own task.
///
/// `broker` is the daemon-global event broker: each connection that subscribes
/// a workspace gets a scoped forwarder onto it (e38.2).
///
/// Never returns under normal operation; the caller runs it as a background
/// task alongside the claim loop. A single accept error is logged and the loop
/// continues (one bad connection must not down the listener).
pub async fn serve(
    listener: UnixListener,
    pool: SqlitePool,
    health: DaemonHealth,
    broker: EventBroker,
) {
    loop {
        match listener.accept().await {
            Ok((stream, _addr)) => {
                let pool = pool.clone();
                let health = health.clone();
                let broker = broker.clone();
                tokio::spawn(async move {
                    if let Err(e) = serve_conn(stream, pool, health, broker).await {
                        tracing::debug!(error = %e, "hangar rpc connection closed");
                    }
                });
            }
            Err(e) => tracing::warn!(error = %e, "hangar rpc accept failed"),
        }
    }
}

/// Serve one plugin connection: gate it (same-uid peer credentials, then the
/// `auth/hello` token handshake on the first frame), then read framed
/// requests, dispatch, write responses, until EOF.
///
/// All outbound frames — responses AND pushed `hangar/event` notifications —
/// flow through one writer task so they never interleave mid-frame. A
/// `workspace/subscribe` for a known workspace (re)registers this connection's
/// event forwarder; EOF tears the forwarder and writer down, which is the
/// subscription's deregistration.
async fn serve_conn(
    stream: UnixStream,
    pool: SqlitePool,
    health: DaemonHealth,
    broker: EventBroker,
) -> std::io::Result<()> {
    // Gate 1 — kernel peer credentials: only this user's processes may talk to
    // the control plane. Reject + close on mismatch (or on a cred-read fault).
    if !auth::same_uid_peer(&stream).unwrap_or(false) {
        tracing::warn!("hangar rpc: rejected connection from foreign-uid peer");
        return Ok(());
    }

    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);

    // The single writer: every outbound frame is queued here so a pushed event
    // can never split a response frame (or vice versa).
    let (out_tx, mut out_rx) = mpsc::channel::<Vec<u8>>(OUTBOUND_QUEUE);
    let writer = tokio::spawn(async move {
        while let Some(frame) = out_rx.recv().await {
            if write_half.write_all(&frame).await.is_err() {
                break;
            }
            if write_half.flush().await.is_err() {
                break;
            }
        }
    });

    // Gate 2 — first-frame token auth: the connection's first frame must be a
    // valid `auth/hello`. Unauthenticated or wrong-token connections get an
    // UNAUTHORIZED error envelope back, then the connection closes — no
    // `hangar/*` method is dispatched and no event forwarder ever exists.
    let authed = async {
        let Some(first) = read_frame(&mut reader).await? else {
            return Ok(false);
        };
        match auth::authenticate_first_frame(&pool, &first).await {
            Ok(ack) => {
                let _ = out_tx.send(encode_frame(&ack)).await;
                Ok(true)
            }
            Err(rejection) => {
                let _ = out_tx.send(encode_frame(&rejection)).await;
                Ok(false)
            }
        }
    }
    .await;
    let proceed = match authed {
        Ok(p) => p,
        Err(e) => {
            drop(out_tx);
            let _ = writer.await;
            return Err(e);
        }
    };
    if !proceed {
        drop(out_tx);
        let _ = writer.await;
        return Ok(());
    }

    let events = broker.sink();
    // The connection's event subscription: at most one forwarder; a
    // re-subscribe replaces it (last subscribe wins, no duplicate delivery).
    let mut forwarder: Option<tokio::task::JoinHandle<()>> = None;
    // The connection's FLEET-WIDE attention subscription (spec P2), independent
    // of the workspace forwarder: a connection may hold both (workspace events +
    // attention nudges) or either. A re-subscribe replaces it.
    let mut attention_forwarder: Option<tokio::task::JoinHandle<()>> = None;
    // Fleet uses a durable global revision stream, independent from workspace
    // and attention subscriptions. Re-subscribing replaces the prior cursor.
    let mut fleet_forwarder: Option<tokio::task::JoinHandle<()>> = None;

    // Idle read timeout so an abandoned / half-open client connection cannot pin
    // this per-connection task (and its fd) forever. The RPC is request/response
    // and clients reconnect per request, so a generous idle window only reclaims
    // dead connections — it never interrupts an in-flight exchange.
    const IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(600);
    let served: std::io::Result<()> = async {
        while let Some(body) =
            match tokio::time::timeout(IDLE_TIMEOUT, read_frame(&mut reader)).await {
                Ok(frame_result) => frame_result?,
                Err(_elapsed) => {
                    tracing::debug!("rpc connection idle {IDLE_TIMEOUT:?}; closing");
                    None
                }
            }
        {
            let req = serde_json::from_slice::<RpcRequest>(&body);
            // Subscribe before dispatch reads the snapshot. Events raised while
            // the snapshot query runs stay buffered in this receiver and are
            // drained after the acknowledgement, closing the snapshot-to-live
            // handoff gap without allowing an event to precede the response.
            let pending_attention_rx = req.as_ref().ok().and_then(|request| {
                (request.method == methods::ATTENTION_SUBSCRIBE)
                    .then(|| broker.subscribe_attention())
            });
            let pending_fleet_rx = req.as_ref().ok().and_then(|request| {
                (request.method == methods::FLEET_SUBSCRIBE).then(|| broker.subscribe_fleet())
            });
            let resp = match &req {
                Ok(req) => dispatch(&pool, req, &health, &events).await,
                Err(e) => RpcResponse {
                    jsonrpc: ainb_hangar_proto::jsonrpc_version(),
                    // We could not parse an id; reply with a null/0 id so the
                    // peer still sees a framed error rather than a dropped
                    // connection.
                    id: RpcId::Number(0),
                    result: None,
                    error: Some(RpcError {
                        code: INVALID_PARAMS,
                        message: format!("malformed request: {e}"),
                        data: None,
                    }),
                },
            };
            let acked = resp.error.is_none();
            if out_tx.send(encode_frame(&resp)).await.is_err() {
                break; // writer gone — the connection is dead
            }
            // Register the event subscription AFTER queueing the ack so the
            // ack frame always precedes the first pushed event.
            if let Ok(req) = &req {
                if acked && req.method == methods::WORKSPACE_SUBSCRIBE {
                    if let Ok(Some(ws)) = resolve(&pool, req).await {
                        if let Some(old) = forwarder.take() {
                            old.abort();
                        }
                        // Register the LIVE forwarder FIRST so no event emitted
                        // from now on is missed, THEN replay the durable backlog.
                        // This ordering guarantees no gap — at worst a boundary
                        // event delivered twice (live + replayed), which the
                        // plugin reconciles via the next snapshot pull.
                        forwarder = Some(spawn_event_forwarder(
                            broker.subscribe(),
                            ws.clone(),
                            out_tx.clone(),
                        ));
                        // T1 resume: a client that carried a `since_seq` catches
                        // up on every durable event after that cursor before it
                        // goes live. Best-effort (a read fault is logged, the
                        // connection stays live).
                        if let Some(since) = subscribe_since_seq(req) {
                            replay_events(&pool, &ws, since, &out_tx).await;
                        }
                    }
                } else if acked && req.method == methods::ATTENTION_SUBSCRIBE {
                    // Register the FLEET-WIDE attention forwarder. The ack above
                    // already carried the current open snapshot; from here the
                    // connection receives live AttentionRaised / AttentionAnswered
                    // deltas. Unlike the workspace forwarder this is NOT filtered
                    // by workspace — it carries the no-workspace host sessions —
                    // with an OPTIONAL narrowing when the client passed a
                    // workspace_id.
                    if let Some(old) = attention_forwarder.take() {
                        old.abort();
                    }
                    let filter = attention_subscribe_filter(req);
                    let rx = pending_attention_rx.unwrap_or_else(|| broker.subscribe_attention());
                    attention_forwarder =
                        Some(spawn_attention_forwarder(rx, filter, out_tx.clone()));
                } else if acked && req.method == methods::FLEET_SUBSCRIBE {
                    if let Some(old) = fleet_forwarder.take() {
                        old.abort();
                    }
                    let head_revision = resp
                        .result
                        .as_ref()
                        .and_then(|value| value.get("snapshot"))
                        .and_then(|value| value.get("head_revision"))
                        .and_then(serde_json::Value::as_i64)
                        .unwrap_or_default();
                    let rx = pending_fleet_rx.unwrap_or_else(|| broker.subscribe_fleet());
                    fleet_forwarder = Some(spawn_fleet_forwarder(
                        pool.clone(),
                        rx,
                        head_revision,
                        out_tx.clone(),
                    ));
                }
            }
        }
        Ok(())
    }
    .await;

    if let Some(f) = forwarder {
        f.abort();
    }
    if let Some(f) = attention_forwarder {
        f.abort();
    }
    if let Some(f) = fleet_forwarder {
        f.abort();
    }
    drop(out_tx);
    let _ = writer.await;
    served
}

/// Outbound frame queue depth per connection (responses + pushed events).
const OUTBOUND_QUEUE: usize = 64;

/// Spawn the per-connection event forwarder: drain the broker, keep only
/// events scoped to `workspace_id` (the resolved row id), frame each as a
/// `hangar/event` notification, and queue it on the connection's writer.
///
/// Ends when the broker closes, or the connection's writer is gone. A lagged
/// receiver (consumer slower than the broadcast buffer) drops the lost events
/// and keeps streaming — the next snapshot pull reconciles authoritatively.
fn spawn_event_forwarder(
    mut rx: broadcast::Receiver<ScopedEvent>,
    workspace_id: String,
    out: mpsc::Sender<Vec<u8>>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(scoped) => {
                    // The workspace boundary: a foreign workspace's event is
                    // never forwarded onto this connection.
                    if scoped.workspace_id != workspace_id {
                        continue;
                    }
                    if out.send(encode_event_frame(&scoped.event)).await.is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(missed)) => {
                    tracing::debug!(missed, "hangar event stream lagged; events dropped");
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    })
}

/// Extract the optional workspace narrowing from an `attention/subscribe`
/// request. Absent or malformed params → `None` (a fleet-wide subscription).
fn attention_subscribe_filter(req: &RpcRequest) -> Option<String> {
    serde_json::from_value::<ainb_hangar_proto::snapshots::AttentionSubscribeParams>(
        req.params.clone(),
    )
    .ok()
    .and_then(|p| p.workspace_id)
}

/// Spawn the per-connection FLEET-WIDE attention forwarder (spec P2): drain the
/// broker's dedicated attention stream, frame each `AttentionRaised` /
/// `AttentionAnswered` as a `hangar/event` notification, and queue it on the
/// connection's writer.
///
/// Unlike [`spawn_event_forwarder`] this is NOT filtered by workspace — attention
/// is host-wide, so it carries the no-workspace host sessions the workspace
/// forwarder drops. An OPTIONAL `filter` narrows it to one workspace: only
/// `AttentionRaised` carries a `workspace_id`, so the filter applies there;
/// `AttentionAnswered` (a bare "row X answered" nudge) is always forwarded — a
/// surface that does not hold the row simply ignores it. Ends when the broker
/// closes or the writer is gone; a lagged receiver drops the missed nudges and
/// keeps streaming (the next `attention/list` pull reconciles authoritatively).
fn spawn_attention_forwarder(
    mut rx: broadcast::Receiver<ainb_hangar_proto::events::HangarEvent>,
    filter: Option<String>,
    out: mpsc::Sender<Vec<u8>>,
) -> tokio::task::JoinHandle<()> {
    use ainb_hangar_proto::events::HangarEvent;
    tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(event) => {
                    // Optional workspace narrowing: applies only to the
                    // workspace-bearing AttentionRaised. A `None`-workspace (host)
                    // event never matches a filter, so a narrowed subscription
                    // correctly excludes host sessions.
                    if let Some(ws) = &filter {
                        if let HangarEvent::AttentionRaised { workspace_id, .. } = &event {
                            if workspace_id.as_deref() != Some(ws.as_str()) {
                                continue;
                            }
                        }
                    }
                    if out.send(encode_event_frame(&event)).await.is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(missed)) => {
                    tracing::debug!(missed, "attention stream lagged; nudges dropped");
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    })
}

/// Spawn a gapless durable Fleet revision forwarder.
///
/// Receiver registration happens before snapshot read. After the snapshot ack
/// is queued, this task drains every durable row after that snapshot head, then
/// uses broadcast revisions only as wakeups. Lag asks the client to reconcile
/// from a fresh snapshot instead of silently claiming a complete stream.
fn spawn_fleet_forwarder(
    pool: SqlitePool,
    mut rx: broadcast::Receiver<i64>,
    mut cursor: i64,
    out: mpsc::Sender<Vec<u8>>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let events = match crate::fleet::events_after_wire(&pool, cursor, REPLAY_BATCH).await {
                Ok(events) => events,
                Err(error) => {
                    tracing::warn!(error = %error, "fleet event replay read failed");
                    return;
                }
            };
            if !events.is_empty() {
                for event in events {
                    cursor = event.revision;
                    let Ok(params) = serde_json::to_value(&event) else {
                        continue;
                    };
                    if out.send(encode_notification_frame("fleet/event", &params)).await.is_err() {
                        return;
                    }
                }
                continue;
            }

            match rx.recv().await {
                Ok(_revision) => {}
                Err(broadcast::error::RecvError::Lagged(missed)) => {
                    let params = serde_json::json!({
                        "after_revision": cursor,
                        "missed": missed,
                    });
                    let _ =
                        out.send(encode_notification_frame("fleet/resync_required", &params)).await;
                    return;
                }
                Err(broadcast::error::RecvError::Closed) => return,
            }
        }
    })
}

/// Max events read from the durable log in one catch-up query. A resume cursor
/// far in the past still bounds each burst; a backlog larger than this is drained
/// by paging in-loop (see [`replay_events`]), never truncated to a single batch.
const REPLAY_BATCH: i64 = 1024;

/// Extract the optional `since_seq` resume cursor from a `workspace/subscribe`
/// request. Absent or malformed params → `None` (a plain subscribe with no
/// backlog, the pre-cursor behaviour).
fn subscribe_since_seq(req: &RpcRequest) -> Option<i64> {
    serde_json::from_value::<ainb_hangar_proto::snapshots::WorkspaceSubscribeParams>(
        req.params.clone(),
    )
    .ok()
    .and_then(|p| p.since_seq)
}

/// Replay a workspace's durable events after `since_seq` onto the connection's
/// writer as `hangar/event` notifications (T1 catch-up).
///
/// Each stored payload is re-framed verbatim via [`encode_event_frame_payload`],
/// so a replayed frame is byte-identical to the live one it mirrors — a resuming
/// subscriber cannot tell catch-up from live.
///
/// The backlog is **paged in-loop**, not read once: the durable log holds one
/// row per emitted event (including high-frequency `TaskProgress`/`TaskMessage`
/// heartbeats), so a single active task can exceed [`REPLAY_BATCH`] during a
/// disconnect. A single capped read delivers the OLDEST `REPLAY_BATCH` events
/// and would silently drop the newest `(since_seq + REPLAY_BATCH, head]` window
/// — the live forwarder (registered before this call) only carries events
/// emitted after subscribe, and the ack advances the client's cursor to the
/// true head, so that window would be lost with no way for the client to detect
/// or drain it. Paging until a short batch signals the head reconstructs a
/// gapless stream; anything appended while we page is covered by the forwarder
/// (at worst a boundary event delivered twice, reconciled by the next snapshot
/// pull).
///
/// Best-effort: a read fault is logged and the connection stays live; a gone
/// writer ends the push early (the forwarder keeps the live stream).
async fn replay_events(
    pool: &SqlitePool,
    workspace_id: &str,
    since_seq: i64,
    out: &mpsc::Sender<Vec<u8>>,
) {
    let mut cursor = since_seq;
    loop {
        let rows = match ainb_hangar_store::repo::event_log::EventOutboxRepo::replay(
            pool,
            workspace_id,
            cursor,
            REPLAY_BATCH,
        )
        .await
        {
            Ok(rows) => rows,
            Err(e) => {
                tracing::warn!(error = %e, "event replay read failed");
                return;
            }
        };
        // A short batch means we have reached the workspace head at read time;
        // record it before consuming `rows` (which moves the elements).
        let drained = (rows.len() as i64) < REPLAY_BATCH;
        for row in rows {
            // Advance the cursor to every read seq — including a malformed one —
            // so the next page starts strictly past it and the loop cannot spin.
            cursor = row.seq;
            // The stored payload IS the serialised `HangarEvent` that was the
            // notification's `params`; parse-then-frame it verbatim.
            let params: serde_json::Value = match serde_json::from_str(&row.payload) {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(error = %e, seq = row.seq, "skipping malformed replay payload");
                    continue;
                }
            };
            if out.send(encode_event_frame_payload(&params)).await.is_err() {
                return; // writer gone — the live forwarder owns the rest
            }
        }
        if drained {
            return;
        }
    }
}

/// Dispatch one decoded request to its handler, returning the response envelope.
///
/// Pure of socket IO (the caller owns the stream); only touches the store and —
/// for the mutating handlers — publishes the matching [`HangarEvent`] onto
/// `events` after the write commits. Every method echoes the request id; an
/// unknown method answers `-32601`.
pub async fn dispatch(
    pool: &SqlitePool,
    req: &RpcRequest,
    health: &DaemonHealth,
    events: &EventSink,
) -> RpcResponse {
    let result = handle(pool, req, health, events).await;
    match result {
        Ok(value) => ok(req.id.clone(), value),
        Err(err) => RpcResponse {
            jsonrpc: ainb_hangar_proto::jsonrpc_version(),
            id: req.id.clone(),
            result: None,
            error: Some(err),
        },
    }
}

/// The fallible dispatch core: route `method` to its handler, mapping store
/// faults to an internal-error envelope and unknown methods to method-not-found.
async fn handle(
    pool: &SqlitePool,
    req: &RpcRequest,
    health: &DaemonHealth,
    events: &EventSink,
) -> Result<serde_json::Value, RpcError> {
    match req.method.as_str() {
        methods::PING => Ok(serde_json::json!({})),
        // `workspace/subscribe` acks with the workspace's REAL snapshot: the
        // current head of its durable event log (T1), so a client records where
        // to resume from. The live push + backlog replay are the stream side
        // (see `serve_conn`); the plugin only needs a non-error ack to reach
        // `Connected`, then pulls the screen snapshots. The ack is unconditional —
        // an unknown workspace has a zero cursor, never an error.
        methods::WORKSPACE_SUBSCRIBE => {
            let cursor = match resolve(pool, req).await? {
                Some(ws) => {
                    ainb_hangar_store::repo::event_log::EventOutboxRepo::head_seq(pool, &ws)
                        .await
                        .map_err(|e| store_err(&e))?
                }
                None => 0,
            };
            to_value(&ainb_hangar_proto::snapshots::SubscribeResult {
                snapshot: ainb_hangar_proto::snapshots::SubscribeSnapshot { cursor },
            })
        }
        methods::HANGAR_ISSUES_LIST => {
            let issues = match resolve(pool, req).await? {
                Some(ws) => snapshots::issues_list(pool, &ws).await.map_err(|e| store_err(&e))?,
                None => Vec::new(),
            };
            to_value(&ainb_hangar_proto::snapshots::IssuesListResult { issues })
        }
        methods::HANGAR_ISSUES_SEARCH => handle_issues_search(pool, req).await,
        methods::HANGAR_SEARCH => handle_search(pool, req).await,
        methods::HANGAR_AGENTS_LIST => {
            let actors = match resolve(pool, req).await? {
                Some(ws) => snapshots::agents_list(pool, &ws, SystemClock.now_ms())
                    .await
                    .map_err(|e| store_err(&e))?,
                None => Vec::new(),
            };
            to_value(&ainb_hangar_proto::snapshots::AgentsListResult { actors })
        }
        methods::HANGAR_SKILLS_LIST => {
            let skills = match resolve(pool, req).await? {
                Some(ws) => snapshots::skills_list(pool, &ws).await.map_err(|e| store_err(&e))?,
                None => Vec::new(),
            };
            to_value(&ainb_hangar_proto::snapshots::SkillsListResult { skills })
        }
        methods::HANGAR_SKILL_GET => {
            let params: ainb_hangar_proto::snapshots::SkillGetParams =
                parse_params(req, "{ workspace_id, skill_id }")?;
            let detail = match resolve_wire(pool, &params.workspace_id).await? {
                Some(ws) => {
                    let skill = skill_id(&params.skill_id)?;
                    snapshots::skill_get(pool, &ws, &skill).await.map_err(|e| skill_repo_err(&e))?
                }
                None => None,
            };
            // A missing skill (or unknown workspace) answers `null` — the detail
            // pane simply renders nothing, never an error.
            to_value(&detail)
        }
        methods::HANGAR_SKILLS_SYNC => {
            let params: ainb_hangar_proto::snapshots::SkillsSyncParams =
                parse_params(req, "{ workspace_id, source_path? }")?;
            let Some(ws) = resolve_wire(pool, &params.workspace_id).await? else {
                return Err(invalid_params(&format!(
                    "unknown workspace `{}`",
                    params.workspace_id
                )));
            };
            let source = match params.source_path.as_deref() {
                Some(p) => std::path::PathBuf::from(p),
                None => crate::skills_sync::default_source_dir().ok_or_else(|| {
                    invalid_params(
                        "no skills source: set $AINB_TOOLKIT_SKILLS_DIR or pass source_path",
                    )
                })?,
            };
            let report = snapshots::skills_sync(pool, &ws, &source)
                .await
                .map_err(|e| internal(&format!("skills sync: {e}")))?;
            to_value(&report)
        }
        methods::HANGAR_SKILL_ATTACH => attach_or_detach(pool, req, true).await,
        methods::HANGAR_SKILL_DETACH => attach_or_detach(pool, req, false).await,
        methods::HANGAR_AUTOPILOTS_LIST
        | methods::HANGAR_AUTOPILOT_RUNS
        | methods::HANGAR_AUTOPILOT_FIRE_NOW
        | methods::HANGAR_AUTOPILOT_SET_ENABLED => handle_autopilot(pool, req, events).await,
        methods::HANGAR_TASKS_LIST => handle_tasks_list(pool, req).await,
        methods::HANGAR_TASK_TRANSITION => handle_task_transition(pool, req, events).await,
        methods::HANGAR_TASK_RETRY => handle_task_retry(pool, req, events).await,
        methods::HANGAR_ISSUE_CREATE => handle_issue_create(pool, req, events).await,
        methods::HANGAR_ISSUE_DELETE => handle_issue_delete(pool, req, events).await,
        methods::HANGAR_ISSUE_CANCEL_ACTIVE => handle_issue_cancel_active(pool, req, events).await,
        methods::HANGAR_ISSUE_UPDATE => handle_issue_update(pool, req, events).await,
        methods::HANGAR_ISSUE_LABEL_ATTACH => handle_issue_label(pool, req, events, true).await,
        methods::HANGAR_ISSUE_LABEL_DETACH => handle_issue_label(pool, req, events, false).await,
        methods::HANGAR_COMMENT_ADD => handle_comment_add(pool, req, events).await,
        methods::HANGAR_AGENT_CREATE => handle_agent_create(pool, req).await,
        methods::HANGAR_AGENT_DELETE => handle_agent_delete(pool, req).await,
        methods::HANGAR_AGENT_UPDATE => handle_agent_update(pool, req).await,
        methods::HANGAR_AGENT_ARCHIVE => handle_agent_archive(pool, req).await,
        methods::HANGAR_MEMBERS_LIST => handle_members_list(pool, req).await,
        methods::HANGAR_MEMBER_SET_ROLE => handle_member_set_role(pool, req).await,
        methods::HANGAR_MEMBER_REMOVE => handle_member_remove(pool, req).await,
        methods::HANGAR_SQUADS_LIST => handle_squads_list(pool, req).await,
        methods::HANGAR_SQUAD_CREATE => handle_squad_create(pool, req).await,
        methods::HANGAR_SQUAD_MEMBER_ADD => handle_squad_member(pool, req, true).await,
        methods::HANGAR_SQUAD_MEMBER_REMOVE => handle_squad_member(pool, req, false).await,
        methods::HANGAR_SQUAD_ASSIGN => handle_squad_assign(pool, req).await,
        methods::HANGAR_SQUAD_FANOUT => handle_squad_fanout(pool, req).await,
        methods::HANGAR_HEALTH => to_value(&health.snapshot(true)),
        methods::HANGAR_DAEMON_HEALTH => handle_daemon_health(pool, req, health).await,
        methods::HANGAR_USAGE_ROLLUP => handle_usage_rollup(pool, req).await,
        methods::HANGAR_RUN_HISTORY => handle_run_history(pool, req).await,
        methods::HANGAR_PR_STATUS_REFRESH => handle_pr_status_refresh(pool, req, events).await,
        methods::HANGAR_INBOX_LIST => handle_inbox_list(pool, req).await,
        methods::HANGAR_INBOX_MARK_READ => handle_inbox_mark_read(pool, req).await,
        methods::HANGAR_BOARDS_LIST => handle_boards_list(pool, req).await,
        methods::HANGAR_BOARD_CREATE => handle_board_create(pool, req).await,
        methods::HANGAR_BOARD_UPDATE => handle_board_update(pool, req).await,
        methods::HANGAR_BOARD_DELETE => handle_board_delete(pool, req).await,
        methods::HANGAR_BOARD_COLUMN_ADD => handle_board_column_add(pool, req).await,
        methods::HANGAR_BOARD_COLUMN_UPDATE => handle_board_column_update(pool, req).await,
        methods::HANGAR_BOARD_COLUMN_DELETE => handle_board_column_delete(pool, req).await,
        methods::HANGAR_BOARD_COLUMN_REORDER => handle_board_column_reorder(pool, req).await,
        methods::HANGAR_BOARD_CARD_ADD => handle_board_card(pool, req, true).await,
        methods::HANGAR_BOARD_CARD_MOVE => handle_board_card(pool, req, false).await,
        methods::HANGAR_BOARD_CARD_CREATE => handle_board_card_create(pool, req).await,
        methods::HANGAR_BOARD_CARD_RUN => handle_board_card_run(pool, req).await,
        methods::HANGAR_ISSUE_RUN => handle_issue_run(pool, req).await,
        methods::HANGAR_BOARD_CARD_CANCEL => handle_board_card_cancel(pool, req, events).await,
        methods::HANGAR_BOARD_CARD_REORDER => handle_board_card_reorder(pool, req).await,
        methods::HANGAR_BOARD_CARD_REMOVE => handle_board_card_remove(pool, req).await,
        methods::HANGAR_BOARD_CARD_TIMELINE => handle_board_card_timeline(pool, req).await,
        methods::HANGAR_BOARD_CARD_ASSIGN_SQUAD => handle_board_card_assign_squad(pool, req).await,
        methods::HANGAR_BOARD_CARD_DEP_ADD => handle_board_card_dep(pool, req, true).await,
        methods::HANGAR_BOARD_CARD_DEP_REMOVE => handle_board_card_dep(pool, req, false).await,
        methods::HANGAR_BOARD_CARD_SET_AUTO_RUN => handle_board_card_set_auto_run(pool, req).await,
        methods::HANGAR_REPO_LIST => handle_repo_list(req),
        methods::FLEET_SNAPSHOT => handle_fleet_snapshot(pool).await,
        // Receiver registration occurs in `serve_conn` before this snapshot is
        // read. The ack carries its exact head, then the forwarder drains rows
        // committed after that head before waiting for live wakeups.
        methods::FLEET_SUBSCRIBE => handle_fleet_subscribe(pool, req).await,
        methods::FLEET_ACTION => handle_fleet_action(pool, req, events).await,
        methods::FLEET_BROADCAST => handle_fleet_broadcast(pool, req, events).await,
        methods::ATTENTION_LIST => handle_attention_list(pool, req).await,
        // `attention/subscribe` acks with the current OPEN snapshot; the live
        // fleet-wide forwarder is the stream side (see `serve_conn`).
        methods::ATTENTION_SUBSCRIBE => handle_attention_subscribe(pool, req).await,
        methods::ATTENTION_ANSWER => handle_attention_answer(pool, req, events).await,
        methods::ATC_REGISTER => handle_atc_register(pool, req).await,
        methods::ATC_LIST => handle_atc_list(pool).await,
        methods::ATC_ESCALATE => handle_atc_escalate(pool, req, events).await,
        methods::ATC_UNREGISTER => handle_atc_unregister(pool, req).await,
        methods::PROFILE_LIST => handle_profile_list(pool).await,
        methods::PROFILE_GET => handle_profile_get(pool, req).await,
        methods::PROFILE_UPSERT => handle_profile_upsert(pool, req).await,
        methods::HANGAR_NOTIFY_RULES_LIST => handle_notify_rules_list(pool, req).await,
        methods::HANGAR_NOTIFY_RULE_SET => handle_notify_rule_set(pool, req).await,
        methods::HANGAR_DAEMON_CONFIG_GET => handle_daemon_config_get(pool, req).await,
        methods::HANGAR_DAEMON_CONFIG_SET => handle_daemon_config_set(pool, req).await,
        methods::HANGAR_DAEMON_CONFIG_LIST => handle_daemon_config_list(pool).await,
        other => Err(RpcError {
            code: METHOD_NOT_FOUND,
            message: format!("unknown method: {other}"),
            data: None,
        }),
    }
}

/// Return the authoritative host Fleet snapshot.
async fn handle_fleet_snapshot(pool: &SqlitePool) -> Result<serde_json::Value, RpcError> {
    let snapshot = crate::fleet::snapshot_wire(pool).await.map_err(|error| store_err(&error))?;
    to_value(&snapshot)
}

/// Register a revision cursor and return the snapshot head paired with it.
async fn handle_fleet_subscribe(
    pool: &SqlitePool,
    req: &RpcRequest,
) -> Result<serde_json::Value, RpcError> {
    let params: ainb_hangar_proto::fleet::FleetSubscribeParams =
        parse_params(req, "{ after_revision }")?;
    if params.after_revision < 0 {
        return Err(invalid_params("after_revision must be non-negative"));
    }
    let snapshot = crate::fleet::snapshot_wire(pool).await.map_err(|error| store_err(&error))?;
    to_value(&ainb_hangar_proto::fleet::FleetSubscribeResult {
        snapshot,
        replay: Vec::new(),
    })
}

/// Execute one optimistic, idempotent Fleet action.
async fn handle_fleet_action(
    pool: &SqlitePool,
    req: &RpcRequest,
    events: &EventSink,
) -> Result<serde_json::Value, RpcError> {
    let params: ainb_hangar_proto::fleet::FleetActionParams =
        parse_params(req, "{ session_key, expected_version, request_id, action }")?;
    let receipt = execute_fleet_action(pool, params, None, events).await?;
    to_value(&ainb_hangar_proto::fleet::FleetActionResult { receipt })
}

/// Deliver one text prompt to explicit stable recipients with bounded fanout.
async fn handle_fleet_broadcast(
    pool: &SqlitePool,
    req: &RpcRequest,
    events: &EventSink,
) -> Result<serde_json::Value, RpcError> {
    use std::collections::HashSet;
    use tokio::sync::Semaphore;

    let params: ainb_hangar_proto::fleet::FleetBroadcastParams =
        parse_params(req, "{ target_keys, text, idempotency_key }")?;
    if params.text.trim().is_empty() {
        return Err(invalid_params("broadcast text must not be empty"));
    }
    if params.idempotency_key.trim().is_empty() {
        return Err(invalid_params("idempotency_key must not be empty"));
    }

    let mut seen = HashSet::new();
    let targets: Vec<_> = params
        .target_keys
        .into_iter()
        .filter(|key| !key.is_empty() && seen.insert(key.clone()))
        .collect();
    let limit = Arc::new(Semaphore::new(8));
    let mut tasks = Vec::new();
    for (index, session_key) in targets.into_iter().enumerate() {
        let pool = pool.clone();
        let text = params.text.clone();
        let idempotency_key = params.idempotency_key.clone();
        let limit = limit.clone();
        let events = events.clone();
        tasks.push(async move {
            let request_id = format!(
                "broadcast:{}",
                stable_fingerprint(&format!("{idempotency_key}\u{0}{session_key}"))
            );
            let fallback_request_id = request_id.clone();
            let fallback_session_key = session_key.clone();
            let fallback_idempotency_key = idempotency_key.clone();
            let _permit = match limit.acquire_owned().await {
                Ok(permit) => permit,
                Err(error) => {
                    let now = SystemClock.now_ms();
                    return (
                        index,
                        ainb_hangar_proto::fleet::FleetActionReceipt {
                            request_id: fallback_request_id,
                            session_key: fallback_session_key,
                            action_kind: "send_prompt".to_string(),
                            action_fingerprint: stable_fingerprint(&error.to_string()),
                            expected_version: 1,
                            idempotency_key: Some(fallback_idempotency_key),
                            status: ainb_hangar_proto::fleet::ActionReceiptStatus::Failed,
                            detail: Some(error.to_string()),
                            session_version: None,
                            created_at: now,
                            updated_at: now,
                        },
                    );
                }
            };
            let receipt =
                match ainb_hangar_store::repo::fleet::FleetRepo::get_session(&pool, &session_key)
                    .await
                {
                    Ok(Some(session)) => {
                        execute_fleet_action(
                            &pool,
                            ainb_hangar_proto::fleet::FleetActionParams {
                                session_key,
                                expected_version: session.version,
                                request_id,
                                action: ainb_hangar_proto::fleet::ControlAction::SendPrompt {
                                    text,
                                },
                            },
                            Some(idempotency_key),
                            &events,
                        )
                        .await
                    }
                    Ok(None) => {
                        rejected_broadcast_receipt(
                            &pool,
                            request_id,
                            session_key,
                            idempotency_key,
                            "session not found",
                        )
                        .await
                    }
                    Err(error) => Err(store_err(&error)),
                };
            let receipt = receipt.unwrap_or_else(|error| {
                let now = SystemClock.now_ms();
                ainb_hangar_proto::fleet::FleetActionReceipt {
                    request_id: fallback_request_id,
                    session_key: fallback_session_key,
                    action_kind: "send_prompt".to_string(),
                    action_fingerprint: stable_fingerprint(&error.message),
                    expected_version: 1,
                    idempotency_key: Some(fallback_idempotency_key),
                    status: ainb_hangar_proto::fleet::ActionReceiptStatus::Failed,
                    detail: Some(error.message),
                    session_version: None,
                    created_at: now,
                    updated_at: now,
                }
            });
            (index, receipt)
        });
    }

    let mut receipts = join_all(tasks).await;
    receipts.sort_by_key(|(index, _)| *index);
    to_value(&ainb_hangar_proto::fleet::FleetBroadcastResult {
        receipts: receipts.into_iter().map(|(_, receipt)| receipt).collect(),
    })
}

async fn rejected_broadcast_receipt(
    pool: &SqlitePool,
    request_id: String,
    session_key: String,
    idempotency_key: String,
    detail: &str,
) -> Result<ainb_hangar_proto::fleet::FleetActionReceipt, RpcError> {
    let now = SystemClock.now_ms();
    let row = ainb_hangar_store::repo::fleet::FleetRepo::upsert_action_receipt(
        pool,
        &ainb_hangar_store::repo::fleet::NewActionReceipt {
            request_id,
            session_key,
            action_kind: "send_prompt".to_string(),
            action_fingerprint: stable_fingerprint(detail),
            expected_version: 1,
            idempotency_key: Some(idempotency_key),
            status: "REJECTED".to_string(),
            detail: Some(detail.to_string()),
            session_version: None,
            created_at: now,
            updated_at: now,
        },
    )
    .await
    .map_err(fleet_repo_err)?;
    Ok(action_receipt_wire(&row))
}

async fn execute_fleet_action(
    pool: &SqlitePool,
    params: ainb_hangar_proto::fleet::FleetActionParams,
    idempotency_key: Option<String>,
    events: &EventSink,
) -> Result<ainb_hangar_proto::fleet::FleetActionReceipt, RpcError> {
    use ainb_hangar_proto::fleet::{ActionReceiptStatus, ControlAction};
    use ainb_hangar_store::repo::fleet::{FleetRepo, NewActionReceipt};

    if params.session_key.is_empty() || params.request_id.is_empty() {
        return Err(invalid_params(
            "session_key and request_id must not be empty",
        ));
    }
    if params.expected_version < 1 {
        return Err(invalid_params("expected_version must be positive"));
    }
    let action_json = serde_json::to_string(&params.action)
        .map_err(|error| internal(&format!("serialize action: {error}")))?;
    let action_fingerprint = stable_fingerprint(&action_json);

    if let Some(existing) = FleetRepo::get_action_receipt(pool, &params.request_id)
        .await
        .map_err(|error| store_err(&error))?
    {
        if existing.session_key != params.session_key
            || existing.action_kind != params.action.kind()
            || existing.action_fingerprint != action_fingerprint
            || existing.expected_version != params.expected_version
            || existing.idempotency_key != idempotency_key
        {
            return Err(invalid_params(
                "request_id was reused for a different Fleet action",
            ));
        }
        return Ok(action_receipt_wire(&existing));
    }

    if matches!(&params.action, ControlAction::Start { .. }) {
        return execute_fleet_start(pool, params, idempotency_key, action_fingerprint, events)
            .await;
    }

    let request_fingerprint = match &params.action {
        ControlAction::StructuredAnswer {
            request_fingerprint,
            ..
        }
        | ControlAction::Approve {
            request_fingerprint,
            ..
        }
        | ControlAction::Deny {
            request_fingerprint,
            ..
        }
        | ControlAction::VerifiedPicker {
            request_fingerprint,
            ..
        } => Some(request_fingerprint.as_str()),
        _ => None,
    };
    let session = FleetRepo::validate_action_target(
        pool,
        &params.session_key,
        params.expected_version,
        request_fingerprint,
    )
    .await
    .map_err(fleet_repo_err)?;
    let capabilities: ainb_hangar_proto::fleet::FleetCapabilities =
        serde_json::from_str(&session.capabilities).unwrap_or_default();

    let now = SystemClock.now_ms();
    let pending = NewActionReceipt {
        request_id: params.request_id.clone(),
        session_key: params.session_key.clone(),
        action_kind: params.action.kind().to_string(),
        action_fingerprint,
        expected_version: params.expected_version,
        idempotency_key,
        status: "PENDING".to_string(),
        detail: None,
        session_version: Some(session.version),
        created_at: now,
        updated_at: now,
    };
    let claimed = sqlx::query(
        "INSERT INTO fleet_action_receipt \
         (request_id, session_key, action_kind, action_fingerprint, expected_version, \
          idempotency_key, status, detail, session_version, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) ON CONFLICT(request_id) DO NOTHING",
    )
    .bind(&pending.request_id)
    .bind(&pending.session_key)
    .bind(&pending.action_kind)
    .bind(&pending.action_fingerprint)
    .bind(pending.expected_version)
    .bind(&pending.idempotency_key)
    .bind(&pending.status)
    .bind(&pending.detail)
    .bind(pending.session_version)
    .bind(pending.created_at)
    .bind(pending.updated_at)
    .execute(pool)
    .await
    .map_err(|error| store_err(&error))?
    .rows_affected()
        == 1;
    if !claimed {
        let existing = FleetRepo::get_action_receipt(pool, &params.request_id)
            .await
            .map_err(|error| store_err(&error))?
            .ok_or_else(|| internal("Fleet action receipt claim disappeared"))?;
        if existing.session_key != params.session_key
            || existing.action_kind != params.action.kind()
            || existing.action_fingerprint != pending.action_fingerprint
            || existing.expected_version != params.expected_version
            || existing.idempotency_key != pending.idempotency_key
        {
            return Err(invalid_params(
                "request_id was reused for a different Fleet action",
            ));
        }
        return Ok(action_receipt_wire(&existing));
    }

    let (status, detail) = if !action_capability(&capabilities, &params.action) {
        (
            ActionReceiptStatus::Rejected,
            Some("action unavailable for current session capabilities".to_string()),
        )
    } else if let ControlAction::VerifiedPicker {
        request_fingerprint,
        key,
    } = &params.action
    {
        verified_tmux_picker(
            pool,
            &session,
            params.expected_version,
            request_fingerprint,
            key,
        )
        .await
    } else {
        if session.provider == "codex" {
            match crate::fleet_provider::codex_manager::active_handle().await {
                Some(manager) => {
                    execute_codex_action(pool, events, &session, &params.action, &manager).await
                }
                None => match &params.action {
                    ControlAction::SendPrompt { text } => verified_tmux_send(&session, text).await,
                    _ => (
                        ActionReceiptStatus::Unknown,
                        Some("Codex managed transport is not active".to_string()),
                    ),
                },
            }
        } else {
            match &params.action {
                ControlAction::SendPrompt { text } if text.trim().is_empty() => (
                    ActionReceiptStatus::Rejected,
                    Some("prompt text must not be empty".to_string()),
                ),
                ControlAction::SendPrompt { text } => verified_tmux_send(&session, text).await,
                ControlAction::StructuredAnswer {
                    request_fingerprint,
                    answers,
                    ..
                } if session.provider == "claude" => {
                    execute_claude_structured(pool, &session, request_fingerprint, answers).await
                }
                ControlAction::Approve {
                    request_fingerprint,
                    ..
                }
                | ControlAction::Deny {
                    request_fingerprint,
                    ..
                } if session.provider == "claude" => {
                    let approve = matches!(&params.action, ControlAction::Approve { .. });
                    match claude_broker_decide(
                        session.provider_session_id.as_deref().unwrap_or_default(),
                        request_fingerprint,
                        approve,
                    )
                    .await
                    {
                        Ok(true) => (
                            ActionReceiptStatus::Delivered,
                            Some("claude blocking hook broker".to_string()),
                        ),
                        Ok(false) => (
                            ActionReceiptStatus::Failed,
                            Some("Claude request no longer waiting".to_string()),
                        ),
                        Err(error) => (ActionReceiptStatus::Failed, Some(error.to_string())),
                    }
                }
                ControlAction::StructuredAnswer { .. }
                | ControlAction::Approve { .. }
                | ControlAction::Deny { .. } => (
                    ActionReceiptStatus::Unknown,
                    Some("authoritative provider request transport is not active".to_string()),
                ),
                _ => (
                    ActionReceiptStatus::Unknown,
                    Some("authoritative provider lifecycle transport is not active".to_string()),
                ),
            }
        }
    };

    let mut completed = pending;
    completed.status = receipt_status_token(status).to_string();
    completed.detail = detail;
    completed.updated_at = SystemClock.now_ms();
    let row = FleetRepo::upsert_action_receipt(pool, &completed)
        .await
        .map_err(fleet_repo_err)?;
    Ok(action_receipt_wire(&row))
}

async fn execute_fleet_start(
    pool: &SqlitePool,
    params: ainb_hangar_proto::fleet::FleetActionParams,
    idempotency_key: Option<String>,
    action_fingerprint: String,
    events: &EventSink,
) -> Result<ainb_hangar_proto::fleet::FleetActionReceipt, RpcError> {
    use ainb_hangar_proto::fleet::{ActionReceiptStatus, ControlAction, FleetProvider};
    use ainb_hangar_store::repo::fleet::{FleetRepo, NewActionReceipt};

    let now = SystemClock.now_ms();
    let mut receipt = NewActionReceipt {
        request_id: params.request_id.clone(),
        session_key: params.session_key.clone(),
        action_kind: params.action.kind().to_string(),
        action_fingerprint,
        expected_version: params.expected_version,
        idempotency_key,
        status: "PENDING".to_string(),
        detail: None,
        session_version: None,
        created_at: now,
        updated_at: now,
    };
    let claimed = sqlx::query(
        "INSERT INTO fleet_action_receipt \
         (request_id, session_key, action_kind, action_fingerprint, expected_version, \
          idempotency_key, status, detail, session_version, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) ON CONFLICT(request_id) DO NOTHING",
    )
    .bind(&receipt.request_id)
    .bind(&receipt.session_key)
    .bind(&receipt.action_kind)
    .bind(&receipt.action_fingerprint)
    .bind(receipt.expected_version)
    .bind(&receipt.idempotency_key)
    .bind(&receipt.status)
    .bind(&receipt.detail)
    .bind(receipt.session_version)
    .bind(receipt.created_at)
    .bind(receipt.updated_at)
    .execute(pool)
    .await
    .map_err(|error| store_err(&error))?
    .rows_affected()
        == 1;
    if !claimed {
        let existing = FleetRepo::get_action_receipt(pool, &params.request_id)
            .await
            .map_err(|error| store_err(&error))?
            .ok_or_else(|| internal("Fleet start receipt claim disappeared"))?;
        if existing.session_key != params.session_key
            || existing.action_kind != params.action.kind()
            || existing.action_fingerprint != receipt.action_fingerprint
            || existing.expected_version != params.expected_version
            || existing.idempotency_key != receipt.idempotency_key
        {
            return Err(invalid_params(
                "request_id was reused for a different Fleet action",
            ));
        }
        return Ok(action_receipt_wire(&existing));
    }

    let (status, detail) = match &params.action {
        ControlAction::Start {
            provider: FleetProvider::Codex,
            cwd,
            prompt,
        } => match crate::fleet_provider::codex_manager::active_handle().await {
            Some(manager) => match manager.thread_start(Path::new(cwd), None).await {
                Ok(thread) => match launch_managed_codex_tui(&manager, &thread, cwd).await {
                    Ok((tmux_name, tmux_session)) => {
                        match crate::fleet::register_managed_codex_tmux(
                            pool,
                            events,
                            &thread,
                            cwd,
                            &tmux_session,
                            manager.capabilities(),
                            SystemClock.now_ms(),
                        )
                        .await
                        {
                            Ok(_) => {
                                let turn = match prompt
                                    .as_deref()
                                    .filter(|prompt| !prompt.trim().is_empty())
                                {
                                    Some(prompt) => {
                                        manager.turn_start(&thread, prompt).await.map(|_| ())
                                    }
                                    None => Ok(()),
                                };
                                match turn {
                                    Ok(()) => (
                                        ActionReceiptStatus::Delivered,
                                        Some(format!(
                                            "codex thread {thread}, tmux {}",
                                            tmux_session
                                                .exact_tmux_target
                                                .as_deref()
                                                .unwrap_or(&tmux_name)
                                        )),
                                    ),
                                    Err(error) => (
                                        ActionReceiptStatus::Failed,
                                        Some(format!(
                                            "Codex thread {thread} launched in tmux {tmux_name}, initial prompt failed: {error}"
                                        )),
                                    ),
                                }
                            }
                            Err(error) => {
                                let _ = kill_tmux_session_exact(&tmux_name).await;
                                (ActionReceiptStatus::Failed, Some(error.to_string()))
                            }
                        }
                    }
                    Err(error) => (ActionReceiptStatus::Failed, Some(error)),
                },
                Err(error) => (ActionReceiptStatus::Failed, Some(error.to_string())),
            },
            None => (
                ActionReceiptStatus::Unknown,
                Some("Codex managed transport is not active".to_string()),
            ),
        },
        ControlAction::Start { .. } => (
            ActionReceiptStatus::Rejected,
            Some("provider start transport is unavailable".to_string()),
        ),
        _ => unreachable!("start handler only receives start actions"),
    };
    receipt.status = receipt_status_token(status).to_string();
    receipt.detail = detail;
    receipt.updated_at = SystemClock.now_ms();
    let row = FleetRepo::upsert_action_receipt(pool, &receipt).await.map_err(fleet_repo_err)?;
    Ok(action_receipt_wire(&row))
}

async fn launch_managed_codex_tui(
    manager: &crate::fleet_provider::codex_manager::CodexManagerHandle,
    thread_id: &str,
    cwd: &str,
) -> Result<(String, ainb_fleet_core::types::FleetSession), String> {
    let tmux_name = managed_codex_tmux_name(thread_id, SystemClock.now_ms());
    let codex_binary = std::env::var_os("AINB_CODEX_BIN").unwrap_or_else(|| "codex".into());
    let tmux_binary = std::env::var_os("AINB_TMUX_BIN").unwrap_or_else(|| "tmux".into());
    let command = manager.managed_tui_command(
        &codex_binary,
        [
            std::ffi::OsString::from("resume"),
            std::ffi::OsString::from(thread_id),
        ],
    );
    let tmux_args = managed_codex_tmux_args(&tmux_name, cwd, &command);
    let output = tokio::process::Command::new(&tmux_binary)
        .args(tmux_args)
        .output()
        .await
        .map_err(|error| format!("tmux managed Codex launch failed: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "tmux managed Codex launch exited {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        match ainb_fleet_core::discover::discover_from_tmux().await {
            Ok(sessions) => {
                if let Some(session) = sessions.into_iter().find(|session| {
                    session
                        .exact_tmux_target
                        .as_deref()
                        .is_some_and(|target| target.starts_with(&format!("{tmux_name}:")))
                }) {
                    if session.process_start_fingerprint.is_some() {
                        return Ok((tmux_name, session));
                    }
                }
            }
            Err(error) => {
                let _ = kill_tmux_session_exact(&tmux_name).await;
                return Err(format!(
                    "managed Codex tmux identity lookup failed: {error}"
                ));
            }
        }
        if tokio::time::Instant::now() >= deadline {
            let _ = kill_tmux_session_exact(&tmux_name).await;
            return Err("managed Codex tmux identity lookup timed out".to_string());
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
}

fn managed_codex_tmux_args(
    session_name: &str,
    cwd: &str,
    command: &crate::fleet_provider::codex::CommandSpec,
) -> Vec<std::ffi::OsString> {
    let mut args = ["new-session", "-d", "-s", session_name, "-c", cwd, "--"]
        .into_iter()
        .map(std::ffi::OsString::from)
        .collect::<Vec<_>>();
    args.push(command.program.clone());
    args.extend(command.args.iter().cloned());
    args
}

fn managed_codex_tmux_name(thread_id: &str, now_ms: i64) -> String {
    let safe = thread_id
        .chars()
        .filter(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
        .take(24)
        .collect::<String>();
    let safe = if safe.is_empty() { "thread" } else { &safe };
    format!("fleet-codex-{safe}-{now_ms}")
}

async fn kill_tmux_session_exact(session_name: &str) -> Result<(), String> {
    let tmux_binary = std::env::var_os("AINB_TMUX_BIN").unwrap_or_else(|| "tmux".into());
    let output = tokio::process::Command::new(tmux_binary)
        .args(["kill-session", "-t", session_name])
        .output()
        .await
        .map_err(|error| format!("exact tmux stop failed: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "exact tmux stop exited {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(())
}

#[cfg(test)]
mod fleet_launch_tests {
    use super::{managed_codex_tmux_args, managed_codex_tmux_name, verify_picker_pane};
    use std::ffi::{OsStr, OsString};
    use std::path::Path;

    #[test]
    fn managed_codex_tmux_name_is_unique_and_shell_safe() {
        let first = managed_codex_tmux_name("thread/$ unsafe", 100);
        let second = managed_codex_tmux_name("thread/$ unsafe", 101);
        assert_eq!(first, "fleet-codex-threadunsafe-100");
        assert_ne!(first, second);
        assert!(
            first
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || character == '-')
        );
    }

    #[test]
    fn managed_codex_launch_runs_remote_tui_in_exact_tmux_session() {
        let command = crate::fleet_provider::codex::managed_tui_command(
            OsStr::new("codex"),
            Path::new("/tmp/codex.sock"),
            [OsString::from("resume"), OsString::from("thread-1")],
        );
        let args = managed_codex_tmux_args("fleet-codex-thread-1", "/repo", &command);
        assert_eq!(
            args,
            [
                "new-session",
                "-d",
                "-s",
                "fleet-codex-thread-1",
                "-c",
                "/repo",
                "--",
                "codex",
                "--remote",
                "unix:///tmp/codex.sock",
                "resume",
                "thread-1",
            ]
            .into_iter()
            .map(OsString::from)
            .collect::<Vec<_>>()
        );
    }

    fn picker_request() -> serde_json::Value {
        serde_json::json!({
            "payload": {
                "tool_input": {
                    "questions": [{
                        "question": "Deploy to which region?",
                        "options": [
                            {"label": "Europe", "description": "EU region"},
                            {"label": "United States", "description": "US region"}
                        ]
                    }]
                }
            }
        })
    }

    #[test]
    fn verified_picker_accepts_matching_prompt_and_ordered_options() {
        let pane = "Claude Code\n\
                    ╭──────────────────────────────╮\n\
                    │ Deploy to which region?      │\n\
                    │ 1. Europe                    │\n\
                    │ 2. United States             │\n\
                    ╰──────────────────────────────╯\n\
                    Press ? for help";
        assert_eq!(
            verify_picker_pane("claude", &picker_request(), pane),
            Ok(())
        );
    }

    #[test]
    fn verified_picker_rejects_prompt_mismatch() {
        let pane = "Claude Code\nChoose release channel\n1. Europe\n2. United States";
        let error = verify_picker_pane("claude", &picker_request(), pane).unwrap_err();
        assert!(error.contains("prompt"));
    }

    #[test]
    fn verified_picker_rejects_option_order_mismatch() {
        let pane = "Codex\nDeploy to which region?\n1. United States\n2. Europe";
        let error = verify_picker_pane("codex", &picker_request(), pane).unwrap_err();
        assert!(error.contains("option order"));
    }

    #[test]
    fn verified_picker_rejects_old_match_above_newer_picker() {
        let pane = "Claude Code\n\
                    Deploy to which region?\n\
                    1. Europe\n\
                    2. United States\n\
                    Choose release channel?\n\
                    1. Stable\n\
                    2. Preview";
        let error = verify_picker_pane("claude", &picker_request(), pane).unwrap_err();
        assert!(error.contains("newer picker"));
    }

    #[test]
    fn verified_picker_does_not_reanchor_to_newer_shared_final_label() {
        let request = serde_json::json!({
            "questions": [{
                "question": "Deploy now?",
                "options": ["Yes", "No"]
            }]
        });
        let pane = "Claude Code\n\
                    Deploy now?\n\
                    1. Yes\n\
                    2. No\n\
                    Delete deployment?\n\
                    1. Keep\n\
                    2. No";
        let error = verify_picker_pane("claude", &request, pane).unwrap_err();
        assert!(error.contains("newer picker"));
    }
}

async fn claude_broker_decide(
    session_id: &str,
    request_fingerprint: &str,
    approve: bool,
) -> std::io::Result<bool> {
    if session_id.is_empty() {
        return Ok(false);
    }
    let socket = approve_socket_path()?;
    let session_id = session_id.to_string();
    let request_fingerprint = request_fingerprint.to_string();
    tokio::task::spawn_blocking(move || {
        ainb_plugin_notifyd::broker::client_decide_exact(
            &socket,
            &session_id,
            Some(&request_fingerprint),
            if approve {
                ainb_plugin_notifyd::broker::DecisionKind::Approve
            } else {
                ainb_plugin_notifyd::broker::DecisionKind::Deny
            },
            "Fleet control plane",
        )
    })
    .await
    .map_err(std::io::Error::other)?
}

async fn execute_codex_action(
    pool: &SqlitePool,
    events: &EventSink,
    session: &ainb_hangar_store::repo::fleet::FleetSessionRow,
    action: &ainb_hangar_proto::fleet::ControlAction,
    manager: &crate::fleet_provider::codex_manager::CodexManagerHandle,
) -> (
    ainb_hangar_proto::fleet::ActionReceiptStatus,
    Option<String>,
) {
    use crate::fleet_provider::{ApprovalDecision, QuestionAnswer};
    use ainb_hangar_proto::fleet::{ActionReceiptStatus, ControlAction, FleetProvider};

    let thread_id = session.provider_session_id.as_deref().unwrap_or_default();
    let result: Result<String, crate::fleet_provider::ProviderError> = async {
        match action {
            ControlAction::StructuredAnswer {
                request_identity,
                answers,
                ..
            } => {
                let request =
                    match crate::fleet::current_request_wire(pool, &session.session_key).await {
                        Ok(Some(value)) => serde_json::from_value::<
                            crate::fleet_provider::codex::CodexQuestionRequest,
                        >(value)
                        .map_err(crate::fleet_provider::ProviderError::from),
                        Ok(None) => Err(crate::fleet_provider::ProviderError::Stale(
                            "current Codex question is absent".to_string(),
                        )),
                        Err(error) => Err(crate::fleet_provider::ProviderError::Transport(
                            error.to_string(),
                        )),
                    }?;
                require_codex_identity(request_identity.as_ref(), &request.identity)?;
                let answers = answers
                    .iter()
                    .map(|answer| {
                        let mut values = answer.selected_options.clone();
                        if let Some(text) = answer.text.as_deref().filter(|text| !text.is_empty()) {
                            values.push(text.to_string());
                        }
                        QuestionAnswer {
                            question_id: answer.question_id.clone(),
                            answers: values,
                        }
                    })
                    .collect::<Vec<_>>();
                manager
                    .answer_request_user_input(&request, &answers)
                    .await
                    .map(|receipt| receipt.transport.to_string())
            }
            ControlAction::Approve {
                request_identity, ..
            }
            | ControlAction::Deny {
                request_identity, ..
            } => {
                let request = load_codex_approval(pool, &session.session_key).await?;
                require_codex_identity(request_identity.as_ref(), &request.identity)?;
                let decision = if matches!(action, ControlAction::Approve { .. }) {
                    ApprovalDecision::Approve
                } else {
                    ApprovalDecision::Deny
                };
                manager
                    .decide_approval(&request, decision)
                    .await
                    .map(|receipt| receipt.transport.to_string())
            }
            ControlAction::SendPrompt { text } => {
                manager.thread_read(thread_id).await?;
                manager
                    .turn_start(thread_id, text)
                    .await
                    .map(|turn| format!("codex turn {turn}"))
            }
            ControlAction::Continue => {
                manager.thread_read(thread_id).await?;
                manager
                    .turn_start(thread_id, "continue")
                    .await
                    .map(|turn| format!("codex turn {turn}"))
            }
            ControlAction::Retry => {
                manager.thread_read(thread_id).await?;
                manager
                    .turn_start(thread_id, "retry")
                    .await
                    .map(|turn| format!("codex turn {turn}"))
            }
            ControlAction::Interrupt => {
                manager.thread_read(thread_id).await?;
                let turn_id = latest_codex_turn_id(pool, &session.session_key)
                    .await
                    .map_err(|error| {
                        crate::fleet_provider::ProviderError::Transport(error.to_string())
                    })?
                    .ok_or_else(|| {
                        crate::fleet_provider::ProviderError::Stale(
                            "active Codex turn identity is absent".to_string(),
                        )
                    })?;
                manager
                    .turn_interrupt(thread_id, &turn_id)
                    .await
                    .map(|_| format!("codex turn {turn_id} interrupted"))
            }
            ControlAction::Stop => {
                manager.thread_read(thread_id).await?;
                let tmux_name = exact_live_tmux_session_name(session).await?;
                if session.lifecycle_state == "RUNNING" {
                    let turn_id = latest_codex_turn_id(pool, &session.session_key)
                        .await
                        .map_err(|error| {
                            crate::fleet_provider::ProviderError::Transport(error.to_string())
                        })?
                        .ok_or_else(|| {
                            crate::fleet_provider::ProviderError::Stale(
                                "active Codex turn identity is absent".to_string(),
                            )
                        })?;
                    manager.turn_interrupt(thread_id, &turn_id).await?;
                }
                kill_tmux_session_exact(&tmux_name)
                    .await
                    .map_err(crate::fleet_provider::ProviderError::Transport)?;
                persist_codex_exit(pool, events, session, manager, "codex_stopped").await?;
                Ok(format!(
                    "codex thread {thread_id} stopped in tmux {tmux_name}"
                ))
            }
            ControlAction::Restart => {
                manager.thread_read(thread_id).await?;
                let tmux_name = exact_live_tmux_session_name(session).await?;
                kill_tmux_session_exact(&tmux_name)
                    .await
                    .map_err(crate::fleet_provider::ProviderError::Transport)?;
                let (new_tmux_name, tmux_session) =
                    match launch_managed_codex_tui(manager, thread_id, &session.cwd).await {
                        Ok(launched) => launched,
                        Err(error) => {
                            persist_codex_exit(
                                pool,
                                events,
                                session,
                                manager,
                                "codex_restart_failed",
                            )
                            .await?;
                            return Err(crate::fleet_provider::ProviderError::Transport(error));
                        }
                    };
                if let Err(error) = crate::fleet::register_managed_codex_tmux(
                    pool,
                    events,
                    thread_id,
                    &session.cwd,
                    &tmux_session,
                    manager.capabilities(),
                    SystemClock.now_ms(),
                )
                .await
                {
                    let _ = kill_tmux_session_exact(&new_tmux_name).await;
                    persist_codex_exit(pool, events, session, manager, "codex_restart_failed")
                        .await?;
                    return Err(crate::fleet_provider::ProviderError::Transport(
                        error.to_string(),
                    ));
                }
                Ok(format!(
                    "codex thread {thread_id} restarted from tmux {tmux_name} into {new_tmux_name}"
                ))
            }
            ControlAction::Kill => {
                manager.thread_read(thread_id).await?;
                let tmux_name = exact_live_tmux_session_name(session).await?;
                if session.lifecycle_state == "RUNNING" {
                    let turn_id = latest_codex_turn_id(pool, &session.session_key)
                        .await
                        .map_err(|error| {
                            crate::fleet_provider::ProviderError::Transport(error.to_string())
                        })?
                        .ok_or_else(|| {
                            crate::fleet_provider::ProviderError::Stale(
                                "active Codex turn identity is absent".to_string(),
                            )
                        })?;
                    manager.turn_interrupt(thread_id, &turn_id).await?;
                }
                kill_tmux_session_exact(&tmux_name)
                    .await
                    .map_err(crate::fleet_provider::ProviderError::Transport)?;
                persist_codex_exit(pool, events, session, manager, "codex_killed").await?;
                Ok(format!(
                    "codex thread {thread_id} killed in tmux {tmux_name}"
                ))
            }
            ControlAction::Archive => {
                manager.thread_read(thread_id).await?;
                let tmux_name = exact_live_tmux_session_name(session).await?;
                kill_tmux_session_exact(&tmux_name)
                    .await
                    .map_err(crate::fleet_provider::ProviderError::Transport)?;
                if let Err(error) = manager.thread_archive(thread_id).await {
                    persist_codex_exit(pool, events, session, manager, "codex_archive_failed")
                        .await?;
                    return Err(error);
                }
                persist_codex_exit(pool, events, session, manager, "codex_archived").await?;
                Ok(format!(
                    "codex thread {thread_id} archived after tmux {tmux_name} stop"
                ))
            }
            ControlAction::Start {
                provider,
                cwd,
                prompt,
            } if *provider == FleetProvider::Codex => {
                let thread = manager.thread_start(Path::new(cwd), None).await?;
                if let Some(prompt) = prompt.as_deref().filter(|prompt| !prompt.trim().is_empty()) {
                    manager.turn_start(&thread, prompt).await?;
                }
                Ok(format!("codex thread {thread}"))
            }
            _ => Err(crate::fleet_provider::ProviderError::Unsupported(
                "Codex action is not available through app-server".to_string(),
            )),
        }
    }
    .await;

    match result {
        Ok(detail) => (ActionReceiptStatus::Delivered, Some(detail)),
        Err(error) => (ActionReceiptStatus::Failed, Some(error.to_string())),
    }
}

async fn exact_live_tmux_session_name(
    session: &ainb_hangar_store::repo::fleet::FleetSessionRow,
) -> Result<String, crate::fleet_provider::ProviderError> {
    if session.management_state != "MANAGED" {
        return Err(crate::fleet_provider::ProviderError::Stale(
            "managed Codex identity is required".to_string(),
        ));
    }
    let target = session.tmux_target.as_deref().ok_or_else(|| {
        crate::fleet_provider::ProviderError::Stale("exact tmux target is unavailable".to_string())
    })?;
    let fingerprint = session.process_start_fingerprint.as_deref().ok_or_else(|| {
        crate::fleet_provider::ProviderError::Stale(
            "exact tmux process identity is unavailable".to_string(),
        )
    })?;
    let discovered = ainb_fleet_core::discover::discover_from_tmux()
        .await
        .map_err(|error| crate::fleet_provider::ProviderError::Transport(error.to_string()))?;
    if !discovered.iter().any(|candidate| {
        candidate.exact_tmux_target.as_deref() == Some(target)
            && candidate.process_start_fingerprint.as_deref() == Some(fingerprint)
    }) {
        return Err(crate::fleet_provider::ProviderError::Stale(
            "tmux process identity changed".to_string(),
        ));
    }
    target
        .split_once(':')
        .map(|(session_name, _)| session_name)
        .filter(|session_name| !session_name.is_empty())
        .map(str::to_string)
        .ok_or_else(|| {
            crate::fleet_provider::ProviderError::Protocol(
                "exact tmux target has no session name".to_string(),
            )
        })
}

async fn persist_codex_exit(
    pool: &SqlitePool,
    events: &EventSink,
    session: &ainb_hangar_store::repo::fleet::FleetSessionRow,
    manager: &crate::fleet_provider::codex_manager::CodexManagerHandle,
    event_type: &str,
) -> Result<(), crate::fleet_provider::ProviderError> {
    crate::fleet::mark_managed_codex_exited(
        pool,
        events,
        &session.session_key,
        event_type,
        manager.capabilities(),
        SystemClock.now_ms(),
    )
    .await
    .map(|_| ())
    .map_err(|error| crate::fleet_provider::ProviderError::Transport(error.to_string()))
}

fn require_codex_identity(
    supplied: Option<&ainb_hangar_proto::fleet::FleetRequestIdentity>,
    canonical: &crate::fleet_provider::codex::CodexItemRequestIdentity,
) -> Result<(), crate::fleet_provider::ProviderError> {
    let supplied = supplied.ok_or_else(|| {
        crate::fleet_provider::ProviderError::Stale(
            "exact Codex request identity is required".to_string(),
        )
    })?;
    if supplied.request_id != *canonical.request_id.as_value()
        || supplied.thread_id != canonical.thread_id
        || supplied.turn_id != canonical.turn_id
        || supplied.item_id != canonical.item_id
    {
        return Err(crate::fleet_provider::ProviderError::Stale(
            "Codex request identity changed".to_string(),
        ));
    }
    Ok(())
}

async fn load_codex_approval(
    pool: &SqlitePool,
    session_key: &str,
) -> Result<crate::fleet_provider::codex::CodexApprovalRequest, crate::fleet_provider::ProviderError>
{
    use crate::fleet_provider::codex::{
        CodexApprovalKind, CodexApprovalRequest, CodexItemRequestIdentity, RpcRequestId,
    };
    let value = crate::fleet::current_request_wire(pool, session_key)
        .await
        .map_err(|error| crate::fleet_provider::ProviderError::Transport(error.to_string()))?
        .ok_or_else(|| {
            crate::fleet_provider::ProviderError::Stale(
                "current Codex approval is absent".to_string(),
            )
        })?;
    let identity = value.get("identity").ok_or_else(|| {
        crate::fleet_provider::ProviderError::Protocol(
            "stored Codex approval identity is absent".to_string(),
        )
    })?;
    let required = |field: &str| {
        identity
            .get(field)
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| {
                crate::fleet_provider::ProviderError::Protocol(format!(
                    "stored Codex approval {field} is absent"
                ))
            })
    };
    let kind = match value.get("kind").and_then(serde_json::Value::as_str) {
        Some("commandExecution") => CodexApprovalKind::CommandExecution,
        Some("fileChange") => CodexApprovalKind::FileChange,
        Some("permissions") => CodexApprovalKind::Permissions,
        _ => {
            return Err(crate::fleet_provider::ProviderError::Protocol(
                "stored Codex approval kind is invalid".to_string(),
            ));
        }
    };
    Ok(CodexApprovalRequest {
        identity: CodexItemRequestIdentity {
            request_id: RpcRequestId::new(
                identity.get("requestId").cloned().unwrap_or(serde_json::Value::Null),
            )?,
            thread_id: required("threadId")?,
            turn_id: required("turnId")?,
            item_id: required("itemId")?,
        },
        kind,
        params: value.get("params").cloned().unwrap_or(serde_json::Value::Null),
    })
}

async fn latest_codex_turn_id(
    pool: &SqlitePool,
    session_key: &str,
) -> Result<Option<String>, sqlx::Error> {
    let payloads = sqlx::query_scalar::<_, String>(
        "SELECT payload FROM fleet_event WHERE session_key = ? AND applied = 1 \
         ORDER BY revision DESC LIMIT 32",
    )
    .bind(session_key)
    .fetch_all(pool)
    .await?;
    Ok(payloads.into_iter().find_map(|payload| {
        let value: serde_json::Value = serde_json::from_str(&payload).ok()?;
        value
            .get("turnId")
            .or_else(|| value.get("turn_id"))
            .or_else(|| value.pointer("/identity/turnId"))
            .or_else(|| value.pointer("/turn/id"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
    }))
}

async fn execute_claude_structured(
    pool: &SqlitePool,
    session: &ainb_hangar_store::repo::fleet::FleetSessionRow,
    request_fingerprint: &str,
    answers: &[ainb_hangar_proto::fleet::FleetQuestionAnswer],
) -> (
    ainb_hangar_proto::fleet::ActionReceiptStatus,
    Option<String>,
) {
    use ainb_hangar_proto::fleet::ActionReceiptStatus;
    let request = match crate::fleet::current_request_wire(pool, &session.session_key).await {
        Ok(Some(request)) => request,
        Ok(None) => {
            return (
                ActionReceiptStatus::Failed,
                Some("current Claude question is absent".to_string()),
            );
        }
        Err(error) => return (ActionReceiptStatus::Failed, Some(error.to_string())),
    };
    let hook = request.get("payload").unwrap_or(&request);
    let input = hook.get("tool_input").or_else(|| hook.get("input")).unwrap_or(hook);
    let Some(questions) = input.get("questions").and_then(serde_json::Value::as_array) else {
        return (
            ActionReceiptStatus::Failed,
            Some("stored Claude question payload is invalid".to_string()),
        );
    };
    let mut mapped = Vec::with_capacity(answers.len());
    for answer in answers {
        let question = questions.iter().enumerate().find_map(|(index, question)| {
            let id = question
                .get("id")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| index.to_string());
            (id == answer.question_id).then_some(question)
        });
        let Some(question) = question else {
            return (
                ActionReceiptStatus::Failed,
                Some(format!(
                    "Claude question id {} is stale",
                    answer.question_id
                )),
            );
        };
        let Some(question_text) = question.get("question").and_then(serde_json::Value::as_str)
        else {
            return (
                ActionReceiptStatus::Failed,
                Some("Claude question text is absent".to_string()),
            );
        };
        let mut values = answer.selected_options.clone();
        if let Some(text) = answer.text.as_deref().filter(|text| !text.is_empty()) {
            values.push(text.to_string());
        }
        mapped.push(ainb_plugin_notifyd::broker::StructuredQuestionAnswer {
            question: question_text.to_string(),
            selected_options: values,
        });
    }
    let session_id = session.provider_session_id.clone().unwrap_or_default();
    let fingerprint = request_fingerprint.to_string();
    let socket = match approve_socket_path() {
        Ok(socket) => socket,
        Err(error) => return (ActionReceiptStatus::Failed, Some(error.to_string())),
    };
    match tokio::task::spawn_blocking(move || {
        ainb_plugin_notifyd::broker::client_answer_structured(
            &socket,
            &session_id,
            &fingerprint,
            &mapped,
        )
    })
    .await
    {
        Ok(Ok(ack)) if ack.matched => (
            ActionReceiptStatus::Delivered,
            Some("claude structured hook broker".to_string()),
        ),
        Ok(Ok(ack)) if ack.stale => (
            ActionReceiptStatus::Failed,
            Some("Claude structured request is stale".to_string()),
        ),
        Ok(Ok(ack)) => (
            ActionReceiptStatus::Failed,
            ack.error.or_else(|| Some("Claude request no longer waiting".to_string())),
        ),
        Ok(Err(error)) => (ActionReceiptStatus::Failed, Some(error.to_string())),
        Err(error) => (ActionReceiptStatus::Failed, Some(error.to_string())),
    }
}

fn approve_socket_path() -> std::io::Result<PathBuf> {
    #[cfg(any(test, feature = "test-support"))]
    if let Some(path) = APPROVE_SOCKET_OVERRIDE
        .get_or_init(|| RwLock::new(None))
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
    {
        return Ok(path);
    }
    std::env::var_os("AINB_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".agents-in-a-box")))
        .map(|base| base.join("approve.sock"))
        .ok_or_else(|| std::io::Error::other("cannot resolve approve socket"))
}

async fn verified_tmux_send(
    session: &ainb_hangar_store::repo::fleet::FleetSessionRow,
    text: &str,
) -> (
    ainb_hangar_proto::fleet::ActionReceiptStatus,
    Option<String>,
) {
    use ainb_hangar_proto::fleet::ActionReceiptStatus;
    let (Some(target), Some(fingerprint)) = (
        session.tmux_target.as_deref(),
        session.process_start_fingerprint.as_deref(),
    ) else {
        return (
            ActionReceiptStatus::Unknown,
            Some("exact tmux process identity is unavailable".to_string()),
        );
    };
    let discovered = match ainb_fleet_core::discover::discover_from_tmux().await {
        Ok(discovered) => discovered,
        Err(error) => return (ActionReceiptStatus::Failed, Some(error.to_string())),
    };
    let live = discovered.iter().any(|candidate| {
        candidate.exact_tmux_target.as_deref() == Some(target)
            && candidate.process_start_fingerprint.as_deref() == Some(fingerprint)
    });
    if !live {
        return (
            ActionReceiptStatus::Failed,
            Some("tmux process identity changed".to_string()),
        );
    }
    match ainb_fleet_core::send::tmux_send(target, text).await {
        Ok(()) => (
            ActionReceiptStatus::Delivered,
            Some(format!("tmux ({target})")),
        ),
        Err(error) => (ActionReceiptStatus::Failed, Some(error.to_string())),
    }
}

async fn verified_tmux_picker(
    pool: &SqlitePool,
    session: &ainb_hangar_store::repo::fleet::FleetSessionRow,
    expected_version: i64,
    request_fingerprint: &str,
    key: &str,
) -> (
    ainb_hangar_proto::fleet::ActionReceiptStatus,
    Option<String>,
) {
    use ainb_hangar_proto::fleet::ActionReceiptStatus;
    let (Some(target), Some(fingerprint)) = (
        session.tmux_target.as_deref(),
        session.process_start_fingerprint.as_deref(),
    ) else {
        return (
            ActionReceiptStatus::Unknown,
            Some("exact tmux process identity is unavailable".to_string()),
        );
    };
    let discovered = match ainb_fleet_core::discover::discover_from_tmux().await {
        Ok(discovered) => discovered,
        Err(error) => return (ActionReceiptStatus::Failed, Some(error.to_string())),
    };
    let live = discovered.iter().any(|candidate| {
        candidate.exact_tmux_target.as_deref() == Some(target)
            && candidate.process_start_fingerprint.as_deref() == Some(fingerprint)
            && candidate.provider.as_str() == session.provider
    });
    if !live {
        return (
            ActionReceiptStatus::Failed,
            Some("tmux provider or process identity changed".to_string()),
        );
    }
    let request = match crate::fleet::current_request_wire(pool, &session.session_key).await {
        Ok(Some(request)) => request,
        Ok(None) => {
            return (
                ActionReceiptStatus::Failed,
                Some("current structured picker request is absent".to_string()),
            );
        }
        Err(error) => return (ActionReceiptStatus::Failed, Some(error.to_string())),
    };
    let pane = match ainb_fleet_core::read::capture_pane(target, 0).await {
        Ok(pane) => pane,
        Err(error) => return (ActionReceiptStatus::Failed, Some(error.to_string())),
    };
    if let Err(error) = verify_picker_pane(&session.provider, &request, &pane) {
        return (ActionReceiptStatus::Failed, Some(error));
    }
    let refreshed = match ainb_hangar_store::repo::fleet::FleetRepo::validate_action_target(
        pool,
        &session.session_key,
        expected_version,
        Some(request_fingerprint),
    )
    .await
    {
        Ok(refreshed) => refreshed,
        Err(error) => return (ActionReceiptStatus::Failed, Some(error.to_string())),
    };
    if refreshed.provider != session.provider
        || refreshed.tmux_target != session.tmux_target
        || refreshed.process_start_fingerprint != session.process_start_fingerprint
    {
        return (
            ActionReceiptStatus::Failed,
            Some("tmux picker identity changed during verification".to_string()),
        );
    }
    match ainb_fleet_core::send::tmux_send_picker_key(target, key).await {
        Ok(()) => (
            ActionReceiptStatus::Delivered,
            Some(format!("verified tmux picker ({target})")),
        ),
        Err(error) => (ActionReceiptStatus::Failed, Some(error.to_string())),
    }
}

#[derive(Debug, PartialEq, Eq)]
struct PickerQuestionEvidence {
    prompt: String,
    option_labels: Vec<String>,
}

const MAX_ACTIVE_PICKER_TRAILING_CHARS: usize = 512;

fn verify_picker_pane(
    provider: &str,
    request: &serde_json::Value,
    pane: &str,
) -> Result<(), String> {
    let questions = picker_question_evidence(provider, request)?;
    let visible = normalize_picker_text(pane);
    let mut cursor = 0;
    for (question_index, question) in questions.into_iter().enumerate() {
        let prompt = normalize_picker_text(&question.prompt);
        let prompt_offset = if question_index == 0 {
            visible[cursor..].rfind(&prompt)
        } else {
            visible[cursor..].find(&prompt)
        }
        .ok_or_else(|| "visible picker prompt does not match current request".to_string())?;
        cursor += prompt_offset + prompt.len();
        for label in question.option_labels {
            let label = normalize_picker_text(&label);
            let label_offset = visible[cursor..].find(&label).ok_or_else(|| {
                "visible picker option order does not match current request".to_string()
            })?;
            cursor += label_offset + label.len();
        }
    }
    let trailing = &visible[cursor..];
    if has_later_picker_candidate(pane, cursor) {
        return Err("visible picker is stale because a newer picker follows it".to_string());
    }
    if trailing.chars().count() > MAX_ACTIVE_PICKER_TRAILING_CHARS {
        return Err("visible picker is not active at terminal input".to_string());
    }
    Ok(())
}

fn has_later_picker_candidate(pane: &str, matched_end: usize) -> bool {
    let lines = pane
        .lines()
        .map(normalize_picker_text)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    let mut offset = 0;
    let mut anchor = None;
    for (index, line) in lines.iter().enumerate() {
        if index > 0 {
            offset += 1;
        }
        let line_end = offset + line.len();
        if matched_end <= line_end {
            anchor = Some(index);
            break;
        }
        offset = line_end;
    }
    let Some(anchor) = anchor else {
        return true;
    };
    lines[anchor + 1..].iter().any(|line| {
        line.trim_end().ends_with('?') || line.split_whitespace().any(is_numbered_picker_token)
    })
}

fn is_numbered_picker_token(token: &str) -> bool {
    let token = token.trim_start_matches(['>', '›', '❯', '○', '●', '◉']);
    let digits = token.chars().take_while(char::is_ascii_digit).count();
    digits > 0 && matches!(&token[digits..], "." | ")")
}

fn picker_question_evidence(
    provider: &str,
    request: &serde_json::Value,
) -> Result<Vec<PickerQuestionEvidence>, String> {
    if !matches!(provider, "claude" | "codex") {
        return Err("verified picker provider is unsupported".to_string());
    }
    let hook = request.get("payload").unwrap_or(request);
    let input = hook.get("tool_input").or_else(|| hook.get("input")).unwrap_or(hook);
    let questions = input
        .get("questions")
        .and_then(serde_json::Value::as_array)
        .filter(|questions| !questions.is_empty())
        .ok_or_else(|| "stored picker request has no structured questions".to_string())?;
    questions
        .iter()
        .map(|question| {
            let prompt = question
                .get("question")
                .and_then(serde_json::Value::as_str)
                .filter(|prompt| !prompt.trim().is_empty())
                .ok_or_else(|| "stored picker question prompt is absent".to_string())?;
            let options = question
                .get("options")
                .and_then(serde_json::Value::as_array)
                .filter(|options| !options.is_empty())
                .ok_or_else(|| "stored picker question has no ordered options".to_string())?;
            let option_labels = options
                .iter()
                .map(|option| {
                    option
                        .as_str()
                        .or_else(|| option.get("label").and_then(serde_json::Value::as_str))
                        .filter(|label| !label.trim().is_empty())
                        .map(str::to_string)
                        .ok_or_else(|| "stored picker option label is absent".to_string())
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(PickerQuestionEvidence {
                prompt: prompt.to_string(),
                option_labels,
            })
        })
        .collect()
}

fn normalize_picker_text(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_whitespace()
                || matches!(
                    character,
                    '│' | '─'
                        | '┌'
                        | '┐'
                        | '└'
                        | '┘'
                        | '├'
                        | '┤'
                        | '┬'
                        | '┴'
                        | '┼'
                        | '╭'
                        | '╮'
                        | '╯'
                        | '╰'
                )
            {
                ' '
            } else {
                character
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn action_capability(
    capabilities: &ainb_hangar_proto::fleet::FleetCapabilities,
    action: &ainb_hangar_proto::fleet::ControlAction,
) -> bool {
    use ainb_hangar_proto::fleet::ControlAction;
    match action {
        ControlAction::StructuredAnswer { .. } => capabilities.structured_answer,
        ControlAction::Approve { .. } | ControlAction::Deny { .. } => capabilities.approvals,
        ControlAction::VerifiedPicker { .. } => capabilities.verified_picker,
        ControlAction::SendPrompt { .. } => capabilities.send_prompt || capabilities.tmux_text,
        ControlAction::Continue => capabilities.continue_turn,
        ControlAction::Retry => capabilities.retry,
        ControlAction::Interrupt => capabilities.interrupt,
        ControlAction::Start { .. } => capabilities.start,
        ControlAction::Restart => capabilities.restart,
        ControlAction::Stop => capabilities.stop,
        ControlAction::Kill => capabilities.kill,
        ControlAction::Archive => capabilities.archive,
    }
}

fn stable_fingerprint(value: &str) -> String {
    let hash = value.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
    });
    format!("fnv1a64:{hash:016x}")
}

const fn receipt_status_token(
    status: ainb_hangar_proto::fleet::ActionReceiptStatus,
) -> &'static str {
    use ainb_hangar_proto::fleet::ActionReceiptStatus;
    match status {
        ActionReceiptStatus::Pending => "PENDING",
        ActionReceiptStatus::Delivered => "DELIVERED",
        ActionReceiptStatus::Failed => "FAILED",
        ActionReceiptStatus::Unknown => "UNKNOWN",
        ActionReceiptStatus::Rejected => "REJECTED",
    }
}

fn action_receipt_wire(
    row: &ainb_hangar_store::repo::fleet::ActionReceiptRow,
) -> ainb_hangar_proto::fleet::FleetActionReceipt {
    use ainb_hangar_proto::fleet::ActionReceiptStatus;
    ainb_hangar_proto::fleet::FleetActionReceipt {
        request_id: row.request_id.clone(),
        session_key: row.session_key.clone(),
        action_kind: row.action_kind.clone(),
        action_fingerprint: row.action_fingerprint.clone(),
        expected_version: row.expected_version,
        idempotency_key: row.idempotency_key.clone(),
        status: match row.status.as_str() {
            "PENDING" => ActionReceiptStatus::Pending,
            "DELIVERED" => ActionReceiptStatus::Delivered,
            "FAILED" => ActionReceiptStatus::Failed,
            "REJECTED" => ActionReceiptStatus::Rejected,
            _ => ActionReceiptStatus::Unknown,
        },
        detail: row.detail.clone(),
        session_version: row.session_version,
        created_at: row.created_at,
        updated_at: row.updated_at,
    }
}

fn fleet_repo_err(error: ainb_hangar_store::repo::fleet::FleetRepoError) -> RpcError {
    use ainb_hangar_store::repo::fleet::FleetRepoError;
    match error {
        FleetRepoError::Sql(error) => store_err(&error),
        FleetRepoError::SessionNotFound { .. }
        | FleetRepoError::StaleVersion { .. }
        | FleetRepoError::RequestFingerprintMismatch { .. }
        | FleetRepoError::ReceiptCollision { .. }
        | FleetRepoError::EventIdCollision { .. } => invalid_params(&error.to_string()),
    }
}

/// `profile/list` (P5): the indexed agent profiles, slug-ordered. A read over the
/// fs-watch-maintained index — the body always lives on disk.
async fn handle_profile_list(pool: &SqlitePool) -> Result<serde_json::Value, RpcError> {
    use ainb_hangar_store::repo::profile::ProfileRepo;
    let rows = ProfileRepo::list(pool)
        .await
        .map_err(|e| internal(&format!("profile list: {e}")))?;
    let profiles = rows
        .into_iter()
        .map(|r| ainb_hangar_proto::snapshots::ProfileRow {
            slug: r.slug,
            tier: r.tier,
            mtime: r.mtime,
        })
        .collect();
    to_value(&ainb_hangar_proto::snapshots::ProfileListResult { profiles })
}

/// `profile/get` (P5): one master's parsed fields + both compile previews, read
/// from disk (the source of truth). An unknown slug returns the not-found result
/// (a read miss, not an error).
async fn handle_profile_get(
    pool: &SqlitePool,
    req: &RpcRequest,
) -> Result<serde_json::Value, RpcError> {
    let params: ainb_hangar_proto::snapshots::ProfileGetParams = parse_params(req, "{ slug }")?;
    // Containment guard: the slug is joined into `<profiles>/<slug>.md`, so an
    // unvalidated `../` would escape the profiles dir and read any parseable
    // `.md` on disk. Mirror the `profile/upsert` guard — an invalid slug is a
    // read miss (not-found), never a traversal. read_master hardens this too.
    if !ainb_hangar_core::profile::is_valid_slug(&params.slug) {
        return to_value(&ainb_hangar_proto::snapshots::ProfileGetResult::not_found());
    }
    // Best-effort: keep the index current so a get after an out-of-band edit
    // reflects disk (the RPC path does not depend on the watcher being alive).
    let dir = profiles_dir_or_err()?;
    let master = match crate::profile::read_master(&dir, &params.slug) {
        Ok(Some(m)) => m,
        Ok(None) => {
            return to_value(&ainb_hangar_proto::snapshots::ProfileGetResult::not_found());
        }
        Err(e) => return Err(internal(&format!("profile read: {e}"))),
    };
    let _ = pool; // index unaffected by a get; the arg keeps the handler uniform.
    to_value(&profile_get_result(&master))
}

/// `profile/upsert` (P5): write the canonical master to disk and refresh the DB
/// index. Rejects an invalid slug / tier with `INVALID_PARAMS`.
async fn handle_profile_upsert(
    pool: &SqlitePool,
    req: &RpcRequest,
) -> Result<serde_json::Value, RpcError> {
    use ainb_hangar_core::profile::{ModelTier, ProfileMaster, is_valid_slug};
    let params: ainb_hangar_proto::snapshots::ProfileUpsertParams =
        parse_params(req, "{ slug, description, tier, tools, color, body }")?;

    if !is_valid_slug(&params.slug) {
        return Err(invalid_params(&format!(
            "invalid profile slug {:?}: must be kebab-case ([a-z0-9-])",
            params.slug
        )));
    }
    let tier = ModelTier::parse(&params.tier).ok_or_else(|| {
        invalid_params(&format!(
            "unknown model tier {:?}: expected premium | balanced | fast",
            params.tier
        ))
    })?;

    let master = ProfileMaster {
        slug: params.slug.clone(),
        description: params.description,
        tier,
        tools: params.tools,
        color: if params.color.is_empty() {
            None
        } else {
            Some(params.color)
        },
        body: params.body,
    };

    let dir = profiles_dir_or_err()?;
    let path = crate::profile::write_master(&dir, &master)
        .map_err(|e| internal(&format!("profile write: {e}")))?;
    // Refresh the index directly so `profile/list` reflects the write immediately
    // (the fs-watch would also catch it; the two converge on the same row).
    crate::profile::refresh_index(pool, &dir)
        .await
        .map_err(|e| internal(&format!("profile index refresh: {e}")))?;

    to_value(&ainb_hangar_proto::snapshots::ProfileUpsertResult {
        slug: master.slug,
        path: path.to_string_lossy().into_owned(),
    })
}

/// Resolve the profiles directory or fail with an internal error (the Hangar home
/// could not be resolved — a daemon-environment fault, not a client one).
fn profiles_dir_or_err() -> Result<PathBuf, RpcError> {
    crate::profile::profiles_dir()
        .ok_or_else(|| internal("cannot resolve the Hangar home for the profiles directory"))
}

/// Build a [`ProfileGetResult`](ainb_hangar_proto::snapshots::ProfileGetResult)
/// from a parsed master: its fields plus both compile previews (lossless Claude,
/// lossy Codex + dropped-field warnings).
fn profile_get_result(
    master: &ainb_hangar_core::profile::ProfileMaster,
) -> ainb_hangar_proto::snapshots::ProfileGetResult {
    let claude = master.compile_claude();
    let codex = master.compile_codex();
    ainb_hangar_proto::snapshots::ProfileGetResult {
        found: true,
        slug: master.slug.clone(),
        description: master.description.clone(),
        tier: master.tier.as_str().to_string(),
        tools: master.tools.clone(),
        color: master.color.clone().unwrap_or_default(),
        body: master.body.clone(),
        claude_preview: claude.contents,
        codex_preview: ainb_hangar_proto::snapshots::CodexPreview {
            config_fragment: codex.config_fragment,
            prompt: codex.prompt_contents,
            warnings: codex.warnings,
        },
    }
}

/// Extract the `workspace_id` from a workspace-scoped request's params.
fn workspace_id(req: &RpcRequest) -> Result<String, RpcError> {
    let params: ainb_hangar_proto::snapshots::WorkspaceScopedParams =
        serde_json::from_value(req.params.clone()).map_err(|e| RpcError {
            code: INVALID_PARAMS,
            message: format!("expected {{ workspace_id }}: {e}"),
            data: None,
        })?;
    Ok(params.workspace_id)
}

/// Resolve a wire workspace identifier (slug OR id) to the real workspace row id.
///
/// v1 is single-workspace; the plugin subscribes by slug (`"default"`), but real
/// workspaces are created with a ULID `id` distinct from their `slug`. The
/// `id = ?1 OR slug = ?1` form accepts BOTH a slug (the plugin's wire value) and a
/// literal id (any future id-passing caller). Returns `None` when no workspace
/// matches; callers then return an empty snapshot rather than an error.
async fn resolve_workspace_id(
    pool: &SqlitePool,
    wire: &str,
) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar("SELECT id FROM workspace WHERE id = ?1 OR slug = ?1 LIMIT 1")
        .bind(wire)
        .fetch_optional(pool)
        .await
}

/// Extract the wire `workspace_id` from `req` and resolve it to the real row id.
///
/// Returns `Ok(None)` (an empty-snapshot signal) when no workspace matches, and
/// an `INVALID_PARAMS` error only when the params are malformed (no `workspace_id`).
async fn resolve(pool: &SqlitePool, req: &RpcRequest) -> Result<Option<String>, RpcError> {
    let wire = workspace_id(req)?;
    resolve_workspace_id(pool, &wire).await.map_err(|e| store_err(&e))
}

/// Deserialize a request's `params` into `T`, mapping a shape mismatch to an
/// `INVALID_PARAMS` error whose message names the expected shape.
fn parse_params<T: serde::de::DeserializeOwned>(
    req: &RpcRequest,
    shape: &str,
) -> Result<T, RpcError> {
    serde_json::from_value(req.params.clone()).map_err(|e| RpcError {
        code: INVALID_PARAMS,
        message: format!("expected {shape}: {e}"),
        data: None,
    })
}

/// Resolve a wire workspace identifier (slug OR id) to the real row id,
/// returning `None` when no workspace matches and mapping a store fault to an
/// internal error. The id-bearing P6.5 handlers use this (they carry their own
/// params struct, unlike [`resolve`] which extracts `workspace_id` itself).
async fn resolve_wire(pool: &SqlitePool, wire: &str) -> Result<Option<WorkspaceId>, RpcError> {
    let id = resolve_workspace_id(pool, wire).await.map_err(|e| store_err(&e))?;
    Ok(id.and_then(|id| WorkspaceId::from_str(id).ok()))
}

/// Build a typed [`SkillId`] from a wire string, erroring on an empty id.
fn skill_id(raw: &str) -> Result<SkillId, RpcError> {
    SkillId::from_str(raw.to_string())
        .map_err(|_| invalid_params("skill_id must be a non-empty string"))
}

/// Build a typed [`AgentId`] from a wire string, erroring on an empty id.
fn agent_id(raw: &str) -> Result<AgentId, RpcError> {
    AgentId::from_str(raw.to_string())
        .map_err(|_| invalid_params("agent_id must be a non-empty string"))
}

/// Build a typed [`AutopilotId`] from a wire string, erroring on an empty id.
fn autopilot_id(raw: &str) -> Result<AutopilotId, RpcError> {
    AutopilotId::from_str(raw.to_string())
        .map_err(|_| invalid_params("autopilot_id must be a non-empty string"))
}

/// Dispatch `hangar/task_transition` (P8.4): drive the store FSM column-move,
/// then — only when a row actually moved — push the matching lifecycle event
/// to subscribed plugins (e38.2). A foreign / unknown task id moves nothing;
/// that is a no-op, not an error (mirrors the autopilot fire-now foreign-id
/// behaviour) and must not announce a state change that never happened. Split
/// out of [`handle`] to keep that dispatcher within the line cap.
async fn handle_task_transition(
    pool: &SqlitePool,
    req: &RpcRequest,
    events: &EventSink,
) -> Result<serde_json::Value, RpcError> {
    let params: ainb_hangar_proto::snapshots::TaskTransitionParams =
        parse_params(req, "{ workspace_id, task_id, to_status }")?;
    // The mutating handler must not silently no-op on a typo'd workspace.
    let ws = resolve_wire_or_reject(pool, &params.workspace_id).await?;
    let to = parse_task_status(&params.to_status)?;
    // Read the task BEFORE the move so we can distinguish a genuine into-terminal
    // edge from a terminal REPLAY. `transition_status` is an unconditional UPDATE, so
    // a `done -> done` re-drag reports `moved = true`; firing the dependency unblock
    // on that replay would RE-RUN an already-finished dependent. We only fire on a
    // real non-terminal → terminal edge. The pre-read also carries the issue /
    // workspace the hook keys on (unchanged by the move).
    use ainb_hangar_store::repo::task::TaskRepo;
    let before = TaskRepo::get_by_id(pool, &params.task_id).await.map_err(|e| store_err(&e))?;
    let was_terminal = before
        .as_ref()
        .is_some_and(|t| matches!(t.status.as_str(), "done" | "failed" | "cancelled"));
    let moved = snapshots::task_transition(pool, &SystemClock, ws.as_str(), &params.task_id, to)
        .await
        .map_err(|e| store_err(&e))?;
    if moved {
        if let Some(event) = task_transition_event(&params.task_id, to, SystemClock.now_ms()) {
            events.emit(ws.as_str(), event);
        }
        // tcp T4 / F7 + FANOUT-SEMANTICS: a MANUAL move to a terminal column must
        // fire the SAME dependency re-eval the finalize seam runs, so a card
        // hand-completed on the Kanban unblocks (and auto-runs) its dependents just
        // like a finalize-driven completion — but ONLY on a real non-terminal →
        // terminal edge (never a terminal replay). The hook keys on the issue's whole
        // active set, so a blocker that did not actually finish is a store-guarded
        // no-op. Best-effort.
        if to.is_terminal() && !was_terminal {
            if let Some(task) = before {
                crate::board::unblock_dependents_after_terminal(pool, &task).await;
            }
        }
    }
    Ok(serde_json::json!({}))
}

/// Dispatch `hangar/task_retry`: force-requeue one terminal task at an operator's
/// explicit request (the Task Kanban failed-column / task-detail `R`).
///
/// Unlike the automatic retry seam in the run loop, this is a HUMAN override:
/// [`RetryService::force_requeue`] bypasses both the `RetryDisposition` reason gate
/// and the `max_attempts` cap, so a terminal `agent_error` (which never
/// auto-retries) still spawns a fresh `queued` child. On a spawn we emit
/// [`HangarEvent::TaskQueued`] so every subscribed board re-pulls its task list and
/// the new attempt card appears in the queued column — the visible confirmation of
/// the requeue.
///
/// Workspace-scoped like the sibling mutators: an unknown workspace or a foreign /
/// missing task id is an `INVALID_PARAMS` rejection (a mutating handler must not
/// silently no-op on a typo). A non-terminal task answers `{ new_task_id: null }`
/// (nothing to requeue). A per-(issue, agent) pending-slot collision surfaces the
/// store error.
async fn handle_task_retry(
    pool: &SqlitePool,
    req: &RpcRequest,
    events: &EventSink,
) -> Result<serde_json::Value, RpcError> {
    use ainb_hangar_core::idgen::{IdGen, SystemIdGen};
    use ainb_hangar_store::repo::task::TaskRepo;
    use ainb_hangar_store::service::retry::{RetryDecision, RetryService};

    let params: ainb_hangar_proto::snapshots::TaskRetryParams =
        parse_params(req, "{ workspace_id, task_id }")?;
    let ws = resolve_wire_or_reject(pool, &params.workspace_id).await?;
    let task = TaskRepo::get_by_id(pool, &params.task_id)
        .await
        .map_err(|e| store_err(&e))?
        .filter(|t| t.workspace_id == ws.as_str())
        .ok_or_else(|| {
            invalid_params(&format!("no task `{}` in this workspace", params.task_id))
        })?;

    let new_id = SystemIdGen.new_ulid();
    let decision = RetryService::force_requeue(pool, &task, &new_id, &SystemClock)
        .await
        .map_err(|e| store_err(&e))?;

    let new_task_id = match decision {
        RetryDecision::Spawned { new_task_id } => {
            // Announce the fresh attempt so boards re-pull and surface the queued
            // card. TaskQueued needs the issue + agent ids; a task with no issue
            // still requeues, it just publishes no queue event (the next snapshot
            // pull reconciles either way).
            if let (Ok(task_id), Some(issue_raw)) = (
                ainb_hangar_core::ids::TaskId::from_str(new_task_id.clone()),
                task.issue_id.clone(),
            ) {
                if let (Ok(issue_id), Ok(agent_id)) = (
                    ainb_hangar_core::ids::IssueId::from_str(issue_raw),
                    AgentId::from_str(task.agent_id.clone()),
                ) {
                    events.emit(
                        ws.as_str(),
                        ainb_hangar_proto::events::HangarEvent::TaskQueued {
                            task_id,
                            issue_id,
                            agent_id,
                        },
                    );
                }
            }
            Some(new_task_id)
        }
        RetryDecision::DoNotRetry => None,
    };

    to_value(&ainb_hangar_proto::snapshots::TaskRetryResult { new_task_id })
}

/// Map a committed task transition onto its wire [`HangarEvent`] (e38.2).
///
/// `running` announces a start; the three terminal statuses announce a finish
/// with the matching [`TaskResult`](ainb_hangar_proto::events::TaskResult).
/// `queued` / `dispatched` map to no event: the only queue-shaped variant
/// ([`HangarEvent::TaskQueued`]) carries `issue_id` + `agent_id`, which a bare
/// column move does not know — and we never invent new variants. Returns
/// `None` (silently) for those, or for a malformed empty task id.
fn task_transition_event(
    task_id: &str,
    to: ainb_hangar_core::task_status::TaskStatus,
    now_ms: i64,
) -> Option<ainb_hangar_proto::events::HangarEvent> {
    use ainb_hangar_core::task_status::TaskStatus;
    use ainb_hangar_proto::events::{HangarEvent, TaskResult};

    let task_id = ainb_hangar_core::ids::TaskId::from_str(task_id.to_string()).ok()?;
    let at = chrono::DateTime::from_timestamp_millis(now_ms)?;
    match to {
        TaskStatus::Running => Some(HangarEvent::TaskStarted {
            task_id,
            started_at: at,
        }),
        TaskStatus::Done => Some(HangarEvent::TaskFinished {
            task_id,
            result: TaskResult::Success,
            ended_at: at,
        }),
        TaskStatus::Failed => Some(HangarEvent::TaskFinished {
            task_id,
            result: TaskResult::Failure,
            ended_at: at,
        }),
        TaskStatus::Cancelled => Some(HangarEvent::TaskFinished {
            task_id,
            result: TaskResult::Cancelled,
            ended_at: at,
        }),
        TaskStatus::Queued | TaskStatus::Dispatched => None,
    }
}

/// Parse a Kanban card-move target status from its wire token, rejecting an
/// unknown token with `INVALID_PARAMS` (P8.4). The six valid tokens are the
/// `snake_case` [`TaskStatus`] variants.
///
/// [`TaskStatus`]: ainb_hangar_core::task_status::TaskStatus
fn parse_task_status(raw: &str) -> Result<ainb_hangar_core::task_status::TaskStatus, RpcError> {
    serde_json::from_value::<ainb_hangar_core::task_status::TaskStatus>(serde_json::Value::String(
        raw.to_string(),
    ))
    .map_err(|_| {
        invalid_params(&format!(
            "to_status must be one of queued/dispatched/running/done/failed/cancelled, got `{raw}`"
        ))
    })
}

/// Dispatch `hangar/issues_search` (e38.12): ranked title + description +
/// comment substring search within a workspace, answering with the matching
/// [`IssueRow`]s in rank order (reusing the `issues_list` result envelope).
///
/// A read like `hangar/issues_list`: an unknown workspace yields an empty result
/// rather than an `INVALID_PARAMS` rejection (search is non-mutating, so a
/// mistyped workspace is "no matches", not a client error). Split out of
/// [`handle`] to keep that dispatcher within the line cap.
async fn handle_issues_search(
    pool: &SqlitePool,
    req: &RpcRequest,
) -> Result<serde_json::Value, RpcError> {
    let params: ainb_hangar_proto::snapshots::IssueSearchParams =
        parse_params(req, "{ workspace_id, query }")?;
    let issues = match resolve_workspace_id(pool, &params.workspace_id)
        .await
        .map_err(|e| store_err(&e))?
    {
        Some(ws) => snapshots::issues_search(pool, &ws, &params.query)
            .await
            .map_err(|e| store_err(&e))?,
        None => Vec::new(),
    };
    to_value(&ainb_hangar_proto::snapshots::IssuesListResult { issues })
}

/// Dispatch `hangar/search` (e38.13): ranked cross-entity command-palette search
/// across the workspace's issues, agents, skills, and autopilots, answering with
/// the matching [`SearchEntry`]s in rank order.
///
/// A read like `hangar/issues_search`: an unknown workspace yields an empty result
/// rather than an `INVALID_PARAMS` rejection (search is non-mutating, so a
/// mistyped workspace is "no matches", not a client error). Split out of [`handle`]
/// to keep that dispatcher within the line cap.
///
/// [`SearchEntry`]: ainb_hangar_proto::snapshots::SearchEntry
async fn handle_search(pool: &SqlitePool, req: &RpcRequest) -> Result<serde_json::Value, RpcError> {
    let params: ainb_hangar_proto::snapshots::SearchParams =
        parse_params(req, "{ workspace_id, query }")?;
    let entries = match resolve_workspace_id(pool, &params.workspace_id)
        .await
        .map_err(|e| store_err(&e))?
    {
        Some(ws) => snapshots::search(pool, &ws, &params.query).await.map_err(|e| store_err(&e))?,
        None => Vec::new(),
    };
    to_value(&ainb_hangar_proto::snapshots::SearchResult { entries })
}

/// Dispatch `hangar/tasks_list` (P8.4): snapshot the workspace's task queue for
/// the Kanban board. An unknown workspace yields an empty set (a read). Split out
/// of [`handle`] to keep that dispatcher within the line cap.
async fn handle_tasks_list(
    pool: &SqlitePool,
    req: &RpcRequest,
) -> Result<serde_json::Value, RpcError> {
    // tcp T2: the board card surfaces a PR'd card's CI + merge status. The fetch
    // rides the same injectable seam the issue task-detail badge uses (a `gh`
    // subprocess in production, a stub `gh` under `HANGAR_GH_PATH` in e2e), and
    // only fires for the handful of cards that captured a PR. It is wrapped in the
    // shared TTL cache so the board's per-event `tasks_list` re-pull coalesces to
    // ~one `gh` spawn per PR URL per window rather than one per card per event.
    // `Arc`-shared so the le3 concurrent fetch can hand each spawned task an owned
    // (`'static`) clone; the TTL cache lives behind the single wrapped provider.
    let provider: std::sync::Arc<dyn crate::pr_status::PrStatusProvider> =
        std::sync::Arc::new(crate::pr_status::CachingPrStatusProvider::new(
            crate::pr_status::GhPrStatusProvider::from_env(),
        ));
    let tasks = match resolve(pool, req).await? {
        Some(ws) => snapshots::tasks_list(pool, &ws, provider).await.map_err(|e| store_err(&e))?,
        None => Vec::new(),
    };
    to_value(&ainb_hangar_proto::snapshots::TasksListResult { tasks })
}

/// Dispatch `hangar/usage_rollup` (e38.35): snapshot the workspace's token/cost
/// usage dashboard (grand totals + per-agent breakdown) off the durable
/// `task_usage` aggregate. An unknown workspace yields all-zero totals + an empty
/// rollup (a read). Split out of [`handle`] to keep that dispatcher within the
/// line cap.
async fn handle_usage_rollup(
    pool: &SqlitePool,
    req: &RpcRequest,
) -> Result<serde_json::Value, RpcError> {
    let rollup = match resolve(pool, req).await? {
        Some(ws) => snapshots::usage_rollup(pool, &ws).await.map_err(|e| store_err(&e))?,
        None => ainb_hangar_proto::snapshots::UsageRollupResult::default(),
    };
    to_value(&rollup)
}

/// Default row cap for `hangar/run_history` when the caller omits `limit`.
const RUN_HISTORY_DEFAULT_LIMIT: i64 = 100;
/// Hard ceiling on `hangar/run_history` rows — a caller cannot ask for an
/// unbounded scan (a huge or negative limit is clamped into `1..=MAX`).
const RUN_HISTORY_MAX_LIMIT: i64 = 500;

/// Dispatch `hangar/run_history` (P10 / D19): snapshot the workspace's per-run
/// observability timeline (newest finished first) off the durable `run_history`
/// rows the run loop appends at each finalize seam. An unknown workspace yields an
/// empty timeline (a read). The optional `limit` is clamped to
/// `1..=RUN_HISTORY_MAX_LIMIT`. Split out of [`handle`] to keep that dispatcher
/// within the line cap.
async fn handle_run_history(
    pool: &SqlitePool,
    req: &RpcRequest,
) -> Result<serde_json::Value, RpcError> {
    let params: ainb_hangar_proto::snapshots::RunHistoryParams =
        serde_json::from_value(req.params.clone()).map_err(|e| RpcError {
            code: INVALID_PARAMS,
            message: format!("expected {{ workspace_id, limit? }}: {e}"),
            data: None,
        })?;
    let limit = params
        .limit
        .unwrap_or(RUN_HISTORY_DEFAULT_LIMIT)
        .clamp(1, RUN_HISTORY_MAX_LIMIT);
    let history = match resolve_workspace_id(pool, &params.workspace_id)
        .await
        .map_err(|e| store_err(&e))?
    {
        Some(ws) => snapshots::run_history(pool, &ws, limit).await.map_err(|e| store_err(&e))?,
        None => ainb_hangar_proto::snapshots::RunHistoryResult::default(),
    };
    to_value(&history)
}

/// Dispatch `hangar/pr_status_refresh` (e38.34): fetch the CI + merge status of
/// an issue's bound PR and auto-move the issue to Done on merge.
///
/// Mutating + workspace-scoped: resolves the workspace and **rejects** a mistyped
/// one with `INVALID_PARAMS` (never a silent no-op, mirroring
/// [`handle_task_transition`]). Delegates to [`snapshots::refresh_pr_status`] with
/// the production [`crate::pr_status::GhPrStatusProvider`] (a `gh` subprocess that
/// degrades to all-`Unknown` when absent / unauthenticated). When the refresh
/// performed the auto-Done transition, pushes the `IssueUpdated` event so a
/// subscribed board reflects the column move, and answers with
/// `transitioned_to_done: true`. An issue with no bound PR answers an all-unknown
/// status + `false` (a read). Split out of [`handle`] to keep that dispatcher
/// within the line cap.
async fn handle_pr_status_refresh(
    pool: &SqlitePool,
    req: &RpcRequest,
    events: &EventSink,
) -> Result<serde_json::Value, RpcError> {
    use ainb_hangar_proto::events::HangarEvent;

    let params: ainb_hangar_proto::snapshots::PrStatusRefreshParams =
        parse_params(req, "{ workspace_id, issue_id }")?;
    // The mutating handler must not silently no-op on a typo'd workspace.
    let ws = resolve_wire_or_reject(pool, &params.workspace_id).await?;
    let provider = crate::pr_status::GhPrStatusProvider::from_env();
    let (status, transitioned) =
        snapshots::refresh_pr_status(pool, ws.as_str(), &params.issue_id, &provider)
            .await
            .map_err(|e| store_err(&e))?;
    // Only a committed transition announces the column move to subscribers.
    if let Some(row) = transitioned.clone() {
        events.emit(ws.as_str(), HangarEvent::IssueUpdated(row));
    }
    to_value(&ainb_hangar_proto::snapshots::PrStatusRefreshResult {
        status,
        transitioned_to_done: transitioned.is_some(),
    })
}

/// Dispatch `hangar/issue_create` (e38.29): create one new issue, push the
/// matching `IssueCreated` event, and answer with the persisted row.
///
/// Mirrors [`handle_comment_add`]'s contract: the mutating handler resolves the
/// workspace and **rejects** a mistyped one with `INVALID_PARAMS` (never a silent
/// no-op), validates a non-blank title, parses the creator actor-ref, then drives
/// the store insert with a daemon-minted id + timestamp. The new row is announced
/// to subscribers so a subscribed issue list re-renders it without a full
/// re-pull. Split out of [`handle`] to keep that dispatcher within the line cap.
async fn handle_issue_create(
    pool: &SqlitePool,
    req: &RpcRequest,
    events: &EventSink,
) -> Result<serde_json::Value, RpcError> {
    use ainb_hangar_core::actor::ActorRef;
    use ainb_hangar_core::idgen::SystemIdGen;
    use ainb_hangar_proto::events::HangarEvent;
    use std::str::FromStr as _;

    let params: ainb_hangar_proto::snapshots::IssueCreateParams = parse_params(
        req,
        "{ workspace_id, title, description?, creator, external_ref?, acceptance_criteria?, context_refs?, priority?, due_date?, labels? }",
    )?;
    // The mutating handler must not silently no-op on a typo'd workspace.
    let ws = resolve_wire_or_reject(pool, &params.workspace_id).await?;
    // A blank title is a client error, not an empty row.
    if params.title.trim().is_empty() {
        return Err(invalid_params("issue title must not be empty"));
    }
    let creator = ActorRef::from_str(&params.creator).map_err(|e| {
        invalid_params(&format!(
            "creator must be `agent:<id>` or `member:<id>`: {e}"
        ))
    })?;
    // 0043: an upstream link is optional; a blank one links nothing (stored NULL).
    let external_ref = params.external_ref.as_deref().map(str::trim).filter(|s| !s.is_empty());
    // 0046: an optional parent makes the new issue a sub-issue. Validate the parent
    // resolves in THIS workspace (mirrors the assignee-resolve contract) — a
    // foreign/unknown parent is a client error, never a silent cross-tenant link.
    let parent_issue_id =
        params.parent_issue_id.as_deref().map(str::trim).filter(|s| !s.is_empty());
    // 0048: trim-drop blank list elements at the boundary — an empty-string
    // criterion / ref is a UI artefact, not data. An empty list is valid (no error).
    let acceptance_criteria: Vec<String> = params
        .acceptance_criteria
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(ToString::to_string)
        .collect();
    let context_refs: Vec<String> = params
        .context_refs
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(ToString::to_string)
        .collect();
    // 0014 priority: `0..3` (P3..P0). An out-of-vocabulary value is a client
    // error, mirroring multica's `validateIssueEnum` — NEVER silently clamped,
    // which would persist an urgency the author did not ask for.
    let priority = params.priority.unwrap_or(0);
    if !(0..=3).contains(&priority) {
        return Err(invalid_params("issue priority must be 0..3 (P3..P0)"));
    }
    // 0014 due date: the wire carries epoch ms at UTC midnight (the client parses
    // the `YYYY-MM-DD` calendar day with `proto::dates::parse_calendar_date_ms`),
    // so any i64 is accepted here — a pre-1970 deadline is legal, if odd.
    let due_date = params.due_date;
    // 0016 labels: trim-drop blanks like the other lists, and dedupe preserving
    // first-seen order. `LabelRepo::attach` is idempotent so a duplicate would not
    // corrupt the join, but the response row must not imply the repeat mattered.
    let mut labels: Vec<String> = Vec::new();
    for name in params.labels.iter().map(|s| s.trim()).filter(|s| !s.is_empty()) {
        if !labels.iter().any(|seen| seen == name) {
            labels.push(name.to_string());
        }
    }
    if let Some(parent) = parent_issue_id {
        let ok = ainb_hangar_store::repo::issue::IssueRepo::get_by_id(pool, parent)
            .await
            .map_err(|e| store_err(&e))?
            .is_some_and(|p| p.workspace_id == ws.as_str());
        if !ok {
            return Err(invalid_params(&format!(
                "parent issue `{parent}` not found in this workspace"
            )));
        }
    }
    let row = snapshots::issue_create(
        pool,
        &SystemIdGen,
        &SystemClock,
        &snapshots::IssueCreateInput {
            workspace_id: ws.as_str(),
            title: &params.title,
            description: params.description.as_deref(),
            creator: &creator,
            external_ref,
            parent_issue_id,
            acceptance_criteria: &acceptance_criteria,
            context_refs: &context_refs,
            priority,
            due_date,
            labels: &labels,
        },
    )
    .await
    .map_err(|e| store_err(&e))?;
    // A committed insert announces the new issue to subscribers.
    events.emit(ws.as_str(), HangarEvent::IssueCreated(row.clone()));
    to_value(&row)
}

/// Dispatch `hangar/issue_delete` (63d): delete one issue and all its history,
/// push the matching `IssueDeleted` event, and answer with `{}`.
///
/// Mirrors [`handle_issue_update`]'s contract: the mutating handler resolves the
/// workspace and **rejects** a mistyped one with `INVALID_PARAMS`, then drives the
/// store's single-transaction cascade. A `(id, workspace)` pair that matches no
/// issue is rejected as a not-found error (never a cross-tenant delete), and an
/// ACTIVE task on the issue refuses the delete (`INVALID_PARAMS` telling the caller
/// to cancel the run first). Only a committed delete pushes the event.
async fn handle_issue_delete(
    pool: &SqlitePool,
    req: &RpcRequest,
    events: &EventSink,
) -> Result<serde_json::Value, RpcError> {
    use ainb_hangar_core::ids::IssueId;
    use ainb_hangar_proto::events::HangarEvent;
    use ainb_hangar_store::repo::issue::{IssueDeleteError, IssueRepo};

    let params: ainb_hangar_proto::snapshots::IssueDeleteParams =
        parse_params(req, "{ workspace_id, issue_id }")?;
    // The mutating handler must not silently no-op on a typo'd workspace.
    let ws = resolve_wire_or_reject(pool, &params.workspace_id).await?;
    IssueRepo::delete_cascade(pool, ws.as_str(), &params.issue_id)
        .await
        .map_err(|e| match e {
            // An unknown id or a cross-tenant issue: reject rather than ack a
            // delete that never happened.
            IssueDeleteError::NotFound => {
                invalid_params(&format!("no issue `{}` in this workspace", params.issue_id))
            }
            // A live run blocks the delete — surface the "cancel first" message,
            // tagged with a machine-readable marker so the TUI can offer an inline
            // "cancel the run(s) & delete" instead of dead-ending on the text. The
            // `data` field is append-only (an older client ignores it and still
            // reads the human message).
            IssueDeleteError::ActiveTasks(n) => RpcError {
                code: INVALID_PARAMS,
                message: e.to_string(),
                data: Some(serde_json::json!({ "reason": "active_tasks", "active": n })),
            },
            IssueDeleteError::Db(ref db) => store_err(db),
        })?;
    // A committed delete announces the removal so a subscribed issue list drops
    // the row without a full re-pull.
    let issue_id = IssueId::from_str(params.issue_id.as_str())
        .map_err(|e| invalid_params(&format!("malformed issue id: {e}")))?;
    events.emit(ws.as_str(), HangarEvent::IssueDeleted { issue_id });
    to_value(&serde_json::json!({}))
}

/// Dispatch `hangar/issue_cancel_active`: cancel EVERY active task on one issue,
/// with no board coordinates — the Issues-screen "cancel the run(s) & delete"
/// affordance.
///
/// The board-less sibling of [`handle_board_card_cancel`]: it resolves the issue's
/// ENTIRE active set (a squad card fans out N tasks onto one issue, so there may be
/// several) and cancels each via the idempotent `CancelTaskService` FSM edge,
/// signalling each live run to KILL and pushing its terminal event. Per-task
/// outcomes:
/// - `Transitioned` — this call won the cancel: SIGNAL kill + push terminal.
/// - `AlreadyTerminal` — an idempotent replay; counted, nothing more.
/// - `TerminalMismatch` — that task finished naturally first; leave it.
/// A per-task store fault is logged and the loop continues (a surviving sibling is
/// worse than a clean error); it only surfaces if siblings remain active after the
/// pass. An issue with no active task is a clean `{ cancelled: 0 }`, never an error.
/// On any cancel the card's board placement (if any) is aggregate-auto-moved and
/// its dependents re-evaluated, matching the card-cancel path.
async fn handle_issue_cancel_active(
    pool: &SqlitePool,
    req: &RpcRequest,
    events: &EventSink,
) -> Result<serde_json::Value, RpcError> {
    use ainb_hangar_store::repo::task::TaskRepo;
    use ainb_hangar_store::service::cancel::CancelTaskService;
    use ainb_hangar_store::service::finalize::{FinalizeError, FinalizeOutcome};

    let params: ainb_hangar_proto::snapshots::IssueCancelActiveParams =
        parse_params(req, "{ workspace_id, issue_id }")?;
    let ws = resolve_wire_or_reject(pool, &params.workspace_id).await?;
    if params.issue_id.trim().is_empty() {
        return Err(invalid_params("issue_id must not be empty"));
    }

    // The issue's ENTIRE active set (newest first). Empty = nothing to cancel — a
    // clean `{ cancelled: 0 }` the caller surfaces as a note, never an error. The
    // newest task is the "primary" whose board card the post-drain reconcile keys off.
    let active = TaskRepo::active_tasks_for_issue(pool, ws.as_str(), &params.issue_id)
        .await
        .map_err(|e| store_err(&e))?;
    let Some(primary) = active.first() else {
        return to_value(&ainb_hangar_proto::snapshots::IssueCancelActiveResult { cancelled: 0 });
    };

    let mut cancelled: u64 = 0;
    let mut last_err: Option<String> = None;
    for task in &active {
        match CancelTaskService::cancel(pool, &task.id, &SystemClock).await {
            Ok(FinalizeOutcome::Transitioned) => {
                // `false` = no live run was registered (queued-but-unclaimed, or
                // owned by another daemon) — the DB flip alone cancels it.
                let signalled = crate::cancel::registry().signal(&task.id);
                crate::run_loop::emit_task_finished(
                    events,
                    task,
                    ainb_hangar_proto::events::TaskResult::Cancelled,
                    &SystemClock,
                );
                tracing::info!(task_id = %task.id, signalled, issue = %params.issue_id, "issue cancel: task cancelled");
                cancelled += 1;
            }
            Ok(FinalizeOutcome::AlreadyTerminal) => cancelled += 1,
            Err(FinalizeError::TerminalMismatch { .. }) => {}
            Err(e) => {
                tracing::warn!(task_id = %task.id, error = %e, "issue cancel: a task cancel errored; continuing");
                last_err = Some(e.to_string());
            }
        }
    }

    // Honesty guard: if any per-task cancel raised a store fault, the cancel may be
    // PARTIAL — re-read the active set and surface an error while siblings survive,
    // rather than reporting a clean success (which would let the caller's delete
    // retry get refused again with no explanation).
    if let Some(e) = last_err {
        let residual = TaskRepo::active_tasks_for_issue(pool, ws.as_str(), &params.issue_id)
            .await
            .map_err(|e| store_err(&e))?;
        if !residual.is_empty() {
            return Err(internal(&format!(
                "cancel partially failed: {} task(s) still active ({e})",
                residual.len()
            )));
        }
    }

    if cancelled > 0 {
        // Reconcile any board placement of this issue now the set has drained
        // (best-effort + idempotent, matching the card-cancel path). Harmless when
        // the issue is not on any board.
        crate::board::auto_move_after_terminal(pool, primary).await;
        crate::board::unblock_dependents_after_terminal(pool, primary).await;
    }

    to_value(&ainb_hangar_proto::snapshots::IssueCancelActiveResult { cancelled })
}

/// Dispatch `hangar/issue_update` (e38.8): edit one issue's fields, push the
/// matching `IssueUpdated` event, and answer with the refreshed row.
///
/// Mirrors [`handle_task_transition`]'s contract: the mutating handler resolves
/// the workspace and **rejects** a mistyped one with `INVALID_PARAMS` (never a
/// silent no-op), parses the assignee actor-ref, then drives the
/// workspace-scoped store edit. A `(id, workspace)` pair that matches no row
/// (an unknown id, or an issue owned by another tenant) is rejected as a
/// not-found error — never a cross-tenant edit. Only a committed edit pushes the
/// event. Split out of [`handle`] to keep that dispatcher within the line cap.
/// Persist an F6 card edit (repo, agent, upstream ref, branches) onto the durable
/// issue, mirroring `board_card_create`. Called only once the target row resolved,
/// so a foreign / unknown issue is rejected before any write. Safe while a run is
/// in flight: the running task captured its repo + agent at ENQUEUE, so an edit
/// only steers the NEXT run. Crucially the branch write lands BEFORE the named-agent
/// auto-dispatch in [`handle_issue_update`], so that run reads the card's real
/// `source_branch` instead of a NULL that would branch the worktree off `main`.
#[allow(clippy::too_many_arguments)]
async fn persist_card_edits(
    pool: &SqlitePool,
    workspace_id: &str,
    issue_id: &str,
    repo_ref: Option<&str>,
    agent: Option<ainb_hangar_core::agent_kind::AgentKind>,
    external_ref: Option<&str>,
    source_branch: Option<&str>,
    target_branch: Option<&str>,
) -> Result<(), RpcError> {
    use ainb_hangar_store::repo::card_parity::CardParityRepo;

    // bead pv8 parity with `board_card_create`: a remote-only favorite pick arrives
    // as its REMOTE indicator (`owner/repo`, a URL) — not an absolute path, not
    // `scratch`. Resolve it to a LOCAL clone path BEFORE persisting so the
    // run/provision path (which only understands a path or `scratch`) never sees a
    // bare remote it would mistake for a filesystem path. A path / `scratch` passes
    // through untouched; the clone runs once, idempotently.
    let resolved_repo_ref = match repo_ref {
        Some(r) => {
            let ainb_dir = ainb_hangar_core::hangar_home()
                .ok_or_else(|| internal("cannot resolve hangar home to clone a remote favorite"))?;
            Some(resolve_card_repo_ref(&ainb_dir, r).await?)
        }
        None => None,
    };
    CardParityRepo::set_issue_repo_agent(
        pool,
        workspace_id,
        issue_id,
        resolved_repo_ref.as_deref(),
        agent,
    )
    .await
    .map_err(|e| store_err(&e))?;
    CardParityRepo::set_issue_external_ref(pool, workspace_id, issue_id, external_ref)
        .await
        .map_err(|e| store_err(&e))?;
    CardParityRepo::set_issue_branches(pool, workspace_id, issue_id, source_branch, target_branch)
        .await
        .map_err(|e| store_err(&e))?;
    Ok(())
}

/// Read an issue's current `state` before an update, but only when the edit
/// actually changes `state` (0046). Workspace-scoped, so a foreign/unknown id
/// reads `None`. The daemon feeds this to the child-done cascade so it sees the
/// real non-terminal → terminal transition; a non-state edit skips the query.
async fn issue_prev_state_for_cascade(
    pool: &SqlitePool,
    ws: &ainb_hangar_core::ids::WorkspaceId,
    issue_id: &str,
    update: &ainb_hangar_store::repo::issue::IssueFieldUpdate,
) -> Result<Option<String>, RpcError> {
    if update.state.is_none() {
        return Ok(None);
    }
    Ok(
        ainb_hangar_store::repo::issue::IssueRepo::get_by_id(pool, issue_id)
            .await
            .map_err(|e| store_err(&e))?
            .filter(|i| i.workspace_id == ws.as_str())
            .map(|i| i.state),
    )
}

async fn handle_issue_update(
    pool: &SqlitePool,
    req: &RpcRequest,
    events: &EventSink,
) -> Result<serde_json::Value, RpcError> {
    use ainb_hangar_proto::events::HangarEvent;

    use ainb_hangar_core::agent_kind::AgentKind;

    let params: ainb_hangar_proto::snapshots::IssueUpdateParams = parse_params(
        req,
        "{ workspace_id, issue_id, state?, assignee?, priority?, due_date?, title?, repo_ref?, agent?, external_ref? }",
    )?;
    // The mutating handler must not silently no-op on a typo'd workspace.
    let ws = resolve_wire_or_reject(pool, &params.workspace_id).await?;
    let update = issue_field_update_from_params(&params)?;

    // 0046: capture the issue's PRE-update state (only when a state edit is
    // requested) so a completion can fire the child-done → parent cascade below.
    let prev_state = issue_prev_state_for_cascade(pool, &ws, &params.issue_id, &update).await?;

    // F6 card edit: the card's repo + chosen agent are persisted on the durable
    // card (the issue) exactly as `board_card_create` does — trim the repo, drop an
    // unrecognised agent token (the F4 cascade decides), and only write when a
    // repo/agent edit is actually requested.
    let repo_ref = params.repo_ref.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let agent = params.agent.as_deref().and_then(AgentKind::parse);
    // 0043: an upstream-issue link edit (blank leaves it unchanged, not cleared).
    let external_ref = params.external_ref.as_deref().map(str::trim).filter(|s| !s.is_empty());
    // 0042: the branch overrides the create-wizard Source field carries. Blank
    // leaves each unchanged (never cleared); persisting them BEFORE the named-agent
    // auto-dispatch below is what lets that run read the card's real source branch.
    let source_branch = params.source_branch.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let target_branch = params.target_branch.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let edits_card = repo_ref.is_some()
        || agent.is_some()
        || external_ref.is_some()
        || source_branch.is_some()
        || target_branch.is_some();

    // Resolve (and, for the field edit, write) the refreshed row. A field edit runs
    // the scoped UPDATE + re-read; a repo/agent-ONLY edit changes no field-UPDATE
    // column, so read the row directly to resolve identity + answer with it.
    let row = if !update.is_empty() {
        snapshots::issue_update(pool, ws.as_str(), &params.issue_id, &update)
            .await
            .map_err(|e| store_err(&e))?
    } else if edits_card {
        snapshots::issue_row(pool, ws.as_str(), &params.issue_id)
            .await
            .map_err(|e| store_err(&e))?
    } else {
        // A truly-empty edit resolves no row (the existing no-op-rejects contract).
        None
    };
    // No row matched the (id, workspace) pair: an unknown id or a cross-tenant
    // issue. Reject rather than ack a write that never happened.
    let Some(row) = row else {
        return Err(invalid_params(&format!(
            "no issue `{}` in this workspace",
            params.issue_id
        )));
    };

    // Persist the card's repo + agent AFTER the row resolved (so a foreign / unknown
    // issue is rejected before any write). Safe while a run is in flight: the running
    // task captured its repo + agent at ENQUEUE (`set_task_repo_agent_in_tx`), so an
    // edit only steers the NEXT run — never mutates a task already dispatched.
    if edits_card {
        persist_card_edits(
            pool,
            ws.as_str(),
            &params.issue_id,
            repo_ref,
            agent,
            external_ref,
            source_branch,
            target_branch,
        )
        .await?;
    }

    // A committed edit announces the refreshed row to subscribers. Re-read AFTER
    // the card-parity writes so the pushed row reflects a just-set external_ref.
    let row = if external_ref.is_some() {
        snapshots::issue_row(pool, ws.as_str(), &params.issue_id)
            .await
            .map_err(|e| store_err(&e))?
            .unwrap_or(row)
    } else {
        row
    };

    // In-product recovery from a dead end: an assignment that names an AGENT
    // re-dispatches the issue through the shared `run_card` launch core, mirroring
    // the create-time dispatch. `agent_error` is terminal + non-retryable, so
    // without this a stuck issue had no in-product path back to work short of
    // filing a brand-new one — the TUI `a` picker and `issue_update --assign` both
    // route here. Reusing `run_card` reads the card's persisted repo/branch/agent,
    // mints a fresh run generation, and — via the one-active-run guard — never
    // double-dispatches (a re-assign only re-runs once the prior run is terminal).
    // Best-effort: a launch guard (no repo, a run already active, an unfinished
    // blocker, a not-yet-dispatchable provider) leaves the assignee edit committed
    // without a new run rather than failing the edit; only a store fault propagates.
    if let Some(Some(actor)) = update.assignee.as_ref() {
        if actor.kind() == ainb_hangar_core::actor::ActorKind::Agent {
            if let Some(issue) =
                ainb_hangar_store::repo::issue::IssueRepo::get_by_id(pool, &params.issue_id)
                    .await
                    .map_err(|e| store_err(&e))?
                    .filter(|i| i.workspace_id == ws.as_str())
            {
                match run_card(
                    pool,
                    &ws,
                    None,
                    &issue,
                    "headless",
                    None,
                    None,
                    None,
                    Some(actor),
                    None, // owner-invoked recovery re-dispatch
                )
                .await
                {
                    Ok(_) => {}
                    Err(CardRunError::Db(e)) => return Err(store_err(&e)),
                    Err(other) => {
                        tracing::info!(
                            issue = %params.issue_id,
                            reason = %card_run_err(other).message,
                            "issue_update: assignee set but re-dispatch skipped",
                        );
                    }
                }
            }
        }
    }

    // 0046: a state edit that moved this sub-issue into a terminal token cascades a
    // roll-up comment onto its parent (and wakes an agent/squad parent). Fires
    // AFTER the state UPDATE committed, best-effort — the comment is the durable
    // side; the event push + parent wake are opportunistic. A non-terminal edit, a
    // top-level issue, or an unclosed stage barrier is a silent no-op.
    if let (Some(prev), Some(new_state)) = (prev_state.as_deref(), params.state.as_deref()) {
        crate::board::maybe_cascade_child_done(
            pool,
            &ws,
            &params.issue_id,
            prev,
            new_state,
            events,
        )
        .await;
    }

    events.emit(ws.as_str(), HangarEvent::IssueUpdated(row.clone()));
    to_value(&row)
}

/// Map the wire [`IssueUpdateParams`] onto the store's [`IssueFieldUpdate`],
/// parsing the optional assignee actor-ref (`"agent:<id>"` / `"member:<id>"`).
///
/// The three nullable-field states cross the boundary intact: the wire
/// [`FieldUpdate`] (omitted / null / value) maps onto the store's nested
/// `Option<Option<_>>` (leave / clear / set). A malformed assignee ref is an
/// `INVALID_PARAMS` client error.
///
/// [`IssueUpdateParams`]: ainb_hangar_proto::snapshots::IssueUpdateParams
/// [`IssueFieldUpdate`]: ainb_hangar_store::repo::issue::IssueFieldUpdate
/// [`FieldUpdate`]: ainb_hangar_proto::snapshots::FieldUpdate
fn issue_field_update_from_params(
    params: &ainb_hangar_proto::snapshots::IssueUpdateParams,
) -> Result<ainb_hangar_store::repo::issue::IssueFieldUpdate, RpcError> {
    use ainb_hangar_core::actor::ActorRef;
    use ainb_hangar_proto::snapshots::FieldUpdate;
    use std::str::FromStr as _;

    let assignee = match &params.assignee {
        FieldUpdate::Keep => None,
        FieldUpdate::Clear => Some(None),
        FieldUpdate::Set(raw) => {
            let actor = ActorRef::from_str(raw).map_err(|e| {
                invalid_params(&format!(
                    "assignee must be `agent:<id>` or `member:<id>`: {e}"
                ))
            })?;
            Some(Some(actor))
        }
    };
    let due_date = match params.due_date {
        FieldUpdate::Keep => None,
        FieldUpdate::Clear => Some(None),
        FieldUpdate::Set(ts) => Some(Some(ts)),
    };
    // F6 card edit: a title is set only when present + non-blank (a blank title is
    // a client error, mirroring `issue_create`, never a stored empty title).
    let title = match &params.title {
        None => None,
        Some(t) if t.trim().is_empty() => {
            return Err(invalid_params("title must not be blank"));
        }
        Some(t) => Some(t.trim().to_string()),
    };
    Ok(ainb_hangar_store::repo::issue::IssueFieldUpdate {
        title,
        state: params.state.clone(),
        assignee,
        priority: params.priority,
        due_date,
    })
}

/// Dispatch `hangar/issue_label_attach` (`attach = true`) /
/// `hangar/issue_label_detach` (`attach = false`) (e38.10): mutate one issue's
/// labels, push the matching `IssueUpdated` event, and answer with the refreshed
/// row.
///
/// Mirrors [`handle_issue_update`]'s contract: the mutating handler resolves the
/// workspace and **rejects** a mistyped one with `INVALID_PARAMS` (never a silent
/// no-op), then drives the workspace-scoped store mutation. An `(issue_id,
/// workspace)` pair that matches no row (an unknown id, or an issue owned by
/// another tenant) is rejected as a not-found error — never a cross-tenant
/// (de)label. Only a committed mutation pushes the event. Split out of
/// [`handle`] to keep that dispatcher within the line cap.
async fn handle_issue_label(
    pool: &SqlitePool,
    req: &RpcRequest,
    events: &EventSink,
    attach: bool,
) -> Result<serde_json::Value, RpcError> {
    use ainb_hangar_proto::events::HangarEvent;

    let params: ainb_hangar_proto::snapshots::IssueLabelParams =
        parse_params(req, "{ workspace_id, issue_id, name, color? }")?;
    // The mutating handler must not silently no-op on a typo'd workspace.
    let ws = resolve_wire_or_reject(pool, &params.workspace_id).await?;
    // A blank label name is a client error, not a no-op mutation.
    if params.name.trim().is_empty() {
        return Err(invalid_params("label name must not be empty"));
    }
    let row = if attach {
        snapshots::issue_label_attach(
            pool,
            &ws,
            &params.issue_id,
            params.name.trim(),
            params.color.as_deref(),
        )
        .await
    } else {
        snapshots::issue_label_detach(pool, &ws, &params.issue_id, params.name.trim()).await
    }
    .map_err(|e| label_repo_err(&e))?;
    // No row matched the (id, workspace) pair: an unknown id or a cross-tenant
    // issue. Reject rather than ack a write that never happened.
    let Some(row) = row else {
        return Err(invalid_params(&format!(
            "no issue `{}` in this workspace",
            params.issue_id
        )));
    };
    // A committed label change announces the refreshed row to subscribers so a
    // subscribed issue list re-renders the chip.
    events.emit(ws.as_str(), HangarEvent::IssueUpdated(row.clone()));
    to_value(&row)
}

/// Map a [`LabelRepoError`] onto an RPC error: the issue-not-found guard is a
/// client error (`INVALID_PARAMS`, the caller used a foreign / unknown issue id),
/// every other fault is an internal store error.
///
/// [`LabelRepoError`]: ainb_hangar_store::repo::label::LabelRepoError
fn label_repo_err(e: &ainb_hangar_store::repo::label::LabelRepoError) -> RpcError {
    use ainb_hangar_store::repo::label::LabelRepoError;
    match e {
        LabelRepoError::IssueNotFound => invalid_params("no issue in this workspace"),
        LabelRepoError::Db(db) => internal(&format!("label store error: {db}")),
    }
}

/// Dispatch `hangar/comment_add` (e38.5): append a comment to one issue, push the
/// matching `CommentAdded` event, and answer with the persisted row.
///
/// Mirrors [`handle_issue_update`]'s contract: the mutating handler resolves the
/// workspace and **rejects** a mistyped one with `INVALID_PARAMS` (never a silent
/// no-op), parses the author actor-ref + validates a non-empty body, then drives
/// the workspace-scoped store insert. An `(issue_id, workspace)` pair that
/// matches no row (an unknown id, or an issue owned by another tenant) is rejected
/// as a not-found error — never a cross-tenant comment. Only a committed insert
/// pushes the event. Split out of [`handle`] to keep that dispatcher within the
/// line cap.
async fn handle_comment_add(
    pool: &SqlitePool,
    req: &RpcRequest,
    events: &EventSink,
) -> Result<serde_json::Value, RpcError> {
    use ainb_hangar_core::actor::ActorRef;
    use ainb_hangar_core::idgen::SystemIdGen;
    use ainb_hangar_proto::events::HangarEvent;
    use std::str::FromStr as _;

    let params: ainb_hangar_proto::snapshots::CommentAddParams =
        parse_params(req, "{ workspace_id, issue_id, author, body }")?;
    // The mutating handler must not silently no-op on a typo'd workspace.
    let ws = resolve_wire_or_reject(pool, &params.workspace_id).await?;
    // A blank comment is a client error, not an empty row.
    if params.body.trim().is_empty() {
        return Err(invalid_params("comment body must not be empty"));
    }
    let author = ActorRef::from_str(&params.author).map_err(|e| {
        invalid_params(&format!(
            "author must be `agent:<id>` or `member:<id>`: {e}"
        ))
    })?;
    let row = snapshots::comment_add(
        pool,
        &SystemIdGen,
        &SystemClock,
        ws.as_str(),
        &params.issue_id,
        &author,
        &params.body,
    )
    .await
    .map_err(|e| store_err(&e))?;
    // No row landed: the (issue, workspace) pair matched no issue — an unknown id
    // or a cross-tenant issue. Reject rather than ack a write that never happened.
    let Some(row) = row else {
        return Err(invalid_params(&format!(
            "no issue `{}` in this workspace",
            params.issue_id
        )));
    };
    // A committed insert announces the new comment to subscribers.
    events.emit(ws.as_str(), HangarEvent::CommentAdded(row.clone()));
    // e38.7 — the collaboration trigger: now that the comment has committed,
    // parse its @-mentions and enqueue a task for every agent that resolves in
    // this workspace. Firing AFTER the commit means a spawn-side fault can never
    // lose the comment; an unknown handle resolves to nothing and is ignored. A
    // store fault here is logged, not surfaced — the comment already landed and a
    // failed trigger must not turn a successful comment into an RPC error.
    if let Err(e) = snapshots::spawn_mention_tasks(
        pool,
        &SystemIdGen,
        &SystemClock,
        ws.as_str(),
        row.issue_id.as_str(),
        &params.body,
    )
    .await
    {
        tracing::warn!(error = %e, "comment mention task spawn failed");
    }
    to_value(&row)
}

/// Dispatch `hangar/agent_update` (e38.15): edit one agent's config knobs and
/// answer with the refreshed [`ActorRow`](ainb_hangar_proto::events::ActorRow).
///
/// Mirrors [`handle_issue_update`]'s contract: the mutating handler resolves the
/// workspace and **rejects** a mistyped one with `INVALID_PARAMS` (never a silent
/// no-op), maps the wire params onto the store's partial-edit struct, then drives
/// the workspace-scoped edit. An `(agent_id, workspace)` pair that matches no row
/// (an unknown id, or an agent owned by another tenant) is rejected as a
/// not-found error — never a cross-tenant edit. This bead persists + exposes the
/// config; the provider EXEC consumption of `model`/`args` is a separate bead
/// (e38.16), so no event is pushed (the agent list is not event-driven — the
/// plugin re-pulls `agents_list` after a mutation).
/// Dispatch `hangar/agent_create`: create one agent from scratch, filling every
/// FK behind the scenes, and answer with the refreshed `agents_list` so the
/// client folds the new agent into the cache that drives its "has an agent" gate.
///
/// The daemon ensures the default workspace + owner (so the fresh-home / TUI
/// create path never rejects on a not-yet-materialised default workspace), binds
/// the single default runtime (the id the claim loop keys off, so the agent's
/// tasks actually run), and mints the id — the caller supplies only `name`
/// (+ optional `provider` / `instructions`). An empty `name` or an unsupported
/// `provider` is rejected with `INVALID_PARAMS`. The recorded provider is HONOURED
/// at dispatch (the daemon spawns that backend per task), so a `codex` agent runs
/// codex even though it binds the single `claude`-advertised runtime.
async fn handle_agent_create(
    pool: &SqlitePool,
    req: &RpcRequest,
) -> Result<serde_json::Value, RpcError> {
    let params: ainb_hangar_proto::snapshots::AgentCreateParams = parse_params(
        req,
        "{ workspace_id?, name, provider?, model?, instructions? }",
    )?;
    let name = params.name.trim();
    if name.is_empty() {
        return Err(invalid_params("agent name must not be empty"));
    }
    let provider = ainb_hangar_store::bootstrap::normalize_provider(params.provider.as_deref())
        .map_err(|e| invalid_params(&e))?;
    let wire = params.workspace_id.as_deref().unwrap_or("").trim();
    let ws = resolve_or_bootstrap_default(pool, wire).await?;
    let created = ainb_hangar_store::bootstrap::create_agent(
        pool,
        ws.as_str(),
        name,
        &provider,
        params.instructions,
    )
    .await
    .map_err(|e| store_err(&e))?;
    // Optional create-time model override (gap #9) + token budget (0042): applied
    // as a single follow-up config write rather than widening create_agent's
    // signature across every caller. A blank model is treated as absent (no
    // spurious empty-string write, so an unset model stays NULL).
    let model = params.model.as_deref().map(str::trim).filter(|s| !s.is_empty());
    if model.is_some() || params.token_budget.is_some() {
        let update = ainb_hangar_store::repo::agent::AgentConfigUpdate {
            model: model.map(|m| Some(m.to_string())),
            token_budget: params.token_budget.map(Some),
            ..Default::default()
        };
        ainb_hangar_store::repo::agent::AgentRepo::update_config(
            pool,
            ws.as_str(),
            &created.id,
            &update,
        )
        .await
        .map_err(|e| store_err(&e))?;
    }
    // Answer with the refreshed roster (the same shape agents_list returns) so
    // the plugin folds the new agent into its cached list and the squad gate clears.
    let actors = snapshots::agents_list(pool, ws.as_str(), SystemClock.now_ms())
        .await
        .map_err(|e| store_err(&e))?;
    to_value(&ainb_hangar_proto::snapshots::AgentsListResult { actors })
}

/// Dispatch `hangar/agent_delete` (Agents screen `x` remove, slice 2): delete one
/// named agent and answer with the refreshed `agents_list` so the client folds the
/// shrunk roster back into its picker cache.
///
/// Mirrors [`handle_issue_delete`]'s contract: resolve + reject a mistyped
/// workspace, then drive the workspace-scoped delete. A `(agent_id, workspace)`
/// pair that matches no row is a not-found error (never a cross-tenant delete); an
/// agent with a live task is refused with a machine-readable `active_tasks` marker
/// (so the TUI can offer "cancel the run first"); an agent still FK-pinned by run
/// history is refused with an "archive instead" message. A fresh, never-run agent
/// deletes cleanly.
async fn handle_agent_delete(
    pool: &SqlitePool,
    req: &RpcRequest,
) -> Result<serde_json::Value, RpcError> {
    use ainb_hangar_store::repo::agent::{AgentDeleteError, AgentRepo};

    let params: ainb_hangar_proto::snapshots::AgentDeleteParams =
        parse_params(req, "{ workspace_id, agent_id }")?;
    // The mutating handler must not silently no-op on a typo'd workspace.
    let ws = resolve_wire_or_reject(pool, &params.workspace_id).await?;
    AgentRepo::delete(pool, ws.as_str(), &params.agent_id)
        .await
        .map_err(|e| match e {
            AgentDeleteError::NotFound => {
                invalid_params(&format!("no agent `{}` in this workspace", params.agent_id))
            }
            // A live run blocks the delete — surface the "cancel first" message
            // tagged with a machine-readable marker (append-only `data`) so the TUI
            // can offer an inline cancel instead of dead-ending on the text.
            AgentDeleteError::ActiveTasks(n) => RpcError {
                code: INVALID_PARAMS,
                message: e.to_string(),
                data: Some(serde_json::json!({ "reason": "active_tasks", "active": n })),
            },
            // FK-pinned history: refuse rather than orphan, pointing at archive.
            AgentDeleteError::HasHistory => RpcError {
                code: INVALID_PARAMS,
                message: e.to_string(),
                data: Some(serde_json::json!({ "reason": "has_history" })),
            },
            AgentDeleteError::Db(ref db) => store_err(db),
        })?;
    // Answer with the refreshed roster (the same shape agents_list / agent_create
    // return) so the plugin folds the shrunk list into its picker cache.
    let actors = snapshots::agents_list(pool, ws.as_str(), SystemClock.now_ms())
        .await
        .map_err(|e| store_err(&e))?;
    to_value(&ainb_hangar_proto::snapshots::AgentsListResult { actors })
}

async fn handle_agent_update(
    pool: &SqlitePool,
    req: &RpcRequest,
) -> Result<serde_json::Value, RpcError> {
    let params: ainb_hangar_proto::snapshots::AgentUpdateParams = parse_params(
        req,
        "{ workspace_id, agent_id, name?, instructions?, model?, cli_args?, mcp_config?, \
         thinking?, agent_env? }",
    )?;
    // The mutating handler must not silently no-op on a typo'd workspace.
    let ws = resolve_wire_or_reject(pool, &params.workspace_id).await?;
    let update = agent_config_update_from_params(&params);
    // An edit with no field set is a client error: there is nothing to write.
    if update.is_empty() {
        return Err(invalid_params(
            "nothing to update: set at least one of \
             name/instructions/model/cli_args/mcp_config/thinking/agent_env",
        ));
    }
    let row = snapshots::agent_update(
        pool,
        ws.as_str(),
        &params.agent_id,
        &update,
        SystemClock.now_ms(),
    )
    .await
    .map_err(|e| store_err(&e))?;
    let Some(row) = row else {
        return Err(invalid_params(&format!(
            "no agent `{}` in this workspace",
            params.agent_id
        )));
    };
    to_value(&row)
}

/// Map the wire [`AgentUpdateParams`] onto the store's [`AgentConfigUpdate`].
///
/// The four nullable text fields cross the boundary via the wire [`FieldUpdate`]
/// (omitted / null / value) → the store's `Option<Option<_>>` (leave / clear /
/// set); the two JSON collection fields and `name` map straight through.
///
/// [`AgentUpdateParams`]: ainb_hangar_proto::snapshots::AgentUpdateParams
/// [`AgentConfigUpdate`]: ainb_hangar_store::repo::agent::AgentConfigUpdate
/// [`FieldUpdate`]: ainb_hangar_proto::snapshots::FieldUpdate
fn agent_config_update_from_params(
    params: &ainb_hangar_proto::snapshots::AgentUpdateParams,
) -> ainb_hangar_store::repo::agent::AgentConfigUpdate {
    ainb_hangar_store::repo::agent::AgentConfigUpdate {
        name: params.name.clone(),
        instructions: field_to_nested(&params.instructions),
        model: field_to_nested(&params.model),
        cli_args: params.cli_args.clone(),
        mcp_config: field_to_nested(&params.mcp_config),
        thinking: field_to_nested(&params.thinking),
        token_budget: field_to_nested(&params.token_budget),
        agent_env: params.agent_env.clone(),
    }
}

/// Collapse a wire three-state [`FieldUpdate`](ainb_hangar_proto::snapshots::FieldUpdate)
/// (omitted / null / value) into the store's nested-`Option` shape (leave / clear
/// / set): `Keep → None`, `Clear → Some(None)`, `Set(v) → Some(Some(v))`. Shared
/// by the four nullable agent config fields so the boundary mapping is written
/// once.
#[allow(clippy::option_option)] // the nested Option IS the store's 3-state encoding
fn field_to_nested<T: Clone>(
    fu: &ainb_hangar_proto::snapshots::FieldUpdate<T>,
) -> Option<Option<T>> {
    use ainb_hangar_proto::snapshots::FieldUpdate;
    match fu {
        FieldUpdate::Keep => None,
        FieldUpdate::Clear => Some(None),
        FieldUpdate::Set(v) => Some(Some(v.clone())),
    }
}

/// Dispatch `hangar/agent_archive` (e38.15): archive or un-archive one agent and
/// answer with the refreshed [`ActorRow`](ainb_hangar_proto::events::ActorRow).
///
/// Mirrors [`handle_agent_update`]'s contract: resolve + reject a mistyped
/// workspace, then drive the workspace-scoped flip. A `(agent_id, workspace)`
/// pair that matches no row is a not-found error, never a cross-tenant flip.
async fn handle_agent_archive(
    pool: &SqlitePool,
    req: &RpcRequest,
) -> Result<serde_json::Value, RpcError> {
    let params: ainb_hangar_proto::snapshots::AgentArchiveParams =
        parse_params(req, "{ workspace_id, agent_id, archived }")?;
    let ws = resolve_wire_or_reject(pool, &params.workspace_id).await?;
    let row = snapshots::agent_archive(
        pool,
        ws.as_str(),
        &params.agent_id,
        params.archived,
        SystemClock.now_ms(),
    )
    .await
    .map_err(|e| store_err(&e))?;
    let Some(row) = row else {
        return Err(invalid_params(&format!(
            "no agent `{}` in this workspace",
            params.agent_id
        )));
    };
    to_value(&row)
}

/// Dispatch `hangar/members_list` (e38.11): snapshot the workspace's human
/// members as a [`MembersListResult`](ainb_hangar_proto::snapshots::MembersListResult).
///
/// A read, so an unknown / foreign workspace yields an empty list (never an
/// error), mirroring [`HANGAR_AGENTS_LIST`](methods::HANGAR_AGENTS_LIST). Split
/// out of [`handle`] to keep that dispatcher within the line cap.
async fn handle_members_list(
    pool: &SqlitePool,
    req: &RpcRequest,
) -> Result<serde_json::Value, RpcError> {
    let members = match resolve(pool, req).await? {
        Some(ws) => snapshots::members_list(pool, &ws).await.map_err(|e| store_err(&e))?,
        None => Vec::new(),
    };
    to_value(&ainb_hangar_proto::snapshots::MembersListResult { members })
}

/// Dispatch `hangar/member_set_role` (e38.11): change one member's role and
/// answer with the refreshed
/// [`MembersListResult`](ainb_hangar_proto::snapshots::MembersListResult).
///
/// Mirrors [`handle_agent_update`]'s contract: the mutating handler resolves the
/// workspace and **rejects** a mistyped one with `INVALID_PARAMS` (never a silent
/// no-op), validates the role token against the closed `owner`/`admin`/`member`
/// set, then drives the workspace-scoped edit. A `(workspace, user_id)` pair that
/// matches no member is rejected as a not-found error (never a cross-tenant edit),
/// and demoting the workspace's only owner is rejected so a workspace always keeps
/// an owner. The member list is not event-driven (the settings pane re-pulls), so
/// no event is pushed.
async fn handle_member_set_role(
    pool: &SqlitePool,
    req: &RpcRequest,
) -> Result<serde_json::Value, RpcError> {
    use ainb_hangar_store::repo::member::{MemberRepo, MemberRole};

    let params: ainb_hangar_proto::snapshots::MemberSetRoleParams =
        parse_params(req, "{ workspace_id, user_id, role }")?;
    let ws = resolve_wire_or_reject(pool, &params.workspace_id).await?;
    let role = MemberRole::parse(&params.role)
        .ok_or_else(|| invalid_params("role must be one of owner/admin/member"))?;
    MemberRepo::set_role(pool, &ws, &params.user_id, role)
        .await
        .map_err(|e| member_repo_err(&e))?;
    members_list_value(pool, &ws).await
}

/// Dispatch `hangar/member_remove` (e38.11): remove one member and answer with the
/// refreshed [`MembersListResult`](ainb_hangar_proto::snapshots::MembersListResult).
///
/// Mirrors [`handle_member_set_role`]'s contract: resolve + reject a mistyped
/// workspace, then drive the workspace-scoped removal. A `(workspace, user_id)`
/// pair that matches no member is a not-found error (never a cross-tenant remove),
/// and removing the workspace's only owner is rejected.
async fn handle_member_remove(
    pool: &SqlitePool,
    req: &RpcRequest,
) -> Result<serde_json::Value, RpcError> {
    use ainb_hangar_store::repo::member::MemberRepo;

    let params: ainb_hangar_proto::snapshots::MemberRemoveParams =
        parse_params(req, "{ workspace_id, user_id }")?;
    let ws = resolve_wire_or_reject(pool, &params.workspace_id).await?;
    MemberRepo::remove(pool, &ws, &params.user_id)
        .await
        .map_err(|e| member_repo_err(&e))?;
    members_list_value(pool, &ws).await
}

/// Re-read `ws`'s members and serialize them as a
/// [`MembersListResult`](ainb_hangar_proto::snapshots::MembersListResult) wire
/// value. Shared by the two member mutations so each answers with the same
/// refreshed view the settings pane renders.
async fn members_list_value(
    pool: &SqlitePool,
    ws: &WorkspaceId,
) -> Result<serde_json::Value, RpcError> {
    let members = snapshots::members_list(pool, ws.as_str()).await.map_err(|e| store_err(&e))?;
    to_value(&ainb_hangar_proto::snapshots::MembersListResult { members })
}

/// Map a [`MemberRepoError`] onto an RPC error: a not-found / last-owner /
/// invalid-role rejection is a client error (`INVALID_PARAMS`), every store fault
/// an internal error. Mirrors [`autopilot_repo_err`].
///
/// [`MemberRepoError`]: ainb_hangar_store::repo::member::MemberRepoError
fn member_repo_err(e: &ainb_hangar_store::repo::member::MemberRepoError) -> RpcError {
    use ainb_hangar_store::repo::member::MemberRepoError;
    match e {
        MemberRepoError::NotFound => {
            invalid_params("no member with that user id in this workspace")
        }
        MemberRepoError::LastOwner => {
            invalid_params("a workspace must always keep at least one owner")
        }
        MemberRepoError::InvalidRole => invalid_params("role must be one of owner/admin/member"),
        MemberRepoError::EmptyEmail => invalid_params("email must not be empty"),
        MemberRepoError::AlreadyMember => {
            invalid_params("that user is already a member of this workspace")
        }
        MemberRepoError::Db(db) => store_err(db),
    }
}

/// Dispatch `hangar/squads_list` (e38.17): snapshot the workspace's squads (each
/// with its leader + members) as a
/// [`SquadsListResult`](ainb_hangar_proto::snapshots::SquadsListResult).
///
/// A read, so an unknown / foreign workspace yields an empty list (never an
/// error), mirroring [`handle_members_list`].
async fn handle_squads_list(
    pool: &SqlitePool,
    req: &RpcRequest,
) -> Result<serde_json::Value, RpcError> {
    let squads = match resolve(pool, req).await? {
        Some(ws) => snapshots::squads_list(pool, &ws).await.map_err(|e| store_err(&e))?,
        None => Vec::new(),
    };
    to_value(&ainb_hangar_proto::snapshots::SquadsListResult { squads })
}

/// Dispatch `hangar/squad_create` (e38.17): create one squad with a leader and
/// answer with the refreshed
/// [`SquadsListResult`](ainb_hangar_proto::snapshots::SquadsListResult).
///
/// Mirrors [`handle_member_set_role`]'s contract: the mutating handler resolves
/// the workspace and **rejects** a mistyped one with `INVALID_PARAMS` (never a
/// silent no-op), parses the leader actor-ref, mints a fresh squad id, then drives
/// the workspace-scoped insert. A name already used in the workspace is rejected
/// (the resolve-or-reject guard). The leader actor-ref is how leader-routing takes
/// effect — an `agent` leader's id becomes a squad-assigned task's `agent_id`.
async fn handle_squad_create(
    pool: &SqlitePool,
    req: &RpcRequest,
) -> Result<serde_json::Value, RpcError> {
    use ainb_hangar_core::actor::ActorRef;
    use ainb_hangar_core::idgen::{IdGen, SystemIdGen};
    use ainb_hangar_store::repo::squad::SquadRepo;
    use std::str::FromStr as _;

    let params: ainb_hangar_proto::snapshots::SquadCreateParams =
        parse_params(req, "{ workspace_id, name, leader }")?;
    // Ensure-then-resolve: a squad create against a just-booted default workspace
    // (or before the boot seed materialised it) lays it down rather than rejecting.
    let ws = resolve_or_bootstrap_default(pool, &params.workspace_id).await?;
    if params.name.trim().is_empty() {
        return Err(invalid_params("squad name must not be empty"));
    }
    let leader = ActorRef::from_str(&params.leader).map_err(|e| {
        invalid_params(&format!(
            "leader must be `agent:<id>` or `member:<id>`: {e}"
        ))
    })?;
    let id = SystemIdGen.new_ulid();
    SquadRepo::create(pool, &ws, &id, &params.name, &leader, SystemClock.now_ms())
        .await
        .map_err(|e| squad_repo_err(&e))?;
    squads_list_value(pool, &ws).await
}

/// Dispatch `hangar/squad_member_add` (`add = true`) and
/// `hangar/squad_member_remove` (`add = false`) (e38.17): mutate one squad's
/// membership and answer with the refreshed
/// [`SquadsListResult`](ainb_hangar_proto::snapshots::SquadsListResult).
///
/// Mirrors [`handle_squad_create`]'s contract: resolve + reject a mistyped
/// workspace, parse the member actor-ref, then drive the workspace-scoped
/// mutation. A `(workspace, squad_id)` pair that matches no squad is rejected as a
/// not-found error (never a cross-tenant edit). Add is idempotent; remove of an
/// absent member is a no-op.
async fn handle_squad_member(
    pool: &SqlitePool,
    req: &RpcRequest,
    add: bool,
) -> Result<serde_json::Value, RpcError> {
    use ainb_hangar_core::actor::ActorRef;
    use ainb_hangar_store::repo::squad::SquadRepo;
    use std::str::FromStr as _;

    let params: ainb_hangar_proto::snapshots::SquadMemberParams =
        parse_params(req, "{ workspace_id, squad_id, member }")?;
    let ws = resolve_wire_or_reject(pool, &params.workspace_id).await?;
    let member = ActorRef::from_str(&params.member).map_err(|e| {
        invalid_params(&format!(
            "member must be `agent:<id>` or `member:<id>`: {e}"
        ))
    })?;
    let outcome = if add {
        SquadRepo::add_member(pool, &ws, &params.squad_id, &member).await
    } else {
        SquadRepo::remove_member(pool, &ws, &params.squad_id, &member).await
    };
    outcome.map_err(|e| squad_repo_err(&e))?;
    squads_list_value(pool, &ws).await
}

/// Re-read `ws`'s squads and serialize them as a
/// [`SquadsListResult`](ainb_hangar_proto::snapshots::SquadsListResult) wire
/// value. Shared by the three squad mutations so each answers with the same
/// refreshed view the status view renders.
async fn squads_list_value(
    pool: &SqlitePool,
    ws: &WorkspaceId,
) -> Result<serde_json::Value, RpcError> {
    let squads = snapshots::squads_list(pool, ws.as_str()).await.map_err(|e| store_err(&e))?;
    to_value(&ainb_hangar_proto::snapshots::SquadsListResult { squads })
}

/// The run generation to stamp on a standalone squad assign / fan-out (migration
/// 0039, tcp 8ln): a fresh assign onto an issue is a new run epoch, so mint the
/// issue's NEXT generation; an ad-hoc (issueless) assign carries `0`, since no card
/// aggregate ever reads its rows. Bumping here keeps a repeated squad-screen assign
/// on the same issue from folding a prior run's terminal rows into the current one.
///
/// Unlike [`run_card`] (which mints under the per-card launch slot + the
/// one-active-run guard), this legacy path has no such guard: two assigns racing on
/// one issue in the same instant could stamp the SAME generation and fold together
/// as one run. Tolerated — the per-(issue, agent) pending-unique index caps
/// duplicate dispatch, and the board Run path never routes through here.
async fn squad_assign_generation(
    pool: &SqlitePool,
    issue_id: Option<&str>,
) -> Result<i64, RpcError> {
    match issue_id {
        Some(issue_id) => {
            ainb_hangar_store::repo::task::TaskRepo::next_generation_for_issue(pool, issue_id)
                .await
                .map_err(|e| store_err(&e))
        }
        None => Ok(0),
    }
}

/// Dispatch `hangar/squad_assign` (e38.17): route a task to the squad's LEADER,
/// the product seam that makes leader routing TAKE EFFECT.
///
/// Mirrors [`handle_squad_create`]'s contract: resolve + reject a mistyped
/// workspace, then call [`SquadAssignService::assign_to_leader`], which resolves
/// the squad's leader agent, derives the leader's runtime, and enqueues a task
/// keyed to the leader so the existing claim/dispatch path routes it there. A
/// squad with a human-member leader (no agent to dispatch to) or an unknown squad
/// is rejected (`INVALID_PARAMS`). Answers with the enqueued task id + the leader
/// identity it routed to.
///
/// [`SquadAssignService::assign_to_leader`]: ainb_hangar_store::service::squad_assign::SquadAssignService::assign_to_leader
async fn handle_squad_assign(
    pool: &SqlitePool,
    req: &RpcRequest,
) -> Result<serde_json::Value, RpcError> {
    use ainb_hangar_core::idgen::SystemIdGen;
    use ainb_hangar_store::service::squad_assign::{
        SquadAssignRequest, SquadAssignService, SquadAssignment,
    };

    let params: ainb_hangar_proto::snapshots::SquadAssignParams = parse_params(
        req,
        "{ workspace_id, squad_id, issue_id?, work_dir?, priority? }",
    )?;
    let ws = resolve_wire_or_reject(pool, &params.workspace_id).await?;
    let generation = squad_assign_generation(pool, params.issue_id.as_deref()).await?;
    let request = SquadAssignRequest {
        issue_id: params.issue_id.as_deref(),
        work_dir: params.work_dir.as_deref(),
        priority: params.priority.unwrap_or(0),
        generation,
        ..SquadAssignRequest::default()
    };
    let SquadAssignment {
        task_id,
        leader_agent_id,
        runtime_id,
    } = SquadAssignService::assign_to_leader(
        pool,
        &ws,
        &params.squad_id,
        &request,
        &SystemIdGen,
        &SystemClock,
    )
    .await
    .map_err(|e| squad_assign_err(&e))?;
    to_value(&ainb_hangar_proto::snapshots::SquadAssignResult {
        task_id,
        leader_agent_id,
        runtime_id,
    })
}

/// Dispatch `hangar/squad_fanout` (P7): fan an issue out across the WHOLE squad —
/// brief the LEADER *and* enqueue one task per distinct `agent` member, all on the
/// same issue.
///
/// Mirrors [`handle_squad_assign`]'s contract (same params, same workspace
/// resolve-or-reject, same human-leader / unknown-squad rejection), but calls
/// [`SquadAssignService::assign_fanout`], which additionally resolves the squad's
/// `agent` members and enqueues a task per member keyed to that member's runtime.
/// The per-(issue, agent) claim guard (migration `0012`) lets the leader and every
/// member hold their own pending task on the one issue. Answers with the leader's
/// brief task plus one dispatch per fanned-out member.
///
/// [`SquadAssignService::assign_fanout`]: ainb_hangar_store::service::squad_assign::SquadAssignService::assign_fanout
async fn handle_squad_fanout(
    pool: &SqlitePool,
    req: &RpcRequest,
) -> Result<serde_json::Value, RpcError> {
    use ainb_hangar_core::idgen::SystemIdGen;
    use ainb_hangar_store::service::squad_assign::{
        SquadAssignRequest, SquadAssignService, SquadFanout,
    };

    let params: ainb_hangar_proto::snapshots::SquadAssignParams = parse_params(
        req,
        "{ workspace_id, squad_id, issue_id?, work_dir?, priority? }",
    )?;
    let ws = resolve_wire_or_reject(pool, &params.workspace_id).await?;
    let generation = squad_assign_generation(pool, params.issue_id.as_deref()).await?;
    let request = SquadAssignRequest {
        issue_id: params.issue_id.as_deref(),
        work_dir: params.work_dir.as_deref(),
        priority: params.priority.unwrap_or(0),
        generation,
        ..SquadAssignRequest::default()
    };
    let SquadFanout { leader, members } = SquadAssignService::assign_fanout(
        pool,
        &ws,
        &params.squad_id,
        &request,
        &SystemIdGen,
        &SystemClock,
    )
    .await
    .map_err(|e| squad_assign_err(&e))?;
    to_value(&ainb_hangar_proto::snapshots::SquadFanoutResult {
        leader: ainb_hangar_proto::snapshots::SquadAssignResult {
            task_id: leader.task_id,
            leader_agent_id: leader.leader_agent_id,
            runtime_id: leader.runtime_id,
        },
        members: members
            .into_iter()
            .map(|m| ainb_hangar_proto::snapshots::SquadMemberDispatchRow {
                task_id: m.task_id,
                agent_id: m.agent_id,
                runtime_id: m.runtime_id,
            })
            .collect(),
    })
}

/// Map a [`SquadAssignError`] onto an RPC error: a no-agent-leader / missing-leader
/// rejection is a client error (`INVALID_PARAMS`), every store fault an internal
/// error.
///
/// [`SquadAssignError`]: ainb_hangar_store::service::squad_assign::SquadAssignError
fn squad_assign_err(e: &ainb_hangar_store::service::squad_assign::SquadAssignError) -> RpcError {
    use ainb_hangar_store::service::squad_assign::SquadAssignError;
    match e {
        SquadAssignError::NoAgentLeader => invalid_params(
            "squad has no agent leader to route to (unknown squad or a human leader)",
        ),
        SquadAssignError::LeaderAgentMissing(id) => {
            invalid_params(&format!("squad leader agent `{id}` not found"))
        }
        SquadAssignError::MemberAgentMissing(id) => {
            invalid_params(&format!("squad member agent `{id}` not found"))
        }
        SquadAssignError::Db(db) => store_err(db),
    }
}

/// Map a [`SquadRepoError`] onto an RPC error: a duplicate-name / not-found
/// rejection is a client error (`INVALID_PARAMS`), every store fault an internal
/// error. Mirrors [`member_repo_err`].
///
/// [`SquadRepoError`]: ainb_hangar_store::repo::squad::SquadRepoError
fn squad_repo_err(e: &ainb_hangar_store::repo::squad::SquadRepoError) -> RpcError {
    use ainb_hangar_store::repo::squad::SquadRepoError;
    match e {
        SquadRepoError::DuplicateName => {
            invalid_params("a squad with that name already exists in this workspace")
        }
        SquadRepoError::NotFound => invalid_params("no squad with that id in this workspace"),
        SquadRepoError::Db(db) => store_err(db),
    }
}

// ---------------------------------------------------------------------------
// P4 — user-defined kanban boards (D8).
// ---------------------------------------------------------------------------

/// The task-FSM status tokens a column's `fsm_state` may map to. A non-empty
/// `fsm_state` outside this set is rejected (a typo would silently never match).
const KNOWN_FSM_STATES: &[&str] = &[
    "queued",
    "dispatched",
    "running",
    "done",
    "failed",
    "cancelled",
];

/// Dispatch `hangar/boards_list` (P4): snapshot the workspace's boards. A read,
/// so an unknown workspace answers an empty list rather than an error.
async fn handle_boards_list(
    pool: &SqlitePool,
    req: &RpcRequest,
) -> Result<serde_json::Value, RpcError> {
    let boards = match resolve(pool, req).await? {
        Some(ws) => snapshots::boards_list(pool, &ws).await.map_err(|e| store_err(&e))?,
        None => Vec::new(),
    };
    to_value(&ainb_hangar_proto::snapshots::BoardsListResult { boards })
}

/// Dispatch `hangar/board_create` (P4): create one empty board, then answer with
/// the refreshed board list. Rejects a blank name and a duplicate (the
/// resolve-or-reject `(workspace, name)` guard).
async fn handle_board_create(
    pool: &SqlitePool,
    req: &RpcRequest,
) -> Result<serde_json::Value, RpcError> {
    use ainb_hangar_core::idgen::{IdGen, SystemIdGen};
    use ainb_hangar_store::repo::board::BoardRepo;

    let params: ainb_hangar_proto::snapshots::BoardCreateParams =
        parse_params(req, "{ workspace_id, name }")?;
    let ws = resolve_wire_or_reject(pool, &params.workspace_id).await?;
    if params.name.trim().is_empty() {
        return Err(invalid_params("board name must not be empty"));
    }
    let id = SystemIdGen.new_ulid();
    BoardRepo::create(pool, &ws, &id, &params.name, SystemClock.now_ms())
        .await
        .map_err(|e| board_repo_err(&e))?;
    boards_list_value(pool, &ws).await
}

/// Dispatch `hangar/board_update` (P4): rename a board and/or flip its auto-move
/// master toggle. A rename to a blank name is rejected.
async fn handle_board_update(
    pool: &SqlitePool,
    req: &RpcRequest,
) -> Result<serde_json::Value, RpcError> {
    use ainb_hangar_store::repo::board::BoardRepo;

    let params: ainb_hangar_proto::snapshots::BoardUpdateParams =
        parse_params(req, "{ workspace_id, board_id, name?, auto_move? }")?;
    let ws = resolve_wire_or_reject(pool, &params.workspace_id).await?;
    if let Some(n) = params.name.as_deref() {
        if n.trim().is_empty() {
            return Err(invalid_params("board name must not be empty"));
        }
    }
    BoardRepo::update(
        pool,
        &ws,
        &params.board_id,
        params.name.as_deref(),
        params.auto_move,
    )
    .await
    .map_err(|e| board_repo_err(&e))?;
    boards_list_value(pool, &ws).await
}

/// Dispatch `hangar/board_delete` (P4): delete a board with its columns + cards.
async fn handle_board_delete(
    pool: &SqlitePool,
    req: &RpcRequest,
) -> Result<serde_json::Value, RpcError> {
    use ainb_hangar_store::repo::board::BoardRepo;

    let params: ainb_hangar_proto::snapshots::BoardIdParams =
        parse_params(req, "{ workspace_id, board_id }")?;
    let ws = resolve_wire_or_reject(pool, &params.workspace_id).await?;
    BoardRepo::delete(pool, &ws, &params.board_id)
        .await
        .map_err(|e| board_repo_err(&e))?;
    boards_list_value(pool, &ws).await
}

/// Dispatch `hangar/board_column_add` (P4): append a column. Validates the
/// `fsm_state` token (when present + non-empty) so a typo cannot yield a column
/// that never matches an auto-move.
async fn handle_board_column_add(
    pool: &SqlitePool,
    req: &RpcRequest,
) -> Result<serde_json::Value, RpcError> {
    use ainb_hangar_core::idgen::{IdGen, SystemIdGen};
    use ainb_hangar_store::repo::board::BoardRepo;

    let params: ainb_hangar_proto::snapshots::BoardColumnAddParams = parse_params(
        req,
        "{ workspace_id, board_id, name, fsm_state?, auto_move? }",
    )?;
    let ws = resolve_wire_or_reject(pool, &params.workspace_id).await?;
    if params.name.trim().is_empty() {
        return Err(invalid_params("column name must not be empty"));
    }
    // A blank fsm_state means "manual column"; a non-blank one must be a known
    // task status.
    let fsm_state = normalise_fsm_state(params.fsm_state.as_deref())?;
    let id = SystemIdGen.new_ulid();
    BoardRepo::column_add(
        pool,
        &ws,
        &params.board_id,
        &id,
        &params.name,
        fsm_state,
        params.auto_move.unwrap_or(false),
    )
    .await
    .map_err(|e| board_repo_err(&e))?;
    boards_list_value(pool, &ws).await
}

/// Dispatch `hangar/board_column_update` (P4): rename / re-map / retune a column.
/// `fsm_state` is tri-state: omitted leaves the mapping, empty clears it, a token
/// sets it (validated).
async fn handle_board_column_update(
    pool: &SqlitePool,
    req: &RpcRequest,
) -> Result<serde_json::Value, RpcError> {
    use ainb_hangar_store::repo::board::BoardRepo;

    let params: ainb_hangar_proto::snapshots::BoardColumnUpdateParams = parse_params(
        req,
        "{ workspace_id, board_id, column_id, name?, fsm_state?, auto_move? }",
    )?;
    let ws = resolve_wire_or_reject(pool, &params.workspace_id).await?;
    if let Some(n) = params.name.as_deref() {
        if n.trim().is_empty() {
            return Err(invalid_params("column name must not be empty"));
        }
    }
    // Map the wire Option<String> onto the repo's Option<Option<&str>>:
    // None => leave unchanged; Some("") => clear to a manual column; Some(tok) =>
    // set (validated).
    let fsm_state = match params.fsm_state.as_deref() {
        None => None,
        Some("") => Some(None),
        Some(tok) => {
            if !KNOWN_FSM_STATES.contains(&tok) {
                return Err(invalid_params(&format!(
                    "fsm_state `{tok}` is not a task status ({})",
                    KNOWN_FSM_STATES.join(", ")
                )));
            }
            Some(Some(tok))
        }
    };
    BoardRepo::column_update(
        pool,
        &ws,
        &params.board_id,
        &params.column_id,
        params.name.as_deref(),
        fsm_state,
        params.auto_move,
    )
    .await
    .map_err(|e| board_repo_err(&e))?;
    boards_list_value(pool, &ws).await
}

/// Dispatch `hangar/board_column_delete` (P4): delete a column (cards park
/// unmapped, remaining columns renumber).
async fn handle_board_column_delete(
    pool: &SqlitePool,
    req: &RpcRequest,
) -> Result<serde_json::Value, RpcError> {
    use ainb_hangar_store::repo::board::BoardRepo;

    let params: ainb_hangar_proto::snapshots::BoardColumnDeleteParams =
        parse_params(req, "{ workspace_id, board_id, column_id }")?;
    let ws = resolve_wire_or_reject(pool, &params.workspace_id).await?;
    BoardRepo::column_delete(pool, &ws, &params.board_id, &params.column_id)
        .await
        .map_err(|e| board_repo_err(&e))?;
    boards_list_value(pool, &ws).await
}

/// Dispatch `hangar/board_column_reorder` (P4): set a board's column order. The
/// id list must be exactly the board's current columns (a permutation).
async fn handle_board_column_reorder(
    pool: &SqlitePool,
    req: &RpcRequest,
) -> Result<serde_json::Value, RpcError> {
    use ainb_hangar_store::repo::board::BoardRepo;

    let params: ainb_hangar_proto::snapshots::BoardColumnReorderParams =
        parse_params(req, "{ workspace_id, board_id, column_ids }")?;
    let ws = resolve_wire_or_reject(pool, &params.workspace_id).await?;
    BoardRepo::column_reorder(pool, &ws, &params.board_id, &params.column_ids)
        .await
        .map_err(|e| board_repo_err(&e))?;
    boards_list_value(pool, &ws).await
}

/// Dispatch `hangar/board_card_add` (`add = true`) and `hangar/board_card_move`
/// (`add = false`) (P4): place / move an issue card on a board.
async fn handle_board_card(
    pool: &SqlitePool,
    req: &RpcRequest,
    add: bool,
) -> Result<serde_json::Value, RpcError> {
    use ainb_hangar_store::repo::board::BoardRepo;

    let params: ainb_hangar_proto::snapshots::BoardCardParams =
        parse_params(req, "{ workspace_id, board_id, issue_id, column_id? }")?;
    let ws = resolve_wire_or_reject(pool, &params.workspace_id).await?;
    if params.issue_id.trim().is_empty() {
        return Err(invalid_params("issue_id must not be empty"));
    }
    if add {
        BoardRepo::card_add(
            pool,
            &ws,
            &params.board_id,
            &params.issue_id,
            params.column_id.as_deref(),
            SystemClock.now_ms(),
        )
        .await
        .map_err(|e| board_repo_err(&e))?;
    } else {
        BoardRepo::card_move(
            pool,
            &ws,
            &params.board_id,
            &params.issue_id,
            params.column_id.as_deref(),
        )
        .await
        .map_err(|e| board_repo_err(&e))?;
    }
    boards_list_value(pool, &ws).await
}

/// Validate an optional column `fsm_state` for the ADD path: `None` / `Some("")`
/// both mean "manual column" (`None` stored); a non-blank token must be known.
fn normalise_fsm_state<'a>(raw: Option<&'a str>) -> Result<Option<&'a str>, RpcError> {
    match raw {
        None | Some("") => Ok(None),
        Some(tok) => {
            if KNOWN_FSM_STATES.contains(&tok) {
                Ok(Some(tok))
            } else {
                Err(invalid_params(&format!(
                    "fsm_state `{tok}` is not a task status ({})",
                    KNOWN_FSM_STATES.join(", ")
                )))
            }
        }
    }
}

/// `hangar/board_card_create` (ccc / D8, D16): create an issue from a card and
/// place it on a board in one atomic round-trip.
///
/// Creates a fresh `open` issue titled `title`, assigns it to the agent named for
/// `assignee_profile` (D16: the board-assignee slug is the profile slug) when one
/// resolves in the workspace — else leaves it unassigned — then places the card in
/// `column_id` (omit for unmapped). The creator is the TUI author (`member:me`,
/// mirroring the plugin's `SELF_AUTHOR_REF`). Answers with the refreshed
/// `BoardsListResult`, exactly like every other `board_*` mutation.
async fn handle_board_card_create(
    pool: &SqlitePool,
    req: &RpcRequest,
) -> Result<serde_json::Value, RpcError> {
    use ainb_hangar_core::actor::{ActorKind, ActorRef};
    use ainb_hangar_core::idgen::{IdGen, SystemIdGen};
    use ainb_hangar_store::repo::board::BoardRepo;
    use ainb_hangar_store::repo::issue::{IssueRepo, NewIssue};

    let params: ainb_hangar_proto::snapshots::BoardCardCreateParams = parse_params(
        req,
        "{ workspace_id, board_id, column_id?, title, assignee_profile? }",
    )?;
    let ws = resolve_wire_or_reject(pool, &params.workspace_id).await?;
    if params.title.trim().is_empty() {
        return Err(invalid_params("card title must not be empty"));
    }

    // D16: the assignee profile slug names the agent that runs the card. Resolve
    // it to an in-workspace agent so the later `board_card_run` routes to a real
    // runtime; an unresolved profile leaves the issue unassigned (the run then
    // falls back to the workspace's agent).
    let assignee = match params.assignee_profile.as_deref().map(str::trim) {
        Some(slug) if !slug.is_empty() => resolve_agent_by_name(pool, &ws, slug)
            .await?
            .map(|agent| ActorRef::new(ActorKind::Agent, agent.id))
            .transpose()
            .map_err(|e| internal(&format!("build assignee ref: {e}")))?,
        _ => None,
    };

    // Prevalidate the placement target BEFORE creating the issue so a bad board /
    // column rejects up front and never strands an orphan issue (the create is an
    // atomic round-trip: nothing persists unless the card can be placed).
    let board = board_in_ws(pool, &ws, &params.board_id).await?;
    if let Some(col) = params.column_id.as_deref() {
        if !board.columns.iter().any(|c| c.id == col) {
            return Err(invalid_params("no column with that id on this board"));
        }
    }

    // The TUI user owns cards it creates — mirror the plugin's `SELF_AUTHOR_REF`.
    let creator = ActorRef::new(ActorKind::Member, "me")
        .map_err(|e| internal(&format!("build creator ref: {e}")))?;
    let issue_id = SystemIdGen.new_ulid();
    IssueRepo::insert(
        pool,
        &NewIssue {
            id: issue_id.clone(),
            workspace_id: ws.as_str().to_string(),
            title: params.title.clone(),
            description: None,
            state: "open".to_string(),
            assignee,
            creator,
            created_at: SystemClock.now_ms(),
            priority: 0,
            due_date: None,
            labels: Vec::new(),
            acceptance_criteria: Vec::new(),
            context_refs: Vec::new(),
            parent_issue_id: None,
            stage: None,
        },
    )
    .await
    .map_err(|e| store_err(&e))?;

    BoardRepo::card_add(
        pool,
        &ws,
        &params.board_id,
        &issue_id,
        params.column_id.as_deref(),
        SystemClock.now_ms(),
    )
    .await
    .map_err(|e| board_repo_err(&e))?;

    // F2/F3/F4: persist the card's repo + chosen agent onto the durable card
    // (the issue) so a later run / rerun / reload provisions the right worktree
    // and provider. Both are optional at create — the run enforces "repo
    // required" (F2) and resolves the agent via the F4 cascade when unset. An
    // unrecognised agent token is dropped (cascade decides), never a reject.
    let repo_ref_raw = params.repo_ref.as_deref().map(str::trim).filter(|s| !s.is_empty());
    // bead pv8: a remote-only favorite pick arrives as its REMOTE indicator (not
    // an absolute path, not `scratch`). Resolve it to a LOCAL clone path here so
    // the run/provision path — which only understands a path or `scratch` — never
    // sees a bare remote. A path / `scratch` passes through untouched.
    let resolved_repo_ref = match repo_ref_raw {
        Some(r) => {
            let ainb_dir = ainb_hangar_core::hangar_home()
                .ok_or_else(|| internal("cannot resolve hangar home to clone a remote favorite"))?;
            Some(resolve_card_repo_ref(&ainb_dir, r).await?)
        }
        None => None,
    };
    let agent = params.agent.as_deref().and_then(ainb_hangar_core::agent_kind::AgentKind::parse);
    if resolved_repo_ref.is_some() || agent.is_some() {
        ainb_hangar_store::repo::card_parity::CardParityRepo::set_issue_repo_agent(
            pool,
            ws.as_str(),
            &issue_id,
            resolved_repo_ref.as_deref(),
            agent,
        )
        .await
        .map_err(|e| store_err(&e))?;
    }
    boards_list_value(pool, &ws).await
}

/// Resolve a card's picked `repo_ref` to a value the run / provision path accepts
/// — an absolute checkout path or `scratch` — cloning a remote-only favorite's
/// REMOTE indicator into the managed clones dir along the way (bead pv8).
///
/// `scratch` and an absolute path (`/…`) pass through unchanged. Anything else is
/// a remote indicator (`owner/repo`, an `https://` / `file://` URL): it is cloned
/// ONCE — idempotently, reusing an existing clone — into
/// `<hangar_home>/clones/<dir>` via [`ainb_fleet_core::repo_clone::ensure_clone`],
/// and its local path is returned. The blocking `git clone` runs on a blocking
/// thread so it never stalls the async runtime.
///
/// A clone failure is surfaced as an error (the card is NOT created; the user
/// retries) rather than persisting an unprovisionable remote that the provision
/// path would mistake for a path and loop on.
///
/// NOTE (interim): the clone is synchronous within card-create, so the FIRST
/// card on a new remote blocks until the clone finishes (subsequent picks reuse
/// instantly). The async-with-inbox-note refinement (card created immediately,
/// clone in the background) is deferred — it needs a run-path guard for an
/// unresolved remote, which a sibling owns.
async fn resolve_card_repo_ref(ainb_dir: &Path, repo_ref: &str) -> Result<String, RpcError> {
    // Already a value the provision path understands.
    if repo_ref == "scratch" || repo_ref.starts_with('/') {
        return Ok(repo_ref.to_string());
    }
    // A remote-only favorite: clone into the managed dir, persist the local path.
    let ainb_dir = ainb_dir.to_path_buf();
    let remote = repo_ref.to_string();
    let path = tokio::task::spawn_blocking(move || {
        ainb_fleet_core::repo_clone::ensure_clone(&ainb_dir, &remote)
    })
    .await
    .map_err(|e| internal(&format!("clone task failed to join: {e}")))?
    .map_err(|e| {
        internal(&format!(
            "clone of remote favorite {repo_ref:?} failed: {e}"
        ))
    })?;
    path.into_os_string()
        .into_string()
        .map_err(|_| internal("cloned repo path is not valid UTF-8"))
}

/// `hangar/board_card_run` (ccc / D6, D16): launch a card's issue on its assignee
/// profile now.
///
/// Enqueues one `agent_task_queue` row for the card's issue keyed to the assignee
/// agent's `(agent_id, runtime_id)` — the same claim/dispatch path a squad
/// assignment rides ([`SquadAssignService`]) — so the claim loop runs it and the
/// D8 auto-move hook slides the card on each FSM transition. The agent resolves
/// from the issue's assignee (D16), falling back to the workspace's agent so a
/// card always runs. `mode` (`headless` / `interactive`, D6 `Run ▾`) is validated
/// and echoed; a single-agent card honours either, but a SQUAD card is a headless
/// batch and REJECTS `interactive` (so the echoed mode is never a lie about the run).
///
/// [`SquadAssignService`]: ainb_hangar_store::service::squad_assign::SquadAssignService
async fn handle_board_card_run(
    pool: &SqlitePool,
    req: &RpcRequest,
) -> Result<serde_json::Value, RpcError> {
    use ainb_hangar_core::agent_kind::AgentKind;
    use ainb_hangar_store::repo::issue::IssueRepo;

    let params: ainb_hangar_proto::snapshots::BoardCardRunParams =
        parse_params(req, "{ workspace_id, board_id, issue_id, mode }")?;
    let ws = resolve_wire_or_reject(pool, &params.workspace_id).await?;
    let mode = match params.mode.trim() {
        "" | "headless" => "headless",
        "interactive" => "interactive",
        other => {
            return Err(invalid_params(&format!(
                "mode must be `headless` or `interactive`, got `{other}`"
            )));
        }
    };

    // The issue must be a real CARD on this board (not merely any workspace issue)
    // — the run is a card affordance, so a non-card / foreign-board issue id is
    // rejected rather than silently enqueued.
    let board = board_in_ws(pool, &ws, &params.board_id).await?;
    if !board.cards.iter().any(|c| c.issue_id == params.issue_id) {
        return Err(invalid_params("that issue is not a card on this board"));
    }

    // The card's issue must exist in this workspace (a tenant guard + a real card).
    let issue = IssueRepo::get_by_id(pool, &params.issue_id)
        .await
        .map_err(|e| store_err(&e))?
        .filter(|i| i.workspace_id == ws.as_str())
        .ok_or_else(|| invalid_params("no issue with that id in this workspace"))?;

    // The shared launch core (refuse-run guard → squad fan-out vs single enqueue)
    // runs the card; the finalize auto-run seam calls the SAME `run_card`. Thread
    // the run-time repo/agent overrides (spec F4/F5) from the request.
    let run_override = params.repo_ref.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let agent_override = params.agent.as_deref().and_then(AgentKind::parse);
    let outcome = run_card(
        pool,
        &ws,
        Some(&params.board_id),
        &issue,
        mode,
        run_override,
        agent_override,
        params.source_branch.as_deref().map(str::trim).filter(|s| !s.is_empty()),
        None, // a board card runs under the card's own assignee (no wizard override)
        None, // owner-invoked (the local TUI operator); the gate admits the owner
    )
    .await
    .map_err(card_run_err)?;

    let result = match outcome {
        CardRunOutcome::Single {
            task_id,
            agent_id,
            runtime_id,
        } => ainb_hangar_proto::snapshots::BoardCardRunResult {
            task_id,
            agent_id,
            runtime_id,
            mode: mode.to_string(),
            member_task_ids: Vec::new(),
        },
        CardRunOutcome::Squad {
            leader_task_id,
            leader_agent_id,
            leader_runtime_id,
            member_task_ids,
        } => ainb_hangar_proto::snapshots::BoardCardRunResult {
            task_id: leader_task_id,
            agent_id: leader_agent_id,
            runtime_id: leader_runtime_id,
            mode: mode.to_string(),
            member_task_ids,
        },
    };
    to_value(&result)
}

/// `hangar/issue_run`: enqueue a run of one issue WITHOUT a board (the Issues
/// create-wizard dispatch; plans/hangar-task-agent-model.md).
///
/// The board-less sibling of [`handle_board_card_run`]: same mode validation,
/// same tenant guard, the SAME [`run_card`] launch core (refuse-run guard →
/// squad fan-out vs single enqueue, repo REQUIRED, F4 cascade with the board
/// tier skipped via `board_id = None`, 0042 source-branch resolve) — minus the
/// board-membership check, so an Issues-screen task needs no user board to
/// exist. Answers the same [`BoardCardRunResult`] shape.
///
/// [`BoardCardRunResult`]: ainb_hangar_proto::snapshots::BoardCardRunResult
async fn handle_issue_run(
    pool: &SqlitePool,
    req: &RpcRequest,
) -> Result<serde_json::Value, RpcError> {
    use ainb_hangar_core::agent_kind::AgentKind;
    use ainb_hangar_store::repo::issue::IssueRepo;

    let params: ainb_hangar_proto::snapshots::IssueRunParams =
        parse_params(req, "{ workspace_id, issue_id, mode }")?;
    let ws = resolve_wire_or_reject(pool, &params.workspace_id).await?;
    let mode = match params.mode.trim() {
        "" | "headless" => "headless",
        "interactive" => "interactive",
        other => {
            return Err(invalid_params(&format!(
                "mode must be `headless` or `interactive`, got `{other}`"
            )));
        }
    };

    // Tenant guard: the issue must exist in this workspace.
    let issue = IssueRepo::get_by_id(pool, &params.issue_id)
        .await
        .map_err(|e| store_err(&e))?
        .filter(|i| i.workspace_id == ws.as_str())
        .ok_or_else(|| invalid_params("no issue with that id in this workspace"))?;

    // Brief-or-link required (0043): an Issues-screen dispatch of an issue with
    // NEITHER a non-empty description NOR an upstream link would fall to the
    // useless one-word/FALLBACK prompt (the title alone is not a brief). Refuse at
    // the point it matters — create stays unblocked so title-only backlog stubs
    // are fine. Scoped to this path (not the shared `run_card`) because a Kanban
    // board card is created title-only through a wizard with no brief field.
    let has_brief = issue.description.as_deref().is_some_and(|d| !d.trim().is_empty());
    let has_link = issue.external_ref.as_deref().is_some_and(|e| !e.trim().is_empty());
    if !has_brief && !has_link {
        return Err(invalid_params(
            "add a brief or link an issue before running — an empty card would run a useless prompt",
        ));
    }

    let run_override_raw = params.repo_ref.as_deref().map(str::trim).filter(|s| !s.is_empty());
    // bead pv8 parity: resolve a remote-only favorite (`owner/repo`, a URL) to a
    // LOCAL clone path before dispatch, exactly as `board_card_create` /
    // `handle_issue_update` do — the run/provision path only understands a path or
    // `scratch`. Idempotent: a pre-resolved path (from the card edit above or the
    // board path) double-passes harmlessly; a path / `scratch` is untouched.
    let run_override_owned = match run_override_raw {
        Some(r) => {
            let ainb_dir = ainb_hangar_core::hangar_home()
                .ok_or_else(|| internal("cannot resolve hangar home to clone a remote favorite"))?;
            Some(resolve_card_repo_ref(&ainb_dir, r).await?)
        }
        None => None,
    };
    let run_override = run_override_owned.as_deref();
    let agent_override = params.agent.as_deref().and_then(AgentKind::parse);
    let source_override = params.source_branch.as_deref().map(str::trim).filter(|s| !s.is_empty());
    // V3-F3: a run-time assignee override names the NAMED workspace agent the run
    // dispatches under. A malformed ref is dropped (the run then resolves the
    // agent from the issue's persisted assignee) rather than failing the run — the
    // wire stays forward-compatible, matching the `agent`-token drop above.
    let assignee_override = params
        .assignee
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .and_then(|s| s.parse::<ainb_hangar_core::actor::ActorRef>().ok());
    // gap #8: an optional invoker identity. Omitted (`None`) defaults to the
    // workspace owner inside `run_card` — the ordinary single-operator Run, which
    // the gate always admits. A multi-user caller (or a test) can name a non-owner
    // member here to be gated against the agent's allow-list.
    let invoker = params.invoker_user_id.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let outcome = run_card(
        pool,
        &ws,
        None, // board-less: the F4 board tier is skipped
        &issue,
        mode,
        run_override,
        agent_override,
        source_override,
        assignee_override.as_ref(),
        invoker,
    )
    .await
    .map_err(card_run_err)?;

    let result = match outcome {
        CardRunOutcome::Single {
            task_id,
            agent_id,
            runtime_id,
        } => ainb_hangar_proto::snapshots::BoardCardRunResult {
            task_id,
            agent_id,
            runtime_id,
            mode: mode.to_string(),
            member_task_ids: Vec::new(),
        },
        CardRunOutcome::Squad {
            leader_task_id,
            leader_agent_id,
            leader_runtime_id,
            member_task_ids,
        } => ainb_hangar_proto::snapshots::BoardCardRunResult {
            task_id: leader_task_id,
            agent_id: leader_agent_id,
            runtime_id: leader_runtime_id,
            mode: mode.to_string(),
            member_task_ids,
        },
    };
    to_value(&result)
}

/// The outcome of launching a card: either a single-agent task or a squad fan-out
/// (the leader brief + the member task ids). Shared by the `board_card_run` RPC
/// handler and the finalize auto-run seam.
pub(crate) enum CardRunOutcome {
    Single {
        task_id: String,
        agent_id: String,
        runtime_id: String,
    },
    Squad {
        leader_task_id: String,
        leader_agent_id: String,
        leader_runtime_id: String,
        member_task_ids: Vec<String>,
    },
}

/// Why a card could not be launched. The RPC handler maps each to an
/// `INVALID_PARAMS` (client-visible) or internal error; the auto-run seam treats
/// `Blocked` / `ActiveRun` as benign no-ops (log-and-skip) since they mean the card
/// is not launchable right now, not that anything is wrong.
pub(crate) enum CardRunError {
    /// The card has unfinished blockers (their display ids) — F7 refuse-run.
    Blocked(Vec<String>),
    /// The card already has an active run (its status).
    ActiveRun(String),
    /// The card has no repo to run in (F2).
    NoRepo,
    /// The resolved provider is not yet dispatchable (F8: copilot).
    NotDispatchable(ainb_hangar_core::agent_kind::AgentKind),
    /// A squad fan-out was rejected (unknown squad, dangling member, …).
    Squad(ainb_hangar_store::service::squad_assign::SquadAssignError),
    /// `interactive` mode was requested for a SQUAD card. A squad runs as a headless
    /// batch (the leader coordinates the members), so interactive is not supported —
    /// rejected loudly rather than silently downgraded, so the reply never lies about
    /// the mode the card ran in (tcp T4 / FANOUT-SEMANTICS).
    InteractiveSquad,
    /// The workspace has no agent to run a single-agent card on.
    NoAgent,
    /// The resolved agent is not invocable by the effective invoker (gap #8: the
    /// agent is `private`, or `public_to` without the invoker on its allow-list).
    /// Carries `(agent_id, invoker)` for the client-visible message. No task row is
    /// written.
    NotInvocable { agent_id: String, invoker: String },
    /// A store fault.
    Db(sqlx::Error),
}

/// Map a [`CardRunError`] onto an RPC error for the `board_card_run` handler.
fn card_run_err(e: CardRunError) -> RpcError {
    match e {
        CardRunError::Blocked(refs) => invalid_params(&format!(
            "this card is blocked by unfinished cards ({}); finish them (or remove the dependency) first",
            refs.join(", ")
        )),
        CardRunError::ActiveRun(status) => invalid_params(&format!(
            "a run is already active for this card ({status}); cancel it or wait for it to finish"
        )),
        CardRunError::NoRepo => invalid_params(
            "a repo is required to run this card — pick one, or use the scratch repo",
        ),
        CardRunError::NotDispatchable(kind) => invalid_params(&format!(
            "the {kind} provider is not yet wired for dispatch (F8) — pick claude or codex",
        )),
        CardRunError::Squad(se) => squad_assign_err(&se),
        CardRunError::InteractiveSquad => invalid_params(
            "interactive mode is not supported for a squad card — a squad runs as a headless batch; use headless",
        ),
        CardRunError::NoAgent => invalid_params("this workspace has no agent to run the card on"),
        CardRunError::NotInvocable { agent_id, invoker } => invalid_params(&format!(
            "agent {agent_id} is not invocable by {invoker} — it is private or you are not on its allow-list"
        )),
        CardRunError::Db(db) => store_err(&db),
    }
}

/// The card launches currently in flight, keyed by issue id (tcp T4 hardening).
///
/// [`run_card`]'s blocked/active checks and its enqueue are separate statements,
/// so two CONCURRENT launches of one card — a manual Run racing the finalize
/// auto-run — could both pass the checks before either inserts. The migration-0012
/// unique index only backstops `queued`/`dispatched` rows: once the first task is
/// claimed to `running`, the second insert would slip through and the card runs
/// twice. Every launch path lives in this one daemon process (the daemon owns the
/// socket, the claim loop, AND the store — architecture invariant #1), so an
/// in-process per-issue slot held across the whole check+enqueue serializes them:
/// the loser refuses exactly like a run that lost to an already-active task.
static CARD_LAUNCHES_IN_FLIGHT: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashSet<String>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashSet::new()));

/// RAII slot in [`CARD_LAUNCHES_IN_FLIGHT`]: acquired at [`run_card`] entry,
/// released on drop (any exit path). The mutex is only held inside
/// acquire/release — never across an await — so it cannot block the runtime.
struct CardLaunchSlot(String);

impl CardLaunchSlot {
    /// Claim the launch slot for `issue_id`, or `None` when another launch of the
    /// same card is already in flight.
    fn acquire(issue_id: &str) -> Option<Self> {
        let mut set = CARD_LAUNCHES_IN_FLIGHT
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        set.insert(issue_id.to_string()).then(|| Self(issue_id.to_string()))
    }
}

impl Drop for CardLaunchSlot {
    fn drop(&mut self) {
        CARD_LAUNCHES_IN_FLIGHT
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.0);
    }
}

/// Launch a card's issue NOW — the shared core behind the `board_card_run` RPC and
/// the F7 auto-run seam (tcp T4).
///
/// Order of guards (each a hard stop):
///   1. F7 refuse-run — a card with any UNFINISHED blocker never dispatches (it is
///      not launchable until its blockers finish);
///   2. one-active-run guard — a card with an active (queued/dispatched/running)
///      task cannot start another (card = issue), which also stops a squad card
///      from being double-fanned;
///   3. F2 repo-required — the run-time override, else the card's persisted repo,
///      else a refusal (never a "random" run);
///   4. F4 agent cascade + F8 dispatchable check.
///
/// Then it forks: a card with an assigned SQUAD (`issue.squad_id`, migration 0035)
/// FANS OUT via [`SquadAssignService::assign_fanout`] — the leader brief plus one
/// task per distinct `agent` member, each stamped with the card's repo so each
/// provisions its OWN worktree; otherwise it enqueues ONE task on the card's
/// assignee agent (the pre-T4 single-agent path). `board_id` scopes the F4 board
/// tier (pass `None` from the auto-run seam, which is board-agnostic).
pub(crate) async fn run_card(
    pool: &SqlitePool,
    ws: &WorkspaceId,
    board_id: Option<&str>,
    issue: &ainb_hangar_store::repo::issue::Issue,
    mode: &str,
    repo_override: Option<&str>,
    agent_override: Option<ainb_hangar_core::agent_kind::AgentKind>,
    source_branch_override: Option<&str>,
    assignee_override: Option<&ainb_hangar_core::actor::ActorRef>,
    invoker_user_id: Option<&str>,
) -> Result<CardRunOutcome, CardRunError> {
    use ainb_hangar_core::idgen::{IdGen, SystemIdGen};
    use ainb_hangar_store::repo::card_dependency::CardDependencyRepo;
    use ainb_hangar_store::repo::card_parity::CardParityRepo;
    use ainb_hangar_store::repo::task::{NewTask, TaskRepo};
    use ainb_hangar_store::service::squad_assign::{SquadAssignRequest, SquadAssignService};

    let issue_id = issue.id.as_str();

    // 0. One launch of a card at a time (in-process slot, held to the end of this
    //    function): a manual Run racing the finalize auto-run serializes here, so
    //    the checks below can never both pass for one card. The loser reports the
    //    same "already active" refusal a lost re-run gets.
    let _launch_slot = CardLaunchSlot::acquire(issue_id)
        .ok_or_else(|| CardRunError::ActiveRun("launching".to_string()))?;

    // 1. F7 refuse-run: a card with any UNFINISHED blocker is not dispatched.
    let blockers = CardDependencyRepo::unfinished_blockers_of(pool, issue_id)
        .await
        .map_err(CardRunError::Db)?;
    if !blockers.is_empty() {
        let refs = blockers.iter().map(|b| crate::rpc::snapshots::short_display_id(b)).collect();
        return Err(CardRunError::Blocked(refs));
    }

    // 2. One active run per card (card = issue). Blocks a re-run — and a second
    //    squad fan-out — until the current run finishes or is cancelled.
    if let Some(active) = TaskRepo::active_task_for_issue(pool, ws.as_str(), issue_id)
        .await
        .map_err(CardRunError::Db)?
    {
        return Err(CardRunError::ActiveRun(active.status));
    }

    // 2a. Mint this run's GENERATION (migration 0039, tcp 8ln): a fresh Run / rerun
    //     of a card is a new run epoch, so stamp all of this run's tasks (the single
    //     task, or the whole fan-out) with it. The card-state folds (aggregate /
    //     blocker-finished / auto-move / chip) scope to an issue's LATEST generation,
    //     so a prior run's terminal rows never poison this one. Minted here — under
    //     the launch slot + the one-active-run guard above — so no two runs of one
    //     card can share a generation.
    let generation = TaskRepo::next_generation_for_issue(pool, issue_id)
        .await
        .map_err(CardRunError::Db)?;

    // 3. F2 repo-required: run-time override, else the card's persisted repo.
    let (card_repo, card_agent) = CardParityRepo::get_issue_repo_agent(pool, issue_id)
        .await
        .map_err(CardRunError::Db)?
        .unwrap_or((None, None));
    let repo_ref = repo_override.map(str::to_string).or(card_repo).ok_or(CardRunError::NoRepo)?;

    // 3b. Source branch (0042): run-time override, else the card's persisted
    // source_branch; `None` lets provision branch off the repo's default HEAD.
    let card_source = CardParityRepo::get_issue_branches(pool, issue_id)
        .await
        .map_err(CardRunError::Db)?
        .and_then(|(source, _target)| source);
    let source_branch = source_branch_override.map(str::to_string).or(card_source);

    // 4. F4 agent cascade + F8 dispatchable check.
    let agent_kind = match agent_override.or(card_agent) {
        Some(k) => k,
        None => CardParityRepo::resolve_agent_cascade(pool, ws, board_id)
            .await
            .map_err(CardRunError::Db)?,
    };
    if !agent_kind.is_dispatchable() {
        return Err(CardRunError::NotDispatchable(agent_kind));
    }

    // F4: record the just-run agent as last-used (best-effort — never fail a run).
    if let Err(e) = CardParityRepo::set_last_used_agent(pool, agent_kind).await {
        tracing::warn!(error = %e, "card_run: last-used agent write failed");
    }

    // Fork: a squad card FANS OUT; a single-agent card enqueues one task.
    let squad_id = CardParityRepo::get_issue_squad(pool, issue_id)
        .await
        .map_err(CardRunError::Db)?;
    if let Some(squad_id) = squad_id {
        // Squad fan-out: leader brief + one task per member, each stamped with the
        // card's repo (own worktree) + resolved provider. A squad is a HEADLESS batch
        // (the leader coordinates the members): `interactive` has no coherent meaning
        // across a fan-out, so reject it loudly rather than silently discard it and
        // echo back a mode the run never used (tcp T4 / FANOUT-SEMANTICS). Only a
        // headless request reaches the fan-out, so the reply's echoed mode is honest.
        if mode == "interactive" {
            return Err(CardRunError::InteractiveSquad);
        }
        let request = SquadAssignRequest {
            issue_id: Some(issue_id),
            repo_ref: Some(&repo_ref),
            agent_kind: Some(agent_kind),
            generation,
            ..SquadAssignRequest::default()
        };
        let fanout = SquadAssignService::assign_fanout(
            pool,
            ws,
            &squad_id,
            &request,
            &SystemIdGen,
            &SystemClock,
        )
        .await
        .map_err(CardRunError::Squad)?;
        return Ok(CardRunOutcome::Squad {
            leader_task_id: fanout.leader.task_id,
            leader_agent_id: fanout.leader.leader_agent_id,
            leader_runtime_id: fanout.leader.runtime_id,
            member_task_ids: fanout.members.into_iter().map(|m| m.task_id).collect(),
        });
    }

    // Single-agent: resolve the assignee agent (D16), then enqueue one task keyed
    // to its `(agent_id, runtime_id)` + the resolved repo/agent-kind, in ONE tx.
    // A run-time `assignee_override` (V3-F3: the create wizard targeting a named
    // agent) WINS over the issue's persisted assignee, so a run dispatches under
    // the picked agent even if the persisting `issue_update` has not landed yet.
    let assignee = assignee_override.or(issue.assignee.as_ref());
    let agent = resolve_run_agent_opt(pool, ws, assignee)
        .await
        .map_err(CardRunError::Db)?
        .ok_or(CardRunError::NoAgent)?;

    // gap #8 invocation gate: a run may only be enqueued for an agent the invoker
    // is permitted to invoke (multica canInvokeAgent parity). The EFFECTIVE invoker
    // defaults to the workspace owner (the ordinary single-operator TUI Run) when no
    // explicit invoker is supplied — the owner branch always admits, so the existing
    // Run path is unchanged; the gate only bites a non-owner member (the case the
    // allow-list exists for). Denied here means NO task row is written.
    let invoker_id = match invoker_user_id {
        Some(u) => u.to_string(),
        None => ainb_hangar_store::repo::workspace::WorkspaceRepo::owner_id(pool, ws)
            .await
            .map_err(CardRunError::Db)?
            .unwrap_or_default(),
    };
    let invocable = ainb_hangar_store::repo::agent::AgentRepo::can_invoke(
        pool,
        &agent,
        ainb_hangar_core::actor::ActorKind::Member,
        Some(&invoker_id),
    )
    .await
    .map_err(CardRunError::Db)?;
    if !invocable {
        return Err(CardRunError::NotInvocable {
            agent_id: agent.id.clone(),
            invoker: invoker_id,
        });
    }

    let task_id = SystemIdGen.new_ulid();
    let mut tx = pool.begin().await.map_err(CardRunError::Db)?;
    TaskRepo::insert_in_tx(
        &mut tx,
        &NewTask {
            id: task_id.clone(),
            workspace_id: ws.as_str().to_string(),
            runtime_id: agent.runtime_id.clone(),
            agent_id: agent.id.clone(),
            issue_id: Some(issue_id.to_string()),
            work_dir: None,
            priority: 0,
            created_at: SystemClock.now_ms(),
            autopilot_run_id: None,
            generation,
        },
    )
    .await
    .map_err(CardRunError::Db)?;
    if mode == "interactive" {
        sqlx::query("UPDATE agent_task_queue SET mode = 'interactive' WHERE id = ?")
            .bind(&task_id)
            .execute(&mut *tx)
            .await
            .map_err(CardRunError::Db)?;
    }
    CardParityRepo::set_task_repo_agent_in_tx(&mut tx, &task_id, Some(&repo_ref), agent_kind)
        .await
        .map_err(CardRunError::Db)?;
    CardParityRepo::set_task_source_branch_in_tx(&mut tx, &task_id, source_branch.as_deref())
        .await
        .map_err(CardRunError::Db)?;
    tx.commit().await.map_err(CardRunError::Db)?;

    Ok(CardRunOutcome::Single {
        task_id,
        agent_id: agent.id,
        runtime_id: agent.runtime_id,
    })
}

/// Resolve the agent a card run routes to (the issue's assignee agent when it names
/// an in-workspace agent, else the workspace's first non-archived agent), returning
/// `None` when the workspace has no agent at all. Used by [`run_card`], which maps
/// `None` to `CardRunError::NoAgent`.
async fn resolve_run_agent_opt(
    pool: &SqlitePool,
    ws: &WorkspaceId,
    assignee: Option<&ainb_hangar_core::actor::ActorRef>,
) -> Result<Option<ainb_hangar_store::repo::agent::Agent>, sqlx::Error> {
    use ainb_hangar_core::actor::ActorKind;
    use ainb_hangar_store::repo::agent::AgentRepo;

    if let Some(actor) = assignee {
        if actor.kind() == ActorKind::Agent {
            if let Some(agent) = AgentRepo::get(pool, actor.id())
                .await?
                .filter(|a| a.workspace_id == ws.as_str() && !a.archived)
            {
                return Ok(Some(agent));
            }
        }
    }
    Ok(AgentRepo::list_by_workspace(pool, ws.as_str()).await?.into_iter().next())
}

/// `hangar/board_card_assign_squad` (tcp T4 / F7): assign (or clear) a SQUAD as a
/// card's assignee.
///
/// Persists `issue.squad_id` (migration 0035) so a later `board_card_run` fans the
/// card out across the whole squad. A `Some(squad_id)` is validated to name a real
/// squad in the workspace (`SquadRepo::get` — no cross-tenant / dangling ref); a
/// `None` clears the assignment. The card must be on this board. Answers with the
/// refreshed `BoardsListResult`, like every `board_*` mutation.
async fn handle_board_card_assign_squad(
    pool: &SqlitePool,
    req: &RpcRequest,
) -> Result<serde_json::Value, RpcError> {
    use ainb_hangar_store::repo::card_parity::CardParityRepo;
    use ainb_hangar_store::repo::squad::SquadRepo;

    let params: ainb_hangar_proto::snapshots::BoardCardAssignSquadParams =
        parse_params(req, "{ workspace_id, board_id, issue_id, squad_id? }")?;
    let ws = resolve_wire_or_reject(pool, &params.workspace_id).await?;

    // The issue must be a real card on this board (a squad assignment is a card
    // affordance) — reject a non-card / foreign-board issue id up front.
    let board = board_in_ws(pool, &ws, &params.board_id).await?;
    if !board.cards.iter().any(|c| c.issue_id == params.issue_id) {
        return Err(invalid_params("that issue is not a card on this board"));
    }

    // A set squad must exist in this workspace (the column carries no FK, so this
    // is the guard against a dangling / cross-tenant squad id).
    if let Some(squad_id) = params.squad_id.as_deref() {
        let known = SquadRepo::list(pool, &ws)
            .await
            .map_err(|e| store_err(&e))?
            .iter()
            .any(|s| s.id == squad_id);
        if !known {
            return Err(invalid_params("no squad with that id in this workspace"));
        }
    }

    if !CardParityRepo::set_issue_squad(pool, &ws, &params.issue_id, params.squad_id.as_deref())
        .await
        .map_err(|e| store_err(&e))?
    {
        return Err(invalid_params("no issue with that id in this workspace"));
    }
    boards_list_value(pool, &ws).await
}

/// `hangar/board_card_dep_add` (`add = true`) / `hangar/board_card_dep_remove`
/// (`add = false`) (tcp T4 / F7): add or remove a `depends-on` edge between two
/// cards.
///
/// Both endpoints must be cards on this board. On add, a self-edge / cycle /
/// unknown endpoint is rejected ([`card_dep_err`]); a re-add is idempotent. On
/// remove, an absent edge is a no-op. Answers with the refreshed
/// `BoardsListResult`.
async fn handle_board_card_dep(
    pool: &SqlitePool,
    req: &RpcRequest,
    add: bool,
) -> Result<serde_json::Value, RpcError> {
    use ainb_hangar_store::repo::card_dependency::CardDependencyRepo;

    let params: ainb_hangar_proto::snapshots::BoardCardDepParams = parse_params(
        req,
        "{ workspace_id, board_id, dependent_issue_id, blocker_issue_id }",
    )?;
    let ws = resolve_wire_or_reject(pool, &params.workspace_id).await?;

    // Both endpoints must be cards on this board — a dependency is a board
    // affordance between two of its cards, not any two workspace issues.
    let board = board_in_ws(pool, &ws, &params.board_id).await?;
    let on_board = |id: &str| board.cards.iter().any(|c| c.issue_id == id);
    if !on_board(&params.dependent_issue_id) || !on_board(&params.blocker_issue_id) {
        return Err(invalid_params("both cards must be on this board"));
    }

    if add {
        CardDependencyRepo::add_edge(
            pool,
            &ws,
            &params.dependent_issue_id,
            &params.blocker_issue_id,
            SystemClock.now_ms(),
        )
        .await
        .map_err(|e| card_dep_err(&e))?;
    } else {
        CardDependencyRepo::remove_edge(
            pool,
            &ws,
            &params.dependent_issue_id,
            &params.blocker_issue_id,
        )
        .await
        .map_err(|e| store_err(&e))?;
    }
    boards_list_value(pool, &ws).await
}

/// `hangar/board_card_set_auto_run` (tcp T4 / F7): flip a card's auto-run flag.
///
/// Persists `issue.auto_run` (migration 0036) so the finalize seam auto-launches
/// the card the instant its last blocker completes. The card must be on this board.
/// Answers with the refreshed `BoardsListResult`.
async fn handle_board_card_set_auto_run(
    pool: &SqlitePool,
    req: &RpcRequest,
) -> Result<serde_json::Value, RpcError> {
    use ainb_hangar_store::repo::card_dependency::CardDependencyRepo;

    let params: ainb_hangar_proto::snapshots::BoardCardAutoRunParams =
        parse_params(req, "{ workspace_id, board_id, issue_id, auto_run }")?;
    let ws = resolve_wire_or_reject(pool, &params.workspace_id).await?;

    let board = board_in_ws(pool, &ws, &params.board_id).await?;
    if !board.cards.iter().any(|c| c.issue_id == params.issue_id) {
        return Err(invalid_params("that issue is not a card on this board"));
    }

    if !CardDependencyRepo::set_auto_run(pool, &ws, &params.issue_id, params.auto_run)
        .await
        .map_err(|e| store_err(&e))?
    {
        return Err(invalid_params("no issue with that id in this workspace"));
    }
    boards_list_value(pool, &ws).await
}

/// Map a [`CardDependencyError`] onto an RPC error: a self-edge / cycle / not-found
/// rejection is a client error (`INVALID_PARAMS`), a store fault an internal error.
///
/// [`CardDependencyError`]: ainb_hangar_store::repo::card_dependency::CardDependencyError
fn card_dep_err(e: &ainb_hangar_store::repo::card_dependency::CardDependencyError) -> RpcError {
    use ainb_hangar_store::repo::card_dependency::CardDependencyError;
    match e {
        CardDependencyError::SelfDependency => invalid_params("a card cannot depend on itself"),
        CardDependencyError::Cycle => invalid_params("that dependency would create a cycle"),
        CardDependencyError::NotFound => invalid_params("both cards must be on this board"),
        CardDependencyError::Db(db) => store_err(db),
    }
}

/// `hangar/board_card_cancel` (tcp T3 / F6 + T4 / FANOUT-SEMANTICS): cancel a
/// card's in-flight run(s).
///
/// Resolves the card's ENTIRE active set — a squad card fans out N tasks onto one
/// issue, so cancelling only the newest sibling left the leader + the rest burning
/// tokens (and later re-moving the "cancelled" card). This cancels EVERY active
/// (`queued` / `dispatched` / `running`) task of the issue: each is flipped to
/// `cancelled` (the idempotent `CancelTaskService` FSM edge, whose SQL conditional
/// finalize arbitrates the cancel-vs-natural-finish race per task) and its run is
/// SIGNALLED to KILL — a headless process group (via the runner's `kill_on_drop`)
/// or the interactive tmux session by its exact name. Each run's worktree is torn
/// down (keep-if-dirty) on its own finalize seam. The per-task outcomes fold into
/// ONE card-level story: a single aggregate auto-move + dependency re-eval after the
/// set drains. A card whose whole set is already terminal cannot be retroactively
/// cancelled (`cancelled = false`, never an error).
async fn handle_board_card_cancel(
    pool: &SqlitePool,
    req: &RpcRequest,
    events: &EventSink,
) -> Result<serde_json::Value, RpcError> {
    use ainb_hangar_store::repo::task::TaskRepo;
    use ainb_hangar_store::service::cancel::CancelTaskService;
    use ainb_hangar_store::service::finalize::{FinalizeError, FinalizeOutcome};

    let params: ainb_hangar_proto::snapshots::BoardCardCancelParams =
        parse_params(req, "{ workspace_id, board_id, issue_id }")?;
    let ws = resolve_wire_or_reject(pool, &params.workspace_id).await?;

    // The issue must be a real CARD on this board — a cancel is a card affordance,
    // so a non-card / foreign-board issue id is rejected, not silently acted on.
    let board = board_in_ws(pool, &ws, &params.board_id).await?;
    if !board.cards.iter().any(|c| c.issue_id == params.issue_id) {
        return Err(invalid_params("that issue is not a card on this board"));
    }

    // Resolve the card's ENTIRE active set (newest first). An empty set means the
    // card's whole run is already terminal (or it never ran) — a clean no-op the
    // caller surfaces as a note, never an error. The newest task is the "primary"
    // whose id the (single-valued) reply carries, matching the pre-fan-out shape.
    let active = TaskRepo::active_tasks_for_issue(pool, ws.as_str(), &params.issue_id)
        .await
        .map_err(|e| store_err(&e))?;
    let Some(primary) = active.first().cloned() else {
        return to_value(&ainb_hangar_proto::snapshots::BoardCardCancelResult {
            task_id: None,
            cancelled: false,
        });
    };

    // Cancel EVERY active task of the card. Each cancel's conditional finalize is
    // the per-task arbiter of the cancel-vs-natural-finish race:
    // - `Transitioned` — this call won the cancel for that task: SIGNAL its kill and
    //   push its terminal event.
    // - `AlreadyTerminal` — an idempotent replay of a prior cancel; nothing more.
    // - `TerminalMismatch` — that task finished naturally first; leave it.
    // A per-task store fault is logged and the loop continues (leaving a sibling
    // running is worse than a clean error); it only surfaces if NOTHING cancelled.
    let mut any_cancelled = false;
    let mut last_err: Option<String> = None;
    for task in &active {
        match CancelTaskService::cancel(pool, &task.id, &SystemClock).await {
            Ok(FinalizeOutcome::Transitioned) => {
                // `false` = no live run was registered (queued-but-unclaimed, or
                // owned by another daemon) — the DB flip alone cancels it.
                let signalled = crate::cancel::registry().signal(&task.id);
                crate::run_loop::emit_task_finished(
                    events,
                    task,
                    ainb_hangar_proto::events::TaskResult::Cancelled,
                    &SystemClock,
                );
                tracing::info!(task_id = %task.id, signalled, issue = %params.issue_id, "card cancel: sibling cancelled");
                any_cancelled = true;
            }
            Ok(FinalizeOutcome::AlreadyTerminal) => any_cancelled = true,
            Err(FinalizeError::TerminalMismatch { .. }) => {}
            Err(e) => {
                tracing::warn!(task_id = %task.id, error = %e, "card cancel: a sibling cancel errored; continuing");
                last_err = Some(e.to_string());
            }
        }
    }

    // Honesty guard: if any per-task cancel raised a store fault, the cancel may be
    // PARTIAL — re-read the active set and surface an error when siblings survived,
    // rather than reporting a clean success while a leader/member keeps burning. An
    // errored task that ended up terminal anyway (it raced to a natural finish) leaves
    // an empty residual and is tolerated.
    if let Some(e) = last_err {
        let residual = TaskRepo::active_tasks_for_issue(pool, ws.as_str(), &params.issue_id)
            .await
            .map_err(|e| store_err(&e))?;
        if !residual.is_empty() {
            return Err(internal(&format!(
                "cancel partially failed: {} task(s) still active ({e})",
                residual.len()
            )));
        }
    }

    if !any_cancelled {
        // Nothing to cancel — every active task finished naturally between the read
        // and the cancel. Report not-cancelled, never an error.
        return to_value(&ainb_hangar_proto::snapshots::BoardCardCancelResult {
            task_id: Some(primary.id),
            cancelled: false,
        });
    }

    // ONE card-level story now the set has drained: aggregate-auto-move the card
    // (lands in the `cancelled` column unless a sibling had failed) and re-evaluate
    // dependents (a partly-done-then-cancelled blocker can still unblock). Both are
    // best-effort and idempotent with each run future's own finalize seam.
    crate::board::auto_move_after_terminal(pool, &primary).await;
    crate::board::unblock_dependents_after_terminal(pool, &primary).await;
    to_value(&ainb_hangar_proto::snapshots::BoardCardCancelResult {
        task_id: Some(primary.id),
        cancelled: true,
    })
}

/// `hangar/board_card_reorder` (tcp T3 / F6): set the order of one column's cards.
///
/// A pure `board_card.ord` rewrite within the given column (`column_id = None` for
/// the unmapped pool): `issue_ids` must be exactly that column's current cards (a
/// permutation), else the repo rejects it and nothing is written. No card changes
/// column. Answers with the refreshed `BoardsListResult`, like every `board_*`
/// mutation.
async fn handle_board_card_reorder(
    pool: &SqlitePool,
    req: &RpcRequest,
) -> Result<serde_json::Value, RpcError> {
    use ainb_hangar_store::repo::board::BoardRepo;

    let params: ainb_hangar_proto::snapshots::BoardCardReorderParams =
        parse_params(req, "{ workspace_id, board_id, column_id?, issue_ids }")?;
    let ws = resolve_wire_or_reject(pool, &params.workspace_id).await?;
    BoardRepo::card_reorder(
        pool,
        &ws,
        &params.board_id,
        params.column_id.as_deref(),
        &params.issue_ids,
    )
    .await
    .map_err(|e| board_repo_err(&e))?;
    boards_list_value(pool, &ws).await
}

/// `hangar/board_card_remove` (tcp T3 / F6): take an issue card off a board.
///
/// Removes ONLY the board placement — the underlying issue is left intact (a card
/// can be re-added, and it still shows in the issue list). A card with an ACTIVE
/// (`queued` / `dispatched` / `running`) run is REFUSED: removing it would strand a
/// live task, so the caller must cancel the run first (delete-while-running =
/// cancel-first). Idempotent otherwise — removing a card not on the board is a
/// clean no-op. Answers with the refreshed `BoardsListResult`.
async fn handle_board_card_remove(
    pool: &SqlitePool,
    req: &RpcRequest,
) -> Result<serde_json::Value, RpcError> {
    use ainb_hangar_store::repo::board::{BoardRepo, CardRemoveOutcome};

    let params: ainb_hangar_proto::snapshots::BoardCardParams =
        parse_params(req, "{ workspace_id, board_id, issue_id }")?;
    let ws = resolve_wire_or_reject(pool, &params.workspace_id).await?;
    if params.issue_id.trim().is_empty() {
        return Err(invalid_params("issue_id must not be empty"));
    }

    // The active-run guard + the delete are ONE atomic statement in the repo (no
    // TOCTOU window a concurrent `board_card_run` could slip through). A card with
    // a live run is refused (cancel-first); a card that is not on the board is an
    // idempotent no-op.
    match BoardRepo::card_remove(pool, &ws, &params.board_id, &params.issue_id)
        .await
        .map_err(|e| board_repo_err(&e))?
    {
        CardRemoveOutcome::BlockedByActiveRun => Err(invalid_params(
            "this card has an active run; cancel it before removing the card",
        )),
        CardRemoveOutcome::Removed | CardRemoveOutcome::NotOnBoard => {
            boards_list_value(pool, &ws).await
        }
    }
}

/// `hangar/board_card_timeline` (tcp T3 / F6, P10 §4.9): the raw stream-json
/// transcript of a card's newest run, for the prettied timeline overlay.
///
/// Resolves the card's most recent task, derives the deterministic per-task logs
/// dir ([`crate::execenv::logs_dir`] — the exact tree the run wrote), and reads a
/// bounded TAIL of whichever provider log exists (`claude.jsonl` / `codex.jsonl`).
/// The plugin parses the returned text into the transcript taxonomy. A card that
/// never ran, or whose log is absent/unreadable, yields an empty transcript (a
/// read: never an `INVALID_PARAMS` on a missing log).
async fn handle_board_card_timeline(
    pool: &SqlitePool,
    req: &RpcRequest,
) -> Result<serde_json::Value, RpcError> {
    /// Cap the returned transcript at 512 KiB so a long run never floods the
    /// socket; the plugin's timeline is a tail view, and the parser skips the
    /// leading partial line a mid-file seek leaves.
    const TAIL_CAP: u64 = 512 * 1024;

    let params: ainb_hangar_proto::snapshots::BoardCardParams =
        parse_params(req, "{ workspace_id, board_id, issue_id }")?;
    let ws = resolve_wire_or_reject(pool, &params.workspace_id).await?;

    // The issue must be a real card on this board (a timeline is a card affordance).
    let board = board_in_ws(pool, &ws, &params.board_id).await?;
    if !board.cards.iter().any(|c| c.issue_id == params.issue_id) {
        return Err(invalid_params("that issue is not a card on this board"));
    }

    // The card's newest task (any status) — its run is the one to show.
    let task_id: Option<String> = sqlx::query_scalar(
        "SELECT id FROM agent_task_queue WHERE issue_id = ? AND workspace_id = ? \
         ORDER BY created_at DESC, id DESC LIMIT 1",
    )
    .bind(&params.issue_id)
    .bind(ws.as_str())
    .fetch_optional(pool)
    .await
    .map_err(|e| store_err(&e))?;

    let Some(task_id) = task_id else {
        // No run yet — an empty transcript, not an error.
        return to_value(&ainb_hangar_proto::snapshots::BoardCardTimelineResult::default());
    };

    let ws_slug = crate::run_loop::workspace_slug(pool, ws.as_str())
        .await
        .map_err(|e| internal(&format!("resolve workspace slug: {e}")))?;
    // Candidate `logs/` dirs, newest slug scheme first then the pre-T4 legacy
    // slug, so a run written under EITHER scheme resolves — a pre-upgrade task's
    // transcript is never stranded by the T4 collision-resistant slug change.
    let log_dirs =
        crate::execenv::logs_dir_candidates(&crate::run_loop::hangar_home(), &ws_slug, &task_id);

    // The run tees exactly one provider log; read whichever exists (a bounded
    // tail) across the candidate dirs, newest scheme first. from_utf8_lossy + the
    // parser's leading-partial-line skip make a mid-char seek boundary harmless.
    let (provider, jsonl) = log_dirs
        .iter()
        .flat_map(|logs| {
            [("claude", "claude.jsonl"), ("codex", "codex.jsonl")]
                .map(move |(provider, file)| (provider, logs.join(file)))
        })
        .find_map(|(provider, path)| {
            read_tail(&path, TAIL_CAP).map(|text| (provider.to_string(), text))
        })
        .map_or((None, String::new()), |(p, t)| (Some(p), t));

    to_value(&ainb_hangar_proto::snapshots::BoardCardTimelineResult {
        task_id: Some(task_id),
        provider,
        jsonl,
    })
}

/// Read the last `cap` bytes of `path` as a lossy string, or `None` when the file
/// does not exist / cannot be read (a missing run log is not an error). Seeking to
/// the tail keeps a huge run log off the heap; a mid-char boundary at the seek
/// point decodes losslessly and the transcript parser skips the leading partial
/// line.
fn read_tail(path: &std::path::Path, cap: u64) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    if len > cap {
        f.seek(SeekFrom::Start(len - cap)).ok()?;
    }
    let mut buf = Vec::new();
    f.take(cap).read_to_end(&mut buf).ok()?;
    Some(String::from_utf8_lossy(&buf).into_owned())
}

/// `hangar/repo_list` (spec F3): the card-create `@` autocomplete roster.
///
/// Reads New Session's FavoritesStore (`~/.agents-in-a-box/favorites.yaml`) +
/// RepositoryCache scan cache (`~/.agents-in-a-box/cache/repositories.json`) AS-IS
/// via the fleet-core roster reader — favorites first (★, most-recent-first),
/// then scanned repos in cache order, deduped. NEVER triggers a scan. Host-scoped
/// (the roster is not workspace-partitioned); a cold / first-run install yields an
/// empty roster. Reads the REAL user home (`dirs::home_dir()`, honouring `$HOME`),
/// NOT the `$AINB_HANGAR_HOME` override — favorites/cache live under `~` regardless
/// of where the daemon's db is redirected.
fn handle_repo_list(req: &RpcRequest) -> Result<serde_json::Value, RpcError> {
    // Params are `{}`; tolerate (and ignore) any body a caller sends.
    let _ = req;
    let Some(ainb_dir) = dirs::home_dir().map(|h| h.join(".agents-in-a-box")) else {
        // No resolvable home → an empty roster (the picker still offers scratch).
        return to_value(&ainb_hangar_proto::snapshots::RepoListResult { repos: Vec::new() });
    };
    let repos = ainb_fleet_core::repo_roster::read_roster(&ainb_dir)
        .into_iter()
        .map(|e| ainb_hangar_proto::snapshots::RepoWireRow {
            name: e.name,
            path: e.path,
            remote: e.remote,
            is_favorite: e.is_favorite,
            last_used_ms: e.last_used_ms,
        })
        .collect();
    to_value(&ainb_hangar_proto::snapshots::RepoListResult { repos })
}

/// Fetch board `board_id` in `ws`, or an `INVALID_PARAMS` rejection when no such
/// board exists in the workspace — the membership guard both card handlers key
/// off so a card create/run cannot target a foreign / unknown board.
async fn board_in_ws(
    pool: &SqlitePool,
    ws: &WorkspaceId,
    board_id: &str,
) -> Result<ainb_hangar_store::repo::board::Board, RpcError> {
    ainb_hangar_store::repo::board::BoardRepo::list(pool, ws)
        .await
        .map_err(|e| store_err(&e))?
        .into_iter()
        .find(|b| b.id == board_id)
        .ok_or_else(|| invalid_params("no board with that id in this workspace"))
}

/// Resolve `slug` to a non-archived agent in `ws` by NAME (D16: the assignee
/// profile slug is the agent's name). Returns `None` when no such agent exists.
async fn resolve_agent_by_name(
    pool: &SqlitePool,
    ws: &WorkspaceId,
    slug: &str,
) -> Result<Option<ainb_hangar_store::repo::agent::Agent>, RpcError> {
    use ainb_hangar_store::repo::agent::AgentRepo;
    let agents = AgentRepo::list_by_workspace(pool, ws.as_str())
        .await
        .map_err(|e| store_err(&e))?;
    Ok(agents.into_iter().find(|a| a.name == slug))
}

/// Re-read `ws`'s boards and serialize them as a
/// [`BoardsListResult`](ainb_hangar_proto::snapshots::BoardsListResult) wire
/// value — the refreshed view every `board_*` mutation answers with.
async fn boards_list_value(
    pool: &SqlitePool,
    ws: &WorkspaceId,
) -> Result<serde_json::Value, RpcError> {
    let boards = snapshots::boards_list(pool, ws.as_str()).await.map_err(|e| store_err(&e))?;
    to_value(&ainb_hangar_proto::snapshots::BoardsListResult { boards })
}

/// Map a [`BoardRepoError`] onto an RPC error: a duplicate-name / not-found /
/// bad-reorder rejection is a client error (`INVALID_PARAMS`), every store fault
/// an internal error. Mirrors [`squad_repo_err`].
///
/// [`BoardRepoError`]: ainb_hangar_store::repo::board::BoardRepoError
fn board_repo_err(e: &ainb_hangar_store::repo::board::BoardRepoError) -> RpcError {
    use ainb_hangar_store::repo::board::BoardRepoError;
    match e {
        BoardRepoError::DuplicateName => {
            invalid_params("a board with that name already exists in this workspace")
        }
        BoardRepoError::DuplicateAutoMove => {
            invalid_params("another auto-move column already maps this task state on this board")
        }
        BoardRepoError::NotFound => {
            invalid_params("no board, column, or card with that id in this workspace")
        }
        BoardRepoError::BadReorder => {
            invalid_params("reorder must list exactly the board's current columns")
        }
        BoardRepoError::Db(db) => store_err(db),
    }
}

/// Resolve a workspace-scoped request's `{ workspace_id }` to a typed
/// [`WorkspaceId`], returning `None` (an empty-snapshot signal) when no
/// workspace matches. Used by the autopilot *list* handler, which carries only
/// the shared scoped params (unlike fire/runs/set which parse their own struct).
async fn resolve_wire_from_scoped(
    pool: &SqlitePool,
    req: &RpcRequest,
) -> Result<Option<WorkspaceId>, RpcError> {
    let wire = workspace_id(req)?;
    resolve_wire(pool, &wire).await
}

/// Map an [`AutopilotRepoError`] onto an RPC error: a cron-validation failure is
/// a client error (`INVALID_PARAMS`), every store fault an internal error.
fn autopilot_repo_err(e: &ainb_hangar_store::repo::autopilot::AutopilotRepoError) -> RpcError {
    use ainb_hangar_store::repo::autopilot::AutopilotRepoError;
    match e {
        AutopilotRepoError::Cron(c) => invalid_params(&format!("invalid cron: {c}")),
        other => internal(&format!("autopilot store error: {other}")),
    }
}

/// An `INVALID_PARAMS` error with `message`.
fn invalid_params(message: &str) -> RpcError {
    RpcError {
        code: INVALID_PARAMS,
        message: message.to_string(),
        data: None,
    }
}

/// An `INTERNAL_ERROR` with `message`.
fn internal(message: &str) -> RpcError {
    RpcError {
        code: INTERNAL_ERROR,
        message: message.to_string(),
        data: None,
    }
}

/// Map a [`SkillRepoError`] onto an RPC error: the cross-workspace guard is a
/// client error (`INVALID_PARAMS`, the caller used a foreign id), every other
/// fault is an internal store error.
fn skill_repo_err(e: &ainb_hangar_store::repo::skill::SkillRepoError) -> RpcError {
    use ainb_hangar_store::repo::skill::SkillRepoError;
    match e {
        SkillRepoError::CrossWorkspace => {
            invalid_params("agent and skill must belong to the subscribed workspace")
        }
        other => internal(&format!("skill store error: {other}")),
    }
}

/// Shared handler for `hangar/skill_attach` (`attach = true`) and
/// `hangar/skill_detach` (`attach = false`): resolve the subscribed workspace
/// and thread it (with the typed agent + skill ids) into the secured repo.
async fn attach_or_detach(
    pool: &SqlitePool,
    req: &RpcRequest,
    attach: bool,
) -> Result<serde_json::Value, RpcError> {
    let params: ainb_hangar_proto::snapshots::SkillAttachParams =
        parse_params(req, "{ workspace_id, agent_id, skill_id }")?;
    let Some(ws) = resolve_wire(pool, &params.workspace_id).await? else {
        return Err(invalid_params(&format!(
            "unknown workspace `{}`",
            params.workspace_id
        )));
    };
    let agent = agent_id(&params.agent_id)?;
    let skill = skill_id(&params.skill_id)?;
    if attach {
        snapshots::skill_attach(pool, &ws, &agent, &skill)
            .await
            .map_err(|e| skill_repo_err(&e))?;
    } else {
        snapshots::skill_detach(pool, &ws, &agent, &skill)
            .await
            .map_err(|e| skill_repo_err(&e))?;
    }
    Ok(serde_json::json!({}))
}

/// Dispatch the four P7.5 autopilot-manager RPCs. Each resolves + scopes by
/// workspace (a foreign id yields an empty snapshot for the reads, fires/toggles
/// nothing for the mutations) and drives the workspace-scoped autopilot snapshot
/// mappers. The two mutations publish their matching [`HangarEvent`] onto
/// `events` after the write commits (e38.2). Split out of [`handle`] to keep
/// that dispatcher within the line cap.
async fn handle_autopilot(
    pool: &SqlitePool,
    req: &RpcRequest,
    events: &EventSink,
) -> Result<serde_json::Value, RpcError> {
    match req.method.as_str() {
        methods::HANGAR_AUTOPILOTS_LIST => {
            let autopilots = match resolve_wire_from_scoped(pool, req).await? {
                Some(ws) => snapshots::autopilots_list(pool, &ws)
                    .await
                    .map_err(|e| autopilot_repo_err(&e))?,
                None => Vec::new(),
            };
            to_value(&ainb_hangar_proto::snapshots::AutopilotsListResult { autopilots })
        }
        methods::HANGAR_AUTOPILOT_RUNS => {
            let params: ainb_hangar_proto::snapshots::AutopilotRunsParams =
                parse_params(req, "{ workspace_id, autopilot_id, limit }")?;
            let runs = match resolve_wire(pool, &params.workspace_id).await? {
                Some(ws) => {
                    let id = autopilot_id(&params.autopilot_id)?;
                    snapshots::autopilot_runs(pool, &ws, &id, params.limit)
                        .await
                        .map_err(|e| autopilot_repo_err(&e))?
                }
                None => Vec::new(),
            };
            to_value(&ainb_hangar_proto::snapshots::AutopilotRunsResult { runs })
        }
        methods::HANGAR_AUTOPILOT_FIRE_NOW => {
            let params: ainb_hangar_proto::snapshots::AutopilotFireNowParams =
                parse_params(req, "{ workspace_id, autopilot_id }")?;
            let ws = resolve_wire_or_reject(pool, &params.workspace_id).await?;
            let id = autopilot_id(&params.autopilot_id)?;
            let fired = snapshots::autopilot_fire_now(pool, &SystemClock, &ws, &id)
                .await
                .map_err(|e| internal(&format!("autopilot fire: {e}")))?;
            // A foreign autopilot id fires nothing — announce only real runs.
            if fired {
                events.emit(
                    ws.as_str(),
                    ainb_hangar_proto::events::HangarEvent::AutopilotRunChanged {
                        autopilot_id: id.to_string(),
                        status: "running".to_string(),
                    },
                );
            }
            Ok(serde_json::json!({}))
        }
        methods::HANGAR_AUTOPILOT_SET_ENABLED => {
            let params: ainb_hangar_proto::snapshots::AutopilotSetEnabledParams =
                parse_params(req, "{ workspace_id, autopilot_id, enabled }")?;
            let ws = resolve_wire_or_reject(pool, &params.workspace_id).await?;
            let id = autopilot_id(&params.autopilot_id)?;
            snapshots::autopilot_set_enabled(pool, &SystemClock, &ws, &id, params.enabled)
                .await
                .map_err(|e| autopilot_repo_err(&e))?;
            // Push the refreshed row so the manager table updates in place
            // (the AutopilotUpdated contract carries the full wire row).
            // Best-effort: a re-read fault only skips the push — the toggle
            // itself already committed and the next snapshot reconciles.
            if let Ok(rows) = snapshots::autopilots_list(pool, &ws).await {
                if let Some(row) = rows.into_iter().find(|r| r.id == id.as_str()) {
                    events.emit(
                        ws.as_str(),
                        ainb_hangar_proto::events::HangarEvent::AutopilotUpdated(row),
                    );
                }
            }
            Ok(serde_json::json!({}))
        }
        other => Err(RpcError {
            code: METHOD_NOT_FOUND,
            message: format!("unknown autopilot method: {other}"),
            data: None,
        }),
    }
}

/// Dispatch `hangar/daemon_health` (P8.5).
///
/// Resolves the workspace (an unknown one yields empty runtimes + zero
/// concurrency, but the daemon-global throughput window + claim-cache figure
/// still report), then builds + serialises the snapshot.
async fn handle_daemon_health(
    pool: &SqlitePool,
    req: &RpcRequest,
    health: &DaemonHealth,
) -> Result<serde_json::Value, RpcError> {
    let ws = resolve(pool, req).await?;
    let snapshot = daemon_health_snapshot(pool, health, ws.as_deref(), &SystemClock)
        .await
        .map_err(|e| store_err(&e))?;
    to_value(&snapshot)
}

/// Dispatch `hangar/inbox_list` (e38.14): snapshot the workspace's aggregated
/// inbox + unread count. A read like `hangar/issues_list`: an unknown workspace
/// yields an empty list + zero unread (no `INVALID_PARAMS` rejection). Split out
/// of [`handle`] to keep that dispatcher within the line cap.
async fn handle_inbox_list(
    pool: &SqlitePool,
    req: &RpcRequest,
) -> Result<serde_json::Value, RpcError> {
    let (entries, unread) = match resolve(pool, req).await? {
        Some(ws) => snapshots::inbox_list(pool, &ws).await.map_err(|e| store_err(&e))?,
        None => (Vec::new(), 0),
    };
    to_value(&ainb_hangar_proto::snapshots::InboxListResult { entries, unread })
}

/// Dispatch `hangar/inbox_mark_read` (e38.14): mark every currently-unread inbox
/// entry in the workspace read, then answer with how many were flipped + the
/// post-sweep unread count.
///
/// A mutating handler: it resolves the workspace and **rejects** a mistyped one
/// with `INVALID_PARAMS` (never a silent no-op, mirroring [`handle_comment_add`]),
/// so a typo'd workspace can never quietly "succeed" while marking nothing. The
/// sweep is workspace-scoped, so a sibling tenant's inbox is never touched.
async fn handle_inbox_mark_read(
    pool: &SqlitePool,
    req: &RpcRequest,
) -> Result<serde_json::Value, RpcError> {
    let wire = workspace_id(req)?;
    let ws = resolve_wire_or_reject(pool, &wire).await?;
    let (marked, unread) = snapshots::inbox_mark_read(pool, &SystemClock, ws.as_str())
        .await
        .map_err(|e| store_err(&e))?;
    to_value(&ainb_hangar_proto::snapshots::InboxMarkReadResult { marked, unread })
}

/// Dispatch `attention/list` (spec P2): snapshot the OPEN attention rows for a
/// scope. `fleet = true` is the host-wide feed; `fleet = false` selects the
/// workspace list (`workspace_id = Some(ws)`) or the no-workspace host rows
/// (`workspace_id = None`). A read, so an unknown workspace yields an empty list
/// (no `INVALID_PARAMS`, mirroring [`handle_inbox_list`]).
async fn handle_attention_list(
    pool: &SqlitePool,
    req: &RpcRequest,
) -> Result<serde_json::Value, RpcError> {
    let params: ainb_hangar_proto::snapshots::AttentionListParams =
        parse_params(req, "{ workspace_id?, fleet }")?;
    let attention = attention_snapshot(pool, params.fleet, params.workspace_id.as_deref()).await?;
    to_value(&ainb_hangar_proto::snapshots::AttentionListResult { attention })
}

/// Dispatch `attention/subscribe` (spec P2): ack with the current OPEN snapshot.
/// `workspace_id = None` (the default) is the FLEET-WIDE snapshot every session
/// raises into; `Some(ws)` narrows to one workspace. The live delta stream is
/// registered in [`serve_conn`] after this ack.
async fn handle_attention_subscribe(
    pool: &SqlitePool,
    req: &RpcRequest,
) -> Result<serde_json::Value, RpcError> {
    let params: ainb_hangar_proto::snapshots::AttentionSubscribeParams =
        parse_params(req, "{ workspace_id? }")?;
    // No workspace filter → the fleet-wide snapshot (every workspace + host);
    // a narrowing workspace → that workspace's open rows.
    let fleet = params.workspace_id.is_none();
    let attention = attention_snapshot(pool, fleet, params.workspace_id.as_deref()).await?;
    to_value(&ainb_hangar_proto::snapshots::AttentionSubscribeResult { attention })
}

/// Shared open-attention snapshot for `attention/list` + `attention/subscribe`.
///
/// Resolves an optional wire workspace id to the real row id (an unknown one
/// yields an empty list, a read). `fleet` overrides the workspace scope with the
/// host-wide feed.
async fn attention_snapshot(
    pool: &SqlitePool,
    fleet: bool,
    workspace_wire: Option<&str>,
) -> Result<Vec<ainb_hangar_proto::events::AttentionRow>, RpcError> {
    if fleet {
        return snapshots::attention_list(pool, None, true).await.map_err(|e| store_err(&e));
    }
    match workspace_wire {
        Some(wire) => match resolve_workspace_id(pool, wire).await.map_err(|e| store_err(&e))? {
            Some(real) => snapshots::attention_list(pool, Some(&real), false)
                .await
                .map_err(|e| store_err(&e)),
            // Unknown workspace → empty list (a read, never an error).
            None => Ok(Vec::new()),
        },
        // No workspace → the no-workspace host rows.
        None => snapshots::attention_list(pool, None, false).await.map_err(|e| store_err(&e)),
    }
}

/// Dispatch `attention/answer` (spec P2): route one answer through the answer
/// router — first-answer-wins claim + C1 misroute guard + verified last-mile
/// send — and return the tagged outcome. A store fault maps to an internal error.
async fn handle_attention_answer(
    pool: &SqlitePool,
    req: &RpcRequest,
    events: &EventSink,
) -> Result<serde_json::Value, RpcError> {
    let params: ainb_hangar_proto::snapshots::AnswerParams =
        parse_params(req, "{ attention_id, answer, answered_by, is_answer? }")?;
    let result = crate::answer::answer(pool, events, &params, SystemClock.now_ms())
        .await
        .map_err(|e| store_err(&e))?;
    to_value(&result)
}

/// Dispatch `atc/register` (spec P9, D12): register (or re-register) an ATC
/// instance so its heartbeat becomes a daemon cron. The daemon-native
/// replacement for `ainb fleet atc setup`'s launchd/systemd timer install:
/// computes the first heartbeat tick from the (defaulted) cron, upserts the
/// `atc_instance` row, and answers the persisted name + next tick. Idempotent by
/// name. A blank name is a client error. Split out of [`handle`] to keep that
/// dispatcher within the line cap.
async fn handle_atc_register(
    pool: &SqlitePool,
    req: &RpcRequest,
) -> Result<serde_json::Value, RpcError> {
    use ainb_hangar_store::repo::atc_instance::{AtcInstanceRepo, RegisterAtc};

    let params: ainb_hangar_proto::snapshots::AtcRegisterParams = parse_params(
        req,
        "{ name, cwd?, tmux_session?, heartbeat_cron?, err_retry_cap?, idle_pause_min? }",
    )?;
    if params.name.trim().is_empty() {
        return Err(invalid_params("atc instance name must not be empty"));
    }
    // Default the heartbeat cron to every-2-min (the standalone ATC's default),
    // validated by the same cron parser the register/reschedule seam uses.
    let cron = params.heartbeat_cron.clone().unwrap_or_else(|| "*/2 * * * *".to_string());
    let now_ms = SystemClock.now_ms();
    let next_tick_at = crate::atc::next_heartbeat_tick(&cron, now_ms);
    // A cron that does not parse is a client error (never a silently-unscheduled
    // instance).
    if next_tick_at.is_none() && ainb_hangar_core::autopilot::cron::parse_cron(&cron).is_err() {
        return Err(invalid_params(&format!("invalid heartbeat cron: {cron}")));
    }
    let reg = RegisterAtc {
        name: params.name.trim().to_string(),
        cwd: params.cwd.clone(),
        tmux_session: params.tmux_session.clone(),
        heartbeat_cron: cron,
        err_retry_cap: params.err_retry_cap.unwrap_or(3),
        idle_pause_min: params.idle_pause_min.unwrap_or(60),
        next_tick_at,
    };
    AtcInstanceRepo::register(pool, &reg, now_ms).await.map_err(|e| store_err(&e))?;
    to_value(&ainb_hangar_proto::snapshots::AtcRegisterResult {
        name: reg.name,
        next_tick_at,
    })
}

/// Dispatch `atc/list` (spec P9, D12): list every registered ATC instance,
/// name-ordered. A read (ATC is host-wide, not workspace-partitioned). Split out
/// of [`handle`] to keep that dispatcher within the line cap.
async fn handle_atc_list(pool: &SqlitePool) -> Result<serde_json::Value, RpcError> {
    use ainb_hangar_store::repo::atc_instance::AtcInstanceRepo;

    let instances = AtcInstanceRepo::list(pool)
        .await
        .map_err(|e| store_err(&e))?
        .into_iter()
        .map(|r| ainb_hangar_proto::snapshots::AtcInstanceWire {
            name: r.name,
            cwd: r.cwd,
            tmux_session: r.tmux_session,
            heartbeat_cron: r.heartbeat_cron,
            err_retry_cap: r.err_retry_cap,
            idle_pause_min: r.idle_pause_min,
            next_tick_at: r.next_tick_at,
            enabled: r.enabled,
            last_heartbeat_at: r.last_heartbeat_at,
        })
        .collect();
    to_value(&ainb_hangar_proto::snapshots::AtcListResult { instances })
}

/// Dispatch `atc/escalate` (spec P9, D12): raise an ATC escalation as an
/// `escalation` attention row through the same pipeline every other input request
/// uses, so it reaches the phone/web push. Answers the raised attention id. A
/// blank instance/session/reason is a client error. Split out of [`handle`] to
/// keep that dispatcher within the line cap.
async fn handle_atc_escalate(
    pool: &SqlitePool,
    req: &RpcRequest,
    events: &EventSink,
) -> Result<serde_json::Value, RpcError> {
    let params: ainb_hangar_proto::snapshots::AtcEscalateParams = parse_params(
        req,
        "{ instance_name, session_id, cwd?, workspace_id?, reason }",
    )?;
    if params.instance_name.trim().is_empty() || params.session_id.trim().is_empty() {
        return Err(invalid_params(
            "atc escalate requires instance_name and session_id",
        ));
    }
    let attention_id = crate::atc::raise_escalation(
        pool,
        events,
        params.instance_name.trim(),
        params.session_id.trim(),
        &params.cwd,
        params.workspace_id.as_deref(),
        &params.reason,
        SystemClock.now_ms(),
    )
    .await
    .map_err(|e| store_err(&e))?;
    to_value(&ainb_hangar_proto::snapshots::AtcEscalateResult { attention_id })
}

/// Dispatch `hangar/notify_rules_list` (tcp T5): the per-attention-kind routing
/// grid for a scope. `workspace_id = None` returns the global rows; a
/// `Some(ws)` returns that workspace's EFFECTIVE rows (override where set, global
/// otherwise). A read: an unknown workspace resolves to the globals rather than
/// erroring, mirroring the other list snapshots. Split out of [`handle`] to keep
/// that dispatcher within the line cap.
async fn handle_notify_rules_list(
    pool: &SqlitePool,
    req: &RpcRequest,
) -> Result<serde_json::Value, RpcError> {
    use ainb_hangar_store::repo::notify_rule::NotifyRuleRepo;

    let params: ainb_hangar_proto::snapshots::NotifyRulesListParams =
        parse_params(req, "{ workspace_id? }")?;
    let ws = match params.workspace_id.as_deref() {
        Some(w) => resolve_wire(pool, w).await?,
        None => None,
    };
    let rules = NotifyRuleRepo::list(pool, ws.as_ref().map(WorkspaceId::as_str))
        .await
        .map_err(|e| store_err(&e))?
        .into_iter()
        .map(|r| ainb_hangar_proto::snapshots::NotifyRuleWireRow {
            kind: r.kind.as_str().to_string(),
            channels: r.channels,
            overridden: r.overridden,
        })
        .collect();
    // Echo the scope this reply answers (agents-in-a-box-cqh) — the REQUESTED
    // `workspace_id`, so a settings grid that flipped its edit scope while this
    // list was in flight can drop a reply for the scope it just left rather than
    // briefly repopulating with the wrong scope's rows.
    to_value(&ainb_hangar_proto::snapshots::NotifyRulesListResult {
        rules,
        workspace_id: params.workspace_id,
    })
}

/// Dispatch `hangar/notify_rule_set` (tcp T5): upsert one routing rule.
/// `workspace_id = None` writes the GLOBAL rule; `Some(ws)` writes a
/// per-workspace override. An unknown `kind` is a client error; a `Some(ws)` that
/// does not resolve is rejected (you cannot override a non-existent workspace).
/// Mutating + idempotent. Split out of [`handle`] to keep that dispatcher within
/// the line cap.
async fn handle_notify_rule_set(
    pool: &SqlitePool,
    req: &RpcRequest,
) -> Result<serde_json::Value, RpcError> {
    use ainb_hangar_store::repo::attention::AttentionKind;
    use ainb_hangar_store::repo::notify_rule::NotifyRuleRepo;

    let params: ainb_hangar_proto::snapshots::NotifyRuleSetParams =
        parse_params(req, "{ workspace_id?, kind, channels }")?;
    let kind = AttentionKind::parse(&params.kind)
        .ok_or_else(|| invalid_params(&format!("unknown attention kind `{}`", params.kind)))?;
    let ws = match params.workspace_id.as_deref() {
        Some(w) => Some(resolve_wire_or_reject(pool, w).await?),
        None => None,
    };
    NotifyRuleRepo::set(
        pool,
        ws.as_ref().map(WorkspaceId::as_str),
        kind,
        params.channels,
    )
    .await
    .map_err(|e| store_err(&e))?;
    to_value(&ainb_hangar_proto::snapshots::NotifyRuleSetResult {
        kind: kind.as_str().to_string(),
        channels: params.channels,
    })
}

/// Dispatch `hangar/daemon_config_get` (D13): read one `daemon_config` value by
/// key. A read — an unknown key returns `value = None` (the caller applies its
/// coded default) rather than erroring. A blank key is a client error. Split out
/// of [`handle`] to keep that dispatcher within the line cap.
async fn handle_daemon_config_get(
    pool: &SqlitePool,
    req: &RpcRequest,
) -> Result<serde_json::Value, RpcError> {
    use ainb_hangar_store::repo::daemon_config::DaemonConfigRepo;

    let params: ainb_hangar_proto::snapshots::DaemonConfigGetParams = parse_params(req, "{ key }")?;
    let key = params.key.trim();
    if key.is_empty() {
        return Err(invalid_params("daemon_config key must not be empty"));
    }
    let value = DaemonConfigRepo::get(pool, key).await.map_err(|e| store_err(&e))?;
    to_value(&ainb_hangar_proto::snapshots::DaemonConfigGetResult {
        key: key.to_string(),
        value,
    })
}

/// The largest `daemon_config` value the set RPC will look at. Every registry
/// kind (bool / bounded int / enum token) is far shorter, so this only bounds
/// absurd input, never a legal one.
const MAX_DAEMON_CONFIG_VALUE_LEN: usize = 256;

/// Dispatch `hangar/daemon_config_set` (D13): write one `daemon_config` value by
/// key. Mutating + idempotent (re-writing the same value is a no-op replace). A
/// blank key is a client error. Split out of [`handle`] to keep that dispatcher
/// within the line cap.
async fn handle_daemon_config_set(
    pool: &SqlitePool,
    req: &RpcRequest,
) -> Result<serde_json::Value, RpcError> {
    use ainb_hangar_store::repo::daemon_config::DaemonConfigRepo;

    let params: ainb_hangar_proto::snapshots::DaemonConfigSetParams =
        parse_params(req, "{ key, value }")?;
    let key = params.key.trim();
    if key.is_empty() {
        return Err(invalid_params("daemon_config key must not be empty"));
    }
    // Bound the value before doing anything with it. Every registry kind (bool /
    // bounded int / enum) rejects a long value anyway, so this cannot change which
    // values are accepted — it just stops an absurd payload being echoed back in a
    // rejection message. (The allocation itself already happened during JSON
    // parsing; a true bound belongs at the frame layer, not here.)
    if params.value.len() > MAX_DAEMON_CONFIG_VALUE_LEN {
        return Err(invalid_params(&format!(
            "daemon_config value must be at most {MAX_DAEMON_CONFIG_VALUE_LEN} bytes"
        )));
    }
    // Every write passes the registry's descriptor gate — the SAME gate the CLI
    // uses — so an out-of-range int / bad bool / unknown enum is rejected
    // identically on both legs, and the stored string is the canonical form the
    // daemon's typed accessors decode.
    //
    // An unknown key is REJECTED rather than passed through. This used to be a
    // generic escape hatch, which meant the two legs of the "single gate"
    // disagreed: the CLI refused `unknown config key`, the RPC silently stored it.
    // The daemon's own internal state (`card_agent.last_used`) is written straight
    // through DaemonConfigRepo in-process and never travels this RPC, so nothing
    // legitimate needs the hatch.
    let desc = ainb_hangar_core::daemon_config::descriptor(key)
        .ok_or_else(|| invalid_params(&format!("unknown config key `{key}`")))?;
    let value = desc.validate(&params.value).map_err(|e| invalid_params(&e))?;
    DaemonConfigRepo::set(pool, key, &value).await.map_err(|e| store_err(&e))?;
    to_value(&ainb_hangar_proto::snapshots::DaemonConfigSetResult {
        key: key.to_string(),
        value,
    })
}

/// Dispatch `hangar/daemon_config_list`: read every user-config knob's stored
/// value in one round trip. Iterates
/// [`ainb_hangar_core::daemon_config::DAEMON_CONFIG_REGISTRY`] so a new registry
/// knob is listed without any handler change; a key with no row reports `value =
/// None` (the caller applies the descriptor's coded default).
async fn handle_daemon_config_list(pool: &SqlitePool) -> Result<serde_json::Value, RpcError> {
    use ainb_hangar_core::daemon_config::DAEMON_CONFIG_REGISTRY;
    use ainb_hangar_store::repo::daemon_config::DaemonConfigRepo;

    let mut entries = Vec::with_capacity(DAEMON_CONFIG_REGISTRY.len());
    for desc in DAEMON_CONFIG_REGISTRY {
        let value = DaemonConfigRepo::get(pool, desc.key).await.map_err(|e| store_err(&e))?;
        entries.push(ainb_hangar_proto::snapshots::DaemonConfigEntry {
            key: desc.key.to_string(),
            value,
        });
    }
    to_value(&ainb_hangar_proto::snapshots::DaemonConfigListResult { entries })
}

/// Dispatch `atc/unregister` (spec P9, D12): disable a registered ATC instance's
/// heartbeat cron. The daemon-native counterpart to `ainb fleet atc teardown`'s
/// timer removal — flips `enabled = 0` and clears `next_tick_at` (via
/// `set_enabled(false, None)`) so `list_schedulable` stops returning it, leaving
/// the instance's audit + retry-ledger rows intact. A blank name is a client
/// error; an unknown name is a no-op (`disabled = false`). Idempotent. Split out
/// of [`handle`] to keep that dispatcher within the line cap.
async fn handle_atc_unregister(
    pool: &SqlitePool,
    req: &RpcRequest,
) -> Result<serde_json::Value, RpcError> {
    use ainb_hangar_store::repo::atc_instance::AtcInstanceRepo;

    let params: ainb_hangar_proto::snapshots::AtcUnregisterParams = parse_params(req, "{ name }")?;
    let name = params.name.trim();
    if name.is_empty() {
        return Err(invalid_params("atc instance name must not be empty"));
    }
    // Only a registered instance is disabled; an unknown name is a clean no-op so
    // teardown is safe to fire unconditionally.
    let disabled = AtcInstanceRepo::get(pool, name).await.map_err(|e| store_err(&e))?.is_some();
    if disabled {
        AtcInstanceRepo::set_enabled(pool, name, false, None)
            .await
            .map_err(|e| store_err(&e))?;
    }
    to_value(&ainb_hangar_proto::snapshots::AtcUnregisterResult {
        name: name.to_string(),
        disabled,
    })
}

/// Build the [`DaemonHealthSnapshot`] for the `hangar/daemon_health` pane (P8.5).
///
/// Mixes real read-model state (the workspace's registered runtimes + its
/// concurrent-task count, both empty/zero for an unknown workspace) with the
/// daemon's in-memory stats (the rolling 60-second throughput window and the
/// claim-cache figure, whose `used` mirrors the concurrent count).
///
/// The throughput window is rendered against the clock's current second so it
/// slides forward even during a quiet period.
async fn daemon_health_snapshot(
    pool: &SqlitePool,
    health: &DaemonHealth,
    workspace_id: Option<&str>,
    clock: &dyn HangarClock,
) -> Result<DaemonHealthSnapshot, sqlx::Error> {
    let (runtimes, concurrent_tasks) = match workspace_id {
        Some(ws) => (
            snapshots::runtime_health(pool, ws, health.pid).await?,
            snapshots::concurrent_task_count(pool, ws).await?,
        ),
        None => (Vec::new(), 0),
    };
    let now_sec = clock.now_ms() / 1_000;
    Ok(DaemonHealthSnapshot {
        runtimes,
        claim_cache: health.stats.claim_cache(concurrent_tasks),
        concurrent_tasks,
        task_throughput_60s: health.stats.throughput_window(now_sec),
        daemon_version: health.version.clone(),
        // Live drift probe: a stale daemon serving a newer database (or a dead
        // database file) must surface as a loud banner, not silent zero stats.
        db_error: ainb_hangar_store::schema_drift(pool).await,
    })
}

/// Resolve a wire workspace id, rejecting an unknown workspace with an
/// `INVALID_PARAMS` error (the mutating autopilot handlers must not silently
/// no-op on a typo'd workspace).
async fn resolve_wire_or_reject(pool: &SqlitePool, wire: &str) -> Result<WorkspaceId, RpcError> {
    resolve_wire(pool, wire)
        .await?
        .ok_or_else(|| invalid_params(&format!("unknown workspace `{wire}`")))
}

/// Resolve `wire` to a workspace, lazily laying down the DEFAULT workspace when
/// it is unresolved AND the caller meant the default (an empty wire, or the
/// literal default slug) — the fresh-home / TUI create path, which must
/// ensure-then-resolve rather than reject a not-yet-materialised default.
///
/// A non-empty, non-default wire that resolves to nothing is still rejected
/// (`INVALID_PARAMS`) — a typo'd or foreign workspace must never be silently
/// bootstrapped into existence.
async fn resolve_or_bootstrap_default(
    pool: &SqlitePool,
    wire: &str,
) -> Result<WorkspaceId, RpcError> {
    if let Some(ws) = resolve_wire(pool, wire).await? {
        return Ok(ws);
    }
    let means_default =
        wire.is_empty() || wire == ainb_hangar_store::bootstrap::DEFAULT_WORKSPACE_SLUG;
    if !means_default {
        return Err(invalid_params(&format!("unknown workspace `{wire}`")));
    }
    ainb_hangar_store::bootstrap::ensure_default_workspace(pool)
        .await
        .map_err(|e| store_err(&e))?;
    resolve_wire_or_reject(pool, ainb_hangar_store::bootstrap::DEFAULT_WORKSPACE_SLUG).await
}

/// Serialize a result payload to a JSON value, mapping a (near-impossible)
/// serialize fault to an internal error.
fn to_value<T: serde::Serialize>(value: &T) -> Result<serde_json::Value, RpcError> {
    serde_json::to_value(value).map_err(|e| RpcError {
        code: INTERNAL_ERROR,
        message: format!("serialize result: {e}"),
        data: None,
    })
}

/// Map a store/sqlx error onto an internal-error envelope.
fn store_err(e: &sqlx::Error) -> RpcError {
    RpcError {
        code: INTERNAL_ERROR,
        message: format!("store error: {e}"),
        data: None,
    }
}

/// Build a success response echoing `id`.
fn ok(id: RpcId, result: serde_json::Value) -> RpcResponse {
    RpcResponse {
        jsonrpc: ainb_hangar_proto::jsonrpc_version(),
        id,
        result: Some(result),
        error: None,
    }
}

/// Frame a response in a Content-Length envelope.
fn encode_frame(resp: &RpcResponse) -> Vec<u8> {
    let body = serde_json::to_vec(resp).unwrap_or_else(|_| b"{}".to_vec());
    let header = format!("Content-Length: {}\r\n\r\n", body.len());
    let mut out = Vec::with_capacity(header.len() + body.len());
    out.extend_from_slice(header.as_bytes());
    out.extend_from_slice(&body);
    out
}

/// Read one Content-Length frame body from an async reader. `None` on clean EOF.
async fn read_frame<R: tokio::io::AsyncBufRead + Unpin>(
    r: &mut R,
) -> std::io::Result<Option<Vec<u8>>> {
    use tokio::io::AsyncBufReadExt;
    let mut content_length: Option<usize> = None;
    loop {
        let mut line = String::new();
        let n = r.read_line(&mut line).await?;
        if n == 0 {
            return Ok(None);
        }
        let trimmed = line.trim_end_matches("\r\n");
        if trimmed.is_empty() {
            let Some(len) = content_length else {
                // Blank line with no Content-Length seen — skip (lenient).
                continue;
            };
            if len > MAX_BODY_BYTES {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("Content-Length {len} exceeds cap {MAX_BODY_BYTES}"),
                ));
            }
            let mut body = vec![0u8; len];
            r.read_exact(&mut body).await?;
            return Ok(Some(body));
        }
        if let Some((name, value)) = trimmed.split_once(':') {
            if name.trim().eq_ignore_ascii_case("Content-Length") {
                content_length = value.trim().parse().ok();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ainb_hangar_store::Store;

    fn health() -> DaemonHealth {
        DaemonHealth {
            socket_path: "/tmp/hangar.sock".into(),
            pid: 42,
            started_at: Instant::now(),
            version: "0.1.0".into(),
            stats: Arc::new(HealthStats::default()),
        }
    }

    /// A throwaway event sink (no subscribers — emissions are dropped).
    fn sink() -> EventSink {
        EventBroker::new().sink()
    }

    /// A PR-status provider that never shells `gh` — the seam for `tasks_list`
    /// snapshots in tests whose cards carry no PR (so it is never even called).
    /// `Arc`-boxed to match `tasks_list`'s shared-provider signature (le3).
    fn no_pr() -> std::sync::Arc<dyn crate::pr_status::PrStatusProvider> {
        std::sync::Arc::new(crate::pr_status::FakePrStatusProvider::new(
            ainb_hangar_proto::pr_status::PrStatus::default(),
        ))
    }

    fn req(method: &str, params: serde_json::Value) -> RpcRequest {
        RpcRequest {
            jsonrpc: ainb_hangar_proto::jsonrpc_version(),
            id: RpcId::Number(1),
            method: method.into(),
            params,
        }
    }

    #[tokio::test]
    async fn ping_acks() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let resp = dispatch(
            store.pool(),
            &req(methods::PING, serde_json::Value::Null),
            &health(),
            &sink(),
        )
        .await;
        assert!(resp.error.is_none());
        assert_eq!(resp.id, RpcId::Number(1));
    }

    #[tokio::test]
    async fn subscribe_acks_with_snapshot_envelope() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let resp = dispatch(
            store.pool(),
            &req(
                methods::WORKSPACE_SUBSCRIBE,
                serde_json::json!({"workspace_id":"default"}),
            ),
            &health(),
            &sink(),
        )
        .await;
        assert!(resp.error.is_none(), "subscribe must ack: {resp:?}");
        assert!(resp.result.unwrap().get("snapshot").is_some());
    }

    #[tokio::test]
    async fn unknown_method_is_method_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let resp = dispatch(
            store.pool(),
            &req("nope/nope", serde_json::Value::Null),
            &health(),
            &sink(),
        )
        .await;
        assert_eq!(resp.error.unwrap().code, METHOD_NOT_FOUND);
    }

    /// P5 end-to-end over `dispatch`: `profile/upsert` writes the master + indexes
    /// it, `profile/get` returns the parsed fields + BOTH compile previews (Claude
    /// lossless with the tier resolved, Codex lossy with dropped-field warnings),
    /// and `profile/list` shows the indexed row. Home-isolated so the write lands
    /// under a tempdir, never the operator's real `~/.agents-in-a-box`.
    #[test]
    fn profile_upsert_get_list_over_dispatch() {
        ainb_hangar_store::test_support::with_isolated_home(|home| {
            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
            rt.block_on(async {
                let store = Store::open_in(home).await.unwrap();
                let pool = store.pool();

                // Upsert a profile with a Codex-incompatible field set.
                let up = dispatch(
                    pool,
                    &req(
                        methods::PROFILE_UPSERT,
                        serde_json::json!({
                            "slug": "code-reviewer",
                            "description": "Reviews a diff",
                            "tier": "premium",
                            "tools": ["Read", "Grep"],
                            "color": "cyan",
                            "body": "You are a reviewer."
                        }),
                    ),
                    &health(),
                    &sink(),
                )
                .await;
                assert!(up.error.is_none(), "upsert must ack: {up:?}");
                assert!(up.result.unwrap()["path"].as_str().unwrap().ends_with("code-reviewer.md"));

                // Get returns the parsed fields + both previews.
                let got = dispatch(
                    pool,
                    &req(
                        methods::PROFILE_GET,
                        serde_json::json!({"slug": "code-reviewer"}),
                    ),
                    &health(),
                    &sink(),
                )
                .await;
                let got = got.result.expect("get result");
                assert_eq!(got["found"], true);
                assert_eq!(got["tier"], "premium");
                assert!(
                    got["claude_preview"].as_str().unwrap().contains("model: opus"),
                    "Claude preview resolves the tier"
                );
                assert!(
                    got["codex_preview"]["config_fragment"]
                        .as_str()
                        .unwrap()
                        .contains("model = \"gpt-5\""),
                    "Codex preview resolves the tier"
                );
                assert_eq!(
                    got["codex_preview"]["warnings"].as_array().unwrap().len(),
                    2,
                    "Codex drops tools + color with a warning each"
                );

                // List shows the indexed row.
                let list = dispatch(
                    pool,
                    &req(methods::PROFILE_LIST, serde_json::json!({})),
                    &health(),
                    &sink(),
                )
                .await
                .result
                .expect("list result");
                let profiles = list["profiles"].as_array().unwrap();
                assert_eq!(profiles.len(), 1);
                assert_eq!(profiles[0]["slug"], "code-reviewer");
                assert_eq!(profiles[0]["tier"], "premium");
            });
        });
    }

    /// `profile/get` on an unknown slug is a read miss, not an error.
    #[test]
    fn profile_get_unknown_slug_is_not_found() {
        ainb_hangar_store::test_support::with_isolated_home(|home| {
            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
            rt.block_on(async {
                let store = Store::open_in(home).await.unwrap();
                let got = dispatch(
                    store.pool(),
                    &req(methods::PROFILE_GET, serde_json::json!({"slug": "ghost"})),
                    &health(),
                    &sink(),
                )
                .await;
                assert!(got.error.is_none());
                assert_eq!(got.result.unwrap()["found"], false);
            });
        });
    }

    /// `profile/upsert` rejects an invalid slug with `INVALID_PARAMS` (never
    /// writes a malformed master path).
    #[test]
    fn profile_upsert_rejects_bad_slug() {
        ainb_hangar_store::test_support::with_isolated_home(|home| {
            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
            rt.block_on(async {
                let store = Store::open_in(home).await.unwrap();
                let resp = dispatch(
                    store.pool(),
                    &req(
                        methods::PROFILE_UPSERT,
                        serde_json::json!({"slug": "Bad_Slug", "tier": "fast"}),
                    ),
                    &health(),
                    &sink(),
                )
                .await;
                assert_eq!(resp.error.unwrap().code, INVALID_PARAMS);
            });
        });
    }

    #[tokio::test]
    async fn health_reports_socket_and_connected() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let resp = dispatch(
            store.pool(),
            &req(methods::HANGAR_HEALTH, serde_json::json!({})),
            &health(),
            &sink(),
        )
        .await;
        let v = resp.result.unwrap();
        assert_eq!(v["socket_path"], "/tmp/hangar.sock");
        assert_eq!(v["connected"], true);
        assert_eq!(v["pid"], 42);
    }

    #[tokio::test]
    async fn issues_list_missing_workspace_id_is_invalid_params() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let resp = dispatch(
            store.pool(),
            &req(methods::HANGAR_ISSUES_LIST, serde_json::json!({})),
            &health(),
            &sink(),
        )
        .await;
        assert_eq!(resp.error.unwrap().code, INVALID_PARAMS);
    }

    /// `hangar/issue_update` edits a seeded issue's fields through the
    /// dispatcher and answers with the refreshed row (e38.8).
    #[tokio::test]
    async fn issue_update_edits_seeded_issue() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        crate::seed::seed_p4_fixture(store.pool()).await.unwrap();
        let resp = dispatch(
            store.pool(),
            &req(
                methods::HANGAR_ISSUE_UPDATE,
                serde_json::json!({
                    "workspace_id": "default",
                    "issue_id": "issue-1",
                    "state": "done",
                    "priority": 2,
                }),
            ),
            &health(),
            &sink(),
        )
        .await;
        assert!(resp.error.is_none(), "{resp:?}");
        let v = resp.result.unwrap();
        assert_eq!(v["id"], "issue-1");
        assert_eq!(v["state"], "done");
        assert_eq!(v["priority"], 2);
    }

    /// A malformed assignee ref is an `INVALID_PARAMS` client error, not a store
    /// fault — the mapper rejects it before any write.
    #[tokio::test]
    async fn issue_update_malformed_assignee_is_invalid_params() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        crate::seed::seed_p4_fixture(store.pool()).await.unwrap();
        let resp = dispatch(
            store.pool(),
            &req(
                methods::HANGAR_ISSUE_UPDATE,
                serde_json::json!({
                    "workspace_id": "default",
                    "issue_id": "issue-1",
                    "assignee": "not-an-actor-ref",
                }),
            ),
            &health(),
            &sink(),
        )
        .await;
        assert_eq!(resp.error.unwrap().code, INVALID_PARAMS);
    }

    /// An unknown issue id is rejected (not a silent no-op), mirroring the
    /// mutating workspace-reject contract.
    #[tokio::test]
    async fn issue_update_unknown_issue_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        crate::seed::seed_p4_fixture(store.pool()).await.unwrap();
        let resp = dispatch(
            store.pool(),
            &req(
                methods::HANGAR_ISSUE_UPDATE,
                serde_json::json!({
                    "workspace_id": "default",
                    "issue_id": "no-such-issue",
                    "state": "done",
                }),
            ),
            &health(),
            &sink(),
        )
        .await;
        assert_eq!(resp.error.unwrap().code, INVALID_PARAMS);
    }

    /// `hangar/issue_create` creates a new issue through the dispatcher, answers
    /// with the persisted row, and the row actually lands in the `issue` table
    /// (e38.29).
    #[tokio::test]
    async fn issue_create_lands_new_issue() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        crate::seed::seed_p4_fixture(store.pool()).await.unwrap();
        let resp = dispatch(
            store.pool(),
            &req(
                methods::HANGAR_ISSUE_CREATE,
                serde_json::json!({
                    "workspace_id": "default",
                    "title": "Ship the create flow",
                    "creator": "member:me",
                }),
            ),
            &health(),
            &sink(),
        )
        .await;
        assert!(resp.error.is_none(), "{resp:?}");
        let v = resp.result.unwrap();
        assert_eq!(v["title"], "Ship the create flow");
        assert_eq!(v["state"], "open");
        assert_eq!(v["creator"], "member:me");
        // The real proof: the row is in the DB, not just echoed in the response.
        let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM issue WHERE title = ?")
            .bind("Ship the create flow")
            .fetch_one(store.pool())
            .await
            .unwrap();
        assert_eq!(n, 1, "created issue not found in the DB");
    }

    /// e38.21: a workspace's configured `issue_prefix` is applied to a created
    /// issue's title — the prefix actually takes effect (the response row AND the
    /// stored DB row both carry it), not just that the column stores a value.
    #[tokio::test]
    async fn issue_create_applies_workspace_issue_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        crate::seed::seed_p4_fixture(store.pool()).await.unwrap();
        // Configure the seeded workspace with an issue prefix.
        sqlx::query("UPDATE workspace SET issue_prefix = ? WHERE id = ?")
            .bind("[OPS] ")
            .bind(crate::seed::WS_ID)
            .execute(store.pool())
            .await
            .unwrap();

        let resp = dispatch(
            store.pool(),
            &req(
                methods::HANGAR_ISSUE_CREATE,
                serde_json::json!({
                    "workspace_id": "default",
                    "title": "fix the build",
                    "creator": "member:me",
                }),
            ),
            &health(),
            &sink(),
        )
        .await;
        assert!(resp.error.is_none(), "{resp:?}");
        // The response row carries the prefixed title (so does the IssueCreated
        // event — it is built from the same row).
        assert_eq!(
            resp.result.unwrap()["title"],
            "[OPS] fix the build",
            "the created issue's title must carry the workspace prefix"
        );
        // The real proof: the stored row carries the prefixed title, not the bare
        // input title.
        let stored: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM issue WHERE title = ?")
            .bind("[OPS] fix the build")
            .fetch_one(store.pool())
            .await
            .unwrap();
        assert_eq!(stored, 1, "the prefixed title must be persisted");
        let bare: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM issue WHERE title = ?")
            .bind("fix the build")
            .fetch_one(store.pool())
            .await
            .unwrap();
        assert_eq!(bare, 0, "the bare (unprefixed) title must not be stored");
    }

    /// A blank title is an `INVALID_PARAMS` client error, not an empty row.
    #[tokio::test]
    async fn issue_create_blank_title_is_invalid_params() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        crate::seed::seed_p4_fixture(store.pool()).await.unwrap();
        let resp = dispatch(
            store.pool(),
            &req(
                methods::HANGAR_ISSUE_CREATE,
                serde_json::json!({
                    "workspace_id": "default",
                    "title": "   ",
                    "creator": "member:me",
                }),
            ),
            &health(),
            &sink(),
        )
        .await;
        assert_eq!(resp.error.unwrap().code, INVALID_PARAMS);
    }

    /// An unknown workspace is rejected (not a silent no-op), mirroring the
    /// mutating workspace-reject contract.
    #[tokio::test]
    async fn issue_create_unknown_workspace_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        crate::seed::seed_p4_fixture(store.pool()).await.unwrap();
        let resp = dispatch(
            store.pool(),
            &req(
                methods::HANGAR_ISSUE_CREATE,
                serde_json::json!({
                    "workspace_id": "no-such-ws",
                    "title": "orphan",
                    "creator": "member:me",
                }),
            ),
            &health(),
            &sink(),
        )
        .await;
        assert_eq!(resp.error.unwrap().code, INVALID_PARAMS);
    }

    /// `hangar/comment_add` appends a comment to a seeded issue through the
    /// dispatcher and answers with the persisted row (e38.5).
    #[tokio::test]
    async fn comment_add_appends_to_seeded_issue() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        crate::seed::seed_p4_fixture(store.pool()).await.unwrap();
        let resp = dispatch(
            store.pool(),
            &req(
                methods::HANGAR_COMMENT_ADD,
                serde_json::json!({
                    "workspace_id": "default",
                    "issue_id": "issue-1",
                    "author": "member:user-1",
                    "body": "looks good",
                }),
            ),
            &health(),
            &sink(),
        )
        .await;
        assert!(resp.error.is_none(), "{resp:?}");
        let v = resp.result.unwrap();
        assert_eq!(v["issue_id"], "issue-1");
        assert_eq!(v["author"], "member:user-1");
        assert_eq!(v["body"], "looks good");
    }

    /// A blank comment body is an `INVALID_PARAMS` client error — never an empty
    /// persisted row.
    #[tokio::test]
    async fn comment_add_empty_body_is_invalid_params() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        crate::seed::seed_p4_fixture(store.pool()).await.unwrap();
        let resp = dispatch(
            store.pool(),
            &req(
                methods::HANGAR_COMMENT_ADD,
                serde_json::json!({
                    "workspace_id": "default",
                    "issue_id": "issue-1",
                    "author": "member:user-1",
                    "body": "   ",
                }),
            ),
            &health(),
            &sink(),
        )
        .await;
        assert_eq!(resp.error.unwrap().code, INVALID_PARAMS);
    }

    /// An unknown issue id is rejected (not a silent no-op), mirroring the
    /// mutating workspace-reject contract.
    #[tokio::test]
    async fn comment_add_unknown_issue_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        crate::seed::seed_p4_fixture(store.pool()).await.unwrap();
        let resp = dispatch(
            store.pool(),
            &req(
                methods::HANGAR_COMMENT_ADD,
                serde_json::json!({
                    "workspace_id": "default",
                    "issue_id": "no-such-issue",
                    "author": "member:user-1",
                    "body": "hi",
                }),
            ),
            &health(),
            &sink(),
        )
        .await;
        assert_eq!(resp.error.unwrap().code, INVALID_PARAMS);
    }

    /// `hangar/agent_update` edits a seeded agent's config through the dispatcher
    /// and answers with the refreshed actor row (e38.15).
    #[tokio::test]
    async fn agent_update_edits_seeded_agent() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        crate::seed::seed_p4_fixture(store.pool()).await.unwrap();
        let resp = dispatch(
            store.pool(),
            &req(
                methods::HANGAR_AGENT_UPDATE,
                serde_json::json!({
                    "workspace_id": "default",
                    "agent_id": "agent-1",
                    "name": "claude-pro",
                    "model": "claude-opus-4",
                }),
            ),
            &health(),
            &sink(),
        )
        .await;
        assert!(resp.error.is_none(), "{resp:?}");
        let v = resp.result.unwrap();
        assert_eq!(v["actor_ref"], "agent:agent-1");
        assert_eq!(v["display_name"], "claude-pro");
    }

    /// An unknown agent id is rejected (not a silent no-op), mirroring the
    /// mutating workspace-reject contract.
    #[tokio::test]
    async fn agent_update_unknown_agent_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        crate::seed::seed_p4_fixture(store.pool()).await.unwrap();
        let resp = dispatch(
            store.pool(),
            &req(
                methods::HANGAR_AGENT_UPDATE,
                serde_json::json!({
                    "workspace_id": "default",
                    "agent_id": "no-such-agent",
                    "name": "ghost",
                }),
            ),
            &health(),
            &sink(),
        )
        .await;
        assert_eq!(resp.error.unwrap().code, INVALID_PARAMS);
    }

    /// `hangar/agent_archive` flips the flag through the dispatcher and answers
    /// with the refreshed actor row (e38.15).
    #[tokio::test]
    async fn agent_archive_flips_seeded_agent() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        crate::seed::seed_p4_fixture(store.pool()).await.unwrap();
        let resp = dispatch(
            store.pool(),
            &req(
                methods::HANGAR_AGENT_ARCHIVE,
                serde_json::json!({
                    "workspace_id": "default",
                    "agent_id": "agent-1",
                    "archived": true,
                }),
            ),
            &health(),
            &sink(),
        )
        .await;
        assert!(resp.error.is_none(), "{resp:?}");
        assert_eq!(resp.result.unwrap()["actor_ref"], "agent:agent-1");
    }

    /// `hangar/agent_delete` removes a fresh (never-run) agent through the
    /// dispatcher and answers with the refreshed roster no longer carrying it. The
    /// agent is created via `agent_create` first so it has no FK-pinned history.
    #[tokio::test]
    async fn agent_delete_removes_a_fresh_agent() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        crate::seed::seed_p4_fixture(store.pool()).await.unwrap();

        // Create a throwaway agent, then read its id back off the refreshed roster.
        let created = dispatch(
            store.pool(),
            &req(
                methods::HANGAR_AGENT_CREATE,
                serde_json::json!({ "workspace_id": "default", "name": "throwaway" }),
            ),
            &health(),
            &sink(),
        )
        .await;
        assert!(created.error.is_none(), "{created:?}");
        let actors = created.result.unwrap();
        let new_ref = actors["actors"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["display_name"] == "throwaway")
            .expect("created agent is on the roster")["actor_ref"]
            .as_str()
            .unwrap()
            .to_string();
        let new_id = new_ref.strip_prefix("agent:").unwrap();

        let resp = dispatch(
            store.pool(),
            &req(
                methods::HANGAR_AGENT_DELETE,
                serde_json::json!({ "workspace_id": "default", "agent_id": new_id }),
            ),
            &health(),
            &sink(),
        )
        .await;
        assert!(resp.error.is_none(), "{resp:?}");
        let roster = resp.result.unwrap();
        let still_there = roster["actors"]
            .as_array()
            .unwrap()
            .iter()
            .any(|a| a["display_name"] == "throwaway");
        assert!(
            !still_there,
            "the deleted agent must be gone from the roster"
        );
    }

    /// The guided-create wire (gap #9) persists the FULL structured draft:
    /// `agent_create` with provider + model + instructions writes all three onto
    /// the row, not just the name — the load-bearing proof the widened wire lands.
    #[tokio::test]
    async fn agent_create_persists_provider_model_and_instructions() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        crate::seed::seed_p4_fixture(store.pool()).await.unwrap();

        let created = dispatch(
            store.pool(),
            &req(
                methods::HANGAR_AGENT_CREATE,
                serde_json::json!({
                    "workspace_id": "default",
                    "name": "guidedbot",
                    "provider": "codex",
                    "model": "gpt-5-codex",
                    "instructions": "be terse",
                }),
            ),
            &health(),
            &sink(),
        )
        .await;
        assert!(created.error.is_none(), "{created:?}");

        let row: (String, Option<String>, Option<String>) = sqlx::query_as(
            "SELECT provider, model, instructions FROM agent WHERE name = 'guidedbot'",
        )
        .fetch_one(store.pool())
        .await
        .unwrap();
        assert_eq!(row.0, "codex", "provider persisted");
        assert_eq!(row.1.as_deref(), Some("gpt-5-codex"), "model persisted");
        assert_eq!(row.2.as_deref(), Some("be terse"), "instructions persisted");
    }

    /// A create that omits `model` leaves the column NULL — no spurious
    /// empty-string write from the create-time follow-up.
    #[tokio::test]
    async fn agent_create_without_model_leaves_model_null() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        crate::seed::seed_p4_fixture(store.pool()).await.unwrap();

        let created = dispatch(
            store.pool(),
            &req(
                methods::HANGAR_AGENT_CREATE,
                serde_json::json!({
                    "workspace_id": "default",
                    "name": "nomodelbot",
                    "provider": "claude",
                }),
            ),
            &health(),
            &sink(),
        )
        .await;
        assert!(created.error.is_none(), "{created:?}");

        let model: Option<String> =
            sqlx::query_scalar("SELECT model FROM agent WHERE name = 'nomodelbot'")
                .fetch_one(store.pool())
                .await
                .unwrap();
        assert!(model.is_none(), "model stays NULL when not supplied");
    }

    /// An unknown agent id is rejected (not a silent no-op), mirroring the mutating
    /// workspace-reject contract.
    #[tokio::test]
    async fn agent_delete_unknown_agent_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        crate::seed::seed_p4_fixture(store.pool()).await.unwrap();
        let resp = dispatch(
            store.pool(),
            &req(
                methods::HANGAR_AGENT_DELETE,
                serde_json::json!({ "workspace_id": "default", "agent_id": "no-such-agent" }),
            ),
            &health(),
            &sink(),
        )
        .await;
        assert_eq!(resp.error.unwrap().code, INVALID_PARAMS);
    }

    /// `hangar/skill_get` returns the seeded `commit` skill's detail, scoped to
    /// the subscribed workspace.
    #[tokio::test]
    async fn skill_get_returns_detail_for_seeded_skill() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        crate::seed::seed_p4_fixture(store.pool()).await.unwrap();
        let resp = dispatch(
            store.pool(),
            &req(
                methods::HANGAR_SKILL_GET,
                serde_json::json!({"workspace_id":"default","skill_id":"skill-commit"}),
            ),
            &health(),
            &sink(),
        )
        .await;
        assert!(resp.error.is_none(), "{resp:?}");
        let v = resp.result.unwrap();
        assert_eq!(v["slug"], "skill-commit");
        assert_eq!(v["name"], "commit");
    }

    /// A skill id from another workspace resolves to `null` (tenant isolation),
    /// never another tenant's body.
    #[tokio::test]
    async fn skill_get_foreign_workspace_is_null() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        crate::seed::seed_p4_fixture(store.pool()).await.unwrap();
        let resp = dispatch(
            store.pool(),
            &req(
                methods::HANGAR_SKILL_GET,
                serde_json::json!({"workspace_id":"nope","skill_id":"skill-commit"}),
            ),
            &health(),
            &sink(),
        )
        .await;
        assert!(resp.error.is_none());
        assert!(resp.result.unwrap().is_null());
    }

    /// `hangar/skill_attach` then `hangar/skill_detach` toggle the junction for a
    /// seeded agent + the unused `review` skill.
    #[tokio::test]
    async fn skill_attach_then_detach_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        crate::seed::seed_p4_fixture(store.pool()).await.unwrap();

        let attach = dispatch(
            store.pool(),
            &req(
                methods::HANGAR_SKILL_ATTACH,
                serde_json::json!({"workspace_id":"default","agent_id":"agent-1","skill_id":"skill-review"}),
            ),
            &health(),
            &sink(),
        )
        .await;
        assert!(attach.error.is_none(), "{attach:?}");
        // `review` is now used.
        let skills = snapshots::skills_list(store.pool(), crate::seed::WS_ID).await.unwrap();
        assert!(skills.iter().any(|s| s.name == "review" && s.used));

        let detach = dispatch(
            store.pool(),
            &req(
                methods::HANGAR_SKILL_DETACH,
                serde_json::json!({"workspace_id":"default","agent_id":"agent-1","skill_id":"skill-review"}),
            ),
            &health(),
            &sink(),
        )
        .await;
        assert!(detach.error.is_none(), "{detach:?}");
        let skills = snapshots::skills_list(store.pool(), crate::seed::WS_ID).await.unwrap();
        assert!(skills.iter().any(|s| s.name == "review" && !s.used));
    }

    /// A cross-workspace attach (foreign agent id) is rejected with
    /// `INVALID_PARAMS` and writes nothing.
    #[tokio::test]
    async fn skill_attach_cross_workspace_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        crate::seed::seed_p4_fixture(store.pool()).await.unwrap();
        let resp = dispatch(
            store.pool(),
            &req(
                methods::HANGAR_SKILL_ATTACH,
                serde_json::json!({"workspace_id":"default","agent_id":"nonexistent-agent","skill_id":"skill-commit"}),
            ),
            &health(),
            &sink(),
        )
        .await;
        assert_eq!(resp.error.unwrap().code, INVALID_PARAMS);
    }

    /// `hangar/autopilots_list` returns a seeded autopilot, scoped to the
    /// subscribed workspace, with its latest run's status in `last_run_status`.
    #[tokio::test]
    async fn autopilots_list_returns_seeded_with_last_run() {
        use ainb_hangar_core::clock::FixedClock;
        use ainb_hangar_store::repo::autopilot::{AutopilotRepo, NewAutopilot};

        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        crate::seed::seed_p4_fixture(store.pool()).await.unwrap();

        let ws = WorkspaceId::from_str(crate::seed::WS_ID).unwrap();
        let clock = FixedClock(1_700_000_000_000);
        let ap_id = AutopilotRepo::create(
            store.pool(),
            &clock,
            &NewAutopilot {
                workspace_id: ws.clone(),
                agent_id: AgentId::from_str("agent-1").unwrap(),
                name: "daily-triage".into(),
                instructions: Some("triage".into()),
                cron_expr: "0 9 * * 1-5".into(),
                max_concurrent_runs: 1,
                execution_mode: ainb_hangar_store::repo::autopilot::ExecutionMode::default(),
                concurrency_policy: ainb_hangar_store::repo::autopilot::ConcurrencyPolicy::default(
                ),
            },
        )
        .await
        .unwrap();
        AutopilotRepo::insert_run(store.pool(), &ap_id, 1_699_000_000_000, "completed")
            .await
            .unwrap();

        let resp = dispatch(
            store.pool(),
            &req(
                methods::HANGAR_AUTOPILOTS_LIST,
                serde_json::json!({"workspace_id":"default"}),
            ),
            &health(),
            &sink(),
        )
        .await;
        assert!(resp.error.is_none(), "{resp:?}");
        let v = resp.result.unwrap();
        let rows = v["autopilots"].as_array().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["name"], "daily-triage");
        assert_eq!(rows[0]["cron_expr"], "0 9 * * 1-5");
        assert_eq!(rows[0]["enabled"], true);
        assert_eq!(rows[0]["last_run_status"], "completed");
    }

    /// A foreign workspace yields an empty autopilot list (tenant isolation).
    #[tokio::test]
    async fn autopilots_list_foreign_workspace_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        crate::seed::seed_p4_fixture(store.pool()).await.unwrap();
        let resp = dispatch(
            store.pool(),
            &req(
                methods::HANGAR_AUTOPILOTS_LIST,
                serde_json::json!({"workspace_id":"nope"}),
            ),
            &health(),
            &sink(),
        )
        .await;
        assert!(resp.error.is_none());
        assert_eq!(
            resp.result.unwrap()["autopilots"].as_array().unwrap().len(),
            0
        );
    }

    /// `hangar/autopilot_set_enabled(false)` then `(true)` toggles the row;
    /// `disable` clears the flag, `enable` sets it again.
    #[tokio::test]
    async fn autopilot_set_enabled_toggles_scoped_row() {
        use ainb_hangar_core::clock::FixedClock;
        use ainb_hangar_store::repo::autopilot::{AutopilotRepo, NewAutopilot};

        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        crate::seed::seed_p4_fixture(store.pool()).await.unwrap();
        let ws = WorkspaceId::from_str(crate::seed::WS_ID).unwrap();
        let clock = FixedClock(1_700_000_000_000);
        let ap_id = AutopilotRepo::create(
            store.pool(),
            &clock,
            &NewAutopilot {
                workspace_id: ws.clone(),
                agent_id: AgentId::from_str("agent-1").unwrap(),
                name: "nightly".into(),
                instructions: None,
                cron_expr: "0 2 * * *".into(),
                max_concurrent_runs: 1,
                execution_mode: ainb_hangar_store::repo::autopilot::ExecutionMode::default(),
                concurrency_policy: ainb_hangar_store::repo::autopilot::ConcurrencyPolicy::default(
                ),
            },
        )
        .await
        .unwrap();

        let disable = dispatch(
            store.pool(),
            &req(
                methods::HANGAR_AUTOPILOT_SET_ENABLED,
                serde_json::json!({"workspace_id":"default","autopilot_id":ap_id.as_str(),"enabled":false}),
            ),
            &health(),
            &sink(),
        )
        .await;
        assert!(disable.error.is_none(), "{disable:?}");
        let ap = AutopilotRepo::get(store.pool(), &ws, &ap_id).await.unwrap().unwrap();
        assert!(!ap.enabled, "disable must clear the enabled flag");

        let enable = dispatch(
            store.pool(),
            &req(
                methods::HANGAR_AUTOPILOT_SET_ENABLED,
                serde_json::json!({"workspace_id":"default","autopilot_id":ap_id.as_str(),"enabled":true}),
            ),
            &health(),
            &sink(),
        )
        .await;
        assert!(enable.error.is_none(), "{enable:?}");
        let ap = AutopilotRepo::get(store.pool(), &ws, &ap_id).await.unwrap().unwrap();
        assert!(ap.enabled, "enable must set the enabled flag");
    }

    /// `hangar/autopilot_fire_now` runs the P7.4 enqueue path: a fresh
    /// `autopilot_run` row appears for the seeded autopilot.
    #[tokio::test]
    async fn autopilot_fire_now_creates_a_run() {
        use ainb_hangar_core::clock::FixedClock;
        use ainb_hangar_store::repo::autopilot::{AutopilotRepo, NewAutopilot};

        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        crate::seed::seed_p4_fixture(store.pool()).await.unwrap();
        let ws = WorkspaceId::from_str(crate::seed::WS_ID).unwrap();
        let clock = FixedClock(1_700_000_000_000);
        let ap_id = AutopilotRepo::create(
            store.pool(),
            &clock,
            &NewAutopilot {
                workspace_id: ws.clone(),
                agent_id: AgentId::from_str("agent-1").unwrap(),
                name: "manual".into(),
                instructions: Some("go".into()),
                cron_expr: "0 0 * * *".into(),
                max_concurrent_runs: 1,
                execution_mode: ainb_hangar_store::repo::autopilot::ExecutionMode::default(),
                concurrency_policy: ainb_hangar_store::repo::autopilot::ConcurrencyPolicy::default(
                ),
            },
        )
        .await
        .unwrap();

        let before = AutopilotRepo::list_runs(store.pool(), &ws, &ap_id, 100).await.unwrap().len();
        let fire = dispatch(
            store.pool(),
            &req(
                methods::HANGAR_AUTOPILOT_FIRE_NOW,
                serde_json::json!({"workspace_id":"default","autopilot_id":ap_id.as_str()}),
            ),
            &health(),
            &sink(),
        )
        .await;
        assert!(fire.error.is_none(), "{fire:?}");
        let after = AutopilotRepo::list_runs(store.pool(), &ws, &ap_id, 100).await.unwrap().len();
        assert_eq!(after, before + 1, "fire_now must create one autopilot_run");
    }

    /// `hangar/autopilot_runs` lists the seeded runs latest-first; a foreign
    /// autopilot id yields an empty set (tenant isolation through the join).
    #[tokio::test]
    async fn autopilot_runs_latest_first_and_scoped() {
        use ainb_hangar_core::clock::FixedClock;
        use ainb_hangar_store::repo::autopilot::{AutopilotRepo, NewAutopilot};

        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        crate::seed::seed_p4_fixture(store.pool()).await.unwrap();
        let ws = WorkspaceId::from_str(crate::seed::WS_ID).unwrap();
        let clock = FixedClock(1_700_000_000_000);
        let ap_id = AutopilotRepo::create(
            store.pool(),
            &clock,
            &NewAutopilot {
                workspace_id: ws.clone(),
                agent_id: AgentId::from_str("agent-1").unwrap(),
                name: "weekly".into(),
                instructions: None,
                cron_expr: "0 9 * * MON".into(),
                max_concurrent_runs: 1,
                execution_mode: ainb_hangar_store::repo::autopilot::ExecutionMode::default(),
                concurrency_policy: ainb_hangar_store::repo::autopilot::ConcurrencyPolicy::default(
                ),
            },
        )
        .await
        .unwrap();
        AutopilotRepo::insert_run(store.pool(), &ap_id, 100, "failed").await.unwrap();
        AutopilotRepo::insert_run(store.pool(), &ap_id, 200, "completed").await.unwrap();

        let resp = dispatch(
            store.pool(),
            &req(
                methods::HANGAR_AUTOPILOT_RUNS,
                serde_json::json!({"workspace_id":"default","autopilot_id":ap_id.as_str(),"limit":10}),
            ),
            &health(),
            &sink(),
        )
        .await;
        assert!(resp.error.is_none(), "{resp:?}");
        let runs = resp.result.unwrap()["runs"].as_array().unwrap().clone();
        assert_eq!(runs.len(), 2);
        // Latest-first: the 200-stamped completed run leads.
        assert_eq!(runs[0]["status"], "completed");
        assert_eq!(runs[1]["status"], "failed");

        // A foreign autopilot id yields an empty set, never another tenant's runs.
        let foreign = dispatch(
            store.pool(),
            &req(
                methods::HANGAR_AUTOPILOT_RUNS,
                serde_json::json!({"workspace_id":"default","autopilot_id":"01HANGARNOSUCHAUTOPILOT00","limit":10}),
            ),
            &health(),
            &sink(),
        )
        .await;
        assert!(foreign.error.is_none());
        assert_eq!(foreign.result.unwrap()["runs"].as_array().unwrap().len(), 0);
    }

    /// `hangar/tasks_list` returns the seeded running task, scoped to the
    /// subscribed workspace, carrying its raw lifecycle status.
    #[tokio::test]
    async fn tasks_list_returns_seeded_running_task() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        crate::seed::seed_p4_fixture(store.pool()).await.unwrap();
        let resp = dispatch(
            store.pool(),
            &req(
                methods::HANGAR_TASKS_LIST,
                serde_json::json!({"workspace_id":"default"}),
            ),
            &health(),
            &sink(),
        )
        .await;
        assert!(resp.error.is_none(), "{resp:?}");
        let v = resp.result.unwrap();
        let tasks = v["tasks"].as_array().unwrap();
        assert_eq!(tasks.len(), 1, "the fixture seeds exactly one task");
        assert_eq!(tasks[0]["id"], "task-1");
        assert_eq!(tasks[0]["status"], "running");
        assert_eq!(tasks[0]["agent_id"], "agent-1");
    }

    /// tcp T2: a card surfaces the run's durable artifacts — the recorded
    /// `branch`, the PR captured into its `result`, and the CI + merge status
    /// fetched through the injectable provider (a fake here — never real `gh`).
    #[tokio::test]
    async fn tasks_list_surfaces_branch_pr_and_ci_status() {
        use ainb_hangar_proto::pr_status::{CiRollup, MergeState, Mergeable, PrStatus};
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        crate::seed::seed_p4_fixture(store.pool()).await.unwrap();
        // Record a produced branch + a captured PR on the seeded task, exactly as a
        // committed finalize (branch) + a `gh pr create` capture (result.pr_url) would.
        sqlx::query("UPDATE agent_task_queue SET branch = ?, result = ? WHERE id = 'task-1'")
            .bind("ainb/task-1")
            .bind(r#"{"content":"","pr_url":"https://github.com/o/r/pull/9"}"#)
            .execute(store.pool())
            .await
            .unwrap();
        // A provider that answers a passing, mergeable, open PR — no `gh`, no net.
        let provider = crate::pr_status::FakePrStatusProvider::new(PrStatus {
            ci: CiRollup::Pass,
            mergeable: Mergeable::Mergeable,
            state: MergeState::Open,
        });
        let cards = snapshots::tasks_list(
            store.pool(),
            crate::seed::WS_ID,
            std::sync::Arc::new(provider),
        )
        .await
        .unwrap();
        let card = cards.iter().find(|c| c.id.as_str() == "task-1").unwrap();
        assert_eq!(
            card.branch.as_deref(),
            Some("ainb/task-1"),
            "branch surfaced"
        );
        assert_eq!(
            card.pr_url.as_deref(),
            Some("https://github.com/o/r/pull/9"),
            "captured PR url surfaced"
        );
        assert_eq!(
            card.pr_status.map(|s| s.ci),
            Some(CiRollup::Pass),
            "the PR's CI rollup is fetched and surfaced on the card"
        );
        assert_eq!(
            card.pr_status.map(|s| s.mergeable),
            Some(Mergeable::Mergeable)
        );
    }

    /// A card that captured no PR carries no `pr_url` and no `pr_status` — the
    /// provider is never consulted (a failing fake would still yield `None`).
    #[tokio::test]
    async fn tasks_list_no_pr_card_has_no_status() {
        use ainb_hangar_proto::pr_status::{CiRollup, MergeState, Mergeable, PrStatus};
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        crate::seed::seed_p4_fixture(store.pool()).await.unwrap();
        // A fake that would report Fail if ever consulted — it must NOT be, since
        // the seeded task has no captured pr_url.
        let provider = crate::pr_status::FakePrStatusProvider::new(PrStatus {
            ci: CiRollup::Fail,
            mergeable: Mergeable::Conflicting,
            state: MergeState::Closed,
        });
        let cards = snapshots::tasks_list(
            store.pool(),
            crate::seed::WS_ID,
            std::sync::Arc::new(provider),
        )
        .await
        .unwrap();
        let card = cards.iter().find(|c| c.id.as_str() == "task-1").unwrap();
        assert_eq!(card.pr_url, None, "no PR captured");
        assert_eq!(card.pr_status, None, "no PR → no status fetched");
    }

    /// An issue row surfaces its latest completed task's `branch` (ch3), mirroring
    /// the `pr_url` derivation — so the task-detail opened FROM THE ISSUE LIST (a
    /// synthetic task with no per-run branch) can render the run-branch line.
    #[tokio::test]
    async fn issue_row_surfaces_latest_task_branch() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        crate::seed::seed_p4_fixture(store.pool()).await.unwrap();
        // task-1 belongs to issue-1; record the branch its run committed on.
        sqlx::query("UPDATE agent_task_queue SET branch = ? WHERE id = 'task-1'")
            .bind("ainb/task-1")
            .execute(store.pool())
            .await
            .unwrap();

        let row = snapshots::issue_row(store.pool(), crate::seed::WS_ID, "issue-1")
            .await
            .unwrap()
            .expect("issue-1 exists");
        assert_eq!(
            row.branch.as_deref(),
            Some("ainb/task-1"),
            "the issue row carries its latest task's branch for the issue-list detail"
        );

        // An issue whose tasks committed no branch surfaces `None`, never an empty
        // string (issue-2 has no task with a branch in the fixture).
        let no_branch = snapshots::issue_row(store.pool(), crate::seed::WS_ID, "issue-2")
            .await
            .unwrap()
            .expect("issue-2 exists");
        assert_eq!(
            no_branch.branch, None,
            "no committed branch → None, not empty"
        );
    }

    /// A foreign workspace yields an empty task list (tenant isolation).
    #[tokio::test]
    async fn tasks_list_foreign_workspace_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        crate::seed::seed_p4_fixture(store.pool()).await.unwrap();
        let resp = dispatch(
            store.pool(),
            &req(
                methods::HANGAR_TASKS_LIST,
                serde_json::json!({"workspace_id":"nope"}),
            ),
            &health(),
            &sink(),
        )
        .await;
        assert!(resp.error.is_none());
        assert_eq!(resp.result.unwrap()["tasks"].as_array().unwrap().len(), 0);
    }

    /// Build a local bare repo with one commit and return a `file://` URL to it —
    /// a fake "remote" a remote-only favorite pick can be cloned from, without any
    /// network (bead pv8).
    fn make_file_remote(root: &std::path::Path) -> String {
        use std::process::Command;
        let work = root.join("src-work");
        std::fs::create_dir_all(&work).unwrap();
        let git = |args: &[&str]| {
            assert!(
                Command::new("git")
                    .args(args)
                    .current_dir(&work)
                    .output()
                    .unwrap()
                    .status
                    .success(),
                "git {args:?} failed"
            );
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "t@t.t"]);
        git(&["config", "user.name", "t"]);
        std::fs::write(work.join("README.md"), "hi").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "init"]);
        let bare = root.join("remote.git");
        assert!(
            Command::new("git")
                .args(["clone", "--bare", "-q"])
                .arg(&work)
                .arg(&bare)
                .output()
                .unwrap()
                .status
                .success(),
            "bare clone failed"
        );
        format!("file://{}", bare.display())
    }

    /// `scratch` and an absolute path are already provision-ready and pass through
    /// `resolve_card_repo_ref` untouched — no clone, no `git` spawn (bead pv8).
    #[tokio::test]
    async fn resolve_card_repo_ref_passes_through_path_and_scratch() {
        let tmp = tempfile::tempdir().unwrap();
        let ainb = tmp.path().join(".agents-in-a-box");
        assert_eq!(
            resolve_card_repo_ref(&ainb, "scratch").await.unwrap(),
            "scratch"
        );
        assert_eq!(
            resolve_card_repo_ref(&ainb, "/src/widget").await.unwrap(),
            "/src/widget"
        );
        // Neither pass-through touched the clones dir.
        assert!(
            !ainb.join("clones").exists(),
            "no clone dir created for path/scratch"
        );
    }

    /// A remote-only favorite pick (a `file://` remote here) is CLONED into the
    /// managed clones dir and resolved to that LOCAL checkout path — so the
    /// untouched provision path only ever sees a path, never a bare remote
    /// (bead pv8 / Codex trap #1). Idempotent: a second resolve reuses the clone.
    #[tokio::test]
    async fn resolve_card_repo_ref_clones_remote_only_favorite() {
        let tmp = tempfile::tempdir().unwrap();
        let ainb = tmp.path().join(".agents-in-a-box");
        let remote = make_file_remote(tmp.path());

        let resolved = resolve_card_repo_ref(&ainb, &remote).await.unwrap();
        let path = std::path::Path::new(&resolved);
        assert!(
            path.is_absolute(),
            "resolved to an absolute local path, not a remote"
        );
        assert!(
            path.starts_with(ainb.join("clones")),
            "clone lives under the managed dir"
        );
        assert!(path.join(".git").exists(), "a real checkout landed");
        assert!(
            path.join("README.md").exists(),
            "the remote's content is present"
        );

        // A second pick of the same remote reuses the SAME clone (idempotent).
        let again = resolve_card_repo_ref(&ainb, &remote).await.unwrap();
        assert_eq!(
            again, resolved,
            "the clone is reused, not re-cloned to a new dir"
        );
    }

    /// A remote that cannot be cloned surfaces an error (the card is not created;
    /// the user retries) and leaves no partial checkout (bead pv8).
    #[tokio::test]
    async fn resolve_card_repo_ref_errors_on_unclonable_remote() {
        let tmp = tempfile::tempdir().unwrap();
        let ainb = tmp.path().join(".agents-in-a-box");
        // A file:// URL to a path that is not a repo → clone fails.
        let bad = format!("file://{}/nope.git", tmp.path().display());
        let err = resolve_card_repo_ref(&ainb, &bad).await;
        assert!(
            err.is_err(),
            "an unclonable remote is an error, not a bogus repo_ref"
        );
    }

    /// The Issues-wizard EDIT path (`handle_issue_update`) resolves a remote-only
    /// favorite pick (`owner/repo`, a URL) to a LOCAL clone path before persisting
    /// it on the card — mirroring `board_card_create` (bead pv8). Without this the
    /// card holds a bare remote the provision path mistakes for a filesystem path,
    /// and no clone/worktree is ever created (issue-wizard-repo-ref-no-clone).
    #[tokio::test]
    async fn issue_update_clones_remote_only_repo_ref() {
        use ainb_hangar_store::repo::card_parity::CardParityRepo;

        // Hold the SHARED home-env lock across the whole set_var → dispatch → restore
        // window so a sibling `with_isolated_home` test cannot clobber
        // `$AINB_HANGAR_HOME` mid-dispatch (where the clone dir is resolved).
        let _guard = ainb_hangar_store::test_support::lock_env();
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        crate::seed::seed_p4_fixture(store.pool()).await.unwrap();

        // Point the daemon's hangar home (where clones land) at a scratch dir so the
        // test never writes under the real `~/.agents-in-a-box`.
        let home = dir.path().join("hangar-home");
        let prior = std::env::var_os(ainb_hangar_core::paths::HANGAR_HOME_ENV);
        std::env::set_var(ainb_hangar_core::paths::HANGAR_HOME_ENV, &home);
        let remote = make_file_remote(dir.path());

        let resp = dispatch(
            store.pool(),
            &req(
                methods::HANGAR_ISSUE_UPDATE,
                serde_json::json!({
                    "workspace_id": "default",
                    "issue_id": "issue-1",
                    "repo_ref": remote,
                }),
            ),
            &health(),
            &sink(),
        )
        .await;

        match prior {
            Some(v) => std::env::set_var(ainb_hangar_core::paths::HANGAR_HOME_ENV, v),
            None => std::env::remove_var(ainb_hangar_core::paths::HANGAR_HOME_ENV),
        }

        assert!(resp.error.is_none(), "{resp:?}");
        let (repo, _agent) = CardParityRepo::get_issue_repo_agent(store.pool(), "issue-1")
            .await
            .unwrap()
            .expect("issue-1 exists");
        let repo = repo.expect("a repo_ref was persisted on the card");
        assert_ne!(
            repo, remote,
            "the raw remote must NOT be persisted verbatim"
        );
        let path = std::path::Path::new(&repo);
        assert!(
            path.is_absolute() && path.join(".git").exists(),
            "the card holds a LOCAL clone checkout, not a bare remote: {repo}"
        );
        assert!(
            path.starts_with(home.join("clones")),
            "the clone lives under the hangar-home managed clones dir: {repo}"
        );
    }

    /// The Issues-wizard RUN path (`handle_issue_run`) resolves a run-time
    /// remote-only `repo_ref` override to a LOCAL clone path before dispatch, so
    /// the enqueued task captures a checkout path — never the raw `owner/repo` the
    /// provision path would treat as a bogus filesystem path
    /// (issue-wizard-repo-ref-no-clone).
    #[tokio::test]
    async fn issue_run_clones_remote_only_repo_ref_override() {
        use ainb_hangar_store::repo::card_parity::CardParityRepo;

        // Shared home-env lock (see the update test above) — serialises against
        // every other `$AINB_HANGAR_HOME`-mutating daemon test.
        let _guard = ainb_hangar_store::test_support::lock_env();
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        crate::seed::seed_p4_fixture(store.pool()).await.unwrap();

        let home = dir.path().join("hangar-home");
        let prior = std::env::var_os(ainb_hangar_core::paths::HANGAR_HOME_ENV);
        std::env::set_var(ainb_hangar_core::paths::HANGAR_HOME_ENV, &home);
        let remote = make_file_remote(dir.path());

        // issue-2 has a seeded brief (satisfying the brief-or-link guard) and no
        // active task (so the one-active-run guard lets it launch).
        let resp = dispatch(
            store.pool(),
            &req(
                methods::HANGAR_ISSUE_RUN,
                serde_json::json!({
                    "workspace_id": "default",
                    "issue_id": "issue-2",
                    "mode": "headless",
                    "repo_ref": remote,
                }),
            ),
            &health(),
            &sink(),
        )
        .await;

        match prior {
            Some(v) => std::env::set_var(ainb_hangar_core::paths::HANGAR_HOME_ENV, v),
            None => std::env::remove_var(ainb_hangar_core::paths::HANGAR_HOME_ENV),
        }

        assert!(resp.error.is_none(), "{resp:?}");
        let task_id = resp.result.unwrap()["task_id"].as_str().unwrap().to_string();
        let (repo, _agent) = CardParityRepo::get_task_repo_agent(store.pool(), &task_id)
            .await
            .unwrap()
            .expect("the run enqueued a task");
        let repo = repo.expect("the enqueued task captured a repo_ref");
        assert_ne!(
            repo, remote,
            "the dispatched task must NOT carry the raw remote override"
        );
        let path = std::path::Path::new(&repo);
        assert!(
            path.is_absolute() && path.join(".git").exists(),
            "the task's repo_ref is a LOCAL clone checkout, not a bare remote: {repo}"
        );
        assert!(
            path.starts_with(home.join("clones")),
            "the clone lives under the hangar-home managed clones dir: {repo}"
        );
    }

    /// `hangar/task_transition` drives the real store FSM: moving the seeded
    /// `running` task to `done` updates the row's status (visible on the next
    /// `tasks_list`).
    #[tokio::test]
    async fn task_transition_moves_card_via_fsm() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        crate::seed::seed_p4_fixture(store.pool()).await.unwrap();

        let resp = dispatch(
            store.pool(),
            &req(
                methods::HANGAR_TASK_TRANSITION,
                serde_json::json!({"workspace_id":"default","task_id":"task-1","to_status":"done"}),
            ),
            &health(),
            &sink(),
        )
        .await;
        assert!(resp.error.is_none(), "{resp:?}");

        // The board snapshot now reports the task in `done`.
        let tasks = snapshots::tasks_list(store.pool(), crate::seed::WS_ID, no_pr()).await.unwrap();
        let moved = tasks.iter().find(|t| t.id.as_str() == "task-1").unwrap();
        assert_eq!(
            moved.status, "done",
            "transition must move the task to done"
        );
    }

    /// A foreign workspace task-transition is rejected (`INVALID_PARAMS` on the
    /// unknown workspace) and moves no row — the mutation must not silently no-op.
    #[tokio::test]
    async fn task_transition_foreign_workspace_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        crate::seed::seed_p4_fixture(store.pool()).await.unwrap();
        let resp = dispatch(
            store.pool(),
            &req(
                methods::HANGAR_TASK_TRANSITION,
                serde_json::json!({"workspace_id":"nope","task_id":"task-1","to_status":"done"}),
            ),
            &health(),
            &sink(),
        )
        .await;
        assert_eq!(resp.error.unwrap().code, INVALID_PARAMS);
        // The seeded task stays `running` (no cross-tenant move).
        let tasks = snapshots::tasks_list(store.pool(), crate::seed::WS_ID, no_pr()).await.unwrap();
        assert_eq!(tasks[0].status, "running");
    }

    /// A foreign task id (right workspace, wrong task) moves nothing but is not an
    /// error (a no-op, mirroring the autopilot fire-now foreign-id behaviour).
    #[tokio::test]
    async fn task_transition_foreign_task_id_is_noop() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        crate::seed::seed_p4_fixture(store.pool()).await.unwrap();
        let resp = dispatch(
            store.pool(),
            &req(
                methods::HANGAR_TASK_TRANSITION,
                serde_json::json!({"workspace_id":"default","task_id":"no-such-task","to_status":"done"}),
            ),
            &health(),
            &sink(),
        )
        .await;
        assert!(
            resp.error.is_none(),
            "foreign task id is a no-op, not an error"
        );
    }

    /// An illegal `to_status` token is rejected with `INVALID_PARAMS` before any
    /// store write.
    #[tokio::test]
    async fn task_transition_illegal_status_is_invalid_params() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        crate::seed::seed_p4_fixture(store.pool()).await.unwrap();
        let resp = dispatch(
            store.pool(),
            &req(
                methods::HANGAR_TASK_TRANSITION,
                serde_json::json!({"workspace_id":"default","task_id":"task-1","to_status":"banana"}),
            ),
            &health(),
            &sink(),
        )
        .await;
        assert_eq!(resp.error.unwrap().code, INVALID_PARAMS);
    }

    /// `hangar/daemon_health` reports the seeded running task as a concurrent
    /// task, the claim-cache figure (used = concurrent, fixed capacity), and a
    /// full 60-sample throughput window seeded from the shared stats collector.
    #[tokio::test]
    async fn daemon_health_reports_concurrency_and_throughput_window() {
        use crate::health_stats::THROUGHPUT_WINDOW;

        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        crate::seed::seed_p4_fixture(store.pool()).await.unwrap();

        // Seed the in-memory throughput ring with a completion + a failure at the
        // current second so they fall inside the `now-59..=now` snapshot window
        // (the handler renders the ring against the live `SystemClock`).
        let health = health();
        let now_sec = ainb_hangar_core::clock::SystemClock.now_ms() / 1_000;
        health.stats.record_completed(now_sec);
        health.stats.record_failed(now_sec);

        let resp = dispatch(
            store.pool(),
            &req(
                methods::HANGAR_DAEMON_HEALTH,
                serde_json::json!({"workspace_id":"default"}),
            ),
            &health,
            &sink(),
        )
        .await;
        assert!(resp.error.is_none(), "{resp:?}");
        let snap: DaemonHealthSnapshot = serde_json::from_value(resp.result.unwrap()).unwrap();

        // The fixture seeds exactly one `running` task → concurrency 1.
        assert_eq!(snap.concurrent_tasks, 1);
        assert_eq!(snap.claim_cache.used, 1);
        assert_eq!(
            snap.claim_cache.capacity,
            crate::health_stats::DEFAULT_CLAIM_CAPACITY
        );
        // The throughput window is always the full minute.
        assert_eq!(snap.task_throughput_60s.len(), THROUGHPUT_WINDOW);
        // The seeded second carries one completion + one failure somewhere in
        // the window.
        assert!(
            snap.task_throughput_60s.iter().any(|s| s.completed == 1 && s.failed == 1),
            "the seeded throughput second must appear in the window"
        );
    }

    /// A foreign workspace yields empty runtimes + zero concurrency, but still
    /// reports the daemon-global throughput window (in-memory state is not
    /// workspace-scoped).
    #[tokio::test]
    async fn daemon_health_foreign_workspace_empty_runtimes() {
        use crate::health_stats::THROUGHPUT_WINDOW;

        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        crate::seed::seed_p4_fixture(store.pool()).await.unwrap();
        let resp = dispatch(
            store.pool(),
            &req(
                methods::HANGAR_DAEMON_HEALTH,
                serde_json::json!({"workspace_id":"nope"}),
            ),
            &health(),
            &sink(),
        )
        .await;
        assert!(resp.error.is_none());
        let snap: DaemonHealthSnapshot = serde_json::from_value(resp.result.unwrap()).unwrap();
        assert!(snap.runtimes.is_empty());
        assert_eq!(snap.concurrent_tasks, 0);
        assert_eq!(snap.task_throughput_60s.len(), THROUGHPUT_WINDOW);
    }

    #[tokio::test]
    async fn issues_list_empty_workspace_returns_empty_vec() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let resp = dispatch(
            store.pool(),
            &req(
                methods::HANGAR_ISSUES_LIST,
                serde_json::json!({"workspace_id":"nope"}),
            ),
            &health(),
            &sink(),
        )
        .await;
        assert!(resp.error.is_none());
        assert_eq!(resp.result.unwrap()["issues"].as_array().unwrap().len(), 0);
    }

    /// Resume replay must deliver the ENTIRE backlog after the cursor, not just
    /// the first [`REPLAY_BATCH`] rows. A single capped read would drop the
    /// newest `(since_seq + REPLAY_BATCH, head]` window while the ack advanced
    /// the client past it — a permanent silent gap. Seed a backlog spanning
    /// three pages and assert every event is replayed, in order.
    #[tokio::test]
    async fn replay_events_drains_backlog_larger_than_one_batch() {
        use ainb_hangar_store::repo::event_log::{EventOutboxRepo, NewEvent};

        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();

        // Seed the owning workspace so the FK-scoped inserts resolve.
        sqlx::query("INSERT INTO workspace (id, slug, name, created_at) VALUES (?, ?, ?, ?)")
            .bind("ws-a")
            .bind("ws-a")
            .bind("ws-a")
            .bind(1_000_i64)
            .execute(pool)
            .await
            .unwrap();

        // A backlog spanning three pages: 1024 + 1024 + 500.
        let total: i64 = REPLAY_BATCH * 2 + 500;
        for i in 0..total {
            EventOutboxRepo::append(
                pool,
                &NewEvent {
                    workspace_id: "ws-a".into(),
                    event_type: "task_progress".into(),
                    entity: Some(format!("t{i}")),
                    payload: format!("{{\"n\":{i}}}"),
                    ts: 1_000 + i,
                },
            )
            .await
            .unwrap();
        }

        // Buffer wider than the backlog so replay never blocks on a full queue.
        let (tx, mut rx) = mpsc::channel::<Vec<u8>>((total as usize) + 16);
        replay_events(pool, "ws-a", 0, &tx).await;
        drop(tx);

        let mut delivered = 0i64;
        while rx.recv().await.is_some() {
            delivered += 1;
        }
        assert_eq!(
            delivered, total,
            "every backlog event after the cursor must be replayed (no truncation at REPLAY_BATCH)"
        );
    }

    /// A mid-log cursor replays only the tail after it — still fully, across the
    /// batch boundary — never the truncated oldest slice.
    #[tokio::test]
    async fn replay_events_from_midlog_cursor_delivers_full_tail() {
        use ainb_hangar_store::repo::event_log::{EventOutboxRepo, NewEvent};

        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();

        sqlx::query("INSERT INTO workspace (id, slug, name, created_at) VALUES (?, ?, ?, ?)")
            .bind("ws-a")
            .bind("ws-a")
            .bind("ws-a")
            .bind(1_000_i64)
            .execute(pool)
            .await
            .unwrap();

        let total: i64 = REPLAY_BATCH + 300;
        let mut seqs = Vec::new();
        for i in 0..total {
            let seq = EventOutboxRepo::append(
                pool,
                &NewEvent {
                    workspace_id: "ws-a".into(),
                    event_type: "task_progress".into(),
                    entity: Some(format!("t{i}")),
                    payload: format!("{{\"n\":{i}}}"),
                    ts: 1_000 + i,
                },
            )
            .await
            .unwrap();
            seqs.push(seq);
        }

        // Resume from the 10th event's seq: expect exactly `total - 10` frames,
        // which still crosses the REPLAY_BATCH boundary.
        let cursor = seqs[9];
        let (tx, mut rx) = mpsc::channel::<Vec<u8>>((total as usize) + 16);
        replay_events(pool, "ws-a", cursor, &tx).await;
        drop(tx);

        let mut delivered = 0i64;
        while rx.recv().await.is_some() {
            delivered += 1;
        }
        assert_eq!(delivered, total - 10);
    }

    /// The rule-list RPC returns the seeded global defaults, a set RPC overrides a
    /// rule, and a per-workspace override supersedes the global for that workspace
    /// only — the full T5 grid round-trip through the dispatcher.
    #[tokio::test]
    async fn notify_rules_list_and_set_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        crate::seed::seed_p4_fixture(store.pool()).await.unwrap();
        let pool = store.pool();

        // Find one kind's row in a `rules` array, cloned.
        fn row_for(rules: &serde_json::Value, kind: &str) -> serde_json::Value {
            rules
                .as_array()
                .unwrap()
                .iter()
                .find(|r| r["kind"] == kind)
                .cloned()
                .unwrap_or_else(|| panic!("no rule row for {kind}"))
        }

        // The seeded global grid: escalation is loud, ask is phone+web+os (0038
        // restored phone) + atc (0040 folded in the ATC feed), waiting is
        // board-only, and nothing is marked overridden at global scope.
        let resp = dispatch(
            pool,
            &req(methods::HANGAR_NOTIFY_RULES_LIST, serde_json::json!({})),
            &health(),
            &sink(),
        )
        .await;
        assert!(resp.error.is_none(), "{resp:?}");
        let rules = resp.result.unwrap()["rules"].clone();
        assert_eq!(
            row_for(&rules, "escalation")["channels"],
            serde_json::json!(["phone", "web", "os"])
        );
        assert_eq!(
            row_for(&rules, "ask_user_question")["channels"],
            serde_json::json!(["phone", "web", "os", "atc"])
        );
        assert_eq!(
            row_for(&rules, "waiting")["channels"],
            serde_json::json!([])
        );
        assert_eq!(
            row_for(&rules, "error")["overridden"],
            serde_json::json!(false)
        );

        // Override ASK for the seeded `default` workspace → phone only.
        let set = dispatch(
            pool,
            &req(
                methods::HANGAR_NOTIFY_RULE_SET,
                serde_json::json!({
                    "workspace_id": "default",
                    "kind": "ask_user_question",
                    "channels": ["phone"],
                }),
            ),
            &health(),
            &sink(),
        )
        .await;
        assert!(set.error.is_none(), "{set:?}");
        assert_eq!(
            set.result.unwrap()["channels"],
            serde_json::json!(["phone"])
        );

        // The workspace grid shows the override (marked)...
        let ws_rules = dispatch(
            pool,
            &req(
                methods::HANGAR_NOTIFY_RULES_LIST,
                serde_json::json!({"workspace_id": "default"}),
            ),
            &health(),
            &sink(),
        )
        .await
        .result
        .unwrap();
        let ws_ask = row_for(&ws_rules["rules"], "ask_user_question");
        assert_eq!(ws_ask["channels"], serde_json::json!(["phone"]));
        assert_eq!(ws_ask["overridden"], serde_json::json!(true));

        // ...while the global grid is untouched.
        let global = dispatch(
            pool,
            &req(methods::HANGAR_NOTIFY_RULES_LIST, serde_json::json!({})),
            &health(),
            &sink(),
        )
        .await
        .result
        .unwrap();
        assert_eq!(
            row_for(&global["rules"], "ask_user_question")["channels"],
            serde_json::json!(["phone", "web", "os", "atc"]),
            "global untouched"
        );
    }

    /// A set RPC with an unknown attention kind is rejected as INVALID_PARAMS
    /// rather than silently writing a rule the CHECK constraint would reject.
    #[tokio::test]
    async fn notify_rule_set_rejects_unknown_kind() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let resp = dispatch(
            store.pool(),
            &req(
                methods::HANGAR_NOTIFY_RULE_SET,
                serde_json::json!({"kind": "not_a_kind", "channels": ["web"]}),
            ),
            &health(),
            &sink(),
        )
        .await;
        assert_eq!(resp.error.unwrap().code, INVALID_PARAMS);
    }

    /// daemon_config get/set round-trip (D13): an unknown key reads `None`, a set
    /// persists, and a follow-up get returns the written value — the wire path the
    /// Settings auto-standup toggle rides.
    #[tokio::test]
    async fn daemon_config_get_set_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();

        // Fresh store: `autostandup.enabled` has no row → value is null.
        let got = dispatch(
            pool,
            &req(
                methods::HANGAR_DAEMON_CONFIG_GET,
                serde_json::json!({"key": "autostandup.enabled"}),
            ),
            &health(),
            &sink(),
        )
        .await;
        assert!(got.error.is_none(), "{got:?}");
        assert_eq!(got.result.unwrap()["value"], serde_json::Value::Null);

        // Write it on.
        let set = dispatch(
            pool,
            &req(
                methods::HANGAR_DAEMON_CONFIG_SET,
                serde_json::json!({"key": "autostandup.enabled", "value": "true"}),
            ),
            &health(),
            &sink(),
        )
        .await;
        assert!(set.error.is_none(), "{set:?}");
        assert_eq!(set.result.unwrap()["value"], serde_json::json!("true"));

        // The follow-up get returns the persisted value.
        let got2 = dispatch(
            pool,
            &req(
                methods::HANGAR_DAEMON_CONFIG_GET,
                serde_json::json!({"key": "autostandup.enabled"}),
            ),
            &health(),
            &sink(),
        )
        .await;
        assert_eq!(got2.result.unwrap()["value"], serde_json::json!("true"));
    }

    /// A blank daemon_config key is rejected as INVALID_PARAMS on both get and set.
    #[tokio::test]
    async fn daemon_config_rejects_blank_key() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let get = dispatch(
            store.pool(),
            &req(
                methods::HANGAR_DAEMON_CONFIG_GET,
                serde_json::json!({"key": "  "}),
            ),
            &health(),
            &sink(),
        )
        .await;
        assert_eq!(get.error.unwrap().code, INVALID_PARAMS);
        let set = dispatch(
            store.pool(),
            &req(
                methods::HANGAR_DAEMON_CONFIG_SET,
                serde_json::json!({"key": "", "value": "x"}),
            ),
            &health(),
            &sink(),
        )
        .await;
        assert_eq!(set.error.unwrap().code, INVALID_PARAMS);
    }

    /// `daemon_config_list` returns one entry per registry knob (unset → null),
    /// and reflects a prior write.
    #[tokio::test]
    async fn daemon_config_list_covers_registry_and_reflects_writes() {
        use ainb_hangar_core::daemon_config::DAEMON_CONFIG_REGISTRY;

        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();

        let listed = dispatch(
            pool,
            &req(methods::HANGAR_DAEMON_CONFIG_LIST, serde_json::json!({})),
            &health(),
            &sink(),
        )
        .await;
        assert!(listed.error.is_none(), "{listed:?}");
        let entries = listed.result.unwrap()["entries"].as_array().unwrap().clone();
        assert_eq!(
            entries.len(),
            DAEMON_CONFIG_REGISTRY.len(),
            "one list entry per registry knob"
        );

        // Write one knob, then confirm the list reflects it.
        dispatch(
            pool,
            &req(
                methods::HANGAR_DAEMON_CONFIG_SET,
                serde_json::json!({"key": "autostandup.stagnant_min", "value": "30"}),
            ),
            &health(),
            &sink(),
        )
        .await;
        let relisted = dispatch(
            pool,
            &req(methods::HANGAR_DAEMON_CONFIG_LIST, serde_json::json!({})),
            &health(),
            &sink(),
        )
        .await;
        let entries = relisted.result.unwrap()["entries"].as_array().unwrap().clone();
        let row = entries
            .iter()
            .find(|e| e["key"] == "autostandup.stagnant_min")
            .expect("stagnant_min listed");
        assert_eq!(row["value"], serde_json::json!("30"));
    }

    /// A registry-validated set rejects an out-of-range int / bad enum with
    /// `INVALID_PARAMS`, and normalizes a tolerant/mixed-case value it accepts.
    #[tokio::test]
    async fn daemon_config_set_validates_registry_knobs() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();

        // Out-of-range int → rejected.
        let bad = dispatch(
            pool,
            &req(
                methods::HANGAR_DAEMON_CONFIG_SET,
                serde_json::json!({"key": "autostandup.stagnant_min", "value": "99999"}),
            ),
            &health(),
            &sink(),
        )
        .await;
        assert_eq!(bad.error.unwrap().code, INVALID_PARAMS);

        // Bad enum → rejected.
        let bad_enum = dispatch(
            pool,
            &req(
                methods::HANGAR_DAEMON_CONFIG_SET,
                serde_json::json!({"key": "card_agent.default", "value": "gemini"}),
            ),
            &health(),
            &sink(),
        )
        .await;
        assert_eq!(bad_enum.error.unwrap().code, INVALID_PARAMS);

        // Mixed-case enum → accepted + normalized to the canonical spelling.
        let ok = dispatch(
            pool,
            &req(
                methods::HANGAR_DAEMON_CONFIG_SET,
                serde_json::json!({"key": "card_agent.default", "value": "CODEX"}),
            ),
            &health(),
            &sink(),
        )
        .await;
        assert!(ok.error.is_none(), "{ok:?}");
        assert_eq!(ok.result.unwrap()["value"], serde_json::json!("codex"));
    }

    /// The set RPC and the CLI are meant to be ONE gate, so they must agree on
    /// what a legal key is. The RPC used to pass unknown keys straight through to
    /// the table while the CLI rejected them with `unknown config key` — the two
    /// legs disagreed, and anything could be written into `daemon_config`.
    #[tokio::test]
    async fn daemon_config_set_rejects_unknown_keys_like_the_cli() {
        use ainb_hangar_store::repo::daemon_config::DaemonConfigRepo;
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();

        for key in ["not.a.knob", "card_agent.last_used"] {
            let got = dispatch(
                pool,
                &req(
                    methods::HANGAR_DAEMON_CONFIG_SET,
                    serde_json::json!({"key": key, "value": "x"}),
                ),
                &health(),
                &sink(),
            )
            .await;
            assert_eq!(
                got.error.as_ref().map(|e| e.code),
                Some(INVALID_PARAMS),
                "`{key}` is not a registry knob and must be refused, got {got:?}"
            );
            assert_eq!(
                DaemonConfigRepo::get(pool, key).await.unwrap(),
                None,
                "a refused key must not be written"
            );
        }

        // `card_agent.last_used` is internal state the daemon writes in-process
        // through the repo — refusing it over RPC does not disturb that path.
        DaemonConfigRepo::set(pool, "card_agent.last_used", "codex").await.unwrap();
        assert_eq!(
            DaemonConfigRepo::get(pool, "card_agent.last_used").await.unwrap(),
            Some("codex".to_string())
        );
    }

    /// An absurdly long value is refused up front rather than echoed back.
    #[tokio::test]
    async fn daemon_config_set_bounds_the_value_length() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let huge = "9".repeat(MAX_DAEMON_CONFIG_VALUE_LEN + 1);
        let got = dispatch(
            store.pool(),
            &req(
                methods::HANGAR_DAEMON_CONFIG_SET,
                serde_json::json!({"key": "autostandup.stagnant_min", "value": huge}),
            ),
            &health(),
            &sink(),
        )
        .await;
        let err = got.error.expect("an over-long value is refused");
        assert_eq!(err.code, INVALID_PARAMS);
        assert!(
            !err.message.contains("999999"),
            "the rejection must not echo the payload back: {}",
            err.message
        );
    }

    /// The 0043 issue-run dispatch guard: an `issue_run` of an issue with NEITHER a
    /// brief nor an upstream link is refused; either one present clears the brief
    /// guard (and then falls to the repo guard — a DIFFERENT refusal — proving the
    /// brief guard let it through). Scoped to `issue_run`, not the shared board
    /// path (a Kanban card is title-only by design).
    #[tokio::test]
    async fn issue_run_refuses_a_brief_less_and_ref_less_issue() {
        use ainb_hangar_store::bootstrap;
        use ainb_hangar_store::repo::card_parity::CardParityRepo;
        use ainb_hangar_store::repo::issue::{IssueRepo, NewIssue};

        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        let ws = bootstrap::ensure_default_workspace(pool).await.unwrap();
        bootstrap::ensure_runtime(pool, &bootstrap::default_runtime_id(), 1)
            .await
            .unwrap();
        bootstrap::create_agent(pool, &ws, "worker", "claude", None).await.unwrap();

        let seed = |title: &'static str, desc: Option<&'static str>| {
            let ws = ws.clone();
            async move {
                let id =
                    ainb_hangar_core::idgen::IdGen::new_ulid(&ainb_hangar_core::idgen::SystemIdGen);
                IssueRepo::insert(
                    pool,
                    &NewIssue {
                        id: id.clone(),
                        workspace_id: ws,
                        title: title.into(),
                        description: desc.map(Into::into),
                        state: "todo".into(),
                        creator: ainb_hangar_core::actor::ActorRef::new(
                            ainb_hangar_core::actor::ActorKind::Member,
                            "stevie",
                        )
                        .unwrap(),
                        created_at: 1,
                        priority: 0,
                        assignee: None,
                        due_date: None,
                        labels: Vec::new(),
                        parent_issue_id: None,
                        stage: None,
                        acceptance_criteria: Vec::new(),
                        context_refs: Vec::new(),
                    },
                )
                .await
                .unwrap();
                id
            }
        };

        let run = |issue_id: String| {
            let ws = ws.clone();
            async move {
                dispatch(
                    pool,
                    &req(
                        methods::HANGAR_ISSUE_RUN,
                        serde_json::json!({
                            "workspace_id": ws,
                            "issue_id": issue_id,
                            "mode": "headless",
                        }),
                    ),
                    &health(),
                    &sink(),
                )
                .await
            }
        };

        // (1) Neither brief nor ref → refused with the brief-or-link message.
        let bare = seed("just a title", None).await;
        let err = run(bare).await.error.expect("a brief-less, ref-less run is refused");
        assert_eq!(err.code, INVALID_PARAMS);
        assert!(
            err.message.contains("add a brief or link an issue"),
            "the refusal names the brief-or-link requirement: {}",
            err.message
        );

        // (2) A brief present → clears the brief guard (falls to the repo guard,
        //     a DIFFERENT refusal, since no repo is pinned).
        let briefed = seed("has a brief", Some("do the thing carefully")).await;
        let err = run(briefed).await.error.expect("no repo is pinned, so it still cannot run");
        assert!(
            err.message.contains("repo is required"),
            "a briefed issue passes the brief guard and stops at the repo guard: {}",
            err.message
        );

        // (3) A linked ref present (no brief) → also clears the brief guard.
        let linked = seed("linked only", None).await;
        CardParityRepo::set_issue_external_ref(pool, &ws, &linked, Some("acme/api#7"))
            .await
            .unwrap();
        let err = run(linked).await.error.expect("no repo is pinned, so it still cannot run");
        assert!(
            err.message.contains("repo is required"),
            "a linked issue passes the brief guard and stops at the repo guard: {}",
            err.message
        );
    }

    /// The brief-or-link guard is scoped to `handle_issue_run` ONLY. The shared
    /// [`run_card`] core — behind `board_card_run` and autopilot dispatch — must
    /// NOT refuse a brief-less, ref-less issue, so a Kanban/board launch is
    /// unaffected. This locks the path-scoping against a future refactor that
    /// might move the check into the shared core (which would silently break
    /// board dispatch).
    #[tokio::test]
    async fn run_card_does_not_apply_the_brief_or_link_guard() {
        use ainb_hangar_store::bootstrap;
        use ainb_hangar_store::repo::issue::{IssueRepo, NewIssue};

        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        let ws = bootstrap::ensure_default_workspace(pool).await.unwrap();
        bootstrap::ensure_runtime(pool, &bootstrap::default_runtime_id(), 1)
            .await
            .unwrap();
        bootstrap::create_agent(pool, &ws, "worker", "claude", None).await.unwrap();

        // A brief-less, ref-less issue — exactly what `handle_issue_run` refuses.
        let id = ainb_hangar_core::idgen::IdGen::new_ulid(&ainb_hangar_core::idgen::SystemIdGen);
        IssueRepo::insert(
            pool,
            &NewIssue {
                id: id.clone(),
                workspace_id: ws.clone(),
                title: "just a title".into(),
                description: None,
                state: "todo".into(),
                creator: ainb_hangar_core::actor::ActorRef::new(
                    ainb_hangar_core::actor::ActorKind::Member,
                    "stevie",
                )
                .unwrap(),
                created_at: 1,
                priority: 0,
                assignee: None,
                due_date: None,
                labels: Vec::new(),
                parent_issue_id: None,
                stage: None,
                acceptance_criteria: Vec::new(),
                context_refs: Vec::new(),
            },
        )
        .await
        .unwrap();
        let issue = IssueRepo::get_by_id(pool, &id).await.unwrap().unwrap();
        let ws_id = WorkspaceId::from_str(&ws).unwrap();

        // The shared core, called with a repo pinned + agent kind — it must launch
        // (a Single task), never the brief-or-link refusal that lives only in
        // `handle_issue_run`.
        let outcome = run_card(
            pool,
            &ws_id,
            None,
            &issue,
            "headless",
            Some("scratch"),
            ainb_hangar_core::agent_kind::AgentKind::parse("claude"),
            None,
            None,
            None,
        )
        .await;

        assert!(
            outcome.is_ok(),
            "the shared run_card must launch a brief-less issue — no brief-or-link \
             guard belongs in the shared path (board_card_run / autopilot use it)"
        );
    }

    /// V3-F3 core: a run-time `assignee_override` routes the run to the NAMED
    /// agent it names, NOT the workspace's alphabetically-first agent (the
    /// fallback the create wizard hit before it could target a named agent).
    ///
    /// The mutation-provable heart of the fix: two agents `alpha` (first by name)
    /// and `omega` (last) exist, the issue carries NO persisted assignee, and the
    /// run is dispatched with `assignee_override = agent:<omega>`. It must launch
    /// under `omega`. Break the override plumbing (drop the param, or prefer the
    /// issue's `None` assignee) → resolution falls to `alpha` → this test goes red.
    #[tokio::test]
    async fn run_card_assignee_override_beats_alphabetical_fallback() {
        use ainb_hangar_store::bootstrap;
        use ainb_hangar_store::repo::issue::{IssueRepo, NewIssue};

        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        let ws = bootstrap::ensure_default_workspace(pool).await.unwrap();
        bootstrap::ensure_runtime(pool, &bootstrap::default_runtime_id(), 1)
            .await
            .unwrap();
        // Two named agents; `alpha` sorts first so a fallback (ORDER BY name) picks
        // it. The override must select `omega` regardless.
        let alpha = bootstrap::create_agent(pool, &ws, "alpha", "claude", None).await.unwrap();
        let omega = bootstrap::create_agent(pool, &ws, "omega", "claude", None).await.unwrap();
        assert_ne!(alpha.id, omega.id);

        // Issue with NO persisted assignee — the override is the ONLY signal.
        let id = ainb_hangar_core::idgen::IdGen::new_ulid(&ainb_hangar_core::idgen::SystemIdGen);
        IssueRepo::insert(
            pool,
            &NewIssue {
                id: id.clone(),
                workspace_id: ws.clone(),
                title: "hand this to omega".into(),
                description: Some("do the work".into()),
                state: "todo".into(),
                creator: ainb_hangar_core::actor::ActorRef::new(
                    ainb_hangar_core::actor::ActorKind::Member,
                    "stevie",
                )
                .unwrap(),
                created_at: 1,
                priority: 0,
                assignee: None,
                due_date: None,
                labels: Vec::new(),
                parent_issue_id: None,
                stage: None,
                acceptance_criteria: Vec::new(),
                context_refs: Vec::new(),
            },
        )
        .await
        .unwrap();
        let issue = IssueRepo::get_by_id(pool, &id).await.unwrap().unwrap();
        let ws_id = WorkspaceId::from_str(&ws).unwrap();

        let override_ref = ainb_hangar_core::actor::ActorRef::new(
            ainb_hangar_core::actor::ActorKind::Agent,
            omega.id.clone(),
        )
        .unwrap();

        let outcome = run_card(
            pool,
            &ws_id,
            None,
            &issue,
            "headless",
            Some("scratch"),
            None, // no provider override — the named agent's own provider drives spawn
            None,
            Some(&override_ref),
            None,
        )
        .await;

        match outcome {
            Ok(CardRunOutcome::Single { agent_id, .. }) => {
                assert_eq!(
                    agent_id, omega.id,
                    "the run must dispatch under the OVERRIDE agent (omega), not the \
                     alphabetical fallback (alpha)"
                );
                assert_ne!(
                    agent_id, alpha.id,
                    "alpha is the fallback the override must beat"
                );
            }
            Ok(CardRunOutcome::Squad { .. }) => panic!("a non-squad issue must run as Single"),
            Err(_) => panic!("the run must launch under the override agent"),
        }
    }

    /// The override is optional: with NO `assignee_override` and NO persisted
    /// assignee, the run still launches under the workspace's first agent (the
    /// deterministic fallback the provider-chip wizard path relies on). This locks
    /// the fallback so the override plumbing never silently makes a run un-runnable
    /// when no named agent is targeted.
    #[tokio::test]
    async fn run_card_without_assignee_override_falls_back_to_first_agent() {
        use ainb_hangar_store::bootstrap;
        use ainb_hangar_store::repo::issue::{IssueRepo, NewIssue};

        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        let ws = bootstrap::ensure_default_workspace(pool).await.unwrap();
        bootstrap::ensure_runtime(pool, &bootstrap::default_runtime_id(), 1)
            .await
            .unwrap();
        let alpha = bootstrap::create_agent(pool, &ws, "alpha", "claude", None).await.unwrap();
        bootstrap::create_agent(pool, &ws, "omega", "claude", None).await.unwrap();

        let id = ainb_hangar_core::idgen::IdGen::new_ulid(&ainb_hangar_core::idgen::SystemIdGen);
        IssueRepo::insert(
            pool,
            &NewIssue {
                id: id.clone(),
                workspace_id: ws.clone(),
                title: "no target".into(),
                description: Some("do the work".into()),
                state: "todo".into(),
                creator: ainb_hangar_core::actor::ActorRef::new(
                    ainb_hangar_core::actor::ActorKind::Member,
                    "stevie",
                )
                .unwrap(),
                created_at: 1,
                priority: 0,
                assignee: None,
                due_date: None,
                labels: Vec::new(),
                parent_issue_id: None,
                stage: None,
                acceptance_criteria: Vec::new(),
                context_refs: Vec::new(),
            },
        )
        .await
        .unwrap();
        let issue = IssueRepo::get_by_id(pool, &id).await.unwrap().unwrap();
        let ws_id = WorkspaceId::from_str(&ws).unwrap();

        let outcome = run_card(
            pool,
            &ws_id,
            None,
            &issue,
            "headless",
            Some("scratch"),
            None,
            None,
            None,
            None,
        )
        .await;

        match outcome {
            Ok(CardRunOutcome::Single { agent_id, .. }) => {
                assert_eq!(
                    agent_id, alpha.id,
                    "the fallback picks the first agent by name"
                );
            }
            Ok(CardRunOutcome::Squad { .. }) => panic!("a non-squad issue must run as Single"),
            Err(_) => panic!("the run must launch on the fallback agent"),
        }
    }

    /// In-product recovery: assigning an AGENT to an issue via `hangar/issue_update`
    /// (the TUI `a` picker + `issue update --assign` both route here) re-dispatches
    /// the issue — it inserts exactly ONE `agent_task_queue` row keyed to that
    /// agent, so a stuck / unassigned issue is no longer a dead end.
    ///
    /// Mutation-provable heart of the fix: the issue starts with NO tasks; the
    /// only mutation is the assignee edit. Drop the `run_card` re-dispatch from
    /// `handle_issue_update` → zero task rows → this test goes red.
    #[tokio::test]
    async fn issue_update_assign_to_agent_enqueues_one_task() {
        use ainb_hangar_store::bootstrap;
        use ainb_hangar_store::repo::card_parity::CardParityRepo;
        use ainb_hangar_store::repo::issue::{IssueRepo, NewIssue};

        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        let ws = bootstrap::ensure_default_workspace(pool).await.unwrap();
        bootstrap::ensure_runtime(pool, &bootstrap::default_runtime_id(), 1)
            .await
            .unwrap();
        let agent = bootstrap::create_agent(pool, &ws, "worker", "claude", None).await.unwrap();

        // An unassigned issue that already carries a repo (the create path persists
        // it) — exactly the shape a failed/agent_error issue has when a user
        // re-assigns it to recover.
        let id = ainb_hangar_core::idgen::IdGen::new_ulid(&ainb_hangar_core::idgen::SystemIdGen);
        IssueRepo::insert(
            pool,
            &NewIssue {
                id: id.clone(),
                workspace_id: ws.clone(),
                title: "recover me".into(),
                description: Some("do the work".into()),
                state: "todo".into(),
                creator: ainb_hangar_core::actor::ActorRef::new(
                    ainb_hangar_core::actor::ActorKind::Member,
                    "stevie",
                )
                .unwrap(),
                created_at: 1,
                priority: 0,
                assignee: None,
                due_date: None,
                labels: Vec::new(),
                parent_issue_id: None,
                stage: None,
                acceptance_criteria: Vec::new(),
                context_refs: Vec::new(),
            },
        )
        .await
        .unwrap();
        CardParityRepo::set_issue_repo_agent(pool, &ws, &id, Some("scratch"), None)
            .await
            .unwrap();

        // Baseline: no tasks yet.
        let before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_task_queue")
            .fetch_one(pool)
            .await
            .unwrap();
        assert_eq!(before, 0, "no task exists before the assignment");

        // Assign the agent through the real RPC seam the TUI picker fires.
        let resp = dispatch(
            pool,
            &req(
                methods::HANGAR_ISSUE_UPDATE,
                serde_json::json!({
                    "workspace_id": ws,
                    "issue_id": id,
                    "assignee": format!("agent:{}", agent.id),
                }),
            ),
            &health(),
            &sink(),
        )
        .await;
        assert!(
            resp.error.is_none(),
            "the assign RPC must succeed: {:?}",
            resp.error
        );

        // Exactly ONE task, keyed to the assigned agent, on this issue.
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_task_queue")
            .fetch_one(pool)
            .await
            .unwrap();
        assert_eq!(
            count, 1,
            "assigning an agent enqueues exactly one recovery task"
        );
        let (task_agent, task_issue): (String, Option<String>) =
            sqlx::query_as("SELECT agent_id, issue_id FROM agent_task_queue LIMIT 1")
                .fetch_one(pool)
                .await
                .unwrap();
        assert_eq!(
            task_agent, agent.id,
            "the task routes to the assigned agent"
        );
        assert_eq!(
            task_issue.as_deref(),
            Some(id.as_str()),
            "the task carries the issue"
        );
    }

    /// gap #8 enqueue guard: the invocation gate actually BLOCKS a run, it does not
    /// merely report. A PRIVATE agent invoked by a NON-OWNER member yields
    /// `NotInvocable` and writes NO `agent_task_queue` row; the workspace OWNER
    /// always enqueues (no regression); once the member is allow-listed
    /// (`public_to` + member target) the SAME member enqueues exactly one task.
    #[tokio::test]
    async fn run_card_gates_a_private_agent_against_a_non_owner_member() {
        use ainb_hangar_core::clock::SystemClock;
        use ainb_hangar_core::idgen::SystemIdGen;
        use ainb_hangar_core::ids::WorkspaceId;
        use ainb_hangar_store::bootstrap;
        use ainb_hangar_store::repo::agent::AgentRepo;
        use ainb_hangar_store::repo::agent_invocation_target::AgentInvocationTargetRepo;
        use ainb_hangar_store::repo::card_parity::CardParityRepo;
        use ainb_hangar_store::repo::issue::{IssueRepo, NewIssue};
        use ainb_hangar_store::repo::member::{MemberRepo, MemberRole};

        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        let ws = bootstrap::ensure_default_workspace(pool).await.unwrap();
        bootstrap::ensure_runtime(pool, &bootstrap::default_runtime_id(), 1)
            .await
            .unwrap();
        // create_agent yields a PRIVATE agent (permission_mode default).
        let agent = bootstrap::create_agent(pool, &ws, "secret-bot", "claude", None).await.unwrap();
        let ws_id = WorkspaceId::from_str(ws.clone()).unwrap();
        let bob = MemberRepo::add(pool, &ws_id, "bob@example.com", MemberRole::Member)
            .await
            .unwrap();

        // Two runnable issues (repo = scratch) so the one-active-run guard never
        // masks a gate outcome.
        let mk_issue = |title: &str| {
            let id =
                ainb_hangar_core::idgen::IdGen::new_ulid(&ainb_hangar_core::idgen::SystemIdGen);
            (id.clone(), title.to_string())
        };
        let (issue1, _) = mk_issue("private run one");
        let (issue2, _) = mk_issue("private run two");
        for (iid, title) in [(&issue1, "private run one"), (&issue2, "private run two")] {
            IssueRepo::insert(
                pool,
                &NewIssue {
                    id: iid.clone(),
                    workspace_id: ws.clone(),
                    title: title.into(),
                    description: Some("do the work".into()),
                    state: "todo".into(),
                    creator: ainb_hangar_core::actor::ActorRef::new(
                        ainb_hangar_core::actor::ActorKind::Member,
                        "stevie",
                    )
                    .unwrap(),
                    created_at: 1,
                    priority: 0,
                    assignee: None,
                    due_date: None,
                    labels: Vec::new(),
                    parent_issue_id: None,
                    stage: None,
                    acceptance_criteria: Vec::new(),
                    context_refs: Vec::new(),
                },
            )
            .await
            .unwrap();
            CardParityRepo::set_issue_repo_agent(pool, &ws, iid, Some("scratch"), None)
                .await
                .unwrap();
        }
        let load =
            |iid: String| async move { IssueRepo::get_by_id(pool, &iid).await.unwrap().unwrap() };

        // (a) DENY: private agent + non-owner member bob → NotInvocable, no task row.
        let denied = run_card(
            pool,
            &ws_id,
            None,
            &load(issue1.clone()).await,
            "headless",
            None,
            None,
            None,
            None,
            Some(&bob.user_id),
        )
        .await;
        assert!(
            matches!(denied, Err(CardRunError::NotInvocable { .. })),
            "a non-owner member must NOT invoke a private agent (private, no target)",
        );
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_task_queue")
            .fetch_one(pool)
            .await
            .unwrap();
        assert_eq!(count, 0, "a blocked run writes NO task row");

        // (b) OWNER (default None invoker) always enqueues — no regression.
        let owner_run = run_card(
            pool,
            &ws_id,
            None,
            &load(issue1.clone()).await,
            "headless",
            None,
            None,
            None,
            None,
            None,
        )
        .await;
        assert!(
            owner_run.is_ok(),
            "owner-invoked run must enqueue even for a private agent"
        );
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_task_queue")
            .fetch_one(pool)
            .await
            .unwrap();
        assert_eq!(count, 1, "the owner's run enqueued exactly one task");

        // (c) Allow-list bob (member target, mode public_to) → the SAME member now
        //     enqueues, on the second issue.
        AgentRepo::set_permission_mode(pool, &agent.id, "public_to").await.unwrap();
        AgentInvocationTargetRepo::add(
            pool,
            &SystemIdGen,
            &SystemClock,
            &agent.id,
            "member",
            &bob.user_id,
            None,
        )
        .await
        .unwrap();
        let member_run = run_card(
            pool,
            &ws_id,
            None,
            &load(issue2.clone()).await,
            "headless",
            None,
            None,
            None,
            None,
            Some(&bob.user_id),
        )
        .await;
        assert!(
            member_run.is_ok(),
            "an allow-listed member must invoke the now-public_to agent"
        );
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_task_queue")
            .fetch_one(pool)
            .await
            .unwrap();
        assert_eq!(
            count, 2,
            "the allow-listed member's run enqueued the second task"
        );
    }

    /// Pattern-B handover regression: the create-wizard fires ONE `issue_update`
    /// carrying BOTH a `source_branch` AND a NAMED-agent assignee, then the
    /// named-agent auto-dispatch re-runs the card. The dispatched task MUST branch
    /// FROM the wizard's source branch, not `main`.
    ///
    /// Mutation-provable: drop the `set_issue_branches` persist that now runs
    /// BEFORE the auto-dispatch and the card's `source_branch` stays NULL, so the
    /// auto-dispatched `agent_task_queue.source_branch` comes back NULL and both
    /// assertions below go red — the exact silent break the fake-script e2e missed.
    #[tokio::test]
    async fn issue_update_named_agent_persists_source_branch_for_autodispatch() {
        use ainb_hangar_store::bootstrap;
        use ainb_hangar_store::repo::card_parity::CardParityRepo;
        use ainb_hangar_store::repo::issue::{IssueRepo, NewIssue};

        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        let ws = bootstrap::ensure_default_workspace(pool).await.unwrap();
        bootstrap::ensure_runtime(pool, &bootstrap::default_runtime_id(), 1)
            .await
            .unwrap();
        let agent = bootstrap::create_agent(pool, &ws, "reviewer", "claude", None).await.unwrap();

        // An issue that carries a repo but NO source branch yet — the state right
        // before the wizard's edit lands.
        let id = ainb_hangar_core::idgen::IdGen::new_ulid(&ainb_hangar_core::idgen::SystemIdGen);
        IssueRepo::insert(
            pool,
            &NewIssue {
                id: id.clone(),
                workspace_id: ws.clone(),
                title: "hand off to reviewer".into(),
                description: Some("review the V3 tip".into()),
                state: "todo".into(),
                creator: ainb_hangar_core::actor::ActorRef::new(
                    ainb_hangar_core::actor::ActorKind::Member,
                    "stevie",
                )
                .unwrap(),
                created_at: 1,
                priority: 0,
                assignee: None,
                due_date: None,
                labels: Vec::new(),
                parent_issue_id: None,
                stage: None,
                acceptance_criteria: Vec::new(),
                context_refs: Vec::new(),
            },
        )
        .await
        .unwrap();
        CardParityRepo::set_issue_repo_agent(pool, &ws, &id, Some("scratch"), None)
            .await
            .unwrap();

        let handover_branch = "ainb/01KY4F2P90AHH53FJ5HQ3Q70GT";

        // ONE RPC carrying source_branch + a named-agent assignee — the wizard shape.
        let resp = dispatch(
            pool,
            &req(
                methods::HANGAR_ISSUE_UPDATE,
                serde_json::json!({
                    "workspace_id": ws,
                    "issue_id": id,
                    "assignee": format!("agent:{}", agent.id),
                    "source_branch": handover_branch,
                }),
            ),
            &health(),
            &sink(),
        )
        .await;
        assert!(
            resp.error.is_none(),
            "the assign+source RPC must succeed: {:?}",
            resp.error
        );

        // The card persisted the wizard's source branch.
        let (persisted_source, _target) =
            CardParityRepo::get_issue_branches(pool, &id).await.unwrap().unwrap();
        assert_eq!(
            persisted_source.as_deref(),
            Some(handover_branch),
            "issue.source_branch must persist the wizard's Source field"
        );

        // The auto-dispatched task branched FROM that source, not from main/NULL.
        let task_source: Option<String> = sqlx::query_scalar(
            "SELECT source_branch FROM agent_task_queue WHERE issue_id = ? LIMIT 1",
        )
        .bind(&id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(
            task_source.as_deref(),
            Some(handover_branch),
            "the named-agent auto-dispatch must branch from the persisted source"
        );
    }
}
