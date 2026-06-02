//! Manifest v2 schema (TOML &harr; Rust).
//!
//! Lives in `~/.agents-in-a-box/plugins/<name>/manifest.toml` and is
//! read at host startup (discovery) and again at `plugin/init` for
//! validation. Schema layout:
//!
//! ```toml
//! [plugin]
//! name = "burndown"
//! version = "2.0.0"
//! abi_version = 2
//! description = "Daily/weekly/project usage burndown panels"
//!
//! [capabilities]
//! read_sessions = true            # bool form
//! network = ["api.example.com"]   # list form (allow-list of hosts)
//!
//! [provides]
//! screens = ["analytics"]
//! commands = ["/usage"]
//! cli_namespaces = ["usage"]
//! snapshots = ["sessions.usage_data"]
//!
//! [subscribes]
//! snapshots = ["sessions.usage_data"]
//!
//! [lifecycle]
//! spawn = "lazy"
//! idle_reap_secs = 600
//! ```

use serde::{Deserialize, Serialize};

/// Top-level manifest. Field names match the TOML section headers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    /// Identity + ABI compatibility metadata.
    pub plugin: PluginMeta,
    /// Capability set the plugin requests at install time.
    #[serde(default)]
    pub capabilities: Capabilities,
    /// What the plugin offers (screens, commands, CLI namespaces, snapshot topics).
    #[serde(default)]
    pub provides: Provides,
    /// Snapshot topics the plugin wants pushed to it via `plugin/handle_event`.
    #[serde(default)]
    pub subscribes: Subscribes,
    /// Spawn timing + idle reap policy.
    #[serde(default)]
    pub lifecycle: Lifecycle,
    /// User-configurable variables (flat scalars), declared via `[[config]]`.
    /// Resolved values land in config.toml `[plugins.<name>]` and are injected
    /// at `plugin/init` (see [`crate::params::PluginInitParams::config`]).
    #[serde(default)]
    pub config: Vec<ConfigField>,
}

/// The value type of a `[[config]]` field — drives which Settings widget the
/// host renders and how the plugin parses the injected value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ConfigKind {
    /// Filesystem path (text input, `~`-expansion done by the plugin).
    Path,
    /// Free-form string (text input).
    String,
    /// Boolean toggle (choice widget).
    Bool,
    /// One of an enumerated `choices` set (choice widget).
    Enum,
    /// Integer (number input).
    Int,
}

/// One user-configurable variable declared by a plugin's `[[config]]` block.
///
/// All fields are flat scalars (no nested tables / lists of records) — the
/// host renders one Settings row per field and persists the resolved value to
/// config.toml `[plugins.<name>].<key>`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigField {
    /// Config key. Stored verbatim under `[plugins.<name>]`.
    pub key: String,
    /// Value type — selects the Settings widget + plugin-side parse.
    pub kind: ConfigKind,
    /// Human-readable label shown in the Settings screen.
    pub label: String,
    /// Default value (as a string; the plugin parses per `kind`).
    #[serde(default)]
    pub default: String,
    /// Allowed values when `kind == Enum`. Empty for other kinds.
    #[serde(default)]
    pub choices: Vec<String>,
}

/// `[plugin]` section.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginMeta {
    /// Plugin identifier — must match the directory name under `plugins/`.
    pub name: String,
    /// Semver release version of the plugin binary.
    pub version: String,
    /// Wire protocol ABI revision the plugin targets. Host gates compatibility on this.
    pub abi_version: u32,
    /// Free-form one-line description shown in the plugin picker.
    #[serde(default)]
    pub description: String,
}

/// Either an unconditional grant (`true` / `false`) or a constrained
/// allow-list of values (paths, hostnames, action names, ...).
///
/// Serialized as the TOML primitive (bool) or array of strings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CapabilityGrant {
    /// `true` = unconditional grant; `false` = explicit denial.
    Bool(bool),
    /// Allow-list of values the capability is constrained to.
    List(Vec<String>),
}

impl Default for CapabilityGrant {
    fn default() -> Self {
        Self::Bool(false)
    }
}

impl CapabilityGrant {
    /// Returns true iff the capability is granted in *any* form
    /// (unconditional `true` or a non-empty allow-list).
    #[must_use]
    pub const fn is_granted(&self) -> bool {
        match self {
            Self::Bool(b) => *b,
            Self::List(items) => !items.is_empty(),
        }
    }

    /// Returns the allow-list if this grant is a list, else `None`.
    #[must_use]
    pub fn allow_list(&self) -> Option<&[String]> {
        match self {
            Self::Bool(_) => None,
            Self::List(items) => Some(items),
        }
    }
}

