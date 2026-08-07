//! The adapter process and the protocol conversation with it.
//!
//! Everything on the wire comes from the UPSTREAM `agent-client-protocol`
//! crate, pinned to an exact 1.x. Nothing here re-implements a schema type or a
//! framing rule.
//!
//! Four invariants live in this file, each of them a bug the spike actually
//! observed:
//!
//! * **Handler before load.** The `session/update` handler is registered on the
//!   builder, which is consumed BEFORE the connection exists, so no request can
//!   possibly be issued first. `session/load` replays history as notifications
//!   AHEAD of its own reply; a handler registered after the call would silently
//!   drop the whole conversation. The plan calls this the single most likely
//!   implementation bug in the port, so it is closed by construction rather
//!   than by ordering discipline.
//! * **Mode pinning (I13).** The spike saw `claude-agent-acp` report
//!   `currentModeId: bypassPermissions` inherited from ambient state, and as a
//!   direct consequence zero agent-to-client requests fired across every probe.
//!   Every session therefore asserts its configured mode and a failure to prove
//!   it is a hard spawn failure, never a warning. The assertion does not stop
//!   at spawn: every later `current_mode_update` is recorded, and
//!   [`AdapterProcess::mode_violated`] is the standing check the pool consults
//!   before it prompts.
//! * **Allowlisted child environment (I13).** `env_clear` then an explicit
//!   list. The daemon's environment is not the adapter's business, and ambient
//!   inheritance is how the mode leak happened in the first place.
//! * **Every reply inspected.** `-32602` is surfaced as
//!   [`AcpError::InvalidParams`], never swallowed. The spike lost an entire
//!   probe run to `session/set_config_option` taking `configId` rather than
//!   `optionId`: the wrong name errored, the error was ignored, and the run
//!   silently continued on the default model.
//!
//! ## Drift from the plan (upstream API)
//!
//! The plan says `session/new` "ALWAYS carries an explicit mode". The pinned
//! upstream `NewSessionRequest` has no mode field (v1 has none), so the mode is
//! asserted instead: read `currentModeId` from the reply, and if it is not the
//! configured mode issue `session/set_session_mode` and require the adapter to
//! ECHO the new mode back as a `current_mode_update` notification within
//! [`MODE_ASSERT_TIMEOUT`]. No echo means the mode is unproven, which fails the
//! spawn exactly like a mismatch: an unverifiable permission mode is the very
//! state the spike proved is dangerous.

use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::schema::v1::{
    CancelNotification, CloseSessionRequest, ContentBlock, InitializeRequest, LoadSessionRequest,
    NewSessionRequest, PromptRequest, PromptResponse, RequestPermissionOutcome,
    RequestPermissionRequest, RequestPermissionResponse, SelectedPermissionOutcome,
    SessionModeState, SessionNotification, SessionUpdate, SetSessionConfigOptionRequest,
    SetSessionModeRequest, TextContent,
};
use agent_client_protocol::{Agent, ByteStreams, Client, ConnectionTo, JsonRpcRequest, Responder};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, oneshot};
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use crate::config::{AdapterConfig, allowlisted_env};

/// How long the adapter has to echo a mode change before the spawn fails.
pub const MODE_ASSERT_TIMEOUT: Duration = Duration::from_secs(5);

/// How long each SPAWN-PHASE request (`initialize`, `session/new`,
/// `session/load`) has to answer before the adapter is declared dead.
///
/// The upstream connection has no request timeout of its own, so an adapter
/// that accepts the pipe and then never replies otherwise parks the pool's
/// spawn path forever, holding the slot against every session queued behind
/// it. 30 s is well past a cold `npx` start on a loaded box and well short of
/// an operator wondering why nothing happened.
///
/// Deliberately NOT applied to `session/prompt`: a turn legitimately runs for
/// minutes, and its deadline is the pool's business, not this file's.
pub const SPAWN_TIMEOUT: Duration = Duration::from_secs(30);

/// JSON-RPC `Invalid params`.
const INVALID_PARAMS: i32 = -32602;

