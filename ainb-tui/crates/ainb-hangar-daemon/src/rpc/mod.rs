//! The daemon's `UnixListener` JSON-RPC server (P4.10).
//!
//! P3.7's plugin dials `~/.ainb/hangar.sock` through the host `unix_socket_dial`
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
use std::time::Instant;

use ainb_hangar_core::clock::{HangarClock, SystemClock};
use ainb_hangar_core::ids::{AgentId, AutopilotId, SkillId, WorkspaceId};
use ainb_hangar_proto::methods;
use ainb_hangar_proto::settings::{DaemonHealthSnapshot, HealthSnapshot};
use ainb_hangar_proto::{RpcError, RpcId, RpcRequest, RpcResponse};
use sqlx::SqlitePool;

use crate::events::{EventBroker, EventSink, ScopedEvent, encode_event_frame};
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
/// the plugin's dial target (`~/.ainb/hangar.sock`) when the store resolves to
/// the default `~/.ainb`, and follows `$AINB_HANGAR_HOME` when overridden so a
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

    let served: std::io::Result<()> = async {
        while let Some(body) = read_frame(&mut reader).await? {
            let req = serde_json::from_slice::<RpcRequest>(&body);
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
                        forwarder = Some(spawn_event_forwarder(
                            broker.subscribe(),
                            ws,
                            out_tx.clone(),
                        ));
                    }
                }
            }
        }
        Ok(())
    }
    .await;

    if let Some(f) = forwarder {
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
        // `workspace/subscribe` acks with an (empty) snapshot envelope; the
        // event push side is the stream client's concern. The plugin only needs
        // a non-error ack to reach `Connected`, then pulls the screen snapshots.
        // We still resolve the wire id so an unknown workspace surfaces no error
        // (the ack is unconditional; the screens pull real rows next).
        methods::WORKSPACE_SUBSCRIBE => {
            let _ = resolve(pool, req).await?;
            Ok(serde_json::json!({ "snapshot": {} }))
        }
        methods::HANGAR_ISSUES_LIST => {
            let issues = match resolve(pool, req).await? {
                Some(ws) => snapshots::issues_list(pool, &ws).await.map_err(|e| store_err(&e))?,
                None => Vec::new(),
            };
            to_value(&ainb_hangar_proto::snapshots::IssuesListResult { issues })
        }
        methods::HANGAR_AGENTS_LIST => {
            let actors = match resolve(pool, req).await? {
                Some(ws) => snapshots::agents_list(pool, &ws).await.map_err(|e| store_err(&e))?,
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
        methods::HANGAR_TASKS_LIST => {
            let tasks = match resolve(pool, req).await? {
                Some(ws) => snapshots::tasks_list(pool, &ws).await.map_err(|e| store_err(&e))?,
                None => Vec::new(),
            };
            to_value(&ainb_hangar_proto::snapshots::TasksListResult { tasks })
        }
        methods::HANGAR_TASK_TRANSITION => handle_task_transition(pool, req, events).await,
        methods::HANGAR_ISSUE_CREATE => handle_issue_create(pool, req, events).await,
        methods::HANGAR_ISSUE_UPDATE => handle_issue_update(pool, req, events).await,
        methods::HANGAR_ISSUE_LABEL_ATTACH => handle_issue_label(pool, req, events, true).await,
        methods::HANGAR_ISSUE_LABEL_DETACH => handle_issue_label(pool, req, events, false).await,
        methods::HANGAR_COMMENT_ADD => handle_comment_add(pool, req, events).await,
        methods::HANGAR_AGENT_UPDATE => handle_agent_update(pool, req).await,
        methods::HANGAR_AGENT_ARCHIVE => handle_agent_archive(pool, req).await,
        methods::HANGAR_HEALTH => to_value(&health.snapshot(true)),
        methods::HANGAR_DAEMON_HEALTH => handle_daemon_health(pool, req, health).await,
        other => Err(RpcError {
            code: METHOD_NOT_FOUND,
            message: format!("unknown method: {other}"),
            data: None,
        }),
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
    let moved = snapshots::task_transition(pool, &SystemClock, ws.as_str(), &params.task_id, to)
        .await
        .map_err(|e| store_err(&e))?;
    if moved {
        if let Some(event) = task_transition_event(&params.task_id, to, SystemClock.now_ms()) {
            events.emit(ws.as_str(), event);
        }
    }
    Ok(serde_json::json!({}))
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

    let params: ainb_hangar_proto::snapshots::IssueCreateParams =
        parse_params(req, "{ workspace_id, title, description?, creator }")?;
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
    let row = snapshots::issue_create(
        pool,
        &SystemIdGen,
        &SystemClock,
        ws.as_str(),
        &params.title,
        params.description.as_deref(),
        &creator,
    )
    .await
    .map_err(|e| store_err(&e))?;
    // A committed insert announces the new issue to subscribers.
    events.emit(ws.as_str(), HangarEvent::IssueCreated(row.clone()));
    to_value(&row)
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
async fn handle_issue_update(
    pool: &SqlitePool,
    req: &RpcRequest,
    events: &EventSink,
) -> Result<serde_json::Value, RpcError> {
    use ainb_hangar_proto::events::HangarEvent;

    let params: ainb_hangar_proto::snapshots::IssueUpdateParams = parse_params(
        req,
        "{ workspace_id, issue_id, state?, assignee?, priority?, due_date? }",
    )?;
    // The mutating handler must not silently no-op on a typo'd workspace.
    let ws = resolve_wire_or_reject(pool, &params.workspace_id).await?;
    let update = issue_field_update_from_params(&params)?;
    let row = snapshots::issue_update(pool, ws.as_str(), &params.issue_id, &update)
        .await
        .map_err(|e| store_err(&e))?;
    // No row matched the (id, workspace) pair: an unknown id or a cross-tenant
    // issue. Reject rather than ack a write that never happened.
    let Some(row) = row else {
        return Err(invalid_params(&format!(
            "no issue `{}` in this workspace",
            params.issue_id
        )));
    };
    // A committed edit announces the refreshed row to subscribers.
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
    Ok(ainb_hangar_store::repo::issue::IssueFieldUpdate {
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
    let row = snapshots::agent_update(pool, ws.as_str(), &params.agent_id, &update)
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
    let row = snapshots::agent_archive(pool, ws.as_str(), &params.agent_id, params.archived)
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
        let tasks = snapshots::tasks_list(store.pool(), crate::seed::WS_ID).await.unwrap();
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
        let tasks = snapshots::tasks_list(store.pool(), crate::seed::WS_ID).await.unwrap();
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
}
