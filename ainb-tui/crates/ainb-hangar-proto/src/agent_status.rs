//! One status truth for every surface (spec D14, phase T0-daemon).
//!
//! The TUI fleet panel, `ainb fleet needs`, `ainb-web`'s `/api/needs` and later
//! the desktop and the phone must show ONE row per agent with the SAME state.
//! Today they do not: the CLI folds `current_state` with a live tmux
//! `classify()` fallback, the web dashboard maps `attention/list`, and the panel
//! renders `fleet/snapshot`. Three readers, three vocabularies, three answers
//! for one agent.
//!
//! This module is the single derivation. It is deliberately PURE: it takes a
//! [`FleetSession`] and whether the inbox holds an open card for it, and returns
//! the row every surface renders, so "same state everywhere" is a property of
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

use crate::fleet::{
    AttentionState, FleetProvider, FleetSession, FleetSnapshot, LifecycleState, ManagementState,
    PaneBinding,
};

/// The evidence tier a row's state came from (D14).
///
/// Ordered best-first, so `<` means "better evidence". Only [`Tier::Hook`] and
/// [`Tier::AcpFeed`] may assert that a human is needed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    /// 0: a provider lifecycle hook pushed this.
    Hook,
    /// 1: an ACP session feed reported it.
    AcpFeed,
    /// 2: an in-band OSC frame carried it.
    OscFrame,
    /// 3: the process table implied it.
    Process,
    /// 4: the session transcript implied it.
    Transcript,
    /// 5: a tmux pane scrape implied it.
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
    /// `status_unknown_event{provider,name}`: provider event names this daemon
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
/// An unmapped name is survivable, the event still lands with its clocks and
/// its identity, it just asserts no transition, but it is how a provider's new
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
    /// Why, in one operator-facing sentence, when `pane_unbound` is true.
    ///
    /// The three cases have different fixes, so they are never collapsed: no
    /// candidate pane at all, two that collide, or a binding invalidated
    /// because the pane it held is now running something else (#961). The last
    /// one also names the pane that was lost and what is available now.
    ///
    /// `#[serde(default)]` so a daemon that does not compute it, and a client
    /// reading an older reply, both see `None` rather than failing the read.
    #[serde(default)]
    pub pane_unbound_detail: Option<String>,
    /// The host this agent runs on (D14 identity, `local` until paired hosts
    /// exist), so a row stays addressable once it is mirrored off-box.
    #[serde(default = "local_host_id")]
    pub host_id: String,
    /// The agent's last turn completed. Refines [`AgentState::Idle`] for the
    /// `done` lens; it never becomes a state of its own, because idle means
    /// "free", not "the work is done".
    #[serde(default)]
    pub turn_complete: bool,
    /// What kind of human input the agent is waiting on, when
    /// [`AgentState::Waiting`]; `None` otherwise. Carried as an enum so no
    /// surface re-reads the attention string (#1015).
    #[serde(default)]
    pub wait_kind: Option<WaitKind>,
    /// How an operator can reach the session's terminal, derived once here
    /// instead of per surface from capabilities and pane strings.
    #[serde(default)]
    pub attachment: Attachment,
}

fn local_host_id() -> String {
    LOCAL_HOST_ID.to_string()
}

/// The `host_id` of the machine the daemon runs on, until R1 pairs hosts.
pub const LOCAL_HOST_ID: &str = "local";

/// The kind of human input a waiting agent needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WaitKind {
    /// A structured question.
    Ask,
    /// A tool or permission approval.
    Approval,
    /// An explicit wait marker.
    Waiting,
    /// An error the agent cannot pass on its own.
    Error,
}

impl WaitKind {
    /// The token every surface prints.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ask => "ask",
            Self::Approval => "approval",
            Self::Waiting => "waiting",
            Self::Error => "error",
        }
    }
}

/// How an operator can reach a session's terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Attachment {
    /// An exact tmux pane can be attached.
    Tmux,
    /// The session is live but no pane is bound (#916): nothing can attach or
    /// type into it until a later event binds one.
    Unbound,
    /// A managed session reachable through daemon actions, not a pane.
    Remote,
    /// No way to reach it.
    #[default]
    None,
}

