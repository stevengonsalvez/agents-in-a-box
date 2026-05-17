//! [`Runtime`] — entry point. Owns the tokio runtime and the per-plugin
//! tasks. Hands out [`crate::RuntimeHandle`] to TUI/CLI consumers.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use parking_lot::RwLock;
use tokio::runtime::Runtime as TokioRuntime;

use crate::error::RuntimeError;
use crate::handle::{HandleInner, RuntimeHandle};
use crate::plugin_task::{self, DirtyMap, Inbox, InboxMap, KeyInbox, RenderCache};
use crate::registry::{self, ChannelRegistry, RegisteredPlugin};
use crate::snapshot::SnapshotStore;
use crate::types::{LifecycleState, PluginId, RuntimeConfig};

/// Per-plugin handle bundle stored inside the runtime's plugin map.
pub(crate) struct PluginHandle {
    pub(crate) inbox: Inbox,
    /// Priority side-channel reserved for `plugin/handle_key`
    /// notifications. See [`crate::plugin_task::KeyInbox`] for the
    /// rationale (Esc-during-chunked-publish starvation fix).
    pub(crate) key_inbox: KeyInbox,
    pub(crate) cache: RenderCache,
    pub(crate) state: Arc<RwLock<LifecycleState>>,
    pub(crate) plugin: Arc<RegisteredPlugin>,
    /// Render-dirty witness. Set whenever something has happened that
    /// the plugin's UI state may have observed (`send_key`, an event
    /// publish landing in its inbox) and the host therefore needs to
    /// kick a `plugin/render` to repaint. The host's render-tick loop
    /// drains this with `RuntimeHandle::take_render_dirty` and skips
    /// the render-kick entirely when it's clean — collapses the
    /// 250 ms tick-rate idle wait into an event-driven repaint.
    ///
    /// Initialised `true` so the first paint after registration always
    /// fires (the plugin's snapshot bootstrap chunks may or may not
    /// have landed by then, but either way the host needs an initial
    /// `plugin/render` to populate the screen).
    pub(crate) render_dirty: Arc<AtomicBool>,
}

/// Owns the tokio runtime + per-plugin tasks. Construct with
/// [`Runtime::new`]; pass the [`RuntimeHandle`] into the TUI.
pub struct Runtime {
    /// Tokio multi-threaded runtime. Wrapped in `Option` so the
    /// explicit [`Self::shutdown`] path can take it out and call
    /// `shutdown_background()` without tripping the "cannot drop a
    /// runtime in a context where blocking is not allowed" panic.
    /// `None` only after `shutdown()` has consumed it — every other
    /// method assumes `Some(_)` via `as_ref().expect(...)`.
    tokio: Option<TokioRuntime>,
    /// Snapshot store shared with every plugin task.
    snapshots: SnapshotStore,
    /// Channel registry (action → plugin).
    channels: ChannelRegistry,
    /// Per-plugin handles.
    plugins: Arc<RwLock<HashMap<PluginId, Arc<PluginHandle>>>>,
    /// Lightweight fan-out map: `plugin_id → Inbox`. Kept in sync with
    /// `plugins`. Plugin tasks hold a clone so they can deliver
    /// `Command::HandleEvent` to subscribers on `host/snapshot/publish`
    /// without taking a dependency on the public `PluginHandle` type.
    inboxes: InboxMap,
    /// Parallel `plugin_id → render-dirty` flag map. Kept in sync with
    /// `plugins`; shared with plugin tasks so plugin→plugin publishes
    /// can mark subscribers dirty without taking a `PluginHandle` dep.
    dirty: DirtyMap,
    /// Tunables.
    config: RuntimeConfig,
}

impl Runtime {
    /// Construct a new runtime with default config.
    pub fn new() -> Result<(Self, RuntimeHandle), RuntimeError> {
        Self::with_config(RuntimeConfig::default())
    }

    /// Construct with explicit config.
    pub fn with_config(config: RuntimeConfig) -> Result<(Self, RuntimeHandle), RuntimeError> {
        let tokio = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("ainb-plugin-rt")
            .build()
            .map_err(RuntimeError::Io)?;
        let snapshots = SnapshotStore::new();
        let channels = ChannelRegistry::new();
        let plugins: Arc<RwLock<HashMap<PluginId, Arc<PluginHandle>>>> =
            Arc::new(RwLock::new(HashMap::new()));
        let inboxes: InboxMap = Arc::new(parking_lot::RwLock::new(HashMap::new()));
        let dirty: DirtyMap = Arc::new(parking_lot::RwLock::new(HashMap::new()));
        let handle = RuntimeHandle::new(HandleInner {
            tokio: tokio.handle().clone(),
            snapshots: snapshots.clone(),
            channels: channels.clone(),
            plugins: plugins.clone(),
            inboxes: inboxes.clone(),
            dirty: dirty.clone(),
            config,
            key_generation: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        });
        Ok((
            Self {
                tokio: Some(tokio),
                snapshots,
                channels,
                plugins,
                inboxes,
                dirty,
                config,
            },
            handle,
        ))
    }

