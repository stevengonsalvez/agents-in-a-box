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
    /// A plugin process hosted by another surface (#1040): the hangar plugin
    /// inside a TUI or a desktop shell. Its hello names that host separately,
    /// so its own `pid` stays the plugin's and a host is never misnamed.
    Plugin,
    /// Legacy or unrecognised client which supplied no surface metadata, or a
    /// kind a newer client sends that this build does not know.
    #[serde(other)]
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
            Self::Plugin => "plugin",
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

/// The surface hosting a plugin connection (#1040): what kind it is and its
/// process id, as the plugin runtime handed them to the plugin at init.
///
/// A claim only. The daemon folds a plugin's transient connection into its
/// host's presence only when `pid` is the connection's peer process or that
/// peer's parent, so a plugin cannot hide behind a surface it does not run in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SurfaceHost {
    /// The hosting surface's kind (`tui`, `desktop`, ...).
    pub kind: SurfaceKind,
    /// The hosting surface's process id.
    pub pid: u32,
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
    /// For a [`SurfaceKind::Plugin`] connection, the surface hosting it, set by
    /// the daemon only when the kernel backed the plugin's host claim (#1040).
    /// `None` for every other connection, and for an unbacked claim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_surface: Option<SurfaceHost>,
}

impl ConnectionRow {
    /// The surface a person acting through this connection sat at (#1073): a
    /// verified plugin's host (the TUI whose Hangar screen sent an answer),
    /// otherwise the connection's own surface kind. An unbacked host claim is
    /// attributed to the plugin itself, so it cannot borrow a surface's name.
    #[must_use]
    pub fn attributed_kind(&self) -> SurfaceKind {
        self.host_surface.map_or(self.surface.kind, |host| host.kind)
    }
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

    /// #1040: the plugin kind has a stable name, and a kind this build does not
    /// know decodes as `unknown` instead of refusing the whole hello.
    #[test]
    fn a_plugin_kind_is_named_and_an_unknown_kind_still_decodes() {
        assert_eq!(
            serde_json::to_string(&SurfaceKind::Plugin).unwrap(),
            "\"plugin\""
        );
        assert_eq!(SurfaceKind::Plugin.as_str(), "plugin");
        let later: SurfaceKind = serde_json::from_str("\"watch\"").expect("decodes");
        assert_eq!(later, SurfaceKind::Unknown);
        let host: SurfaceHost =
            serde_json::from_value(serde_json::json!({"kind": "desktop", "pid": 42})).unwrap();
        assert_eq!(
            host,
            SurfaceHost {
                kind: SurfaceKind::Desktop,
                pid: 42
            }
        );
    }

    /// #1073: a verified plugin acts for its host; anything else for itself.
    #[test]
    fn a_verified_plugin_row_is_attributed_to_its_host() {
        let row = |kind, host_surface| ConnectionRow {
            conn_id: 1,
            surface: SurfaceInfo { kind, pid: 7 },
            host: "box".into(),
            connected_at: DateTime::<Utc>::UNIX_EPOCH,
            tmux_clients: Vec::new(),
            host_surface,
        };
        let tui = Some(SurfaceHost {
            kind: SurfaceKind::Tui,
            pid: 5,
        });
        assert_eq!(
            row(SurfaceKind::Plugin, tui).attributed_kind(),
            SurfaceKind::Tui
        );
        assert_eq!(
            row(SurfaceKind::Plugin, None).attributed_kind(),
            SurfaceKind::Plugin
        );
        assert_eq!(
            row(SurfaceKind::Web, None).attributed_kind(),
            SurfaceKind::Web
        );
        let wire = serde_json::to_value(row(SurfaceKind::Web, None)).unwrap();
        assert!(
            wire.get("host_surface").is_none(),
            "absent unless a host was verified"
        );
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
