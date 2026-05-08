//! `plugin.toml` schema.

use serde::{Deserialize, Serialize};

/// Top-level manifest. Loaded from `plugin.toml` next to `plugin.wasm` in the
/// plugin cache directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub plugin: PluginTable,
    #[serde(default)]
    pub capabilities: CapabilitiesTable,
    #[serde(default)]
    pub provides: ProvidesTable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginTable {
    pub name: String,
    pub version: String,
    /// Minimum compatible host version (semver).
    pub ainb_min_version: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default)]
    pub license: Option<String>,
    #[serde(default)]
    pub homepage: Option<String>,
}

/// `[capabilities]` table — see [`crate::capabilities::Capability`] for semantics.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilitiesTable {
    #[serde(default)]
    pub read_sessions: bool,
    #[serde(default)]
    pub write_plugin_data: bool,
    #[serde(default)]
    pub event_bus: bool,
    #[serde(default)]
    pub read_claude_logs: bool,
    #[serde(default)]
    pub read_codex_logs: bool,
    #[serde(default)]
    pub spawn_subprocess: bool,
    #[serde(default)]
    pub network: Vec<String>,
    #[serde(default)]
    pub filesystem: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProvidesTable {
    #[serde(default)]
    pub screens: Vec<String>,
    #[serde(default)]
    pub commands: Vec<String>,
    #[serde(default)]
    pub sidebar: Vec<String>,
    #[serde(default)]
    pub statusline: Vec<String>,
    #[serde(default)]
    pub providers: Vec<String>,
}

impl Manifest {
    /// Parse a `plugin.toml` source. Returns the parsed manifest or a `toml`
    /// error.
    pub fn from_toml(s: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_manifest() {
        let src = r#"
            [plugin]
            name = "hello"
            version = "0.1.0"
            ainb_min_version = "1.1.0"
        "#;
        let m = Manifest::from_toml(src).expect("parses");
        assert_eq!(m.plugin.name, "hello");
        assert_eq!(m.plugin.version, "0.1.0");
        assert!(!m.capabilities.read_sessions);
    }

    #[test]
    fn parses_full_manifest_with_capabilities() {
        let src = r#"
            [plugin]
            name = "burndown"
            version = "0.1.0"
            ainb_min_version = "1.1.0"
            description = "Usage analytics"

            [capabilities]
            read_sessions = true
            read_claude_logs = true
            write_plugin_data = true
            network = ["api.example.com:443"]
            filesystem = ["~/.claude/projects/**"]

            [provides]
            screens = ["analytics"]
            commands = ["/usage", "/burndown"]
        "#;
        let m = Manifest::from_toml(src).expect("parses");
        assert!(m.capabilities.read_sessions);
        assert_eq!(m.capabilities.network, vec!["api.example.com:443"]);
        assert_eq!(m.provides.screens, vec!["analytics"]);
        assert_eq!(m.provides.commands.len(), 2);
    }
}
