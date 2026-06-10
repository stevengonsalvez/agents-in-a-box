//! Per-plugin tokio task.
//!
//! Owns:
//! - the child process handle (stdin/stdout/stderr pipes + `Child`)
//! - the lifecycle FSM (idle → spawning → running → backoff → quarantined)
//! - the request ledger (`HashMap<corr_id, oneshot::Sender<...>>`)
//! - failure history for the quarantine window
//! - per-plugin subscriptions, last-used timestamp, idle-reap deadline
//!
//! One task per plugin. Tasks are addressed by [`crate::types::PluginId`]
//! through the [`Inbox`] mpsc sender held in the runtime's plugin map.

use std::collections::HashMap;
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ainb_plugin_protocol::errors::RpcError;
use ainb_plugin_protocol::methods;
use ainb_plugin_protocol::params::{
    ActionInvokeParams, ActionInvokeResult, CliDispatchParams, CliDispatchResult, FsDirEntry,
    FsReadDirParams, FsReadDirResult, FsReadFileParams, FsReadFileResult, HandleEventParams,
    HandleKeyParams, HandleMouseParams, LogParams, PluginInitParams, PluginInitResult,
    PluginShutdownParams, RenderParams, RenderResult, SnapshotGetParams, SnapshotGetResult,
    SnapshotPublishParams, SnapshotSubscribeParams, SnapshotSubscribeResult, Viewport,
};
use ainb_plugin_protocol::wire_buffer::WireBuffer;
use bytes::Bytes;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Child;
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, error, info, warn};

use crate::error::RuntimeError;
use crate::framing::{read_frame, write_frame};
use crate::process::{SIGTERM, signal_pgrp, spawn_plugin};
use crate::registry::RegisteredPlugin;
use crate::rpc::{
    IdCounter, Inbound, build_error_response, build_notification, build_request, build_response,
    parse_inbound,
};
use crate::snapshot::SnapshotStore;
use crate::types::{
    ActionOutcome, CliOutcome, LifecycleState, PluginId, RenderOutcome, RuntimeConfig, Topic,
};

/// Wire-protocol ABI version the runtime advertises.
const ABI_VERSION: u32 = 2;

/// Cached render output kept alive between async response and the
/// next `try_recv_render` poll on the TUI thread.
#[derive(Debug, Default, Clone)]
pub struct RenderCache {
    inner: Arc<parking_lot::Mutex<Option<WireBuffer>>>,
}

impl RenderCache {
    /// Construct an empty cache.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace the cached buffer.
    pub fn put(&self, buf: WireBuffer) {
        *self.inner.lock() = Some(buf);
    }

    /// Pop the cached buffer (returns `None` if nothing cached).
    pub fn try_take(&self) -> Option<WireBuffer> {
        self.inner.lock().take()
    }
}

/// Commands sent from a [`crate::RuntimeHandle`] to a per-plugin task.
#[derive(Debug)]
pub enum Command {
    /// Issue a `plugin/render` and reply on the oneshot.
    Render {
        /// Viewport size requested.
        viewport: Viewport,
        /// Render generation hint passed through to the plugin.
        generation: u64,
        /// Reply channel.
        reply: oneshot::Sender<RenderOutcome>,
    },
    /// Issue a `plugin/cli_dispatch`.
    Cli {
        /// CLI namespace.
        namespace: String,
        /// Argv with namespace stripped.
        argv: Vec<String>,
        /// Reply channel.
        reply: oneshot::Sender<CliOutcome>,
    },
    /// Issue a `host/action/invoke` (host-mediated; reply comes from
    /// the plugin that owns the action).
    Action {
        /// Action name.
        action: String,
        /// Payload bytes.
        payload: Bytes,
        /// Caller-supplied timeout; 0 ms = no timeout.
        timeout_ms: u64,
        /// Reply channel.
        reply: oneshot::Sender<ActionOutcome>,
    },
    /// Forward a `plugin/handle_event` notification (snapshot delivery).
    HandleEvent {
        /// Topic.
        topic: Topic,
        /// Snapshot bytes.
        payload: Bytes,
    },
    /// Send `plugin/shutdown` and reap the process.
    Shutdown,
    /// Clear quarantine + allow respawn.
    Reload,
    /// Test aid: force the child to be `kill -9`'d to exercise
    /// crash-recovery code paths. Hidden from rustdoc — this is not
    /// part of the public API surface.
    #[doc(hidden)]
    InjectKill,
    /// Best-effort wake: spawn the child if not already running. Used
    /// by `Runtime::register` to honour `manifest.lifecycle.spawn = "eager"`.
    /// No reply — failures are recorded on the task's failure ledger
    /// and surface through `RuntimeHandle::lifecycle_state`.
    EnsureSpawned,
}

/// Inbox a [`crate::RuntimeHandle`] uses to drive the task.
pub type Inbox = mpsc::UnboundedSender<Command>;

/// Priority side-channel reserved for `plugin/handle_key` notifications.
///
/// Carved out of the main [`Inbox`] so a flood of `HandleEvent` chunks
/// (chunked `sessions.usage_data` publishes can enqueue 50+ items per
/// refresh on large datasets) can't queue ahead of an Esc keypress in
/// the FIFO ordering. The plugin task's `tokio::select!` is `biased;`
/// and reads from the key receiver first, so any pending keystroke is
/// dispatched before another `HandleEvent` is pulled — restores Esc
/// responsiveness even during a multi-second chunk drain.
pub type KeyInbox = mpsc::UnboundedSender<HandleKeyParams>;

/// Priority side-channel reserved for `plugin/handle_mouse` notifications.
///
/// Mirrors [`KeyInbox`]: a dedicated channel drained ahead of the main
/// [`Inbox`] in the task's `biased;` select, so a click or scroll on a
/// plugin screen can't queue behind a backlog of `HandleEvent` chunks.
pub type MouseInbox = mpsc::UnboundedSender<HandleMouseParams>;

/// Map of `plugin_id → inbox` for snapshot fan-out.
///
/// Used by [`PluginTask`] to fan out subscriber notifications when a
/// plugin issues `host/snapshot/publish`. Shared (clone-able `Arc`) with
/// `Runtime`, which maintains it alongside the public plugin handle map.
pub type InboxMap = Arc<parking_lot::RwLock<HashMap<PluginId, Inbox>>>;

/// Map of `plugin_id → render-dirty flag`.
///
/// Mirrors [`InboxMap`] — when a plugin's `host/snapshot/publish` fans
/// out to subscribers, each subscriber's flag is set so the host's
/// render-tick loop knows to kick a `plugin/render` for it. Without this
/// the dirty bit set on the host-side `publish_snapshot` path would miss
/// every plugin→plugin publish (session-reader → burndown is the
/// load-bearing case).
pub type DirtyMap = Arc<parking_lot::RwLock<HashMap<PluginId, Arc<std::sync::atomic::AtomicBool>>>>;

/// Maximum number of *consecutive* self-requested redraw frames the host
/// will honor before it stops re-marking the plugin's render-dirty flag.
///
/// `RenderResult.redraw` is a `requestAnimationFrame` analogue: a plugin
/// returns `redraw = true` to ask the host to paint it again next tick
/// without waiting for input. A well-behaved plugin uses it for short,
/// self-terminating animations (the radial-map recentre is ~6 frames; a
/// search spinner runs for the search duration — seconds). A buggy or
/// malicious plugin can return `redraw = true` *forever*, sustaining a
/// ~30 FPS render+repaint loop with no input — real battery/CPU drain
/// with no designed defense (the 33 ms input poll in `main.rs` is an
/// incidental backstop, not a cap).
///
/// At the host's ~30 FPS render cadence this bound is ≈ 20 s of
/// uninterrupted self-animation. That comfortably clears every
/// legitimate animation we ship or expect (a multi-second spinner is
/// ~150-300 frames) while bounding a runaway: once the streak exceeds
/// the cap the host logs one `warn!` and stops honoring the hint until
/// the streak is broken by an input event or a `redraw = false` frame,
/// either of which resets the counter and re-arms the animation.
pub const MAX_CONSECUTIVE_REDRAWS: u32 = 600;

