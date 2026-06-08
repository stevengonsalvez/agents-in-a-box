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
use crate::plugin_task::{Command, InboxMap};
use crate::registry::{ChannelRegistry, RegisteredPlugin};
use crate::runtime::PluginHandle;
use crate::snapshot::SnapshotStore;
use crate::types::{
    ActionOutcome, CliOutcome, LifecycleState, PluginId, RenderOutcome, RuntimeConfig, Topic,
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
    /// Lightweight fan-out map (plugin_id → Inbox). Mirrors `plugins`
    /// for the publish path; see [`crate::plugin_task::InboxMap`].
    pub(crate) inboxes: InboxMap,
    /// Parallel `plugin_id → render-dirty` map. See `runtime::PluginHandle`.
    /// Held here so [`RuntimeHandle::mark_render_dirty`] can flip a
    /// subscriber's flag through a single `RwLock` read instead of
    /// reaching through `plugins.read()` + `PluginHandle`. The same
    /// `Arc<AtomicBool>` lives in both maps — flipping one is visible
    /// from the other.
    pub(crate) dirty: crate::plugin_task::DirtyMap,
    pub(crate) config: RuntimeConfig,
    /// Monotonic counter the host bumps once per `send_key` call.
    /// Stamped into `HandleKeyParams.generation`; the plugin echoes it
    /// back via the next `plugin/render` so the host has a freshness
    /// witness. Shared across every clone of the handle so the same
    /// sequence works regardless of which `RuntimeHandle` queued the
    /// keystroke.
    pub(crate) key_generation: Arc<AtomicU64>,
    /// Monotonic counter the host bumps once per `send_mouse` call.
    /// Parallel to [`Self::key_generation`]; stamped into
    /// `HandleMouseParams.generation` as the same kind of freshness
    /// witness for forwarded mouse events.
    pub(crate) mouse_generation: Arc<AtomicU64>,
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
            let _ = tx.send(ActionOutcome::RuntimeError(format!(
                "unknown action: {action}"
            )));
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

    /// Forward a single normalized mouse event to the plugin owning the
    /// focused screen. Non-blocking — mirrors [`Self::send_key`]: the
    /// per-plugin tokio task picks it off the priority mouse inbox and
    /// writes the `plugin/handle_mouse` notification frame.
    ///
    /// `mouse.col`/`mouse.row` MUST already be translated into the
    /// plugin's viewport coordinate space by the caller (the host's
    /// mouse forwarder subtracts the screen origin).
    ///
    /// Returns `false` if the plugin is unknown or the task is gone; the
    /// event is dropped in either case (mouse events, like keys, tolerate
    /// loss).
    pub fn send_mouse(
        &self,
        plugin_id: &PluginId,
        screen_id: impl Into<String>,
        mouse: ainb_plugin_protocol::params::MouseEvent,
    ) -> bool {
        let Some(handle) = self.lookup(plugin_id) else {
            return false;
        };
        let generation = self.inner.mouse_generation.fetch_add(1, Ordering::Relaxed);
        let params = ainb_plugin_protocol::params::HandleMouseParams {
            screen_id: screen_id.into(),
            mouse,
            generation,
        };
        // Mark dirty BEFORE enqueue (same race-avoidance as `send_key`).
        handle.render_dirty.store(true, Ordering::Release);
        handle.mouse_inbox.send(params).is_ok()
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

    /// Lifecycle state of a plugin.
    #[must_use]
    pub fn lifecycle_state(&self, plugin_id: &PluginId) -> Option<LifecycleState> {
        self.lookup(plugin_id).map(|p| *p.state.read())
    }

    /// Registered plugins, in registration order.
    #[must_use]
    pub fn registered_plugins(&self) -> Vec<Arc<RegisteredPlugin>> {
        self.inner.plugins.read().values().map(|p| p.plugin.clone()).collect()
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
        self.discover_filtered_with_config(root, filter, |_| serde_json::Value::Null)
    }

    /// Like [`Self::discover_filtered`], but resolves each kept plugin's
    /// per-plugin config via `config_for` (the host maps `plugins.values[id]`
    /// → JSON) and stamps it onto the [`RegisteredPlugin`] before registration.
    /// The runtime forwards that config into `PluginInitParams.config` at
    /// `plugin/init`. Plugins for which `config_for` returns JSON `null` behave
    /// exactly as before — this is a strict superset of `discover_filtered`.
    pub fn discover_filtered_with_config<F, C>(
        &self,
        root: &Path,
        filter: F,
        mut config_for: C,
    ) -> Result<Vec<RegisteredPlugin>, RuntimeError>
    where
        F: Fn(&RegisteredPlugin) -> bool,
        C: FnMut(&RegisteredPlugin) -> serde_json::Value,
    {
        let discovered = crate::registry::discover(root)?;
        let kept: Vec<RegisteredPlugin> = discovered
            .into_iter()
            .filter(&filter)
            .map(|p| {
                let config = config_for(&p);
                p.with_config(config)
            })
            .collect();
        for p in &kept {
            self.register_kept(p.clone());
        }
        Ok(kept)
    }

    /// Register one already-discovered (and config-stamped) plugin: route its
    /// actions, spawn its per-plugin task, wire its inbox + dirty flag, and
    /// store its [`PluginHandle`]. Shared by every discovery entry point so the
    /// registration contract has a single definition.
    fn register_kept(&self, plugin: RegisteredPlugin) {
        self.inner.channels.register(plugin.clone());
        let arc = Arc::new(plugin);
        let eager = matches!(
            arc.manifest.lifecycle.spawn,
            ainb_plugin_protocol::manifest::SpawnMode::Eager
        );
        let (inbox, key_inbox, mouse_inbox, cache, state) = crate::plugin_task::spawn(
            arc.clone(),
            self.inner.snapshots.clone(),
            self.inner.inboxes.clone(),
            self.inner.dirty.clone(),
            self.inner.config,
            &self.inner.tokio,
        );
        if eager {
            let _ = inbox.send(crate::plugin_task::Command::EnsureSpawned);
        }
        self.inner.inboxes.write().insert(arc.id.clone(), inbox.clone());
        // Start dirty so the first paint after registration kicks a render.
        let render_dirty = Arc::new(std::sync::atomic::AtomicBool::new(true));
        self.inner.dirty.write().insert(arc.id.clone(), render_dirty.clone());
        self.inner.plugins.write().insert(
            arc.id.clone(),
            Arc::new(PluginHandle {
                inbox,
                key_inbox,
                mouse_inbox,
                cache,
                state,
                plugin: arc,
                render_dirty,
            }),
        );
    }

    /// Reload (clear quarantine + failure history).
    pub fn reload(&self, plugin_id: &PluginId) -> Result<(), RuntimeError> {
        let h = self
            .lookup(plugin_id)
            .ok_or_else(|| RuntimeError::UnknownPlugin(plugin_id.clone()))?;
        h.inbox.send(Command::Reload).map_err(|_| RuntimeError::ShuttingDown)?;
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
        h.inbox.send(Command::InjectKill).map_err(|_| RuntimeError::ShuttingDown)?;
        Ok(())
    }
}
