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
    /// Open cancellable streaming subscriptions via `host/event_stream_subscribe`.
    /// List form is a topic-prefix allow-list (e.g. `["workspace:*"]`);
    /// bool-true grants wildcard subscribe across all topics.
    #[serde(default)]
    pub event_stream_subscribe: CapabilityGrant,
    /// Ask the host to spawn host-supervised child processes via
    /// `host/spawn_managed_subprocess`. List form is a mandatory allow-list
    /// of binary names/paths (e.g. `["ainb-hangar-daemon"]`); a bool-true
    /// grant is rejected at manifest validation (`-32003`) so there is no
    /// way to request an unrestricted "spawn anything" grant.
    #[serde(default)]
    pub spawn_managed_subprocess: CapabilityGrant,
    /// Dial whitelisted `AF_UNIX` sockets via `host/unix_socket_dial`. List
    /// form is a mandatory allow-list of socket paths (e.g.
    /// `["~/.ainb/hangar.sock"]`); a bool-true grant is rejected at
    /// manifest validation (`-32003`) so there is no way to request an
    /// unrestricted "dial any socket" grant. The host canonicalizes the
    /// requested path (symlink resolution) before comparing it against the
    /// list, defending shared dev boxes against arbitrary `AF_UNIX` abuse.
    #[serde(default)]
    pub unix_socket_dial: CapabilityGrant,
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
                event_stream_subscribe: CapabilityGrant::List(vec!["workspace:*".into()]),
                spawn_managed_subprocess: CapabilityGrant::List(vec![
                    "ainb-hangar-daemon".into()
                ]),
                unix_socket_dial: CapabilityGrant::List(vec!["~/.ainb/hangar.sock".into()]),
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
    fn event_stream_subscribe_cap_round_trips_list_form() {
        // List form whitelists topic prefixes (e.g. `["workspace:*"]`).
        let toml_src = r#"
[plugin]
name = "hangar-tui"
version = "0.1.0"
abi_version = 2

[capabilities]
event_stream_subscribe = ["workspace:*", "stream:*"]
"#;
        let m: Manifest = toml::from_str(toml_src).unwrap();
        assert_eq!(
            m.capabilities.event_stream_subscribe.allow_list().unwrap(),
            ["workspace:*", "stream:*"]
        );
        // Round-trips byte-stable.
        let s = toml::to_string(&m).unwrap();
        let back: Manifest = toml::from_str(&s).unwrap();
        assert_eq!(m, back);
    }

    #[test]
    fn event_stream_subscribe_cap_round_trips_bool_form() {
        // Bool-true grant = wildcard subscribe.
        let toml_src = r#"
[plugin]
name = "x"
version = "1.0.0"
abi_version = 2

[capabilities]
event_stream_subscribe = true
"#;
        let m: Manifest = toml::from_str(toml_src).unwrap();
        assert!(matches!(
            m.capabilities.event_stream_subscribe,
            CapabilityGrant::Bool(true)
        ));
        let s = toml::to_string(&m).unwrap();
        let back: Manifest = toml::from_str(&s).unwrap();
        assert_eq!(m, back);
    }

    #[test]
    fn event_stream_subscribe_cap_defaults_denied() {
        let toml_src = r#"
[plugin]
name = "x"
version = "1.0.0"
abi_version = 2
"#;
        let m: Manifest = toml::from_str(toml_src).unwrap();
        assert!(!m.capabilities.event_stream_subscribe.is_granted());
    }

    #[test]
    fn spawn_managed_subprocess_cap_round_trips_list_form() {
        // List form whitelists binary names/paths.
        let toml_src = r#"
[plugin]
name = "hangar-tui"
version = "0.1.0"
abi_version = 2

[capabilities]
spawn_managed_subprocess = ["ainb-hangar-daemon"]
"#;
        let m: Manifest = toml::from_str(toml_src).unwrap();
        assert_eq!(
            m.capabilities.spawn_managed_subprocess.allow_list().unwrap(),
            ["ainb-hangar-daemon"]
        );
        let s = toml::to_string(&m).unwrap();
        let back: Manifest = toml::from_str(&s).unwrap();
        assert_eq!(m, back);
    }

    #[test]
    fn spawn_managed_subprocess_cap_round_trips_bool_form() {
        // Bool-true grant survives the manifest round-trip at the SCHEMA
        // level (semantic rejection happens at validation / handler time,
        // returning -32003 — not here).
        let toml_src = r#"
[plugin]
name = "x"
version = "1.0.0"
abi_version = 2

[capabilities]
spawn_managed_subprocess = true
"#;
        let m: Manifest = toml::from_str(toml_src).unwrap();
        assert!(matches!(
            m.capabilities.spawn_managed_subprocess,
            CapabilityGrant::Bool(true)
        ));
        let s = toml::to_string(&m).unwrap();
        let back: Manifest = toml::from_str(&s).unwrap();
        assert_eq!(m, back);
    }

    #[test]
    fn spawn_managed_subprocess_cap_defaults_denied() {
        let toml_src = r#"
[plugin]
name = "x"
version = "1.0.0"
abi_version = 2
"#;
        let m: Manifest = toml::from_str(toml_src).unwrap();
        assert!(!m.capabilities.spawn_managed_subprocess.is_granted());
    }

    #[test]
    fn unix_socket_dial_cap_round_trips_list_form() {
        // List form whitelists socket paths.
        let toml_src = r#"
[plugin]
name = "hangar-tui"
version = "0.1.0"
abi_version = 2

[capabilities]
unix_socket_dial = ["~/.ainb/hangar.sock", "${XDG_RUNTIME_DIR}/ainb-hangar.sock"]
"#;
        let m: Manifest = toml::from_str(toml_src).unwrap();
        assert_eq!(
            m.capabilities.unix_socket_dial.allow_list().unwrap(),
            ["~/.ainb/hangar.sock", "${XDG_RUNTIME_DIR}/ainb-hangar.sock"]
        );
        let s = toml::to_string(&m).unwrap();
        let back: Manifest = toml::from_str(&s).unwrap();
        assert_eq!(m, back);
    }

    #[test]
    fn unix_socket_dial_cap_round_trips_bool_form() {
        // Bool-true grant survives the manifest round-trip at the SCHEMA
        // level (semantic rejection — `-32003` — happens at validation /
        // handler time, not here).
        let toml_src = r#"
[plugin]
name = "x"
version = "1.0.0"
abi_version = 2

[capabilities]
unix_socket_dial = true
"#;
        let m: Manifest = toml::from_str(toml_src).unwrap();
        assert!(matches!(
            m.capabilities.unix_socket_dial,
            CapabilityGrant::Bool(true)
        ));
        let s = toml::to_string(&m).unwrap();
        let back: Manifest = toml::from_str(&s).unwrap();
        assert_eq!(m, back);
    }

    #[test]
    fn unix_socket_dial_cap_defaults_denied() {
        let toml_src = r#"
[plugin]
name = "x"
version = "1.0.0"
abi_version = 2
"#;
        let m: Manifest = toml::from_str(toml_src).unwrap();
        assert!(!m.capabilities.unix_socket_dial.is_granted());
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
}
