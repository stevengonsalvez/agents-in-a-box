//! Plugin discovery + action → plugin routing.
//!
//! [`RegisteredPlugin`] is the discovery output: manifest already
//! parsed, binary path resolved. The runtime spawns from this.
//! [`ChannelRegistry`] keeps the action → plugin map needed by
//! `host/action/invoke` dispatch.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ainb_plugin_protocol::manifest::Manifest;
use parking_lot::RwLock;

use crate::error::RuntimeError;
use crate::types::PluginId;

/// One discovered plugin: parsed manifest + resolved binary path.
#[derive(Debug, Clone)]
pub struct RegisteredPlugin {
    /// Stable id (matches `manifest.plugin.name`).
    pub id: PluginId,
    /// Parsed manifest.
    pub manifest: Manifest,
    /// Resolved binary path. The runtime `Command::new`s this directly.
    pub binary_path: PathBuf,
    /// Original manifest path (passed back through `plugin/init`).
    pub manifest_path: PathBuf,
}

impl RegisteredPlugin {
    /// Construct a [`RegisteredPlugin`] from already-parsed metadata.
    /// Used by the test harness to skip filesystem discovery.
    #[must_use]
    pub fn new(manifest: Manifest, binary_path: PathBuf, manifest_path: PathBuf) -> Self {
        Self {
            id: PluginId::from(manifest.plugin.name.clone()),
            manifest,
            binary_path,
            manifest_path,
        }
    }
}

/// Discover every plugin under `root`.
///
/// Layout: `<root>/<plugin-name>/manifest.toml` and `<root>/<plugin-name>/<plugin-name>`
/// (the binary). Binary may also live at the path declared in
/// `[plugin].binary` if a future manifest extension surfaces — for now
/// we use the canonical `<root>/<name>/<name>` convention.
pub fn discover(root: &Path) -> Result<Vec<RegisteredPlugin>, RuntimeError> {
    let mut out = Vec::new();
    let entries = match std::fs::read_dir(root) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(e.into()),
    };
    for entry in entries {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let dir = entry.path();
        let manifest_path = dir.join("manifest.toml");
        if !manifest_path.exists() {
            continue;
        }
        let bytes = std::fs::read_to_string(&manifest_path)?;
        let manifest: Manifest = toml::from_str(&bytes)?;
        let binary_path = dir.join(&manifest.plugin.name);
        out.push(RegisteredPlugin::new(manifest, binary_path, manifest_path));
    }
    Ok(out)
}

/// Action name → plugin id (or vector of ids if multiple plugins
/// advertise the same action; the runtime picks the first registered
/// for now and surfaces a warning).
#[derive(Debug, Default, Clone)]
pub struct ChannelRegistry {
    inner: Arc<RwLock<Inner>>,
}

#[derive(Debug, Default)]
struct Inner {
    actions: HashMap<String, Vec<PluginId>>,
    /// Plugins keyed by id for O(1) lookup.
    plugins: HashMap<PluginId, Arc<RegisteredPlugin>>,
}

impl ChannelRegistry {
    /// Construct an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a plugin. Each entry in `manifest.provides.cli_namespaces`
    /// is treated as an action key for now (the wire `host/action/invoke`
    /// `action` field is the only routing input we have today).
    pub fn register(&self, plugin: RegisteredPlugin) {
        let id = plugin.id.clone();
        let actions: Vec<String> = plugin.manifest.provides.cli_namespaces.clone();
        let arc = Arc::new(plugin);
        let mut inner = self.inner.write();
        inner.plugins.insert(id.clone(), arc);
        for action in actions {
            inner.actions.entry(action).or_default().push(id.clone());
        }
    }

    /// Look up the first plugin advertising `action`.
    #[must_use]
    pub fn route_action(&self, action: &str) -> Option<PluginId> {
        self.inner
            .read()
            .actions
            .get(action)
            .and_then(|v| v.first().cloned())
    }

    /// Fetch a registered plugin by id.
    #[must_use]
    pub fn get(&self, id: &PluginId) -> Option<Arc<RegisteredPlugin>> {
        self.inner.read().plugins.get(id).cloned()
    }

    /// All registered plugin ids in registration order.
    #[must_use]
    pub fn ids(&self) -> Vec<PluginId> {
        self.inner.read().plugins.keys().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ainb_plugin_protocol::manifest::{
        Capabilities, Lifecycle, PluginMeta, Provides, SpawnMode, Subscribes,
    };

    fn manifest(name: &str, cli: &[&str]) -> Manifest {
        Manifest {
            plugin: PluginMeta {
                name: name.into(),
                version: "1.0.0".into(),
                abi_version: 2,
                description: String::new(),
            },
            capabilities: Capabilities::default(),
            provides: Provides {
                cli_namespaces: cli.iter().map(|s| (*s).into()).collect(),
                ..Provides::default()
            },
            subscribes: Subscribes::default(),
            lifecycle: Lifecycle {
                spawn: SpawnMode::Lazy,
                idle_reap_secs: 600,
            },
        }
    }

    #[test]
    fn register_routes_actions() {
        let r = ChannelRegistry::new();
        r.register(RegisteredPlugin::new(
            manifest("burndown", &["usage", "burn"]),
            PathBuf::from("/bin/burndown"),
            PathBuf::from("/etc/manifest.toml"),
        ));
        assert_eq!(
            r.route_action("usage"),
            Some(PluginId::from("burndown"))
        );
        assert_eq!(
            r.route_action("burn"),
            Some(PluginId::from("burndown"))
        );
        assert!(r.route_action("missing").is_none());
    }

    #[test]
    fn discover_handles_missing_root() {
        // Discovery on a nonexistent path returns empty, not error.
        let dir = std::env::temp_dir().join("ainb-runtime-test-nonexistent-path");
        let _ = std::fs::remove_dir_all(&dir);
        let plugins = discover(&dir).unwrap();
        assert!(plugins.is_empty());
    }

    #[test]
    fn discover_picks_up_manifests() {
        let tmp = tempfile::tempdir().unwrap();
        let plugin_dir = tmp.path().join("burndown");
        std::fs::create_dir(&plugin_dir).unwrap();
        std::fs::write(
            plugin_dir.join("manifest.toml"),
            r#"
[plugin]
name = "burndown"
version = "1.0.0"
abi_version = 2
"#,
        )
        .unwrap();
        let plugins = discover(tmp.path()).unwrap();
        assert_eq!(plugins.len(), 1);
        assert_eq!(plugins[0].id, PluginId::from("burndown"));
    }
}
