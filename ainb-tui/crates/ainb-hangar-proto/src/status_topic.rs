//! The agent-status envelope the TUI host publishes to its plugins (#1031).
//!
//! One owner reads `fleet/roster_status`: the host task that keeps section 20
//! current. The hangar plugin renders the Fleet panel from what that owner
//! publishes on [`AGENT_STATUS_TOPIC`] instead of paying a second whole-Fleet
//! projection per event with a subscription and a read of its own.
//!
//! ```text
//!  daemon ──roster_status──▶ host task ──▶ section 20 (StatusView)
//!                                              │ version moved
//!                                              ▼
//!                      AgentStatusEnvelope::from_view / absent
//!                                              │ host/snapshot publish
//!                                              ▼
//!  plugin handle_event ──▶ AgentStatusEnvelope::into_view ──▶ apply_view
//! ```
//!
//! The envelope is the whole view, not a delta: the snapshot bus keeps only the
//! latest payload per topic and replays nothing, so a plugin that subscribes
//! late reads one envelope and has everything. It carries the rows exactly as
//! the daemon joined them, `cwd`, `display_name`, raw `current_request` and
//! fingerprints included, which the #983 section 20 frame leaves out. Moving
//! them from the plugin's own daemon socket onto the plugin bus is a trust
//! boundary change, bounded by the runtime's topic-scoped `event_bus` grant:
//! only a plugin whose grant names [`AGENT_STATUS_TOPIC`] can read it (the
//! in-tree hangar plugin), the blanket grant does not cover `fleet.` topics,
//! and the TUI host is the only publisher. Failure reasons are scrubbed by the
//! host before publishing. `StatusView` itself stays without `Serialize`
//! (#983): the wire shape lives here and nowhere else, locked by
//! `the_envelope_key_paths_match_the_committed_list`.

use serde::{Deserialize, Serialize};

use crate::agent_status::RosterStatusRow;
use crate::status_view::{AgentCard, StatusView, ViewHealth};

/// The host-published snapshot topic the envelope rides on.
pub const AGENT_STATUS_TOPIC: &str = "fleet.agent_status";

/// The host-published topic carrying the clock cards age on (#1054).
///
/// A separate, tiny topic so a once-a-second tick never re-encodes every row
/// of [`AGENT_STATUS_TOPIC`].
pub const AGENT_STATUS_CLOCK_TOPIC: &str = "fleet.agent_status.clock";

/// One tick of the clock a Fleet card's age is measured on (#1054).
///
/// Published by the host's agent-status task, the section 20 owner, once a
/// second while it holds cards. Each publish also marks the subscribing panel
/// for a repaint, which is what makes an idle card's age advance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentStatusClock {
    /// The host's estimate of the daemon's clock, epoch ms: the clock evidence
    /// stamps are on.
    pub clock_ms: i64,
}

/// The largest encoded envelope the host publishes.
///
/// The plugin framer refuses a body over 16 MiB, and a snapshot payload rides
/// it as base64 (4/3 of its size) inside a JSON-RPC notification. Half of that
/// ceiling leaves room for the envelope around it; a roster that does not fit
/// is published as unreachable with a reason instead of being cut short.
pub const AGENT_STATUS_ENVELOPE_MAX_BYTES: usize = 6 * 1024 * 1024;

/// How current the published view is, or why there is none.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgentStatusHealth {
    /// The rows are the newest revision the owner has seen.
    Live,
    /// A newer revision was observed than the rows were read at.
    Stale {
        /// The revision the rows were read at.
        read_revision: i64,
        /// The newest revision observed since.
        head_revision: i64,
    },
    /// The daemon stopped answering; the rows are frozen as last read.
    Unreachable {
        /// Owner's local clock, epoch ms, when the daemon stopped answering.
        stale_since_ms: i64,
        /// Why, in one operator-facing phrase.
        reason: String,
    },
    /// The owner has no view to publish, and why.
    Absent {
        /// Why, in one operator-facing phrase.
        reason: String,
    },
}

