//! ainb plugin host — wasmi runtime, capability gate, registries, loader.
//!
//! Phase 1 scope: the host can `discover` a plugin directory, validate the
//! manifest, instantiate the WASM module under capability-gated imports, and
//! call the `_init` export. Registries are present but empty; ainb-core does
//! not yet depend on this crate.

#![allow(clippy::module_name_repetitions)]

pub mod capabilities;
pub mod host_fns;
pub mod loader;
pub mod manifest_validate;
pub mod registry;
pub mod runtime;

use std::path::Path;

use ainb_plugin_api::Manifest;
use anyhow::Result;

pub use loader::{LoadedPlugin, PluginId};
pub use registry::{
    CommandRegistry, ProviderRegistry, ScreenRegistry, SidebarRegistry, StatuslineRegistry,
};
pub use runtime::HostState;

/// The top-level plugin host. One per running ainb process.
pub struct PluginHost {
    engine: wasmi::Engine,
    plugins: Vec<LoadedPlugin>,
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
            plugins: Vec::new(),
            screen_registry: ScreenRegistry::default(),
            command_registry: CommandRegistry::default(),
            sidebar_registry: SidebarRegistry::default(),
            statusline_registry: StatuslineRegistry::default(),
            provider_registry: ProviderRegistry::default(),
        }
    }

    /// Reference to the underlying wasmi engine — exposed so loaders + tests
    /// can build [`wasmi::Module`]s without a separate engine.
    #[must_use]
    pub fn engine(&self) -> &wasmi::Engine {
        &self.engine
    }

    /// Discover plugins under `root` (typically
    /// `~/.agents-in-a-box/plugins/cache/`). Phase 1 stub: returns empty.
    /// Real impl lands in Phase 4.
    pub fn discover(&mut self, _root: &Path) -> Result<()> {
        Ok(())
    }

    /// Load a single plugin from its on-disk directory. The directory must
    /// contain `plugin.toml` and `plugin.wasm`.
    pub fn load_dir(&mut self, dir: &Path) -> Result<PluginId> {
        let plugin = loader::load_from_dir(&self.engine, dir)?;
        let id = plugin.id().to_string();
        self.plugins.push(plugin);
        Ok(PluginId(id))
    }

    /// Load a plugin from already-resolved bytes + manifest. Test-friendly
    /// alternative to [`Self::load_dir`].
    pub fn load_bytes(&mut self, manifest: Manifest, wasm: &[u8]) -> Result<PluginId> {
        let plugin = loader::load_from_bytes(&self.engine, manifest, wasm)?;
        let id = plugin.id().to_string();
        self.plugins.push(plugin);
        Ok(PluginId(id))
    }

    /// Iterate currently loaded plugins (read-only).
    pub fn plugins(&self) -> impl Iterator<Item = &LoadedPlugin> {
        self.plugins.iter()
    }

    /// Tear everything down; calls `_shutdown` on each loaded plugin.
    pub fn shutdown(&mut self) {
        for p in &mut self.plugins {
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
