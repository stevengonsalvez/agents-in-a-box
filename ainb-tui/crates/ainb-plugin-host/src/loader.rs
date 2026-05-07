//! Plugin loader.
//!
//! `load_from_dir` resolves `plugin.toml` + `plugin.wasm` next to each other
//! under the cache directory. `load_from_bytes` is the test-friendly variant.
//! Both validate the manifest, build the capability set, link gated host-fns
//! into the wasmi `Linker`, instantiate the module, and call `_init` if the
//! plugin exports it.

use std::fs;
use std::path::Path;

use ainb_plugin_api::CapabilitySet;
use ainb_plugin_api::Manifest;
use anyhow::{Context, Result};
use wasmi::{Engine, Instance, Module, Store};

use crate::host_fns::{link_baseline, link_capabilities};
use crate::manifest_validate::validate;
use crate::runtime::HostState;

/// Stable identifier for a loaded plugin (the `plugin.name` from its manifest).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PluginId(pub String);

impl std::fmt::Display for PluginId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// One plugin instance after `_init` has run successfully.
pub struct LoadedPlugin {
    manifest: Manifest,
    store: Store<HostState>,
    instance: Instance,
}

impl LoadedPlugin {
    #[must_use]
    pub fn id(&self) -> &str {
        &self.manifest.plugin.name
    }

    #[must_use]
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    /// Borrow the wasmi store immutably (e.g. to inspect the capability set
    /// or log ring from a test).
    #[must_use]
    pub fn host_state(&self) -> &HostState {
        self.store.data()
    }

    /// Call the plugin's `_shutdown` export if present, then drop the store
    /// when this `LoadedPlugin` is dropped.
    pub fn shutdown(&mut self) -> Result<()> {
        if let Ok(f) = self
            .instance
            .get_typed_func::<(), ()>(&self.store, "_shutdown")
        {
            f.call(&mut self.store, ())
                .with_context(|| format!("_shutdown trapped for {}", self.manifest.plugin.name))?;
        }
        Ok(())
    }
}

/// Load a plugin from `<dir>/plugin.toml` + `<dir>/plugin.wasm`.
pub fn load_from_dir(engine: &Engine, dir: &Path) -> Result<LoadedPlugin> {
    let toml_path = dir.join("plugin.toml");
    let wasm_path = dir.join("plugin.wasm");
    let toml_src = fs::read_to_string(&toml_path)
        .with_context(|| format!("read {}", toml_path.display()))?;
    let manifest =
        Manifest::from_toml(&toml_src).with_context(|| format!("parse {}", toml_path.display()))?;
    let wasm =
        fs::read(&wasm_path).with_context(|| format!("read {}", wasm_path.display()))?;
    load_from_bytes(engine, manifest, &wasm)
}

/// Load a plugin from already-resolved manifest + bytes.
///
/// Used by tests and by `load_from_dir`. The hot path:
///
/// 1. Validate the manifest (semver, capability allowlist sanity).
/// 2. Compile the WASM module.
/// 3. Build a `Linker` with baseline host-fns + every gated host-fn the
///    manifest authorises. Imports the plugin asks for that aren't authorised
///    are simply absent → wasmi reports an unresolved-import error at step 4.
/// 4. Instantiate. This is where the capability gate fires.
/// 5. Call `_init` if exported and check its return code.
pub fn load_from_bytes(engine: &Engine, manifest: Manifest, wasm: &[u8]) -> Result<LoadedPlugin> {
    validate(&manifest).with_context(|| {
        format!("manifest validation for {}", manifest.plugin.name)
    })?;

    let caps = CapabilitySet::from_manifest_table(&manifest.capabilities);
    let module = Module::new(engine, wasm).context("compile WASM")?;

    let host_state = HostState::new(manifest.plugin.name.clone(), caps.clone());
    let mut store = Store::new(engine, host_state);

    let mut linker = <wasmi::Linker<HostState>>::new(engine);
    link_baseline(&mut linker).context("link baseline host-fns")?;
    link_capabilities(&mut linker, &caps).context("link capability-gated host-fns")?;

    let pre = linker
        .instantiate(&mut store, &module)
        .context("wasmi instantiate (capability gate fires here)")?;
    let instance = pre
        .start(&mut store)
        .context("plugin start function trapped")?;

    if let Ok(init) = instance.get_typed_func::<(), i32>(&store, "_init") {
        let rc = init.call(&mut store, ()).context("_init trapped")?;
        anyhow::ensure!(rc == 0, "_init returned non-zero status {rc}");
    }

    Ok(LoadedPlugin {
        manifest,
        store,
        instance,
    })
}
