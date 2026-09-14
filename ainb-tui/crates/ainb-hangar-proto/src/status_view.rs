//! The agent-status view every surface renders from (D14, T0-section, #1015).
//!
//! One pure reducer over `fleet/roster_status` replies, shared by the TUI Fleet
//! panel (in the hangar plugin) and by section 20 of the app state (and so by
//! the desktop and the phone through W0-mirror). Two surfaces folding the same
//! replies through two reducers is exactly the drift D14 removes, so neither
//! keeps one of its own.
//!
//! ```text
//!  fleet/roster_status ──▶ StatusView::apply ──▶ cards + health ──▶ render
//!  read failed         ──▶ StatusView::mark_unreachable (rows frozen)
//!  newer fleet event   ──▶ StatusView::observe_head      (Stale until read)
//! ```
//!
//! Deliberately no `Serialize`: section 20 must not be serialised until the
//! redaction layer exists (#983). The rows it holds are wire types, but the view
//! itself is a local fold.

use std::collections::BTreeMap;

use crate::agent_status::{AgentStatusRow, RosterStatusResult};
use crate::fleet::FleetSession;

/// One agent as a surface renders it: what the session is, and its state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentCard {
    /// The roster half of the joined read.
    pub session: FleetSession,
    /// The status half of the joined read.
    pub status: AgentStatusRow,
}

impl AgentCard {
    /// Whether two cards differ in anything a surface renders. Transport
    /// heartbeat stamps (`last_observed_at`, `version`, `updated_revision`) are
    /// ignored, so a heartbeat that changes no rendered fact is not a change.
    #[must_use]
    pub fn renders_same_as(&self, other: &Self) -> bool {
        fn without_heartbeat(session: &FleetSession) -> FleetSession {
            FleetSession {
                last_observed_at: 0,
                version: 0,
                updated_revision: 0,
                ..session.clone()
            }
        }
        self.status == other.status
            && without_heartbeat(&self.session) == without_heartbeat(&other.session)
    }
}

/// How current the view is, which every surface renders instead of guessing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ViewHealth {
    /// The last read is the newest revision this surface has seen.
    Live,
    /// A newer Fleet revision has been observed than the rows were read at:
    /// they describe an earlier instant until the next read lands.
    Stale {
        /// The revision the rows were read at.
        read_revision: i64,
        /// The newest revision observed since.
        head_revision: i64,
    },
    /// The host did not answer the last read. Rows are frozen as last read,
    /// never turned into `unverifiable`: unreachability is not a state (D14).
    Unreachable {
        /// Local clock, epoch ms, when the host stopped answering.
        stale_since_ms: i64,
        /// Why, in one operator-facing phrase.
        reason: String,
    },
}

/// The view a surface renders: the last joined read, and how current it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusView {
    /// The host the rows came from.
    pub host_id: String,
    /// The revision the rows were read at.
    pub read_revision: i64,
    /// Local clock, epoch ms, when the last read was received. Stamped here,
    /// never computed from a remote stamp (D14 clock table).
    pub received_at_ms: i64,
    /// How current the rows are.
    pub health: ViewHealth,
    /// One card per session, by `session_key`.
    pub cards: BTreeMap<String, AgentCard>,
}

impl StatusView {
    /// The view built from its first read.
    #[must_use]
    pub fn from_read(result: RosterStatusResult, received_at_ms: i64) -> Self {
        let mut view = Self {
            host_id: String::new(),
            read_revision: i64::MIN,
            received_at_ms,
            health: ViewHealth::Live,
            cards: BTreeMap::new(),
        };
        view.apply(result, received_at_ms);
        view
    }

    /// Fold one joined read. Returns whether anything a surface renders
    /// changed (a card, the host, or the health), so a versioned owner bumps
    /// only then and a heartbeat-only read leaves it untouched.
    ///
    /// A read older than the one already held is refused (returns `false` and
    /// changes nothing): it describes an earlier instant.
    pub fn apply(&mut self, result: RosterStatusResult, received_at_ms: i64) -> bool {
        if result.read_revision < self.read_revision {
            return false;
        }
        let cards: BTreeMap<String, AgentCard> = result
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
            .collect();
        let host_id = cards
            .values()
            .next()
            .map_or_else(|| self.host_id.clone(), |card| card.status.host_id.clone());
        let cards_changed = cards.len() != self.cards.len()
            || cards.iter().any(|(key, card)| {
                self.cards.get(key).is_none_or(|held| !held.renders_same_as(card))
            });
        let changed = cards_changed || host_id != self.host_id || self.health != ViewHealth::Live;
        self.cards = cards;
        self.host_id = host_id;
        self.read_revision = result.read_revision;
        self.received_at_ms = received_at_ms;
        self.health = ViewHealth::Live;
        changed
    }

    /// A newer Fleet revision was observed. The rows go stale until a read at
    /// or past it lands. Returns whether the health changed.
    pub fn observe_head(&mut self, head_revision: i64) -> bool {
        if head_revision <= self.read_revision {
            return false;
        }
        if matches!(self.health, ViewHealth::Unreachable { .. }) {
            return false;
        }
        let next = ViewHealth::Stale {
            read_revision: self.read_revision,
            head_revision,
        };
        let changed = self.health != next;
        self.health = next;
        changed
    }

