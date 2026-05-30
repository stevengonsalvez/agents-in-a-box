//! [`RuntimeHandle`] — Send + Clone façade the TUI consumes.
//!
//! Hard architectural constraint: every method usable from the TUI
//! render thread is **synchronous** and **non-blocking** at the level
//! of "user-visible latency". `try_recv_render` and `snapshot_get`
//! never `.await`. The async-returning surface
//! ([`render`](Self::render), [`dispatch_cli`](Self::dispatch_cli),
//! [`invoke_action`](Self::invoke_action)) returns a `oneshot::Receiver`
//! — caller polls it from a non-render thread or via `try_recv`.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use ainb_plugin_protocol::params::{HandleKeyParams, Viewport};
use ainb_plugin_protocol::wire_buffer::WireBuffer;
use bytes::Bytes;
use parking_lot::RwLock;
use tokio::sync::oneshot;

use crate::error::RuntimeError;
use crate::event_stream::{stream_topic, EventStreamRegistry};
use crate::managed_subprocess::ManagedSubprocessRegistry;
use crate::unix_socket::UnixSocketRegistry;
use crate::plugin_task::{Command, InboxMap};
use crate::registry::{ChannelRegistry, RegisteredPlugin};
use crate::runtime::PluginHandle;
use crate::snapshot::SnapshotStore;
use crate::types::{
    ActionOutcome, CliOutcome, LifecycleState, LogTap, LogTapFn, PluginId, RenderOutcome,
    RuntimeConfig, Topic,
};

/// Internal wiring shared between [`crate::Runtime`] and every clone
/// of [`RuntimeHandle`].
#[derive(Clone)]
#[allow(missing_debug_implementations)]
pub(crate) struct HandleInner {
    pub(crate) tokio: tokio::runtime::Handle,
    pub(crate) snapshots: SnapshotStore,
    pub(crate) channels: ChannelRegistry,
    pub(crate) plugins: Arc<RwLock<HashMap<PluginId, Arc<PluginHandle>>>>,
    /// Lightweight fan-out map (`plugin_id` → Inbox). Mirrors `plugins`
    /// for the publish path; see [`crate::plugin_task::InboxMap`].
    pub(crate) inboxes: InboxMap,
    /// Parallel `plugin_id → render-dirty` map. See `runtime::PluginHandle`.
    /// Held here so [`RuntimeHandle::mark_render_dirty`] can flip a
    /// subscriber's flag through a single `RwLock` read instead of
    /// reaching through `plugins.read()` + `PluginHandle`. The same
    /// `Arc<AtomicBool>` lives in both maps — flipping one is visible
    /// from the other.
    pub(crate) dirty: crate::plugin_task::DirtyMap,
    /// Cap-gated event-stream registry shared with every plugin task.
    /// `publish_stream_event` reads it to fan out to subscribed streams.
    pub(crate) event_streams: EventStreamRegistry,
    /// Cap-gated managed-subprocess registry shared with every plugin
    /// task. Held here so the `reload` path can re-spawn plugin tasks
    /// with the same registry the `Runtime` owns.
    pub(crate) managed_subprocess: ManagedSubprocessRegistry,
    /// Cap-gated unix-socket dial registry shared with every plugin task.
    /// Held here so the `reload` path re-spawns plugin tasks with the same
    /// registry the `Runtime` owns.
    pub(crate) unix_sockets: UnixSocketRegistry,
    /// Optional host-side log tap (sentinel capture for tests).
    pub(crate) log_tap: LogTap,
    /// Shared platform secret backend (DI). Held here so the `reload` path
    /// re-spawns plugin tasks with the same backend the `Runtime` owns.
    pub(crate) secret_backend: crate::secret_store::SharedSecretBackend,
    /// Shared host workspace store (DI). Held here so the `reload` path
    /// re-spawns plugin tasks with the same store the `Runtime` owns.
    pub(crate) workspace_store: crate::workspace_store::SharedWorkspaceStore,
    pub(crate) config: RuntimeConfig,
    /// Monotonic counter the host bumps once per `send_key` call.
    /// Stamped into `HandleKeyParams.generation`; the plugin echoes it
    /// back via the next `plugin/render` so the host has a freshness
    /// witness. Shared across every clone of the handle so the same
    /// sequence works regardless of which `RuntimeHandle` queued the
    /// keystroke.
    pub(crate) key_generation: Arc<AtomicU64>,
}