/// One published agent-status view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentStatusEnvelope {
    /// Publish order within one owner. A subscriber drops an envelope at or
    /// below the last one it applied, so a late `snapshot_get` and an event
    /// already in flight cannot step the panel backwards.
    pub sequence: u64,
    /// The daemon revision the rows were read at.
    pub revision: i64,
    /// The host the rows came from.
    pub host_id: String,
    /// Owner's local clock, epoch ms, when the read landed.
    pub read_at_ms: i64,
    /// The newest Fleet revision the owner has been told about.
    pub head_revision: i64,
    /// How current the rows are.
    pub health: AgentStatusHealth,
    /// The joined rows, in `session_key` order.
    pub rows: Vec<RosterStatusRow>,
}

impl AgentStatusEnvelope {
    /// The envelope for a view the owner holds.
    #[must_use]
    pub fn from_view(sequence: u64, view: &StatusView) -> Self {
        let health = match &view.health {
            ViewHealth::Live => AgentStatusHealth::Live,
            ViewHealth::Stale {
                read_revision,
                head_revision,
            } => AgentStatusHealth::Stale {
                read_revision: *read_revision,
                head_revision: *head_revision,
            },
            ViewHealth::Unreachable {
                stale_since_ms,
                reason,
            } => AgentStatusHealth::Unreachable {
                stale_since_ms: *stale_since_ms,
                reason: reason.clone(),
            },
        };
        Self {
            sequence,
            revision: view.read_revision,
            host_id: view.host_id.clone(),
            read_at_ms: view.received_at_ms,
            head_revision: view.head_revision,
            health,
            rows: view
                .cards()
                .map(|card| RosterStatusRow {
                    session: card.session.clone(),
                    status: card.status.clone(),
                    read_revision: view.read_revision,
                })
                .collect(),
        }
    }

    /// The envelope for an owner with no view: the daemon cannot serve one.
    #[must_use]
    pub fn absent(sequence: u64, reason: impl Into<String>, head_revision: i64) -> Self {
        Self {
            sequence,
            revision: head_revision,
            host_id: String::new(),
            read_at_ms: 0,
            head_revision,
            health: AgentStatusHealth::Absent {
                reason: reason.into(),
            },
            rows: Vec::new(),
        }
    }

    /// The view this envelope describes, or the reason there is none.
    ///
    /// # Errors
    /// Returns the absent reason when the owner published no view.
    pub fn into_view(self) -> Result<StatusView, String> {
        let health = match self.health {
            AgentStatusHealth::Absent { reason } => return Err(reason),
            AgentStatusHealth::Live => ViewHealth::Live,
            AgentStatusHealth::Stale {
                read_revision,
                head_revision,
            } => ViewHealth::Stale {
                read_revision,
                head_revision,
            },
            AgentStatusHealth::Unreachable {
                stale_since_ms,
                reason,
            } => ViewHealth::Unreachable {
                stale_since_ms,
                reason,
            },
        };
        Ok(StatusView {
            host_id: self.host_id,
            read_revision: self.revision,
            received_at_ms: self.read_at_ms,
            health,
            head_revision: self.head_revision,
            cards: self
                .rows
                .into_iter()
                .map(|row| {
                    (
                        row.status.session_key.clone(),
                        AgentCard {
                            session: row.session,
                            status: row.status,
                        },
                    )
                })
                .collect(),
        })
    }
}

#[cfg(test)]
mod tests {
    /// #1054: the clock tick has one stable field and round-trips.
    #[test]
    fn a_clock_tick_round_trips() {
        let tick = AgentStatusClock {
            clock_ms: 1_789_409_614_717,
        };
        let wire = serde_json::to_value(tick).unwrap();
        assert_eq!(
            wire,
            serde_json::json!({ "clock_ms": 1_789_409_614_717_i64 })
        );
        assert_eq!(
            serde_json::from_value::<AgentStatusClock>(wire).unwrap(),
            tick
        );
    }

    use super::*;
    use crate::agent_status::{RosterStatusResult, status_row};
    use crate::fleet::{
        AttentionState, FleetCapabilities, FleetConfidence, FleetProvenance, FleetProvider,
        FleetSession, LifecycleState, ManagementState, PaneBinding, TransportHealth,
    };