/// `claude-agent-acp`'s typed "Resource not found" for a well-formed but
/// unknown session id (spike gate re-run 2026-08-06; it carries `data.uri`).
const RESOURCE_NOT_FOUND: i32 = -32002;

/// JSON-RPC `Internal error`, which is all `codex-acp` gives an unknown session.
const INTERNAL_ERROR: i32 = -32603;

/// The one and only place this string is matched.
///
/// HAZARD, named deliberately: `codex-acp` (1.1.10, re-measured 2026-08-06) does
/// NOT type its unknown-session failure. It answers an opaque `-32603` whose
/// `data.details` reads `no rollout found for thread id <uuid>`, which is
/// indistinguishable from a genuine internal fault except by this substring. An
/// adapter release that rewords the message silently turns "rebuild the context"
/// into "fail the spawn", so the fallback is deliberately the SAFE direction
/// (spawn failure, retried once, receipt says so) rather than a silent rebuild
/// of every internal error. `claude-agent-acp` needs none of this: its -32002 is
/// typed.
const CODEX_UNKNOWN_SESSION_NEEDLE: &str = "no rollout found";

/// Everything that can go wrong between the daemon and one adapter, typed so
/// the pool can decide fail-fast vs retry without string matching.
#[derive(Debug, thiserror::Error)]
pub enum AcpError {
    /// The adapter binary could not be started.
    #[error("failed to spawn adapter {adapter}: {source}")]
    Spawn {
        /// Adapter token.
        adapter: String,
        /// Underlying spawn failure.
        source: std::io::Error,
    },
    /// The connection died, or never came up.
    #[error("adapter transport failed: {0}")]
    Transport(String),
    /// The adapter answered `initialize` with a version we do not speak.
    #[error("adapter negotiated protocol version {offered}, expected {expected}")]
    ProtocolVersion {
        /// What we pinned.
        expected: u16,
        /// What the adapter answered.
        offered: u16,
    },
    /// The pinned permission mode could not be established (I13). Always a
    /// hard spawn failure.
    #[error("permission mode {requested:?} was not established (observed {observed:?})")]
    ModeMismatch {
        /// The configured mode.
        requested: String,
        /// What the adapter reported, if anything.
        observed: Option<String>,
    },
    /// A request was rejected as `-32602`. Surfaced typed because a swallowed
    /// one silently continues on a default the operator did not choose.
    #[error("adapter rejected {method} params: {message}")]
    InvalidParams {
        /// The method that was rejected.
        method: &'static str,
        /// The adapter's message.
        message: String,
    },
    /// Any other JSON-RPC error.
    #[error("adapter returned {code} for {method}: {message}")]
    Rpc {
        /// The method that failed.
        method: &'static str,
        /// JSON-RPC error code.
        code: i32,
        /// The adapter's message.
        message: String,
    },
    /// `session/load` was asked of an adapter that does not advertise it.
    #[error("adapter {adapter} does not support session/load")]
    LoadUnsupported {
        /// Adapter token.
        adapter: String,
    },
}

impl AcpError {
    /// Does this `session/load` failure mean "the adapter has never heard of
    /// this session", and therefore "rebuild the context", rather than "the
    /// spawn failed"?
    ///
    /// The two adapters answer the SAME condition in two shapes (spike gate
    /// re-run 2026-08-06, measured against `claude-agent-acp` 0.65.0 and
    /// `codex-acp` 1.1.10):
    ///
    /// | Adapter | Unknown session id |
    /// |---|---|
    /// | `claude-agent-acp` | typed `-32002` "Resource not found" with `data.uri` |
    /// | `codex-acp` | opaque `-32603` with `data.details` `no rollout found for thread id <uuid>` |
    ///
    /// [`CODEX_UNKNOWN_SESSION_NEEDLE`] carries the string-match hazard note;
    /// this is the ONE place either shape is decided. Everything else, a closed
    /// transport included, is a spawn failure: a session we cannot prove is
    /// unknown must not be silently rebuilt, because a rebuild throws away the
    /// adapter-side history the load was supposed to recover.
    #[must_use]
    pub fn load_means_rebuild(&self) -> bool {
        match self {
            // The adapter cannot load at all: the rebuild path is the only path.
            Self::LoadUnsupported { .. } => true,
            Self::Rpc { code, message, .. } => {
                *code == RESOURCE_NOT_FOUND
                    || (*code == INTERNAL_ERROR && message.contains(CODEX_UNKNOWN_SESSION_NEEDLE))
            }
            _ => false,
        }
    }
}

