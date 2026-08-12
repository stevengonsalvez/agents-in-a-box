//! Persistent managed Codex app-server transport.
//!
//! One actor owns the proxy reader and writer. Commands enter through a
//! cloneable handle, while server requests and notifications leave through one
//! bounded ordered receiver. This prevents competing reads from reordering
//! app-server lifecycle events or mismatching JSON-RPC responses.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::OnceLock;
use std::time::Duration;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use futures_util::{SinkExt, StreamExt};
use nix::errno::Errno;
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::net::UnixStream;
use tokio::process::{Child, Command};
use tokio::sync::RwLock;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::{Instant, sleep};
use tokio_tungstenite::{WebSocketStream, client_async, tungstenite::Message};

#[cfg(test)]
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};

use super::codex::{
    CodexApprovalKind, CodexApprovalRequest, CodexCapabilities, CodexInboundEnvelope,
    CodexQuestionRequest, CommandSpec, RpcRequestId, app_server_command, managed_tui_command,
    parse_inbound_envelope, probe_codex,
};
use super::{ApprovalDecision, ProviderError, ProviderReceipt, QuestionAnswer};

use crate::events::EventSink;

const INITIALIZE_ID: u64 = 1;

static ACTIVE_MANAGER: OnceLock<RwLock<Option<CodexManagerHandle>>> = OnceLock::new();

static TRANSPORT_HEALTH: OnceLock<RwLock<CodexTransportHealth>> = OnceLock::new();

/// Persistent manager configuration.
#[derive(Debug, Clone)]
pub struct CodexManagerConfig {
    /// Codex executable path.
    pub codex_binary: OsString,
    /// Shared app-server Unix socket.
    pub socket_path: PathBuf,
    /// Fleet client version sent during initialize.
    pub client_version: String,
    /// Maximum app-server socket startup wait.
    pub startup_timeout: Duration,
    /// Maximum wait for one app-server command response.
    pub request_timeout: Duration,
    /// Ordered inbound channel capacity.
    pub event_capacity: usize,
}

impl CodexManagerConfig {
    /// Build configuration with conservative local defaults.
    pub fn new(codex_binary: impl Into<OsString>, socket_path: impl Into<PathBuf>) -> Self {
        Self {
            codex_binary: codex_binary.into(),
            socket_path: socket_path.into(),
            client_version: env!("CARGO_PKG_VERSION").into(),
            startup_timeout: Duration::from_secs(5),
            request_timeout: Duration::from_secs(10),
            event_capacity: 256,
        }
    }
}

/// Cloneable command channel for one persistent Codex connection.
#[derive(Clone)]
pub struct CodexManagerHandle {
    commands: mpsc::Sender<ManagerCommand>,
    capabilities: Arc<CodexCapabilities>,
    socket_path: Arc<PathBuf>,
    owns_server: bool,
    request_timeout: Duration,
}

impl CodexManagerHandle {
    /// Negotiated version-probed capabilities.
    pub fn capabilities(&self) -> &CodexCapabilities {
        &self.capabilities
    }

    /// Canonical Unix socket shared by every Ainb-managed Codex thread.
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    /// Ownership of the app-server bound at [`Self::socket_path`].
    pub fn ownership(&self) -> &'static str {
        if self.owns_server { "owned" } else { "adopted" }
    }

    /// Managed TUI command connected to this manager's shared app-server.
    pub fn managed_tui_command(
        &self,
        codex_binary: &std::ffi::OsStr,
        additional_args: impl IntoIterator<Item = OsString>,
    ) -> CommandSpec {
        managed_tui_command(codex_binary, &self.socket_path, additional_args)
    }

    /// Start a new app-server thread and return exact thread ID.
    pub async fn thread_start(
        &self,
        cwd: &Path,
        model: Option<&str>,
    ) -> Result<String, ProviderError> {
        let result = self
            .request(
                "thread/start",
                json!({ "cwd": cwd, "model": model, "ephemeral": false, "threadSource": "user" }),
            )
            .await?;
        nested_id(&result, "thread")
    }

    /// Start an Interactive thread with its launch policy set on the server.
    ///
    /// Remote clients do not carry CLI environment or permission flags to the
    /// app-server, so this policy must be attached when the thread is created.
    pub async fn thread_start_interactive(
        &self,
        cwd: &Path,
        model: Option<&str>,
        skip_permissions: bool,
    ) -> Result<String, ProviderError> {
        let result = self
            .request(
                "thread/start",
                interactive_thread_start_params(cwd, model, skip_permissions),
            )
            .await?;
        nested_id(&result, "thread")
    }

    /// Resume exact app-server thread.
    pub async fn thread_resume(&self, thread_id: &str) -> Result<Value, ProviderError> {
        self.request("thread/resume", json!({ "threadId": thread_id })).await
    }

    /// Read exact app-server thread with turns included.
    pub async fn thread_read(&self, thread_id: &str) -> Result<Value, ProviderError> {
        self.request(
            "thread/read",
            json!({ "threadId": thread_id, "includeTurns": true }),
        )
        .await
    }

    /// Archive exact app-server thread when supported by installed schema.
    pub async fn thread_archive(&self, thread_id: &str) -> Result<Value, ProviderError> {
        if !self.capabilities.thread_archive {
            return Err(ProviderError::Unsupported(
                "Codex thread archive absent from generated schema".into(),
            ));
        }
        self.request("thread/archive", json!({ "threadId": thread_id })).await
    }

    /// Start turn with one text input and return exact turn ID.
    pub async fn turn_start(&self, thread_id: &str, text: &str) -> Result<String, ProviderError> {
        let result = self
            .request(
                "turn/start",
                json!({
                    "threadId": thread_id,
                    "input": [{ "type": "text", "text": text }],
                }),
            )
            .await?;
        nested_id(&result, "turn")
    }

    /// Interrupt exact active turn.
    pub async fn turn_interrupt(
        &self,
        thread_id: &str,
        turn_id: &str,
    ) -> Result<Value, ProviderError> {
        self.request(
            "turn/interrupt",
            json!({ "threadId": thread_id, "turnId": turn_id }),
        )
        .await
    }

    /// Answer exact request-user-input server request.
    pub async fn answer_request_user_input(
        &self,
        request: &CodexQuestionRequest,
        answers: &[QuestionAnswer],
    ) -> Result<ProviderReceipt, ProviderError> {
        if !self.capabilities.request_user_input {
            return Err(ProviderError::Unsupported(
                "Codex request-user-input absent from generated schema".into(),
            ));
        }
        validate_answers(request, answers)?;
        let answer_map = answers
            .iter()
            .map(|answer| {
                (
                    answer.question_id.clone(),
                    json!({ "answers": answer.answers }),
                )
            })
            .collect::<serde_json::Map<_, _>>();
        self.respond(
            request.identity.request_id.clone(),
            json!({ "answers": answer_map }),
        )
        .await?;
        Ok(ProviderReceipt {
            authoritative: true,
            transport: "codex-app-server-manager",
        })
    }

    /// Decide exact app-server approval request.
    pub async fn decide_approval(
        &self,
        request: &CodexApprovalRequest,
        decision: ApprovalDecision,
    ) -> Result<ProviderReceipt, ProviderError> {
        if !self.capabilities.approvals {
            return Err(ProviderError::Unsupported(
                "Codex approvals absent from generated schema".into(),
            ));
        }
        self.respond(
            request.identity.request_id.clone(),
            approval_result(request, decision)?,
        )
        .await?;
        if decision == ApprovalDecision::DenyAndInterrupt
            && request.kind == CodexApprovalKind::Permissions
        {
            self.turn_interrupt(&request.identity.thread_id, &request.identity.turn_id)
                .await?;
        }
        Ok(ProviderReceipt {
            authoritative: true,
            transport: "codex-app-server-manager",
        })
    }

    /// Request graceful actor shutdown.
    pub async fn shutdown(&self) -> Result<(), ProviderError> {
        let (reply, response) = oneshot::channel();
        self.with_timeout(async {
            self.commands
                .send(ManagerCommand::Shutdown { reply })
                .await
                .map_err(|_| manager_closed())?;
            response.await.map_err(|_| manager_closed())?
        })
        .await
    }

    async fn request(&self, method: &'static str, params: Value) -> Result<Value, ProviderError> {
        let (reply, response) = oneshot::channel();
        self.with_timeout(async {
            self.commands
                .send(ManagerCommand::Request {
                    method,
                    params,
                    reply,
                })
                .await
                .map_err(|_| manager_closed())?;
            response.await.map_err(|_| manager_closed())?
        })
        .await
    }

    async fn respond(&self, request_id: RpcRequestId, result: Value) -> Result<(), ProviderError> {
        let (reply, response) = oneshot::channel();
        self.with_timeout(async {
            self.commands
                .send(ManagerCommand::Respond {
                    request_id,
                    result,
                    reply,
                })
                .await
                .map_err(|_| manager_closed())?;
            response.await.map_err(|_| manager_closed())?
        })
        .await
    }

    async fn with_timeout<T>(
        &self,
        future: impl Future<Output = Result<T, ProviderError>>,
    ) -> Result<T, ProviderError> {
        tokio::time::timeout(self.request_timeout, future)
            .await
            .map_err(|_| ProviderError::Transport("Codex manager command timed out".into()))?
    }
}

fn interactive_thread_start_params(
    cwd: &Path,
    model: Option<&str>,
    skip_permissions: bool,
) -> Value {
    let mut params =
        json!({ "cwd": cwd, "model": model, "ephemeral": false, "threadSource": "user" });
    if skip_permissions {
        let object = params.as_object_mut().expect("interactive thread params are an object");
        object.insert("approvalPolicy".into(), json!("never"));
        object.insert("sandbox".into(), json!("danger-full-access"));
    }
    params
}

/// Running manager, ordered inbound receiver, and actor join handle.
pub struct ManagedCodexManager {
    /// Cloneable control handle.
    pub handle: CodexManagerHandle,
    /// Ordered server requests and notifications.
    pub events: mpsc::Receiver<CodexInboundEnvelope>,
    task: JoinHandle<Result<(), ProviderError>>,
}

impl ManagedCodexManager {
    /// Wait for actor exit and child cleanup.
    pub async fn wait(self) -> Result<(), ProviderError> {
        self.task.await.map_err(|error| {
            ProviderError::Transport(format!("Codex manager task failed: {error}"))
        })?
    }
}

/// Return active process-wide Codex manager handle when transport is healthy.
pub async fn active_handle() -> Option<CodexManagerHandle> {
    active_manager().read().await.clone()
}

/// Wait briefly for daemon startup to publish its manager handle.
pub async fn wait_for_active_handle(timeout: Duration) -> Option<CodexManagerHandle> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(handle) = active_handle().await {
            return Some(handle);
        }
        if Instant::now() >= deadline {
            return None;
        }
        sleep(Duration::from_millis(50)).await;
    }
}

/// Health of the managed Codex transport, for the daemon's status surfaces.
///
/// The service loop retries forever by design; without this record a wedged
/// transport is invisible: the observed run reached attempt 225 over ~2h with
/// nothing but WARN lines to show for it.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct CodexTransportHealth {
    /// Failed spawn/serve cycles since the last healthy transport.
    pub consecutive_failures: usize,
    /// Whether the streak reached [`CODEX_SERVICE_DEGRADED_AFTER`].
    pub degraded: bool,
    /// Why the most recent cycle ended. Not always an error: a transport that
    /// closes cleanly and gets respawned is still a failed cycle.
    pub last_failure: Option<String>,
    /// Epoch ms of the most recent failure.
    pub last_failure_at: Option<i64>,
}

/// Current managed-Codex transport health snapshot.
///
/// TODO(phase-5 follow-up): nothing reads this yet, so today the only surfaced
/// signal is the ERROR log. `ainb fleet daemons` has no hangar-daemon row at all
/// (`ainb_core::fleet::daemons::probe::DaemonKind` enumerates bridge, notifyd,
/// approve-broker, ATC and fleet-daemon only), and this crate cannot depend on
/// `ainb-core` to add one: the dependency runs the other way. Surfacing it needs a
/// new `DaemonKind` plus a heartbeat writer in `ainb-core`, or a field on the daemon
/// status RPC in `rpc/mod.rs`; both are outside this file.
pub async fn transport_health() -> CodexTransportHealth {
    transport_health_state().read().await.clone()
}

/// One locally discovered Codex app-server. Socket paths stay private.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexAppServerInventoryRow {
    /// Local process identifier.
    pub pid: u32,
    /// `owned`, `adopted`, or `external`.
    pub ownership: String,
    /// Whether the process requested remote control enrollment.
    pub remote_control: bool,
    /// Managed transport health, or `unknown` for external processes.
    pub health: String,
}

/// Bounded local app-server inventory for diagnostics.
pub async fn app_server_inventory() -> Vec<CodexAppServerInventoryRow> {
    let active = active_handle().await;
    let health = transport_health().await;
    let Some(ps_output) = ps_process_table().await else {
        return Vec::new();
    };
    ps_output
        .lines()
        .filter_map(parse_ps_row)
        .filter(|row| is_codex_app_server(row.args) && !row.args.contains("app-server proxy"))
        .map(|row| {
            let managed = active.as_ref().is_some_and(|handle| {
                codex_server_socket(row.args)
                    .is_some_and(|socket| socket == handle.socket_path().to_string_lossy())
            });
            CodexAppServerInventoryRow {
                pid: row.pid,
                ownership: if managed {
                    active.as_ref().expect("active checked").ownership().to_string()
                } else {
                    "external".to_string()
                },
                remote_control: row.args.split_whitespace().any(|arg| arg == "--remote-control"),
                health: if managed {
                    if health.degraded {
                        "degraded"
                    } else {
                        "healthy"
                    }
                    .to_string()
                } else {
                    "unknown".to_string()
                },
            }
        })
        .take(APP_SERVER_INVENTORY_LIMIT)
        .collect()
}

/// Consecutive failures after which the transport is called degraded and logged at
/// ERROR instead of WARN.
///
/// Five failures spans ~31s of capped backoff (1+2+4+8+16), long enough that a
/// `codex` upgrade or a one-off restart does not cry wolf, short enough that a truly
/// wedged transport is loud within a minute.
pub const CODEX_SERVICE_DEGRADED_AFTER: usize = 5;

/// Whether a streak of `consecutive_failures` counts as a degraded transport.
const fn service_failures_are_degraded(consecutive_failures: usize) -> bool {
    consecutive_failures >= CODEX_SERVICE_DEGRADED_AFTER
}

/// How long a transport must serve before its cycle counts as healthy.
///
/// Comfortably longer than the 16s backoff cap, so a flap can never out-run the
/// streak, and far shorter than any real session.
const MIN_HEALTHY_UPTIME: Duration = Duration::from_secs(60);

/// Whether a transport that served for `uptime` earns a cleared retry streak.
const fn transport_cycle_was_healthy(uptime: Duration) -> bool {
    uptime.as_secs() >= MIN_HEALTHY_UPTIME.as_secs()
}