    /// The host stopped answering. Rows stay frozen as last read; the first
    /// failure's clock is kept while the host stays unreachable. Returns
    /// whether the health changed.
    pub fn mark_unreachable(&mut self, reason: impl Into<String>, now_ms: i64) -> bool {
        let reason = reason.into();
        let stale_since_ms = match &self.health {
            ViewHealth::Unreachable { stale_since_ms, .. } => *stale_since_ms,
            _ => now_ms,
        };
        let next = ViewHealth::Unreachable {
            stale_since_ms,
            reason,
        };
        let changed = self.health != next;
        self.health = next;
        changed
    }

    /// Cards in `session_key` order.
    pub fn cards(&self) -> impl Iterator<Item = &AgentCard> {
        self.cards.values()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_status::{AgentState, RosterStatusRow, status_row};
    use crate::fleet::{
        AttentionState, FleetCapabilities, FleetConfidence, FleetProvenance, FleetProvider,
        LifecycleState, ManagementState, PaneBinding, TransportHealth,
    };

    fn session(key: &str, attention: AttentionState) -> FleetSession {
        FleetSession {
            session_key: key.to_string(),
            provider: FleetProvider::Claude,
            provider_session_id: Some(key.to_string()),
            tmux_target: Some("dev:1.0".to_string()),
            pane_binding: PaneBinding::Bound,
            process_start_fingerprint: None,
            cwd: "/w/app".to_string(),
            display_name: None,
            lifecycle: LifecycleState::Running,
            active_work_count: 0,
            attention,
            current_request_fingerprint: None,
            current_request: None,
            management: ManagementState::Managed,
            transport_health: TransportHealth::Healthy,
            capabilities: FleetCapabilities::default(),
            provenance: FleetProvenance::Authoritative,
            confidence: FleetConfidence::High,
            discovered_at: 1,
            last_observed_at: 10,
            lifecycle_updated_at: 5,
            attention_updated_at: 7,
            model: None,
            reasoning_effort: None,
            model_updated_at: 0,
            version: 1,
            updated_revision: 1,
        }
    }

    fn read(revision: i64, sessions: &[FleetSession]) -> RosterStatusResult {
        RosterStatusResult {
            rows: sessions
                .iter()
                .map(|session| RosterStatusRow {
                    session: session.clone(),
                    status: status_row(session, session.attention != AttentionState::None),
                    read_revision: revision,
                })
                .collect(),
            read_revision: revision,
            unknown_events: Vec::new(),
        }
    }

    /// #1015 criterion: the version bumps on a status-row change, not on a
    /// transport heartbeat. A read whose only difference is heartbeat stamps
    /// and a newer revision changes nothing a surface renders.
    #[test]
    fn a_heartbeat_only_read_is_not_a_change_but_a_state_change_is() {
        let asking = session("claude:a", AttentionState::Ask);
        let mut view = StatusView::from_read(read(3, std::slice::from_ref(&asking)), 100);

        let mut heartbeat = asking.clone();
        heartbeat.last_observed_at = 99;
        heartbeat.version = 2;
        heartbeat.updated_revision = 4;
        assert!(
            !view.apply(read(4, &[heartbeat]), 200),
            "a heartbeat is not a change"
        );
        assert_eq!(view.read_revision, 4);
        assert_eq!(
            view.received_at_ms, 200,
            "the local receipt clock still advances"
        );

        let answered = session("claude:a", AttentionState::None);
        assert!(
            view.apply(read(5, &[answered]), 300),
            "a state change is a change"
        );
        assert_eq!(
            view.cards().next().unwrap().status.state,
            AgentState::Working
        );
    }

    /// Stale and unreachable are rendered facts, not states: rows stay frozen.
    #[test]
    fn stale_and_unreachable_freeze_rows_and_a_fresh_read_goes_live() {
        let mut view =
            StatusView::from_read(read(3, &[session("claude:a", AttentionState::Ask)]), 100);
        assert!(view.observe_head(5));
        assert_eq!(
            view.health,
            ViewHealth::Stale {
                read_revision: 3,
                head_revision: 5
            }
        );
        assert!(!view.observe_head(2), "an older revision is not news");

        assert!(view.mark_unreachable("connection refused", 1_000));
        assert!(
            !view.mark_unreachable("connection refused", 2_000),
            "same failure, same clock"
        );
        assert_eq!(
            view.health,
            ViewHealth::Unreachable {
                stale_since_ms: 1_000,
                reason: "connection refused".into()
            }
        );
        assert_eq!(
            view.cards().next().unwrap().status.state,
            AgentState::Waiting,
            "rows are frozen as last read, never turned unverifiable"
        );

        assert!(!view.apply(read(2, &[]), 3_000), "an older read is refused");
        assert!(view.apply(read(6, &[session("claude:a", AttentionState::Ask)]), 3_000));
        assert_eq!(
            view.health,
            ViewHealth::Live,
            "a fresh read goes live again"
        );
    }
}
