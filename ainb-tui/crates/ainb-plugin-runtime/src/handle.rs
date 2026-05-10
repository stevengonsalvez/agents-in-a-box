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
use std::time::Duration;

use ainb_plugin_protocol::params::Viewport;
use ainb_plugin_protocol::wire_buffer::WireBuffer;
use bytes::Bytes;
use parking_lot::RwLock;
use tokio::sync::oneshot;

use crate::error::RuntimeError;
use crate::plugin_task::Command;
use crate::registry::{ChannelRegistry, RegisteredPlugin};
use crate::runtime::PluginHandle;
use crate::snapshot::SnapshotStore;
use crate::types::{
    ActionOutcome, CliOutcome, LifecycleState, PluginId, RenderOutcome, RuntimeConfig, Topic,
};

/// Internal wiring shared between [`crate::Runtime`] and every clone
/// of [`RuntimeHandle`].
#[derive(Clone)]
pub(crate) struct HandleInner {
    pub(crate) tokio: tokio::runtime::Handle,
    pub(crate) snapshots: SnapshotStore,
    pub(crate) channels: ChannelRegistry,
    pub(crate) plugins: Arc<RwLock<HashMap<PluginId, Arc<PluginHandle>>>>,
    pub(crate) config: RuntimeConfig,
}

/// Send + Clone runtime façade. The TUI thread should hold one of
/// these and never see anything else from this crate.
#[derive(Clone)]
pub struct RuntimeHandle {
    inner: HandleInner,
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

    /// Read a snapshot bytes payload synchronously.
    #[must_use]
    pub fn snapshot_get(&self, topic: &str) -> Option<Bytes> {
        self.inner.snapshots.payload(&Topic::from(topic))
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
        self.inner
            .plugins
            .read()
            .values()
            .map(|p| p.plugin.clone())
            .collect()
    }

    /// Discover plugins under `root` and register each.
    pub fn discover(&self, root: &Path) -> Result<Vec<RegisteredPlugin>, RuntimeError> {
        let plugins = crate::registry::discover(root)?;
        for p in &plugins {
            self.inner.channels.register(p.clone());
            let arc = Arc::new(p.clone());
            let (inbox, cache, state) = crate::plugin_task::spawn(
                arc.clone(),
                self.inner.snapshots.clone(),
                self.inner.config,
                &self.inner.tokio,
            );
            self.inner.plugins.write().insert(
                arc.id.clone(),
                Arc::new(PluginHandle {
                    inbox,
                    cache,
                    state,
                    plugin: arc,
                }),
            );
        }
        Ok(plugins)
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

    /// Test-only: force `SIGKILL` to exercise crash recovery.
    #[cfg(test)]
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