impl Attachment {
    /// The token every surface prints.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Tmux => "tmux",
            Self::Unbound => "unbound",
            Self::Remote => "remote",
            Self::None => "none",
        }
    }
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

    /// [`Self::identity_tuple`] with the row's `host_id`: the tuple the
    /// cross-surface gate compares once rows are addressable off-box (#1015).
    #[must_use]
    pub fn host_identity_tuple(&self) -> (&str, &'static str, &'static str, u8, i64, &str) {
        let (key, state, provenance, tier, observed) = self.identity_tuple();
        (key, state, provenance, tier, observed, &self.host_id)
    }
}

/// One agent's roster session and status in one row, from ONE daemon read
/// (`fleet/roster_status`, #1015).
///
/// The roster half says what the session is (provider, request, capabilities);
/// the status half says what state it is in. Before this every surface read the
/// two separately and joined them itself, which the desktop and the phone would
/// each have had to re-implement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RosterStatusRow {
    /// The session as the roster describes it.
    pub session: FleetSession,
    /// The session's state, derived by the daemon.
    pub status: AgentStatusRow,
    /// The Fleet revision this row was read at.
    pub read_revision: i64,
}

/// The `fleet/roster_status` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RosterStatusResult {
    /// One row per visible session, ordered by `session_key`.
    pub rows: Vec<RosterStatusRow>,
    /// The Fleet revision the whole read was taken at.
    pub read_revision: i64,
    /// As [`AgentStatusResult::unknown_events`].
    #[serde(default)]
    pub unknown_events: Vec<UnknownEventCount>,
}

/// Join a roster snapshot and a status read per `session_key`.
///
/// The ONE join. The daemon's `fleet/roster_status` is built from it, and a
/// surface on the pre-section read (`[fleet.status] legacy_panel`) calls it on
/// its two replies instead of keeping a join of its own. A session the status
/// read does not name is left out: a row without a state is not a row.
#[must_use]
pub fn join(snapshot: &FleetSnapshot, status: &AgentStatusResult) -> RosterStatusResult {
    let states: std::collections::BTreeMap<&str, &AgentStatusRow> =
        status.rows.iter().map(|row| (row.session_key.as_str(), row)).collect();
    let read_revision = snapshot.head_revision.min(status.head_revision);
    let mut rows: Vec<RosterStatusRow> = snapshot
        .sessions
        .iter()
        .filter_map(|session| {
            states.get(session.session_key.as_str()).map(|state| RosterStatusRow {
                session: session.clone(),
                status: (*state).clone(),
                read_revision,
            })
        })
        .collect();
    rows.sort_by(|a, b| a.session.session_key.cmp(&b.session.session_key));
    RosterStatusResult {
        rows,
        read_revision,
        unknown_events: status.unknown_events.clone(),
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
    status_row_with_tier(session, has_open_request, None)
}

/// [`status_row`] for a caller holding the row's STORED tier (D14, migration
/// 0099).
///
/// `stored` is `None` for a caller that has only the wire session, and for a
/// row written before 0099, whose tier is `unknown`. Both fall back to
/// [`tier_of`], so the pre-migration behaviour is exactly today's and no row
/// is given a tier that was reconstructed rather than recorded.
///
/// Separate from `status_row` rather than a new field on [`FleetSession`]:
/// the wire session is built by 34 struct literals across the workspace, and
/// the one surface that needs the stored value today is the daemon's own
/// `fleet/status`, which holds the store row. The TUI Fleet panel reads that
/// method's rows directly (#962), so it never calls this derivation itself.
#[must_use]
pub fn status_row_with_tier(
    session: &FleetSession,
    has_open_request: bool,
    stored: Option<Tier>,
) -> AgentStatusRow {
    let tier = stored.unwrap_or_else(|| tier_of(session));
    let state = state_of(session, tier, has_open_request);
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
        pane_unbound: session.pane_binding == PaneBinding::PaneUnbound,
        // Filled in by the daemon, which holds the binding decision. The
        // derivation here has only the wire session and cannot know why.
        pane_unbound_detail: None,
        // Stamped by the daemon from the stored row; the wire session has none.
        host_id: local_host_id(),
        turn_complete: session.lifecycle == LifecycleState::TurnComplete,
        wait_kind: (state == AgentState::Waiting)
            .then(|| wait_kind_of(session.attention))
            .flatten(),
        attachment: attachment_of(session),
    }
}

