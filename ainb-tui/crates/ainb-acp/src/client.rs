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
    NewSessionRequest, PromptRequest, PromptResponse, SessionModeState, SessionNotification,
    SessionUpdate, SetSessionModeRequest, TextContent,
};
use agent_client_protocol::{Agent, ByteStreams, Client, ConnectionTo, JsonRpcRequest};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, oneshot};
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use crate::config::{AdapterConfig, allowlisted_env};

/// How long the adapter has to echo a mode change before the spawn fails.
pub const MODE_ASSERT_TIMEOUT: Duration = Duration::from_secs(5);

/// JSON-RPC `Invalid params`.
const INVALID_PARAMS: i32 = -32602;

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

/// One running adapter process, hosting many ACP sessions (graft 6).
///
/// Dropping this kills the child (`kill_on_drop`) and ends the connection task.
pub struct AdapterProcess {
    provider: String,
    permission_mode: String,
    connection: ConnectionTo<Agent>,
    info: AgentInfo,
    supports_load: bool,
    observed_modes: Arc<Mutex<HashMap<String, String>>>,
    shutdown: Option<oneshot::Sender<()>>,
    child: Child,
}

impl AdapterProcess {
    /// Spawn `config`'s adapter, connect, and `initialize`.
    ///
    /// `updates` receives EVERY `session/update` for every session multiplexed
    /// on this process; demultiplexing by `session_id` is the pool's job.
    pub async fn spawn(
        config: &AdapterConfig,
        updates: mpsc::UnboundedSender<SessionNotification>,
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
        let connection = connect(stdin, stdout, updates, Arc::clone(&observed_modes));
        let (connection, shutdown) = connection.await?;

        let initialized = send(
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
            child,
        })
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

    /// `session/new` plus the I13 mode assertion. Returns the adapter's id.
    pub async fn new_session(&self, cwd: &Path) -> Result<String, AcpError> {
        let reply = send(&self.connection, "session/new", NewSessionRequest::new(cwd)).await?;
        let session_id = reply.session_id.to_string();
        self.assert_mode(&session_id, reply.modes.as_ref()).await?;
        Ok(session_id)
    }

    /// `session/load` plus the SAME mode assertion.
    ///
    /// The re-assertion is not belt-and-braces: the spike proved codex config
    /// does not survive a load, and the permission mode is the one setting
    /// whose silent loss disables R8 entirely.
    pub async fn load_session(&self, session_id: &str, cwd: &Path) -> Result<(), AcpError> {
        if !self.supports_load {
            return Err(AcpError::LoadUnsupported {
                adapter: self.provider.clone(),
            });
        }
        let reply = send(
            &self.connection,
            "session/load",
            LoadSessionRequest::new(session_id.to_string(), cwd),
        )
        .await?;
        self.assert_mode(session_id, reply.modes.as_ref()).await
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
        let _ = self.child.start_kill();
        let _ = self.child.wait().await;
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
        // stderr is left on the daemon's, so adapter diagnostics reach the
        // daemon log instead of a pipe nobody drains (a full stderr pipe
        // deadlocks the adapter).
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
    observed_modes: Arc<Mutex<HashMap<String, String>>>,
) -> Result<(ConnectionTo<Agent>, oneshot::Sender<()>), AcpError>
where
    I: AsyncWrite + Send + Unpin + 'static,
    O: AsyncRead + Send + Unpin + 'static,
{
    let transport = ByteStreams::new(stdin.compat_write(), stdout.compat());
    let builder = Client.builder().name("ainb-acp").on_receive_notification(
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

fn classify(method: &'static str, error: &agent_client_protocol::Error) -> AcpError {
    let code = i32::from(error.code);
    if code == INVALID_PARAMS {
        return AcpError::InvalidParams {
            method,
            message: error.message.clone(),
        };
    }
    if agent_client_protocol::is_incoming_transport_closed(error) {
        return AcpError::Transport(format!("{method}: {}", error.message));
    }
    AcpError::Rpc {
        method,
        code,
        message: error.message.clone(),
    }
}
