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
    #[serde(default)]
    pub paths: PathsTable,
    #[serde(default)]
    pub subscribes: SubscribesTable,
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

/// `[paths]` table — declared by data-plane plugins (e.g. session-reader)
/// that walk per-provider log directories. Each field is an optional
/// path string; `$HOME` is expanded by the host at link time so plugins
/// don't need to know the user's home directory. A `None` field falls
/// back to the host's built-in default for that provider.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PathsTable {
    #[serde(default)]
    pub claude_projects: Option<String>,
    #[serde(default)]
    pub codex_sessions: Option<String>,
    #[serde(default)]
    pub gemini_sessions: Option<String>,
    #[serde(default)]
    pub copilot_sessions: Option<String>,
}

/// `[subscribes]` table — declarative event subscriptions.
///
/// Plugins listing topics here are auto-subscribed by the host loader
/// after `_init` runs successfully. Subsequent publishes on those
/// topics flow through `pump_events` into the plugin's `_handle_event`
/// export. This avoids forcing every subscriber to declare the
/// `event_bus` capability just to call `ainb_event_subscribe` once at
/// startup.
///
/// Example:
///
/// ```toml
/// [subscribes]
/// topics = ["sessions.usage_data", "sessions.partial_progress"]
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubscribesTable {
    /// Topic names the host should subscribe this plugin to at load
    /// time. Each must match the publisher's exact topic string —
    /// no globbing.
    #[serde(default)]
    pub topics: Vec<String>,
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
    fn parses_manifest_with_paths_block() {
        let src = r#"
            [plugin]
            name = "session-reader"
            version = "0.1.0"
            ainb_min_version = "1.2.0"

            [capabilities]
            read_claude_logs = true
            read_codex_logs = true
            write_plugin_data = true

            [paths]
            claude_projects = "$HOME/.claude/projects"
            codex_sessions  = "$HOME/.codex/sessions"
        "#;
        let m = Manifest::from_toml(src).expect("parses");
        assert_eq!(
            m.paths.claude_projects.as_deref(),
            Some("$HOME/.claude/projects")
        );
        assert_eq!(
            m.paths.codex_sessions.as_deref(),
            Some("$HOME/.codex/sessions")
        );
        assert!(m.paths.gemini_sessions.is_none());
        assert!(m.paths.copilot_sessions.is_none());
    }

    #[test]
    fn paths_block_defaults_to_all_none() {
        let src = r#"
            [plugin]
            name = "p"
            version = "0.1.0"
            ainb_min_version = "1.2.0"
        "#;
        let m = Manifest::from_toml(src).expect("parses");
        assert!(m.paths.claude_projects.is_none());
        assert!(m.paths.codex_sessions.is_none());
        assert!(m.paths.gemini_sessions.is_none());
        assert!(m.paths.copilot_sessions.is_none());
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