const fn wait_kind_of(attention: AttentionState) -> Option<WaitKind> {
    match attention {
        AttentionState::Ask => Some(WaitKind::Ask),
        AttentionState::Approval => Some(WaitKind::Approval),
        AttentionState::Waiting => Some(WaitKind::Waiting),
        AttentionState::Error => Some(WaitKind::Error),
        AttentionState::None => None,
    }
}

/// The attachment a surface offers, from capabilities and the pane binding.
fn attachment_of(session: &FleetSession) -> Attachment {
    let capabilities = &session.capabilities;
    if capabilities.tmux_attach && session.tmux_target.is_some() {
        Attachment::Tmux
    } else if session.pane_binding == PaneBinding::PaneUnbound {
        // Never collapsed into `None`: `None` reads as "never attachable",
        // `Unbound` names a missing pane an operator can chase in `ainb doctor`.
        Attachment::Unbound
    } else if session.management == ManagementState::Managed
        && (capabilities.structured_answer
            || capabilities.structured_dismiss
            || capabilities.approvals
            || capabilities.approval_session
            || capabilities.send_prompt
            || capabilities.continue_turn
            || capabilities.retry
            || capabilities.interrupt
            || capabilities.start
            || capabilities.stop
            || capabilities.restart
            || capabilities.kill
            || capabilities.archive
            || capabilities.verified_picker)
    {
        Attachment::Remote
    } else {
        Attachment::None
    }
}

/// Parse a stored tier token, or `None` for `unknown` and for anything this
/// build does not recognise.
///
/// `unknown` is not an error and not a tier: it is a row from before 0099
/// saying so, and the caller falls back to [`tier_of`]. An unrecognised token
/// takes the same path rather than failing the read, for the same reason the
/// event normalizer answers `None` instead of erroring.
#[must_use]
pub fn parse_tier(token: &str) -> Option<Tier> {
    match token {
        "hook" => Some(Tier::Hook),
        "acp_feed" => Some(Tier::AcpFeed),
        "osc_frame" => Some(Tier::OscFrame),
        "process" => Some(Tier::Process),
        "transcript" => Some(Tier::Transcript),
        "pane_text" => Some(Tier::PaneText),
        _ => None,
    }
}

/// The stored token for a tier, the inverse of [`parse_tier`].
#[must_use]
pub fn tier_token(tier: Tier) -> &'static str {
    match tier {
        Tier::Hook => "hook",
        Tier::AcpFeed => "acp_feed",
        Tier::OscFrame => "osc_frame",
        Tier::Process => "process",
        Tier::Transcript => "transcript",
        Tier::PaneText => "pane_text",
    }
}