/// Send + Clone runtime façade. The TUI thread should hold one of
/// these and never see anything else from this crate.
#[derive(Clone)]
pub struct RuntimeHandle {
    inner: HandleInner,
}

impl std::fmt::Debug for RuntimeHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Opaque on purpose — the inner has tokio + lock primitives
        // that aren't `Debug`. Surface the plugin count, which is the
        // only state likely to be useful in a panic backtrace.
        f.debug_struct("RuntimeHandle")
            .field("plugins", &self.inner.plugins.read().len())
            .finish()
    }
}

impl RuntimeHandle {
    pub(crate) const fn new(inner: HandleInner) -> Self {
        Self { inner }
    }

    fn lookup(&self, plugin_id: &PluginId) -> Option<Arc<PluginHandle>> {
        self.inner.plugins.read().get(plugin_id).cloned()
    }

    /// Issue a `plugin/render`. Returns a oneshot receiver carrying
    /// the [`RenderOutcome`]. The TUI thread should call
    /// [`try_recv_render`](Self::try_recv_render) instead of awaiting
    /// this receiver.
    pub fn render(
        &self,
        plugin_id: &PluginId,
        viewport: Viewport,
        generation: u64,
    ) -> oneshot::Receiver<RenderOutcome> {
        let (tx, rx) = oneshot::channel();
        let Some(handle) = self.lookup(plugin_id) else {
            let _ = tx.send(RenderOutcome::RuntimeError(format!(
                "unknown plugin: {plugin_id}"
            )));
            return rx;
        };
        if handle
            .inbox
            .send(Command::Render {
                viewport,
                generation,
                reply: tx,
            })
            .is_err()
        {
            let (etx, erx) = oneshot::channel();
            let _ = etx.send(RenderOutcome::RuntimeError("plugin task gone".into()));
            return erx;
        }
        rx
    }

    /// Issue a `plugin/cli_dispatch`.
    pub fn dispatch_cli(
        &self,
        plugin_id: &PluginId,
        namespace: &str,
        argv: Vec<String>,
    ) -> oneshot::Receiver<CliOutcome> {
        let (tx, rx) = oneshot::channel();
        let Some(handle) = self.lookup(plugin_id) else {
            let _ = tx.send(CliOutcome::RuntimeError(format!(
                "unknown plugin: {plugin_id}"
            )));
            return rx;
        };
        if handle
            .inbox
            .send(Command::Cli {
                namespace: namespace.to_owned(),
                argv,
                reply: tx,
            })
            .is_err()
        {
            let (etx, erx) = oneshot::channel();
            let _ = etx.send(CliOutcome::RuntimeError("plugin task gone".into()));
            return erx;
        }
        rx
    }

