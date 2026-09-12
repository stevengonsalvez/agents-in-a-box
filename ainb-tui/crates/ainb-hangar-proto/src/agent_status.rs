//! One status truth for every surface (spec D14, phase T0-daemon).
//!
//! The TUI fleet panel, `ainb fleet needs`, `ainb-web`'s `/api/needs` and later
//! the desktop and the phone must show ONE row per agent with the SAME state.
//! Today they do not: the CLI folds `current_state` with a live tmux
//! `classify()` fallback, the web dashboard maps `attention/list`, and the panel
//! renders `fleet/snapshot`. Three readers, three vocabularies, three answers
//! for one agent.
//!
//! This module is the single derivation. It is deliberately PURE — it takes a
//! [`FleetSession`] and whether the inbox holds an open card for it, and returns
//! the row every surface renders — so "same state everywhere" is a property of
//! one function rather than an agreement between three codebases that drift.
//!
//! # Why tier and provenance travel with the state
//!
//! A state without its evidence is not comparable. Six tiers observe a session
//! (hook push > ACP feed > OSC frame > process > transcript > pane text) and
//! only tiers 0 and 1 may open a turn or assert that a human is needed. Carrying
//! the tier is what lets a surface say "waiting, on hook evidence, 3s old"
//! rather than "waiting" and leave the operator to guess whether a pane-scrape
//! guessed it.
//!
//! # Why there is no `done`
//!
//! Silence is not completion. An agent that stops emitting may have finished, or
//! its hook may have failed, or its pane may have been rebuilt. The vocabulary
//! below therefore has no `done`: a finished turn is [`AgentState::Idle`] (the
//! agent is free, and we saw it become free), and an absence of evidence is
//! [`AgentState::Unverifiable`] (we still hold the pane, but nothing has told us
//! anything). No sequence of events ending in silence can produce a state that
//! claims the work is complete.

use serde::{Deserialize, Serialize};

use crate::fleet::{AttentionState, FleetProvider, FleetSession, LifecycleState, ManagementState};

/// The evidence tier a row's state came from (D14).
///
/// Ordered best-first, so `<` means "better evidence". Only [`Tier::Hook`] and
/// [`Tier::AcpFeed`] may assert that a human is needed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    /// 0 — a provider lifecycle hook pushed this.
    Hook,
    /// 1 — an ACP session feed reported it.
    AcpFeed,
    /// 2 — an in-band OSC frame carried it.
    OscFrame,
    /// 3 — the process table implied it.
    Process,
    /// 4 — the session transcript implied it.
    Transcript,
    /// 5 — a tmux pane scrape implied it.
    #[default]
    PaneText,
}

impl Tier {
    /// The tier's spec number, for the wire and for operator display.
    #[must_use]
    pub fn number(self) -> u8 {
        match self {
            Self::Hook => 0,
            Self::AcpFeed => 1,
            Self::OscFrame => 2,
            Self::Process => 3,
            Self::Transcript => 4,
            Self::PaneText => 5,
        }
    }

    /// May a row at this tier assert that a human is needed, or open a turn?
    ///
    /// Only the two tiers the provider itself drives. A pane scrape that reads
    /// like a prompt is a guess, and a guess must never raise a card an
    /// operator is expected to answer.
    #[must_use]
    pub fn may_assert_needs_input(self) -> bool {
        matches!(self, Self::Hook | Self::AcpFeed)
    }
}

/// Who produced the state, in the vocabulary every surface prints.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Provenance {
    /// A provider lifecycle hook.
    Hook,
    /// An ACP session feed.
    Acp,
    /// A tmux pane or process inference.
    #[default]
    Tmux,
}

/// The operator-facing state of one agent.
///
/// Deliberately small. Every surface renders exactly these, so a state that
/// cannot be explained to an operator in one word does not belong here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentState {
    /// The agent is running a turn.
    Working,
    /// The agent is blocked on a human: a question, an approval, an error it
    /// cannot pass, or an explicit wait marker.
    Waiting,
    /// The agent finished its turn and is free. NOT "the work is done".
    Idle,
    /// The process is gone, on process evidence.
    Exited,
    /// We hold the session but nothing has told us its state. Never inferred
    /// into `Idle`, because silence and idleness are different facts.
    #[default]
    Unverifiable,
}