    fn session(index: usize) -> FleetSession {
        FleetSession {
            session_key: format!("claude:{index:04}"),
            provider: FleetProvider::Claude,
            provider_session_id: Some(format!("{index:04}")),
            tmux_target: Some(format!("dev:{index}.0")),
            pane_binding: PaneBinding::Bound,
            process_start_fingerprint: None,
            cwd: format!("/work/repositories/project-{index}/worktrees/feature-branch"),
            display_name: Some(format!("agent number {index}")),
            lifecycle: LifecycleState::Running,
            active_work_count: 1,
            attention: AttentionState::Ask,
            current_request_fingerprint: Some("f".repeat(64)),
            current_request: Some(serde_json::json!({ "tool_input": "x".repeat(2048) })),
            management: ManagementState::Managed,
            transport_health: TransportHealth::Healthy,
            capabilities: FleetCapabilities::default(),
            provenance: FleetProvenance::Authoritative,
            confidence: FleetConfidence::High,
            discovered_at: 1,
            last_observed_at: 10,
            lifecycle_updated_at: 5,
            attention_updated_at: 7,
            model: Some("claude-sonnet-4-5".to_string()),
            reasoning_effort: None,
            model_updated_at: 0,
            version: 1,
            updated_revision: 1,
        }
    }

    fn read(revision: i64, sessions: usize) -> RosterStatusResult {
        RosterStatusResult {
            rows: (0..sessions)
                .map(|index| {
                    let session = session(index);
                    RosterStatusRow {
                        status: status_row(&session, true),
                        session,
                        read_revision: revision,
                    }
                })
                .collect(),
            read_revision: revision,
            unknown_events: Vec::new(),
        }
    }

    /// The plugin folds exactly the view the owner holds, in every health.
    #[test]
    fn a_view_survives_the_envelope_round_trip_in_every_health() {
        let mut view = StatusView::from_read(read(7, 3), 1_000);
        let mut healths = vec![view.clone()];
        view.observe_head(9);
        healths.push(view.clone());
        // A lower read after a reconnect: stale against a revision the rows
        // were not read at, which only an explicit health can carry.
        view.apply(read(2, 1), 1_100);
        healths.push(view.clone());
        view.mark_unreachable("daemon not reachable", 1_200);
        healths.push(view);
        for (sequence, view) in healths.into_iter().enumerate() {
            let envelope = AgentStatusEnvelope::from_view(sequence as u64, &view);
            let wire: AgentStatusEnvelope =
                serde_json::from_slice(&serde_json::to_vec(&envelope).unwrap()).unwrap();
            assert_eq!(wire.sequence, sequence as u64);
            assert_eq!(wire.into_view().expect("a view"), view);
        }
    }

    #[test]
    fn an_absent_owner_publishes_its_reason_and_no_rows() {
        let envelope = AgentStatusEnvelope::absent(4, "daemon has no fleet/status", 12);
        let wire: AgentStatusEnvelope =
            serde_json::from_slice(&serde_json::to_vec(&envelope).unwrap()).unwrap();
        assert!(wire.rows.is_empty());
        assert_eq!(
            wire.into_view(),
            Err("daemon has no fleet/status".to_string())
        );
    }

    /// A large fleet, each agent holding a 2 KiB pending request, stays under
    /// half the cap (about 1.7 MiB).
    #[test]
    fn a_five_hundred_agent_envelope_fits_under_the_cap() {
        let view = StatusView::from_read(read(1, 500), 1);
        let bytes = serde_json::to_vec(&AgentStatusEnvelope::from_view(1, &view)).unwrap();
        assert!(
            bytes.len() < AGENT_STATUS_ENVELOPE_MAX_BYTES / 2,
            "{} bytes for 500 agents",
            bytes.len()
        );
    }

