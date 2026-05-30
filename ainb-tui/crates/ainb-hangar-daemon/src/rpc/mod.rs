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

pub mod snapshots;

use std::path::{Path, PathBuf};
use std::time::Instant;

use ainb_hangar_core::ids::{AgentId, SkillId, WorkspaceId};
use ainb_hangar_proto::methods;
use ainb_hangar_proto::settings::HealthSnapshot;
use ainb_hangar_proto::{RpcError, RpcId, RpcRequest, RpcResponse};
use sqlx::SqlitePool;
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

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

/// Bind the listener at `socket_path`, removing any stale socket file first.
///
/// # Errors
///
/// Returns an error if the parent directory is missing/unwritable or the bind
/// fails for a reason other than a stale socket file (which is removed and
/// retried once).
pub fn bind(socket_path: &Path) -> std::io::Result<UnixListener> {
    // A leftover socket file from a previous (crashed) daemon would make `bind`
    // fail with AddrInUse even though nothing is listening. Remove it first —
    // this is safe because only one daemon owns a given hangar home at a time.
    if socket_path.exists() {
        let _ = std::fs::remove_file(socket_path);
    }
    UnixListener::bind(socket_path)
}

/// Accept connections forever, serving each on its own task.
///
/// Never returns under normal operation; the caller runs it as a background
/// task alongside the claim loop. A single accept error is logged and the loop
/// continues (one bad connection must not down the listener).
pub async fn serve(listener: UnixListener, pool: SqlitePool, health: DaemonHealth) {
    loop {
        match listener.accept().await {
            Ok((stream, _addr)) => {
                let pool = pool.clone();
                let health = health.clone();
                tokio::spawn(async move {
                    if let Err(e) = serve_conn(stream, pool, health).await {
                        tracing::debug!(error = %e, "hangar rpc connection closed");
                    }
                });
            }
            Err(e) => tracing::warn!(error = %e, "hangar rpc accept failed"),
        }
    }
}

/// Serve one plugin connection: read framed requests, dispatch, write responses,
/// until EOF.
async fn serve_conn(
    stream: UnixStream,
    pool: SqlitePool,
    health: DaemonHealth,
) -> std::io::Result<()> {
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);
    while let Some(body) = read_frame(&mut reader).await? {
        let resp = match serde_json::from_slice::<RpcRequest>(&body) {
            Ok(req) => dispatch(&pool, &req, &health).await,
            Err(e) => RpcResponse {
                jsonrpc: ainb_hangar_proto::jsonrpc_version(),
                // We could not parse an id; reply with a null/0 id so the peer
                // still sees a framed error rather than a dropped connection.
                id: RpcId::Number(0),
                result: None,
                error: Some(RpcError {
                    code: INVALID_PARAMS,
                    message: format!("malformed request: {e}"),
                    data: None,
                }),
            },
        };
        let frame = encode_frame(&resp);
        write_half.write_all(&frame).await?;
        write_half.flush().await?;
    }
    Ok(())
}

/// Dispatch one decoded request to its handler, returning the response envelope.
///
/// Pure of socket IO (the caller owns the stream); only touches the store. Every
/// method echoes the request id; an unknown method answers `-32601`.
pub async fn dispatch(pool: &SqlitePool, req: &RpcRequest, health: &DaemonHealth) -> RpcResponse {
    let result = handle(pool, req, health).await;
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
        methods::HANGAR_HEALTH => to_value(&health.snapshot(true)),
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
        }
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
        )
        .await;
        assert_eq!(resp.error.unwrap().code, INVALID_PARAMS);
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
        )
        .await;
        assert!(resp.error.is_none());
        assert_eq!(resp.result.unwrap()["issues"].as_array().unwrap().len(), 0);
    }
}
