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
    ActionInvokeParams, ActionInvokeResult, CliDispatchParams, CliDispatchResult,
    EventStreamCancelParams, EventStreamSubscribeParams, EventStreamSubscribeResult, FsDirEntry,
    FsReadDirParams, FsReadDirResult, FsReadFileParams, FsReadFileResult, HandleActionParams,
    HandleEventParams, HandleKeyParams, HandleMouseParams, LogParams, PluginInitParams,
    PluginInitResult, PluginShutdownParams, RenderParams, RenderResult, SecretStoreGetParams,
    SnapshotGetParams, SnapshotGetResult, SnapshotPublishParams, SnapshotSubscribeParams,
    SnapshotSubscribeResult, SpawnManagedSubprocessParams, SpawnManagedSubprocessResult,
    UnixSocketCloseParams, UnixSocketDialParams, UnixSocketDialResult, UnixSocketSendParams,
    Viewport, WorkspaceCreateParams, WorkspaceDeleteParams, WorkspaceSetActiveParams,
    WorkspaceSetDefaultParams,
};
use ainb_plugin_protocol::wire_buffer::WireBuffer;
use bytes::Bytes;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Child;
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, error, info, warn};

use crate::error::RuntimeError;
use crate::event_stream::{EventStreamRegistry, topic_allowed};
use crate::framing::{read_frame, write_frame};
use crate::inbox::{DropOldestReceiver, DropOldestSender, INPUT_INBOX_CAPACITY, drop_oldest};
use crate::managed_subprocess::ManagedSubprocessRegistry;
use crate::process::{SIGTERM, signal_pgrp, spawn_plugin};
use crate::registry::RegisteredPlugin;
use crate::rpc::{
    IdCounter, Inbound, build_error_response, build_notification, build_request, build_response,
    parse_inbound,
};
use crate::secret_store::{SharedSecretBackend, secret_store_get_logic};
use crate::snapshot::SnapshotStore;
use crate::types::{
    ActionOutcome, CliOutcome, LifecycleState, LogTap, PluginId, RenderOutcome, RuntimeConfig,
    Topic,
};
use crate::unix_socket::{UnixSocketRegistry, path_allowed};
use crate::workspace_store::{
    SharedWorkspaceStore, create_logic, delete_logic, get_active_logic, list_logic,
    set_active_logic, set_default_logic,
};

/// Wire-protocol ABI version the runtime advertises.
const ABI_VERSION: u32 = 2;

/// Esc presses a plugin may leave unanswered in a row before the next one goes
/// to the host (#1087).
///
/// Three is past any honest miss: a plugin that pops one nested level per Esc
/// changes its frame on every press and never starts a streak.
pub const ESC_UNANSWERED_LIMIT: u32 = 3;

/// Esc presses in a row, with no other key between them, after which the next
/// Esc goes to the host whatever the frames showed (#1087 review).
///
/// The frame check cannot judge a plugin that repaints on its own (a spinner,
/// a clock): every frame differs, so every Esc looks answered. No honest
/// screen needs this many Esc presses in a row to back out of its nesting.
pub const ESC_PRESS_CEILING: u32 = 8;

/// What to do with an Esc the host is about to send a plugin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EscVerdict {
    /// Send it to the plugin.
    Deliver,
    /// The plugin has ignored [`ESC_UNANSWERED_LIMIT`] Esc presses in a row, or
    /// the user has pressed Esc [`ESC_PRESS_CEILING`] times with no other key:
    /// give this one to the host, which leaves the screen.
    ReturnToHost,
}

/// Tracks whether a plugin answers Esc, judged only on painted frames.
///
/// A plugin that keeps rendering while ignoring Esc used to hold its screen
/// with Ctrl+C as the only way out (#1087). An Esc counts as UNANSWERED once a
/// frame rendered after it reached the plugin looks exactly like the frame
/// before it, and as ANSWERED when it differs. Only the FIRST such frame is
/// judged, the one the render kicked right after the key produces: a later
/// repaint (a ticking clock, an animation) says nothing about the Esc, and
/// counting it would let any plugin that repaints hold the screen. With no frame yet,
/// there is no evidence and the streak is left alone, so pressing Esc faster
/// than the plugin paints never ejects it. Any other key ends the streak.
#[derive(Debug, Default)]
pub struct BackWatch {
    last_frame: Option<u64>,
    pending: Option<PendingEsc>,
    unanswered: u32,
    /// Esc presses since the last other key, answered or not.
    presses: u32,
}

#[derive(Debug)]
struct PendingEsc {
    generation: u64,
    frame_before: Option<u64>,
    /// Whether the first frame after the Esc differed from the one before it;
    /// `None` until that frame arrives.
    changed: Option<bool>,
}

impl BackWatch {
    /// A painted frame, reflecting every key up to generation `keys_through`
    /// (`None` when no key had been written when it was requested).
    pub fn frame(&mut self, frame: u64, keys_through: Option<u64>) {
        if let Some(pending) = self.pending.as_mut().filter(|pending| pending.changed.is_none()) {
            if keys_through.is_some_and(|keys| keys >= pending.generation) {
                pending.changed = Some(pending.frame_before != Some(frame));
            } else {
                // Requested before the Esc reached the plugin but painted after
                // it was sent: this, not the older frame, is what the Esc
                // started from.
                pending.frame_before = Some(frame);
            }
        }
        self.last_frame = Some(frame);
    }

    /// An Esc with key generation `generation` is about to be sent.
    pub fn esc(&mut self, generation: u64) -> EscVerdict {
        match self.pending.take().and_then(|pending| pending.changed) {
            Some(true) => self.unanswered = 0,
            Some(false) => self.unanswered += 1,
            None => {}
        }
        if self.unanswered >= ESC_UNANSWERED_LIMIT || self.presses >= ESC_PRESS_CEILING {
            self.unanswered = 0;
            self.presses = 0;
            return EscVerdict::ReturnToHost;
        }
        self.presses += 1;
        self.pending = Some(PendingEsc {
            generation,
            frame_before: self.last_frame,
            changed: None,
        });
        EscVerdict::Deliver
    }

    /// A key other than Esc: whatever the plugin did with the last Esc, the
    /// user has moved on.
    pub fn other_key(&mut self) {
        self.pending = None;
        self.unanswered = 0;
        self.presses = 0;
    }
}

/// Hash a painted frame for [`BackWatch`]. Equal buffers hash equal. Hashes
/// the decoded buffer in place: serialising every render response a second
/// time just to compare it would cost the task on each frame.
fn frame_hash(buffer: &WireBuffer) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    buffer.hash(&mut hasher);
    hasher.finish()
}

/// Cached render output kept alive between async response and the
/// next `try_recv_render` poll on the TUI thread.
///
/// `captures_text` is a PERSISTENT latch (not consumed by `try_take`, unlike the
/// buffer): the host reads the focused plugin's current text-capture state on
/// every keystroke, not just when a fresh frame is drained. The per-plugin task
/// refreshes it from each `RenderResult.captures_text` — so it always reflects
/// the last painted frame — and the host reads it via
/// [`RuntimeHandle::captures_text`](crate::RuntimeHandle::captures_text).
#[derive(Debug, Default, Clone)]
pub struct RenderCache {
    inner: Arc<parking_lot::Mutex<Option<WireBuffer>>>,
    captures_text: Arc<std::sync::atomic::AtomicBool>,
    /// When the host last found this plugin's screen on display (#1053).
    shown_at: Arc<parking_lot::Mutex<Option<Instant>>>,
    /// Whether the plugin is answering Esc (#1087).
    back_watch: Arc<parking_lot::Mutex<BackWatch>>,
}

impl RenderCache {
    /// Construct an empty cache.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record that the host has this plugin's screen on display right now.
    pub fn mark_shown(&self) {
        *self.shown_at.lock() = Some(Instant::now());
    }

    /// Whether the host found this plugin's screen on display within `window`.
    #[must_use]
    pub fn shown_within(&self, window: Duration) -> bool {
        self.shown_at.lock().is_some_and(|at| at.elapsed() <= window)
    }

    /// Replace the cached buffer.
    pub fn put(&self, buf: WireBuffer) {
        *self.inner.lock() = Some(buf);
    }

