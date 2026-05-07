//! ainb plugin host — wasmi runtime, capability gate, registries, loader.
//!
//! Phase 1 scope: load + capability-gate + `_init`.
//! Phase 1.5 adds:
//! * `Arc<HostShared>` event bus (subscribe / publish / pump).
//! * `tick()` and `render()` drivers per loaded plugin.
//! * `LoadOutcome` for batch-loading where some plugins legitimately fail.
//! * `degraded` flag — a plugin that traps during a runtime call is marked
//!   degraded so the host's frame loop keeps going.

#![allow(clippy::module_name_repetitions)]

pub mod capabilities;
pub mod host_fns;
pub mod loader;
pub mod manifest_validate;
pub mod registry;
pub mod runtime;

use std::path::Path;
use std::sync::Arc;

use ainb_plugin_api::{Manifest, RenderTarget, WireBuffer};
use anyhow::{anyhow, Context, Result};

pub use loader::{LoadedPlugin, PluginId};
pub use registry::{
    CommandRegistry, ProviderRegistry, ScreenRegistry, SidebarRegistry, StatuslineRegistry,
};
pub use runtime::{HostShared, HostState, QueuedEvent};

/// Result of `PluginHost::load_all` — separates plugins that loaded cleanly
/// from those that failed during validation, instantiation, or `_init`.
#[derive(Debug, Default)]
pub struct LoadOutcome {
    pub loaded: Vec<PluginId>,
    pub failed: Vec<(String, anyhow::Error)>,
}

/// The top-level plugin host. One per running ainb process.
pub struct PluginHost {
    engine: wasmi::Engine,
    pub(crate) shared: Arc<HostShared>,
    pub(crate) plugins: Vec<LoadedPlugin>,
    pub screen_registry: ScreenRegistry,
    pub command_registry: CommandRegistry,
    pub sidebar_registry: SidebarRegistry,
    pub statusline_registry: StatuslineRegistry,
    pub provider_registry: ProviderRegistry,
}

impl PluginHost {
    #[must_use]
    pub fn new() -> Self {
        Self {
            engine: wasmi::Engine::default(),
            shared: Arc::new(HostShared::default()),
            plugins: Vec::new(),
            screen_registry: ScreenRegistry::default(),
            command_registry: CommandRegistry::default(),
            sidebar_registry: SidebarRegistry::default(),
            statusline_registry: StatuslineRegistry::default(),
            provider_registry: ProviderRegistry::default(),
        }
    }

    /// Reference to the underlying wasmi engine.
    #[must_use]
    pub fn engine(&self) -> &wasmi::Engine {
        &self.engine
    }

    /// Reference to the shared cross-plugin state (event bus subscriptions
    /// + queue). Tests use this to assert pre-/post-conditions without
    /// going through the wasmi boundary.
    #[must_use]
    pub fn shared(&self) -> &Arc<HostShared> {
        &self.shared
    }

    /// Discover plugins under `root` (typically
    /// `~/.agents-in-a-box/plugins/cache/`). Phase 1 stub: returns empty.
    pub fn discover(&mut self, _root: &Path) -> Result<()> {
        Ok(())
    }

    /// Load a single plugin from disk.
    pub fn load_dir(&mut self, dir: &Path) -> Result<PluginId> {
        let plugin = loader::load_from_dir(&self.engine, Arc::clone(&self.shared), dir)?;
        let id = plugin.id().to_string();
        self.plugins.push(plugin);
        Ok(PluginId(id))
    }

    /// Load a plugin from already-resolved bytes + manifest.
    pub fn load_bytes(&mut self, manifest: Manifest, wasm: &[u8]) -> Result<PluginId> {
        let plugin =
            loader::load_from_bytes(&self.engine, Arc::clone(&self.shared), manifest, wasm)?;
        let id = plugin.id().to_string();
        self.plugins.push(plugin);
        Ok(PluginId(id))
    }

    /// Batch-load. Each entry's failure is reported in `failed` rather than
    /// short-circuiting — one bad plugin doesn't block the others.
    pub fn load_all(&mut self, plugins: Vec<(Manifest, Vec<u8>)>) -> LoadOutcome {
        let mut out = LoadOutcome::default();
        for (manifest, wasm) in plugins {
            let name = manifest.plugin.name.clone();
            match self.load_bytes(manifest, &wasm) {
                Ok(id) => out.loaded.push(id),
                Err(e) => out.failed.push((name, e)),
            }
        }
        out
    }

    /// Iterate currently loaded plugins (read-only).
    pub fn plugins(&self) -> impl Iterator<Item = &LoadedPlugin> {
        self.plugins.iter()
    }

    /// Find a loaded plugin by id (the `plugin.name` from its manifest).
    #[must_use]
    pub fn get(&self, id: &str) -> Option<&LoadedPlugin> {
        self.plugins.iter().find(|p| p.id() == id)
    }

    /// Drive a plugin's `_render() -> i32` export. A trap or non-zero return
    /// marks the plugin degraded and bubbles up an error; subsequent calls
    /// against a degraded plugin become no-ops returning `Ok(())`.
    ///
    /// Phase 1.5 simplification: `_render` takes no arguments. The full
    /// `(target, area, &mut Buffer)` ABI lands with the screen registry in
    /// Phase 2a; this is enough surface to exercise panic isolation.
    pub fn render_plugin(&mut self, plugin_id: &str) -> Result<()> {
        let plugin = self.find_mut(plugin_id)?;
        if plugin.degraded {
            return Ok(());
        }
        let result = match plugin
            .instance
            .get_typed_func::<(), i32>(&plugin.store, "_render")
        {
            Ok(f) => f.call(&mut plugin.store, ()),
            Err(_) => return Ok(()), // _render not exported — not a failure
        };
        match result {
            Ok(0) => Ok(()),
            Ok(rc) => {
                plugin.degraded = true;
                Err(anyhow!("_render returned non-zero {rc} (plugin marked degraded)"))
            }
            Err(e) => {
                plugin.degraded = true;
                Err(anyhow::Error::new(e).context("_render trapped (plugin marked degraded)"))
            }
        }
    }