impl AgentState {
    /// The token every surface prints and every test compares.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Working => "working",
            Self::Waiting => "waiting",
            Self::Idle => "idle",
            Self::Exited => "exited",
            Self::Unverifiable => "unverifiable",
        }
    }
}

/// The `fleet/status` result: one row per agent plus the revision they were
/// read at, so a client can tell whether two surfaces read the same instant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentStatusResult {
    /// One row per visible agent, ordered by `session_key`.
    pub rows: Vec<AgentStatusRow>,
    /// The Fleet revision these rows were derived from.
    pub head_revision: i64,
    /// `status_unknown_event{provider,name}` — provider event names this daemon
    /// incarnation could not map, most frequent first.
    ///
    /// Carried here rather than behind its own method because the one operator
    /// question it answers ("is my status truth complete?") is asked at the
    /// same moment as the rows themselves. Empty is the healthy answer.
    #[serde(default)]
    pub unknown_events: Vec<UnknownEventCount>,
}

/// One provider event name a daemon could not map to its status vocabulary.
///
/// An unmapped name is survivable — the event still lands with its clocks and
/// its identity, it just asserts no transition — but it is how a provider's new
/// event silently stops advancing a session's state. Counting it makes that a
/// number an operator can see.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnknownEventCount {
    /// The provider that emitted it.
    pub provider: String,
    /// The raw event name, verbatim.
    pub name: String,
    /// Sightings since this daemon started.
    pub count: u64,
}

/// One agent, as every surface shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentStatusRow {
    /// Stable Fleet identity. Never `cwd`.
    pub session_key: String,
    /// Provider that owns the session.
    pub provider: FleetProvider,
    /// Working directory, for display only.
    pub cwd: String,
    /// Human-readable label.
    pub display_name: Option<String>,
    /// The operator-facing state.
    pub state: AgentState,
    /// Who produced `state`.
    pub provenance: Provenance,
    /// The evidence tier `state` came from.
    pub tier: Tier,
    /// When the SOURCE observed the evidence, in epoch milliseconds.
    ///
    /// Never moved by a replay: it describes when the agent did the thing, not
    /// when we read about it, so staleness stays honest across a daemon restart
    /// that re-drains its spool.
    pub evidence_observed_at: i64,
    /// True when the inbox holds an open card for this session, so a surface
    /// can offer the answer affordance without a second query.
    pub has_open_request: bool,
    /// True when no tmux pane is bound (issue #916): the agent may be asking,
    /// and nothing can type an answer into it.
    pub pane_unbound: bool,
}

impl AgentStatusRow {
    /// The tuple the cross-surface identity gate compares.
    ///
    /// Exists so the CLI, the web API and the TUI snapshot assert equality on
    /// the same five fields rather than on three hand-written projections.
    #[must_use]
    pub fn identity_tuple(&self) -> (&str, &'static str, &'static str, u8, i64) {
        (
            &self.session_key,
            self.state.as_str(),
            match self.provenance {
                Provenance::Hook => "hook",
                Provenance::Acp => "acp",
                Provenance::Tmux => "tmux",
            },
            self.tier.number(),
            self.evidence_observed_at,
        )
    }
}

