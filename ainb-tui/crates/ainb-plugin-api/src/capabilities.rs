//! Capability declarations.
//!
//! A plugin's `[capabilities]` table in `plugin.toml` decides which host-fn imports
//! the host links into the WASM instance. Capabilities the plugin did not declare
//! are simply not provided — calls to those imports cause a wasmi link error at
//! instantiation time, not a runtime trap. This is the "refused statically"
//! property.

use serde::{Deserialize, Serialize};

/// One capability the plugin can request.
///
/// `Network` and `Filesystem` carry allowlists; the host enforces those when the
/// plugin calls the corresponding host-fns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    /// Read session metadata via `ainb_sessions_list` / `ainb_session_get`.
    ReadSessions,
    /// Read/write the plugin's scoped data dir via `ainb_data_*`.
    WritePluginData,
    /// Subscribe / publish on the cross-plugin event bus.
    EventBus,
    /// Read parsed Claude session logs.
    ReadClaudeLogs,
    /// Read parsed Codex session logs.
    ReadCodexLogs,
    /// Spawn host subprocesses (reserved — gated separately in MVP).
    SpawnSubprocess,
    /// Outbound HTTP via `ainb_http_request` to the listed hosts.
    Network(Vec<String>),
    /// Read host filesystem via `ainb_fs_read` / `ainb_fs_glob` matching the
    /// listed glob patterns.
    Filesystem(Vec<String>),
}

/// The full set of capabilities a plugin has been granted.
///
/// Built from the manifest by the host loader; consumed by the capability gate
/// to decide which imports to link.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilitySet {
    pub read_sessions: bool,
    pub write_plugin_data: bool,
    pub event_bus: bool,
    pub read_claude_logs: bool,
    pub read_codex_logs: bool,
    pub spawn_subprocess: bool,
    /// Allowlist of host:port pairs the plugin may reach.
    pub network: Vec<String>,
    /// Allowlist of glob patterns the plugin may read.
    pub filesystem: Vec<String>,
}

impl CapabilitySet {
    #[must_use]
    pub fn from_manifest_table(t: &crate::manifest::CapabilitiesTable) -> Self {
        Self {
            read_sessions: t.read_sessions,
            write_plugin_data: t.write_plugin_data,
            event_bus: t.event_bus,
            read_claude_logs: t.read_claude_logs,
            read_codex_logs: t.read_codex_logs,
            spawn_subprocess: t.spawn_subprocess,
            network: t.network.clone(),
            filesystem: t.filesystem.clone(),
        }
    }
}
