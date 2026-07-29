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

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::process::{Child, Command};
use tokio::sync::RwLock;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::{Instant, sleep};

use super::codex::{
    CodexApprovalKind, CodexApprovalRequest, CodexCapabilities, CodexInboundEnvelope,
    CodexQuestionRequest, CommandSpec, RpcRequestId, app_server_command, managed_tui_command,
    parse_inbound_envelope, probe_codex, proxy_command,
};
use super::{ApprovalDecision, ProviderError, ProviderReceipt, QuestionAnswer};

use crate::events::EventSink;

const INITIALIZE_ID: u64 = 1;

static ACTIVE_MANAGER: OnceLock<RwLock<Option<CodexManagerHandle>>> = OnceLock::new();

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
    request_timeout: Duration,
}

impl CodexManagerHandle {
    /// Negotiated version-probed capabilities.
    pub fn capabilities(&self) -> &CodexCapabilities {
        &self.capabilities
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
        let result = self.request("thread/start", json!({ "cwd": cwd, "model": model })).await?;
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
                    tracing::warn!(attempt = attempt + 1, error = %error, "Codex managed transport unavailable");
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
            reset_service_attempt(&mut attempt);
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
                crate::fleet::replay_unprojected_codex_events(&pool, &events, handle.capabilities()).await
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
            if let Err(error) = task
                .await
                .map_err(|join| {
                    ProviderError::Transport(format!("Codex manager task failed: {join}"))
                })
                .and_then(|result| result)
            {
                tracing::warn!(error = %error, "Codex managed transport stopped");
            }
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

fn epoch_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| {
            i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
        })
}

/// Spawn shared app-server, one proxy, initialize protocol, then start manager.
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
    if !capabilities.stdio_proxy {
        return Err(ProviderError::Unsupported(
            "installed Codex has no app-server stdio proxy".into(),
        ));
    }

    // Fail loud before we add another app-server to an already-piled-up host. A
    // SIGKILLed daemon leaks its child at ppid==1; the boot reaper handles that,
    // but this cap is the second line of defence against an unbounded spawn loop
    // (e.g. a wedged service-retry). Surfaces as a Transport error the service loop
    // logs + backs off on, never a silent 900-process pileup.
    //
    // This runs BEFORE prepare_socket, so at the cap the daemon also declines to
    // ADOPT the one healthy shared server it might otherwise reuse; an intentional,
    // honest downgrade: on an already-saturated host we refuse loudly rather than
    // quietly join the pile.
    enforce_codex_server_cap().await?;

    let preparation = prepare_socket(&config.socket_path, &config.codex_binary).await?;
    let mut owns_server = preparation.owns_server;
    let owner_marker_path = socket_owner_marker(&config.socket_path);
    let mut server = if !owns_server {
        None
    } else {
        let mut server_command = tokio_command(app_server_command(
            &config.codex_binary,
            &config.socket_path,
        ));
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

    let mut proxy_command = tokio_command(proxy_command(&config.codex_binary, &config.socket_path));
    proxy_command.kill_on_drop(true);
    let mut proxy = match proxy_command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(proxy) => proxy,
        Err(error) => {
            if let Some(server) = server.as_mut() {
                stop_child(server).await;
            }
            if owns_server {
                remove_owned_socket(Some(&config.socket_path), Some(&owner_marker_path)).await;
            }
            return Err(error.into());
        }
    };
    let Some(stdin) = proxy.stdin.take() else {
        stop_child(&mut proxy).await;
        if let Some(server) = server.as_mut() {
            stop_child(server).await;
        }
        if owns_server {
            remove_owned_socket(Some(&config.socket_path), Some(&owner_marker_path)).await;
        }
        return Err(ProviderError::Transport(
            "Codex proxy stdin unavailable".into(),
        ));
    };
    let Some(stdout) = proxy.stdout.take() else {
        stop_child(&mut proxy).await;
        if let Some(server) = server.as_mut() {
            stop_child(server).await;
        }
        if owns_server {
            remove_owned_socket(Some(&config.socket_path), Some(&owner_marker_path)).await;
        }
        return Err(ProviderError::Transport(
            "Codex proxy stdout unavailable".into(),
        ));
    };
    let cleanup: Box<dyn ProcessCleanup> = Box::new(ChildCleanup {
        server,
        proxy,
        owned_socket_path: owns_server.then(|| config.socket_path.clone()),
        owner_marker_path: owns_server.then_some(owner_marker_path),
    });

    spawn_connection(
        BufReader::new(stdout),
        stdin,
        capabilities,
        config,
        preparation.marker_repair,
        cleanup,
    )
    .await
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

/// Default max concurrent codex app-server processes before `spawn` refuses more.
const DEFAULT_CODEX_MAX_SERVERS: usize = 8;

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
/// Requires both `bin/codex` and `app-server`. The desktop Codex/ChatGPT app runs
/// `.../ChatGPT.app/Contents/Resources/codex` (no `bin/codex`), so it never matches.
fn is_codex_app_server(args: &str) -> bool {
    args.contains("bin/codex") && args.contains("app-server")
}