/// Derive one agent's status row from its Fleet session.
///
/// `has_open_request` is the inbox's answer for this session, passed in rather
/// than queried so this stays pure and so the caller reads the inbox once for
/// the whole snapshot instead of once per row.
///
/// Precedence is attention-over-lifecycle: a session that is RUNNING and also
/// holds an ASK is [`AgentState::Waiting`], because "it is working" is true but
/// useless when a human is the thing it is waiting on.
#[must_use]
pub fn status_row(session: &FleetSession, has_open_request: bool) -> AgentStatusRow {
    let tier = tier_of(session);
    let state = state_of(session, tier);
    AgentStatusRow {
        session_key: session.session_key.clone(),
        provider: session.provider,
        cwd: session.cwd.clone(),
        display_name: session.display_name.clone(),
        state,
        provenance: provenance_of(tier),
        tier,
        evidence_observed_at: evidence_observed_at(session, state),
        has_open_request,
        pane_unbound: session.pane_binding == crate::fleet::PaneBinding::PaneUnbound,
    }
}

/// Which tier's evidence this row's state rests on.
///
/// Derived from what the store already records, because the tier column itself
/// arrives with the D14 migration. An ACP session is tier 1 by construction; a
/// row whose state groups were written by an authoritative observer is a hook
/// push; everything else is the tmux scan.
#[must_use]
pub fn tier_of(session: &FleetSession) -> Tier {
    if session.provider == FleetProvider::Acp {
        return Tier::AcpFeed;
    }
    // A managed row, or one with a provider session id, was keyed by a hook:
    // the tmux scan cannot learn a provider's own session id.
    if session.management == ManagementState::Managed
        || session.provider_session_id.as_deref().is_some_and(|id| !id.is_empty())
    {
        Tier::Hook
    } else {
        Tier::PaneText
    }
}

/// The provenance token that goes with a tier.
#[must_use]
pub fn provenance_of(tier: Tier) -> Provenance {
    match tier {
        Tier::Hook => Provenance::Hook,
        Tier::AcpFeed => Provenance::Acp,
        Tier::OscFrame | Tier::Process | Tier::Transcript | Tier::PaneText => Provenance::Tmux,
    }
}

/// Fold a session's two independent state groups into one operator state.
fn state_of(session: &FleetSession, tier: Tier) -> AgentState {
    // Only tiers 0 and 1 may assert that a human is needed. A pane scrape that
    // reads like a prompt is a guess, and acting on it would raise a card
    // nobody can answer.
    if tier.may_assert_needs_input() {
        match session.attention {
            AttentionState::Ask
            | AttentionState::Approval
            | AttentionState::Waiting
            | AttentionState::Error => return AgentState::Waiting,
            AttentionState::None => {}
        }
    }
    match session.lifecycle {
        LifecycleState::Starting | LifecycleState::Running => AgentState::Working,
        // A completed turn means the agent is free, NOT that the work is done.
        LifecycleState::TurnComplete | LifecycleState::Idle => AgentState::Idle,
        LifecycleState::Exited => AgentState::Exited,
        // Silence. We hold the session and know nothing about it, which is a
        // different fact from "it is idle" and is never folded into one.
        LifecycleState::Unknown => AgentState::Unverifiable,
    }
}