/// What `initialize` told us about the adapter.
///
/// The version is persisted to `fleet_acp_session.provider_version`: adapter
/// drift on npm is this port's top risk, and without a recorded version a later
/// resume cannot tell that the adapter changed underneath it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentInfo {
    /// `agentInfo.name`, or the adapter token when the adapter omits it.
    pub name: String,
    /// `agentInfo.version`, or `None` when the adapter omits it.
    pub version: Option<String>,
}

/// One `session/request_permission` the adapter is BLOCKED on.
///
/// The adapter cannot make progress until this is answered, so the responder is
/// carried (not answered inline): the daemon raises an attention row, the human
/// answers through `fleet/action`, and only then does this reply reach the
/// adapter's pending JSON-RPC id. A dropped `PermissionRequest` is answered
/// `Cancelled` by the upstream responder's own cancellation, so an unanswered
/// permission can never wedge the adapter forever.
pub struct PermissionRequest {
    /// The adapter's request, verbatim.
    pub request: RequestPermissionRequest,
    responder: Responder<RequestPermissionResponse>,
}

impl std::fmt::Debug for PermissionRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PermissionRequest")
            .field("session_id", &self.session_id())
            .field("options", &self.option_ids())
            .finish_non_exhaustive()
    }
}

impl PermissionRequest {
    /// The ACP session this permission belongs to (the demux key).
    #[must_use]
    pub fn session_id(&self) -> String {
        self.request.session_id.to_string()
    }

    /// The adapter's pending JSON-RPC request id, as JSON.
    ///
    /// Carried onto the fleet event so an operator can correlate the attention
    /// row with the adapter's own log line.
    #[must_use]
    pub fn rpc_id(&self) -> serde_json::Value {
        self.responder.id()
    }

    /// Every option id the adapter offered, in its order.
    #[must_use]
    pub fn option_ids(&self) -> Vec<String> {
        self.request.options.iter().map(|option| option.option_id.to_string()).collect()
    }

    /// The option ids paired with their human labels, for the attention row.
    #[must_use]
    pub fn options_wire(&self) -> Vec<serde_json::Value> {
        self.request
            .options
            .iter()
            .map(|option| {
                serde_json::json!({
                    "optionId": option.option_id.to_string(),
                    "name": option.name,
                    "kind": option.kind,
                })
            })
            .collect()
    }

    /// Answer with one of the adapter's own option ids.
    ///
    /// An id the adapter never offered is refused HERE rather than sent: the
    /// adapter would answer `-32602` and the turn would fail for a reason no
    /// operator could read back from the receipt.
    pub fn answer_selected(self, option_id: &str) -> Result<(), AcpError> {
        if !self.option_ids().iter().any(|id| id == option_id) {
            return Err(AcpError::InvalidParams {
                method: "session/request_permission",
                message: format!("option {option_id:?} was not offered by the adapter"),
            });
        }
        self.responder
            .respond(RequestPermissionResponse::new(
                RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(
                    option_id.to_string(),
                )),
            ))
            .map_err(|error| AcpError::Transport(error.message))
    }

    /// Answer `Cancelled`: the operator declined, or the session is going away.
    pub fn answer_cancelled(self) -> Result<(), AcpError> {
        self.responder
            .respond(RequestPermissionResponse::new(
                RequestPermissionOutcome::Cancelled,
            ))
            .map_err(|error| AcpError::Transport(error.message))
    }
}

/// One running adapter process, hosting many ACP sessions (graft 6).
///
/// Dropping this kills the child (`kill_on_drop`) and ends the connection task.
pub struct AdapterProcess {
    provider: String,
    permission_mode: String,
    /// [`AdapterConfig::config_options`], the static settings re-applied after
    /// every `session/load` alongside the mode.
    config_options: Vec<(String, String)>,
    connection: ConnectionTo<Agent>,
    info: AgentInfo,
    supports_load: bool,
    observed_modes: Arc<Mutex<HashMap<String, String>>>,
    shutdown: Option<oneshot::Sender<()>>,
    child: Mutex<Child>,
}