    /// Invoke a host action. Routes through the plugin advertising the
    /// action namespace.
    pub fn invoke_action(
        &self,
        action: &str,
        payload: Bytes,
        timeout: Duration,
    ) -> oneshot::Receiver<ActionOutcome> {
        let (tx, rx) = oneshot::channel();
        let Some(plugin_id) = self.inner.channels.route_action(action) else {
            let _ = tx.send(ActionOutcome::RuntimeError(format!("unknown action: {action}")));
            return rx;
        };
        let Some(handle) = self.lookup(&plugin_id) else {
            let _ = tx.send(ActionOutcome::RuntimeError(format!(
                "plugin gone: {plugin_id}"
            )));
            return rx;
        };
        let timeout_ms = u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX);
        if handle
            .inbox
            .send(Command::Action {
                action: action.to_owned(),
                payload,
                timeout_ms,
                reply: tx,
            })
            .is_err()
        {
            let (etx, erx) = oneshot::channel();
            let _ = etx.send(ActionOutcome::RuntimeError("plugin task gone".into()));
            return erx;
        }
        rx
    }

    /// Pop the latest cached render buffer for a plugin. Non-blocking;
    /// returns `None` if no render has completed since the last poll.
    ///
    /// Architectural lint: this method MUST stay synchronous and
    /// MUST NOT call `.await`. The TUI render path depends on it.
    #[must_use]
    pub fn try_recv_render(&self, plugin_id: &PluginId) -> Option<WireBuffer> {
        self.lookup(plugin_id).and_then(|p| p.cache.try_take())
    }

    /// Atomically check-and-clear the render-dirty flag for a plugin.
    /// Returns `true` iff the host should kick a fresh `plugin/render`
    /// this tick because state may have changed since the last paint.
    ///
    /// Set by `send_key` and `publish_snapshot` (for each subscriber)
    /// and initially by plugin registration. The render-tick loop
    /// calls this once per plugin per tick and skips the render kick
    /// entirely when the result is `false` — turning the loop from a
    /// fixed-cadence render storm into an event-driven repaint.
    pub fn take_render_dirty(&self, plugin_id: &PluginId) -> bool {
        self.lookup(plugin_id)
            .is_some_and(|p| p.render_dirty.swap(false, Ordering::AcqRel))
    }

    /// Explicitly mark a plugin's screen as needing a repaint. Used by
    /// the host when something OUTSIDE the runtime (e.g. a viewport
    /// resize) ought to drive a fresh render even though no key or
    /// event arrived. Routed through [`HandleInner::dirty`] so the
    /// host doesn't take a `plugins.read()` lock for a single bit
    /// flip.
    pub fn mark_render_dirty(&self, plugin_id: &PluginId) {
        if let Some(flag) = self.inner.dirty.read().get(plugin_id) {
            flag.store(true, Ordering::Release);
        }
    }

    /// Read a snapshot bytes payload synchronously.
    #[must_use]
    pub fn snapshot_get(&self, topic: &str) -> Option<Bytes> {
        self.inner.snapshots.payload(&Topic::from(topic))
    }

    /// Forward a single normalized key event to the plugin owning the
    /// focused screen. Non-blocking — the per-plugin tokio task picks
    /// the command up off its mpsc inbox and writes the
    /// `plugin/handle_key` notification frame.
    ///
    /// The host allocates a monotonic `generation` per call (shared
    /// across all clones of [`RuntimeHandle`]) so the plugin can echo
    /// it back via the next `plugin/render` and the host can prove the
    /// keystroke landed before the frame was painted.
    ///
    /// Returns `false` if the plugin is unknown or the task is gone;
    /// the keystroke is dropped on the floor in either case. Caller is
    /// expected to surface a soft error or simply ignore — interactive
    /// keys are tolerant of loss compared to snapshots.
    pub fn send_key(
        &self,
        plugin_id: &PluginId,
        screen_id: impl Into<String>,
        key: ainb_plugin_protocol::params::KeyEvent,
    ) -> bool {
        let Some(handle) = self.lookup(plugin_id) else {
            return false;
        };
        let generation = self.inner.key_generation.fetch_add(1, Ordering::Relaxed);
        let params = HandleKeyParams {
            screen_id: screen_id.into(),
            key,
            generation,
        };
        // Mark dirty BEFORE enqueue so the host's next tick can't race
        // ahead and clear it before the plugin observes the keystroke.
        // Worst case the host fires one no-op render kick — harmless.
        handle.render_dirty.store(true, Ordering::Release);
        // Priority channel — bypasses the main FIFO inbox so an Esc
        // keypress doesn't queue behind chunked `HandleEvent` publishes
        // (a 50-chunk `sessions.usage_data` refresh was previously
        // starving Esc on the burndown screen).
        handle.key_inbox.send(params).is_ok()
    }

    /// Publish a snapshot from the host side. Non-blocking. Subscriber
    /// fan-out happens on the tokio runtime.
    pub fn publish_snapshot(&self, topic: &str, payload: Bytes) -> u64 {
        let topic_owned = Topic::from(topic);
        let v = self.inner.snapshots.publish(topic_owned.clone(), payload.clone());
        let subs = self.inner.snapshots.subscribers(&topic_owned);
        if subs.is_empty() {
            return v;
        }
        let plugins = self.inner.plugins.clone();
        self.inner.tokio.spawn(async move {
            let map = plugins.read();
            for sub in subs {
                if let Some(handle) = map.get(&sub) {
                    // Mark dirty BEFORE the enqueue so the host's
                    // render tick can't drain the flag between the
                    // event landing and the next render kick.
                    handle.render_dirty.store(true, Ordering::Release);
                    let _ = handle.inbox.send(Command::HandleEvent {
                        topic: topic_owned.clone(),
                        payload: payload.clone(),
                    });
                }
            }
        });
        v
    }

    /// Publish an event to every plugin holding an
    /// [`event_stream`](crate::event_stream) subscription on `topic`.
    ///
    /// Each matching stream receives a `plugin/handle_event`
    /// notification under the stream's own delivery topic
    /// `stream:<stream_id>` (NOT the raw `topic`), so a plugin holding
    /// several streams can demux. Streams dropped on plugin teardown are
    /// not delivered to (no leak to dead processes). Non-blocking — the
    /// fan-out runs on the tokio runtime.
    ///
    /// Returns the number of streams the event was dispatched to.
    pub fn publish_stream_event(&self, topic: &str, payload: Bytes) -> usize {
        let topic_owned = Topic::from(topic);
        let streams = self.inner.event_streams.streams_for_topic(&topic_owned);
        if streams.is_empty() {
            return 0;
        }
        let n = streams.len();
        let plugins = self.inner.plugins.clone();
        self.inner.tokio.spawn(async move {
            let map = plugins.read();
            for (plugin, stream_id) in streams {
                if let Some(handle) = map.get(&plugin) {
                    handle.render_dirty.store(true, Ordering::Release);
                    let _ = handle.inbox.send(Command::HandleEvent {
                        topic: Topic::from(stream_topic(&stream_id)),
                        payload: payload.clone(),
                    });
                }
            }
        });
        n
    }

    /// Install a host-side log tap that observes every `host/log` line
    /// emitted by any plugin. Returns a shared buffer the caller drains.
    ///
    /// Test/diagnostic aid: the CTS anti-cheat axes read sentinel log
    /// lines through this to prove a cap-allowed handler actually ran.
    /// Replaces any previously installed tap.
    #[must_use]
    pub fn install_log_tap(&self) -> std::sync::Arc<std::sync::Mutex<Vec<String>>> {
        let sink = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let sink_for_fn = sink.clone();
        let tap: LogTapFn = std::sync::Arc::new(move |p| {
            sink_for_fn.lock().unwrap().push(p.message.clone());
        });
        *self.inner.log_tap.write() = Some(tap);
        sink
    }

    /// Lifecycle state of a plugin.
    #[must_use]
    pub fn lifecycle_state(&self, plugin_id: &PluginId) -> Option<LifecycleState> {
        self.lookup(plugin_id).map(|p| *p.state.read())
    }

    /// Registered plugins, in registration order.
    #[must_use]
    pub fn registered_plugins(&self) -> Vec<Arc<RegisteredPlugin>> {
        self.inner
            .plugins
            .read()
            .values()
            .map(|p| p.plugin.clone())
            .collect()
    }

    /// Discover plugins under `root` and register each. When a
    /// plugin's manifest declares `[lifecycle].spawn = "eager"` the
    /// task is poked with `EnsureSpawned` so the child process is
    /// launched immediately — required for pure-publisher plugins
    /// (e.g. session-reader) that no caller drives directly.
    pub fn discover(&self, root: &Path) -> Result<Vec<RegisteredPlugin>, RuntimeError> {
        self.discover_filtered(root, |_| true)
    }

    /// Discover plugins under `root` and register only those for which
    /// `filter` returns `true`. The returned `Vec` reflects the
    /// *registered* subset, not the on-disk superset — callers logging
    /// "loaded plugins" want this, not the pre-filter list.
    ///
    /// Used by the host's `init_plugin_runtime` to apply env-var /
    /// config.toml allowlist/denylist before any plugin task is
    /// spawned. Filtering at discovery time (vs. spawn time) avoids
    /// allocating per-plugin channels for skipped plugins.
    pub fn discover_filtered<F>(
        &self,
        root: &Path,
        filter: F,
    ) -> Result<Vec<RegisteredPlugin>, RuntimeError>
    where
        F: Fn(&RegisteredPlugin) -> bool,
    {
        let discovered = crate::registry::discover(root)?;
        let kept: Vec<RegisteredPlugin> = discovered.into_iter().filter(&filter).collect();
        for p in &kept {
            self.inner.channels.register(p.clone());
            let arc = Arc::new(p.clone());
            let eager = matches!(
                arc.manifest.lifecycle.spawn,
                ainb_plugin_protocol::manifest::SpawnMode::Eager
            );
            let (inbox, key_inbox, cache, state) = crate::plugin_task::spawn(
                arc.clone(),
                self.inner.snapshots.clone(),
                self.inner.inboxes.clone(),
                self.inner.dirty.clone(),
                self.inner.event_streams.clone(),
                self.inner.managed_subprocess.clone(),
                self.inner.unix_sockets.clone(),
                self.inner.secret_backend.clone(),
                self.inner.workspace_store.clone(),
                self.inner.log_tap.clone(),
                self.inner.config,
                &self.inner.tokio,
            );
            if eager {
                let _ = inbox.send(crate::plugin_task::Command::EnsureSpawned);
            }
            self.inner
                .inboxes
                .write()
                .insert(arc.id.clone(), inbox.clone());
            // Start dirty so the first paint after registration kicks a render.
            let render_dirty = Arc::new(std::sync::atomic::AtomicBool::new(true));
            self.inner
                .dirty
                .write()
                .insert(arc.id.clone(), render_dirty.clone());
            self.inner.plugins.write().insert(
                arc.id.clone(),
                Arc::new(PluginHandle {
                    inbox,
                    key_inbox,
                    cache,
                    state,
                    plugin: arc,
                    render_dirty,
                }),
            );
        }
        Ok(kept)
    }

    /// Reload (clear quarantine + failure history).
    pub fn reload(&self, plugin_id: &PluginId) -> Result<(), RuntimeError> {
        let h = self
            .lookup(plugin_id)
            .ok_or_else(|| RuntimeError::UnknownPlugin(plugin_id.clone()))?;
        h.inbox
            .send(Command::Reload)
            .map_err(|_| RuntimeError::ShuttingDown)?;
        Ok(())
    }

    /// Test/debug aid: force `SIGKILL` on the plugin process to
    /// exercise the crash-recovery code path. Hidden from rustdoc;
    /// not part of the stable surface.
    #[doc(hidden)]
    pub fn inject_kill(&self, plugin_id: &PluginId) -> Result<(), RuntimeError> {
        let h = self
            .lookup(plugin_id)
            .ok_or_else(|| RuntimeError::UnknownPlugin(plugin_id.clone()))?;
        h.inbox
            .send(Command::InjectKill)
            .map_err(|_| RuntimeError::ShuttingDown)?;
        Ok(())
    }
}