/// `[capabilities]` — every key is a [`CapabilityGrant`] (bool or list).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    /// Read Claude/Codex session files under user data dirs.
    #[serde(default)]
    pub read_sessions: CapabilityGrant,
    /// Write to the plugin's own data dir under `~/.agents-in-a-box/plugins/<name>/`.
    #[serde(default)]
    pub write_plugin_data: CapabilityGrant,
    /// Subscribe + publish on the snapshot/event bus.
    #[serde(default)]
    pub event_bus: CapabilityGrant,
    /// Outbound network — list form is an allow-list of hostnames.
    #[serde(default)]
    pub network: CapabilityGrant,
    /// Spawn auxiliary subprocesses from inside the plugin.
    #[serde(default)]
    pub spawn_subprocess: CapabilityGrant,
    /// Read the Claude session log directory specifically.
    #[serde(default)]
    pub read_claude_logs: CapabilityGrant,
    /// Read the Codex session log directory specifically.
    #[serde(default)]
    pub read_codex_logs: CapabilityGrant,
    /// Path-scoped filesystem read grant. List form is the allow-list of path
    /// prefixes the plugin may read under (the outer envelope; config paths
    /// are a choice within it).
    #[serde(default)]
    pub read_paths: CapabilityGrant,
}

/// `[provides]` — what the plugin contributes to the host's UX.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provides {
    /// TUI screens the plugin owns (e.g., `["analytics"]`).
    #[serde(default)]
    pub screens: Vec<String>,
    /// Slash commands the plugin handles (e.g., `["/usage", "/burndown"]`).
    #[serde(default)]
    pub commands: Vec<String>,
    /// CLI subcommand namespaces the plugin owns (e.g., `["usage"]`).
    #[serde(default)]
    pub cli_namespaces: Vec<String>,
    /// Snapshot topics the plugin publishes.
    #[serde(default)]
    pub snapshots: Vec<String>,
}

/// `[subscribes]` — host pushes these to the plugin via `plugin/handle_event`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Subscribes {
    /// Snapshot topics whose updates the plugin wants.
    #[serde(default)]
    pub snapshots: Vec<String>,
}

/// Plugin spawn policy.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SpawnMode {
    /// Spawn immediately on host startup.
    Eager,
    /// Spawn on first use (any incoming method that targets this plugin).
    #[default]
    Lazy,
}

/// `[lifecycle]` — spawn timing and idle reap.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lifecycle {
    /// When to spawn the plugin process. Defaults to [`SpawnMode::Lazy`].
    #[serde(default)]
    pub spawn: SpawnMode,
    /// Reap the process if idle for this many seconds with no live subscriptions.
    /// `0` disables idle reaping.
    #[serde(default = "default_idle_reap_secs")]
    pub idle_reap_secs: u32,
}

impl Default for Lifecycle {
    fn default() -> Self {
        Self {
            spawn: SpawnMode::default(),
            idle_reap_secs: default_idle_reap_secs(),
        }
    }
}