    /// Fold one painted frame into the Esc watch: `frame` hashes the buffer and
    /// `keys_through` is the last key generation written to the plugin before
    /// the render was requested (`None` before any key), so the frame reflects
    /// every key up to it.
    pub fn observe_frame(&self, frame: u64, keys_through: Option<u64>) {
        self.back_watch.lock().frame(frame, keys_through);
    }

    /// Record a key the host is about to send and answer whether to send it.
    ///
    /// `false` means the plugin has left [`ESC_UNANSWERED_LIMIT`] Esc presses in
    /// a row unanswered, so this Esc should go to the host instead.
    #[must_use]
    pub fn admit_key(&self, key: &ainb_plugin_protocol::params::KeyEvent, generation: u64) -> bool {
        use ainb_plugin_protocol::params::{KeyCode, KeyKind};
        let mut watch = self.back_watch.lock();
        if key.code != KeyCode::Esc {
            watch.other_key();
            return true;
        }
        // An auto-repeat or a release is not a fresh request to go back.
        if key.kind != KeyKind::Press {
            return true;
        }
        watch.esc(generation) == EscVerdict::Deliver
    }

    /// Pop the cached buffer (returns `None` if nothing cached).
    pub fn try_take(&self) -> Option<WireBuffer> {
        self.inner.lock().take()
    }

    /// Latch the plugin's text-capture state from its latest frame. Persistent:
    /// survives `try_take` so the host can read it on any keystroke.
    pub fn set_captures_text(&self, capturing: bool) {
        self.captures_text.store(capturing, std::sync::atomic::Ordering::Release);
    }

    /// Read the plugin's text-capture state as of its last painted frame.
    #[must_use]
    pub fn captures_text(&self) -> bool {
        self.captures_text.load(std::sync::atomic::Ordering::Acquire)
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
    /// Forward a `plugin/handle_action` notification: run one of the
    /// plugin's actions by id.
    HandleAction(HandleActionParams),
    /// Send `plugin/shutdown` and reap the process.
    Shutdown,
    /// Clear quarantine + allow respawn.
    Reload,
    /// Test aid: force the child to be `kill -9`'d to exercise
    /// crash-recovery code paths. Hidden from rustdoc — this is not
    /// part of the public API surface.
    #[doc(hidden)]
    InjectKill,
    /// Test aid: run the idle-reap decision now, as if the idle window had
    /// already passed, and reply whether the plugin was reaped. Does not count
    /// as use. Lets a test drive the reap deterministically instead of waiting
    /// on the 5 s idle tick. Hidden from rustdoc.
    #[doc(hidden)]
    ReapIfIdle {
        /// `true` when the check reaped the plugin.
        reply: oneshot::Sender<bool>,
        /// Judge the real time since the last use against the idle window,
        /// instead of treating the window as passed.
        honour_window: bool,
    },
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
///
/// Bounded at [`INPUT_INBOX_CAPACITY`], dropping the oldest key when full
/// (#1087): a plugin that stops reading its stdin stalls the task's frame
/// write, and an unbounded channel then kept every later keystroke in memory.
pub type KeyInbox = DropOldestSender<HandleKeyParams>;

/// Priority side-channel reserved for `plugin/handle_mouse` notifications.
///
/// Mirrors [`KeyInbox`]: a dedicated channel drained ahead of the main
/// [`Inbox`] in the task's `biased;` select, so a click or scroll on a
/// plugin screen can't queue behind a backlog of `HandleEvent` chunks. Bounded
/// the same way.
pub type MouseInbox = DropOldestSender<HandleMouseParams>;

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
    /// One low-frequency probe has been scheduled after the cap tripped.
    /// Prevents a persistent non-animation retry from spinning at frame rate.
    probe_pending: bool,
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
            // reset re-arms the animation. An idle-tick probe consumed this
            // render, so a later tick may schedule the next bounded retry.
            self.probe_pending = false;
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
        self.probe_pending = false;
    }