impl AdapterProcess {
    /// Spawn `config`'s adapter, connect, and `initialize`.
    ///
    /// `updates` receives EVERY `session/update` for every session multiplexed
    /// on this process; demultiplexing by `session_id` is the pool's job.
    pub async fn spawn(
        config: &AdapterConfig,
        updates: mpsc::UnboundedSender<SessionNotification>,
        permissions: mpsc::UnboundedSender<PermissionRequest>,
    ) -> Result<Self, AcpError> {
        let mut child = spawn_child(config)?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| AcpError::Transport("adapter stdin was not piped".to_string()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| AcpError::Transport("adapter stdout was not piped".to_string()))?;

        let observed_modes: Arc<Mutex<HashMap<String, String>>> = Arc::default();
        let connection = connect(
            stdin,
            stdout,
            updates,
            permissions,
            Arc::clone(&observed_modes),
        );
        let (connection, shutdown) = connection.await?;

        let initialized = send_within_spawn_timeout(
            &connection,
            "initialize",
            InitializeRequest::new(ProtocolVersion::V1),
        )
        .await?;
        if initialized.protocol_version != ProtocolVersion::V1 {
            return Err(AcpError::ProtocolVersion {
                expected: ProtocolVersion::V1.as_u16(),
                offered: initialized.protocol_version.as_u16(),
            });
        }

        Ok(Self {
            provider: config.name.clone(),
            permission_mode: config.permission_mode.clone(),
            config_options: config.config_options.clone(),
            info: AgentInfo {
                name: initialized
                    .agent_info
                    .as_ref()
                    .map_or_else(|| config.name.clone(), |info| info.name.clone()),
                version: initialized.agent_info.as_ref().map(|info| info.version.clone()),
            },
            supports_load: initialized.agent_capabilities.load_session,
            connection,
            observed_modes,
            shutdown: Some(shutdown),
            child: Mutex::new(child),
        })
    }

    /// Whether the adapter's stdout is still open.
    ///
    /// The pool consults this BEFORE it issues a prompt, which is what makes
    /// the I6 "provably never reached the adapter" branch provable: a prompt
    /// refused here was never written, so requeueing it cannot double-deliver.
    /// Once `session/prompt` has been issued the answer is no longer knowable
    /// and the outcome is UNKNOWN, never a blind resend.
    pub fn is_alive(&self) -> bool {
        !self.connection.is_incoming_closed()
    }

    /// Resolve when the adapter's incoming stream closes, i.e. when the process
    /// died or shut down. The pool's convergence trigger.
    pub async fn wait_closed(&self) {
        self.connection.incoming_closed().await;
    }

    /// SIGKILL the child without consuming the process handle, so a teardown
    /// path that only holds an `Arc` can still stop it.
    pub fn kill(&self) {
        if let Ok(mut child) = self.child.lock() {
            let _ = child.start_kill();
        }
    }

    /// The adapter token this process serves.
    pub fn provider(&self) -> &str {
        &self.provider
    }

    /// The `agentInfo` recorded at `initialize`.
    pub const fn info(&self) -> &AgentInfo {
        &self.info
    }

    /// Whether the adapter advertises `session/load`.
    ///
    /// Re-probed on every spawn; deliberately NOT persisted (B-defect 5).
    pub const fn supports_load(&self) -> bool {
        self.supports_load
    }

    /// `session/new` plus the I13 mode assertion and the static config
    /// application. Returns the adapter's id.
    pub async fn new_session(&self, cwd: &Path) -> Result<String, AcpError> {
        let reply =
            send_within_spawn_timeout(&self.connection, "session/new", NewSessionRequest::new(cwd))
                .await?;
        let session_id = reply.session_id.to_string();
        self.apply_static_config(&session_id, reply.modes.as_ref()).await?;
        Ok(session_id)
    }

