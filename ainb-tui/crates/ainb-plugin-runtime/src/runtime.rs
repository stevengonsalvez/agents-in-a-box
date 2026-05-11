//! [`Runtime`] — entry point. Owns the tokio runtime and the per-plugin
//! tasks. Hands out [`crate::RuntimeHandle`] to TUI/CLI consumers.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use parking_lot::RwLock;
use tokio::runtime::Runtime as TokioRuntime;

use crate::error::RuntimeError;
use crate::handle::{HandleInner, RuntimeHandle};
use crate::plugin_task::{self, Inbox, InboxMap, RenderCache};
use crate::registry::{self, ChannelRegistry, RegisteredPlugin};
use crate::snapshot::SnapshotStore;
use crate::types::{LifecycleState, PluginId, RuntimeConfig};

/// Per-plugin handle bundle stored inside the runtime's plugin map.
pub(crate) struct PluginHandle {
    pub(crate) inbox: Inbox,
    pub(crate) cache: RenderCache,
    pub(crate) state: Arc<RwLock<LifecycleState>>,
    pub(crate) plugin: Arc<RegisteredPlugin>,
}

/// Owns the tokio runtime + per-plugin tasks. Construct with
/// [`Runtime::new`]; pass the [`RuntimeHandle`] into the TUI.
pub struct Runtime {
    /// Tokio multi-threaded runtime. Kept alive by the [`Runtime`]
    /// instance — dropping it joins every plugin task.
    tokio: TokioRuntime,
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
        let handle = RuntimeHandle::new(HandleInner {
            tokio: tokio.handle().clone(),
            snapshots: snapshots.clone(),
            channels: channels.clone(),
            plugins: plugins.clone(),
            inboxes: inboxes.clone(),
            config,
        });
        Ok((
            Self {
                tokio,
                snapshots,
                channels,
                plugins,
                inboxes,
                config,
            },
            handle,
        ))
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
        let (inbox, cache, state) = plugin_task::spawn(
            arc.clone(),
            self.snapshots.clone(),
            self.inboxes.clone(),
            self.config,
            self.tokio.handle(),
        );
        if eager {
            let _ = inbox.send(plugin_task::Command::EnsureSpawned);
        }
        self.inboxes.write().insert(arc.id.clone(), inbox.clone());
        let handle = Arc::new(PluginHandle {
            inbox,
            cache,
            state,
            plugin: arc.clone(),
        });
        self.plugins.write().insert(arc.id.clone(), handle);
    }

    /// Tokio handle for tests that need to drive the runtime directly.
    #[must_use]
    pub fn tokio_handle(&self) -> tokio::runtime::Handle {
        self.tokio.handle().clone()
    }

    /// Snapshot store reference for tests.
    #[must_use]
    pub const fn snapshots(&self) -> &SnapshotStore {
        &self.snapshots
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        // Send Shutdown to every task; tokio runtime drop joins them.
        for (_, h) in self.plugins.read().iter() {
            let _ = h.inbox.send(crate::plugin_task::Command::Shutdown);
        }
    }
}