const fn default_idle_reap_secs() -> u32 {
    600
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Manifest {
        Manifest {
            plugin: PluginMeta {
                name: "burndown".into(),
                version: "2.0.0".into(),
                abi_version: 2,
                description: "Daily/weekly/project usage burndown panels".into(),
            },
            capabilities: Capabilities {
                read_sessions: CapabilityGrant::Bool(true),
                write_plugin_data: CapabilityGrant::Bool(true),
                event_bus: CapabilityGrant::Bool(true),
                network: CapabilityGrant::List(vec!["api.example.com".into()]),
                spawn_subprocess: CapabilityGrant::Bool(false),
                read_claude_logs: CapabilityGrant::Bool(false),
                read_codex_logs: CapabilityGrant::Bool(false),
                read_paths: CapabilityGrant::List(vec!["~/.learnings".into()]),
            },
            provides: Provides {
                screens: vec!["analytics".into()],
                commands: vec!["/usage".into(), "/burndown".into()],
                cli_namespaces: vec!["usage".into()],
                snapshots: vec![],
            },
            subscribes: Subscribes {
                snapshots: vec!["sessions.usage_data".into()],
            },
            lifecycle: Lifecycle {
                spawn: SpawnMode::Lazy,
                idle_reap_secs: 600,
            },
            config: vec![ConfigField {
                key: "learnings_dir".into(),
                kind: ConfigKind::Path,
                label: "Learnings notes directory".into(),
                default: "~/.learnings/documents/learnings".into(),
                choices: vec![],
            }],
        }
    }

    #[test]
    fn round_trip() {
        let m = fixture();
        let s = toml::to_string(&m).unwrap();
        let back: Manifest = toml::from_str(&s).unwrap();
        assert_eq!(m, back);
    }

    #[test]
    fn parses_bool_and_list_capability_forms() {
        let toml_src = r#"
[plugin]
name = "x"
version = "1.0.0"
abi_version = 2

[capabilities]
read_sessions = true
network = ["api.example.com", "raw.githubusercontent.com"]
"#;
        let m: Manifest = toml::from_str(toml_src).unwrap();
        assert!(matches!(
            m.capabilities.read_sessions,
            CapabilityGrant::Bool(true)
        ));
        assert_eq!(
            m.capabilities.network.allow_list().unwrap(),
            ["api.example.com", "raw.githubusercontent.com"]
        );
    }

    #[test]
    fn capability_grant_default_denies() {
        assert!(!CapabilityGrant::default().is_granted());
        assert!(!CapabilityGrant::Bool(false).is_granted());
        assert!(!CapabilityGrant::List(vec![]).is_granted());
        assert!(CapabilityGrant::Bool(true).is_granted());
        assert!(CapabilityGrant::List(vec!["x".into()]).is_granted());
    }

    #[test]
    fn lifecycle_defaults_lazy_600() {
        let toml_src = r#"
[plugin]
name = "x"
version = "1.0.0"
abi_version = 2
"#;
        let m: Manifest = toml::from_str(toml_src).unwrap();
        assert_eq!(m.lifecycle.spawn, SpawnMode::Lazy);
        assert_eq!(m.lifecycle.idle_reap_secs, 600);
    }

    #[test]
    fn missing_plugin_section_rejected() {
        let toml_src = r"
[capabilities]
read_sessions = true
";
        assert!(toml::from_str::<Manifest>(toml_src).is_err());
    }

    // ===================================================================
    // P0 — read_paths capability + [config] schema
    // ===================================================================

    #[test]
    fn test_read_paths_capability_parses_list() {
        // List form: `read_paths = ["~/.learnings"]` → List(["~/.learnings"]), granted.
        let toml_src = r#"
[plugin]
name = "learnings"
version = "0.1.0"
abi_version = 2

[capabilities]
read_paths = ["~/.learnings"]
"#;
        let m: Manifest = toml::from_str(toml_src).unwrap();
        assert_eq!(
            m.capabilities.read_paths.allow_list().unwrap(),
            ["~/.learnings"]
        );
        assert!(m.capabilities.read_paths.is_granted());

        // Absent → defaults to Bool(false), not granted (ABI back-compat).
        let toml_no_paths = r#"
[plugin]
name = "x"
version = "1.0.0"
abi_version = 2

[capabilities]
read_sessions = true
"#;
        let m2: Manifest = toml::from_str(toml_no_paths).unwrap();
        assert!(matches!(
            m2.capabilities.read_paths,
            CapabilityGrant::Bool(false)
        ));
        assert!(!m2.capabilities.read_paths.is_granted());
    }

    #[test]
    fn test_config_schema_roundtrip() {
        // [[config]] entries parse into Manifest.config: Vec<ConfigField>.
        let toml_src = r#"
[plugin]
name = "learnings"
version = "0.1.0"
abi_version = 2

[[config]]
key = "learnings_dir"
kind = "path"
label = "Learnings notes directory"
default = "~/.learnings/documents/learnings"

[[config]]
key = "spawn_mode"
kind = "enum"
label = "Spawn mode"
default = "lazy"
choices = ["lazy", "eager"]
"#;
        let m: Manifest = toml::from_str(toml_src).unwrap();
        assert_eq!(m.config.len(), 2);
        assert_eq!(m.config[0].key, "learnings_dir");
        assert_eq!(m.config[0].kind, ConfigKind::Path);
        assert_eq!(m.config[0].label, "Learnings notes directory");
        assert_eq!(m.config[0].default, "~/.learnings/documents/learnings");
        assert!(m.config[0].choices.is_empty());
        assert_eq!(m.config[1].kind, ConfigKind::Enum);
        assert_eq!(m.config[1].choices, ["lazy", "eager"]);

        // serialize → parse is identity.
        let s = toml::to_string(&m).unwrap();
        let back: Manifest = toml::from_str(&s).unwrap();
        assert_eq!(m, back);

        // Unknown kind errors.
        let bad = r#"
[plugin]
name = "x"
version = "1.0.0"
abi_version = 2

[[config]]
key = "k"
kind = "wormhole"
label = "l"
default = "d"
"#;
        assert!(toml::from_str::<Manifest>(bad).is_err());
    }

    #[test]
    fn test_config_kind_variants() {
        // Each `kind` token deserializes to the matching ConfigKind.
        let toml_src = r#"
[plugin]
name = "x"
version = "1.0.0"
abi_version = 2

[[config]]
key = "a"
kind = "path"
label = "a"
default = ""

[[config]]
key = "b"
kind = "string"
label = "b"
default = ""

[[config]]
key = "c"
kind = "bool"
label = "c"
default = "false"

[[config]]
key = "d"
kind = "enum"
label = "d"
default = ""

[[config]]
key = "e"
kind = "int"
label = "e"
default = "0"
"#;
        let m: Manifest = toml::from_str(toml_src).unwrap();
        let kinds: Vec<ConfigKind> = m.config.iter().map(|f| f.kind).collect();
        assert_eq!(
            kinds,
            [
                ConfigKind::Path,
                ConfigKind::String,
                ConfigKind::Bool,
                ConfigKind::Enum,
                ConfigKind::Int,
            ]
        );
    }
}