    /// `session/load` plus the SAME mode assertion and the SAME config
    /// application, in the same step.
    ///
    /// Re-applying is not belt-and-braces. The gate re-run of 2026-08-06 proved
    /// it for BOTH adapters, not just codex: `claude-agent-acp` 0.65.0 also
    /// reverts a pinned mode to ambient after a load, and had simply never been
    /// probed for it. A resume that only re-applied for codex would fail every
    /// claude mode assertion and silently degrade to re-prime, so `session/load`
    /// would never actually be used.
    pub async fn load_session(&self, session_id: &str, cwd: &Path) -> Result<(), AcpError> {
        if !self.supports_load {
            return Err(AcpError::LoadUnsupported {
                adapter: self.provider.clone(),
            });
        }
        let reply = send_within_spawn_timeout(
            &self.connection,
            "session/load",
            LoadSessionRequest::new(session_id.to_string(), cwd),
        )
        .await?;
        self.apply_static_config(session_id, reply.modes.as_ref()).await
    }

    /// Assert the pinned mode, then (re)apply every static config option.
    ///
    /// ONE function so `session/new` and `session/load` cannot drift: the plan's
    /// "the same config was applied at `session/new`, so re-applying it restores
    /// the pre-load state exactly" is only true while both paths read the same
    /// list in the same order.
    async fn apply_static_config(
        &self,
        session_id: &str,
        modes: Option<&SessionModeState>,
    ) -> Result<(), AcpError> {
        self.assert_mode(session_id, modes).await?;
        for (config_id, value) in &self.config_options {
            // The reply IS inspected: the spike lost a whole probe run to
            // `session/set_config_option` taking `configId` rather than
            // `optionId`, erroring, and being ignored, so the run silently
            // continued on a model nobody chose.
            send(
                &self.connection,
                "session/set_config_option",
                SetSessionConfigOptionRequest::new(
                    session_id.to_string(),
                    config_id.clone(),
                    value.as_str(),
                ),
            )
            .await?;
        }
        Ok(())
    }

    /// `session/prompt`. Resolves at TURN END, which is what the delivery leg
    /// waits on.
    pub async fn prompt(&self, session_id: &str, text: &str) -> Result<PromptResponse, AcpError> {
        send(
            &self.connection,
            "session/prompt",
            PromptRequest::new(
                session_id.to_string(),
                vec![ContentBlock::Text(TextContent::new(text.to_string()))],
            ),
        )
        .await
    }

    /// `session/cancel`. A notification, so it never blocks behind the turn it
    /// is cancelling.
    pub fn cancel(&self, session_id: &str) -> Result<(), AcpError> {
        self.connection
            .send_notification(CancelNotification::new(session_id.to_string()))
            .map_err(|error| AcpError::Transport(error.message))
    }

    /// `session/close`. Session-level idle eviction; the process stays warm.
    pub async fn close_session(&self, session_id: &str) -> Result<(), AcpError> {
        send(
            &self.connection,
            "session/close",
            CloseSessionRequest::new(session_id.to_string()),
        )
        .await
        .map(|_| ())
    }

    /// The mode last reported for a session, for the pool's health surface.
    ///
    /// The notification handler keeps this current for the WHOLE life of the
    /// session, not just its spawn: a `current_mode_update` arriving mid
    /// conversation overwrites it.
    pub fn observed_mode(&self, session_id: &str) -> Option<String> {
        self.observed_modes.lock().map_or(None, |modes| modes.get(session_id).cloned())
    }

    /// Whether the session's LAST observed mode differs from the pinned one
    /// (I13, standing guarantee rather than a spawn-time snapshot).
    ///
    /// An adapter that flips a live session to `bypassPermissions` mid
    /// conversation is the spike's exact hazard, just later in the timeline.
    /// The pool reads this before it prompts and fails the turn instead of
    /// driving a session whose permission regime silently changed.
    pub fn mode_violated(&self, session_id: &str) -> bool {
        self.observed_mode(session_id)
            .is_some_and(|observed| observed != self.permission_mode)
    }