fn codex_server_socket(args: &str) -> Option<&str> {
    args.rsplit_once("app-server --listen unix://").map(|(_, socket)| socket.trim())
}

fn codex_proxy_socket(args: &str) -> Option<&str> {
    args.rsplit_once("app-server proxy --sock ").map(|(_, socket)| socket.trim())
}

/// PIDs of orphaned codex app-server processes in a `ps -Ao pid,ppid,args` dump.
///
/// A ppid==1 server is only orphaned when no live proxy consumes its socket.
/// Matching against the complete process snapshot protects adopted servers
/// used by other Hangar homes.
fn codex_orphans_to_reap(ps_output: &str) -> Vec<u32> {
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
            codex_server_socket(row.args).is_none_or(|socket| !live_proxy_sockets.contains(socket))
        })
        .map(|row| row.pid)
        .collect()
}

/// Count of live codex app-server SERVERS (any ppid) in a `ps` dump: argv contains
/// `bin/codex` AND `app-server`, but NOT `app-server proxy`. Used to fail loud
/// before piling on more. Proxy processes are excluded because each real server is
/// fronted by a proxy; counting both would trip the cap at half the real server
/// count (~4 server+proxy pairs against the default cap of 8).
fn live_codex_app_server_count(ps_output: &str) -> usize {
    ps_output
        .lines()
        .filter_map(parse_ps_row)
        .filter(|row| is_codex_app_server(row.args) && !row.args.contains("app-server proxy"))
        .count()
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

/// Configured cap on concurrent codex app-server servers (`AINB_CODEX_MAX_SERVERS`,
/// default `DEFAULT_CODEX_MAX_SERVERS`). A non-numeric override falls back to default.
/// Clamped to a floor of 1 so `AINB_CODEX_MAX_SERVERS=0` cannot permanently refuse
/// every spawn (the daemon must always be able to bring up at least one server).
fn codex_server_cap() -> usize {
    std::env::var("AINB_CODEX_MAX_SERVERS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(DEFAULT_CODEX_MAX_SERVERS)
        .max(1)
}

/// Whether the live codex app-server count in a `ps` dump has reached `cap`.
/// Pure so the boundary (`count >= cap`) is unit-tested without spawning `ps`.
fn codex_server_cap_reached(ps_output: &str, cap: usize) -> bool {
    live_codex_app_server_count(ps_output) >= cap
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

/// Refuse to spawn another shared app-server once the cap is already live.
///
/// Turns a silent process pileup (a SIGKILL/OOM leak loop that never runs Drop)
/// into a loud spawn error the service loop logs and backs off on. Fails open when
/// `ps` cannot answer: we never wedge boot on a host without a readable process
/// table.
async fn enforce_codex_server_cap() -> Result<(), ProviderError> {
    let cap = codex_server_cap();
    let Some(ps_output) = ps_process_table().await else {
        return Ok(());
    };
    if codex_server_cap_reached(&ps_output, cap) {
        let live = live_codex_app_server_count(&ps_output);
        return Err(ProviderError::Transport(format!(
            "refusing to spawn Codex app-server: {live} already live, at or above cap {cap} \
             (raise with AINB_CODEX_MAX_SERVERS)"
        )));
    }
    Ok(())
}

/// Reap codex app-server processes orphaned by a prior daemon or plugin broker.
///
/// A codex app-server does not self-daemonize: its `node .../bin/codex app-server`
/// A server can remain at ppid==1 after another daemon adopts it, so the process
/// snapshot excludes every socket targeted by a live `app-server proxy` before
/// signalling candidates. Best-effort: any failure returns the count reaped so
/// far. Returns how many processes were signalled.
pub async fn reap_orphaned_codex_servers(live_socket: &Path) -> usize {
    use nix::errno::Errno;
    use nix::sys::signal::{Signal, kill};
    use nix::unistd::Pid;

    let Some(ps_output) = ps_process_table().await else {
        return 0;
    };
    let candidates = codex_orphans_to_reap(&ps_output);
    if candidates.is_empty() {
        return 0;
    }
    let listeners = socket_listener_pids(live_socket).await;
    let targets = reap_targets(&candidates, listeners.as_deref(), std::process::id());

    let mut reaped = 0_usize;
    for pid in targets {
        let Ok(raw_pid) = i32::try_from(pid) else {
            continue;
        };
        match kill(Pid::from_raw(raw_pid), Signal::SIGTERM) {
            Ok(()) => {
                reaped += 1;
                tracing::info!(pid, "reaped orphaned codex app-server");
            }
            // Already gone between the `ps` read and the signal, not an error.
            Err(Errno::ESRCH) => {}
            Err(error) => {
                tracing::warn!(pid, error = %error, "failed to signal orphaned codex app-server");
            }
        }
    }
    if reaped > 0 {
        tracing::info!(
            reaped,
            candidates = candidates.len(),
            "reaped orphaned codex app-server processes at boot"
        );
    }
    reaped
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

async fn spawn_connection<R, W>(
    mut reader: R,
    mut writer: W,
    capabilities: CodexCapabilities,
    config: CodexManagerConfig,
    marker_repair: Option<MarkerRepair>,
    mut cleanup: Box<dyn ProcessCleanup>,
) -> Result<ManagedCodexManager, ProviderError>
where
    R: AsyncBufRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let (events_tx, events) = mpsc::channel(config.event_capacity.max(1));
    let bootstrap = tokio::time::timeout(
        config.startup_timeout,
        initialize_connection(&mut reader, &mut writer, &config.client_version, &events_tx),
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
        request_timeout: config.request_timeout,
    };
    let task = tokio::spawn(async move {
        let result = run_actor(reader, writer, command_rx, events_tx).await;
        cleanup.cleanup().await;
        result
    });
    Ok(ManagedCodexManager {
        handle,
        events,
        task,
    })
}

async fn initialize_connection<R, W>(
    reader: &mut R,
    writer: &mut W,
    client_version: &str,
    events: &mpsc::Sender<CodexInboundEnvelope>,
) -> Result<(), ProviderError>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    write_message(
        writer,
        &json!({
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
        }),
    )
    .await?;

    loop {
        let message = read_message(reader).await?;
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

    write_message(
        writer,
        &json!({ "jsonrpc": "2.0", "method": "initialized", "params": {} }),
    )
    .await
}

async fn run_actor<R, W>(
    mut reader: R,
    mut writer: W,
    mut commands: mpsc::Receiver<ManagerCommand>,
    events: mpsc::Sender<CodexInboundEnvelope>,
) -> Result<(), ProviderError>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
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
                        if let Err(error) = write_message(
                            &mut writer,
                            &json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }),
                        ).await {
                            let _ = reply.send(Err(error));
                            continue;
                        }
                        pending.insert(id, PendingRequest { method, reply });
                    }
                    ManagerCommand::Respond { request_id, result, reply } => {
                        let outcome = write_message(
                            &mut writer,
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
            message = read_message(&mut reader) => {
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

struct ChildCleanup {
    server: Option<Child>,
    proxy: Child,
    owned_socket_path: Option<PathBuf>,
    owner_marker_path: Option<PathBuf>,
}

impl ProcessCleanup for ChildCleanup {
    fn cleanup(&mut self) -> CleanupFuture<'_> {
        Box::pin(async move {
            stop_child(&mut self.proxy).await;
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

impl Drop for ChildCleanup {
    fn drop(&mut self) {
        let _ = self.proxy.start_kill();
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

    use super::*;
    use crate::fleet_provider::codex::CodexInbound;
    use crate::fleet_provider::{QuestionOption, StructuredQuestion};

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

    #[test]
    fn codex_orphans_spares_proxy_backed_server_from_another_home() {
        assert_eq!(codex_orphans_to_reap(PS_FIXTURE), vec![777]);

        let without_proxy = PS_FIXTURE
            .lines()
            .filter(|line| !line.contains("app-server proxy"))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(codex_orphans_to_reap(&without_proxy), vec![501, 777]);
    }

    #[test]
    fn codex_orphans_ignores_desktop_app_and_non_orphan() {
        // A ppid!=1 line with a matching argv must NOT be returned.
        let dump = " 1500  4242 /home/u/.nvm/bin/codex app-server proxy --sock /t.sock\n";
        assert!(codex_orphans_to_reap(dump).is_empty());
        let adopted_proxy = " 1500     1 /home/u/.nvm/bin/codex app-server proxy --sock /t.sock\n";
        assert!(codex_orphans_to_reap(adopted_proxy).is_empty());
        // The desktop app has neither `bin/codex` nor ppid==1.
        let desktop = "  900     1 /Applications/ChatGPT.app/Contents/Resources/codex --foo\n";
        assert!(codex_orphans_to_reap(desktop).is_empty());
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

    #[test]
    fn live_count_counts_servers_not_proxies() {
        // 501 + 777 are real servers = 2. The 1500 `app-server proxy` line is a
        // consumer, not a server, so it is NOT counted (counting proxies would trip
        // the cap at half the real server count). Desktop app + header excluded.
        assert_eq!(live_codex_app_server_count(PS_FIXTURE), 2);
        // A lone proxy line, regardless of ppid, counts as zero servers.
        let proxy_only = " 1500     1 /home/u/.nvm/bin/codex app-server proxy --sock /tmp/x.sock\n";
        assert_eq!(live_codex_app_server_count(proxy_only), 0);
    }

    #[test]
    fn cap_reached_at_or_above_boundary() {
        // Fixture has 2 live SERVERS (proxies excluded).
        assert!(!codex_server_cap_reached(PS_FIXTURE, 3)); // below the cap
        assert!(codex_server_cap_reached(PS_FIXTURE, 2)); // at the cap
        assert!(codex_server_cap_reached(PS_FIXTURE, 1)); // above the cap
        assert!(!codex_server_cap_reached(
            PS_FIXTURE,
            DEFAULT_CODEX_MAX_SERVERS
        ));
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
            BufReader::new(manager_read),
            manager_write,
            capabilities(true),
            config(),
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
            BufReader::new(manager_read),
            manager_write,
            capabilities(false),
            config(),
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
}