/// Record one failed transport cycle and log it at the severity the streak earns.
///
/// `attempt` is the pre-increment retry counter, so this cycle is failure
/// `attempt + 1`. `reason` distinguishes the two ways a cycle ends (a spawn error, or
/// a transport that served and then closed) and is preserved in the log field and the
/// health record. Returns the recorded health so callers (and tests) can assert on
/// the escalation rather than scrape logs.
async fn note_service_failure(attempt: usize, reason: &str) -> CodexTransportHealth {
    let consecutive_failures = attempt.saturating_add(1);
    let degraded = service_failures_are_degraded(consecutive_failures);
    let health = CodexTransportHealth {
        consecutive_failures,
        degraded,
        last_failure: Some(reason.to_string()),
        last_failure_at: Some(epoch_millis()),
    };
    *transport_health_state().write().await = health.clone();
    if degraded {
        tracing::error!(
            consecutive_failures,
            reason,
            "Codex managed transport degraded: repeated failures, no working app-server"
        );
    } else {
        tracing::warn!(
            attempt = consecutive_failures,
            reason,
            "Codex managed transport unavailable"
        );
    }
    health
}

/// Clear the failure streak once a transport comes up.
async fn clear_service_failures() {
    *transport_health_state().write().await = CodexTransportHealth::default();
}

/// Spawn best-effort daemon service and authoritative Fleet ingest loop.
pub fn spawn_service(
    config: CodexManagerConfig,
    pool: sqlx::SqlitePool,
    events: EventSink,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut attempt = 0_usize;
        loop {
            let manager = match spawn(config.clone()).await {
                Ok(manager) => manager,
                Err(error) => {
                    note_service_failure(attempt, &error.to_string()).await;
                    if let Err(downgrade) =
                        crate::fleet::mark_codex_manager_unavailable(&pool, &events, epoch_millis())
                            .await
                    {
                        tracing::warn!(error = %downgrade, "Codex Fleet transport downgrade failed");
                    }
                    sleep(service_backoff(attempt)).await;
                    attempt = attempt.saturating_add(1);
                    continue;
                }
            };
            clear_service_failures().await;
            let ready_at = Instant::now();
            let ManagedCodexManager {
                handle,
                events: mut inbound,
                task,
            } = manager;
            *active_manager().write().await = Some(handle.clone());
            tracing::info!(
                version = %handle.capabilities().cli_version,
                request_user_input = handle.capabilities().request_user_input,
                approvals = handle.capabilities().approvals,
                "Codex managed transport ready"
            );
            if let Err(error) =
                crate::fleet::recover_codex_manager(&pool, &events, &handle, epoch_millis()).await
            {
                tracing::warn!(error = %error, "Codex Fleet recovery sweep failed");
            }
            if let Err(error) =
                crate::fleet::replay_unprojected_codex_events(&pool, &events, handle.capabilities())
                    .await
            {
                tracing::warn!(error = %error, "Codex Fleet source replay failed");
            }

            let boot_id = epoch_millis();
            let mut sequence = 0_u64;
            while let Some(event) = inbound.recv().await {
                sequence = sequence.wrapping_add(1);
                let event_id = format!("codex-manager:{boot_id}:{sequence}");
                if let Err(error) = crate::fleet::ingest_codex_inbound(
                    &pool,
                    &events,
                    event_id,
                    event,
                    handle.capabilities(),
                    epoch_millis(),
                )
                .await
                {
                    tracing::warn!(error = %error, "Codex Fleet event ingest failed");
                }
            }

            *active_manager().write().await = None;
            // Only a transport that actually SERVED for a while clears the streak.
            // Resetting on spawn success alone would pin a spawn-then-die flap at
            // failure 1 forever: never escalating, and respawning a node app-server
            // every second because the backoff resets too.
            if transport_cycle_was_healthy(ready_at.elapsed()) {
                reset_service_attempt(&mut attempt);
            }
            // A transport that closed cleanly still means the app-server went away and
            // we are about to respawn, so it counts toward the failure streak: a silent
            // respawn loop is the same outage as a loud one.
            let stopped = task
                .await
                .map_err(|join| {
                    ProviderError::Transport(format!("Codex manager task failed: {join}"))
                })
                .and_then(|result| result)
                .err()
                .map_or_else(
                    || "Codex managed transport closed".to_string(),
                    |error| error.to_string(),
                );
            note_service_failure(attempt, &stopped).await;
            if let Err(error) =
                crate::fleet::mark_codex_manager_unavailable(&pool, &events, epoch_millis()).await
            {
                tracing::warn!(error = %error, "Codex Fleet transport downgrade failed");
            }
            sleep(service_backoff(attempt)).await;
            attempt = attempt.saturating_add(1);
        }
    })
}

fn service_backoff(attempt: usize) -> Duration {
    Duration::from_secs(1_u64 << attempt.min(4))
}

fn reset_service_attempt(attempt: &mut usize) {
    *attempt = 0;
}

fn active_manager() -> &'static RwLock<Option<CodexManagerHandle>> {
    ACTIVE_MANAGER.get_or_init(|| RwLock::new(None))
}

fn transport_health_state() -> &'static RwLock<CodexTransportHealth> {
    TRANSPORT_HEALTH.get_or_init(|| RwLock::new(CodexTransportHealth::default()))
}

fn epoch_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| {
            i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
        })
}

/// Spawn shared app-server, connect its Unix WebSocket, initialize protocol, then start manager.
pub async fn spawn(config: CodexManagerConfig) -> Result<ManagedCodexManager, ProviderError> {
    let probe_binary = config.codex_binary.clone();
    let capabilities = tokio::task::spawn_blocking(move || probe_codex(&probe_binary))
        .await
        .map_err(|error| ProviderError::Transport(format!("Codex probe task failed: {error}")))?;
    if !capabilities.app_server {
        return Err(ProviderError::Unsupported(
            "installed Codex cannot generate app-server schema".into(),
        ));
    }
    let preparation = prepare_socket(&config.socket_path, &config.codex_binary).await?;
    let mut owns_server = preparation.owns_server;
    let owner_marker_path = socket_owner_marker(&config.socket_path);
    let mut server = if !owns_server {
        None
    } else {
        let codex_home = prepare_scoped_codex_home(&config.socket_path).await?;
        let mut server_command = tokio_command(app_server_command(
            &config.codex_binary,
            &config.socket_path,
        ));
        server_command.env("CODEX_HOME", &codex_home);
        server_command.kill_on_drop(true);
        let mut child = server_command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        if let Err(error) =
            wait_for_socket(&mut child, &config.socket_path, config.startup_timeout).await
        {
            stop_child(&mut child).await;
            return Err(error);
        }
        let pid = child.id().ok_or_else(|| {
            ProviderError::Transport("Codex app-server process id unavailable".into())
        })?;
        // `wait_for_socket` only proves the socket path appeared, not that THIS child
        // bound it. `prepare_socket` is a check-then-act with no lock, so concurrent
        // callers can each decide they own the server; without this check every loser
        // leaks a live app-server that nothing holds a handle to.
        let listeners = socket_listener_pids(&config.socket_path).await;
        if bound_by_child(pid, listeners.as_deref()) {
            let identity = process_identity(pid).await.ok_or_else(|| {
                ProviderError::Transport("Codex app-server process identity unavailable".into())
            })?;
            let marker = SocketOwnerMarker {
                schema: 1,
                pid,
                process_start_fingerprint: identity.process_start_fingerprint,
                executable: identity.executable,
            };
            let marker_json = serde_json::to_vec(&marker)?;
            if let Err(error) = tokio::fs::write(&owner_marker_path, marker_json).await {
                stop_child(&mut child).await;
                remove_owned_socket(Some(&config.socket_path), Some(&owner_marker_path)).await;
                return Err(error.into());
            }
            Some(child)
        } else {
            // Lost the bind race: reap our own server and adopt the winner's.
            stop_child(&mut child).await;
            owns_server = false;
            None
        }
    };

    let socket = match UnixStream::connect(&config.socket_path).await {
        Ok(socket) => socket,
        Err(error) => {
            if let Some(server) = server.as_mut() {
                stop_child(server).await;
            }
            if owns_server {
                remove_owned_socket(Some(&config.socket_path), Some(&owner_marker_path)).await;
            }
            return Err(ProviderError::Transport(format!(
                "Codex app-server Unix socket connection failed: {error}"
            )));
        }
    };
    let websocket = match client_async("ws://localhost/", socket).await {
        Ok((websocket, _)) => websocket,
        Err(error) => {
            if let Some(server) = server.as_mut() {
                stop_child(server).await;
            }
            if owns_server {
                remove_owned_socket(Some(&config.socket_path), Some(&owner_marker_path)).await;
            }
            return Err(ProviderError::Transport(format!(
                "Codex app-server WebSocket handshake failed: {error}"
            )));
        }
    };
    let cleanup: Box<dyn ProcessCleanup> = Box::new(ServerCleanup {
        server,
        owned_socket_path: owns_server.then(|| config.socket_path.clone()),
        owner_marker_path: owns_server.then_some(owner_marker_path),
    });

    spawn_connection(
        WebSocketTransport { websocket },
        capabilities,
        config,
        owns_server,
        preparation.marker_repair,
        cleanup,
    )
    .await
}

/// Ainb owns remote-control enrollment state, while reusing the user's existing
/// ChatGPT login without copying its credential file. This prevents Codex
/// Desktop and Ainb from racing for one persisted remote server identity.
async fn prepare_scoped_codex_home(socket_path: &Path) -> Result<PathBuf, ProviderError> {
    let scoped_home = socket_path.parent().unwrap_or_else(|| Path::new(".")).join("codex-home");
    let source_home = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".codex")))
        .ok_or_else(|| {
            ProviderError::Transport("cannot resolve Codex authentication home".into())
        })?;
    prepare_scoped_codex_home_with_auth(&scoped_home, &source_home.join("auth.json")).await?;
    Ok(scoped_home)
}