    /// Close the connection and reap the child.
    pub async fn shutdown(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        self.kill();
        let waited = {
            let Ok(mut child) = self.child.lock() else {
                return;
            };
            child.try_wait().ok().flatten()
        };
        if waited.is_none() {
            // `kill_on_drop` reaps whatever this misses; a blocking wait here
            // would need the lock held across an await point.
            tokio::task::yield_now().await;
        }
    }

    async fn assert_mode(
        &self,
        session_id: &str,
        modes: Option<&SessionModeState>,
    ) -> Result<(), AcpError> {
        // No mode state at all means the adapter cannot tell us what permission
        // regime the session is in. Unprovable is treated exactly like wrong.
        let Some(modes) = modes else {
            return Err(AcpError::ModeMismatch {
                requested: self.permission_mode.clone(),
                observed: None,
            });
        };
        let current = modes.current_mode_id.to_string();
        self.record_mode(session_id, &current);
        if current == self.permission_mode {
            return Ok(());
        }

        send(
            &self.connection,
            // The upstream method is `session/set_mode`, NOT
            // `session/set_session_mode` (the type is `SetSessionModeRequest`).
            // Getting this label wrong is exactly the class of bug the
            // "every reply is inspected" rule exists to catch: a mis-named
            // method answers -32601 and a client that ignored replies would
            // have carried on with the ambient permission mode.
            "session/set_mode",
            SetSessionModeRequest::new(session_id.to_string(), self.permission_mode.clone()),
        )
        .await?;
        self.await_mode_echo(session_id).await
    }

