//! Settings-screen wire snapshots (`hangar/health`, providers, keys, workspaces).
//!
//! The settings screen (P4.7) renders four sections from daemon RPC snapshots:
//! the daemon health, the registered LLM providers, the (masked) stored keys, and
//! the workspaces the caller can switch to. These are **pure wire types** —
//! `serde` only, no host deps — matching the rest of `ainb-hangar-proto`.
//!
//! No key *material* ever rides these types: [`KeyRow`] carries only a
//! pre-masked display string. The real secret flows through the
//! `host/secret_store_get` capability and the plugin-side `KeyMaterial` newtype
//! (whose `Debug` redacts), never over this snapshot.

use serde::{Deserialize, Serialize};

/// Daemon health snapshot (`hangar/health`): the daemon-connection section's
/// source of truth.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthSnapshot {
    /// The unix socket path the daemon listens on.
    pub socket_path: String,
    /// The daemon process id.
    pub pid: u32,
    /// Daemon uptime in whole seconds.
    pub uptime_secs: u64,
    /// Daemon version string.
    pub version: String,
    /// Whether the plugin's stream is currently connected.
    pub connected: bool,
}

/// A registered LLM provider row (claude, codex, gemini, copilot, …).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderRow {
    /// Provider name.
    pub name: String,
    /// Whether the provider is currently reachable.
    pub online: bool,
}

/// A stored-key row — *masked only*. Never carries raw key material.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyRow {
    /// The provider this key authenticates.
    pub provider: String,
    /// A pre-masked display form (e.g. `sk-…abcd`); never the real value.
    pub masked: String,
}

/// A workspace row for the workspace-switch section (P5.5).
///
/// The settings Workspace pane renders these as a table: `slug | name |
/// default? | active?`. Switching keys on the stable ULID `id`, never the
/// `slug` (the recently-fixed slug/id conflation bug): `slug` is display-only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceRow {
    /// Stable ULID workspace id — what `s`/`d` switch and default on.
    pub id: String,
    /// Short display handle (e.g. `default`). Display-only.
    #[serde(default)]
    pub slug: String,
    /// Workspace display name.
    pub name: String,
    /// Whether this is the currently-active workspace.
    pub current: bool,
    /// Whether this is the configured default workspace.
    #[serde(default)]
    pub default: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The settings snapshots round-trip through JSON.
    #[test]
    fn snapshots_roundtrip() {
        let health = HealthSnapshot {
            socket_path: "/tmp/h.sock".into(),
            pid: 1,
            uptime_secs: 2,
            version: "0.1.0".into(),
            connected: true,
        };
        let s = serde_json::to_string(&health).unwrap();
        assert_eq!(serde_json::from_str::<HealthSnapshot>(&s).unwrap(), health);

        let key = KeyRow { provider: "claude".into(), masked: "sk-…ab".into() };
        let s = serde_json::to_string(&key).unwrap();
        assert_eq!(serde_json::from_str::<KeyRow>(&s).unwrap(), key);
    }

    /// A `KeyRow`'s masked form must not look like a full secret (defensive: the
    /// daemon owns masking, but assert the wire type carries no raw field).
    #[test]
    fn key_row_carries_only_masked() {
        let key = KeyRow { provider: "claude".into(), masked: "sk-…ab".into() };
        let v = serde_json::to_value(&key).unwrap();
        assert!(v.get("value").is_none(), "KeyRow must not carry a raw value");
        assert!(v.get("masked").is_some());
    }
}