/// Per-plugin runaway-redraw guard.
///
/// Counts *uninterrupted* `redraw = true` frames — a streak with no
/// intervening input event and no `redraw = false` render. While the
/// streak is at or below [`MAX_CONSECUTIVE_REDRAWS`] each redraw hint is
/// honored (the dirty flag is re-marked, kicking the next paint). Once
/// the streak exceeds the cap the governor latches "tripped": it stops
/// honoring redraw hints (returns `false` from
/// [`should_honor_redraw`](Self::should_honor_redraw)) and logs exactly
/// one warning. Any input-driven render or `redraw = false` frame calls
/// [`reset`](Self::reset), clearing the streak and the latch so a fresh
/// animation can run.
#[derive(Debug, Default)]
struct RedrawGovernor {
    /// Length of the current uninterrupted `redraw = true` streak.
    consecutive: u32,
    /// `true` once the streak first exceeded the cap; suppresses both
    /// further honoring and repeat warnings until the next `reset`.
    tripped: bool,
}

impl RedrawGovernor {
    /// Record one `redraw = true` frame and decide whether to honor it.
    ///
    /// `honor` is `true` while the consecutive-redraw streak is within the
    /// [`MAX_CONSECUTIVE_REDRAWS`] budget (re-mark dirty), `false` once it
    /// has been exceeded (drop the hint). `just_tripped` is `true` exactly
    /// on the frame the cap is first crossed, so the caller can emit a
    /// single `warn!`.
    const fn observe_redraw(&mut self) -> RedrawDecision {
        if self.tripped {
            // Already over budget — keep dropping hints silently until a
            // reset re-arms the animation.
            return RedrawDecision {
                honor: false,
                just_tripped: false,
            };
        }
        self.consecutive = self.consecutive.saturating_add(1);
        if self.consecutive > MAX_CONSECUTIVE_REDRAWS {
            self.tripped = true;
            RedrawDecision {
                honor: false,
                just_tripped: true,
            }
        } else {
            RedrawDecision {
                honor: true,
                just_tripped: false,
            }
        }
    }

    /// Break the streak: an input-driven render or a `redraw = false`
    /// frame arrived, so the next self-animation starts from a clean
    /// budget. Clears both the counter and the tripped latch.
    const fn reset(&mut self) {
        self.consecutive = 0;
        self.tripped = false;
    }
}

/// Outcome of [`RedrawGovernor::observe_redraw`].
struct RedrawDecision {
    /// Honor the redraw hint (re-mark the plugin's render-dirty flag)?
    honor: bool,
    /// Did this frame just cross the cap (emit the one-shot warning)?
    just_tripped: bool,
}

/// Spawn a per-plugin task and return its command inbox, key inbox, and
/// render cache.
///
/// Two send-ends are returned by design: the main `Inbox` carries every
/// command except keystrokes, and `KeyInbox` carries `HandleKey`
/// notifications. The plugin task drains the key channel with priority
/// (see `PluginTask::run`'s `biased;` select) so Esc and other
/// keystrokes don't queue behind chunked `HandleEvent` publishes.
pub fn spawn(
    plugin: Arc<RegisteredPlugin>,
    snapshots: SnapshotStore,
    inboxes: InboxMap,
    dirty: DirtyMap,
    config: RuntimeConfig,
    handle: &tokio::runtime::Handle,
) -> (
    Inbox,
    KeyInbox,
    MouseInbox,
    RenderCache,
    Arc<parking_lot::RwLock<LifecycleState>>,
) {
    let (tx, rx) = mpsc::unbounded_channel();
    let (key_tx, key_rx) = mpsc::unbounded_channel();
    let (mouse_tx, mouse_rx) = mpsc::unbounded_channel();
    let cache = RenderCache::new();
    let state = Arc::new(parking_lot::RwLock::new(LifecycleState::Idle));
    let task = PluginTask {
        plugin,
        snapshots,
        inboxes,
        dirty,
        config,
        cache: cache.clone(),
        state: state.clone(),
        rx,
        key_rx,
        mouse_rx,
        ledger: HashMap::new(),
        ids: IdCounter::new(),
        failures: VecDeque::new(),
        respawn_attempts: 0,
        last_used: Instant::now(),
        child: None,
        redraw_governor: RedrawGovernor::default(),
    };
    handle.spawn(task.run());
    (tx, key_tx, mouse_tx, cache, state)
}

/// Bookkeeping for one outstanding request.
#[allow(clippy::large_enum_variant)]
enum Pending {
    Render(oneshot::Sender<RenderOutcome>),
    Cli(oneshot::Sender<CliOutcome>),
    Action(oneshot::Sender<ActionOutcome>),
    Init,
}

struct ChildState {
    /// Child PID — kept for `kill(-pgid, SIGTERM)`.
    pid: i32,
    child: Child,
    stdin: tokio::process::ChildStdin,
    /// Inbound-frame channel filled by [`spawn_stdout_reader`]. We
    /// can't read from `stdout` inside the per-plugin `tokio::select!`
    /// loop because `BufReader::read_line` / `read_exact` are NOT
    /// cancel-safe — a partial body would get re-interpreted as a
    /// header on the next iteration. See Bug A in
    /// `fix/runtime-eager-spawn`.
    inbound_rx: mpsc::UnboundedReceiver<InboundEvent>,
    stdout_reader: tokio::task::JoinHandle<()>,
    stderr_drain: tokio::task::JoinHandle<()>,
}

struct PluginTask {
    plugin: Arc<RegisteredPlugin>,
    snapshots: SnapshotStore,
    /// Subscriber fan-out map (shared with `Runtime`). Used only when
    /// the plugin issues `host/snapshot/publish` — we look up each
    /// subscriber's inbox and forward a `Command::HandleEvent`.
    inboxes: InboxMap,
    /// Parallel `plugin_id → render-dirty` map (shared with `Runtime`).
    /// Set alongside the subscriber inbox dispatch above so the host's
    /// render-tick loop knows to repaint the subscriber on the next
    /// tick. Without this the dirty bit set on the host-side
    /// `publish_snapshot` path would miss every plugin→plugin publish.
    dirty: DirtyMap,
    config: RuntimeConfig,
    cache: RenderCache,
    state: Arc<parking_lot::RwLock<LifecycleState>>,
    rx: mpsc::UnboundedReceiver<Command>,
    /// Priority receiver for `plugin/handle_key` notifications. Drained
    /// before the main `rx` on every loop iteration so keystrokes
    /// (including Esc) are dispatched ahead of any backlog of
    /// `HandleEvent` chunks.
    key_rx: mpsc::UnboundedReceiver<HandleKeyParams>,
    /// Priority receiver for `plugin/handle_mouse` notifications. Drained
    /// alongside `key_rx` (both ahead of the main `rx`) so mouse clicks
    /// and scrolls on a plugin screen aren't starved by a `HandleEvent`
    /// backlog.
    mouse_rx: mpsc::UnboundedReceiver<HandleMouseParams>,
    ledger: HashMap<u64, Pending>,
    ids: IdCounter,
    failures: VecDeque<Instant>,
    respawn_attempts: usize,
    last_used: Instant,
    child: Option<ChildState>,
    /// Runaway-redraw guard. Bounds how many uninterrupted
    /// `RenderResult.redraw = true` frames the host will honor before it
    /// stops re-marking the render-dirty flag — see [`RedrawGovernor`].
    redraw_governor: RedrawGovernor,
}