/// When the source observed the evidence behind `state`.
///
/// Reads the clock of the state group the row is actually reporting, so a
/// session that has been waiting for an hour does not look 3 seconds fresh
/// because an unrelated metadata event touched it.
fn evidence_observed_at(session: &FleetSession, state: AgentState) -> i64 {
    let group = match state {
        AgentState::Waiting => session.attention_updated_at,
        AgentState::Working | AgentState::Idle | AgentState::Exited => session.lifecycle_updated_at,
        AgentState::Unverifiable => 0,
    };
    if group > 0 {
        group
    } else {
        session.last_observed_at
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet::{
        FleetCapabilities, FleetConfidence, FleetProvenance, PaneBinding, TransportHealth,
    };

    fn session(lifecycle: LifecycleState, attention: AttentionState) -> FleetSession {
        FleetSession {
            session_key: "claude:s-1".to_string(),
            provider: FleetProvider::Claude,
            provider_session_id: Some("s-1".to_string()),
            tmux_target: Some("dev:1.0".to_string()),
            pane_binding: PaneBinding::Bound,
            process_start_fingerprint: None,
            cwd: "/w/app".to_string(),
            display_name: None,
            lifecycle,
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
            last_observed_at: 9,
            lifecycle_updated_at: 5,
            attention_updated_at: 7,
            model: None,
            reasoning_effort: None,
            model_updated_at: 0,
            version: 1,
            updated_revision: 1,
        }
    }

    /// The gate: a hook row that says "waiting" keeps saying waiting, on hook
    /// provenance, whatever a pane scrape of the same pane would read.
    #[test]
    fn a_hook_waiting_row_reports_waiting_on_hook_evidence() {
        let row = status_row(&session(LifecycleState::Running, AttentionState::Ask), true);
        assert_eq!(row.state, AgentState::Waiting);
        assert_eq!(row.provenance, Provenance::Hook);
        assert_eq!(row.tier, Tier::Hook);
        assert_eq!(
            row.evidence_observed_at, 7,
            "the attention clock, not metadata"
        );
    }

    /// A tier-5 row may describe a pane, but it may not claim a human is
    /// needed: that assertion belongs to the provider, not to a scrape.
    #[test]
    fn a_pane_scrape_never_asserts_that_a_human_is_needed() {
        let mut pane = session(LifecycleState::Running, AttentionState::Ask);
        pane.management = ManagementState::Degraded;
        pane.provider_session_id = None;
        let row = status_row(&pane, false);
        assert_eq!(row.tier, Tier::PaneText);
        assert_eq!(row.provenance, Provenance::Tmux);
        assert_eq!(
            row.state,
            AgentState::Working,
            "the pane's own lifecycle reading stands; its attention guess does not"
        );
    }

    /// Silence is not completion. There is no state in the vocabulary that
    /// claims the work finished, and an unknown lifecycle never becomes idle.
    #[test]
    fn silence_is_unverifiable_and_never_done() {
        let row = status_row(
            &session(LifecycleState::Unknown, AttentionState::None),
            false,
        );
        assert_eq!(row.state, AgentState::Unverifiable);
        for state in [
            AgentState::Working,
            AgentState::Waiting,
            AgentState::Idle,
            AgentState::Exited,
            AgentState::Unverifiable,
        ] {
            assert_ne!(state.as_str(), "done", "no state may claim completion");
        }
    }

    /// A finished turn is the agent being free, which is `idle`. Naming it
    /// `done` is the mistake this vocabulary exists to prevent.
    #[test]
    fn a_completed_turn_is_idle_not_done() {
        let row = status_row(
            &session(LifecycleState::TurnComplete, AttentionState::None),
            false,
        );
        assert_eq!(row.state, AgentState::Idle);
        assert_eq!(row.evidence_observed_at, 5, "the lifecycle clock");
    }

    #[test]
    fn an_acp_child_is_tier_one_with_acp_provenance() {
        let mut acp = session(LifecycleState::Running, AttentionState::Approval);
        acp.provider = FleetProvider::Acp;
        let row = status_row(&acp, true);
        assert_eq!(row.tier, Tier::AcpFeed);
        assert_eq!(row.provenance, Provenance::Acp);
        assert_eq!(row.state, AgentState::Waiting, "tier 1 may assert it");
    }

    #[test]
    fn tier_numbers_match_the_spec_order() {
        assert_eq!(
            [
                Tier::Hook,
                Tier::AcpFeed,
                Tier::OscFrame,
                Tier::Process,
                Tier::Transcript,
                Tier::PaneText
            ]
            .map(Tier::number),
            [0, 1, 2, 3, 4, 5]
        );
        assert!(Tier::Hook < Tier::PaneText, "better evidence sorts first");
    }

    /// An unbound pane is carried on the row so every surface can warn without
    /// a second query, and so the CLI and the panel warn about the same rows.
    #[test]
    fn an_unbound_pane_is_reported_on_the_row() {
        let mut unbound = session(LifecycleState::Idle, AttentionState::Ask);
        unbound.tmux_target = None;
        unbound.pane_binding = PaneBinding::PaneUnbound;
        assert!(status_row(&unbound, true).pane_unbound);
    }
}