async fn prepare_scoped_codex_home_with_auth(
    scoped_home: &Path,
    source_auth: &Path,
) -> Result<(), ProviderError> {
    tokio::fs::create_dir_all(scoped_home).await?;
    #[cfg(unix)]
    tokio::fs::set_permissions(scoped_home, std::fs::Permissions::from_mode(0o700)).await?;

    let scoped_auth = scoped_home.join("auth.json");
    match tokio::fs::symlink_metadata(&scoped_auth).await {
        Ok(_) => return Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    if !source_auth.is_file() {
        return Err(ProviderError::Transport(format!(
            "Codex ChatGPT login unavailable at {}",
            source_auth.display()
        )));
    }
    #[cfg(unix)]
    tokio::fs::symlink(source_auth, &scoped_auth).await?;
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SocketDisposition {
    Create,
    Adopt,
    RecoverOwned,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SocketOwnerMarker {
    schema: u8,
    pid: u32,
    process_start_fingerprint: String,
    executable: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProcessIdentity {
    process_start_fingerprint: String,
    executable: String,
}

#[derive(Debug)]
struct SocketPreparation {
    owns_server: bool,
    marker_repair: Option<MarkerRepair>,
}

#[derive(Debug)]
struct MarkerRepair {
    path: PathBuf,
    expected: Option<Vec<u8>>,
    owner: SocketOwnerMarker,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SocketFileIdentity {
    device: u64,
    inode: u64,
}

const fn socket_disposition(
    socket_exists: bool,
    marker_owned: bool,
    identity_matches: bool,
) -> SocketDisposition {
    if !socket_exists {
        SocketDisposition::Create
    } else if marker_owned && !identity_matches {
        SocketDisposition::RecoverOwned
    } else {
        SocketDisposition::Adopt
    }
}

async fn prepare_socket(
    socket_path: &Path,
    codex_binary: &std::ffi::OsStr,
) -> Result<SocketPreparation, ProviderError> {
    if !socket_path.exists() {
        return Ok(SocketPreparation {
            owns_server: true,
            marker_repair: None,
        });
    }
    let marker = socket_owner_marker(socket_path);
    let marker_bytes = tokio::fs::read(&marker).await.ok();
    let owner = marker_bytes
        .as_deref()
        .and_then(|contents| serde_json::from_slice::<SocketOwnerMarker>(contents).ok());
    let marker_owned = owner.as_ref().is_some_and(|owner| {
        owner.schema == 1 && executable_matches(&owner.executable, codex_binary)
    });
    let identity_matches = match owner.as_ref().filter(|_| marker_owned) {
        Some(owner) => process_identity(owner.pid).await.is_some_and(|current| {
            current.process_start_fingerprint == owner.process_start_fingerprint
                && current.executable == owner.executable
        }),
        None => false,
    };
    match socket_disposition(true, marker_owned, identity_matches) {
        SocketDisposition::RecoverOwned => {
            prepare_unmarked_socket(socket_path, &marker, marker_bytes, codex_binary).await
        }
        SocketDisposition::Adopt if marker_owned => Ok(SocketPreparation {
            owns_server: false,
            marker_repair: None,
        }),
        SocketDisposition::Adopt if owner.is_some() => Ok(SocketPreparation {
            owns_server: false,
            marker_repair: None,
        }),
        SocketDisposition::Adopt => {
            prepare_unmarked_socket(socket_path, &marker, marker_bytes, codex_binary).await
        }
        SocketDisposition::Create => Ok(SocketPreparation {
            owns_server: true,
            marker_repair: None,
        }),
    }
}

async fn prepare_unmarked_socket(
    socket_path: &Path,
    marker_path: &Path,
    expected_marker: Option<Vec<u8>>,
    codex_binary: &std::ffi::OsStr,
) -> Result<SocketPreparation, ProviderError> {
    let identity = socket_file_identity(socket_path)?;
    match UnixStream::connect(socket_path).await {
        Ok(stream) => {
            drop(stream);
            let marker_repair =
                listener_owner_marker(socket_path, codex_binary)
                    .await
                    .map(|owner| MarkerRepair {
                        path: marker_path.to_path_buf(),
                        expected: expected_marker,
                        owner,
                    });
            Ok(SocketPreparation {
                owns_server: false,
                marker_repair,
            })
        }
        Err(error) if stale_socket_error(&error) => {
            if socket_file_identity(socket_path)? == identity {
                tokio::fs::remove_file(socket_path).await?;
                if expected_marker.is_some() {
                    let _ = tokio::fs::remove_file(marker_path).await;
                }
                Ok(SocketPreparation {
                    owns_server: true,
                    marker_repair: None,
                })
            } else {
                Ok(SocketPreparation {
                    owns_server: false,
                    marker_repair: None,
                })
            }
        }
        Err(_) => Ok(SocketPreparation {
            owns_server: false,
            marker_repair: None,
        }),
    }
}

fn socket_file_identity(path: &Path) -> Result<SocketFileIdentity, ProviderError> {
    use std::os::unix::fs::MetadataExt as _;
    let metadata = std::fs::symlink_metadata(path)?;
    Ok(SocketFileIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

fn stale_socket_error(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound
    )
}

async fn listener_owner_marker(
    socket_path: &Path,
    codex_binary: &std::ffi::OsStr,
) -> Option<SocketOwnerMarker> {
    let output = Command::new("lsof")
        .args(["-n", "-t", "--"])
        .arg(socket_path)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .await
        .ok()
        .filter(|output| output.status.success())?;
    let mut owners = Vec::new();
    for pid in String::from_utf8(output.stdout).ok()?.lines() {
        let pid = pid.trim().parse::<u32>().ok()?;
        let identity = process_identity(pid).await?;
        if executable_matches(&identity.executable, codex_binary) {
            owners.push(SocketOwnerMarker {
                schema: 1,
                pid,
                process_start_fingerprint: identity.process_start_fingerprint,
                executable: identity.executable,
            });
        }
    }
    (owners.len() == 1).then(|| owners.remove(0))
}

/// PIDs currently holding `socket_path`, or `None` when `lsof` cannot answer.
///
/// `None` means "unknown", never "nobody"; callers must not read it as proof the
/// socket is free.
async fn socket_listener_pids(socket_path: &Path) -> Option<Vec<u32>> {
    let output = Command::new("lsof")
        .args(["-n", "-t", "--"])
        .arg(socket_path)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .await
        .ok()
        .filter(|output| output.status.success())?;
    String::from_utf8(output.stdout)
        .ok()?
        .lines()
        .map(|line| line.trim().parse::<u32>().ok())
        .collect()
}

/// Whether the app-server just spawned is the sole process holding the socket.
///
/// `listeners == None` means `lsof` could not answer, so we trust the spawn rather
/// than kill a healthy server on a host without `lsof`. Any other shape (a different
/// pid, several pids, or none) means this child did not win the bind.
fn bound_by_child(child_pid: u32, listeners: Option<&[u32]>) -> bool {
    listeners.is_none_or(|pids| pids.len() == 1 && pids[0] == child_pid)
}

/// Diagnostic output bound. This never gates spawning: other Codex clients and
/// IDEs own independent app-servers, while Ainb reuses only its exact socket.
const APP_SERVER_INVENTORY_LIMIT: usize = 8;

/// One `ps -Ao pid,ppid,args` row, borrowed from the dump.
struct PsRow<'a> {
    pid: u32,
    ppid: u32,
    args: &'a str,
}

/// Split `s` into (first whitespace-delimited token, rest-including-whitespace).
/// `None` when there is no token (empty or all-whitespace).
fn split_first_token(s: &str) -> Option<(&str, &str)> {
    let s = s.trim_start();
    if s.is_empty() {
        return None;
    }
    Some(s.find(char::is_whitespace).map_or((s, ""), |end| (&s[..end], &s[end..])))
}

/// Parse one `ps -Ao pid,ppid,args` line into pid, ppid, and the args remainder.
/// Returns `None` when the pid/ppid columns do not parse (e.g. the header row).
fn parse_ps_row(line: &str) -> Option<PsRow<'_>> {
    let (pid, rest) = split_first_token(line)?;
    let pid = pid.parse::<u32>().ok()?;
    let (ppid, args) = split_first_token(rest)?;
    let ppid = ppid.parse::<u32>().ok()?;
    Some(PsRow {
        pid,
        ppid,
        args: args.trim_start(),
    })
}

/// Whether an argv is a codex app-server invocation from us or the plugin broker.
///
/// Anchored on argv STRUCTURE, not substrings. The executable must be an installed
/// `.../bin/codex` — either exec'd directly or run as `node <path>/bin/codex`, the
/// two shapes `ps` reports — and `app-server` must be a distinct token, not a
/// fragment of some other word.
///
/// This used to be `args.contains("bin/codex") && args.contains("app-server")` over
/// the whole command line, with `ppid == 1` as the only other filter, evaluated
/// every 60s. Any detached process that merely QUOTED both strings — a `tail` of a
/// log path, a `grep`, an editor, a shell one-liner — became a SIGTERM then SIGKILL
/// target within two seconds.
///
/// The desktop Codex/ChatGPT app (`.../ChatGPT.app/Contents/Resources/codex`) is
/// still excluded: its parent directory is not `bin`.
fn is_codex_app_server(args: &str) -> bool {
    let mut tokens = args.split_whitespace();
    let Some(argv0) = tokens.next() else {
        return false;
    };
    // `node .../bin/codex` puts the real executable in argv[1].
    if !is_installed_codex(argv0) && !tokens.next().is_some_and(is_installed_codex) {
        return false;
    }
    tokens.any(|token| token == "app-server")
}

/// Is `path` an installed `.../bin/codex` executable, by path components rather
/// than by substring?
fn is_installed_codex(path: &str) -> bool {
    let path = Path::new(path);
    path.file_name().is_some_and(|name| name == "codex")
        && path.parent().and_then(Path::file_name).is_some_and(|dir| dir == "bin")
}

/// Socket of a `codex app-server --listen unix://<socket>` row.
///
/// Takes the whole argv remainder, NOT the first token, because a Hangar home may
/// contain spaces (`/Users/x/Home A/codex-app-server.sock`, the fixture covers it).
/// That is only sound while the socket is the FINAL argument, which
/// `codex::app_server_command` and `codex::proxy_command` both guarantee; the socket
/// now feeds a filesystem lookup in [`adoption_is_credible`], so if a future `codex`
/// appends an argument after it, fix it here rather than at the call sites.
fn codex_server_socket(args: &str) -> Option<&str> {
    args.rsplit_once("app-server --listen unix://").map(|(_, socket)| socket.trim())
}

/// Socket consumed by a `codex app-server proxy --sock <socket>` row. Same
/// final-argument coupling as [`codex_server_socket`].
fn codex_proxy_socket(args: &str) -> Option<&str> {
    args.rsplit_once("app-server proxy --sock ").map(|(_, socket)| socket.trim())
}

/// Whether `pid` names a running process.
///
/// `kill(pid, 0)` answers `EPERM` for a live process owned by another user, so only
/// `ESRCH` proves death; every other errno is read as alive, which keeps callers on
/// the sparing side of an ambiguous answer. `pid <= 0` is never a process: raw 0
/// would signal our whole process group.
///
/// Deliberately the opposite polarity to `ainb_core::fleet::daemons::is_pid_alive`,
/// which treats `EPERM` as dead: that one decides whether to SHOW a daemon as
/// running, where a false positive is the bad outcome. Here a wrong answer kills a
/// process, so ambiguity must resolve to "alive". (The daemon crate cannot depend on
/// `ainb-core`, the dependency runs the other way, so the two cannot share code.)
fn pid_is_running(pid: u32) -> bool {
    i32::try_from(pid)
        .is_ok_and(|raw| raw > 0 && kill(Pid::from_raw(raw), None) != Err(Errno::ESRCH))
}

/// Whether a live proxy on `socket` is credible evidence that someone adopted the
/// server behind it.
///
/// The sparing rule exists to protect a server ADOPTED by another live Hangar home;
/// nothing previously checked that home still existed, so a server and its proxy
/// orphaned together by one daemon death vouched for each other forever. Two
/// judgements, in order:
///
/// 1. **Not a Hangar home** (`<parent>/hangar` is not a directory): somebody else's
///    socket, e.g. a plugin broker's temp dir. We have no pidfile to judge it by, so
///    we do not judge it: the spare stands. Widening kill authority to sockets this
///    daemon never managed would trade a leak for a destroyed live session.
/// 2. **A Hangar home**: the adopting daemon's registration is
///    `<home>/hangar/daemon.pid` (see [`crate::pid_path_in`]). Missing, unparseable,
///    or dead means abandoned, not adopted: reap. `<home>/hangar/` itself survives
///    a SIGKILL (it holds the store, the logs and the pid file), so a crashed home is
///    still reachable by judgement 2 rather than escaping through judgement 1.
///
/// Known limit: this is liveness, not identity. A recycled pid makes an orphan
/// immortal again. That fails toward sparing, so it is a leak, never a wrong kill;
/// closing it means a `ps` round-trip per candidate for a start-time fingerprint
/// (the [`SocketOwnerMarker`] shape), which is not worth it on a 60s sweep.
fn adoption_is_credible(socket: &str) -> bool {
    let Some(home) = Path::new(socket).parent() else {
        return true;
    };
    if !home.join("hangar").is_dir() {
        return true;
    }
    std::fs::read_to_string(crate::pid_path_in(home))
        .ok()
        .and_then(|contents| contents.trim().parse::<u32>().ok())
        .is_some_and(pid_is_running)
}

/// PIDs of orphaned codex app-server processes in a `ps -Ao pid,ppid,args` dump.
///
/// A ppid==1 server is spared only when a live proxy consumes its socket AND
/// `adoption_credible` accepts that proxy as evidence of a real adopter. A live
/// proxy alone proves nothing: proxy and server can be orphaned as a pair by the
/// same daemon death and then keep each other alive forever.
///
/// `adoption_credible` is injected so the selection stays pure and both forks are
/// unit-testable without spawning real daemons.
fn codex_orphans_to_reap(ps_output: &str, adoption_credible: impl Fn(&str) -> bool) -> Vec<u32> {
    let rows = ps_output.lines().filter_map(parse_ps_row).collect::<Vec<_>>();
    let live_proxy_sockets = rows
        .iter()
        .filter(|row| is_codex_app_server(row.args))
        .filter_map(|row| codex_proxy_socket(row.args))
        .collect::<std::collections::HashSet<_>>();

    rows.iter()
        .filter(|row| {
            row.ppid == 1 && is_codex_app_server(row.args) && !row.args.contains("app-server proxy")
        })
        .filter(|row| {
            // No parseable `--listen unix://` socket means nothing can be consuming
            // it, so there is no adoption to verify.
            codex_server_socket(row.args).is_none_or(|socket| {
                !(live_proxy_sockets.contains(socket) && adoption_credible(socket))
            })
        })
        .map(|row| row.pid)
        .collect()
}

/// An obsolete pre-lock Hangar daemon that still drives a legacy stdio proxy.
///
/// This is deliberately narrower than the app-server orphan reaper. The old
/// daemon must be orphaned, recognisably Hangar, and have a direct child proxy
/// for this exact home socket before Ainb is allowed to stop it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct LegacyProxyDaemon {
    pid: u32,
    process_start_fingerprint: String,
    proxy_pid: u32,
    proxy_start_fingerprint: String,
}

/// Select orphaned legacy Hangar daemon pids whose direct proxy child targets
/// `live_socket`.
///
/// A proxy on its own is not authority to signal anything. The parent-child
/// relationship confines this compatibility recovery to the obsolete Ainb
/// topology that predated the native WebSocket manager.
fn legacy_proxy_daemon_pairs(ps_output: &str, live_socket: &Path) -> Vec<(u32, u32)> {
    let expected_socket = live_socket.to_string_lossy();
    let rows = ps_output.lines().filter_map(parse_ps_row).collect::<Vec<_>>();
    rows.iter()
        .filter(|parent| {
            parent.ppid == 1 && crate::single_instance::is_hangar_daemon_args(parent.args)
        })
        .flat_map(|parent| {
            rows.iter().filter_map(|child| {
                (child.ppid == parent.pid
                    && is_codex_app_server(child.args)
                    && codex_proxy_socket(child.args)
                        .is_some_and(|socket| socket == expected_socket))
                .then_some((parent.pid, child.pid))
            })
        })
        .collect()
}

/// Confirm a selected daemon remains the same process before signalling it.
///
/// A PID is reusable after the first process exits, so the start fingerprint is
/// required before both TERM and KILL. The fresh process-table check also proves
/// the legacy parent-child topology still targets this exact home.
async fn legacy_proxy_daemon_is_current(
    candidate: &LegacyProxyDaemon,
    live_socket: &Path,
) -> bool {
    process_identity(candidate.pid)
        .await
        .is_some_and(|identity| identity.process_start_fingerprint == candidate.process_start_fingerprint)
        && process_identity(candidate.proxy_pid).await.is_some_and(|identity| {
            identity.process_start_fingerprint == candidate.proxy_start_fingerprint
        })
        && ps_process_table()
            .await
            .is_some_and(|ps_output| {
                legacy_proxy_daemon_pairs(&ps_output, live_socket)
                    .contains(&(candidate.pid, candidate.proxy_pid))
            })
}

/// Confirm the proxy child remains the exact same process after its parent exits.
async fn legacy_proxy_child_is_current(candidate: &LegacyProxyDaemon, live_socket: &Path) -> bool {
    process_identity(candidate.proxy_pid)
        .await
        .is_some_and(|identity| identity.process_start_fingerprint == candidate.proxy_start_fingerprint)
        && ps_process_table().await.is_some_and(|ps_output| {
            ps_output.lines().filter_map(parse_ps_row).any(|row| {
                row.pid == candidate.proxy_pid
                    && is_codex_app_server(row.args)
                    && codex_proxy_socket(row.args)
                        .is_some_and(|socket| socket == live_socket.to_string_lossy())
            })
        })
}

/// From ppid==1 orphan candidates, drop our own pid and the pid(s) listening on our
/// shared socket, then return who to signal.
///
/// Machine-wide live proxy consumers are removed before this function. The
/// listener spare also protects this daemon's adoption window before its proxy
/// starts.
fn reap_targets(candidates: &[u32], listeners: Option<&[u32]>, self_pid: u32) -> Vec<u32> {
    candidates
        .iter()
        .copied()
        .filter(|pid| *pid != self_pid)
        .filter(|pid| listeners.is_none_or(|live| !live.contains(pid)))
        .collect()
}

/// Read the full process table via `ps -Ao pid,ppid,args`, or `None` on failure.
async fn ps_process_table() -> Option<String> {
    let output = Command::new("ps")
        .args(["-Ao", "pid,ppid,args"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .await
        .ok()
        .filter(|output| output.status.success())?;
    String::from_utf8(output.stdout).ok()
}

/// Waits governing one reap sweep's SIGTERM → SIGKILL escalation.
///
/// Injected rather than hardcoded so tests exercise the escalation without burning
/// two real seconds per case.
#[derive(Debug, Clone, Copy)]
struct ReapTiming {
    /// Time a SIGTERMed orphan gets to exit before the SIGKILL escalation.
    term_grace: Duration,
    /// Time a SIGKILLed orphan gets to disappear before it is called unkillable.
    kill_confirm: Duration,
    /// Liveness poll step inside both waits.
    poll: Duration,
}

impl ReapTiming {
    /// Production waits: 1.5s to exit politely, 0.5s to confirm the kill, probed
    /// every 100ms. A node app-server shuts down in tens of milliseconds, and at the
    /// default sweep interval the whole sweep runs once per 60s, so a 2s worst case
    /// is free. Only reached when an orphan actually ignores SIGTERM.
    const PRODUCTION: Self = Self {
        term_grace: Duration::from_millis(1_500),
        kill_confirm: Duration::from_millis(500),
        poll: Duration::from_millis(100),
    };
}

/// What one reap sweep actually achieved, as opposed to what it signalled.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct ReapOutcome {
    /// Processes confirmed gone (including those already dead when signalled).
    reaped: usize,
    /// Of `reaped`, how many ignored SIGTERM and needed SIGKILL.
    escalated: usize,
    /// Still alive after SIGKILL, or impossible to signal at all.
    survived: usize,
    /// Left alone because ownership could not be proved (see [`OwnedPid`]) — a
    /// codex belonging to another home, another user, or another tool.
    unproven: usize,
}

/// Signal delivery seam: `(pid, signal)` where `None` is the liveness probe.
///
/// Injected so the SIGTERM → SIGKILL escalation is testable without real processes.
/// `Send + Sync` because the reap sweep is held across awaits inside a spawned task.
type Signaller<'a> = &'a (dyn Fn(u32, Option<Signal>) -> Result<(), Errno> + Send + Sync);

/// A pid this daemon has PROVED it may signal.
///
/// The only constructor outside this module's tests is
/// [`OwnedPid::holding_socket`], and [`deliver`] is the only path that sends a real
/// signal, so "kill a process we do not own" is a type error rather than a review
/// miss. A liveness probe (`kill(pid, 0)`) needs no proof: it changes nothing.
#[derive(Debug)]
struct OwnedPid(u32);

impl OwnedPid {
    /// Prove `pid` is one of THIS home's codex app-servers, or refuse to signal it.
    ///
    /// The proof is possession: the process must hold a unix socket bound to
    /// `socket` (`<hangar home>/codex-app-server.sock`) under the current uid.
    /// A server orphaned by a crashed daemon of this home still reports that bound
    /// name even after its socket file has been replaced, which is exactly the leak
    /// the sweep exists to clear; a codex belonging to another `$AINB_HANGAR_HOME`,
    /// another user, or another tool entirely can never satisfy it.
    ///
    /// Fails closed: no `lsof`, an unreadable process, or any other ambiguity
    /// answers `None` and the caller spares the process.
    fn holding_socket(pid: u32, socket: &Path) -> Option<Self> {
        pid_holds_socket(pid, socket).then_some(Self(pid))
    }
}

/// The ONLY path that delivers a real signal: it needs an [`OwnedPid`].
fn deliver(owned: &OwnedPid, signal: Signal, send: Signaller<'_>) -> Result<(), Errno> {
    send(owned.0, Some(signal))
}

/// Ownership-proof seam, injected so the escalation stays testable without real
/// sockets. Synchronous by design: it shells out to `lsof` for at most a handful of
/// candidates on a 60s sweep (~40ms each), which is not worth an async seam.
type Prover<'a> = &'a (dyn Fn(u32) -> Option<OwnedPid> + Send + Sync);

/// Does `pid` hold a unix socket bound to `socket`, under the current uid?
///
/// `lsof -a -p <pid> -u <uid> -U -F n` lists the bound NAME of every unix socket the
/// process holds; `-a` ANDs the pid and uid filters, so another user's process
/// answers empty. Matching the bound name rather than looking the path up
/// (`lsof -- <path>`) is deliberate: an orphan whose socket file was replaced by a
/// successor still reports the name it bound.
///
/// (Duplicated in `ainb-plugin-notifyd::procs` and `ainb_core::cli::hangar`: this
/// crate cannot depend on either, the dependency runs the other way.)
fn pid_holds_socket(pid: u32, socket: &Path) -> bool {
    let uid = nix::unistd::Uid::current().as_raw().to_string();
    let Ok(out) = std::process::Command::new("lsof")
        .args(["-a", "-p", &pid.to_string(), "-u", &uid, "-U", "-F", "n"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
    else {
        return false;
    };
    if !out.status.success() {
        return false;
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| line.strip_prefix('n'))
        .map(ainb_hangar_core::lsof::strip_type_suffix)
        .any(|name| socket_names_match(name, socket))
}

/// Compare a bound socket name from `lsof` to the path we expect.
///
/// Exact match first: the daemon binds the very path it resolved. The fallback
/// re-resolves both parent directories so a home reached through a symlink
/// (`/tmp` -> `/private/tmp` on macOS) still matches; the socket file itself is not
/// canonicalized because an orphan's path may already have been replaced.
fn socket_names_match(name: &str, expected: &Path) -> bool {
    if Path::new(name) == expected {
        return true;
    }
    let real_dir = |p: &Path| p.parent().and_then(|dir| std::fs::canonicalize(dir).ok());
    match (
        real_dir(Path::new(name)),
        real_dir(expected),
        Path::new(name).file_name(),
        expected.file_name(),
    ) {
        (Some(a), Some(b), Some(x), Some(y)) => a == b && x == y,
        _ => false,
    }
}

fn nix_signal(pid: u32, signal: Option<Signal>) -> Result<(), Errno> {
    let raw = i32::try_from(pid).map_err(|_| Errno::EINVAL)?;
    if raw <= 0 {
        return Err(Errno::EINVAL);
    }
    kill(Pid::from_raw(raw), signal)
}

/// Poll `pids` until each is gone or `budget` expires; returns those still alive.
///
/// Bounded by construction: at most `budget / poll` iterations, and the first check
/// happens before any sleep so an already-dead batch costs nothing.
async fn wait_for_exit(
    pids: &[u32],
    budget: Duration,
    poll: Duration,
    signal: Signaller<'_>,
) -> Vec<u32> {
    let deadline = Instant::now() + budget;
    let mut alive = pids.to_vec();
    loop {
        alive.retain(|pid| signal(*pid, None) != Err(Errno::ESRCH));
        if alive.is_empty() || Instant::now() >= deadline {
            return alive;
        }
        sleep(poll).await;
    }
}

/// SIGTERM every PROVEN target, confirm death, and SIGKILL whatever survives the
/// grace.
///
/// `prove` runs twice per target: once before the SIGTERM and again before the
/// escalation, because a target that exits during the grace can have its pid
/// recycled by an unrelated process, which must not inherit our SIGKILL. A target
/// that cannot be proved is left alone and counted in `unproven`.
///
/// The previous version counted a successful SIGTERM as a reap without ever looking
/// again, so a server ignoring SIGTERM was reported reaped on every 60s sweep while
/// it kept running.
async fn terminate_orphans(
    targets: &[u32],
    timing: ReapTiming,
    signal: Signaller<'_>,
    prove: Prover<'_>,
) -> ReapOutcome {
    let mut outcome = ReapOutcome::default();
    let mut signalled = Vec::with_capacity(targets.len());
    for pid in targets {
        let Some(owned) = prove(*pid) else {
            outcome.unproven += 1;
            tracing::debug!(
                pid,
                "sparing a codex app-server we cannot prove belongs to this home"
            );
            continue;
        };
        match deliver(&owned, Signal::SIGTERM, signal) {
            Ok(()) => signalled.push(*pid),
            // Gone between the `ps` read and the signal, not an error.
            Err(Errno::ESRCH) => outcome.reaped += 1,
            Err(error) => {
                outcome.survived += 1;
                tracing::warn!(pid, error = %error, "failed to signal orphaned codex app-server");
            }
        }
    }

    let stubborn = wait_for_exit(&signalled, timing.term_grace, timing.poll, signal).await;
    outcome.reaped += signalled.len() - stubborn.len();

    let mut killed = Vec::with_capacity(stubborn.len());
    for pid in &stubborn {
        // Re-prove: the SIGTERM may have worked and the pid been recycled.
        let Some(owned) = prove(*pid) else {
            outcome.unproven += 1;
            tracing::debug!(
                pid,
                "sparing a SIGTERM survivor we can no longer prove is ours"
            );
            continue;
        };
        match deliver(&owned, Signal::SIGKILL, signal) {
            Ok(()) => killed.push(*pid),
            // Exited during the SIGKILL loop itself: reaped, but not by us escalating.
            Err(Errno::ESRCH) => outcome.reaped += 1,
            Err(error) => {
                outcome.survived += 1;
                tracing::warn!(pid, error = %error, "failed to SIGKILL orphaned codex app-server");
            }
        }
    }

    let unkillable = wait_for_exit(&killed, timing.kill_confirm, timing.poll, signal).await;
    outcome.escalated = killed.len() - unkillable.len();
    outcome.reaped += outcome.escalated;
    outcome.survived += unkillable.len();
    for pid in unkillable {
        tracing::error!(pid, "orphaned codex app-server survived SIGKILL");
    }
    outcome
}

/// Stop obsolete orphaned Hangar daemons that still run the legacy Codex proxy
/// against this home's native WebSocket socket.
///
/// This runs only during boot. Current Hangar never launches the proxy, so a
/// periodic process-table scan would add cost without improving recovery. The
/// parent is revalidated by start fingerprint and direct-child topology before
/// each signal, preventing PID reuse or an argv change from widening authority.
pub async fn reap_legacy_codex_proxy_daemons(live_socket: &Path) -> usize {
    let Some(ps_output) = ps_process_table().await else {
        return 0;
    };
    let mut reaped = 0;
    for (pid, proxy_pid) in legacy_proxy_daemon_pairs(&ps_output, live_socket) {
        let Some(identity) = process_identity(pid).await else {
            continue;
        };
        let Some(proxy_identity) = process_identity(proxy_pid).await else {
            continue;
        };
        let candidate = LegacyProxyDaemon {
            pid,
            process_start_fingerprint: identity.process_start_fingerprint,
            proxy_pid,
            proxy_start_fingerprint: proxy_identity.process_start_fingerprint,
        };
        if !legacy_proxy_daemon_is_current(&candidate, live_socket).await {
            continue;
        }
        match nix_signal(pid, Some(Signal::SIGTERM)) {
            Ok(()) | Err(Errno::ESRCH) => {}
            Err(error) => {
                tracing::warn!(pid, error = %error, "failed to stop legacy codex proxy daemon");
                continue;
            }
        }
        let parent_exited = wait_for_exit(
            &[pid],
            ReapTiming::PRODUCTION.term_grace,
            ReapTiming::PRODUCTION.poll,
            &nix_signal,
        )
        .await
        .is_empty();
        if parent_exited {
            reaped += 1;
        } else {
            if !legacy_proxy_daemon_is_current(&candidate, live_socket).await {
                continue;
            }
            match nix_signal(pid, Some(Signal::SIGKILL)) {
                Ok(()) | Err(Errno::ESRCH) => {}
                Err(error) => {
                    tracing::warn!(pid, error = %error, "failed to kill legacy codex proxy daemon");
                    continue;
                }
            }
            if wait_for_exit(
                &[pid],
                ReapTiming::PRODUCTION.kill_confirm,
                ReapTiming::PRODUCTION.poll,
                &nix_signal,
            )
            .await
            .is_empty()
            {
                reaped += 1;
            } else {
                tracing::error!(pid, "legacy codex proxy daemon survived SIGKILL");
                continue;
            }
        }
        if !legacy_proxy_child_is_current(&candidate, live_socket).await {
            continue;
        }
        match nix_signal(proxy_pid, Some(Signal::SIGTERM)) {
            Ok(()) | Err(Errno::ESRCH) => {}
            Err(error) => {
                tracing::warn!(proxy_pid, error = %error, "failed to stop legacy codex proxy child");
                continue;
            }
        }
        if wait_for_exit(
            &[proxy_pid],
            ReapTiming::PRODUCTION.term_grace,
            ReapTiming::PRODUCTION.poll,
            &nix_signal,
        )
        .await
        .is_empty()
        {
            continue;
        }
        if !legacy_proxy_child_is_current(&candidate, live_socket).await {
            continue;
        }
        if let Err(error) = nix_signal(proxy_pid, Some(Signal::SIGKILL)) {
            if error != Errno::ESRCH {
                tracing::warn!(proxy_pid, error = %error, "failed to kill legacy codex proxy child");
            }
            continue;
        }
        if !wait_for_exit(
            &[proxy_pid],
            ReapTiming::PRODUCTION.kill_confirm,
            ReapTiming::PRODUCTION.poll,
            &nix_signal,
        )
        .await
        .is_empty()
        {
            tracing::error!(proxy_pid, "legacy codex proxy child survived SIGKILL");
        }
    }
    reaped
}

/// Reap codex app-server processes orphaned by a prior daemon or plugin broker.
///
/// A codex app-server does not self-daemonize: its `node .../bin/codex app-server`
/// stays at ppid==1 once its spawner dies. Such a server is spared only while a live
/// proxy consumes its socket AND the Hangar home behind that socket still has a
/// running daemon (see [`adoption_is_credible`]); the holder of our own live socket
/// and this process are spared unconditionally by [`reap_targets`].
///
/// Selection is not authority: every surviving candidate must then PROVE it is ours
/// by holding a socket bound to `live_socket` under our uid ([`OwnedPid`]) before it
/// is signalled at all. That is what confines the sweep to this
/// `$AINB_HANGAR_HOME`'s own leaked servers. A codex belonging to another home is
/// now left running even when its daemon is dead: leaking an orphan is cheap,
/// killing another stack's live app-server is not.
///
/// Each proven target is SIGTERMed, confirmed dead, then SIGKILLed if it outlives
/// the grace period. Best-effort: an unreadable process table reaps nothing.
/// Returns how many processes were confirmed gone.
pub async fn reap_orphaned_codex_servers(live_socket: &Path) -> usize {
    let Some(ps_output) = ps_process_table().await else {
        return 0;
    };
    let candidates = codex_orphans_to_reap(&ps_output, adoption_is_credible);
    if candidates.is_empty() {
        return 0;
    }
    let listeners = socket_listener_pids(live_socket).await;
    let targets = reap_targets(&candidates, listeners.as_deref(), std::process::id());

    let outcome = terminate_orphans(&targets, ReapTiming::PRODUCTION, &nix_signal, &|pid| {
        OwnedPid::holding_socket(pid, live_socket)
    })
    .await;
    if outcome.reaped > 0 || outcome.survived > 0 || outcome.unproven > 0 {
        tracing::info!(
            reaped = outcome.reaped,
            escalated = outcome.escalated,
            survived = outcome.survived,
            unproven = outcome.unproven,
            candidates = candidates.len(),
            "reaped orphaned codex app-server processes"
        );
    }
    outcome.reaped
}

async fn repair_owner_marker(repair: MarkerRepair) -> Result<bool, ProviderError> {
    let mut lock_path = repair.path.as_os_str().to_os_string();
    lock_path.push(".lock");
    let lock_path = PathBuf::from(lock_path);
    let lock = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&lock_path)
        .await;
    let Ok(lock) = lock else {
        return Ok(false);
    };
    let current = tokio::fs::read(&repair.path).await.ok();
    if current != repair.expected {
        drop(lock);
        let _ = tokio::fs::remove_file(&lock_path).await;
        return Ok(false);
    }
    let result = async {
        let mut temporary = repair.path.as_os_str().to_os_string();
        temporary.push(format!(".{}.tmp", std::process::id()));
        let temporary = PathBuf::from(temporary);
        tokio::fs::write(&temporary, serde_json::to_vec(&repair.owner)?).await?;
        tokio::fs::rename(&temporary, &repair.path).await?;
        Ok::<_, ProviderError>(true)
    }
    .await;
    drop(lock);
    let _ = tokio::fs::remove_file(&lock_path).await;
    result
}

fn executable_matches(recorded: &str, expected: &std::ffi::OsStr) -> bool {
    Path::new(recorded).file_name() == Path::new(expected).file_name()
}

fn socket_owner_marker(socket_path: &Path) -> PathBuf {
    let mut marker = socket_path.as_os_str().to_os_string();
    marker.push(".ainb-owner");
    PathBuf::from(marker)
}

async fn process_identity(pid: u32) -> Option<ProcessIdentity> {
    let process_start_fingerprint = ps_field(pid, "lstart=").await?;
    let executable = ps_field(pid, "comm=").await?;
    Some(ProcessIdentity {
        process_start_fingerprint,
        executable,
    })
}

async fn ps_field(pid: u32, field: &str) -> Option<String> {
    let output = Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", field])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .await
        .ok()
        .filter(|output| output.status.success())?;
    let value = String::from_utf8(output.stdout).ok()?.trim().to_string();
    (!value.is_empty()).then_some(value)
}

fn tokio_command(spec: CommandSpec) -> Command {
    let mut command = Command::new(spec.program);
    command.args(spec.args);
    command
}

async fn wait_for_socket(
    child: &mut Child,
    socket: &Path,
    timeout: Duration,
) -> Result<(), ProviderError> {
    let deadline = Instant::now() + timeout;
    loop {
        if socket.exists() {
            return Ok(());
        }
        if let Some(status) = child.try_wait()? {
            return Err(ProviderError::Transport(format!(
                "Codex app-server exited before socket bind: {status}"
            )));
        }
        if Instant::now() >= deadline {
            return Err(ProviderError::Transport(format!(
                "Codex app-server socket startup timed out: {}",
                socket.display()
            )));
        }
        sleep(Duration::from_millis(25)).await;
    }
}

enum ManagerCommand {
    Request {
        method: &'static str,
        params: Value,
        reply: oneshot::Sender<Result<Value, ProviderError>>,
    },
    Respond {
        request_id: RpcRequestId,
        result: Value,
        reply: oneshot::Sender<Result<(), ProviderError>>,
    },
    Shutdown {
        reply: oneshot::Sender<Result<(), ProviderError>>,
    },
}

struct PendingRequest {
    method: &'static str,
    reply: oneshot::Sender<Result<Value, ProviderError>>,
}

type JsonRpcFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, ProviderError>> + Send + 'a>>;

trait JsonRpcTransport: Send {
    fn write_message<'a>(&'a mut self, message: &'a Value) -> JsonRpcFuture<'a, ()>;
    fn read_message<'a>(&'a mut self) -> JsonRpcFuture<'a, Value>;
}

#[cfg(test)]
struct LineTransport<R, W> {
    reader: R,
    writer: W,
}

#[cfg(test)]
impl<R, W> LineTransport<R, W> {
    const fn new(reader: R, writer: W) -> Self {
        Self { reader, writer }
    }
}

#[cfg(test)]
impl<R, W> JsonRpcTransport for LineTransport<R, W>
where
    R: AsyncBufRead + Unpin + Send,
    W: AsyncWrite + Unpin + Send,
{
    fn write_message<'a>(&'a mut self, message: &'a Value) -> JsonRpcFuture<'a, ()> {
        Box::pin(write_message(&mut self.writer, message))
    }

    fn read_message<'a>(&'a mut self) -> JsonRpcFuture<'a, Value> {
        Box::pin(read_message(&mut self.reader))
    }
}

struct WebSocketTransport {
    websocket: WebSocketStream<UnixStream>,
}

impl JsonRpcTransport for WebSocketTransport {
    fn write_message<'a>(&'a mut self, message: &'a Value) -> JsonRpcFuture<'a, ()> {
        Box::pin(async move {
            let text = serde_json::to_string(message)?;
            self.websocket.send(Message::Text(text)).await.map_err(|error| {
                ProviderError::Transport(format!(
                    "Codex app-server WebSocket write failed: {error}"
                ))
            })
        })
    }

    fn read_message<'a>(&'a mut self) -> JsonRpcFuture<'a, Value> {
        Box::pin(async move {
            loop {
                let message = self
                    .websocket
                    .next()
                    .await
                    .ok_or_else(|| {
                        ProviderError::Transport("Codex app-server WebSocket closed".into())
                    })?
                    .map_err(|error| {
                        ProviderError::Transport(format!(
                            "Codex app-server WebSocket read failed: {error}"
                        ))
                    })?;
                match message {
                    Message::Text(text) => {
                        return serde_json::from_str(&text).map_err(ProviderError::from);
                    }
                    Message::Ping(payload) => {
                        self.websocket.send(Message::Pong(payload)).await.map_err(|error| {
                            ProviderError::Transport(format!(
                                "Codex app-server WebSocket pong failed: {error}"
                            ))
                        })?
                    }
                    Message::Pong(_) => {}
                    Message::Close(frame) => {
                        return Err(ProviderError::Transport(format!(
                            "Codex app-server WebSocket closed: {frame:?}"
                        )));
                    }
                    Message::Binary(_) => {
                        return Err(ProviderError::Protocol(
                            "Codex app-server sent a binary WebSocket message".into(),
                        ));
                    }
                    _ => {}
                }
            }
        })
    }
}