impl PluginTask {
    fn set_state(&self, s: LifecycleState) {
        *self.state.write() = s;
    }

    async fn run(mut self) {
        let mut idle_tick = tokio::time::interval(Duration::from_secs(5));
        idle_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                // `biased;` makes the macro check arms top-down rather
                // than randomising, so keystrokes always preempt the
                // main command channel. Without this a 50-chunk
                // `HandleEvent` flood (chunked usage_data publish on a
                // 100k+ call dataset) would starve Esc and other
                // navigation keys until the chunks drained.
                biased;
                key = self.key_rx.recv() => if let Some(params) = key {
                    self.handle_key_command(params).await;
                },
                mouse = self.mouse_rx.recv() => if let Some(params) = mouse {
                    self.handle_mouse_command(params).await;
                },
                cmd = self.rx.recv() => match cmd {
                    Some(Command::Shutdown) | None => { self.shutdown().await; break; }
                    Some(c) => self.handle_command(c).await,
                },
                inbound = next_inbound(&mut self.child) => {
                    match inbound {
                        InboundEvent::Frame(body) => self.handle_inbound(&body).await,
                        InboundEvent::Eof | InboundEvent::Error => self.handle_exit().await,
                    }
                }
                _ = idle_tick.tick() => self.maybe_idle_reap().await,
            }
        }
    }

    /// Dispatch a `plugin/handle_key` notification.
    ///
    /// Mirrors the legacy `Command::HandleKey` arm (now removed): same
    /// idle-drop policy, same wire shape, just sourced from the
    /// priority channel rather than the multiplexed command channel.
    async fn handle_key_command(&mut self, params: HandleKeyParams) {
        self.last_used = Instant::now();
        // A keystroke is interactivity: re-arm the redraw governor so a
        // self-animation that follows the input (e.g. a recentre kicked
        // off by an arrow key) starts from a fresh frame budget even if a
        // prior animation had tripped the cap.
        self.redraw_governor.reset();
        if self.child.is_none() {
            // No process to push to. A key pressed before the plugin
            // is spawned has no plausible destination — the user
            // almost certainly won't expect it to be replayed once
            // the process is up.
            debug!(plugin = %self.plugin.id, "handle_key dropped (idle)");
            return;
        }
        let json = serde_json::to_value(params).expect("HandleKeyParams is serializable");
        let _ = self.send_notification(methods::PLUGIN_HANDLE_KEY, json).await;
    }

    /// Dispatch a `plugin/handle_mouse` notification. Mirrors
    /// [`Self::handle_key_command`]: same idle-drop policy and wire shape,
    /// sourced from the priority mouse channel.
    async fn handle_mouse_command(&mut self, params: HandleMouseParams) {
        self.last_used = Instant::now();
        // Mouse input is interactivity too — re-arm the redraw governor
        // for the same reason as `handle_key_command`.
        self.redraw_governor.reset();
        if self.child.is_none() {
            // No process to push to — a click before the plugin spawns
            // has no plausible destination, so drop it rather than replay.
            debug!(plugin = %self.plugin.id, "handle_mouse dropped (idle)");
            return;
        }
        let json = serde_json::to_value(params).expect("HandleMouseParams is serializable");
        let _ = self.send_notification(methods::PLUGIN_HANDLE_MOUSE, json).await;
    }

    async fn handle_command(&mut self, cmd: Command) {
        self.last_used = Instant::now();
        match cmd {
            Command::Render {
                viewport,
                generation,
                reply,
            } => {
                if let Err(e) = self.ensure_running().await {
                    let _ = reply.send(RenderOutcome::RuntimeError(e.to_string()));
                    return;
                }
                let params = serde_json::to_value(RenderParams {
                    viewport,
                    generation,
                })
                .expect("RenderParams is serializable");
                let id = self.ids.allocate();
                self.ledger.insert(id, Pending::Render(reply));
                if let Err(e) = self.send_request(id, methods::PLUGIN_RENDER, params).await {
                    if let Some(Pending::Render(r)) = self.ledger.remove(&id) {
                        let _ = r.send(RenderOutcome::RuntimeError(e.to_string()));
                    }
                }
            }
            Command::Cli {
                namespace,
                argv,
                reply,
            } => {
                if let Err(e) = self.ensure_running().await {
                    let _ = reply.send(CliOutcome::RuntimeError(e.to_string()));
                    return;
                }
                let params = serde_json::to_value(CliDispatchParams { namespace, argv })
                    .expect("CliDispatchParams is serializable");
                let id = self.ids.allocate();
                self.ledger.insert(id, Pending::Cli(reply));
                if let Err(e) = self.send_request(id, methods::PLUGIN_CLI_DISPATCH, params).await {
                    if let Some(Pending::Cli(r)) = self.ledger.remove(&id) {
                        let _ = r.send(CliOutcome::RuntimeError(e.to_string()));
                    }
                }
            }
            Command::Action {
                action,
                payload,
                timeout_ms,
                reply,
            } => {
                // For Phase 7a, the runtime delivers actions to the plugin
                // owning the namespace by sending it a synthesized
                // `plugin/handle_event` carrying the action — the v2
                // wire spec carries a dedicated host/action/invoke
                // shape, but the *target* plugin's task issues that.
                // Caller-supplied timeout enforced via tokio::time::timeout.
                if let Err(e) = self.ensure_running().await {
                    let _ = reply.send(ActionOutcome::RuntimeError(e.to_string()));
                    return;
                }
                let params = serde_json::to_value(ActionInvokeParams {
                    action,
                    payload,
                    timeout_ms,
                })
                .expect("ActionInvokeParams is serializable");
                let id = self.ids.allocate();
                self.ledger.insert(id, Pending::Action(reply));
                if let Err(e) = self.send_request(id, methods::HOST_ACTION_INVOKE, params).await {
                    if let Some(Pending::Action(r)) = self.ledger.remove(&id) {
                        let _ = r.send(ActionOutcome::RuntimeError(e.to_string()));
                    }
                }
            }
            Command::HandleEvent { topic, payload } => {
                if self.child.is_none() {
                    // No process to push to — caller's choice to lazy-spawn or skip.
                    debug!(plugin = %self.plugin.id, "handle_event dropped (idle)");
                    return;
                }
                let params = serde_json::to_value(HandleEventParams {
                    topic: topic.as_str().to_owned(),
                    payload,
                })
                .expect("HandleEventParams is serializable");
                let _ = self.send_notification(methods::PLUGIN_HANDLE_EVENT, params).await;
            }
            Command::Reload => {
                self.failures.clear();
                self.respawn_attempts = 0;
                if matches!(*self.state.read(), LifecycleState::Quarantined) {
                    self.set_state(LifecycleState::Idle);
                }
            }
            Command::Shutdown => unreachable!("handled in run() loop"),
            Command::InjectKill => {
                if let Some(cs) = &mut self.child {
                    let _ = cs.child.start_kill();
                }
            }
            Command::EnsureSpawned => {
                if let Err(e) = self.ensure_running().await {
                    warn!(plugin = %self.plugin.id, "eager spawn failed: {e}");
                }
            }
        }
    }

    async fn ensure_running(&mut self) -> Result<(), RuntimeError> {
        match *self.state.read() {
            LifecycleState::Running => return Ok(()),
            LifecycleState::Quarantined => {
                return Err(RuntimeError::Quarantined(self.plugin.id.clone()));
            }
            _ => {}
        }
        self.spawn_and_init().await
    }

    async fn spawn_and_init(&mut self) -> Result<(), RuntimeError> {
        self.set_state(LifecycleState::Spawning);
        let mut child = match spawn_plugin(&self.plugin.binary_path) {
            Ok(c) => c,
            Err(e) => {
                self.record_failure();
                return Err(e);
            }
        };
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| RuntimeError::Wire("stdin pipe missing".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| RuntimeError::Wire("stdout pipe missing".into()))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| RuntimeError::Wire("stderr pipe missing".into()))?;
        let pid_u32 =
            child.id().ok_or_else(|| RuntimeError::Wire("child pid unavailable".into()))?;
        let pid = i32::try_from(pid_u32)
            .map_err(|_| RuntimeError::Wire(format!("pid {pid_u32} doesn't fit in i32")))?;
        let plugin_name = self.plugin.id.clone();
        let stderr_drain = tokio::spawn(drain_stderr(plugin_name, stderr));
        let (inbound_tx, inbound_rx) = mpsc::unbounded_channel();
        let stdout_reader = spawn_stdout_reader(BufReader::new(stdout), inbound_tx);
        self.child = Some(ChildState {
            pid,
            child,
            stdin,
            inbound_rx,
            stdout_reader,
            stderr_drain,
        });
        self.send_init().await?;
        self.set_state(LifecycleState::Running);
        self.respawn_attempts = 0;
        Ok(())
    }

    async fn send_init(&mut self) -> Result<(), RuntimeError> {
        let granted: Vec<String> = collect_granted_capabilities(&self.plugin.manifest);
        let params = serde_json::to_value(PluginInitParams {
            manifest_path: self.plugin.manifest_path.to_string_lossy().into_owned(),
            granted_capabilities: granted,
            abi_version: ABI_VERSION,
            // Host-resolved `[plugins.<name>]` table (JSON), stamped onto the
            // RegisteredPlugin at discovery; JSON null when unconfigured.
            config: self.plugin.config.clone(),
        })
        .expect("PluginInitParams serializable");
        let id = self.ids.allocate();
        self.ledger.insert(id, Pending::Init);
        self.send_request(id, methods::PLUGIN_INIT, params).await?;
        Ok(())
    }

    async fn send_request(
        &mut self,
        id: u64,
        method: &str,
        params: Value,
    ) -> Result<(), RuntimeError> {
        let body = build_request(id, method, params)?;
        let cs = self
            .child
            .as_mut()
            .ok_or_else(|| RuntimeError::ProcessExited(self.plugin.id.clone()))?;
        write_frame(&mut cs.stdin, &body).await?;
        Ok(())
    }

    async fn send_notification(&mut self, method: &str, params: Value) -> Result<(), RuntimeError> {
        let body = build_notification(method, params)?;
        let cs = self
            .child
            .as_mut()
            .ok_or_else(|| RuntimeError::ProcessExited(self.plugin.id.clone()))?;
        write_frame(&mut cs.stdin, &body).await?;
        Ok(())
    }

    async fn handle_inbound(&mut self, body: &[u8]) {
        let parsed = match parse_inbound(body) {
            Ok(p) => p,
            Err(e) => {
                warn!(plugin = %self.plugin.id, "decode failed: {e}");
                return;
            }
        };
        match parsed {
            Inbound::Response { id, result } => {
                debug!(plugin = %self.plugin.id, id, ok = result.is_ok(), "inbound response");
                self.handle_response(id, result).await;
            }
            Inbound::Request { id, method, params } => {
                debug!(plugin = %self.plugin.id, id, method = %method, "inbound request");
                self.handle_host_request(id, &method, params).await;
            }
            Inbound::Notification { method, params } => {
                debug!(plugin = %self.plugin.id, method = %method, "inbound notification");
                self.handle_host_notification(&method, params);
            }
        }
    }

    async fn handle_response(&mut self, id: u64, result: Result<Value, RpcError>) {
        let Some(pending) = self.ledger.remove(&id) else {
            warn!(plugin = %self.plugin.id, "stray response id={id}");
            return;
        };
        match pending {
            Pending::Render(reply) => {
                let outcome = match result {
                    Ok(v) => match serde_json::from_value::<RenderResult>(v) {
                        Ok(rr) => {
                            // Self-animation: the plugin asked to be painted
                            // again next tick. Re-mark its render-dirty flag
                            // so the host's render loop kicks another
                            // `plugin/render` without waiting for input.
                            //
                            // Bounded by the per-plugin `redraw_governor`:
                            // a finite N-frame animation gets all N
                            // re-marks, but an unbounded `redraw = true`
                            // stream is cut off after
                            // `MAX_CONSECUTIVE_REDRAWS` so a buggy/malicious
                            // plugin can't sustain a battery-draining
                            // render loop forever. A `redraw = false` frame
                            // here resets the streak, re-arming the
                            // governor for the next animation (input events
                            // reset it too — see `handle_key_command` /
                            // `handle_mouse_command`).
                            if rr.redraw {
                                let decision = self.redraw_governor.observe_redraw();
                                if decision.just_tripped {
                                    warn!(
                                        plugin = %self.plugin.id,
                                        cap = MAX_CONSECUTIVE_REDRAWS,
                                        "plugin exceeded consecutive self-redraw cap; \
                                         ignoring its redraw hint until the next input \
                                         or non-redraw frame"
                                    );
                                }
                                if decision.honor {
                                    if let Some(flag) = self.dirty.read().get(&self.plugin.id) {
                                        flag.store(true, std::sync::atomic::Ordering::Release);
                                    }
                                }
                            } else {
                                // A settled (non-redraw) frame ends any
                                // active self-animation streak.
                                self.redraw_governor.reset();
                            }
                            self.cache.put(rr.buffer.clone());
                            RenderOutcome::Ok(rr.buffer)
                        }
                        Err(e) => RenderOutcome::RuntimeError(format!("decode: {e}")),
                    },
                    Err(e) => RenderOutcome::PluginError {
                        code: e.code,
                        message: e.message,
                    },
                };
                let _ = reply.send(outcome);
            }
            Pending::Cli(reply) => {
                let outcome = match result {
                    Ok(v) => match serde_json::from_value::<CliDispatchResult>(v) {
                        Ok(r) => CliOutcome::Ok(r),
                        Err(e) => CliOutcome::RuntimeError(format!("decode: {e}")),
                    },
                    Err(e) => CliOutcome::PluginError {
                        code: e.code,
                        message: e.message,
                    },
                };
                let _ = reply.send(outcome);
            }
            Pending::Action(reply) => {
                let outcome = match result {
                    Ok(v) => match serde_json::from_value::<ActionInvokeResult>(v) {
                        Ok(r) => ActionOutcome::Ok(r.payload),
                        Err(e) => ActionOutcome::RuntimeError(format!("decode: {e}")),
                    },
                    Err(e) => ActionOutcome::PluginError {
                        code: e.code,
                        message: e.message,
                    },
                };
                let _ = reply.send(outcome);
            }
            Pending::Init => match result {
                Ok(v) => {
                    if let Err(e) = serde_json::from_value::<PluginInitResult>(v) {
                        warn!(plugin = %self.plugin.id, "init result decode: {e}");
                    }
                }
                Err(e) => {
                    error!(plugin = %self.plugin.id, "init failed: {} {}", e.code, e.message);
                    self.record_failure();
                    self.kill_child().await;
                }
            },
        }
    }

    async fn handle_host_request(&mut self, id: u64, method: &str, params: Value) {
        let result = match method {
            methods::HOST_SNAPSHOT_GET => self.host_snapshot_get(params),
            methods::HOST_SNAPSHOT_SUBSCRIBE => self.host_snapshot_subscribe(params),
            methods::HOST_FS_READ_FILE => self.host_fs_read_file(params),
            methods::HOST_FS_READ_DIR => self.host_fs_read_dir(params),
            // host/action/invoke arriving FROM the plugin would be cross-plugin
            // routing — out of scope for the per-plugin task; rejected.
            other => Err(RpcError::method_not_found(other)),
        };
        let body = match result {
            Ok(v) => match build_response(id, v) {
                Ok(b) => b,
                Err(e) => {
                    error!(plugin = %self.plugin.id, "encode response: {e}");
                    return;
                }
            },
            Err(rpc_err) => match build_error_response(Some(id), rpc_err) {
                Ok(b) => b,
                Err(e) => {
                    error!(plugin = %self.plugin.id, "encode error response: {e}");
                    return;
                }
            },
        };
        if let Some(cs) = &mut self.child {
            debug!(plugin = %self.plugin.id, id, bytes = body.len(), "host->plugin response: writing");
            match write_frame(&mut cs.stdin, &body).await {
                Ok(()) => {
                    debug!(plugin = %self.plugin.id, id, "host->plugin response: write_frame OK");
                }
                Err(e) => warn!(plugin = %self.plugin.id, id, "write response: {e}"),
            }
        } else {
            warn!(plugin = %self.plugin.id, id, "host->plugin response: child missing, dropping response");
        }
    }

    fn host_snapshot_get(&self, params: Value) -> Result<Value, RpcError> {
        let p: SnapshotGetParams =
            serde_json::from_value(params).map_err(|e| RpcError::invalid_params(e.to_string()))?;
        let topic = Topic::from(p.topic);
        let (payload, version) = match self.snapshots.get(&topic) {
            Some((p, v)) => (Some(p), v),
            None => (None, 0),
        };
        let res = SnapshotGetResult { payload, version };
        Ok(serde_json::to_value(res).expect("SnapshotGetResult serializable"))
    }

    fn host_snapshot_subscribe(&self, params: Value) -> Result<Value, RpcError> {
        let p: SnapshotSubscribeParams =
            serde_json::from_value(params).map_err(|e| RpcError::invalid_params(e.to_string()))?;
        self.snapshots.subscribe(Topic::from(p.topic), self.plugin.id.clone());
        Ok(serde_json::to_value(SnapshotSubscribeResult::default())
            .expect("SnapshotSubscribeResult serializable"))
    }

    /// `host/fs/read_file` — read a file the plugin requested, gated by the
    /// plugin's `read_paths` capability. The target must resolve under one of
    /// the granted path prefixes (the security envelope); otherwise the read
    /// is denied with [`CAPABILITY_DENIED`](ainb_plugin_protocol::errors::CAPABILITY_DENIED).
    fn host_fs_read_file(&self, params: Value) -> Result<Value, RpcError> {
        let p: FsReadFileParams =
            serde_json::from_value(params).map_err(|e| RpcError::invalid_params(e.to_string()))?;
        let resolved = self.guard_read_path(&p.path)?;
        let bytes = std::fs::read(&resolved)
            .map_err(|e| RpcError::invalid_params(format!("read {}: {e}", p.path)))?;
        let res = FsReadFileResult {
            bytes: bytes.into(),
        };
        Ok(serde_json::to_value(res).expect("FsReadFileResult serializable"))
    }

    /// `host/fs/read_dir` — enumerate a directory the plugin requested, gated
    /// by the plugin's `read_paths` capability (same envelope as
    /// [`host_fs_read_file`](Self::host_fs_read_file)).
    fn host_fs_read_dir(&self, params: Value) -> Result<Value, RpcError> {
        let p: FsReadDirParams =
            serde_json::from_value(params).map_err(|e| RpcError::invalid_params(e.to_string()))?;
        let resolved = self.guard_read_path(&p.path)?;
        let mut entries = Vec::new();
        let read = std::fs::read_dir(&resolved)
            .map_err(|e| RpcError::invalid_params(format!("read_dir {}: {e}", p.path)))?;
        for entry in read.flatten() {
            let meta = entry.metadata();
            let is_dir = meta.as_ref().is_ok_and(std::fs::Metadata::is_dir);
            let size = if is_dir {
                0
            } else {
                meta.map_or(0, |m| m.len())
            };
            entries.push(FsDirEntry {
                name: entry.file_name().to_string_lossy().into_owned(),
                is_dir,
                size,
            });
        }
        let res = FsReadDirResult { entries };
        Ok(serde_json::to_value(res).expect("FsReadDirResult serializable"))
    }

    /// Resolve `requested` and enforce the `read_paths` capability envelope.
    ///
    /// A read is allowed iff the resolved target path is under one of the
    /// granted `read_paths` prefixes. `~` in grant prefixes is expanded to
    /// `$HOME`; **both** target and prefixes are resolved against a real
    /// on-disk anchor (see [`resolve_against_existing_ancestor`]) so `..`
    /// traversal cannot escape the envelope *and* a symlinked ancestor
    /// (`/tmp -> /private/tmp`, a symlinked `$HOME`) does not split the two
    /// operands — which would otherwise over-deny a legitimate in-envelope
    /// read of a not-yet-existing file. Returns the resolved path on success,
    /// or a [`CAPABILITY_DENIED`](ainb_plugin_protocol::errors::CAPABILITY_DENIED)
    /// error otherwise.
    fn guard_read_path(&self, requested: &str) -> Result<std::path::PathBuf, RpcError> {
        let grant = &self.plugin.manifest.capabilities.read_paths;
        let allow_list = grant.allow_list().filter(|l| !l.is_empty()).ok_or_else(|| {
            RpcError::capability_denied("read_paths (no path-scoped fs read granted)")
        })?;

        // Resolve the target symmetrically with the prefixes: canonicalize the
        // deepest existing ancestor (resolving any symlinks in the on-disk
        // portion) and re-append the non-existent lexical tail. This keeps a
        // `/tmp` target from diverging from a `/private/tmp` prefix when the
        // file does not yet exist, while still defeating `..` escapes (the tail
        // is `..`-collapsed before the existing-ancestor walk).
        let resolved = resolve_against_existing_ancestor(std::path::Path::new(requested));

        for prefix in allow_list {
            let expanded = expand_tilde(prefix);
            let prefix_resolved = resolve_against_existing_ancestor(&expanded);
            if resolved.starts_with(&prefix_resolved) {
                return Ok(resolved);
            }
        }

        Err(RpcError::capability_denied(format!(
            "read_paths: {requested} is outside the granted envelope"
        )))
    }

    fn handle_host_notification(&self, method: &str, params: Value) {
        match method {
            methods::HOST_SNAPSHOT_PUBLISH => {
                let Ok(p) = serde_json::from_value::<SnapshotPublishParams>(params) else {
                    warn!(plugin = %self.plugin.id, "bad snapshot publish");
                    return;
                };
                let topic = Topic::from(p.topic);
                let payload = p.payload;
                let _ = self.snapshots.publish(topic.clone(), payload.clone());
                // Fan out to every subscriber — the snapshot store
                // only retains the *latest* publish, so chunked publishes
                // (session-reader → burndown) would lose all but the
                // last chunk if we relied on a passive snapshot_get.
                let subs = self.snapshots.subscribers(&topic);
                if !subs.is_empty() {
                    let inboxes = self.inboxes.read();
                    let dirty = self.dirty.read();
                    for sub in subs {
                        // Don't echo back to the publisher.
                        if sub == self.plugin.id {
                            continue;
                        }
                        if let Some(flag) = dirty.get(&sub) {
                            // Mark dirty BEFORE the inbox send so the
                            // host's render tick can't drain the flag
                            // between the event landing and the next
                            // render kick. Worst case the host fires
                            // one no-op render — harmless.
                            flag.store(true, std::sync::atomic::Ordering::Release);
                        }
                        if let Some(inbox) = inboxes.get(&sub) {
                            let _ = inbox.send(Command::HandleEvent {
                                topic: topic.clone(),
                                payload: payload.clone(),
                            });
                        }
                    }
                }
            }
            methods::HOST_LOG => {
                let Ok(p) = serde_json::from_value::<LogParams>(params) else {
                    warn!(plugin = %self.plugin.id, "bad log payload");
                    return;
                };
                info!(plugin = %self.plugin.id, level = ?p.level, "{}", p.message);
            }
            other => debug!(plugin = %self.plugin.id, "ignoring notification: {other}"),
        }
    }

    async fn handle_exit(&mut self) {
        warn!(plugin = %self.plugin.id, "plugin exited / pipe closed");
        // Drain any outstanding ledger entries with a runtime-error.
        let pending: Vec<(u64, Pending)> = self.ledger.drain().collect();
        for (_, p) in pending {
            match p {
                Pending::Render(r) => {
                    let _ = r.send(RenderOutcome::RuntimeError("plugin exited".into()));
                }
                Pending::Cli(r) => {
                    let _ = r.send(CliOutcome::RuntimeError("plugin exited".into()));
                }
                Pending::Action(r) => {
                    let _ = r.send(ActionOutcome::RuntimeError("plugin exited".into()));
                }
                Pending::Init => {}
            }
        }
        if let Some(cs) = self.child.take() {
            cs.stderr_drain.abort();
            cs.stdout_reader.abort();
        }
        self.record_failure();
        if self.is_quarantine_due() {
            self.set_state(LifecycleState::Quarantined);
            error!(plugin = %self.plugin.id, "quarantined after {} fails", self.failures.len());
            return;
        }
        self.set_state(LifecycleState::Backoff);
        self.respawn_attempts += 1;
        let backoff = self
            .config
            .respawn_backoff
            .get(self.respawn_attempts.saturating_sub(1))
            .copied()
            .unwrap_or(Duration::from_secs(16));
        debug!(plugin = %self.plugin.id, "backoff {backoff:?}");
        tokio::time::sleep(backoff).await;
        self.set_state(LifecycleState::Idle);

        // Honour `manifest.lifecycle.spawn = "eager"` on the *exit
        // path*, not only at registration. An eager plugin that exits
        // (process crash, broken pipe, etc.) needs to come back without
        // a host event triggering it — otherwise a one-shot failure
        // wedges the plugin dead for the rest of the TUI session. The
        // original bug: session-reader shipped a single oversize chunk,
        // host framer rejected it, plugin's stdout pipe closed, plugin
        // exited; with no respawn here the burndown UI stayed at
        // "Scanning sessions…" forever. Lazy plugins are left alone —
        // they only spawn when first used.
        if matches!(
            self.plugin.manifest.lifecycle.spawn,
            ainb_plugin_protocol::manifest::SpawnMode::Eager
        ) && !matches!(*self.state.read(), LifecycleState::Quarantined)
        {
            debug!(
                plugin = %self.plugin.id,
                "eager: respawning after exit (attempt {})",
                self.respawn_attempts
            );
            if let Err(e) = self.spawn_and_init().await {
                warn!(
                    plugin = %self.plugin.id,
                    "eager respawn after exit failed: {e}"
                );
                // spawn_and_init transitions to Spawning then either
                // Running on success or leaves us in Spawning on
                // failure; explicitly fall back to Idle so the next
                // failure scheduling math doesn't confuse states.
                self.set_state(LifecycleState::Idle);
            }
        }
    }

    fn record_failure(&mut self) {
        let now = Instant::now();
        self.failures.push_back(now);
        let cutoff = now.checked_sub(self.config.failure_window).unwrap_or(now);
        while let Some(front) = self.failures.front() {
            if *front < cutoff {
                self.failures.pop_front();
            } else {
                break;
            }
        }
    }

    fn is_quarantine_due(&self) -> bool {
        self.failures.len() >= self.config.quarantine_failure_threshold
    }

    async fn maybe_idle_reap(&mut self) {
        let elapsed = self.last_used.elapsed();
        let reap_threshold =
            Duration::from_secs(u64::from(self.plugin.manifest.lifecycle.idle_reap_secs))
                .max(self.config.idle_reap);
        let has_subs = self.plugin.manifest.subscribes.snapshots.iter().any(|_| true);
        if matches!(*self.state.read(), LifecycleState::Running)
            && elapsed >= reap_threshold
            && !has_subs
        {
            info!(plugin = %self.plugin.id, "idle reap (idle for {elapsed:?})");
            self.shutdown().await;
            self.set_state(LifecycleState::Idle);
        }
    }

    async fn shutdown(&mut self) {
        if self.child.is_none() {
            return;
        }
        self.set_state(LifecycleState::ShuttingDown);
        let params = serde_json::to_value(PluginShutdownParams::default())
            .expect("PluginShutdownParams serializable");
        let _ = self.send_notification(methods::PLUGIN_SHUTDOWN, params).await;
        // Wait up to 5s for graceful exit; then SIGTERM the process group.
        let cs = self.child.as_mut().unwrap();
        let pid = cs.pid;
        let exit = tokio::time::timeout(Duration::from_secs(5), cs.child.wait()).await;
        if exit.is_err() {
            warn!(plugin = %self.plugin.id, "graceful exit timeout — SIGTERM pgrp");
            let _ = signal_pgrp(pid, SIGTERM);
            let _ = tokio::time::timeout(Duration::from_secs(2), cs.child.wait()).await;
        }
        self.kill_child().await;
    }

    async fn kill_child(&mut self) {
        if let Some(mut cs) = self.child.take() {
            let _ = cs.child.start_kill();
            let _ = cs.child.wait().await;
            cs.stderr_drain.abort();
            cs.stdout_reader.abort();
        }
        self.snapshots.unsubscribe_all(&self.plugin.id);
    }
}