    /// Permit exactly one render on the runtime's low-frequency idle tick
    /// after a runaway stream was capped. This lets backpressured plugins
    /// retry retained work without restoring a frame-rate redraw loop.
    const fn schedule_probe(&mut self) -> bool {
        if self.tripped && !self.probe_pending {
            self.probe_pending = true;
            true
        } else {
            false
        }
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
#[allow(clippy::too_many_arguments)] // wiring fan-out: maps + registries the task shares with Runtime
pub fn spawn(
    plugin: Arc<RegisteredPlugin>,
    snapshots: SnapshotStore,
    inboxes: InboxMap,
    dirty: DirtyMap,
    event_streams: EventStreamRegistry,
    managed_subprocess: ManagedSubprocessRegistry,
    unix_sockets: UnixSocketRegistry,
    secret_backend: SharedSecretBackend,
    workspace_store: SharedWorkspaceStore,
    log_tap: LogTap,
    config: RuntimeConfig,
    handle: &tokio::runtime::Handle,
) -> (
    Inbox,
    KeyInbox,
    MouseInbox,
    RenderCache,
    Arc<parking_lot::RwLock<LifecycleState>>,
    Arc<std::sync::atomic::AtomicBool>,
) {
    let (tx, rx) = mpsc::unbounded_channel();
    let (key_tx, key_rx) = drop_oldest(INPUT_INBOX_CAPACITY);
    let (mouse_tx, mouse_rx) = drop_oldest(INPUT_INBOX_CAPACITY);
    let cache = RenderCache::new();
    let state = Arc::new(parking_lot::RwLock::new(LifecycleState::Idle));
    let render_wedged = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let task = PluginTask {
        plugin,
        snapshots,
        inboxes,
        dirty,
        event_streams,
        managed_subprocess,
        unix_sockets,
        secret_backend,
        workspace_store,
        // The task keeps a clone of its OWN inbox so the unix-socket read
        // loop can deliver `socket:<id>` frames back to this plugin.
        self_inbox: tx.clone(),
        log_tap,
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
        render_deadlines: HashMap::new(),
        render_key_stamps: HashMap::new(),
        last_key_written: None,
        render_wedged: render_wedged.clone(),
    };
    handle.spawn(task.run());
    (tx, key_tx, mouse_tx, cache, state, render_wedged)
}

/// Resolve to the outstanding render's request id once its deadline passes.
///
/// Pends forever when no render is in flight, so the `select!` arm holding it
/// simply never fires. Takes the deadline by value so the arm's future borrows
/// nothing from the task.
async fn await_render_deadline(deadline: Option<(u64, Instant)>) -> u64 {
    match deadline {
        Some((id, at)) => {
            tokio::time::sleep(at.saturating_duration_since(Instant::now())).await;
            id
        }
        None => std::future::pending().await,
    }
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
    /// Set once a frame write outlived `frame_write_timeout`, so the warning
    /// is logged once per process rather than once per queued write.
    write_timed_out: bool,
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
    /// Cap-gated event-stream registry (shared with `Runtime`). The task
    /// inserts on `host/event_stream_subscribe`, removes on
    /// `host/event_stream_cancel`, and drops every owned stream on
    /// teardown so no events leak to a dead/quarantined process.
    event_streams: EventStreamRegistry,
    /// Cap-gated managed-subprocess registry (shared with `Runtime`). The
    /// task spawns on `host/spawn_managed_subprocess` and kills every
    /// child this plugin owns on teardown so no host-supervised process
    /// outlives the plugin that requested it.
    managed_subprocess: ManagedSubprocessRegistry,
    /// Cap-gated unix-socket dial registry (shared with `Runtime`). The
    /// task dials on `host/unix_socket_dial`, writes on
    /// `host/unix_socket_send`, closes on `host/unix_socket_close`, and
    /// drops every socket this plugin owns on teardown so no `socket:<id>`
    /// frame leaks to a dead/quarantined process.
    unix_sockets: UnixSocketRegistry,
    /// Shared platform secret backend (DI). `host/secret_store_get` reads
    /// through this; production uses the macOS Keychain / linux stub, tests
    /// inject an in-memory double.
    secret_backend: SharedSecretBackend,
    /// Shared host workspace store (DI). The `host/workspace_*` caps read /
    /// write the active+default switch state in `~/.agents-in-a-box/hangar/state.toml`
    /// and broadcast `WorkspaceChanged` through this; tests inject a double.
    workspace_store: SharedWorkspaceStore,
    /// Clone of this task's own [`Inbox`]. The unix-socket read loop holds
    /// it so reads from a dialled socket are delivered back to this plugin
    /// as `Command::HandleEvent` under topic `socket:<stream_id>`.
    self_inbox: Inbox,
    /// Optional host-side log tap. When installed, every `host/log` line
    /// is forwarded to it (sentinel capture for the CTS anti-cheat path).
    log_tap: LogTap,
    config: RuntimeConfig,
    cache: RenderCache,
    state: Arc<parking_lot::RwLock<LifecycleState>>,
    rx: mpsc::UnboundedReceiver<Command>,
    /// Priority receiver for `plugin/handle_key` notifications. Drained
    /// before the main `rx` on every loop iteration so keystrokes
    /// (including Esc) are dispatched ahead of any backlog of
    /// `HandleEvent` chunks.
    key_rx: DropOldestReceiver<HandleKeyParams>,
    /// Priority receiver for `plugin/handle_mouse` notifications. Drained
    /// alongside `key_rx` (both ahead of the main `rx`) so mouse clicks
    /// and scrolls on a plugin screen aren't starved by a `HandleEvent`
    /// backlog.
    mouse_rx: DropOldestReceiver<HandleMouseParams>,
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
    /// Deadline per outstanding `plugin/render`.
    ///
    /// A plugin that blocks inside its own `render` never answers, and the SDK
    /// holds the per-plugin mutex across that future — so its inline
    /// `handle_key` dispatch is stuck behind it too. Without a deadline the
    /// reply oneshot simply never resolves: the host paints a stale frame and
    /// nothing anywhere reports that the screen has stopped responding.
    ///
    /// A map rather than one slot because the host does not serialise renders —
    /// it kicks one per dirty tick — so a plugin slower than the tick genuinely
    /// has several in flight, and each needs its own deadline.
    render_deadlines: HashMap<u64, Instant>,
    /// Per outstanding `plugin/render`, the last key generation written to the
    /// plugin before it was requested, so its frame can be judged against the
    /// Esc presses it reflects ([`BackWatch`]).
    render_key_stamps: HashMap<u64, Option<u64>>,
    /// The generation of the last key written to the plugin, `None` before any.
    last_key_written: Option<u64>,
    /// Set when a render blew its deadline or a frame write outlived
    /// `frame_write_timeout` (#1118), cleared when the plugin answers again.
    ///
    /// The host reads this (`RuntimeHandle::render_wedged`) to stop forwarding
    /// `q`/`Esc` into a plugin that cannot service them, so the user can always
    /// leave the screen. Same escape hatch a dead plugin already gets.
    render_wedged: Arc<std::sync::atomic::AtomicBool>,
}

impl PluginTask {
    fn set_state(&self, s: LifecycleState) {
        *self.state.write() = s;
    }

    async fn run(mut self) {
        let mut idle_tick = tokio::time::interval(Duration::from_secs(5));
        idle_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            // Computed before the select so the watchdog arm's future borrows
            // nothing from `self` (the other arms hold mutable borrows).
            let next_deadline = self.earliest_render_deadline();
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
                _ = idle_tick.tick() => {
                    self.maybe_idle_reap().await;
                    self.retry_capped_redraw();
                }
                id = await_render_deadline(next_deadline) => {
                    self.expire_render(id);
                }
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
        let generation = params.generation;
        let json = serde_json::to_value(params).expect("HandleKeyParams is serializable");
        if self.send_notification(methods::PLUGIN_HANDLE_KEY, json).await.is_ok() {
            self.last_key_written = Some(generation);
        }
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

    /// The soonest deadline among the outstanding renders — the one the
    /// watchdog arm sleeps on. `None` when nothing is in flight.
    fn earliest_render_deadline(&self) -> Option<(u64, Instant)> {
        self.render_deadlines
            .iter()
            .min_by_key(|(_, at)| **at)
            .map(|(id, at)| (*id, *at))
    }

    /// Fail an outstanding `plugin/render` that blew its deadline.
    ///
    /// The plugin may still answer later; that late reply finds no ledger entry
    /// and is dropped. What matters is that the host gets an answer now, and
    /// that `render_wedged` flips so `q`/`Esc` stop being forwarded into a
    /// plugin that cannot service them.
    fn expire_render(&mut self, id: u64) {
        self.render_deadlines.remove(&id);
        self.render_key_stamps.remove(&id);
        let Some(Pending::Render(reply)) = self.ledger.remove(&id) else {
            return;
        };
        let budget = self.config.default_render_timeout;
        warn!(
            plugin = %self.plugin.id,
            ?budget,
            "plugin/render exceeded its budget; the screen is not repainting and \
             its keys are not being serviced — releasing q/Esc to the host"
        );
        self.render_wedged.store(true, std::sync::atomic::Ordering::Release);
        let _ = reply.send(RenderOutcome::RuntimeError(format!(
            "render exceeded {budget:?}"
        )));
    }

    async fn handle_command(&mut self, cmd: Command) {
        // Every command is use, except the test aid that asks whether the
        // plugin would be reaped as idle, and a delivery on a latest-state
        // topic (#1063 review): the host's once-a-second card clock is such a
        // topic, and counting it as use would keep a plugin that nobody looks
        // at alive forever, undoing the #1040 reap. A plugin on screen stays
        // alive through `shown_within`, not through its deliveries.
        let is_use = match &cmd {
            Command::ReapIfIdle { .. } => false,
            Command::HandleEvent { topic, .. } => !self
                .plugin
                .manifest
                .subscribes
                .latest_state
                .iter()
                .any(|latest| latest == topic.as_str()),
            _ => true,
        };
        if is_use {
            self.last_used = Instant::now();
        }
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
                } else {
                    self.render_deadlines
                        .insert(id, Instant::now() + self.config.default_render_timeout);
                    self.render_key_stamps.insert(id, self.last_key_written);
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
            Command::HandleAction(params) => {
                self.last_used = Instant::now();
                self.redraw_governor.reset();
                // Unlike a key, an action can name a plugin whose screen is
                // not showing (a palette command, another renderer's click),
                // so it spawns the plugin rather than dropping.
                if let Err(e) = self.ensure_running().await {
                    debug!(plugin = %self.plugin.id, error = %e, "handle_action dropped (spawn failed)");
                    return;
                }
                let json =
                    serde_json::to_value(params).expect("HandleActionParams is serializable");
                let _ = self.send_notification(methods::PLUGIN_HANDLE_ACTION, json).await;
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
            Command::ReapIfIdle {
                reply,
                honour_window,
            } => {
                let reaped = self.idle_reap(!honour_window).await;
                let _ = reply.send(reaped);
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

    /// Bring the subprocess up and complete `plugin/init`.
    ///
    /// Every failure inside settles the lifecycle state and counts against the
    /// circuit breaker. That used to be true of no failure at all: the whole
    /// function returned with the state still `Spawning`, which
    /// `ensure_running` treats as spawnable, and the quarantine check lived
    /// only on `handle_exit` — the path for a process that started and then
    /// died. A plugin that could never start (binary deleted by an upgrade
    /// under a running TUI) or that started but never completed init (ABI
    /// mismatch after a half-upgrade) was therefore retried on every later
    /// render, CLI, or action kick, forever, without ever tripping the breaker.
    async fn spawn_and_init(&mut self) -> Result<(), RuntimeError> {
        self.set_state(LifecycleState::Spawning);
        match self.spawn_and_init_inner().await {
            Ok(()) => Ok(()),
            Err(e) => {
                warn!(
                    plugin = %self.plugin.id,
                    binary = %self.plugin.binary_path.display(),
                    error = %e,
                    "plugin spawn failed"
                );
                // A failure after the exec (init timeout, ABI rejection) leaves
                // a live child behind. Reap it here rather than letting the
                // next attempt overwrite `self.child` and strand it.
                if let Some(cs) = self.child.take() {
                    cs.stderr_drain.abort();
                    cs.stdout_reader.abort();
                }
                self.record_failure();
                if self.is_quarantine_due() {
                    self.set_state(LifecycleState::Quarantined);
                    error!(
                        plugin = %self.plugin.id,
                        "quarantined after {} failed spawns",
                        self.failures.len()
                    );
                } else {
                    self.set_state(LifecycleState::Idle);
                }
                Err(e)
            }
        }
    }

    /// Raw spawn + init. Returns the first error; `spawn_and_init` owns the
    /// state transition, failure accounting, and child cleanup for all of them.
    async fn spawn_and_init_inner(&mut self) -> Result<(), RuntimeError> {
        let mut child = spawn_plugin(&self.plugin.binary_path)?;
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
            write_timed_out: false,
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
            // The plugin is this process's direct child (#1040).
            host: Some(ainb_plugin_protocol::params::PluginHost {
                kind: self.config.host_kind.to_string(),
                pid: std::process::id(),
            }),
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
        self.write_to_child(&body).await
    }

    async fn send_notification(&mut self, method: &str, params: Value) -> Result<(), RuntimeError> {
        let body = build_notification(method, params)?;
        self.write_to_child(&body).await
    }

    /// Write one frame to the plugin, bounded by `frame_write_timeout` (#1118).
    ///
    /// A plugin that stops reading its stdin fills the pipe, and an unbounded
    /// write parks this task for good: no render deadline fires, no key is
    /// drained, and the host keeps forwarding Esc into a plugin that will never
    /// take it. A write past the bound is a dead plugin. The task flags it
    /// wedged, so the host takes `q` and Esc at once, and kills the child, so
    /// the closed pipe drops it through the same `handle_exit` a crash takes.
    /// The half-written frame goes with the process.
    async fn write_to_child(&mut self, body: &[u8]) -> Result<(), RuntimeError> {
        let bound = self.config.frame_write_timeout;
        let plugin = self.plugin.id.clone();
        let cs = self.child.as_mut().ok_or_else(|| RuntimeError::ProcessExited(plugin.clone()))?;
        if let Ok(written) = tokio::time::timeout(bound, write_frame(&mut cs.stdin, body)).await {
            written?;
            return Ok(());
        }
        if !cs.write_timed_out {
            cs.write_timed_out = true;
            warn!(
                plugin = %plugin,
                ?bound,
                "a frame write to the plugin did not finish: it is not reading its stdin, \
                 so it is treated as dead"
            );
        }
        self.render_wedged.store(true, std::sync::atomic::Ordering::Release);
        let _ = cs.child.start_kill();
        // Drop the plugin now rather than on stdout EOF: a helper the plugin
        // spawned can hold the pipe open, and then EOF never comes and every
        // queued key burns another full bound. The reader owns the only
        // inbound sender, so aborting it ends the inbound channel and the next
        // loop turn reads `Eof` into `handle_exit`, which aborts it again
        // harmlessly.
        cs.stdout_reader.abort();
        Err(RuntimeError::ProcessExited(plugin))
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
                self.handle_host_notification(&method, params).await;
            }
        }
    }

    async fn handle_response(&mut self, id: u64, result: Result<Value, RpcError>) {
        // ANY response is proof the plugin is reading and answering again, which
        // is exactly what `render_wedged` claims it is not doing. Lift it here
        // rather than only on a matched render: a render the watchdog already
        // expired has had its ledger entry removed, so its late reply lands on
        // the stray path below — and a plugin that is merely slow would
        // otherwise have q/Esc diverted away from it forever.
        self.render_wedged.store(false, std::sync::atomic::Ordering::Release);
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
                            // Latch the plugin's text-capture state from this
                            // frame so the host can suppress its global
                            // single-char shortcuts while the plugin's input is
                            // focused (8hx). Persistent — survives try_take.
                            self.cache.set_captures_text(rr.captures_text);
                            if let Some(keys_through) = self.render_key_stamps.remove(&id) {
                                self.cache.observe_frame(frame_hash(&rr.buffer), keys_through);
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
                // Only THIS render's answer disarms the watchdog. The host does
                // not serialise renders — it kicks one per dirty tick — so a
                // plugin slower than the tick has several outstanding at once.
                // Clearing unconditionally let a late reply to an older render
                // disarm the deadline of one still in flight, and lift the wedge
                // while the plugin was still blocked.
                // Disarm only THIS render's deadline — a late reply to an older
                // one must not disarm a request still in flight. The wedge is
                // lifted for every response, in `handle_response`.
                self.render_deadlines.remove(&id);
                self.render_key_stamps.remove(&id);
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
            methods::HOST_EVENT_STREAM_SUBSCRIBE => self.host_event_stream_subscribe(params),
            methods::HOST_SPAWN_MANAGED_SUBPROCESS => self.host_spawn_managed_subprocess(params),
            methods::HOST_UNIX_SOCKET_DIAL => self.host_unix_socket_dial(params).await,
            methods::HOST_SECRET_STORE_GET => self.host_secret_store_get(params),
            methods::HOST_WORKSPACE_LIST => Ok(list_logic(self.workspace_store.as_ref())),
            methods::HOST_WORKSPACE_GET_ACTIVE => {
                Ok(get_active_logic(self.workspace_store.as_ref()))
            }
            methods::HOST_WORKSPACE_SET_ACTIVE => self.host_workspace_set_active(params),
            methods::HOST_WORKSPACE_SET_DEFAULT => self.host_workspace_set_default(params),
            methods::HOST_WORKSPACE_CREATE => self.host_workspace_create(params),
            methods::HOST_WORKSPACE_DELETE => self.host_workspace_delete(params),
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
        if self.child.is_some() {
            debug!(plugin = %self.plugin.id, id, bytes = body.len(), "host->plugin response: writing");
            match self.write_to_child(&body).await {
                Ok(()) => {
                    debug!(plugin = %self.plugin.id, id, "host->plugin response: write_frame OK");
                }
                Err(e) => warn!(plugin = %self.plugin.id, id, "write response: {e}"),
            }
        } else {
            warn!(plugin = %self.plugin.id, id, "host->plugin response: child missing, dropping response");
        }
    }

    /// The `event_bus` grant every snapshot-bus call needs, for this topic;
    /// `-32001` without it. See [`event_bus_covers`] for the list form.
    fn require_event_bus(&self, topic: &str) -> Result<(), RpcError> {
        require_event_bus_grant(&self.plugin.manifest.capabilities.event_bus, topic)
    }

    fn host_snapshot_get(&self, params: Value) -> Result<Value, RpcError> {
        let p: SnapshotGetParams =
            serde_json::from_value(params).map_err(|e| RpcError::invalid_params(e.to_string()))?;
        self.require_event_bus(&p.topic)?;
        let topic = Topic::from(p.topic);
        let (payload, version) = match self.snapshots.get(&topic) {
            Some((p, v, _publisher)) => (Some(p), v),
            None => (None, 0),
        };
        let res = SnapshotGetResult { payload, version };
        Ok(serde_json::to_value(res).expect("SnapshotGetResult serializable"))
    }

    fn host_snapshot_subscribe(&self, params: Value) -> Result<Value, RpcError> {
        let p: SnapshotSubscribeParams =
            serde_json::from_value(params).map_err(|e| RpcError::invalid_params(e.to_string()))?;
        self.require_event_bus(&p.topic)?;
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

    /// Handle `host/event_stream_subscribe`.
    ///
    /// Cap gate (two stages):
    /// 1. The `event_stream_subscribe` grant must be present at all
    ///    (`is_granted`); otherwise reject with `-32001`.
    /// 2. The requested topic must satisfy the grant's allow-list
    ///    (list form = topic-prefix whitelist; bool-true = wildcard);
    ///    otherwise reject with `-32001` carrying the attempted topic in
    ///    `data.topic`.
    ///
    /// On success the host mints an opaque, unforgeable `stream_id` and
    /// records the stream; events for the topic are thereafter pushed to
    /// the plugin under `stream:<stream_id>`.
    fn host_event_stream_subscribe(&self, params: Value) -> Result<Value, RpcError> {
        let p: EventStreamSubscribeParams =
            serde_json::from_value(params).map_err(|e| RpcError::invalid_params(e.to_string()))?;
        let grant = &self.plugin.manifest.capabilities.event_stream_subscribe;
        if !grant.is_granted() {
            return Err(RpcError::capability_denied("event_stream_subscribe"));
        }
        if !topic_allowed(grant.allow_list(), &p.topic) {
            return Err(RpcError::capability_denied("event_stream_subscribe")
                .with_data(serde_json::json!({ "topic": p.topic })));
        }
        let topic = Topic::from(p.topic);
        // Position the stream at the topic's current version (or the
        // requested resume point). The first event the plugin observes
        // is the next publish after this point.
        let version = self.snapshots.get(&topic).map_or(0, |(_, v, _)| v);
        let stream_id = self.event_streams.subscribe(self.plugin.id.clone(), topic);
        let res = EventStreamSubscribeResult {
            stream_id,
            version: p.since_version.unwrap_or(version),
        };
        Ok(serde_json::to_value(res).expect("EventStreamSubscribeResult serializable"))
    }

    /// Handle `host/spawn_managed_subprocess`.
    ///
    /// Cap gate (three stages, all returning before any fork):
    /// 1. The `spawn_managed_subprocess` grant must be present at all
    ///    (`is_granted`); otherwise reject with `-32001`.
    /// 2. The grant MUST be list-form. A bool-true grant is a request for
    ///    an unrestricted "spawn anything" capability and is rejected with
    ///    `-32003 MANIFEST_VALIDATION` — there is no legitimate wildcard
    ///    spawn (defends shared dev boxes against arbitrary exec).
    /// 3. The requested `bin` must be on the allow-list (exact match);
    ///    otherwise reject with `-32001` carrying the attempted path in
    ///    `data.bin`.
    ///
    /// On success the host spawns the child under the leak guard, records
    /// it against this plugin (so it's reaped on teardown), and returns
    /// the opaque handle + pid.
    fn host_spawn_managed_subprocess(&self, params: Value) -> Result<Value, RpcError> {
        let p: SpawnManagedSubprocessParams =
            serde_json::from_value(params).map_err(|e| RpcError::invalid_params(e.to_string()))?;
        let grant = &self.plugin.manifest.capabilities.spawn_managed_subprocess;
        if !grant.is_granted() {
            return Err(RpcError::capability_denied("spawn_managed_subprocess"));
        }
        // List-form mandatory: a bool-true grant is rejected outright.
        let Some(allow) = grant.allow_list() else {
            return Err(RpcError::manifest_validation(
                "spawn_managed_subprocess must be a list-form allow-list of \
                 binaries; bool-true grant rejected",
            ));
        };
        if !crate::managed_subprocess::bin_allowed(allow, &p.bin) {
            return Err(RpcError::capability_denied("spawn_managed_subprocess")
                .with_data(serde_json::json!({ "bin": p.bin })));
        }
        let spawned = self
            .managed_subprocess
            .spawn(
                self.plugin.id.clone(),
                &p.bin,
                &p.argv,
                &p.env_allowlist,
                p.cwd.as_deref(),
            )
            .map_err(|e| {
                RpcError::new(ainb_plugin_protocol::errors::INVALID_PARAMS, e.to_string())
            })?;
        let res = SpawnManagedSubprocessResult {
            handle: spawned.handle,
            pid: spawned.pid,
        };
        Ok(serde_json::to_value(res).expect("SpawnManagedSubprocessResult serializable"))
    }

    /// Handle `host/unix_socket_dial`.
    ///
    /// Cap gate (three stages, all returning before any `connect`):
    /// 1. The `unix_socket_dial` grant must be present at all
    ///    (`is_granted`); otherwise reject with `-32001`.
    /// 2. The grant MUST be list-form. A bool-true grant is a request for
    ///    an unrestricted "dial any socket" capability and is rejected with
    ///    `-32003 MANIFEST_VALIDATION` — there is no legitimate wildcard
    ///    unix dial (defends shared dev boxes against arbitrary `AF_UNIX`
    ///    abuse).
    /// 3. The requested `path` must, after host-side env/`~` expansion and
    ///    symlink canonicalization, exactly match a canonicalized
    ///    allow-list entry; otherwise reject with `-32001` carrying the
    ///    attempted path in `data.path`. Canonicalization is what defeats
    ///    a symlink that resolves outside the whitelist.
    ///
    /// On success the host dials the socket, mints an opaque `stream_id`,
    /// spawns a read loop that re-emits reads under `socket:<stream_id>`,
    /// and returns the id.
    async fn host_unix_socket_dial(&self, params: Value) -> Result<Value, RpcError> {
        let p: UnixSocketDialParams =
            serde_json::from_value(params).map_err(|e| RpcError::invalid_params(e.to_string()))?;
        let grant = &self.plugin.manifest.capabilities.unix_socket_dial;
        if !grant.is_granted() {
            return Err(RpcError::capability_denied("unix_socket_dial"));
        }
        // List-form mandatory: a bool-true grant is rejected outright.
        let Some(allow) = grant.allow_list() else {
            return Err(RpcError::manifest_validation(
                "unix_socket_dial must be a list-form allow-list of socket \
                 paths; bool-true grant rejected",
            ));
        };
        if !path_allowed(allow, &p.path) {
            return Err(RpcError::capability_denied("unix_socket_dial")
                .with_data(serde_json::json!({ "path": p.path })));
        }
        let expanded = crate::unix_socket::expand_path(&p.path);
        // The plugin's host render-dirty flag. Threaded into the dial read loop
        // so each forwarded daemon socket event (snapshot result / pushed
        // `hangar/event`) marks the plugin dirty and the host re-paints once the
        // data lands — without it an async snapshot sits unpainted (blank board)
        // until an unrelated keystroke happens to mark dirty.
        let render_dirty = self
            .dirty
            .read()
            .get(&self.plugin.id)
            .cloned()
            .unwrap_or_else(|| Arc::new(std::sync::atomic::AtomicBool::new(false)));
        let stream_id = self
            .unix_sockets
            .dial(
                self.plugin.id.clone(),
                &expanded,
                self.self_inbox.clone(),
                render_dirty,
            )
            .await
            .map_err(|e| {
                RpcError::new(ainb_plugin_protocol::errors::INVALID_PARAMS, e.to_string())
            })?;
        let res = UnixSocketDialResult { stream_id };
        Ok(serde_json::to_value(res).expect("UnixSocketDialResult serializable"))
    }

    /// Handle `host/secret_store_get`.
    ///
    /// A thin shell over [`secret_store_get_logic`]: it decodes the
    /// `(scope, key)` params, then delegates the cap gate, scope parse,
    /// injected-backend lookup, and base64 encode to the testable logic
    /// function. The `secrets:read` grant is enforced there (grant present +
    /// `key` on the list-form allow-list), never by omitting the method from
    /// the dispatcher.
    ///
    /// On a permitted request the platform backend performs the lookup:
    /// macOS reads the login Keychain; the linux stub returns `-32005`. A
    /// miss returns `-32004`, a locked backend `-32006`, denied access
    /// `-32007`. The secret is base64-encoded into `value` so it never rides
    /// the wire as a raw byte array.
    fn host_secret_store_get(&self, params: Value) -> Result<Value, RpcError> {
        let p: SecretStoreGetParams =
            serde_json::from_value(params).map_err(|e| RpcError::invalid_params(e.to_string()))?;
        let grant = &self.plugin.manifest.capabilities.secrets_read;
        secret_store_get_logic(grant, self.secret_backend.as_ref(), &p)
    }

    /// Handle `host/workspace_set_active`.
    ///
    /// Delegates to [`set_active_logic`], which gates on `workspace:write`
    /// (`-32001` when the grant is absent, before any store hit), validates the
    /// id against the catalogue (`-32602` for an unknown id), writes
    /// `active_workspace` to `state.toml`, and broadcasts `WorkspaceChanged` so
    /// subscribed plugins re-fetch.
    fn host_workspace_set_active(&self, params: Value) -> Result<Value, RpcError> {
        let p: WorkspaceSetActiveParams =
            serde_json::from_value(params).map_err(|e| RpcError::invalid_params(e.to_string()))?;
        let grant = &self.plugin.manifest.capabilities.workspace_write;
        set_active_logic(grant, self.workspace_store.as_ref(), &p.workspace_id)
    }

    /// Handle `host/workspace_set_default`.
    ///
    /// Delegates to [`set_default_logic`] (gated by `workspace:write`).
    /// Validates the id and writes `default_workspace` to `state.toml`; never
    /// changes the active workspace and emits no event (a default change is
    /// silent).
    fn host_workspace_set_default(&self, params: Value) -> Result<Value, RpcError> {
        let p: WorkspaceSetDefaultParams =
            serde_json::from_value(params).map_err(|e| RpcError::invalid_params(e.to_string()))?;
        let grant = &self.plugin.manifest.capabilities.workspace_write;
        set_default_logic(grant, self.workspace_store.as_ref(), &p.workspace_id)
    }

    /// Handle `host/workspace_create`.
    ///
    /// Delegates to [`create_logic`] (gated by `workspace:write`): creates the
    /// workspace + owner member in the daemon store, folds it into the host
    /// catalogue, and returns the new row (`active`/`default` false).
    fn host_workspace_create(&self, params: Value) -> Result<Value, RpcError> {
        let p: WorkspaceCreateParams =
            serde_json::from_value(params).map_err(|e| RpcError::invalid_params(e.to_string()))?;
        let grant = &self.plugin.manifest.capabilities.workspace_write;
        create_logic(grant, self.workspace_store.as_ref(), &p.slug, &p.name)
    }

    /// Handle `host/workspace_delete`.
    ///
    /// Delegates to [`delete_logic`] (gated by `workspace:write`): refuses the
    /// effective-active + last workspace, then tears the workspace down in the
    /// daemon store and drops it from the host catalogue.
    fn host_workspace_delete(&self, params: Value) -> Result<Value, RpcError> {
        let p: WorkspaceDeleteParams =
            serde_json::from_value(params).map_err(|e| RpcError::invalid_params(e.to_string()))?;
        let grant = &self.plugin.manifest.capabilities.workspace_write;
        delete_logic(grant, self.workspace_store.as_ref(), &p.workspace_id)
    }

    async fn handle_host_notification(&self, method: &str, params: Value) {
        match method {
            methods::HOST_SNAPSHOT_PUBLISH => {
                // A notification has no error reply, so a publish without the
                // grant for its topic is dropped rather than answered with
                // `-32001`.
                let Ok(p) = serde_json::from_value::<SnapshotPublishParams>(params) else {
                    warn!(plugin = %self.plugin.id, "bad snapshot publish");
                    return;
                };
                if self.require_event_bus(&p.topic).is_err() {
                    warn!(plugin = %self.plugin.id, topic = %p.topic, "snapshot publish denied: no event_bus grant for the topic");
                    return;
                }
                // `fleet.` topics are host-publish-only: a grant lets a plugin
                // read one, never write it, so a subscriber can trust every
                // delivery on it came from the host (#1089).
                if is_host_publish_only(&p.topic) {
                    warn!(plugin = %self.plugin.id, topic = %p.topic, "snapshot publish denied: the topic is host-publish-only");
                    return;
                }
                // `ui.state` is one view per plugin: a bare publish is stored
                // under the publisher's own `ui.state/<id>`, and a publish to
                // another plugin's slot is refused, so two plugins can never
                // overwrite each other's view.
                let own_ui_state =
                    ainb_plugin_protocol::topics::ui_state_topic(self.plugin.id.as_str());
                let topic = if p.topic == ainb_plugin_protocol::topics::UI_STATE {
                    Topic::from(own_ui_state)
                } else if p.topic.starts_with(ainb_plugin_protocol::topics::UI_STATE_PREFIX)
                    && p.topic != own_ui_state
                {
                    warn!(plugin = %self.plugin.id, topic = %p.topic, "ui.state publish for another plugin refused");
                    return;
                } else {
                    Topic::from(p.topic)
                };
                let payload = p.payload;
                // Stamp the publisher from the wire connection this task
                // owns — the plugin can't self-report a different id.
                let _ =
                    self.snapshots.publish(topic.clone(), payload.clone(), self.plugin.id.clone());
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
            methods::HOST_EVENT_STREAM_CANCEL => {
                let Ok(p) = serde_json::from_value::<EventStreamCancelParams>(params) else {
                    warn!(plugin = %self.plugin.id, "bad event_stream_cancel payload");
                    return;
                };
                let removed = self.event_streams.cancel(&self.plugin.id, &p.stream_id);
                debug!(
                    plugin = %self.plugin.id,
                    stream_id = %p.stream_id,
                    removed,
                    "event_stream_cancel"
                );
            }
            methods::HOST_UNIX_SOCKET_SEND => {
                let Ok(p) = serde_json::from_value::<UnixSocketSendParams>(params) else {
                    warn!(plugin = %self.plugin.id, "bad unix_socket_send payload");
                    return;
                };
                let ok = self.unix_sockets.send(&self.plugin.id, &p.stream_id, &p.bytes).await;
                debug!(
                    plugin = %self.plugin.id,
                    stream_id = %p.stream_id,
                    ok,
                    "unix_socket_send"
                );
            }
            methods::HOST_UNIX_SOCKET_CLOSE => {
                let Ok(p) = serde_json::from_value::<UnixSocketCloseParams>(params) else {
                    warn!(plugin = %self.plugin.id, "bad unix_socket_close payload");
                    return;
                };
                let removed = self.unix_sockets.close(&self.plugin.id, &p.stream_id);
                debug!(
                    plugin = %self.plugin.id,
                    stream_id = %p.stream_id,
                    removed,
                    "unix_socket_close"
                );
            }
            methods::HOST_LOG => {
                let Ok(p) = serde_json::from_value::<LogParams>(params) else {
                    warn!(plugin = %self.plugin.id, "bad log payload");
                    return;
                };
                // Forward to an installed log tap (sentinel capture for
                // CTS anti-cheat) before the normal tracing emit.
                if let Some(tap) = self.log_tap.read().as_ref() {
                    tap(&p);
                }
                info!(plugin = %self.plugin.id, level = ?p.level, "{}", p.message);
            }
            other => debug!(plugin = %self.plugin.id, "ignoring notification: {other}"),
        }
    }

    /// Answer every request still waiting on the plugin with `why`, so a
    /// caller sees a runtime error instead of a dropped channel.
    fn fail_pending(&mut self, why: &str) {
        self.render_key_stamps.clear();
        let pending: Vec<(u64, Pending)> = self.ledger.drain().collect();
        for (_, p) in pending {
            match p {
                Pending::Render(r) => {
                    let _ = r.send(RenderOutcome::RuntimeError(why.into()));
                }
                Pending::Cli(r) => {
                    let _ = r.send(CliOutcome::RuntimeError(why.into()));
                }
                Pending::Action(r) => {
                    let _ = r.send(ActionOutcome::RuntimeError(why.into()));
                }
                Pending::Init => {}
            }
        }
    }

    async fn handle_exit(&mut self) {
        warn!(plugin = %self.plugin.id, "plugin exited / pipe closed");
        self.fail_pending("plugin exited");
        if let Some(cs) = self.child.take() {
            cs.stderr_drain.abort();
            cs.stdout_reader.abort();
        }
        // Process is gone (crash / broken pipe): drop subscriptions and
        // streams so no further events are routed to a dead subscriber,
        // and reap every managed child this plugin owned so no
        // host-supervised process outlives its requester.
        self.snapshots.unsubscribe_all(&self.plugin.id);
        self.forget_ui_state();
        self.event_streams.drop_plugin(&self.plugin.id);
        self.managed_subprocess.kill_plugin(&self.plugin.id).await;
        self.unix_sockets.drop_plugin(&self.plugin.id);
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
                // spawn_and_init now settles the state itself on failure:
                // Quarantined once the breaker trips, else Idle. Only fill in
                // Idle for the states it does not settle, and NEVER downgrade
                // Quarantined — doing so re-armed `ensure_running` (which
                // treats Idle as spawnable) and re-exec'd a binary that can
                // never start, which is precisely the eager-plugin case
                // (session-reader) this breaker exists to stop.
                if !matches!(*self.state.read(), LifecycleState::Quarantined) {
                    self.set_state(LifecycleState::Idle);
                }
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
        self.idle_reap(false).await;
    }

    /// Reap the plugin if it is running, past its idle window (or
    /// `window_elapsed`, for [`Command::ReapIfIdle`]), and nothing keeps it
    /// alive. Returns whether it was reaped.
    async fn idle_reap(&mut self, window_elapsed: bool) -> bool {
        let elapsed = self.last_used.elapsed();
        let reap_threshold =
            Duration::from_secs(u64::from(self.plugin.manifest.lifecycle.idle_reap_secs))
                .max(self.config.idle_reap);
        // A subscription to a stream keeps the plugin: a delivery published
        // while it is reaped is gone. A latest-state topic does not, because the
        // respawned plugin reads the latest value again (#1040).
        let keeps_alive = self.plugin.manifest.subscribes.blocks_idle_reap()
            // A screen on display is in use even when nothing is typed or
            // published into it (#1053): an open Hangar screen must not
            // freeze after its idle window. The host marks it every tick.
            || self.cache.shown_within(SHOWN_GRACE);
        if matches!(*self.state.read(), LifecycleState::Running)
            && (window_elapsed || elapsed >= reap_threshold)
            && !keeps_alive
        {
            info!(plugin = %self.plugin.id, "idle reap (idle for {elapsed:?})");
            self.shutdown().await;
            self.set_state(LifecycleState::Idle);
            return true;
        }
        false
    }

    /// Repaint a capped plugin at most once per idle tick (five seconds).
    /// The next `redraw = true` remains capped, while plugins using the frame
    /// to retry retained I/O get another nonblocking enqueue attempt.
    fn retry_capped_redraw(&mut self) {
        if !matches!(*self.state.read(), LifecycleState::Running)
            || !self.redraw_governor.schedule_probe()
        {
            return;
        }
        if let Some(flag) = self.dirty.read().get(&self.plugin.id) {
            flag.store(true, std::sync::atomic::Ordering::Release);
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

    /// The plugin's `ui.state` view described a process that is gone. Left in
    /// the store, a restart would be drawn from it before publishing its own.
    fn forget_ui_state(&self) {
        self.snapshots.remove(&Topic::from(ainb_plugin_protocol::topics::ui_state_topic(
            self.plugin.id.as_str(),
        )));
    }

    async fn kill_child(&mut self) {
        // A request sent before the stop gets its answer here (#1053): without
        // this its sender drops with the ledger and the caller sees a bare
        // channel error, or with a later respawn, a reply routed to nothing.
        self.fail_pending("plugin reaped");
        if let Some(mut cs) = self.child.take() {
            let _ = cs.child.start_kill();
            let _ = cs.child.wait().await;
            cs.stderr_drain.abort();
            cs.stdout_reader.abort();
        }
        self.snapshots.unsubscribe_all(&self.plugin.id);
        self.forget_ui_state();
        // Drop every event stream the plugin held — the process is gone,
        // so further events would leak to a dead subscription.
        self.event_streams.drop_plugin(&self.plugin.id);
        // Reap every managed child this plugin requested — the plugin
        // that owns their lifecycle is gone.
        self.managed_subprocess.kill_plugin(&self.plugin.id).await;
        // Drop every dialled socket the plugin held — the process is gone,
        // so further `socket:<id>` frames would leak to a dead subscription.
        self.unix_sockets.drop_plugin(&self.plugin.id);
    }
}

/// How recently the host must have found a plugin's screen on display for the
/// plugin to count as in use. The host checks every render tick (250 ms), so
/// this only has to outlast a stalled frame or two.
const SHOWN_GRACE: Duration = Duration::from_secs(30);

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
    if c.event_stream_subscribe.is_granted() {
        out.push("event_stream_subscribe".into());
    }
    if c.spawn_managed_subprocess.is_granted() {
        out.push("spawn_managed_subprocess".into());
    }
    if c.unix_socket_dial.is_granted() {
        out.push("unix_socket_dial".into());
    }
    if c.secrets_read.is_granted() {
        out.push("secrets:read".into());
    }
    if c.workspace_write.is_granted() {
        out.push("workspace:write".into());
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

/// Topic prefixes the blanket `event_bus = true` grant does not cover: only a
/// list entry that names the topic does. `fleet.` topics carry what the TUI
/// host knows about every agent (`fleet.agent_status`: working directories,
/// pending tool input), so a plugin must ask for one by name to read it.
///
/// `sessions.refresh_request` is the one host-published topic deliberately
/// left outside this list: burndown publishes it too, to ask session-reader
/// for a rescan, so it stays writable under the blanket grant.
pub(crate) const EXPLICIT_GRANT_TOPIC_PREFIXES: &[&str] = &["fleet."];

/// Whether one topic allow-list entry covers `topic`. The single matcher
/// behind the `event_bus` list grant and the `event_stream_subscribe`
/// allow-list.
///
/// An entry ending in `*` covers every topic that starts with the text
/// before it, except a topic under [`EXPLICIT_GRANT_TOPIC_PREFIXES`]: no
/// wildcard names one, not a bare `*` and not `fleet.*` (#1101). Any other
/// entry covers exactly that topic, which is the only way to reach a
/// `fleet.` topic.
pub(crate) fn grant_entry_covers(entry: &str, topic: &str) -> bool {
    entry.strip_suffix('*').map_or(entry == topic, |prefix| {
        topic.starts_with(prefix)
            && !EXPLICIT_GRANT_TOPIC_PREFIXES.iter().any(|explicit| topic.starts_with(explicit))
    })
}

/// Whether only the host may publish on `topic`. Every
/// [`EXPLICIT_GRANT_TOPIC_PREFIXES`] topic is host state (`fleet.agent_status`
/// and its card clock), so a plugin publish there would spoof it for every
/// subscriber; the grant covers subscribe and read only.
fn is_host_publish_only(topic: &str) -> bool {
    EXPLICIT_GRANT_TOPIC_PREFIXES.iter().any(|prefix| topic.starts_with(prefix))
}

/// Whether an `event_bus` grant covers `topic`.
///
/// `true` covers every topic except those under
/// [`EXPLICIT_GRANT_TOPIC_PREFIXES`]. The list form is a topic allow-list
/// matched entry by entry with [`grant_entry_covers`]. So a plugin granted
/// `["ui.state*"]`, or the blanket `true`, can publish its own view and can
/// neither read nor subscribe to `fleet.agent_status` (#1038 review).
fn event_bus_covers(grant: &ainb_plugin_protocol::manifest::CapabilityGrant, topic: &str) -> bool {
    match grant {
        ainb_plugin_protocol::manifest::CapabilityGrant::Bool(granted) => {
            *granted
                && !EXPLICIT_GRANT_TOPIC_PREFIXES.iter().any(|prefix| topic.starts_with(prefix))
        }
        ainb_plugin_protocol::manifest::CapabilityGrant::List(entries) => {
            entries.iter().any(|entry| grant_entry_covers(entry, topic))
        }
    }
}

/// [`event_bus_covers`] as the `-32001` a snapshot-bus request answers.
fn require_event_bus_grant(
    grant: &ainb_plugin_protocol::manifest::CapabilityGrant,
    topic: &str,
) -> Result<(), RpcError> {
    if event_bus_covers(grant, topic) {
        Ok(())
    } else {
        Err(RpcError::capability_denied("event_bus"))
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
        event_bus_covers, require_event_bus_grant, resolve_against_existing_ancestor,
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
            let (key_tx, mut key_rx) =
                crate::inbox::drop_oldest::<HandleKeyParams>(crate::inbox::INPUT_INBOX_CAPACITY);

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

    #[test]
    fn governor_allows_one_idle_probe_after_cap() {
        let mut gov = RedrawGovernor::default();
        for _ in 0..=MAX_CONSECUTIVE_REDRAWS {
            gov.observe_redraw();
        }

        assert!(gov.tripped, "fixture must cross the redraw cap");
        assert!(gov.schedule_probe(), "idle tick schedules one retry render");
        assert!(
            !gov.schedule_probe(),
            "another idle tick cannot queue a second retry before render"
        );
        assert!(
            !gov.observe_redraw().honor,
            "probe runs once without restoring frame-rate redraw"
        );
        assert!(
            gov.schedule_probe(),
            "a later idle tick can retry retained work again"
        );
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

    /// #1038 review item 1: a plugin granted `event_bus = ["other.*"]` is
    /// denied `-32001` for `fleet.agent_status`, on `host/snapshot/get` and
    /// `host/snapshot/subscribe` alike (both call this one check), while a
    /// listed topic, a `*` prefix and the unconditional grant still pass.
    #[test]
    fn an_event_bus_list_grant_denies_an_unlisted_topic() {
        let other = CapabilityGrant::List(vec!["other.*".into()]);
        let err = require_event_bus_grant(&other, "fleet.agent_status").expect_err("denied");
        assert_eq!(err.code, ainb_plugin_protocol::errors::CAPABILITY_DENIED);
        assert!(event_bus_covers(&other, "other.topic"));

        let hangar = CapabilityGrant::List(vec!["fleet.agent_status".into(), "ui.state*".into()]);
        assert!(event_bus_covers(&hangar, "fleet.agent_status"));
        assert!(
            !event_bus_covers(&hangar, "fleet.agent_status.extra"),
            "an entry without `*` is exact"
        );
        assert!(event_bus_covers(&hangar, "ui.state"));
        assert!(event_bus_covers(&hangar, "ui.state/hangar-tui"));
        assert!(!event_bus_covers(&hangar, "sessions.refresh_request"));

        assert!(event_bus_covers(&CapabilityGrant::Bool(true), "ui.state"));
        assert!(
            !event_bus_covers(&CapabilityGrant::Bool(true), "fleet.agent_status"),
            "the blanket grant does not cover a fleet topic: learnings, session-reader and \
             witr hold it and never named the envelope"
        );
        assert!(!event_bus_covers(&CapabilityGrant::Bool(false), "ui.state"));

        // #1101: no wildcard names a fleet topic; only the exact name does.
        let star = CapabilityGrant::List(vec!["*".into()]);
        assert!(event_bus_covers(&star, "ui.state"));
        assert!(
            !event_bus_covers(&star, "fleet.agent_status"),
            "a bare `*` covers every topic the blanket grant does, and no more"
        );
        assert!(!event_bus_covers(
            &CapabilityGrant::List(vec!["fle*".into()]),
            "fleet.agent_status"
        ));
        let fleet_star = CapabilityGrant::List(vec!["fleet.*".into()]);
        assert!(
            !event_bus_covers(&fleet_star, "fleet.agent_status.clock"),
            "`fleet.*` is a wildcard too, so it names no fleet topic"
        );
        assert!(event_bus_covers(
            &CapabilityGrant::List(vec!["fleet.agent_status.clock".into()]),
            "fleet.agent_status.clock"
        ));
        assert!(!event_bus_covers(
            &CapabilityGrant::List(Vec::new()),
            "ui.state"
        ));
    }

    /// #1087: an Esc the plugin paints no change for is unanswered; after
    /// `ESC_UNANSWERED_LIMIT` of them in a row the next Esc goes to the host.
    #[test]
    fn back_watch_returns_to_host_after_unanswered_esc_presses() {
        use super::{BackWatch, ESC_UNANSWERED_LIMIT, EscVerdict};
        let mut watch = BackWatch::default();
        watch.frame(7, None);
        for n in 1..=u64::from(ESC_UNANSWERED_LIMIT) {
            assert_eq!(watch.esc(n), EscVerdict::Deliver, "esc {n}");
            watch.frame(7, Some(n));
        }
        assert_eq!(watch.esc(99), EscVerdict::ReturnToHost);
        assert_eq!(
            watch.esc(100),
            EscVerdict::Deliver,
            "an eject starts afresh"
        );
    }

    /// A plugin that pops one level per Esc changes its frame every time and is
    /// never ejected, however many levels it has.
    #[test]
    fn back_watch_keeps_a_plugin_that_answers_each_esc() {
        use super::{BackWatch, EscVerdict};
        let mut watch = BackWatch::default();
        watch.frame(0, None);
        for n in 1..=u64::from(super::ESC_PRESS_CEILING) {
            assert_eq!(watch.esc(n), EscVerdict::Deliver, "esc {n}");
            watch.frame(n, Some(n));
        }
    }

    /// Esc presses faster than the plugin paints carry no evidence, and a frame
    /// requested before the Esc reached the plugin says nothing about it.
    #[test]
    fn back_watch_needs_a_frame_after_the_esc() {
        use super::{BackWatch, EscVerdict};
        let mut watch = BackWatch::default();
        watch.frame(7, None);
        for n in 1..=u64::from(super::ESC_PRESS_CEILING) {
            assert_eq!(watch.esc(n), EscVerdict::Deliver, "esc {n}");
            watch.frame(7, Some(n - 1));
        }
    }

    /// Any other key ends the streak.
    #[test]
    fn back_watch_resets_on_another_key() {
        use super::{BackWatch, EscVerdict};
        let mut watch = BackWatch::default();
        watch.frame(7, None);
        for n in 1..=2 {
            assert_eq!(watch.esc(n), EscVerdict::Deliver);
            watch.frame(7, Some(n));
        }
        watch.other_key();
        for n in 3..=5 {
            assert_eq!(watch.esc(n), EscVerdict::Deliver, "esc {n}");
            watch.frame(7, Some(n));
        }
    }

    /// #1087 review: a plugin that repaints on its own (a ticking clock) while
    /// ignoring Esc is still ejected. Only the first frame after each Esc is
    /// judged, so the tick that follows it proves nothing.
    #[test]
    fn back_watch_ejects_a_repainting_plugin_that_ignores_esc() {
        use super::{BackWatch, ESC_UNANSWERED_LIMIT, EscVerdict};
        let mut watch = BackWatch::default();
        let mut clock = 100;
        watch.frame(clock, None);
        for n in 1..=u64::from(ESC_UNANSWERED_LIMIT) {
            assert_eq!(watch.esc(n), EscVerdict::Deliver, "esc {n}");
            watch.frame(clock, Some(n));
            clock += 1;
            watch.frame(clock, Some(n));
        }
        assert_eq!(watch.esc(99), EscVerdict::ReturnToHost);
    }

    /// A frame requested before the Esc but painted after it was sent is what
    /// the Esc starts from, so an Esc that undoes that frame counts as answered.
    #[test]
    fn back_watch_judges_the_esc_against_a_frame_still_in_flight() {
        use super::{BackWatch, EscVerdict};
        let mut watch = BackWatch::default();
        for n in 1..=u64::from(super::ESC_PRESS_CEILING) {
            // A click opened a drawer (frame 2); the Esc closes it (frame 1).
            watch.frame(1, Some(n * 10));
            assert_eq!(watch.esc(n * 10 + 1), EscVerdict::Deliver, "esc {n}");
            watch.frame(2, Some(n * 10));
            watch.frame(1, Some(n * 10 + 1));
        }
    }

    /// #1087 review: a plugin that repaints every frame (a spinner) makes every
    /// Esc look answered, so the press ceiling ejects it regardless.
    #[test]
    fn back_watch_ejects_an_always_repainting_plugin_at_the_press_ceiling() {
        use super::{BackWatch, ESC_PRESS_CEILING, EscVerdict};
        let mut watch = BackWatch::default();
        let mut spinner = 0;
        watch.frame(spinner, None);
        for n in 1..=u64::from(ESC_PRESS_CEILING) {
            assert_eq!(watch.esc(n), EscVerdict::Deliver, "esc {n}");
            spinner += 1;
            watch.frame(spinner, Some(n));
        }
        assert_eq!(watch.esc(99), EscVerdict::ReturnToHost);
        watch.frame(spinner + 1, Some(99));
        assert_eq!(
            watch.esc(100),
            EscVerdict::Deliver,
            "an eject starts afresh"
        );
    }

    /// Another key between Esc presses restarts the ceiling count.
    #[test]
    fn back_watch_press_ceiling_restarts_on_another_key() {
        use super::{BackWatch, ESC_PRESS_CEILING, EscVerdict};
        let mut watch = BackWatch::default();
        for round in 0..3 {
            for n in 1..=u64::from(ESC_PRESS_CEILING) {
                let generation = round * 100 + n;
                assert_eq!(
                    watch.esc(generation),
                    EscVerdict::Deliver,
                    "round {round} esc {n}"
                );
                watch.frame(generation, Some(generation));
            }
            watch.other_key();
        }
    }
}