async fn spawn_connection<T>(
    mut transport: T,
    capabilities: CodexCapabilities,
    config: CodexManagerConfig,
    owns_server: bool,
    marker_repair: Option<MarkerRepair>,
    mut cleanup: Box<dyn ProcessCleanup>,
) -> Result<ManagedCodexManager, ProviderError>
where
    T: JsonRpcTransport + 'static,
{
    let (events_tx, events) = mpsc::channel(config.event_capacity.max(1));
    let bootstrap = tokio::time::timeout(
        config.startup_timeout,
        initialize_connection(&mut transport, &config.client_version, &events_tx),
    )
    .await
    .map_err(|_| ProviderError::Transport("Codex initialize timed out".into()))
    .and_then(|result| result);
    if let Err(error) = bootstrap {
        cleanup.cleanup().await;
        return Err(error);
    }
    if let Some(repair) = marker_repair {
        if let Err(error) = repair_owner_marker(repair).await {
            tracing::warn!(error = %error, "Codex adopted socket marker repair failed");
        }
    }

    let (commands, command_rx) = mpsc::channel(64);
    let capabilities = Arc::new(capabilities);
    let handle = CodexManagerHandle {
        commands,
        capabilities: Arc::clone(&capabilities),
        socket_path: Arc::new(config.socket_path),
        owns_server,
        request_timeout: config.request_timeout,
    };
    let task = tokio::spawn(async move {
        let result = run_actor(transport, command_rx, events_tx).await;
        cleanup.cleanup().await;
        result
    });
    Ok(ManagedCodexManager {
        handle,
        events,
        task,
    })
}