/// Expand a leading `~` / `~/` to `$HOME`. Other paths pass through. Unix-only
/// home resolution via `$HOME`, consistent with the project's platform stance.
fn expand_tilde(path: &str) -> std::path::PathBuf {
    if path == "~" {
        if let Ok(home) = std::env::var("HOME") {
            return std::path::PathBuf::from(home);
        }
    } else if let Some(rest) = path.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return std::path::Path::new(&home).join(rest);
        }
    }
    std::path::PathBuf::from(path)
}

/// Lexically normalize a path without touching the filesystem: collapse `.`
/// and resolve `..` against earlier components. Used as a fallback when a path
/// can't be canonicalized (doesn't yet exist) so the `read_paths` guard still
/// rejects `..` traversal out of the envelope.
fn normalize_lexical(path: &std::path::Path) -> std::path::PathBuf {
    use std::path::Component;
    let mut out = std::path::PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Resolve `path` against a real on-disk anchor so the `read_paths` guard can
/// compare a target and a granted prefix *symmetrically*, even when the target
/// does not exist yet.
///
/// Strategy:
/// 1. Lexically collapse `.`/`..` first ([`normalize_lexical`]) — so a `..`
///    escape (`/grant/../../etc/passwd`) is flattened before anything touches
///    the filesystem and can never re-enter the envelope.
/// 2. Walk up to the deepest *existing* ancestor and `canonicalize` it
///    (resolving every symlink in the on-disk portion, e.g. `/tmp ->
///    /private/tmp`).
/// 3. Re-append the remaining non-existent lexical tail verbatim.
///
/// Because the granted prefix (which always exists) is resolved the same way,
/// a `/tmp` target and a `/private/tmp` prefix no longer diverge when the file
/// is not yet on disk — fixing the spurious `-32001` over-denial — while a
/// `..`-traversal to a real file still canonicalizes outside the envelope and
/// is correctly denied.
fn resolve_against_existing_ancestor(path: &std::path::Path) -> std::path::PathBuf {
    let normalized = normalize_lexical(path);

    // Find the deepest existing ancestor of `normalized`, canonicalize it, then
    // re-append the tail of components that don't (yet) exist on disk.
    let mut anchor = normalized.as_path();
    let mut tail: Vec<&std::ffi::OsStr> = Vec::new();
    loop {
        if let Ok(canon) = std::fs::canonicalize(anchor) {
            let mut out = canon;
            for comp in tail.iter().rev() {
                out.push(comp);
            }
            return out;
        }
        match anchor.parent() {
            // Push the leaf component onto the tail and try the parent.
            Some(parent) => {
                if let Some(name) = anchor.file_name() {
                    tail.push(name);
                }
                anchor = parent;
            }
            // No existing ancestor at all (e.g. a bare relative name, or a root
            // that can't be canonicalized): fall back to the lexical form so the
            // guard still has a deterministic, `..`-free path to compare.
            None => return normalized,
        }
    }
}

fn collect_granted_capabilities(m: &ainb_plugin_protocol::manifest::Manifest) -> Vec<String> {
    let c = &m.capabilities;
    let mut out = Vec::new();
    if c.read_sessions.is_granted() {
        out.push("read_sessions".into());
    }
    if c.write_plugin_data.is_granted() {
        out.push("write_plugin_data".into());
    }
    if c.event_bus.is_granted() {
        out.push("event_bus".into());
    }
    if c.network.is_granted() {
        out.push("network".into());
    }
    if c.spawn_subprocess.is_granted() {
        out.push("spawn_subprocess".into());
    }
    if c.read_claude_logs.is_granted() {
        out.push("read_claude_logs".into());
    }
    if c.read_codex_logs.is_granted() {
        out.push("read_codex_logs".into());
    }
    if c.read_paths.is_granted() {
        out.push("read_paths".into());
    }
    out
}

/// What the stdout reader pushes into the per-plugin inbound channel.
/// `mpsc::recv` IS cancel-safe (drops at the start of the await), so
/// the outer `select!` loop can read these without losing partial-
/// frame state — unlike `read_line`/`read_exact` on `BufReader`.
#[derive(Debug)]
enum InboundEvent {
    Frame(Vec<u8>),
    Eof,
    Error,
}

/// Spawn a task that reads framed inbounds from `stdout` and pushes
/// them onto `tx`. Exits on EOF or unrecoverable decode error. Owns
/// the `BufReader` for its lifetime, which guarantees no cross-await
/// cancellation can corrupt the framer state — the original culprit
/// for the "header block exceeded 8192 bytes" stalls under chunked
/// publish workloads.
fn spawn_stdout_reader(
    mut reader: BufReader<tokio::process::ChildStdout>,
    tx: mpsc::UnboundedSender<InboundEvent>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            match read_frame(&mut reader).await {
                Ok(Some(body)) => {
                    if tx.send(InboundEvent::Frame(body)).is_err() {
                        return;
                    }
                }
                Ok(None) => {
                    let _ = tx.send(InboundEvent::Eof);
                    return;
                }
                Err(e) => {
                    tracing::warn!("frame decode err: {e}");
                    let _ = tx.send(InboundEvent::Error);
                    return;
                }
            }
        }
    })
}