    /// Every leaf key path of the envelope, in the committed form: array items
    /// as `[]`, and `current_request` kept as one opaque leaf because its
    /// inside is the agent's tool input, not a shape this crate owns.
    const ENVELOPE_KEY_PATHS: &str = "
head_revision
health.head_revision
health.kind
health.read_revision
health.reason
health.stale_since_ms
host_id
read_at_ms
revision
rows[].read_revision
rows[].session.active_work_count
rows[].session.attention
rows[].session.attention_updated_at
rows[].session.capabilities.approval_session
rows[].session.capabilities.approvals
rows[].session.capabilities.archive
rows[].session.capabilities.continue_turn
rows[].session.capabilities.interrupt
rows[].session.capabilities.kill
rows[].session.capabilities.restart
rows[].session.capabilities.retry
rows[].session.capabilities.send_prompt
rows[].session.capabilities.start
rows[].session.capabilities.stop
rows[].session.capabilities.structured_answer
rows[].session.capabilities.structured_dismiss
rows[].session.capabilities.tmux_attach
rows[].session.capabilities.tmux_text
rows[].session.capabilities.verified_picker
rows[].session.confidence
rows[].session.current_request
rows[].session.current_request_fingerprint
rows[].session.cwd
rows[].session.discovered_at
rows[].session.display_name
rows[].session.last_observed_at
rows[].session.lifecycle
rows[].session.lifecycle_updated_at
rows[].session.management
rows[].session.model
rows[].session.model_updated_at
rows[].session.pane_binding
rows[].session.process_start_fingerprint
rows[].session.provenance
rows[].session.provider
rows[].session.provider_session_id
rows[].session.reasoning_effort
rows[].session.session_key
rows[].session.tmux_target
rows[].session.transport_health
rows[].session.updated_revision
rows[].session.version
rows[].status.attachment
rows[].status.cwd
rows[].status.display_name
rows[].status.evidence_observed_at
rows[].status.has_open_request
rows[].status.host_id
rows[].status.pane_unbound
rows[].status.pane_unbound_detail
rows[].status.provenance
rows[].status.provider
rows[].status.session_key
rows[].status.state
rows[].status.tier
rows[].status.turn_complete
rows[].status.wait_kind
sequence
";

    fn leaf_paths(
        value: &serde_json::Value,
        path: &str,
        into: &mut std::collections::BTreeSet<String>,
    ) {
        match value {
            serde_json::Value::Object(map) if !path.ends_with(".current_request") => {
                for (key, child) in map {
                    let next = if path.is_empty() {
                        key.clone()
                    } else {
                        format!("{path}.{key}")
                    };
                    leaf_paths(child, &next, into);
                }
            }
            serde_json::Value::Array(items) if !path.ends_with(".current_request") => {
                for item in items {
                    leaf_paths(item, &format!("{path}[]"), into);
                }
            }
            _ => {
                into.insert(path.to_string());
            }
        }
    }

    /// #1038 review item 3: the envelope bypasses the #983 section frame, so its
    /// shape is locked here. Every optional field is filled and every health
    /// variant traced; a path not in [`ENVELOPE_KEY_PATHS`] (a new
    /// `FleetSession` or `AgentStatusRow` field, say) fails until it is triaged
    /// for the plugin bus and added.
    #[test]
    fn the_envelope_key_paths_match_the_committed_list() {
        let mut full = read(7, 1);
        let row = &mut full.rows[0];
        row.session.reasoning_effort = Some("high".to_string());
        row.session.process_start_fingerprint = Some("pane=%1;pid=1;started=1".to_string());
        row.status.pane_unbound_detail = Some("pane gone".to_string());
        row.status.wait_kind = Some(crate::agent_status::WaitKind::Ask);
        let mut view = StatusView::from_read(full, 1);
        let mut paths = std::collections::BTreeSet::new();
        let mut trace = |envelope: &AgentStatusEnvelope| {
            leaf_paths(&serde_json::to_value(envelope).unwrap(), "", &mut paths);
        };
        trace(&AgentStatusEnvelope::from_view(1, &view));
        view.observe_head(9);
        trace(&AgentStatusEnvelope::from_view(2, &view));
        view.mark_unreachable("daemon not reachable", 5);
        trace(&AgentStatusEnvelope::from_view(3, &view));
        trace(&AgentStatusEnvelope::absent(
            4,
            "daemon serves no agent status read",
            9,
        ));

        let committed: std::collections::BTreeSet<String> = ENVELOPE_KEY_PATHS
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_string)
            .collect();
        let added: Vec<_> = paths.difference(&committed).collect();
        let removed: Vec<_> = committed.difference(&paths).collect();
        assert!(
            added.is_empty() && removed.is_empty(),
            "envelope shape drifted.\nadded (triage for the plugin bus, then list):\n{}\nremoved:\n{}",
            added.iter().map(|p| format!("{p}\n")).collect::<String>(),
            removed.iter().map(|p| format!("{p}\n")).collect::<String>(),
        );
    }
}