    /// Drive a plugin's `_tick() -> i32` export. Same degradation contract as
    /// [`Self::render_plugin`].
    pub fn tick_plugin(&mut self, plugin_id: &str) -> Result<()> {
        let plugin = self.find_mut(plugin_id)?;
        if plugin.degraded {
            return Ok(());
        }
        let f = match plugin
            .instance
            .get_typed_func::<(), i32>(&plugin.store, "_tick")
        {
            Ok(f) => f,
            Err(_) => return Ok(()),
        };
        match f.call(&mut plugin.store, ()) {
            Ok(0) => Ok(()),
            Ok(rc) => {
                plugin.degraded = true;
                Err(anyhow!("_tick returned non-zero {rc} (plugin marked degraded)"))
            }
            Err(e) => {
                plugin.degraded = true;
                Err(anyhow::Error::new(e).context("_tick trapped (plugin marked degraded)"))
            }
        }
    }

    /// Drain the event queue and dispatch each event to every subscribed
    /// plugin via its `_handle_event(ptr, len) -> i32` export.
    ///
    /// Each subscriber must export `_alloc(size: i32) -> i32` so the host can
    /// reserve a buffer in plugin memory before writing the payload. Plugins
    /// that don't export `_alloc` skip events silently — this keeps
    /// non-event-bus canaries (which never subscribe) trivially correct.
    pub fn pump_events(&mut self) -> Result<()> {
        let (events, subs) = self.shared.drain_events();
        for ev in events {
            let Some(subscribers) = subs.get(&ev.topic) else {
                continue;
            };
            for sub_id in subscribers {
                if sub_id == &ev.publisher {
                    continue; // don't deliver back to publisher
                }
                let _ = self
                    .deliver_event(sub_id, &ev)
                    .with_context(|| format!("dispatch {} → {sub_id}", ev.topic));
            }
        }
        Ok(())
    }

    fn deliver_event(&mut self, plugin_id: &str, ev: &QueuedEvent) -> Result<()> {
        let plugin = self.find_mut(plugin_id)?;
        if plugin.degraded {
            return Ok(());
        }
        // Encode the event payload. Phase 1.5 wire shape: msgpack-encoded
        // Custom event so `_handle_event` sees a stable PluginEvent.
        let wire = rmp_serde::to_vec_named(&ainb_plugin_api::PluginEvent::Custom {
            topic: ev.topic.clone(),
            payload: serde_json::Value::String(
                String::from_utf8(ev.payload.clone())
                    .unwrap_or_else(|_| String::from_utf8_lossy(&ev.payload).into_owned()),
            ),
        })
        .context("encode event")?;

        let len_i32 = i32::try_from(wire.len()).context("event payload too large")?;

        let alloc = match plugin
            .instance
            .get_typed_func::<i32, i32>(&plugin.store, "_alloc")
        {
            Ok(f) => f,
            Err(_) => return Ok(()), // plugin doesn't accept events
        };
        let ptr = alloc
            .call(&mut plugin.store, len_i32)
            .context("_alloc trapped")?;
        anyhow::ensure!(ptr > 0, "_alloc returned null");

        let memory = plugin
            .instance
            .get_export(&plugin.store, "memory")
            .and_then(wasmi::Extern::into_memory)
            .ok_or_else(|| anyhow!("plugin has no exported memory"))?;
        let off_usize = usize::try_from(ptr).context("alloc ptr negative")?;
        memory
            .write(&mut plugin.store, off_usize, &wire)
            .context("write event payload to plugin memory")?;

        let handler = plugin
            .instance
            .get_typed_func::<(i32, i32), i32>(&plugin.store, "_handle_event")
            .context("_handle_event missing")?;
        match handler.call(&mut plugin.store, (ptr, len_i32)) {
            Ok(_) => Ok(()),
            Err(e) => {
                plugin.degraded = true;
                Err(anyhow::Error::new(e).context("_handle_event trapped"))
            }
        }
    }

    /// Take (and clear) the most recent `WireBuffer` a plugin painted for
    /// the given target. Returns `None` when nothing has been painted since
    /// the previous take. ainb-core's screen layer calls this right after
    /// [`Self::render_plugin`] returns.
    #[must_use]
    pub fn take_render(&self, plugin_id: &str, target: RenderTarget) -> Option<WireBuffer> {
        self.shared.take_render(plugin_id, target)
    }

    fn find_mut(&mut self, plugin_id: &str) -> Result<&mut LoadedPlugin> {
        self.plugins
            .iter_mut()
            .find(|p| p.id() == plugin_id)
            .ok_or_else(|| anyhow!("no plugin loaded with id {plugin_id:?}"))
    }

    /// Tear everything down; calls `_shutdown` on each loaded plugin.
    pub fn shutdown(&mut self) {
        for p in &mut self.plugins {
            self.shared.drop_plugin(p.id());
            let _ = p.shutdown();
        }
        self.plugins.clear();
    }
}

impl Default for PluginHost {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for PluginHost {
    fn drop(&mut self) {
        if !self.plugins.is_empty() {
            self.shutdown();
        }
    }
}