async fn next_inbound(child: &mut Option<ChildState>) -> InboundEvent {
    match child {
        Some(cs) => cs.inbound_rx.recv().await.unwrap_or(InboundEvent::Eof),
        None => {
            // No child — park forever (until select! wakes us via cmd
            // or timer). pending() never resolves.
            std::future::pending().await
        }
    }
}

async fn drain_stderr(plugin: PluginId, stderr: tokio::process::ChildStderr) {
    let mut reader = BufReader::new(stderr).lines();
    while let Ok(Some(line)) = reader.next_line().await {
        // info! so plugin stderr (e.g. eprintln from session-reader)
        // surfaces in the host JSONL by default. Plugins are expected
        // to use host.log() for normal logging — stderr is for unstructured
        // diagnostics that should not be filtered out.
        info!(plugin = %plugin, stream = "stderr", "{line}");
    }
}

#[cfg(test)]
mod tests {
    //! Channel-bias smoke test for the priority key path.
    //!
    //! The plugin task drains its key inbox before the main command
    //! inbox via `tokio::select! { biased; ... }`. Tokio's docs
    //! guarantee biased branches resolve in declaration order, but
    //! this test pins the contract so a future refactor that drops
    //! the keyword (or reorders the branches) trips a unit-level
    //! regression rather than a TUI freeze observed in production.