/// Which tier's evidence this row's state rests on, DERIVED.
///
/// The fallback for a row written before migration 0099, and for a caller that
/// holds only the wire session. It reaches three of the six values: an ACP
/// session is tier 1 by construction, a row carrying a provider session id was
/// keyed by a hook, and everything else reads as the tmux scan. A row actually
/// written by an OSC frame, the process table or the transcript is
/// indistinguishable here from a pane scrape, which is why 0099 stores it and
/// [`status_row_with_tier`] prefers the column.
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
fn state_of(session: &FleetSession, tier: Tier, has_open_request: bool) -> AgentState {
    // Only tiers 0 and 1 may assert that a human is needed. A pane scrape that
    // reads like a prompt is a guess, and acting on it would raise a card
    // nobody can answer.
    //
    // And only while the inbox still holds an open request for the session
    // (#962). Answering closes the card at once, but the session's attention
    // string keeps reading `ASK` until the agent's next hook clears it. Reading
    // the string alone made the one truth say `waiting` for however long that
    // took, on every surface, about a question already answered.
    if tier.may_assert_needs_input() && has_open_request {
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

    /// #962: an answered question is not a wait. The card closed, so the
    /// session's still-`ASK` attention string (awaiting the agent's clearing
    /// hook) must fall back to its lifecycle instead of reporting `waiting`.
    #[test]
    fn a_hook_ask_with_no_open_request_is_not_waiting() {
        let answered = status_row(
            &session(LifecycleState::Running, AttentionState::Ask),
            false,
        );
        assert_eq!(answered.state, AgentState::Working);
        let idle = status_row(&session(LifecycleState::Idle, AttentionState::Ask), false);
        assert_eq!(idle.state, AgentState::Idle);
        assert!(!idle.has_open_request);
    }

    /// #1015: the row carries its own refinements, so no surface re-reads the
    /// lifecycle or attention strings to decide `done`, the wait kind, or how
    /// to attach.
    #[test]
    fn the_row_carries_turn_complete_wait_kind_and_attachment() {
        let mut asking = session(LifecycleState::Running, AttentionState::Approval);
        asking.capabilities.tmux_attach = true;
        let row = status_row(&asking, true);
        assert_eq!(row.wait_kind, Some(WaitKind::Approval));
        assert!(!row.turn_complete);
        assert_eq!(row.attachment, Attachment::Tmux);
        assert_eq!(row.host_id, LOCAL_HOST_ID);

        let answered = status_row(&asking, false);
        assert_eq!(answered.wait_kind, None, "no wait kind without a wait");

        let mut finished = session(LifecycleState::TurnComplete, AttentionState::None);
        finished.pane_binding = PaneBinding::PaneUnbound;
        let row = status_row(&finished, false);
        assert_eq!(row.state, AgentState::Idle);
        assert!(row.turn_complete);
        assert_eq!(row.attachment, Attachment::Unbound);

        let mut remote = session(LifecycleState::Idle, AttentionState::None);
        remote.capabilities.send_prompt = true;
        assert_eq!(status_row(&remote, false).attachment, Attachment::Remote);
        assert_eq!(
            status_row(&session(LifecycleState::Idle, AttentionState::None), false).attachment,
            Attachment::None
        );
    }

    /// An older daemon's `fleet/status` row decodes with the local host and
    /// no refinements rather than failing the read.
    #[test]
    fn a_row_without_the_refinements_decodes_as_local() {
        let mut value = serde_json::to_value(status_row(
            &session(LifecycleState::Idle, AttentionState::None),
            false,
        ))
        .unwrap();
        for field in ["host_id", "turn_complete", "wait_kind", "attachment"] {
            value.as_object_mut().unwrap().remove(field);
        }
        let row: AgentStatusRow = serde_json::from_value(value).unwrap();
        assert_eq!(row.host_id, LOCAL_HOST_ID);
        assert!(!row.turn_complete);
        assert_eq!(row.wait_kind, None);
        assert_eq!(row.attachment, Attachment::None);
    }

    /// The one join: one row per session both reads name, stamped with the
    /// older of the two revisions, and nothing for a session without a state.
    #[test]
    fn join_pairs_roster_and_status_per_session_key() {
        let mut other = session(LifecycleState::Idle, AttentionState::None);
        other.session_key = "claude:s-2".to_string();
        let snapshot = FleetSnapshot {
            head_revision: 9,
            sessions: vec![other, session(LifecycleState::Running, AttentionState::Ask)],
        };
        let status = AgentStatusResult {
            rows: vec![status_row(&snapshot.sessions[1], true)],
            head_revision: 8,
            unknown_events: Vec::new(),
        };
        let joined = join(&snapshot, &status);
        assert_eq!(joined.read_revision, 8);
        assert_eq!(joined.rows.len(), 1, "a session with no state is not a row");
        assert_eq!(joined.rows[0].session.session_key, "claude:s-1");
        assert_eq!(joined.rows[0].status.state, AgentState::Waiting);
        assert_eq!(joined.rows[0].read_revision, 8);
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
