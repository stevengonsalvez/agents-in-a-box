//! Wire types for the daemon's live surface connection registry.

use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// The kind of surface connected to the Hangar daemon.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SurfaceKind {
    /// Terminal TUI surface.
    Tui,
    /// Browser web surface.
    Web,
    /// Native desktop surface.
    Desktop,
    /// Command-line client.
    Cli,
    /// Copilot integration.
    Copilot,
    /// Legacy or unrecognised client which supplied no surface metadata.
    Unknown,
}

impl SurfaceKind {
    /// Stable lowercase label used in daemon-stamped provenance.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Tui => "tui",
            Self::Web => "web",
            Self::Desktop => "desktop",
            Self::Cli => "cli",
            Self::Copilot => "copilot",
            Self::Unknown => "unknown",
        }
    }
}

impl fmt::Display for SurfaceKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Metadata supplied by a client during `auth/hello`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SurfaceInfo {
    /// Client surface category.
    pub kind: SurfaceKind,
    /// Client process identifier.
    pub pid: u32,
}

impl SurfaceInfo {
    /// Metadata for a legacy hello frame which omitted the optional surface.
    #[must_use]
    pub const fn unknown() -> Self {
        Self {
            kind: SurfaceKind::Unknown,
            pid: 0,
        }
    }
}

/// One live authenticated connection, stamped by the daemon.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectionRow {
    /// Daemon-local connection id, unique until daemon restart.
    pub conn_id: u64,
    /// Client-declared surface metadata, or [`SurfaceKind::Unknown`].
    pub surface: SurfaceInfo,
    /// Hostname of the daemon process, never client input.
    pub host: String,
    /// Time the daemon accepted the authenticated hello frame.
    pub connected_at: DateTime<Utc>,
    /// Tmux clients observed by the daemon's periodic probe.
    pub tmux_clients: Vec<String>,
}

/// Result of `hangar/connections_list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectionsListResult {
    /// All currently authenticated live connections.
    pub connections: Vec<ConnectionRow>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn surface_kind_uses_stable_wire_and_provenance_names() {
        let encoded = serde_json::to_string(&SurfaceKind::Tui).expect("kind serializes");
        assert_eq!(encoded, "\"tui\"");
        assert_eq!(SurfaceKind::Unknown.as_str(), "unknown");
    }

    #[test]
    fn unknown_surface_is_an_explicit_legacy_value() {
        assert_eq!(
            SurfaceInfo::unknown(),
            SurfaceInfo {
                kind: SurfaceKind::Unknown,
                pid: 0,
            }
        );
    }
}