    use super::{
        HandleKeyParams, MAX_CONSECUTIVE_REDRAWS, RedrawGovernor, collect_granted_capabilities,
        resolve_against_existing_ancestor,
    };
    use ainb_plugin_protocol::manifest::{
        Capabilities, CapabilityGrant, Lifecycle, Manifest, PluginMeta, Provides, Subscribes,
    };
    use ainb_plugin_protocol::params::{KeyCode, KeyEvent, KeyKind};
    use tokio::sync::mpsc;

    fn manifest_with_caps(capabilities: Capabilities) -> Manifest {
        Manifest {
            plugin: PluginMeta {
                name: "p".into(),
                version: "0.1.0".into(),
                abi_version: 2,
                description: String::new(),
            },
            capabilities,
            provides: Provides::default(),
            subscribes: Subscribes::default(),
            lifecycle: Lifecycle::default(),
            config: Vec::new(),
        }
    }

    #[test]
    fn test_collect_granted_includes_read_paths() {
        // Manifest granting read_paths → collector lists "read_paths".
        let granted = manifest_with_caps(Capabilities {
            read_paths: CapabilityGrant::List(vec!["/x".into()]),
            ..Capabilities::default()
        });
        let caps = collect_granted_capabilities(&granted);
        assert!(
            caps.contains(&"read_paths".to_string()),
            "expected read_paths in {caps:?}"
        );

        // Absent (default Bool(false)) → not listed.
        let ungranted = manifest_with_caps(Capabilities::default());
        let caps = collect_granted_capabilities(&ungranted);
        assert!(
            !caps.contains(&"read_paths".to_string()),
            "did not expect read_paths in {caps:?}"
        );
    }

