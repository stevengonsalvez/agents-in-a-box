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

/// The number of throughput samples in a daemon-health snapshot — one per second
/// over the last rolling minute (P8.5). The daemon's ring buffer (and the
/// sparkline that renders it) is fixed at this width.
pub const THROUGHPUT_WINDOW: usize = 60;

/// One second's task-completion tally in the daemon's rolling throughput window
/// (`hangar/daemon_health`, P8.5).
///
/// The dual-dim sparkline encodes both signals independently: the cell **height**
/// is the total throughput (`completed + failed`), and the **red proportion** of
/// the cell is the failure rate (`failed / (completed + failed)`). A bucket with
/// no terminal tasks renders an empty (zero-height) cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThroughputSample {
    /// Bucket timestamp (epoch seconds) — the second this tally covers.
    pub ts: i64,
    /// Tasks that finished successfully (`done`) in this second.
    pub completed: u32,
    /// Tasks that finished unsuccessfully (`failed` / `cancelled`) in this second.
    pub failed: u32,
}

/// The daemon's bounded claim-slot cache occupancy (`hangar/daemon_health`,
/// P8.5).
///
/// `used / capacity` slots are in use; the health pane renders it as a fill bar.
/// `capacity` is the daemon's configured concurrency ceiling (a fixed view-layer
/// figure, not a tuned metric).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimCache {
    /// Slots currently in use.
    pub used: u32,
    /// Total slot capacity.
    pub capacity: u32,
}

/// One registered runtime endpoint in the daemon-health pane (`hangar/daemon_health`,
/// P8.5).
///
/// Flattened from an `agent_runtime` row (workspace-scoped): the provider, its
/// liveness status, and the daemon pid hosting it. `connected` folds the raw
/// status token (`"online"`) into a boolean the pane renders as a presence dot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeHealthRow {
    /// Provider name (e.g. `"claude"`).
    pub provider: String,
    /// Whether the runtime is connected (`status == "online"`).
    pub connected: bool,
    /// The daemon process id hosting this runtime.
    pub pid: u32,
}

/// The daemon-health snapshot (`hangar/daemon_health`, P8.5): the view-layer
/// health pane's source of truth.
///
/// All in-memory + read-model facts the daemon-health screen (`D`) renders:
/// registered runtimes (from the `agent_runtime` table), the bounded claim-slot
/// cache occupancy, the count of concurrently-executing tasks (from
/// `agent_task_queue`), and the rolling 60-second task-throughput window. This is
/// a snapshot, **not** an aggregate table — the daemon never persists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonHealthSnapshot {
    /// Registered provider runtimes in the workspace.
    pub runtimes: Vec<RuntimeHealthRow>,
    /// The bounded claim-slot cache occupancy.
    pub claim_cache: ClaimCache,
    /// Tasks currently `dispatched` or `running`.
    pub concurrent_tasks: u32,
    /// The rolling per-second throughput window — exactly [`THROUGHPUT_WINDOW`]
    /// samples, oldest-first, the last sample being the most recent whole second.
    pub task_throughput_60s: Vec<ThroughputSample>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The daemon-health snapshot round-trips through JSON, preserving the full
    /// 60-sample throughput window and the dual-dim per-sample tallies (P8.5).
    #[test]
    fn daemon_health_snapshot_roundtrips() {
        let snap = DaemonHealthSnapshot {
            runtimes: vec![RuntimeHealthRow {
                provider: "claude".into(),
                connected: true,
                pid: 14829,
            }],
            claim_cache: ClaimCache {
                used: 12,
                capacity: 64,
            },
            concurrent_tasks: 3,
            task_throughput_60s: (0..THROUGHPUT_WINDOW)
                .map(|i| ThroughputSample {
                    ts: 1_700_000_000 + i64::try_from(i).unwrap(),
                    completed: u32::try_from(i).unwrap(),
                    failed: u32::from(i == 30),
                })
                .collect(),
        };
        let s = serde_json::to_string(&snap).unwrap();
        let back: DaemonHealthSnapshot = serde_json::from_str(&s).unwrap();
        assert_eq!(back, snap);
        assert_eq!(back.task_throughput_60s.len(), THROUGHPUT_WINDOW);
        assert_eq!(back.task_throughput_60s[30].failed, 1);
    }

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
