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
    /// The answering daemon's version string.
    ///
    /// `#[serde(default)]` so a snapshot from a daemon that predates this field
    /// still decodes — the empty string is itself the signal the pane renders as
    /// "stale daemon binary".
    #[serde(default)]
    pub daemon_version: String,
    /// Live database-drift diagnosis, `None` when healthy.
    ///
    /// `Some` when the applied schema is AHEAD of the answering binary's
    /// embedded migrations (a stale daemon serving a newer database — every
    /// pane silently renders zeros) or when the probe query fails outright.
    /// The pane renders this as a loud red banner instead of empty stats.
    #[serde(default)]
    pub db_error: Option<String>,
    /// The ACP agent pool, or `None` when this daemon runs no pool.
    ///
    /// `#[serde(default)]` so a snapshot from a daemon that predates the pool
    /// still decodes as "no pool", which is indistinguishable from the truth.
    #[serde(default)]
    pub acp_pool: Option<AcpPoolHealth>,
}

/// The ACP agent pool's live shape (`hangar/daemon_health`).
///
/// "Why is the copilot stuck?" must be answerable from ONE pane: queue depth,
/// oldest in-flight turn age, and breaker state are all here, and the remedy
/// (`fleet/action Interrupt`) keys on the `session_key` this carries.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcpPoolHealth {
    /// One row per LIVE adapter process (one process per provider, graft 6).
    #[serde(default)]
    pub processes: Vec<AcpProcessHealth>,
    /// One row per hosted session, keyed by the scope it answers in.
    #[serde(default)]
    pub sessions: Vec<AcpSessionHealth>,
    /// Sessions evicted by the session-level LRU since this daemon started.
    /// Their `session_key`s survive; the process stayed warm.
    #[serde(default)]
    pub evicted_total: u32,
}

/// One live adapter process.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcpProcessHealth {
    /// Adapter token (`claude-agent-acp`, `codex-acp`).
    pub provider: String,
    /// `running` while the transport is open, `exited` once it closed.
    pub state: String,
    /// Sessions currently multiplexed on this process.
    pub sessions: u32,
    /// The per-provider session cap.
    pub session_cap: u32,
    /// Turns in flight on this process right now.
    pub in_flight: u32,
    /// The per-process in-flight ceiling.
    pub in_flight_cap: u32,
    /// Whether this provider's `SlotCircuit` is open (deliveries fail fast).
    pub breaker_open: bool,
    /// Consecutive spawn/crash failures behind the breaker.
    pub breaker_failures: u32,
    /// `agentInfo.version` observed at the last successful initialize.
    #[serde(default)]
    pub provider_version: Option<String>,
}

/// One hosted ACP session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcpSessionHealth {
    /// Stable Fleet identity (`acp:<ulid>`).
    pub session_key: String,
    /// The scope this session answers in.
    pub scope_key: String,
    /// Adapter token.
    pub provider: String,
    /// `IDLE`, `ACTIVE`, or `EVICTED`.
    pub state: String,
    /// Prompts queued behind the in-flight turn.
    pub queue_depth: u32,
    /// The bounded queue's capacity; a send to a full queue is REJECTED.
    pub queue_capacity: u32,
    /// Whether a turn is open right now.
    pub turn_open: bool,
    /// Age of the open turn in milliseconds, or `None` when idle.
    #[serde(default)]
    pub turn_age_ms: Option<i64>,
    /// Permission requests raised and still unanswered.
    pub pending_permissions: u32,
    /// Transcript payload bytes COMMITTED for this session since the daemon
    /// adopted it. The pool's `session/update` demux channels are unbounded by
    /// design (dropping a chunk is data loss, and blocking the shared connection
    /// task would stall every other tenant on the process), so this counter is
    /// the growth signal that stands in for backpressure.
    #[serde(default)]
    pub transcript_bytes: u64,
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
            daemon_version: "1.16.0 (abc1234, 2026-07-17, source)".into(),
            db_error: Some("database schema (migration 41) is AHEAD".into()),
            acp_pool: Some(AcpPoolHealth {
                processes: vec![AcpProcessHealth {
                    provider: "claude-agent-acp".into(),
                    state: "running".into(),
                    sessions: 2,
                    session_cap: 16,
                    in_flight: 1,
                    in_flight_cap: 4,
                    breaker_open: false,
                    breaker_failures: 0,
                    provider_version: Some("0.64.0".into()),
                }],
                sessions: vec![AcpSessionHealth {
                    session_key: "acp:01j".into(),
                    scope_key: "session:acp:01j".into(),
                    provider: "claude-agent-acp".into(),
                    state: "ACTIVE".into(),
                    queue_depth: 3,
                    queue_capacity: 32,
                    turn_open: true,
                    turn_age_ms: Some(4_200),
                    pending_permissions: 1,
                    transcript_bytes: 48_216,
                }],
                evicted_total: 1,
            }),
        };
        let s = serde_json::to_string(&snap).unwrap();
        let back: DaemonHealthSnapshot = serde_json::from_str(&s).unwrap();
        assert_eq!(back, snap);
        assert_eq!(back.task_throughput_60s.len(), THROUGHPUT_WINDOW);
        assert_eq!(back.task_throughput_60s[30].failed, 1);
    }

    /// A snapshot from a daemon that PREDATES the `daemon_version` / `db_error`
    /// fields still decodes — with the empty-version sentinel the pane renders
    /// as "stale daemon binary". Tolerant decode is the whole point of the
    /// serde defaults: the old-daemon case is exactly the one that must not
    /// fail to parse.
    #[test]
    fn daemon_health_snapshot_decodes_pre_version_wire() {
        let old_wire = r#"{
            "runtimes": [],
            "claim_cache": {"used": 0, "capacity": 64},
            "concurrent_tasks": 0,
            "task_throughput_60s": []
        }"#;
        let back: DaemonHealthSnapshot = serde_json::from_str(old_wire).unwrap();
        assert_eq!(back.daemon_version, "");
        assert_eq!(back.db_error, None);
        assert_eq!(back.acp_pool, None);
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

        let key = KeyRow {
            provider: "claude".into(),
            masked: "sk-…ab".into(),
        };
        let s = serde_json::to_string(&key).unwrap();
        assert_eq!(serde_json::from_str::<KeyRow>(&s).unwrap(), key);
    }

    /// A `KeyRow`'s masked form must not look like a full secret (defensive: the
    /// daemon owns masking, but assert the wire type carries no raw field).
    #[test]
    fn key_row_carries_only_masked() {
        let key = KeyRow {
            provider: "claude".into(),
            masked: "sk-…ab".into(),
        };
        let v = serde_json::to_value(&key).unwrap();
        assert!(
            v.get("value").is_none(),
            "KeyRow must not carry a raw value"
        );
        assert!(v.get("masked").is_some());
    }
}