async fn initialize_connection<T>(
    transport: &mut T,
    client_version: &str,
    events: &mpsc::Sender<CodexInboundEnvelope>,
) -> Result<(), ProviderError>
where
    T: JsonRpcTransport,
{
    transport
        .write_message(&json!({
            "jsonrpc": "2.0",
            "id": INITIALIZE_ID,
            "method": "initialize",
            "params": {
                "clientInfo": {
                    "name": "agents-in-a-box-fleet",
                    "title": "Fleet",
                    "version": client_version,
                },
                "capabilities": { "experimentalApi": true },
            },
        }))
        .await?;

    loop {
        let message = transport.read_message().await?;
        if message.get("id") == Some(&json!(INITIALIZE_ID)) {
            if let Some(error) = message.get("error") {
                return Err(ProviderError::Protocol(format!(
                    "Codex initialize failed: {error}"
                )));
            }
            if message.get("result").is_none() {
                return Err(ProviderError::Protocol(
                    "Codex initialize response has no result".into(),
                ));
            }
            break;
        }
        let inbound = parse_inbound_envelope(&message)?;
        events
            .send(inbound)
            .await
            .map_err(|_| ProviderError::Transport("Codex event receiver closed".into()))?;
    }

    transport
        .write_message(&json!({ "jsonrpc": "2.0", "method": "initialized", "params": {} }))
        .await
}

async fn run_actor<T>(
    mut transport: T,
    mut commands: mpsc::Receiver<ManagerCommand>,
    events: mpsc::Sender<CodexInboundEnvelope>,
) -> Result<(), ProviderError>
where
    T: JsonRpcTransport,
{
    let mut next_id = INITIALIZE_ID + 1;
    let mut pending = BTreeMap::<u64, PendingRequest>::new();

    loop {
        pending.retain(|_, request| !request.reply.is_closed());
        tokio::select! {
            command = commands.recv() => {
                let Some(command) = command else {
                    fail_pending(&mut pending, "Codex manager handles dropped");
                    return Ok(());
                };
                match command {
                    ManagerCommand::Request { method, params, reply } => {
                        let id = next_id;
                        next_id = next_id.checked_add(1).ok_or_else(|| {
                            ProviderError::Protocol("Codex JSON-RPC request id exhausted".into())
                        })?;
                        if let Err(error) = transport.write_message(
                            &json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }),
                        ).await {
                            let _ = reply.send(Err(error));
                            continue;
                        }
                        pending.insert(id, PendingRequest { method, reply });
                    }
                    ManagerCommand::Respond { request_id, result, reply } => {
                        let outcome = transport.write_message(
                            &json!({ "jsonrpc": "2.0", "id": request_id.as_value(), "result": result }),
                        ).await;
                        let _ = reply.send(outcome);
                    }
                    ManagerCommand::Shutdown { reply } => {
                        fail_pending(&mut pending, "Codex manager shutting down");
                        let _ = reply.send(Ok(()));
                        return Ok(());
                    }
                }
            }
            message = transport.read_message() => {
                let message = message?;
                if message.get("result").is_some() || message.get("error").is_some() {
                    if let Some(id) = message.get("id").and_then(Value::as_u64) {
                        let Some(pending_request) = pending.remove(&id) else {
                            return Err(ProviderError::Protocol(format!(
                                "Codex response has unknown request id {id}"
                            )));
                        };
                        let outcome = if let Some(error) = message.get("error") {
                            Err(ProviderError::Protocol(format!(
                                "Codex {} failed: {error}", pending_request.method
                            )))
                        } else {
                            Ok(message.get("result").cloned().unwrap_or(Value::Null))
                        };
                        let _ = pending_request.reply.send(outcome);
                        continue;
                    }
                }
                events
                    .send(parse_inbound_envelope(&message)?)
                    .await
                    .map_err(|_| ProviderError::Transport("Codex event receiver closed".into()))?;
            }
        }
    }
}

fn fail_pending(pending: &mut BTreeMap<u64, PendingRequest>, reason: &str) {
    for (_, request) in std::mem::take(pending) {
        let _ = request.reply.send(Err(ProviderError::Transport(reason.into())));
    }
}

#[cfg(test)]
async fn write_message<W: AsyncWrite + Unpin>(
    writer: &mut W,
    message: &Value,
) -> Result<(), ProviderError> {
    let encoded = serde_json::to_vec(message)?;
    writer.write_all(&encoded).await?;
    writer.write_all(b"\n").await?;
    writer.flush().await?;
    Ok(())
}

#[cfg(test)]
async fn read_message<R: AsyncBufRead + Unpin>(reader: &mut R) -> Result<Value, ProviderError> {
    let mut line = String::new();
    if reader.read_line(&mut line).await? == 0 {
        return Err(ProviderError::Transport(
            "Codex app-server proxy closed stdout".into(),
        ));
    }
    serde_json::from_str(&line).map_err(ProviderError::from)
}

fn nested_id(result: &Value, field: &str) -> Result<String, ProviderError> {
    result
        .get(field)
        .and_then(|value| value.get("id"))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| ProviderError::Protocol(format!("Codex {field} response has no id")))
}

fn validate_answers(
    request: &CodexQuestionRequest,
    answers: &[QuestionAnswer],
) -> Result<(), ProviderError> {
    if request.questions.len() != answers.len() {
        return Err(ProviderError::Protocol(format!(
            "expected {} Codex answers, got {}",
            request.questions.len(),
            answers.len()
        )));
    }
    for question in &request.questions {
        let count = answers
            .iter()
            .filter(|answer| answer.question_id == question.id && !answer.answers.is_empty())
            .count();
        if count != 1 {
            return Err(ProviderError::Protocol(format!(
                "Codex question {} must have one non-empty answer",
                question.id
            )));
        }
    }
    Ok(())
}

fn approval_result(
    request: &CodexApprovalRequest,
    decision: ApprovalDecision,
) -> Result<Value, ProviderError> {
    match request.kind {
        CodexApprovalKind::CommandExecution | CodexApprovalKind::FileChange => {
            let decision = match decision {
                ApprovalDecision::Approve => "accept",
                ApprovalDecision::ApproveForSession => "acceptForSession",
                ApprovalDecision::Deny => "decline",
                ApprovalDecision::DenyAndInterrupt => "cancel",
            };
            Ok(json!({ "decision": decision }))
        }
        CodexApprovalKind::Permissions => {
            let permissions = match decision {
                ApprovalDecision::Approve | ApprovalDecision::ApproveForSession => {
                    request.params.get("permissions").cloned().ok_or_else(|| {
                        ProviderError::Protocol(
                            "Codex permission request has no permissions profile".into(),
                        )
                    })?
                }
                ApprovalDecision::Deny | ApprovalDecision::DenyAndInterrupt => json!({}),
            };
            let scope = if decision == ApprovalDecision::ApproveForSession {
                "session"
            } else {
                "turn"
            };
            Ok(json!({ "permissions": permissions, "scope": scope }))
        }
    }
}

fn manager_closed() -> ProviderError {
    ProviderError::Transport("Codex manager command channel closed".into())
}

type CleanupFuture<'a> = Pin<Box<dyn Future<Output = ()> + Send + 'a>>;

trait ProcessCleanup: Send {
    fn cleanup(&mut self) -> CleanupFuture<'_>;
}

struct ServerCleanup {
    server: Option<Child>,
    owned_socket_path: Option<PathBuf>,
    owner_marker_path: Option<PathBuf>,
}

impl ProcessCleanup for ServerCleanup {
    fn cleanup(&mut self) -> CleanupFuture<'_> {
        Box::pin(async move {
            if let Some(server) = self.server.as_mut() {
                stop_child(server).await;
            }
            remove_owned_socket(
                self.owned_socket_path.as_deref(),
                self.owner_marker_path.as_deref(),
            )
            .await;
        })
    }
}

async fn remove_owned_socket(socket_path: Option<&Path>, owner_marker_path: Option<&Path>) {
    if let Some(socket_path) = socket_path {
        if socket_path.exists() {
            let _ = tokio::fs::remove_file(socket_path).await;
        }
    }
    if let Some(owner_marker_path) = owner_marker_path {
        if owner_marker_path.exists() {
            let _ = tokio::fs::remove_file(owner_marker_path).await;
        }
    }
}

impl Drop for ServerCleanup {
    fn drop(&mut self) {
        if let Some(server) = self.server.as_mut() {
            let _ = server.start_kill();
        }
    }
}