    /// Tear down the runtime without panicking when dropped from inside
    /// an async context.
    ///
    /// `tokio::runtime::Runtime`'s `Drop` impl calls a blocking-shutdown
    /// that traps if the drop runs inside another tokio runtime's task —
    /// exactly the situation when ainb's `#[tokio::main]` returns and
    /// `AppState.plugin_runtime_owner: Option<Runtime>` goes out of
    /// scope. The user-visible failure is "Cannot drop a runtime in a
    /// context where blocking is not allowed" on every clean exit.
    ///
    /// The fix is to drain the runtime explicitly via
    /// [`TokioRuntime::shutdown_background`], which returns immediately
    /// and joins worker threads off-task. Plugin tasks have already
    /// received `plugin/shutdown` notifications from the TUI's normal
    /// teardown path (or get SIGTERM when our subprocess pipes close on
    /// exit) so a synchronous join would only cost wall-clock without
    /// changing correctness.
    ///
    /// Must be called from `main` before [`App`] drops. Consumes
    /// `self` so callers can't accidentally use it afterwards.
    /// `Drop` is also safe as a fallback (it'll do the same
    /// `shutdown_background` if `shutdown` wasn't called), but
    /// explicit shutdown documents intent at the call site.
    pub fn shutdown(mut self) {
        // Best-effort plugin shutdown notifications first — mirrors
        // the `Drop` impl below. Doing it here so the explicit and
        // fallback paths produce identical lifecycle behaviour.
        for (_, h) in self.plugins.read().iter() {
            let _ = h.inbox.send(crate::plugin_task::Command::Shutdown);
        }
        // Take the runtime out so `Drop` (which runs right after this
        // function returns) sees `None` and skips the redundant
        // `shutdown_background` call. Without `take()` here, `Drop`
        // would still need `Option::take()` to avoid a move-out-of-`&mut self`
        // error inside `Drop::drop`.
        if let Some(rt) = self.tokio.take() {
            rt.shutdown_background();
        }
    }

    /// Discover and register every plugin under `root`.
    ///
    /// Layout: `<root>/<name>/manifest.toml` + `<root>/<name>/<name>` binary.
    pub fn discover(&self, root: &Path) -> Result<Vec<RegisteredPlugin>, RuntimeError> {
        let plugins = registry::discover(root)?;
        for p in &plugins {
            self.register(p.clone());
        }
        Ok(plugins)
    }

    /// Register an already-resolved [`RegisteredPlugin`]. Spawns its
    /// per-plugin task in `Idle` state — first request lazy-spawns
    /// the process. When the manifest declares
    /// `[lifecycle].spawn = "eager"`, an `EnsureSpawned` command is
    /// dispatched immediately so the child is launched without
    /// waiting for a first request. Required for pure-publisher
    /// plugins (e.g. session-reader) that no caller drives directly.
    pub fn register(&self, plugin: RegisteredPlugin) {
        let arc = Arc::new(plugin);
        self.channels.register((*arc).clone());
        let eager = matches!(
            arc.manifest.lifecycle.spawn,
            ainb_plugin_protocol::manifest::SpawnMode::Eager
        );
        let (inbox, key_inbox, cache, state) = plugin_task::spawn(
            arc.clone(),
            self.snapshots.clone(),
            self.inboxes.clone(),
            self.dirty.clone(),
            self.config,
            self.tokio
                .as_ref()
                .expect("plugin runtime alive — register() called after shutdown()")
                .handle(),
        );
        if eager {
            let _ = inbox.send(plugin_task::Command::EnsureSpawned);
        }
        self.inboxes.write().insert(arc.id.clone(), inbox.clone());
        // Start dirty so the first paint after registration kicks a render.
        let render_dirty = Arc::new(AtomicBool::new(true));
        self.dirty.write().insert(arc.id.clone(), render_dirty.clone());
        let handle = Arc::new(PluginHandle {
            inbox,
            key_inbox,
            cache,
            state,
            plugin: arc.clone(),
            render_dirty,
        });
        self.plugins.write().insert(arc.id.clone(), handle);
    }

    /// Tokio handle for tests that need to drive the runtime directly.
    #[must_use]
    pub fn tokio_handle(&self) -> tokio::runtime::Handle {
        self.tokio
            .as_ref()
            .expect("plugin runtime alive — tokio_handle() called after shutdown()")
            .handle()
            .clone()
    }

    /// Snapshot store reference for tests.
    #[must_use]
    pub const fn snapshots(&self) -> &SnapshotStore {
        &self.snapshots
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        // Send Shutdown to every task. Plugin tasks observe this on
        // their inbox and exit their event loop cleanly.
        for (_, h) in self.plugins.read().iter() {
            let _ = h.inbox.send(crate::plugin_task::Command::Shutdown);
        }
        // Tear down the tokio runtime non-blockingly so this is safe
        // to call from inside another runtime's task (e.g. when
        // `AppState` drops at the end of `#[tokio::main]`).
        // `shutdown_background()` returns immediately and joins worker
        // threads off-task — see the `Self::shutdown` doc for the
        // full rationale. Idempotent: if `shutdown()` already ran,
        // `tokio` is `None` and we no-op.
        if let Some(rt) = self.tokio.take() {
            rt.shutdown_background();
        }
    }
}

#[cfg(test)]
mod shutdown_regression_tests {
    //! Lock the fix from PR #129. Before this change, both of these
    //! tests would trip tokio's "Cannot drop a runtime in a context
    //! where blocking is not allowed" panic — the inner `TokioRuntime`
    //! ran its blocking-shutdown while still inside the outer
    //! `#[tokio::test]` runtime's task context.
    //!
    //! Either path (explicit `shutdown()` OR `Drop`) must work; both
    //! tests cover one each so a future regression in either site
    //! fails loudly.
    use super::*;

    #[tokio::test]
    async fn drop_inside_async_context_does_not_panic() {
        // No explicit shutdown — relies on `Drop::drop` calling
        // `shutdown_background()` on the inner `TokioRuntime`.
        let (runtime, _handle) = Runtime::new().expect("runtime");
        drop(runtime);
    }

    #[tokio::test]
    async fn explicit_shutdown_inside_async_context_does_not_panic() {
        // The canonical path called from `main.rs` after `run_tui`
        // returns. Consumes `self` so `Drop` runs after on a
        // `tokio: None` state.
        let (runtime, _handle) = Runtime::new().expect("runtime");
        runtime.shutdown();
    }
}