    /// A unique, repo-local scratch dir under the crate's `target/` (never the
    /// home dir, `~/.cargo`, or the OS temp). The caller creates a *symlink*
    /// inside it, which supplies the symlink asymmetry the guard fix targets —
    /// so we don't need a symlinked OS temp dir to exercise it.
    fn repo_local_tmp(tag: &str) -> std::path::PathBuf {
        let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("rt-guard-tests")
            .join(format!(
                "{tag}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
        std::fs::create_dir_all(&base).expect("create repo-local scratch dir");
        base
    }

    #[test]
    fn resolve_existing_ancestor_resolves_symlink_for_nonexistent_tail() {
        // A grant whose ancestor is a symlink, with a NOT-yet-existing target
        // under it, must resolve to the same on-disk root as the canonicalized
        // grant — so `starts_with` stays true (no spurious deny).
        let base = repo_local_tmp("resolve");
        let real = base.join("real");
        std::fs::create_dir_all(&real).expect("create real dir");
        let link = base.join("link");
        std::os::unix::fs::symlink(&real, &link).expect("symlink link -> real");

        // Prefix (the grant) exists → canonicalizes to `.../real`.
        let prefix = resolve_against_existing_ancestor(&link);
        // Target is a non-existent file under the symlinked grant.
        let ghost = link.join("ghost.md");
        let target = resolve_against_existing_ancestor(&ghost);

        assert!(
            target.starts_with(&prefix),
            "in-envelope non-existent target {target:?} must resolve under prefix {prefix:?}"
        );
        // The tail is preserved verbatim on the canonical root.
        assert_eq!(
            target.file_name().and_then(|s| s.to_str()),
            Some("ghost.md")
        );

        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn resolve_existing_ancestor_denies_parent_dir_escape() {
        // A `..`-escape to a real file outside the grant must NOT resolve under
        // the grant prefix (the headline security claim — preserved by the
        // `..`-collapse-before-anchor step).
        let base = repo_local_tmp("escape");
        let grant = base.join("grant");
        let secret_dir = base.join("secret");
        std::fs::create_dir_all(&grant).expect("create grant");
        std::fs::create_dir_all(&secret_dir).expect("create secret");
        let secret = secret_dir.join("passwd");
        std::fs::write(&secret, b"x").expect("write secret");

        let prefix = resolve_against_existing_ancestor(&grant);
        // `grant/../secret/passwd` escapes to the sibling `secret` dir.
        let escape = grant.join("..").join("secret").join("passwd");
        let target = resolve_against_existing_ancestor(&escape);

        assert!(
            !target.starts_with(&prefix),
            "`..`-escape {target:?} must NOT resolve under grant prefix {prefix:?}"
        );

        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn biased_select_drains_key_inbox_before_command_inbox() {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async move {
            let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel::<&'static str>();
            let (key_tx, mut key_rx) = mpsc::unbounded_channel::<HandleKeyParams>();

            // Fill the main command channel with 100 entries first.
            // Then enqueue a single key. Under the production select!
            // pattern (biased + key_rx first arm), the next pull is
            // the key — even though commands arrived earlier in
            // wall-clock time.
            for _ in 0..100 {
                cmd_tx.send("evt").unwrap();
            }
            let params = HandleKeyParams {
                screen_id: "test".into(),
                key: KeyEvent {
                    code: KeyCode::Esc,
                    mods: 0,
                    kind: KeyKind::Press,
                },
                generation: 1,
            };
            key_tx.send(params.clone()).unwrap();

            let pulled = tokio::select! {
                biased;
                k = key_rx.recv() => k.map(|p| format!("key:{}", p.screen_id)),
                c = cmd_rx.recv() => c.map(|s| format!("cmd:{s}")),
            };
            assert_eq!(pulled, Some("key:test".to_string()));

            // After the priority drain, the remaining 100 commands
            // are still there in FIFO order — the bias didn't drop
            // anything.
            let mut remaining = 0usize;
            while cmd_rx.try_recv().is_ok() {
                remaining += 1;
            }
            assert_eq!(remaining, 100);
        });
    }

    /// A finite, self-terminating animation must get *every* frame
    /// honored — the governor only bounds runaways, never clips a
    /// legitimate animation. Models the radial-map recentre (~6 frames)
    /// and, by clearing the cap, a multi-second spinner.
    #[test]
    fn governor_honors_full_finite_animation() {
        let mut gov = RedrawGovernor::default();

        // (a) A short animation: a handful of redraw frames, all honored.
        for frame in 0..6 {
            let d = gov.observe_redraw();
            assert!(d.honor, "finite animation frame {frame} must be honored");
            assert!(!d.just_tripped, "short animation must not trip the cap");
        }
        // A settled (non-redraw) frame ends the animation and re-arms the
        // budget — exactly what the runtime does in the `else` arm.
        gov.reset();

        // (b) A long-but-finite spinner that runs right up to the cap:
        // every one of `MAX_CONSECUTIVE_REDRAWS` frames is still honored,
        // and the cap is never crossed.
        for frame in 0..MAX_CONSECUTIVE_REDRAWS {
            let d = gov.observe_redraw();
            assert!(
                d.honor,
                "spinner frame {frame} (≤ cap) must be honored, streak still in budget"
            );
            assert!(!d.just_tripped, "frame {frame} (≤ cap) must not trip");
        }
        // The plugin then settles — animation done, no warning ever fired.
        gov.reset();
        assert_eq!(gov.consecutive, 0, "reset clears the streak");
        assert!(!gov.tripped, "a finite animation never trips the latch");
    }

    /// An unbounded `redraw = true` stream is cut off once it exceeds the
    /// cap: the governor stops honoring the hint and signals the one-shot
    /// warning exactly once. After an input event (`reset`) the animation
    /// is re-armed and honored again.
    #[test]
    fn governor_cuts_off_runaway_and_resumes_after_input() {
        let mut gov = RedrawGovernor::default();

        // Frames 1..=cap are honored (proven above); drive straight to the
        // budget edge.
        for _ in 0..MAX_CONSECUTIVE_REDRAWS {
            assert!(gov.observe_redraw().honor);
        }

        // The very next frame crosses the cap: dropped, and the one-shot
        // warning fires exactly here.
        let trip = gov.observe_redraw();
        assert!(!trip.honor, "frame past the cap must be dropped");
        assert!(
            trip.just_tripped,
            "crossing the cap must signal the warning"
        );

        // Every subsequent runaway frame is dropped *silently* — no repeat
        // warnings, no re-marks — for as long as the plugin keeps spamming.
        for _ in 0..10_000 {
            let d = gov.observe_redraw();
            assert!(!d.honor, "runaway frame must stay dropped");
            assert!(!d.just_tripped, "the warning must fire only once");
        }

        // An input event resets the governor (the runtime calls `reset`
        // from `handle_key_command` / `handle_mouse_command`). The next
        // self-animation is honored again from a clean budget.
        gov.reset();
        let after_input = gov.observe_redraw();
        assert!(
            after_input.honor,
            "redraw after an input event must be honored again"
        );
        assert!(!after_input.just_tripped);
        assert_eq!(gov.consecutive, 1, "streak restarts at 1 after reset");
    }

    /// A `redraw = false` frame mid-stream resets the streak just like an
    /// input event would — a brief settle between animations never trips
    /// the cap even if the total frame count exceeds it.
    #[test]
    fn governor_resets_on_non_redraw_frame() {
        let mut gov = RedrawGovernor::default();

        // Two back-to-back animations, each just under the cap, separated
        // by a settle. Without the reset their combined length would trip
        // the cap; with it, neither does.
        for _ in 0..MAX_CONSECUTIVE_REDRAWS {
            assert!(gov.observe_redraw().honor);
        }
        gov.reset(); // models the runtime's `redraw = false` else-arm
        for _ in 0..MAX_CONSECUTIVE_REDRAWS {
            assert!(
                gov.observe_redraw().honor,
                "second animation after a settle must be fully honored"
            );
        }
        assert!(
            !gov.tripped,
            "a settle between animations must avoid the trip"
        );
    }
}