async fn stop_child(child: &mut Child) {
    if child.try_wait().ok().flatten().is_none() {
        let _ = child.start_kill();
    }
    let _ = child.wait().await;
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, duplex, split};
    use tokio::net::UnixListener;
    use tokio_tungstenite::accept_async;

    use super::*;
    use crate::fleet_provider::codex::CodexInbound;
    use crate::fleet_provider::{QuestionOption, StructuredQuestion};

    #[tokio::test]
    async fn scoped_codex_home_symlinks_existing_auth() {
        let root = tempfile::tempdir().unwrap();
        let source_auth = root.path().join("source-auth.json");
        std::fs::write(&source_auth, "existing ChatGPT credentials").unwrap();
        let scoped_home = root.path().join("ainb/codex-home");

        prepare_scoped_codex_home_with_auth(&scoped_home, &source_auth).await.unwrap();

        let scoped_auth = scoped_home.join("auth.json");
        assert!(std::fs::symlink_metadata(&scoped_auth).unwrap().file_type().is_symlink());
        assert_eq!(std::fs::read_link(&scoped_auth).unwrap(), source_auth);
        assert_eq!(
            std::fs::read_to_string(&scoped_auth).unwrap(),
            "existing ChatGPT credentials"
        );
        #[cfg(unix)]
        assert_eq!(
            std::fs::metadata(&scoped_home).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }

    #[test]
    fn bound_by_child_rejects_race_losers() {
        // Sole holder: this child won the bind, so it owns the server.
        assert!(bound_by_child(42, Some(&[42])));
        // Another process won the bind: claiming ownership here strands our child.
        assert!(!bound_by_child(42, Some(&[7])));
        // Several holders, i.e. the duplicate-spawn race: ambiguous, do not claim.
        assert!(!bound_by_child(42, Some(&[7, 42])));
        // Nothing holds the socket: our child did not bind it.
        assert!(!bound_by_child(42, Some(&[])));
        // `lsof` unavailable: trust the spawn rather than kill a healthy server.
        assert!(bound_by_child(42, None));
    }

    #[tokio::test]
    async fn websocket_transport_round_trips_json_over_unix_socket() {
        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("app-server.sock");
        let listener = UnixListener::bind(&socket_path).unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut websocket = accept_async(stream).await.unwrap();
            let message = websocket.next().await.unwrap().unwrap();
            let Message::Text(text) = message else {
                panic!("expected JSON text frame");
            };
            assert_eq!(
                serde_json::from_str::<Value>(&text).unwrap()["method"],
                "initialize"
            );
            websocket
                .send(Message::Text(
                    json!({ "jsonrpc": "2.0", "id": 1, "result": {} }).to_string(),
                ))
                .await
                .unwrap();
        });

        let socket = UnixStream::connect(&socket_path).await.unwrap();
        let (websocket, _) = client_async("ws://localhost/", socket).await.unwrap();
        let mut transport = WebSocketTransport { websocket };
        transport
            .write_message(&json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize" }))
            .await
            .unwrap();
        assert_eq!(transport.read_message().await.unwrap()["id"], 1);
        server.await.unwrap();
    }

    #[tokio::test]
    #[ignore = "requires a locally installed Codex app-server"]
    async fn managed_codex_listener_initializes_over_unix_websocket() {
        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("codex-app-server.sock");
        let config = CodexManagerConfig {
            codex_binary: std::env::var_os("AINB_TEST_CODEX_BINARY")
                .unwrap_or_else(|| "codex".into()),
            socket_path: socket_path.clone(),
            client_version: "test".into(),
            startup_timeout: Duration::from_secs(10),
            request_timeout: Duration::from_secs(10),
            event_capacity: 8,
        };

        let manager = spawn(config).await.expect("initialize managed Codex listener");
        assert!(socket_path.exists());
        let thread_id = manager.handle.thread_start(dir.path(), None).await.unwrap();
        manager.handle.thread_resume(&thread_id).await.unwrap();
        manager.handle.shutdown().await.expect("shutdown managed listener");
        manager.wait().await.expect("reap managed listener");
        assert!(!socket_path.exists());
    }

    /// A realistic `ps -Ao pid,ppid,args` dump: header, one adopted ppid==1
    /// server, one unproxied orphan, a live proxy from another Hangar home, and
    /// the desktop Codex/ChatGPT app.
    const PS_FIXTURE: &str = "\
  PID  PPID ARGS
  501     1 /Users/x/.nvm/versions/node/v20.11.0/bin/codex app-server --listen unix:///Users/x/Home A/codex-app-server.sock
  777     1 node /Users/x/.nvm/versions/node/v20.11.0/bin/codex app-server --listen unix:///var/folders/ab/cd/T/cxc-9Q2/broker.sock
 1500  4242 /Users/x/.nvm/versions/node/v20.11.0/bin/codex app-server proxy --sock /Users/x/Home A/codex-app-server.sock
  900   500 /Applications/ChatGPT.app/Contents/Resources/codex --foo bar