    /// Wait for the adapter's `current_mode_update` echo. The notification
    /// handler has been live since before `initialize`, so an echo emitted
    /// during `session/set_session_mode` is already recorded by the time this
    /// polls.
    async fn await_mode_echo(&self, session_id: &str) -> Result<(), AcpError> {
        let deadline = tokio::time::Instant::now() + MODE_ASSERT_TIMEOUT;
        loop {
            let observed = self.observed_mode(session_id);
            if observed.as_deref() == Some(self.permission_mode.as_str()) {
                return Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(AcpError::ModeMismatch {
                    requested: self.permission_mode.clone(),
                    observed,
                });
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    fn record_mode(&self, session_id: &str, mode: &str) {
        if let Ok(mut modes) = self.observed_modes.lock() {
            modes.insert(session_id.to_string(), mode.to_string());
        }
    }
}

impl Drop for AdapterProcess {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
    }
}

fn spawn_child(config: &AdapterConfig) -> Result<Child, AcpError> {
    let mut command = Command::new(&config.command);
    command
        .args(&config.args)
        // I13: the child starts from NOTHING and gets exactly the allowlist.
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        // stderr is INHERITED, so it goes wherever the daemon's own fd 2 goes
        // (its console under `just dev`, its redirect under a service manager)
        // NOT into the daemon's tracing log, which nothing here writes to.
        // The reason to inherit rather than pipe is that a pipe nobody drains
        // fills and then deadlocks the adapter mid-turn; piping would mean
        // owning a reader task, which is a bigger commitment than adapter
        // diagnostics have earned.
        .stderr(Stdio::inherit())
        .kill_on_drop(true);
    for (name, value) in allowlisted_env(config, &|name| std::env::var(name).ok()) {
        command.env(name, value);
    }
    command.spawn().map_err(|source| AcpError::Spawn {
        adapter: config.name.clone(),
        source,
    })
}

/// Build the connection with the notification handler ALREADY registered.
async fn connect<I, O>(
    stdin: I,
    stdout: O,
    updates: mpsc::UnboundedSender<SessionNotification>,
    permissions: mpsc::UnboundedSender<PermissionRequest>,
    observed_modes: Arc<Mutex<HashMap<String, String>>>,
) -> Result<(ConnectionTo<Agent>, oneshot::Sender<()>), AcpError>
where
    I: AsyncWrite + Send + Unpin + 'static,
    O: AsyncRead + Send + Unpin + 'static,
{
    let transport = ByteStreams::new(stdin.compat_write(), stdout.compat());
    let builder = Client
        .builder()
        .name("ainb-acp")
        .on_receive_notification(
            async move |notification: SessionNotification, _cx: ConnectionTo<Agent>| {
                if let SessionUpdate::CurrentModeUpdate(update) = &notification.update {
                    if let Ok(mut modes) = observed_modes.lock() {
                        modes.insert(
                            notification.session_id.to_string(),
                            update.current_mode_id.to_string(),
                        );
                    }
                }
                // A closed receiver means the pool is gone; the connection task
                // stops on its own shutdown signal, not on this.
                let _ = updates.send(notification);
                Ok(())
            },
            agent_client_protocol::on_receive_notification!(),
        )
        // R8's inbound leg. The handler PARKS the responder rather than
        // answering: the whole point of the permission surface is that a human
        // decides, and the adapter is blocked meanwhile by protocol design.
        // Auto-answering here (the upstream example's YOLO shape) is exactly
        // the silent-approval hazard the spike found.
        .on_receive_request(
            async move |request: RequestPermissionRequest,
                        responder: Responder<RequestPermissionResponse>,
                        _cx: ConnectionTo<Agent>| {
                if let Err(error) = permissions.send(PermissionRequest { request, responder }) {
                    // Nobody is listening (the pool is gone). Dropping the
                    // parked request answers it `Cancelled` through the
                    // responder's own cancellation, which unblocks the adapter
                    // rather than wedging it.
                    drop(error);
                }
                Ok(())
            },
            agent_client_protocol::on_receive_request!(),
        );

    let (connection_tx, connection_rx) = oneshot::channel();
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    tokio::spawn(async move {
        let result = builder
            .connect_with(transport, async move |cx: ConnectionTo<Agent>| {
                let _ = connection_tx.send(cx);
                let _ = shutdown_rx.await;
                Ok(())
            })
            .await;
        if let Err(error) = result {
            tracing::debug!(?error, "acp adapter connection ended");
        }
    });

    let connection = connection_rx
        .await
        .map_err(|_| AcpError::Transport("adapter connection never started".to_string()))?;
    Ok((connection, shutdown_tx))
}

/// Send one request and INSPECT its reply. The only path to the wire.
async fn send<Req>(
    connection: &ConnectionTo<Agent>,
    method: &'static str,
    request: Req,
) -> Result<Req::Response, AcpError>
where
    Req: JsonRpcRequest,
    Req::Response: Send,
{
    connection
        .send_request(request)
        .block_task()
        .await
        .map_err(|error| classify(method, &error))
}

/// [`send`] under [`SPAWN_TIMEOUT`]. An adapter that never answers is a dead
/// adapter, not a slow one, and the spawn path has no other way to find out.
async fn send_within_spawn_timeout<Req>(
    connection: &ConnectionTo<Agent>,
    method: &'static str,
    request: Req,
) -> Result<Req::Response, AcpError>
where
    Req: JsonRpcRequest,
    Req::Response: Send,
{
    tokio::time::timeout(SPAWN_TIMEOUT, send(connection, method, request))
        .await
        .map_err(|_| {
            AcpError::Transport(format!(
                "{method}: no reply within {}s",
                SPAWN_TIMEOUT.as_secs()
            ))
        })?
}

fn classify(method: &'static str, error: &agent_client_protocol::Error) -> AcpError {
    let code = i32::from(error.code);
    // `data` is folded into the message rather than dropped: it is where an
    // adapter puts the only distinguishing detail it gives (codex's "no rollout
    // found …" for an unknown session, claude's `uri`), so discarding it would
    // make [`AcpError::load_means_rebuild`] unable to tell an unknown session
    // from a genuine internal fault, and would leave the delivery receipt
    // reading "Internal error" with no follow-up.
    let message = match &error.data {
        Some(data) => format!("{}; {data}", error.message),
        None => error.message.clone(),
    };
    if code == INVALID_PARAMS {
        return AcpError::InvalidParams { method, message };
    }
    if agent_client_protocol::is_incoming_transport_closed(error) {
        return AcpError::Transport(format!("{method}: {message}"));
    }
    AcpError::Rpc {
        method,
        code,
        message,
    }
}