";

    /// A pid guaranteed not to be running: spawned, exited, and reaped by us.
    fn dead_pid() -> u32 {
        let mut child = std::process::Command::new("true").spawn().expect("spawn true");
        let pid = child.id();
        child.wait().expect("wait true");
        pid
    }

    /// `ps` dump: one ppid==1 server on `socket`, plus a live proxy consuming it.
    fn proxy_backed_orphan_dump(socket: &Path) -> String {
        format!(
            "  PID  PPID ARGS\n\
              501     1 /Users/x/.nvm/versions/node/v20.11.0/bin/codex app-server --listen unix://{socket}\n\
             1500  4242 /Users/x/.nvm/versions/node/v20.11.0/bin/codex app-server proxy --sock {socket}\n",
            socket = socket.display()
        )
    }

    #[test]
    fn codex_orphans_spares_proxy_backed_server_from_another_home() {
        // Updated for the adopting-home check: the spare now requires CREDIBLE
        // adoption, so this case pins the credible branch explicitly. Before, a live
        // proxy alone was enough, with nothing verifying the home still existed.
        assert_eq!(codex_orphans_to_reap(PS_FIXTURE, |_| true), vec![777]);

        let without_proxy = PS_FIXTURE
            .lines()
            .filter(|line| !line.contains("app-server proxy"))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(
            codex_orphans_to_reap(&without_proxy, |_| true),
            vec![501, 777]
        );
    }

    #[test]
    fn codex_orphans_reaps_proxy_backed_server_whose_home_is_dead() {
        // Same dump, dead adopting home: 501 is abandoned, not adopted, so it joins
        // the unproxied 777 as a reap target.
        assert_eq!(codex_orphans_to_reap(PS_FIXTURE, |_| false), vec![501, 777]);
    }

    #[test]
    fn adoption_is_credible_only_inside_a_hangar_home_with_a_live_pid() {
        let home = tempfile::tempdir().unwrap();
        let socket = home.path().join("codex-app-server.sock");
        let socket_arg = socket.to_str().unwrap();
        let pid_path = crate::pid_path_in(home.path());

        // Not a Hangar home (no `hangar/` dir): somebody else's socket, e.g. a
        // plugin broker's temp dir. Not ours to judge, so the spare stands.
        assert!(adoption_is_credible(socket_arg));
        assert!(adoption_is_credible(
            "/var/folders/ab/cd/T/cxc-9Q2/broker.sock"
        ));

        // A Hangar home whose daemon is gone: the `hangar/` dir survives a SIGKILL,
        // so this IS judged, and every unproven form of pidfile means abandoned.
        std::fs::create_dir_all(pid_path.parent().unwrap()).unwrap();
        assert!(!adoption_is_credible(socket_arg), "no pidfile");
        std::fs::write(&pid_path, "not-a-pid").unwrap();
        assert!(!adoption_is_credible(socket_arg), "unparseable pidfile");
        std::fs::write(&pid_path, dead_pid().to_string()).unwrap();
        assert!(!adoption_is_credible(socket_arg), "dead pid");

        // Live pid recorded (ours): a genuine adopting home.
        std::fs::write(&pid_path, format!("{}\n", std::process::id())).unwrap();
        assert!(adoption_is_credible(socket_arg));
    }

    #[test]
    fn a_foreign_socket_keeps_its_proxy_spare() {
        // The broker row (777) has no proxy, so it is reaped either way; give it one
        // and the spare must hold, because `/var/folders/...` is not a Hangar home.
        let dump = format!(
            "{PS_FIXTURE} 1600  4242 /Users/x/.nvm/versions/node/v20.11.0/bin/codex \
             app-server proxy --sock /var/folders/ab/cd/T/cxc-9Q2/broker.sock\n"
        );
        assert!(
            !codex_orphans_to_reap(&dump, adoption_is_credible).contains(&777),
            "a live third-party session must not be killed for having no daemon.pid"
        );
    }

    #[test]
    fn orphan_with_dead_adopting_daemon_is_reaped_and_live_one_is_spared() {
        let home = tempfile::tempdir().unwrap();
        let socket = home.path().join("codex-app-server.sock");
        let dump = proxy_backed_orphan_dump(&socket);
        let pid_path = crate::pid_path_in(home.path());
        std::fs::create_dir_all(pid_path.parent().unwrap()).unwrap();

        // Adopting daemon dead: the proxy-backed orphan IS selected for reaping.
        std::fs::write(&pid_path, dead_pid().to_string()).unwrap();
        assert_eq!(
            codex_orphans_to_reap(&dump, adoption_is_credible),
            vec![501]
        );

        // Adopting daemon alive: still spared.
        std::fs::write(&pid_path, std::process::id().to_string()).unwrap();
        assert!(codex_orphans_to_reap(&dump, adoption_is_credible).is_empty());
    }

    #[test]
    fn pid_is_running_only_treats_esrch_as_dead() {
        assert!(pid_is_running(std::process::id()));
        assert!(!pid_is_running(dead_pid()));
        // Raw 0 would signal our whole process group, so it is never "a process".
        assert!(!pid_is_running(0));
    }

    #[test]
    fn codex_orphans_ignores_desktop_app_and_non_orphan() {
        // A ppid!=1 line with a matching argv must NOT be returned.
        let dump = " 1500  4242 /home/u/.nvm/bin/codex app-server proxy --sock /t.sock\n";
        assert!(codex_orphans_to_reap(dump, |_| true).is_empty());
        let adopted_proxy = " 1500     1 /home/u/.nvm/bin/codex app-server proxy --sock /t.sock\n";
        assert!(codex_orphans_to_reap(adopted_proxy, |_| true).is_empty());
        // The desktop app has neither `bin/codex` nor ppid==1.
        let desktop = "  900     1 /Applications/ChatGPT.app/Contents/Resources/codex --foo\n";
        assert!(codex_orphans_to_reap(desktop, |_| true).is_empty());
    }

    #[test]
    fn legacy_proxy_recovery_targets_only_its_orphaned_hangar_parent() {
        let home = tempfile::tempdir().unwrap();
        let socket = home.path().join("codex-app-server.sock");
        let other = home.path().join("other.sock");
        let dump = format!(
            "  PID  PPID ARGS\n\
              810     1 /tmp/ainb-hangar-daemon\n\
              811   810 /opt/codex/bin/codex app-server proxy --sock {socket}\n\
              820     1 /tmp/ainb-hangar-daemon\n\
              821   820 /opt/codex/bin/codex app-server proxy --sock {other}\n\
              830     1 /tmp/not-ainb\n\
              831   830 /opt/codex/bin/codex app-server proxy --sock {socket}\n\
              840     1 /opt/codex/bin/codex app-server proxy --sock {socket}\n",
            socket = socket.display(),
            other = other.display(),
        );
        assert_eq!(legacy_proxy_daemon_pairs(&dump, &socket), vec![(810, 811)]);
    }

    #[test]
    fn legacy_proxy_recovery_requires_direct_child_and_exact_socket() {
        let home = tempfile::tempdir().unwrap();
        let socket = home.path().join("codex-app-server.sock");
        let dump = format!(
            "  PID  PPID ARGS\n\
              810     1 /tmp/ainb-hangar-daemon\n\
              811  9999 /opt/codex/bin/codex app-server proxy --sock {socket}\n\
              820     1 /tmp/ainb-hangar-daemon\n\
              821   820 /opt/codex/bin/codex app-server proxy --sock /tmp/foreign.sock\n",
            socket = socket.display(),
        );
        assert!(legacy_proxy_daemon_pairs(&dump, &socket).is_empty());
    }

    /// Quoting the markers is not being them. Every line below contains both
    /// `bin/codex` and `app-server` and was a SIGTERM/SIGKILL target under the old
    /// substring matcher.
    #[test]
    fn codex_orphans_ignores_argv_that_merely_quotes_the_markers() {
        for line in [
            "  4242     1 tail -f /Users/x/logs/bin/codex app-server.log",
            "  4243     1 /bin/sh -c echo /opt/bin/codex app-server",
            "  4244     1 grep -R app-server /opt/homebrew/bin/codex",
            "  4245     1 /usr/bin/vim /Users/x/notes/bin/codex app-server.md",
            // A wrapper that is not codex itself, however codex-shaped its name.
            "  4246     1 /opt/bin/codex-wrapper app-server --listen unix:///tmp/a.sock",
            // `app-server` as a fragment rather than a token.
            "  4247     1 /opt/bin/codex app-server-shim --listen unix:///tmp/a.sock",
        ] {
            assert!(
                codex_orphans_to_reap(line, |_| true).is_empty(),
                "must not target: {line}"
            );
        }

        // The real shapes still match, from both `codex ...` and `node .../codex ...`.
        let real = "  501     1 /Users/x/.nvm/versions/node/v20/bin/codex app-server --listen unix:///t/a.sock\n";
        assert_eq!(codex_orphans_to_reap(real, |_| true), vec![501]);
        let via_node = "  502     1 node /Users/x/.nvm/versions/node/v20/bin/codex app-server --listen unix:///t/b.sock\n";
        assert_eq!(codex_orphans_to_reap(via_node, |_| true), vec![502]);
    }

    /// The same defect against the real kernel: a real detached process whose argv
    /// merely CONTAINS both markers must not appear in a sweep of the real process
    /// table.
    #[tokio::test]
    async fn real_process_quoting_the_codex_argv_is_never_a_reap_target() {
        let marker = format!("/ainb-decoy-{}/bin/codex app-server", std::process::id());
        // `sh -c SCRIPT name args...` puts the markers in argv without ever
        // executing anything called codex.
        let pid = spawn_orphan(
            &format!("sh -c \"while :; do sleep 0.2; done\" {marker}"),
            &marker,
        );
        let _cleanup = KillOnDrop(vec![pid]);

        let ps_output = ps_process_table().await.expect("process table");
        assert!(
            ps_output.contains(&marker),
            "the decoy must be visible in the process table"
        );
        assert!(
            !codex_orphans_to_reap(&ps_output, adoption_is_credible).contains(&pid),
            "a process that merely quotes `bin/codex` + `app-server` must never be reaped"
        );
    }

    /// A real process we own that holds `socket` open: the listener is bound here
    /// and handed to a `sleep` child as its stdin, so `lsof` reports the child
    /// holding that bound name. Killed by its exact pid on drop.
    struct SocketDecoy {
        _listener: std::os::unix::net::UnixListener,
        child: std::process::Child,
    }

    impl SocketDecoy {
        fn holding(socket: &Path) -> Self {
            let listener = std::os::unix::net::UnixListener::bind(socket).expect("bind decoy");
            let handed = listener.try_clone().expect("clone decoy socket");
            let child = std::process::Command::new("/bin/sleep")
                .arg("30")
                .stdin(Stdio::from(std::os::fd::OwnedFd::from(handed)))
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("spawn decoy");
            Self {
                _listener: listener,
                child,
            }
        }

        fn pid(&self) -> u32 {
            self.child.id()
        }
    }

    impl Drop for SocketDecoy {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    /// End-to-end through the real entry point: a real detached ppid==1 process
    /// that is a perfect codex-app-server match by argv, but belongs to another
    /// Hangar home, survives a real sweep untouched.
    ///
    /// This is the live incident in miniature. It is deliberately green-only: the
    /// pre-fix code reached the machine-wide process table with no ownership
    /// precondition, so running this against the unfixed reaper on a developer
    /// machine would SIGKILL real app-servers. The red proof for that defect is at
    /// the unit level, where nothing is signalled for real.
    #[tokio::test]
    async fn real_sweep_spares_a_codex_app_server_from_another_home() {
        use std::os::unix::fs::PermissionsExt as _;

        let theirs = tempfile::tempdir().unwrap();
        let ours = tempfile::tempdir().unwrap();
        let script = theirs.path().join("bin").join("codex");
        std::fs::create_dir_all(script.parent().unwrap()).unwrap();
        std::fs::write(&script, "#!/bin/sh\nwhile :; do sleep 0.2; done\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        // Their socket, their home, no daemon.pid: an abandoned orphan by every
        // measure the selection layer applies, and still not ours to kill.
        let their_socket = theirs.path().join("codex-app-server.sock");
        let marker = format!("app-server --listen unix://{}", their_socket.display());
        let stranger = spawn_orphan(&format!("\"{}\" {marker}", script.display()), &marker);
        let _cleanup = KillOnDrop(vec![stranger]);

        let ps_output = ps_process_table().await.expect("process table");
        assert!(
            codex_orphans_to_reap(&ps_output, adoption_is_credible).contains(&stranger),
            "the fixture must be selected by the pure layer, or it proves nothing"
        );

        let reaped = reap_orphaned_codex_servers(&ours.path().join("codex-app-server.sock")).await;
        assert_eq!(reaped, 0, "a foreign home's app-server must not be reaped");
        assert!(
            pid_is_running(stranger),
            "pid {stranger} was killed by a sweep that does not own it"
        );
    }

    /// The ownership proof itself, against real processes and a real `lsof`: only
    /// the holder of THIS home's socket can be signalled.
    #[test]
    fn ownership_proof_accepts_only_this_homes_socket_holder() {
        let mine = tempfile::tempdir().unwrap();
        let theirs = tempfile::tempdir().unwrap();
        let my_socket = mine.path().join("codex-app-server.sock");
        let their_socket = theirs.path().join("codex-app-server.sock");
        let ours = SocketDecoy::holding(&my_socket);
        let stranger = SocketDecoy::holding(&their_socket);

        assert!(
            OwnedPid::holding_socket(ours.pid(), &my_socket).is_some(),
            "the holder of this home's socket is ours to signal"
        );
        assert!(
            OwnedPid::holding_socket(stranger.pid(), &my_socket).is_none(),
            "a codex serving another home must never be signalled"
        );
        // A live process holding no socket at all (pid 1) proves nothing either.
        assert!(OwnedPid::holding_socket(1, &my_socket).is_none());
        assert!(OwnedPid::holding_socket(dead_pid(), &my_socket).is_none());
    }

    /// Scriptable stand-in for a process table: who is alive, and which signals
    /// each pid ignores.
    struct FakeProcesses {
        alive: std::sync::Mutex<std::collections::HashSet<u32>>,
        ignores_term: std::collections::HashSet<u32>,
        ignores_kill: std::collections::HashSet<u32>,
        delivered: std::sync::Mutex<Vec<(u32, Option<Signal>)>>,
    }

    impl FakeProcesses {
        fn new(alive: &[u32]) -> Self {
            Self {
                alive: std::sync::Mutex::new(alive.iter().copied().collect()),
                ignores_term: std::collections::HashSet::new(),
                ignores_kill: std::collections::HashSet::new(),
                delivered: std::sync::Mutex::new(Vec::new()),
            }
        }

        fn signal(&self, pid: u32, signal: Option<Signal>) -> Result<(), Errno> {
            self.delivered.lock().unwrap().push((pid, signal));
            let mut alive = self.alive.lock().unwrap();
            if !alive.contains(&pid) {
                return Err(Errno::ESRCH);
            }
            match signal {
                None => Ok(()),
                Some(Signal::SIGTERM) if self.ignores_term.contains(&pid) => Ok(()),
                Some(Signal::SIGKILL) if self.ignores_kill.contains(&pid) => Ok(()),
                Some(_) => {
                    alive.remove(&pid);
                    Ok(())
                }
            }
        }

        fn delivered(&self, pid: u32) -> Vec<Option<Signal>> {
            self.delivered
                .lock()
                .unwrap()
                .iter()
                .filter(|(target, _)| *target == pid)
                .map(|(_, signal)| *signal)
                .collect()
        }
    }

    /// Ownership stand-in for the escalation tests, which are about SIGTERM →
    /// SIGKILL mechanics against synthetic pids that hold no socket. The proof
    /// itself is exercised against real processes in
    /// [`ownership_proof_accepts_only_this_homes_socket_holder`].
    fn proven(pid: u32) -> Option<OwnedPid> {
        Some(OwnedPid(pid))
    }

    /// Same shape as `ReapTiming::PRODUCTION`, three orders of magnitude faster.
    const TEST_TIMING: ReapTiming = ReapTiming {
        term_grace: Duration::from_millis(15),
        kill_confirm: Duration::from_millis(5),
        poll: Duration::from_millis(1),
    };

    #[tokio::test]
    async fn sigterm_survivor_escalates_to_sigkill() {
        let mut processes = FakeProcesses::new(&[501, 777]);
        // 777 ignores SIGTERM: the case the old code counted as reaped and never
        // looked at again, so it survived every 60s sweep.
        processes.ignores_term.insert(777);

        let outcome = terminate_orphans(
            &[501, 777, 999],
            TEST_TIMING,
            &|pid, signal| processes.signal(pid, signal),
            &proven,
        )
        .await;

        assert_eq!(
            outcome,
            ReapOutcome {
                reaped: 3,
                escalated: 1,
                survived: 0,
                unproven: 0
            }
        );
        // The compliant orphan is SIGTERMed, probed once, and never escalated.
        assert_eq!(processes.delivered(501), vec![Some(Signal::SIGTERM), None]);
        assert!(!processes.delivered(501).contains(&Some(Signal::SIGKILL)));
        // The stubborn one is SIGTERMed, probed, then SIGKILLed, then confirmed.
        let stubborn = processes.delivered(777);
        assert_eq!(stubborn.first(), Some(&Some(Signal::SIGTERM)));
        assert!(stubborn.contains(&Some(Signal::SIGKILL)));
        // 999 was already gone before the first signal: reaped, never escalated.
        assert_eq!(processes.delivered(999), vec![Some(Signal::SIGTERM)]);
        assert!(processes.alive.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn unkillable_orphan_is_reported_survived_not_reaped() {
        let mut processes = FakeProcesses::new(&[501]);
        processes.ignores_term.insert(501);
        processes.ignores_kill.insert(501);

        let outcome = terminate_orphans(
            &[501],
            TEST_TIMING,
            &|pid, signal| processes.signal(pid, signal),
            &proven,
        )
        .await;

        assert_eq!(
            outcome,
            ReapOutcome {
                reaped: 0,
                escalated: 0,
                survived: 1,
                unproven: 0
            }
        );
    }

    /// An unproven target is never signalled at all — not even the SIGTERM.
    #[tokio::test]
    async fn an_unproven_target_is_spared_entirely() {
        let processes = FakeProcesses::new(&[501]);

        let outcome = terminate_orphans(
            &[501],
            TEST_TIMING,
            &|pid, signal| processes.signal(pid, signal),
            &|_| None,
        )
        .await;

        assert_eq!(
            outcome,
            ReapOutcome {
                reaped: 0,
                escalated: 0,
                survived: 0,
                unproven: 1
            }
        );
        assert!(
            processes.delivered(501).is_empty(),
            "an unproven pid must not even be probed for liveness by the reaper"
        );
        assert!(processes.alive.lock().unwrap().contains(&501));
    }

    /// SIGKILLs whatever the real-process test spawned, however the test ends.
    struct KillOnDrop(Vec<u32>);

    impl Drop for KillOnDrop {
        fn drop(&mut self) {
            for pid in &self.0 {
                let _ = nix_signal(*pid, Some(Signal::SIGKILL));
            }
        }
    }

    fn ps_dump_blocking() -> String {
        let output = std::process::Command::new("ps")
            .args(["-Ao", "pid,ppid,args"])
            .output()
            .expect("ps -Ao pid,ppid,args");
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    /// Start `command_line` detached so it reparents to init, and return its pid.
    ///
    /// The intermediate `sh` backgrounds the job and exits, which is exactly the
    /// ppid==1 shape a SIGKILLed daemon leaves its app-server in. `marker` is the
    /// unique argv fragment used to find the grandchild in the process table.
    fn spawn_orphan(command_line: &str, marker: &str) -> u32 {
        let mut shell = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("{command_line} >/dev/null 2>&1 &"))
            .spawn()
            .expect("spawn sh");
        shell.wait().expect("wait sh");
        for _ in 0..100 {
            let found = ps_dump_blocking()
                .lines()
                .filter(|line| line.contains(marker))
                .find_map(|line| parse_ps_row(line).map(|row| row.pid));
            if let Some(pid) = found {
                return pid;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("orphan matching {marker} never appeared in the process table");
    }

    /// End-to-end against the real kernel: a real ppid==1 fake app-server, backed by
    /// a real live proxy, whose Hangar home is dead (the shape the old sparing rule
    /// protected forever) is selected and actually killed, SIGKILL included.
    ///
    /// The fixtures are planted in the machine-wide process table, so a Hangar daemon
    /// running on the same host can select and kill them on its own sweep. The
    /// assertions therefore pin what must be true either way (gone, and never
    /// reported survived) rather than which signal did it.
    #[tokio::test]
    async fn real_proxy_backed_orphan_with_dead_home_is_detected_and_killed() {
        use std::os::unix::fs::PermissionsExt as _;

        let home = tempfile::tempdir().unwrap();
        let script = home.path().join("bin").join("codex");
        std::fs::create_dir_all(script.parent().unwrap()).unwrap();
        // Ignores SIGTERM, so only the escalation can end it.
        std::fs::write(
            &script,
            "#!/bin/sh\ntrap '' TERM\nwhile :; do sleep 0.2; done\n",
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let socket = home.path().join("codex-app-server.sock");
        let pid_path = crate::pid_path_in(home.path());
        std::fs::create_dir_all(pid_path.parent().unwrap()).unwrap();
        std::fs::write(&pid_path, dead_pid().to_string()).unwrap();

        let server_marker = format!("app-server --listen unix://{}", socket.display());
        let proxy_marker = format!("app-server proxy --sock {}", socket.display());
        let server = spawn_orphan(
            &format!("\"{}\" {server_marker}", script.display()),
            &server_marker,
        );
        let proxy = spawn_orphan(
            &format!("\"{}\" {proxy_marker}", script.display()),
            &proxy_marker,
        );
        let _cleanup = KillOnDrop(vec![server, proxy]);

        let ps_output = ps_process_table().await.expect("process table");
        let dead_home = codex_orphans_to_reap(&ps_output, adoption_is_credible);
        assert!(
            dead_home.contains(&server),
            "proxy-backed orphan with a dead home must be reaped, got {dead_home:?}"
        );
        assert!(
            !dead_home.contains(&proxy),
            "a proxy is a consumer, never a reap target"
        );

        // Same real processes, same real `ps` dump, live home: spared.
        std::fs::write(&pid_path, std::process::id().to_string()).unwrap();
        assert!(!codex_orphans_to_reap(&ps_output, adoption_is_credible).contains(&server));

        let timing = ReapTiming {
            term_grace: Duration::from_millis(300),
            kill_confirm: Duration::from_millis(1_000),
            poll: Duration::from_millis(25),
        };
        // `proven` stands in for the ownership proof: this fixture is a shell
        // script, so it holds no `codex-app-server.sock` to prove ownership with.
        // What is under test here is the SIGTERM → SIGKILL escalation against a
        // real TERM-trapping process; the proof has its own real-process test.
        let outcome = terminate_orphans(&[server], timing, &nix_signal, &proven).await;
        assert_eq!(outcome.reaped, 1, "{outcome:?}");
        assert_eq!(
            outcome.survived, 0,
            "a TERM-trapping orphan must not survive"
        );
        assert!(!pid_is_running(server), "pid {server} is still running");
    }

    /// Serialises every test that mutates the process-global `TRANSPORT_HEALTH`.
    static HEALTH_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[tokio::test]
    async fn repeated_service_failures_escalate_to_degraded() {
        assert!(!service_failures_are_degraded(
            CODEX_SERVICE_DEGRADED_AFTER - 1
        ));
        assert!(service_failures_are_degraded(CODEX_SERVICE_DEGRADED_AFTER));

        let _guard = HEALTH_LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
        clear_service_failures().await;
        // `attempt` is the pre-increment counter, so attempt 0 is failure 1.
        let early = note_service_failure(0, "Codex initialize timed out").await;
        assert_eq!(early.consecutive_failures, 1);
        assert!(!early.degraded);

        let degraded = note_service_failure(
            CODEX_SERVICE_DEGRADED_AFTER - 1,
            "Codex initialize timed out",
        )
        .await;
        assert!(degraded.degraded);
        assert_eq!(degraded.consecutive_failures, CODEX_SERVICE_DEGRADED_AFTER);
        assert_eq!(transport_health().await, degraded);

        // A healthy transport clears the streak.
        clear_service_failures().await;
        assert_eq!(transport_health().await, CodexTransportHealth::default());
    }

    #[test]
    fn only_a_transport_that_served_clears_the_retry_streak() {
        // A spawn that succeeds and dies immediately is a flap, not a recovery.
        // Clearing the streak on spawn success alone pinned `consecutive_failures`
        // at 1 forever AND reset the backoff to 1s, respawning an app-server every
        // second without ever escalating.
        assert!(!transport_cycle_was_healthy(Duration::from_millis(80)));
        assert!(!transport_cycle_was_healthy(service_backoff(4)));
        assert!(!transport_cycle_was_healthy(
            MIN_HEALTHY_UPTIME - Duration::from_secs(1)
        ));
        assert!(transport_cycle_was_healthy(MIN_HEALTHY_UPTIME));
        assert!(transport_cycle_was_healthy(Duration::from_secs(3_600)));
    }

    #[test]
    fn reap_targets_spares_live_socket_holder_and_self() {
        let candidates = [501, 777];
        // The live server we may adopt is spared.
        assert_eq!(reap_targets(&candidates, Some(&[501]), 999), vec![777]);
        // Empty listener set: nobody holds the socket, reap every orphan.
        assert_eq!(reap_targets(&candidates, Some(&[]), 999), vec![501, 777]);
        // `lsof` could not answer: ppid==1 alone proves orphan-hood, reap all.
        assert_eq!(reap_targets(&candidates, None, 999), vec![501, 777]);
        // Our own pid is never a target.
        assert_eq!(reap_targets(&[501, 777, 999], None, 999), vec![501, 777]);
    }

    struct FakeCleanup(Arc<AtomicBool>);

    impl ProcessCleanup for FakeCleanup {
        fn cleanup(&mut self) -> CleanupFuture<'_> {
            Box::pin(async move {
                self.0.store(true, Ordering::SeqCst);
            })
        }
    }

    fn capabilities(rui: bool) -> CodexCapabilities {
        CodexCapabilities {
            cli_version: "codex-cli test".into(),
            daemon_version: None,
            app_server: true,
            stdio_proxy: true,
            request_user_input: rui,
            approvals: true,
            thread_archive: true,
        }
    }

    fn config() -> CodexManagerConfig {
        CodexManagerConfig {
            codex_binary: "codex".into(),
            socket_path: "/tmp/test-codex-manager.sock".into(),
            client_version: "test".into(),
            startup_timeout: Duration::from_millis(50),
            request_timeout: Duration::from_secs(1),
            event_capacity: 8,
        }
    }

    async fn server_read<R: AsyncBufRead + Unpin>(reader: &mut R) -> Value {
        let mut line = String::new();
        reader.read_line(&mut line).await.expect("server read");
        serde_json::from_str(&line).expect("server JSON")
    }

    async fn server_write<W: AsyncWrite + Unpin>(writer: &mut W, value: Value) {
        writer.write_all(format!("{value}\n").as_bytes()).await.expect("server write");
        writer.flush().await.expect("server flush");
    }

    #[tokio::test]
    async fn manager_initializes_orders_events_and_routes_exact_commands() {
        let (manager_io, server_io) = duplex(65_536);
        let (manager_read, manager_write) = split(manager_io);
        let (server_read_half, mut server_write_half) = split(server_io);
        let mut server_read_half = BufReader::new(server_read_half);
        let cleanup_flag = Arc::new(AtomicBool::new(false));
        let repair_dir = tempfile::tempdir().unwrap();
        let repair_path = repair_dir.path().join("adopted.ainb-owner");
        let repaired_owner = SocketOwnerMarker {
            schema: 1,
            pid: 42,
            process_start_fingerprint: "start".to_string(),
            executable: "codex".to_string(),
        };

        let server = tokio::spawn(async move {
            let initialize = server_read(&mut server_read_half).await;
            assert_eq!(initialize["method"], "initialize");
            assert_eq!(
                initialize["params"]["capabilities"]["experimentalApi"],
                true
            );
            server_write(
                &mut server_write_half,
                json!({ "jsonrpc": "2.0", "id": INITIALIZE_ID, "result": {} }),
            )
            .await;
            let initialized = server_read(&mut server_read_half).await;
            assert_eq!(initialized["method"], "initialized");

            server_write(
                &mut server_write_half,
                json!({
                    "jsonrpc": "2.0",
                    "id": "rui-7",
                    "method": "item/tool/requestUserInput",
                    "params": {
                        "threadId": "thread-1",
                        "turnId": "turn-2",
                        "itemId": "item-3",
                        "questions": [{
                            "id": "tool",
                            "header": "Tool",
                            "question": "Pick tool",
                            "options": [{ "label": "rg", "description": "Text" }]
                        }]
                    }
                }),
            )
            .await;

            let answer = server_read(&mut server_read_half).await;
            assert_eq!(answer["id"], "rui-7");
            assert_eq!(answer["result"]["answers"]["tool"]["answers"][0], "rg");

            let turn_start = server_read(&mut server_read_half).await;
            assert_eq!(turn_start["method"], "turn/start");
            assert_eq!(turn_start["params"]["threadId"], "thread-1");
            server_write(
                &mut server_write_half,
                json!({
                    "jsonrpc": "2.0",
                    "id": turn_start["id"],
                    "result": { "turn": { "id": "turn-9" } }
                }),
            )
            .await;

            let interrupt = server_read(&mut server_read_half).await;
            assert_eq!(interrupt["method"], "turn/interrupt");
            assert_eq!(interrupt["params"]["turnId"], "turn-9");
            server_write(
                &mut server_write_half,
                json!({ "jsonrpc": "2.0", "id": interrupt["id"], "result": {} }),
            )
            .await;

            server_write(
                &mut server_write_half,
                json!({
                    "jsonrpc": "2.0",
                    "id": "approval-8",
                    "method": "item/commandExecution/requestApproval",
                    "params": {
                        "threadId": "thread-1",
                        "turnId": "turn-9",
                        "itemId": "item-10"
                    }
                }),
            )
            .await;
            let approval = server_read(&mut server_read_half).await;
            assert_eq!(approval["id"], "approval-8");
            assert_eq!(approval["result"]["decision"], "acceptForSession");
            let archive = server_read(&mut server_read_half).await;
            assert_eq!(archive["method"], "thread/archive");
            assert_eq!(archive["params"]["threadId"], "thread-1");
            server_write(
                &mut server_write_half,
                json!({ "jsonrpc": "2.0", "id": archive["id"], "result": {} }),
            )
            .await;
            let mut eof = String::new();
            assert_eq!(
                server_read_half.read_line(&mut eof).await.expect("server EOF"),
                0
            );
        });

        let mut manager = spawn_connection(
            LineTransport::new(BufReader::new(manager_read), manager_write),
            capabilities(true),
            config(),
            false,
            Some(MarkerRepair {
                path: repair_path.clone(),
                expected: None,
                owner: repaired_owner.clone(),
            }),
            Box::new(FakeCleanup(Arc::clone(&cleanup_flag))),
        )
        .await
        .expect("spawn manager");
        let repaired: SocketOwnerMarker =
            serde_json::from_slice(&std::fs::read(&repair_path).unwrap()).unwrap();
        assert_eq!(repaired, repaired_owner);

        let event = manager.events.recv().await.expect("ordered event");
        assert_eq!(event.raw["method"], "item/tool/requestUserInput");
        let CodexInbound::RequestUserInput(request) = event.inbound else {
            panic!("expected request-user-input");
        };
        assert_eq!(request.identity.thread_id, "thread-1");
        manager
            .handle
            .answer_request_user_input(
                &request,
                &[QuestionAnswer {
                    question_id: "tool".into(),
                    answers: vec!["rg".into()],
                }],
            )
            .await
            .expect("answer RUI");
        let turn_id = manager.handle.turn_start("thread-1", "continue").await.expect("turn start");
        assert_eq!(turn_id, "turn-9");
        manager
            .handle
            .turn_interrupt("thread-1", &turn_id)
            .await
            .expect("turn interrupt");
        let approval = manager.events.recv().await.expect("ordered approval");
        assert_eq!(
            approval.raw["method"],
            "item/commandExecution/requestApproval"
        );
        let CodexInbound::Approval(approval) = approval.inbound else {
            panic!("expected approval");
        };
        assert_eq!(approval.identity.item_id, "item-10");
        manager
            .handle
            .decide_approval(&approval, ApprovalDecision::ApproveForSession)
            .await
            .expect("approval response");
        manager.handle.thread_archive("thread-1").await.expect("thread archive");
        manager.handle.shutdown().await.expect("shutdown");
        manager.wait().await.expect("manager wait");
        server.await.expect("fake server");
        assert!(cleanup_flag.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn capability_gate_refuses_rui_without_writing_response() {
        let (manager_io, server_io) = duplex(4096);
        let (manager_read, manager_write) = split(manager_io);
        let (server_read_half, mut server_write_half) = split(server_io);
        let mut server_read_half = BufReader::new(server_read_half);

        let server = tokio::spawn(async move {
            let initialize = server_read(&mut server_read_half).await;
            server_write(
                &mut server_write_half,
                json!({ "jsonrpc": "2.0", "id": initialize["id"], "result": {} }),
            )
            .await;
            let _ = server_read(&mut server_read_half).await;
            let mut eof = String::new();
            assert_eq!(
                server_read_half.read_line(&mut eof).await.expect("server EOF"),
                0
            );
        });
        let cleanup_flag = Arc::new(AtomicBool::new(false));
        let manager = spawn_connection(
            LineTransport::new(BufReader::new(manager_read), manager_write),
            capabilities(false),
            config(),
            false,
            None,
            Box::new(FakeCleanup(cleanup_flag)),
        )
        .await
        .expect("spawn manager");
        let request = CodexQuestionRequest {
            identity: super::super::codex::CodexItemRequestIdentity {
                request_id: RpcRequestId::new(json!("rui-disabled")).expect("request id"),
                thread_id: "thread".into(),
                turn_id: "turn".into(),
                item_id: "item".into(),
            },
            questions: vec![StructuredQuestion {
                id: "tool".into(),
                header: "Tool".into(),
                question: "Pick".into(),
                options: vec![QuestionOption {
                    label: "rg".into(),
                    description: "Text".into(),
                }],
                multi_select: false,
                is_other: false,
                is_secret: false,
            }],
            auto_resolution_ms: None,
        };
        let error = manager
            .handle
            .answer_request_user_input(
                &request,
                &[QuestionAnswer {
                    question_id: "tool".into(),
                    answers: vec!["rg".into()],
                }],
            )
            .await
            .expect_err("RUI must be gated");
        assert!(matches!(error, ProviderError::Unsupported(_)));
        manager.handle.shutdown().await.expect("shutdown");
        manager.wait().await.expect("wait");
        server.await.expect("fake server");
    }

    #[test]
    fn service_retry_backoff_is_bounded() {
        assert_eq!(service_backoff(0), Duration::from_secs(1));
        assert_eq!(service_backoff(1), Duration::from_secs(2));
        assert_eq!(service_backoff(4), Duration::from_secs(16));
        assert_eq!(service_backoff(99), Duration::from_secs(16));
    }

    #[test]
    fn successful_service_start_resets_retry_backoff() {
        let mut attempt = 99;
        reset_service_attempt(&mut attempt);
        assert_eq!(attempt, 0);
        assert_eq!(service_backoff(attempt), Duration::from_secs(1));
    }

    #[tokio::test]
    async fn adopted_socket_is_never_removed_but_owned_socket_is() {
        let dir = tempfile::tempdir().unwrap();
        let adopted = dir.path().join("adopted.sock");
        std::fs::write(&adopted, b"live").unwrap();
        assert_eq!(
            socket_disposition(adopted.exists(), false, false),
            SocketDisposition::Adopt
        );
        remove_owned_socket(None, None).await;
        assert!(adopted.exists());

        let owned = dir.path().join("owned.sock");
        let marker = socket_owner_marker(&owned);
        std::fs::write(&owned, b"owned").unwrap();
        std::fs::write(&marker, b"owned marker").unwrap();
        assert_eq!(
            socket_disposition(true, true, false),
            SocketDisposition::RecoverOwned
        );
        remove_owned_socket(Some(&owned), Some(&marker)).await;
        assert!(!owned.exists());
        assert!(!marker.exists());
    }

    #[test]
    fn socket_disposition_never_claims_unknown_or_live_owner() {
        assert_eq!(
            socket_disposition(false, false, false),
            SocketDisposition::Create
        );
        assert_eq!(
            socket_disposition(true, false, false),
            SocketDisposition::Adopt
        );
        assert_eq!(
            socket_disposition(true, true, true),
            SocketDisposition::Adopt
        );
    }

    #[tokio::test]
    async fn stale_owned_socket_recovers_but_unknown_socket_is_adopted() {
        let dir = tempfile::tempdir().unwrap();
        let owned = dir.path().join("owned.sock");
        let marker = socket_owner_marker(&owned);
        let listener = UnixListener::bind(&owned).unwrap();
        drop(listener);
        let stale = SocketOwnerMarker {
            schema: 1,
            pid: u32::MAX,
            process_start_fingerprint: "stale-start".to_string(),
            executable: "codex".to_string(),
        };
        std::fs::write(&marker, serde_json::to_vec(&stale).unwrap()).unwrap();
        assert!(prepare_socket(&owned, std::ffi::OsStr::new("codex")).await.unwrap().owns_server);
        assert!(!owned.exists());
        assert!(!marker.exists());

        let unknown = dir.path().join("unknown.sock");
        let listener = UnixListener::bind(&unknown).unwrap();
        std::fs::write(socket_owner_marker(&unknown), b"invalid marker").unwrap();
        assert!(
            !prepare_socket(&unknown, std::ffi::OsStr::new("codex"))
                .await
                .unwrap()
                .owns_server
        );
        assert!(unknown.exists());
        drop(listener);
    }

    #[tokio::test]
    async fn stale_owner_marker_never_unlinks_live_replacement_listener() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("replacement.sock");
        let marker = socket_owner_marker(&socket);
        let listener = UnixListener::bind(&socket).unwrap();
        let stale = SocketOwnerMarker {
            schema: 1,
            pid: u32::MAX,
            process_start_fingerprint: "dead-owner".to_string(),
            executable: "codex".to_string(),
        };
        std::fs::write(&marker, serde_json::to_vec(&stale).unwrap()).unwrap();

        let preparation = prepare_socket(&socket, std::ffi::OsStr::new("codex")).await.unwrap();
        assert!(!preparation.owns_server);
        assert!(socket.exists());
        assert!(marker.exists());

        let client = UnixStream::connect(&socket).await.unwrap();
        let (_accepted, _) = listener.accept().await.unwrap();
        drop(client);
    }

    #[tokio::test]
    async fn exact_live_owner_identity_is_adopted() {
        let identity = process_identity(std::process::id()).await.unwrap();
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("live.sock");
        std::fs::write(&socket, b"live").unwrap();
        let marker = SocketOwnerMarker {
            schema: 1,
            pid: std::process::id(),
            process_start_fingerprint: identity.process_start_fingerprint,
            executable: identity.executable.clone(),
        };
        std::fs::write(
            socket_owner_marker(&socket),
            serde_json::to_vec(&marker).unwrap(),
        )
        .unwrap();
        assert!(
            !prepare_socket(&socket, std::ffi::OsStr::new(&identity.executable))
                .await
                .unwrap()
                .owns_server
        );
        assert!(socket.exists());
    }

    #[tokio::test]
    async fn stale_unmarked_socket_inode_is_recovered_once() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("stale-unmarked.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        drop(listener);
        assert!(socket.exists());
        let preparation = prepare_socket(&socket, std::ffi::OsStr::new("codex")).await.unwrap();
        assert!(preparation.owns_server);
        assert!(!socket.exists());
    }

    #[tokio::test]
    async fn atomic_marker_repair_requires_unchanged_original() {
        let dir = tempfile::tempdir().unwrap();
        let marker_path = dir.path().join("socket.ainb-owner");
        let original = b"corrupt".to_vec();
        std::fs::write(&marker_path, &original).unwrap();
        let owner = SocketOwnerMarker {
            schema: 1,
            pid: 42,
            process_start_fingerprint: "start".to_string(),
            executable: "codex".to_string(),
        };
        assert!(
            repair_owner_marker(MarkerRepair {
                path: marker_path.clone(),
                expected: Some(original),
                owner: owner.clone(),
            })
            .await
            .unwrap()
        );
        let repaired: SocketOwnerMarker =
            serde_json::from_slice(&std::fs::read(&marker_path).unwrap()).unwrap();
        assert_eq!(repaired, owner);

        assert!(
            !repair_owner_marker(MarkerRepair {
                path: marker_path,
                expected: Some(b"old".to_vec()),
                owner,
            })
            .await
            .unwrap()
        );
    }

    #[test]
    fn interactive_yolo_policy_is_sent_to_the_app_server() {
        let params = interactive_thread_start_params(Path::new("/worktree"), Some("gpt-5"), true);
        assert_eq!(params["approvalPolicy"], "never");
        assert_eq!(params["sandbox"], "danger-full-access");
        assert_eq!(params["model"], "gpt-5");
        assert_eq!(params["ephemeral"], false);
        assert_eq!(params["threadSource"], "user");

        let default_params = interactive_thread_start_params(Path::new("/worktree"), None, false);
        assert!(default_params.get("approvalPolicy").is_none());
        assert!(default_params.get("sandbox").is_none());
        assert_eq!(default_params["ephemeral"], false);
        assert_eq!(default_params["threadSource"], "user");
    }
}
