//! Fleet control-plane wire types.
//!
//! Sessions use stable provider identity, independent lifecycle and attention
//! state, optimistic concurrency versions, and durable global revisions.

use serde::{Deserialize, Serialize};

/// Current Fleet wire protocol version.
///
/// v2 carries the one bump-and-refuse break for the chat bus: the `Acp`
/// provider token plus the `fleet/message_*`, `fleet/transcript_*` and
/// `fleet/acp_session_create` method family. Clients whose declared range
/// excludes 2 are refused on both the read and the write leg.
pub const FLEET_PROTOCOL_VERSION: u32 = 2;

/// Negotiated capability required for versioned selected-session actions.
pub const FLEET_CAPABILITY_ACTION_EXECUTE: &str = "fleet.action.execute";
/// Negotiated capability required for explicit-recipient broadcasts.
pub const FLEET_CAPABILITY_BROADCAST_EXECUTE: &str = "fleet.broadcast.execute";
/// Negotiated capability required for durable receipt list and exact lookup.
pub const FLEET_CAPABILITY_RECEIPT_READ: &str = "fleet.receipt.read";
/// Negotiated capability required for daemon-owned new-session starts.
pub const FLEET_CAPABILITY_START_EXECUTE: &str = "fleet.start.execute";
/// Negotiated capability for daemon-owned ATC read projections.
pub const FLEET_CAPABILITY_ATC_READ: &str = "fleet.atc.read";
/// Negotiated capability for the payload-free Fleet revision timeline.
pub const FLEET_CAPABILITY_TIMELINE_READ: &str = "fleet.timeline.read";
/// Negotiated capability for bounded daemon-owned usage summaries.
pub const FLEET_CAPABILITY_USAGE_READ: &str = "fleet.usage.read";
/// Negotiated capability for bounded live provider-quota summaries.
pub const FLEET_CAPABILITY_QUOTA_READ: &str = "fleet.quota.read";
/// Negotiated capability for the rich usage dashboard with 53-week history.
pub const FLEET_CAPABILITY_DASHBOARD_READ: &str = "fleet.dashboard.read";
/// Negotiated capability for runtime and provider-hook health.
pub const FLEET_CAPABILITY_RUNTIME_READ: &str = "fleet.runtime.read";
/// Negotiated capability required for chat-bus message sends.
pub const FLEET_CAPABILITY_MESSAGE_SEND: &str = "fleet.message.send";
/// Negotiated capability required for chat-bus message list and subscribe.
pub const FLEET_CAPABILITY_MESSAGE_READ: &str = "fleet.message.read";
/// Negotiated capability required for ACP transcript list and subscribe.
pub const FLEET_CAPABILITY_TRANSCRIPT_READ: &str = "fleet.transcript.read";
/// Negotiated capability required for daemon-owned ACP session creation.
pub const FLEET_CAPABILITY_ACP_SPAWN: &str = "fleet.acp.spawn";
/// Negotiated capability required for the operator export-then-delete of ACP
/// transcript rows.
///
/// Separate from [`FLEET_CAPABILITY_TRANSCRIPT_READ`] so the catalogue names the
/// destructive verb on its own: a build can ship the transcript read surface
/// without advertising the delete, and a client can tell the two apart.
///
/// ADVISORY, not an authorisation boundary (review 2026-08-07). Capabilities are
/// declared by the DAEMON and echoed by `fleet/negotiate`; a client declares
/// version ranges and never capabilities, and negotiate holds no
/// connection-scoped state, so the daemon-side check can only answer "does this
/// build advertise it", never "may this caller use it". Anyone who can call
/// `fleet/transcript_list` on a socket can call `fleet/transcript_prune` on it.
/// The real boundary is the socket itself: same-uid peer credentials plus the
/// bearer token (`rpc/auth.rs`). Splitting this into an enforced permission
/// needs per-connection granted capabilities, which is a v3 surface change.
pub const FLEET_CAPABILITY_TRANSCRIPT_PRUNE: &str = "fleet.transcript.prune";

/// Negotiated capability required to mint a chat channel (buzz-port part 2).
///
/// DEFINED here, deliberately ABSENT from [`FLEET_PROTOCOL_CAPABILITY_IDS`]:
/// part 2's dispatch arms do not exist yet, so advertising it would promise a
/// method that answers -32601. It is appended to the catalogue in the same
/// change that lands `fleet/channel_create`, which is exactly the rule the
/// chat-bus capabilities followed in part 1.
pub const FLEET_CAPABILITY_CHAT_WRITE: &str = "fleet.chat.write";
/// Negotiated capability required to read channels, confirms and activity.
///
/// Defined-but-unadvertised, per [`FLEET_CAPABILITY_CHAT_WRITE`].
pub const FLEET_CAPABILITY_CHAT_READ: &str = "fleet.chat.read";
/// Negotiated capability required for `fleet/copilot_configure`.
///
/// Defined-but-unadvertised, per [`FLEET_CAPABILITY_CHAT_WRITE`]. Gates a
/// PRIVILEGED surface: the persona it carries is a system prompt for an agent
/// holding destructive tools.
pub const FLEET_CAPABILITY_COPILOT_CONFIGURE: &str = "fleet.copilot.configure";
/// Negotiated capability required to answer a guardrail confirm card.
///
/// Defined-but-unadvertised, per [`FLEET_CAPABILITY_CHAT_WRITE`]. Distinct
/// from `fleet.action.execute`: ACP permission requests stay part 1's
/// attention rows answered through `fleet/action`.
pub const FLEET_CAPABILITY_CONFIRM_ANSWER: &str = "fleet.confirm.answer";
/// Negotiated capability required to run one copilot tool call through the
/// guardrail (`fleet/copilot_gate`).
///
/// The copilot's MCP tool server is a separate process, so the classify-park-
/// resolve decision has to cross a socket to reach the daemon that owns it.
/// This is the ONLY caller-visible name for that crossing, and it is separate
/// from [`FLEET_CAPABILITY_CONFIRM_ANSWER`] on purpose: minting a card and
/// answering one are opposite ends of the same dialog, held by different
/// processes.
pub const FLEET_CAPABILITY_COPILOT_GATE: &str = "fleet.copilot.gate";

/// Fleet capability identifiers advertised during protocol negotiation.
///
/// The chat-bus capability consts above are part of the frozen v2 surface but
/// are appended here only in the phase their dispatch arms land, so no daemon
/// build ever advertises a capability whose methods answer -32601.
pub const FLEET_PROTOCOL_CAPABILITY_IDS: &[&str] = &[
    FLEET_CAPABILITY_ACP_SPAWN,
    FLEET_CAPABILITY_ACTION_EXECUTE,
    // Part 2 phase A2 landed the six chat/copilot dispatch arms, so their
    // capabilities are advertised in the SAME change, per the rule below.
    FLEET_CAPABILITY_CHAT_READ,
    FLEET_CAPABILITY_CHAT_WRITE,
    FLEET_CAPABILITY_CONFIRM_ANSWER,
    FLEET_CAPABILITY_COPILOT_CONFIGURE,
    FLEET_CAPABILITY_COPILOT_GATE,
    FLEET_CAPABILITY_ATC_READ,
    FLEET_CAPABILITY_BROADCAST_EXECUTE,
    FLEET_CAPABILITY_MESSAGE_READ,
    FLEET_CAPABILITY_MESSAGE_SEND,
    "fleet.protocol.negotiate",
    FLEET_CAPABILITY_RECEIPT_READ,
    FLEET_CAPABILITY_QUOTA_READ,
    "fleet.snapshot.read",
    FLEET_CAPABILITY_START_EXECUTE,
    "fleet.subscription.live",
    "fleet.subscription.replay",
    "fleet.subscription.resync",
    FLEET_CAPABILITY_TIMELINE_READ,
    FLEET_CAPABILITY_TRANSCRIPT_PRUNE,
    FLEET_CAPABILITY_TRANSCRIPT_READ,
    FLEET_CAPABILITY_RUNTIME_READ,
    FLEET_CAPABILITY_USAGE_READ,
    FLEET_CAPABILITY_DASHBOARD_READ,
];

/// Inclusive supported protocol version range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FleetProtocolRange {
    /// Lowest supported version.
    pub min: u32,
    /// Highest supported version.
    pub max: u32,
}

impl FleetProtocolRange {
    /// Whether this is a non-empty protocol range.
    #[must_use]
    pub const fn is_valid(self) -> bool {
        self.min > 0 && self.min <= self.max
    }

    /// Whether `version` is in this inclusive range.
    #[must_use]
    pub const fn contains(self, version: u32) -> bool {
        self.min <= version && version <= self.max
    }
}

/// Parameters for `fleet/negotiate`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FleetNegotiateParams {
    /// Stable client implementation name.
    pub client_name: String,
    /// Client implementation version.
    pub client_version: String,
    /// Versions the client can safely read.
    pub read_versions: FleetProtocolRange,
    /// Versions the client can safely write.
    pub write_versions: FleetProtocolRange,
}

/// Result from `fleet/negotiate`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FleetNegotiateResult {
    /// Daemon build version.
    pub daemon_version: String,
    /// Exact daemon Fleet protocol version.
    pub protocol_version: u32,
    /// Whether the client can safely read this daemon.
    pub read_compatible: bool,
    /// Whether the client can safely write to this daemon.
    pub write_compatible: bool,
    /// Stable, ordered daemon capability catalogue.
    pub capability_ids: Vec<String>,
}

/// Session provider.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FleetProvider {
    /// Claude Code.
    Claude,
    /// OpenAI Codex.
    Codex,
    /// Google Antigravity.
    Antigravity,
    /// GitHub Copilot CLI.
    Copilot,
    /// ACP-backed headless session; the concrete adapter lives in the
    /// `fleet_acp_session` store row, not on the wire.
    Acp,
    /// Provider could not be determined.
    #[default]
    Unknown,
}

/// Provider session lifecycle, independent from attention.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum LifecycleState {
    /// Process or provider thread is starting.
    Starting,
    /// Turn is actively running.
    Running,
    /// Provider reported turn completion.
    TurnComplete,
    /// Session is alive without an active turn.
    Idle,
    /// Session exited.
    Exited,
    /// Lifecycle cannot be determined.
    #[default]
    Unknown,
}

/// Operator attention state, independent from lifecycle.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum AttentionState {
    /// No operator action requested.
    #[default]
    None,
    /// Provider asks a structured question.
    Ask,
    /// Provider requests approval.
    Approval,
    /// Session is waiting without a structured request.
    Waiting,
    /// Session reported an error needing attention.
    Error,
}

/// Whether Hangar has authoritative provider control.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ManagementState {
    /// Provider adapter supports authoritative actions.
    Managed,
    /// Only discovery or fallback control is available.
    #[default]
    Degraded,
}

/// Health of the preferred provider transport.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum TransportHealth {
    /// Preferred transport is responsive.
    Healthy,
    /// Preferred transport has partial capability.
    Degraded,
    /// Preferred transport is unavailable.
    Unavailable,
    /// Transport state is not known.
    #[default]
    Unknown,
}

/// Authority of observed state.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FleetProvenance {
    /// Exact provider or lifecycle-hook observation.
    Authoritative,
    /// Tmux, process, or transcript inference.
    #[default]
    Inferred,
}

/// Confidence assigned to session identity and state.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum FleetConfidence {
    /// Exact stable identity and authoritative state.
    High,
    /// Stable identity with partially inferred state.
    Medium,
    /// Legacy or weakly inferred identity and state.
    #[default]
    Low,
}

/// Provider and transport actions available for one session.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct FleetCapabilities {
    /// Answer exact structured provider requests.
    pub structured_answer: bool,
    /// Reject an exact structured request without fabricating an answer.
    pub structured_dismiss: bool,
    /// Approve or deny exact provider requests.
    pub approvals: bool,
    /// Approve an exact request for the remainder of the current provider session.
    pub approval_session: bool,
    /// Send generic prompt text.
    pub send_prompt: bool,
    /// Continue a paused session.
    pub continue_turn: bool,
    /// Retry a failed turn.
    pub retry: bool,
    /// Interrupt an active turn.
    pub interrupt: bool,
    /// Start a provider session.
    pub start: bool,
    /// Stop a session gracefully.
    pub stop: bool,
    /// Restart a session.
    pub restart: bool,
    /// Kill a session process.
    pub kill: bool,
    /// Archive a session.
    pub archive: bool,
    /// Attach through tmux.
    pub tmux_attach: bool,
    /// Send plain text through tmux.
    pub tmux_text: bool,
    /// Route verified picker keys through tmux.
    pub verified_picker: bool,
}

/// Canonical Fleet session read-model row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FleetSession {
    /// Stable identity, never cwd.
    pub session_key: String,
    /// Session provider.
    pub provider: FleetProvider,
    /// Provider-owned session identifier.
    pub provider_session_id: Option<String>,
    /// Exact tmux target for attach or fallback.
    pub tmux_target: Option<String>,
    /// Process-start fingerprint for legacy identity.
    pub process_start_fingerprint: Option<String>,
    /// Working directory metadata.
    pub cwd: String,
    /// Human-readable session label.
    pub display_name: Option<String>,
    /// Independent lifecycle state.
    pub lifecycle: LifecycleState,
    /// Number of active provider child tasks, agents, or threads.
    #[serde(default)]
    pub active_work_count: i64,
    /// Independent attention state.
    pub attention: AttentionState,
    /// Fingerprint of current structured request or approval.
    pub current_request_fingerprint: Option<String>,
    /// Complete current structured request for rendering and exact routing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_request: Option<serde_json::Value>,
    /// Managed or degraded control state.
    pub management: ManagementState,
    /// Preferred transport health.
    pub transport_health: TransportHealth,
    /// Available actions.
    pub capabilities: FleetCapabilities,
    /// Last accepted state provenance.
    pub provenance: FleetProvenance,
    /// Identity and state confidence.
    pub confidence: FleetConfidence,
    /// First discovery time in epoch milliseconds.
    pub discovered_at: i64,
    /// Last accepted observation time in epoch milliseconds.
    pub last_observed_at: i64,
    /// Last lifecycle observation time in epoch milliseconds.
    pub lifecycle_updated_at: i64,
    /// Last attention observation time in epoch milliseconds.
    pub attention_updated_at: i64,
    /// Provider-reported model id, verbatim. Absent means never observed, which
    /// is NOT the same as a default model: the key is omitted rather than null
    /// so a client cannot mistake absence for an explicit value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Provider-reported reasoning effort, verbatim. Absent means never observed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    /// Last model observation time in epoch milliseconds. 0 means never observed.
    #[serde(default)]
    pub model_updated_at: i64,
    /// Optimistic concurrency version.
    pub version: i64,
    /// Global revision that last changed this session.
    pub updated_revision: i64,
}

/// Consistent Fleet snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FleetSnapshot {
    /// Highest revision included by the snapshot transaction.
    pub head_revision: i64,
    /// Canonical sessions ordered by stable key.
    pub sessions: Vec<FleetSession>,
}

/// Parameters for `fleet/snapshot`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FleetSnapshotParams {}

/// Parameters for `fleet/subscribe`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FleetSubscribeParams {
    /// Last revision committed by the subscriber.
    #[serde(default)]
    pub after_revision: i64,
}

/// Why a subscription acknowledgement requires a fresh snapshot baseline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FleetReplayResetReason {
    /// Initial subscribe has no prior cursor.
    Bootstrap,
    /// Caller cursor is newer than daemon durable head.
    CursorAhead,
    /// Missed durable interval exceeds the bounded replay response.
    ReplayLimitExceeded,
}

/// Replay status for a subscription acknowledgement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum FleetReplayState {
    /// Replay exactly covers the requested interval.
    Complete,
    /// Snapshot replaces the requested replay interval.
    SnapshotReset {
        /// Reason the daemon cannot provide a complete replay interval.
        reason: FleetReplayResetReason,
    },
}

impl<'de> Deserialize<'de> for FleetReplayState {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::Error;

        let value = serde_json::Value::deserialize(deserializer)?;
        let object = value
            .as_object()
            .ok_or_else(|| D::Error::custom("FleetReplayState must be an object"))?;
        let state = object
            .get("state")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| D::Error::custom("FleetReplayState requires string state"))?;
        match state {
            "complete" if object.len() == 1 => Ok(Self::Complete),
            "snapshot_reset" if object.len() == 2 => {
                let reason = object
                    .get("reason")
                    .cloned()
                    .ok_or_else(|| D::Error::custom("snapshot_reset requires reason"))?;
                let reason = serde_json::from_value(reason).map_err(D::Error::custom)?;
                Ok(Self::SnapshotReset { reason })
            }
            "complete" => Err(D::Error::custom("complete carries unsupported fields")),
            "snapshot_reset" => Err(D::Error::custom("snapshot_reset requires only reason")),
            _ => Err(D::Error::custom("unknown FleetReplayState state")),
        }
    }
}

/// One durable change emitted after a subscription cursor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FleetEvent {
    /// Global monotonic revision.
    pub revision: i64,
    /// Replay-safe event identity.
    pub event_id: String,
    /// Stable target session.
    pub session_key: String,
    /// Observation time in epoch milliseconds.
    pub observed_at: i64,
    /// Event authority.
    pub provenance: FleetProvenance,
    /// Normalized event discriminator.
    pub event_type: String,
    /// Provider or normalized event body.
    pub payload: serde_json::Value,
    /// Session version after event consideration.
    pub session_version: i64,
    /// Whether event changed canonical state.
    pub applied: bool,
}

/// Closed, payload-free Fleet timeline event classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FleetTimelineKind {
    /// Provider session began.
    SessionStarted,
    /// A provider turn started or continued.
    TurnRunning,
    /// Provider asked a structured question.
    QuestionRaised,
    /// Provider requested approval.
    ApprovalRequested,
    /// Provider requested operator attention.
    AttentionWaiting,
    /// Provider turn completed.
    TurnCompleted,
    /// Provider turn completed with failure.
    TurnFailed,
    /// Provider session ended.
    SessionEnded,
    /// Codex managed transport became unavailable.
    ManagerUnavailable,
    /// Codex managed transport recovered.
    ManagerRecovered,
    /// Codex managed TUI started.
    ManagerStarted,
    /// Tmux transport became unavailable.
    TransportUnavailable,
    /// Tmux transport became available.
    TransportAvailable,
    /// Tmux discovery found a session.
    SessionDiscovered,
    /// A legacy session was superseded by its managed identity.
    SessionSuperseded,
}

impl FleetTimelineKind {
    /// Map one known stored Fleet event type to its public closed kind.
    #[must_use]
    pub fn from_event_type(event_type: &str) -> Option<Self> {
        match event_type {
            "SessionStart" => Some(Self::SessionStarted),
            "UserPromptSubmit" | "PreToolUse" | "PostToolUse" => Some(Self::TurnRunning),
            "AskUserQuestion" => Some(Self::QuestionRaised),
            "PermissionRequest" => Some(Self::ApprovalRequested),
            "Notification" => Some(Self::AttentionWaiting),
            "Stop" | "SubagentStop" => Some(Self::TurnCompleted),
            "StopFailure" => Some(Self::TurnFailed),
            "SessionEnd" => Some(Self::SessionEnded),
            "codex_manager_unavailable" => Some(Self::ManagerUnavailable),
            "codex_manager_recovered" => Some(Self::ManagerRecovered),
            "codex_managed_tui_started" => Some(Self::ManagerStarted),
            "tmux_missing" | "tmux_unavailable" => Some(Self::TransportUnavailable),
            "tmux_available" => Some(Self::TransportAvailable),
            "tmux_discovered" => Some(Self::SessionDiscovered),
            "session_superseded" => Some(Self::SessionSuperseded),
            _ => None,
        }
    }
}

/// Parameters for `fleet/timeline`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FleetTimelineParams {
    /// Return rows strictly after this global Fleet revision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after_revision: Option<i64>,
    /// Optional exact stable Fleet session identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_key: Option<String>,
    /// Requested row count. The daemon clamps this to its server maximum.
    pub limit: u32,
}

/// One payload-free Fleet timeline entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FleetTimelineEntry {
    /// Global monotonic revision.
    pub revision: i64,
    /// Stable target session.
    pub session_key: String,
    /// Observation time in epoch milliseconds.
    pub observed_at: i64,
    /// Event authority.
    pub provenance: FleetProvenance,
    /// Closed event classification.
    pub kind: FleetTimelineKind,
    /// Whether this event changed canonical state.
    pub applied: bool,
    /// Session version after event consideration.
    pub session_version: i64,
}

/// Result for `fleet/timeline`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FleetTimelineResult {
    /// Entries in ascending global revision order.
    pub entries: Vec<FleetTimelineEntry>,
    /// Cursor for the next page, or `null` when this page is empty.
    pub next_after_revision: Option<i64>,
}

/// Bounded reporting period accepted by `fleet/usage_summary`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FleetUsagePeriod {
    /// Current UTC calendar day.
    Today,
    /// Seven completed-or-current UTC calendar days.
    #[serde(rename = "trailing_7_days")]
    #[default]
    Trailing7Days,
    /// Thirty completed-or-current UTC calendar days.
    #[serde(rename = "trailing_30_days")]
    Trailing30Days,
}

/// Parameters for `fleet/usage_summary`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FleetUsageSummaryParams {
    /// Bounded period the daemon aggregates.
    #[serde(default)]
    pub period: FleetUsagePeriod,
}

/// Maximum daily buckets the daemon may return for one usage summary.
pub const FLEET_USAGE_MAX_DAILY_BUCKETS: usize = 30;
/// Maximum provider, model, or project buckets per usage-summary breakdown.
pub const FLEET_USAGE_MAX_BREAKDOWN_BUCKETS: usize = 10;
/// Maximum UTF-8 bytes in a safe usage-summary detail message.
pub const FLEET_USAGE_DETAIL_MAX_BYTES: usize = 1_024;

/// Availability of the daemon-owned usage projection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FleetUsageSummaryState {
    /// The daemon is building a summary; no durable totals are ready yet.
    Scanning,
    /// Complete summary for every configured usage source.
    Ready,
    /// Summary is useful but one or more sources could not be fully read.
    Partial,
    /// The daemon cannot provide a summary for this request.
    Unavailable,
}

/// Aggregated token and optional canonical USD cost values.
///
/// `cost_usd` is `None` when no canonical model rate covers every included
/// call. Clients must show tokens instead of synthesising a zero cost.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FleetUsageBucket {
    /// Input tokens.
    pub input_tokens: u64,
    /// Prompt-cache creation tokens.
    pub cache_creation_tokens: u64,
    /// Prompt-cache read tokens.
    pub cache_read_tokens: u64,
    /// Output tokens.
    pub output_tokens: u64,
    /// Reasoning tokens.
    pub reasoning_tokens: u64,
    /// Number of source calls contributing to this aggregate.
    pub call_count: u64,
    /// Number of distinct source sessions contributing to this aggregate.
    pub session_count: u64,
    /// Number of distinct projects contributing to this aggregate.
    pub project_count: u64,
    /// Canonical USD cost when fully priced.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
}

/// One daily usage bucket.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FleetUsageDailyBucket {
    /// ISO-8601 UTC calendar date (`YYYY-MM-DD`).
    pub date: String,
    /// Daily aggregate.
    pub bucket: FleetUsageBucket,
}

/// One provider usage bucket.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FleetUsageProviderBucket {
    /// Canonical provider token from the usage producer.
    pub provider: String,
    /// Provider aggregate.
    pub bucket: FleetUsageBucket,
}

/// One model usage bucket.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FleetUsageModelBucket {
    /// Exact model identifier from the usage producer.
    pub model: String,
    /// Model aggregate.
    pub bucket: FleetUsageBucket,
}

/// One project usage bucket.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FleetUsageProjectBucket {
    /// Human-readable project aggregation key.
    pub project: String,
    /// Resolved upstream repository when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    /// Project aggregate.
    pub bucket: FleetUsageBucket,
}

/// Bounded result for `fleet/usage_summary`.
///
/// Timestamps are epoch milliseconds. A non-`Ready` response may omit every
/// projection field and use `detail` to explain the unavailable or partial
/// source state without exposing local paths, credentials, or raw transcripts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FleetUsageSummaryResult {
    /// Current summary availability.
    pub state: FleetUsageSummaryState,
    /// Time the daemon generated this summary, in epoch milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generated_at: Option<i64>,
    /// Inclusive summary window start, in epoch milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_at: Option<i64>,
    /// Exclusive summary window end, in epoch milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_at: Option<i64>,
    /// Aggregate for the requested period.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub totals: Option<FleetUsageBucket>,
    /// Daily buckets, chronologically ordered and capped by
    /// [`FLEET_USAGE_MAX_DAILY_BUCKETS`].
    #[serde(default)]
    pub daily: Vec<FleetUsageDailyBucket>,
    /// Provider aggregates, producer-defined descending order and capped by
    /// [`FLEET_USAGE_MAX_BREAKDOWN_BUCKETS`].
    #[serde(default)]
    pub providers: Vec<FleetUsageProviderBucket>,
    /// Top model aggregates, capped by [`FLEET_USAGE_MAX_BREAKDOWN_BUCKETS`].
    #[serde(default)]
    pub models: Vec<FleetUsageModelBucket>,
    /// Top project aggregates, capped by [`FLEET_USAGE_MAX_BREAKDOWN_BUCKETS`].
    #[serde(default)]
    pub projects: Vec<FleetUsageProjectBucket>,
    /// Safe daemon status detail for partial or unavailable summaries, capped
    /// by [`FLEET_USAGE_DETAIL_MAX_BYTES`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

// ---------------------------------------------------------------------------
// fleet/usage_dashboard
// ---------------------------------------------------------------------------

/// Maximum weekly buckets the daemon returns for a dashboard response.
pub const FLEET_DASHBOARD_MAX_WEEKLY_BUCKETS: usize = 53;
/// Maximum heatmap cells (one per day, 53 weeks).
pub const FLEET_DASHBOARD_MAX_HEATMAP_CELLS: usize = 371;
/// Maximum session/branch/tool/mcp/shell breakdown buckets.
pub const FLEET_DASHBOARD_MAX_DIMENSION_BUCKETS: usize = 20;

/// Parameters for `fleet/usage_dashboard`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FleetUsageDashboardParams {}

/// One heatmap cell representing a single calendar day.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FleetHeatmapCell {
    /// ISO-8601 UTC calendar date.
    pub date: String,
    /// Number of distinct API calls on this day.
    pub call_count: u64,
    /// Canonical USD cost when fully priced.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
}

/// One weekly usage bucket.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FleetUsageWeeklyBucket {
    /// ISO-8601 UTC date of Monday starting this week.
    pub week_start: String,
    /// Weekly aggregate.
    pub bucket: FleetUsageBucket,
}

/// One session usage bucket.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FleetUsageSessionBucket {
    /// The BARE session id, not a composite.
    ///
    /// `provider` and `project` are separate fields below; a client that wants
    /// the full identity joins the three itself. This once shipped as
    /// `provider:project:session_id`, which no client could re-split because a
    /// project label may contain a colon.
    pub session_id: String,
    /// Provider for this session.
    pub provider: String,
    /// Project for this session.
    pub project: String,
    /// Session aggregate.
    pub bucket: FleetUsageBucket,
}

/// One branch usage bucket.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FleetUsageBranchBucket {
    /// Git branch name.
    pub branch: String,
    /// Branch aggregate.
    pub bucket: FleetUsageBucket,
}

/// One named dimension bucket (tools, MCP servers, shell commands).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FleetUsageNamedBucket {
    /// Dimension key (tool name, MCP server, shell command).
    pub name: String,
    /// Number of calls using this dimension.
    pub call_count: u64,
}

/// Simple linear forecast from trailing daily data.
///
/// Averages are taken over CALENDAR days in the trailing window, not over the
/// days that happen to have data: the next 30 days will contain idle days too,
/// so an active-days-only divisor would quote a working-day rate and overstate
/// an intermittent user's projection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FleetUsageForecast {
    /// Projected cost for the next 30 days in USD, when fully priced.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub projected_30d_cost_usd: Option<f64>,
    /// Projected total tokens for the next 30 days.
    pub projected_30d_tokens: u64,
    /// Mean cost per calendar day across `sample_days`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avg_daily_cost_usd: Option<f64>,
    /// Mean tokens per calendar day across `sample_days`.
    pub avg_daily_tokens: u64,
    /// The divisor actually used: calendar days spanned by the trailing window,
    /// capped at 7 and floored at 1 so a first-day user is not diluted across a
    /// week they were not present for.
    pub sample_days: u32,
}

/// Bounded rich dashboard result for `fleet/usage_dashboard`.
///
/// Extends `fleet/usage_summary` with 53-week history, heatmap, forecast,
/// and additional breakdowns. `fleet/usage_summary` remains the stable
/// lightweight endpoint for quick checks.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FleetUsageDashboardResult {
    /// Current dashboard availability.
    pub state: FleetUsageSummaryState,
    /// Time the daemon generated this dashboard, in epoch milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generated_at: Option<i64>,
    /// Inclusive window start, in epoch milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_at: Option<i64>,
    /// Exclusive window end, in epoch milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_at: Option<i64>,
    /// Whether every included call has a canonical USD rate.
    pub cost_complete: bool,
    /// Aggregate for the full 53-week window.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub totals: Option<FleetUsageBucket>,
    /// Weekly buckets, chronologically ordered.
    #[serde(default)]
    pub weekly: Vec<FleetUsageWeeklyBucket>,
    /// Daily activity heatmap cells for 53 weeks.
    #[serde(default)]
    pub heatmap: Vec<FleetHeatmapCell>,
    /// Linear forecast from trailing data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forecast: Option<FleetUsageForecast>,
    /// Provider breakdowns.
    #[serde(default)]
    pub providers: Vec<FleetUsageProviderBucket>,
    /// Top model breakdowns.
    #[serde(default)]
    pub models: Vec<FleetUsageModelBucket>,
    /// Top project breakdowns.
    #[serde(default)]
    pub projects: Vec<FleetUsageProjectBucket>,
    /// Top session breakdowns.
    #[serde(default)]
    pub sessions: Vec<FleetUsageSessionBucket>,
    /// Top branch breakdowns.
    #[serde(default)]
    pub branches: Vec<FleetUsageBranchBucket>,
    /// Top tool usage counts.
    #[serde(default)]
    pub tools: Vec<FleetUsageNamedBucket>,
    /// Top MCP server usage counts.
    #[serde(default)]
    pub mcp_servers: Vec<FleetUsageNamedBucket>,
    /// Top shell command usage counts.
    #[serde(default)]
    pub shell_commands: Vec<FleetUsageNamedBucket>,
    /// Safe daemon status detail.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// One live provider quota window. `used_percent` is provider-reported, so
/// clients derive remaining quota as `100 - used_percent` without guessing a
/// token cap.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FleetQuotaWindow {
    /// Provider-reported used percentage, clamped to 0..=100 by the producer.
    pub used_percent: u8,
    /// Absolute reset instant in epoch milliseconds when the provider supplied it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resets_at: Option<i64>,
    /// `true` when this came from local transcript estimation, not provider data.
    #[serde(default)]
    pub estimated: bool,
}

/// Parameters for `fleet/quota_summary`. Reserved for future provider filters.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FleetQuotaSummaryParams {}

/// Live quota projection for one provider. Absent windows mean unavailable,
/// not zero usage or unlimited capacity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FleetQuotaProvider {
    /// Stable provider identifier, currently `claude` or `codex`.
    pub provider: String,
    /// Provider's rolling five-hour window, when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub five_hour: Option<FleetQuotaWindow>,
    /// Provider's rolling seven-day window, when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seven_day: Option<FleetQuotaWindow>,
    /// Provider plan tier, when its source reports one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_type: Option<String>,
    /// Source observation time in epoch milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<i64>,
}

/// Bounded daemon-owned live quota result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FleetQuotaSummaryResult {
    /// Availability of the current projection.
    pub state: FleetUsageSummaryState,
    /// Time the daemon assembled this projection, in epoch milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generated_at: Option<i64>,
    /// At most one row per supported provider.
    #[serde(default)]
    pub providers: Vec<FleetQuotaProvider>,
    /// Safe status detail for stale, partial, or unavailable data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Parameters for `fleet/runtime_status`. Kept extensible for additive
/// runtime probes without exposing daemon implementation state.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FleetRuntimeStatusParams {}

/// Health of one supported provider hook.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FleetRuntimeHookStatus {
    /// Stable provider identifier.
    pub provider: String,
    /// The supported installer recorded this provider as installed.
    pub installed: bool,
    /// The installed hook program is present and executable.
    pub hook_ready: bool,
    /// Hook delivery socket is currently accepting connections.
    pub delivery_ready: bool,
    /// The daemon has observed a recent provider event, when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_event: Option<String>,
}

/// One local Codex app-server discovered by the Hangar daemon.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodexAppServerRuntimeStatus {
    /// Process identifier on this machine.
    pub pid: u32,
    /// `owned`, `adopted`, or `external`.
    pub ownership: String,
    /// Whether process argv requested Codex remote control enrollment.
    pub remote_control: bool,
    /// `healthy`, `degraded`, or `unknown` for external processes.
    pub health: String,
}

/// Bounded runtime health returned only by the public Fleet RPC.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FleetRuntimeStatusResult {
    /// Daemon version serving this connection.
    pub daemon_version: String,
    /// Fleet protocol version serving this connection.
    pub protocol_version: u32,
    /// Supported provider hook states. No filesystem paths are exposed.
    pub hooks: Vec<FleetRuntimeHookStatus>,
    /// Bounded local Codex app-server inventory. No filesystem paths are exposed.
    #[serde(default)]
    pub codex_app_servers: Vec<CodexAppServerRuntimeStatus>,
}

/// Initial result for a replay-safe Fleet subscription.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FleetSubscribeResult {
    /// Consistent snapshot registered against live delivery.
    pub snapshot: FleetSnapshot,
    /// Events in `(after_revision, snapshot.head_revision]` when replay is complete.
    pub replay: Vec<FleetEvent>,
    /// Whether the response has complete replay coverage or resets the cursor.
    pub replay_state: FleetReplayState,
}

/// One answer to one provider question.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FleetQuestionAnswer {
    /// Exact provider question identifier.
    pub question_id: String,
    /// Selected option identifiers or labels in provider order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub selected_options: Vec<String>,
    /// Free-text answer when question permits it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

/// Exact provider request routing identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FleetRequestIdentity {
    /// Exact JSON-RPC request id for provider response routing.
    pub request_id: serde_json::Value,
    /// Exact provider thread id.
    pub thread_id: String,
    /// Exact provider turn id.
    pub turn_id: String,
    /// Exact provider item id.
    pub item_id: String,
}

/// Typed Fleet control action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum ControlAction {
    /// Answer exact structured request without generic text fallback.
    StructuredAnswer {
        /// Provider request fingerprint verified against current state.
        request_fingerprint: String,
        /// Exact provider routing identity when provider protocol requires it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request_identity: Option<FleetRequestIdentity>,
        /// Complete ordered answers for all questions.
        answers: Vec<FleetQuestionAnswer>,
    },
    /// Reject exact structured request when provider exposes a safe rejection route.
    DismissStructured {
        /// Provider request fingerprint verified against current state.
        request_fingerprint: String,
        /// Exact provider routing identity when provider protocol requires it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request_identity: Option<FleetRequestIdentity>,
    },
    /// Yield an intercepted Claude interview to its native terminal picker.
    ReleaseStructured {
        /// Provider request fingerprint verified against current state.
        request_fingerprint: String,
    },
    /// Reconcile one Claude interview against its live broker waiter.
    ReconcileStructured {
        /// Provider request fingerprint verified against current state.
        request_fingerprint: String,
    },
    /// Approve exact provider request.
    Approve {
        /// Provider request fingerprint verified against current state.
        request_fingerprint: String,
        /// Exact provider routing identity when provider protocol requires it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request_identity: Option<FleetRequestIdentity>,
    },
    /// Approve an exact provider request for the remainder of the current provider session.
    ApproveForSession {
        /// Provider request fingerprint verified against current state.
        request_fingerprint: String,
        /// Exact provider routing identity when provider protocol requires it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request_identity: Option<FleetRequestIdentity>,
    },
    /// Deny exact provider request.
    Deny {
        /// Provider request fingerprint verified against current state.
        request_fingerprint: String,
        /// Exact provider routing identity when provider protocol requires it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request_identity: Option<FleetRequestIdentity>,
    },
    /// Route one picker key only while exact request state remains current.
    VerifiedPicker {
        /// Provider request fingerprint verified against current state.
        request_fingerprint: String,
        /// Provider-neutral tmux key token from the constrained picker set.
        key: String,
    },
    /// Send generic prompt text.
    SendPrompt {
        /// Prompt text.
        text: String,
    },
    /// Continue current session.
    Continue,
    /// Retry failed turn.
    Retry,
    /// Interrupt active turn.
    Interrupt,
    /// Legacy wire form. New clients must use daemon-owned fleet/start.
    ///
    /// The daemon rejects this variant before selected-session validation, so
    /// it cannot create a session through fleet/action.
    Start {
        /// Provider to start.
        provider: FleetProvider,
        /// Working directory.
        cwd: String,
        /// Optional initial prompt.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prompt: Option<String>,
    },
    /// Restart session.
    Restart,
    /// Stop session gracefully.
    Stop,
    /// Kill session process.
    Kill,
    /// Archive session.
    Archive,
}

impl ControlAction {
    /// Stable token used by durable receipts.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::StructuredAnswer { .. } => "structured_answer",
            Self::DismissStructured { .. } => "dismiss_structured",
            Self::ReleaseStructured { .. } => "release_structured",
            Self::ReconcileStructured { .. } => "reconcile_structured",
            Self::Approve { .. } => "approve",
            Self::ApproveForSession { .. } => "approve_for_session",
            Self::Deny { .. } => "deny",
            Self::VerifiedPicker { .. } => "verified_picker",
            Self::SendPrompt { .. } => "send_prompt",
            Self::Continue => "continue",
            Self::Retry => "retry",
            Self::Interrupt => "interrupt",
            Self::Start { .. } => "start",
            Self::Restart => "restart",
            Self::Stop => "stop",
            Self::Kill => "kill",
            Self::Archive => "archive",
        }
    }
}

/// Parameters for `fleet/action`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FleetActionParams {
    /// Stable target session.
    pub session_key: String,
    /// Required optimistic concurrency version.
    pub expected_version: i64,
    /// Idempotent action request identifier.
    pub request_id: String,
    /// Typed action.
    pub action: ControlAction,
}

/// Durable action delivery status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ActionReceiptStatus {
    /// Action accepted but not resolved.
    Pending,
    /// Provider confirmed delivery.
    Delivered,
    /// Provider confirmed failure.
    Failed,
    /// Delivery outcome cannot be established.
    Unknown,
    /// Hangar rejected action before delivery.
    Rejected,
}

/// The ONE operator-facing token for a receipt status.
///
/// Lives beside the enum rather than in each surface because the daemon, the
/// TUI chat pane and the `ainb fleet msg` CLI all print this word, and three
/// private copies can drift independently: one saying `REFUSED` while another
/// says `REJECTED` is a vocabulary split no test that only reads its own
/// surface would catch. Wildcard-free, so a new variant is a compile error
/// here instead of a leg rendering as whichever arm was written last.
#[must_use]
pub const fn receipt_status_token(status: ActionReceiptStatus) -> &'static str {
    match status {
        ActionReceiptStatus::Pending => "PENDING",
        ActionReceiptStatus::Delivered => "DELIVERED",
        ActionReceiptStatus::Failed => "FAILED",
        ActionReceiptStatus::Unknown => "UNKNOWN",
        ActionReceiptStatus::Rejected => "REJECTED",
    }
}

/// Durable action result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FleetActionReceipt {
    /// Idempotent request identifier.
    pub request_id: String,
    /// Stable target session.
    pub session_key: String,
    /// Stable action token.
    pub action_kind: String,
    /// Exact action payload fingerprint.
    pub action_fingerprint: String,
    /// Session version required by action.
    pub expected_version: i64,
    /// Shared broadcast idempotency key, if any.
    pub idempotency_key: Option<String>,
    /// Honest delivery status.
    pub status: ActionReceiptStatus,
    /// Optional operator-facing detail.
    pub detail: Option<String>,
    /// Session version observed when delivery resolved.
    pub session_version: Option<i64>,
    /// Receipt creation time in epoch milliseconds.
    pub created_at: i64,
    /// Last receipt update time in epoch milliseconds.
    pub updated_at: i64,
}

/// Result for `fleet/action`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FleetActionResult {
    /// Durable action receipt.
    pub receipt: FleetActionReceipt,
}

/// Parameters for `fleet/reproject_claude_interview`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FleetReprojectClaudeInterviewParams {
    /// Exact managed Claude session to recover.
    pub session_key: String,
    /// Version observed before requesting recovery.
    pub expected_version: i64,
}

/// Result for `fleet/reproject_claude_interview`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FleetReprojectClaudeInterviewResult {
    /// Durable Fleet revision of the recovery event.
    pub revision: i64,
    /// Session version after recovery.
    pub session_version: i64,
    /// Whether recovery changed canonical state.
    pub applied: bool,
    /// Whether this request reused a prior recovery event.
    pub duplicate: bool,
}

/// Parameters for `fleet/receipt_list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FleetReceiptListParams {
    /// Maximum number of durable receipts, newest first.
    pub limit: u32,
}

/// Result for `fleet/receipt_list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FleetReceiptListResult {
    /// Durable receipts ordered by `updated_at DESC, request_id DESC`.
    pub receipts: Vec<FleetActionReceipt>,
}

/// Parameters for `fleet/receipt_get`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FleetReceiptGetParams {
    /// Exact idempotent action request identifier.
    pub request_id: String,
}

/// Result for `fleet/receipt_get`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FleetReceiptGetResult {
    /// Durable receipt when the daemon has one for this request id.
    pub receipt: Option<FleetActionReceipt>,
}

/// Parameters for `fleet/start`.
///
/// Start has no selected session and therefore carries no caller-supplied
/// session key or optimistic concurrency version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FleetStartParams {
    /// Idempotent new-session request identifier.
    pub request_id: String,
    /// Provider to start.
    pub provider: FleetProvider,
    /// Working directory for the new provider session.
    pub cwd: String,
    /// Optional initial prompt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
}

/// Result for `fleet/start`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FleetStartResult {
    /// Daemon-generated prospective session identity for this request.
    pub prospective_session_key: String,
    /// Durable start receipt.
    pub receipt: FleetActionReceipt,
}

/// Parameters for `codex/session_ensure`.
///
/// Interactive mode owns its tmux and session metadata. The daemon owns the
/// shared app-server. A new terminal creates its thread itself, then claims the
/// resulting exact identity through this same request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodexSessionEnsureParams {
    /// Stable Ainb Interactive session identity, used for validation and logs.
    pub session_id: String,
    /// Session working directory.
    pub cwd: String,
    /// Exact raw Codex model identifier, when selected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Existing remote thread to resume after an Interactive restart.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    /// Preserve Interactive's explicit yolo launch semantics at thread creation.
    #[serde(default)]
    pub skip_permissions: bool,
}

/// Result for `codex/session_ensure`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodexSessionEnsureResult {
    /// Exact Codex thread identity once the remote terminal has started it.
    /// `None` tells the caller to launch a fresh remote terminal without
    /// `resume`, then retry this request to claim its `thread/started` event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    /// Canonical Unix endpoint consumed by the Interactive tmux client.
    pub endpoint: String,
}

/// Parameters for `codex/session_discard`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodexSessionDiscardParams {
    /// Failed Interactive session identity whose reservation is discarded.
    pub session_id: String,
}

/// Result for `codex/session_discard`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodexSessionDiscardResult {
    /// Whether a daemon reservation was removed.
    pub discarded: bool,
    /// Whether its claimed remote thread was archived.
    pub archived: bool,
}

/// Parameters for `fleet/broadcast`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FleetBroadcastParams {
    /// Explicit stable recipients.
    pub target_keys: Vec<String>,
    /// Text delivered to each recipient.
    pub text: String,
    /// Idempotency boundary shared across recipient actions.
    pub idempotency_key: String,
}

/// Result for `fleet/broadcast`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FleetBroadcastResult {
    /// Per-session delivery receipts.
    pub receipts: Vec<FleetActionReceipt>,
}

/// Maximum rows one `fleet/message_list` page may return.
pub const FLEET_MESSAGE_LIST_MAX: u32 = 100;
/// Maximum recipients one `fleet/message_send` may name.
///
/// Every target costs a durable delivery row plus a verified transport submit
/// that can take seconds, all inside one request. Without a ceiling a single
/// call can hold the daemon for minutes and write an unbounded leg set; 64 is
/// far above any real fan-out (`fleet/broadcast` is the tool for more) and far
/// below a self-inflicted outage.
pub const FLEET_MESSAGE_TARGETS_MAX: usize = 64;
/// Maximum `fleet/message_send` body size, in bytes.
///
/// The body is persisted verbatim and re-submitted to every recipient, so an
/// unbounded one is an unbounded write amplified by the target count. 256 KiB
/// is far past any prompt a human or agent writes and short enough that the
/// worst case stays bounded.
pub const FLEET_MESSAGE_BODY_MAX: usize = 256 * 1024;
/// Maximum chunks one `fleet/transcript_list` page may return.
pub const FLEET_TRANSCRIPT_LIST_MAX: u32 = 100;

/// Chat message kind on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FleetMessageKind {
    /// Operator- or client-authored prompt.
    User,
    /// An agent session's final reply.
    Agent,
    /// Daemon-minted lifecycle marker.
    Marker,
}

/// One persisted chat message.
///
/// `id` is the stable external identity used for threading and cursors on the
/// wire; the daemon resolves every `after_id` to its commit-ordered `seq`
/// server-side, so `seq` itself never crosses the socket.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FleetMessage {
    /// Daemon-minted stable message identity.
    pub id: String,
    /// Minted scope string, for example `session:<key>` or `broadcast:<ulid>`.
    pub scope_key: String,
    /// Replies only: the message id this row answers (the thread join).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_message_id: Option<String>,
    /// `operator`, `copilot`, or a `session_key` — the daemon's record of who
    /// wrote it, taken from [`FleetMessageSendParams::actor`] and never from
    /// the body.
    pub sender: String,
    /// Message kind.
    pub kind: FleetMessageKind,
    /// Message body.
    pub body: String,
    /// Creation time in epoch milliseconds.
    pub created_at: i64,
}

/// Parameters for `fleet/acp_session_create`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FleetAcpSessionCreateParams {
    /// Adapter token validated against the daemon's adapter registry.
    pub provider: String,
    /// Working directory for the ACP session.
    pub cwd: String,
    /// Scope to bind; the daemon mints `session:<session_key>` when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope_key: Option<String>,
}

/// Result for `fleet/acp_session_create`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FleetAcpSessionCreateResult {
    /// Daemon-minted stable session identity (`acp:<ulid>`).
    pub session_key: String,
    /// Scope the session answers in.
    pub scope_key: String,
}

/// Parameters for `fleet/message_send`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FleetMessageSendParams {
    /// Explicit scope; the daemon derives the recipient's own scope for a
    /// direct send and mints `broadcast:<ulid>` for a multi-target send.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope_key: Option<String>,
    /// Who this message is FROM, persisted as [`FleetMessage::sender`] and
    /// replayed into the recipient's re-prime corpus. Absent means `operator`,
    /// which is what every human surface sends.
    ///
    /// Not a permission and not a claim the daemon trusts for authorisation —
    /// the socket token already authenticated the caller. It exists so a
    /// copilot-authored send is DISTINGUISHABLE from a human one at the two
    /// surfaces that matter (the receiving agent's corpus and the chat UIs);
    /// without it a copilot steered by a prompt injection asks another agent to
    /// act while wearing the operator's name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor: Option<String>,
    /// Recipient session keys; every target must already exist.
    pub targets: Vec<String>,
    /// Replies only: the message this send answers, the thread join read back
    /// by `fleet/message_list {origin_id}`.
    ///
    /// The daemon refuses an id it cannot find, and one whose message lives in
    /// a different scope than this send: a thread that joins across scopes is
    /// how a reply ends up in a conversation nobody addressed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_message_id: Option<String>,
    /// Message body.
    pub text: String,
    /// Client idempotency token; replay with different content is rejected.
    pub request_id: String,
}

/// Per-recipient delivery state, reusing the durable receipt vocabulary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FleetMessageDelivery {
    /// Recipient session.
    pub session_key: String,
    /// Honest delivery status for this leg.
    pub state: ActionReceiptStatus,
    /// Why this leg landed where it did (`target_unknown`, `target_not_running`,
    /// `queue_full`, …), when the daemon has a reason to give.
    ///
    /// The daemon has always computed this per leg and persisted it on the
    /// delivery row; it just never crossed the socket, so every surface could
    /// say REJECTED and none of them could say why. A fan-out where one of four
    /// recipients is refused is the case an operator actually has to read, and
    /// "REJECTED" alone does not tell them whether to retry or to go look at
    /// the session.
    ///
    /// Optional and skipped when absent: an older daemon simply omits it and an
    /// older client ignores it, so this is additive on both sides.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Result for `fleet/message_send`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FleetMessageSendResult {
    /// Persisted message identity.
    pub message_id: String,
    /// One entry per requested recipient.
    pub deliveries: Vec<FleetMessageDelivery>,
}

/// Parameters for `fleet/message_list`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FleetMessageListParams {
    /// Optional exact scope filter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope_key: Option<String>,
    /// Optional thread join: only replies to this message id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_id: Option<String>,
    /// Return rows strictly after this message id's commit position.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after_id: Option<String>,
    /// Requested row count, clamped to [`FLEET_MESSAGE_LIST_MAX`].
    pub limit: u32,
}

/// Result for `fleet/message_list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FleetMessageListResult {
    /// Messages in ascending commit order.
    pub messages: Vec<FleetMessage>,
    /// Cursor for the next page, or `null` when this page is empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_after_id: Option<String>,
}

/// Parameters for `fleet/message_subscribe`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FleetMessageSubscribeParams {
    /// Deliver messages strictly after this id; `null` starts at the head.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after_id: Option<String>,
}

/// Result for `fleet/message_subscribe`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FleetMessageSubscribeResult {
    /// Newest committed message id, or `null` on an empty log.
    pub head_id: Option<String>,
}

/// Payload of the `fleet/message_event` notification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FleetMessageEventParams {
    /// The committed message.
    pub message: FleetMessage,
}

/// One ACP transcript chunk (a `fleet_provider_event` row with `source='acp'`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FleetTranscriptChunk {
    /// Commit-ordered transcript cursor (`ingest_order`).
    pub ingest_order: i64,
    /// Replay-safe chunk identity.
    pub event_id: String,
    /// Owning Fleet session.
    pub session_key: String,
    /// Normalized discriminator, `acp.<kind>`.
    pub event_type: String,
    /// Normalized chunk body.
    pub payload: serde_json::Value,
    /// Observation time in epoch milliseconds.
    pub observed_at: i64,
}

/// Parameters for `fleet/transcript_list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FleetTranscriptListParams {
    /// Exact stable session identity.
    pub session_key: String,
    /// Return chunks strictly after this `ingest_order`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after_order: Option<i64>,
    /// Requested chunk count, clamped to [`FLEET_TRANSCRIPT_LIST_MAX`].
    pub limit: u32,
}

/// Result for `fleet/transcript_list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FleetTranscriptListResult {
    /// Chunks in ascending `ingest_order`.
    pub chunks: Vec<FleetTranscriptChunk>,
    /// Cursor for the next page, or `null` when this page is empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_after_order: Option<i64>,
}

/// Parameters for `fleet/transcript_subscribe`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FleetTranscriptSubscribeParams {
    /// Exact stable session identity.
    pub session_key: String,
    /// Deliver chunks strictly after this order; `null` starts at the head.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after_order: Option<i64>,
}

/// Result for `fleet/transcript_subscribe`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FleetTranscriptSubscribeResult {
    /// Newest committed `ingest_order` for the session, or `null` when empty.
    pub head_order: Option<i64>,
}

/// Maximum chunks one `fleet/transcript_prune` may export in a single call.
///
/// The export is materialised in the daemon's memory before it is written, so
/// an unbounded one is the same self-inflicted memory incident an unbounded
/// page would be. A prune that hits this ceiling reports it and deletes
/// nothing, so the operator narrows `--before` rather than losing rows they
/// never saw exported.
pub const FLEET_TRANSCRIPT_PRUNE_MAX: u32 = 50_000;

/// Parameters for `fleet/transcript_prune`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FleetTranscriptPruneParams {
    /// Exact stable session identity.
    pub session_key: String,
    /// Delete rows with `ingest_order` strictly below this watermark.
    pub before_order: i64,
    /// Where the daemon writes the JSONL export before deleting anything.
    ///
    /// `None` is only accepted with [`Self::no_export`] set: deleting a
    /// transcript with no export is a real operator choice, but never a
    /// default and never implicit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub export_path: Option<String>,
    /// Explicit acknowledgement that the rows are to be deleted unexported.
    #[serde(default)]
    pub no_export: bool,
}

/// Result for `fleet/transcript_prune`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FleetTranscriptPruneResult {
    /// Rows written to the export, `0` under `no_export`.
    pub exported: u32,
    /// Rows deleted.
    pub deleted: u32,
    /// Absolute path the export was written to, when one was.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub export_path: Option<String>,
}

/// Payload of the `fleet/transcript_event` notification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FleetTranscriptEventParams {
    /// The committed chunk.
    pub chunk: FleetTranscriptChunk,
}

/// The scope-key grammar, parsed.
///
/// Scopes are MINTED STRINGS, not a schema: `fleet_message.scope_key` is plain
/// text and this enum is the only place the vocabulary is written down. Part 1
/// minted `session:<key>` and `broadcast:<ulid>`; part 2 adds `channel:<id>`
/// with no schema change.
///
/// `fleet/message_send` validates a CALLER-SUPPLIED scope against the
/// recipients it claims to address, and fails closed on a prefix it does not
/// know, so a new scope kind has to be admitted here deliberately. The rule
/// per kind:
///
/// * [`Session`](Self::Session) — the named session must be the send's single
///   recipient, otherwise a caller files a message in someone else's timeline.
/// * [`Broadcast`](Self::Broadcast) — more than one recipient.
/// * [`Channel`](Self::Channel) — every recipient must be a MEMBER of that
///   channel. The membership lookup is the store's, so the check lives with
///   the handler; this type only says the prefix is legal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FleetScope<'a> {
    /// `session:<session_key>` — one session's own timeline.
    Session(&'a str),
    /// `broadcast:<ulid>` — one daemon-minted fan-out.
    Broadcast(&'a str),
    /// `channel:<id>` — a named channel with a recipient set (part 2).
    Channel(&'a str),
}

impl<'a> FleetScope<'a> {
    /// Parse a scope key, or `None` when the prefix is not in the grammar.
    ///
    /// `None` is the fail-closed answer: an unknown prefix is refused, never
    /// minted, so an unrecognised scope can never quietly become a timeline.
    #[must_use]
    pub fn parse(scope_key: &'a str) -> Option<Self> {
        let scope = scope_key.trim();
        let (kind, rest) = scope.split_once(':')?;
        if rest.is_empty() {
            return None;
        }
        match kind {
            "session" => Some(Self::Session(rest)),
            "broadcast" => Some(Self::Broadcast(rest)),
            "channel" => Some(Self::Channel(rest)),
            _ => None,
        }
    }
}

/// Maximum recipients one channel may carry.
///
/// A channel fan-out is ONE `fleet/message_send` with N delivery legs, so the
/// channel ceiling is the send ceiling: a channel that cannot be addressed in
/// a single send is a channel whose messages silently only reach a prefix of
/// its members.
pub const FLEET_CHANNEL_RECIPIENTS_MAX: usize = FLEET_MESSAGE_TARGETS_MAX;
/// Maximum channel name length, in bytes.
pub const FLEET_CHANNEL_NAME_MAX: usize = 128;
/// Maximum copilot persona length, in bytes.
///
/// The persona is a system prompt for an agent holding destructive tools, so
/// it is bounded like any other operator-supplied blob that is replayed into
/// every session start.
pub const FLEET_COPILOT_PERSONA_MAX: usize = 8 * 1024;
/// Maximum rows one `fleet/activity_list` page may return.
pub const FLEET_ACTIVITY_LIST_MAX: u32 = 200;

/// What a chat channel is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FleetChannelKind {
    /// The standing copilot channel: an ACP session whose scope IS the channel.
    Copilot,
    /// A named fan-out channel over an explicit recipient set.
    Broadcast,
}

/// One chat channel and the scope it mints.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FleetChannel {
    /// Daemon-minted stable channel identity.
    pub id: String,
    /// Channel kind.
    pub kind: FleetChannelKind,
    /// Human-readable channel name.
    pub name: String,
    /// The minted scope, always `channel:<id>`.
    pub scope_key: String,
    /// Member session keys; empty for a copilot channel.
    pub recipients: Vec<String>,
    /// Creation time in epoch milliseconds.
    pub created_at: i64,
}

/// Parameters for `fleet/channel_create`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FleetChannelCreateParams {
    /// Channel kind.
    pub kind: FleetChannelKind,
    /// Human-readable name, at most [`FLEET_CHANNEL_NAME_MAX`] bytes.
    pub name: String,
    /// Member session keys, at most [`FLEET_CHANNEL_RECIPIENTS_MAX`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recipients: Option<Vec<String>>,
}

/// Result for `fleet/channel_create`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FleetChannelCreateResult {
    /// The persisted channel, carrying its minted `channel:<id>` scope.
    pub channel: FleetChannel,
}

/// Parameters for `fleet/channel_list`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FleetChannelListParams {}

/// Result for `fleet/channel_list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FleetChannelListResult {
    /// Channels in creation order.
    pub channels: Vec<FleetChannel>,
}

/// Adapter family backing the copilot session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FleetCopilotProvider {
    /// `claude-agent-acp`.
    Claude,
    /// `codex-acp`.
    Codex,
}

/// Parameters for `fleet/copilot_configure`.
///
/// There is deliberately NO permission-mode field. Part 1 pins the mode at
/// `session/new` and re-asserts it after load precisely because an ambient
/// `bypassPermissions` disables the whole permission surface; a settable mode
/// here would be a remote off-switch for the guardrails, reachable by anyone
/// holding [`FLEET_CAPABILITY_COPILOT_CONFIGURE`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FleetCopilotConfigureParams {
    /// Adapter family.
    pub provider: FleetCopilotProvider,
    /// Adapter model id; `None` leaves the daemon's static config in place.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Adapter reasoning effort token; `None` leaves the static config alone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    /// System prompt, at most [`FLEET_COPILOT_PERSONA_MAX`] bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persona: Option<String>,
}

/// Result for `fleet/copilot_configure`.
///
/// The persona is NOT echoed: it is a privileged blob, and a read-back is a
/// second place it can leak from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FleetCopilotConfigureResult {
    /// The copilot session the config was written to.
    pub session_key: String,
    /// Adapter family now in force.
    pub provider: FleetCopilotProvider,
    /// Model override now in force, when one is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Reasoning-effort override now in force, when one is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    /// Whether a persona override is stored.
    pub persona_set: bool,
}

/// Lifecycle of one guardrail confirm card.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FleetConfirmState {
    /// Awaiting an operator; the copilot's tool result is suspended.
    Open,
    /// Answered approve (possibly with edited arguments).
    Approved,
    /// Answered deny.
    Denied,
    /// Reached `expires_at` unanswered; resolved to the tool as denied.
    Expired,
}

/// One guardrail confirm card: a copilot tool call held for an operator.
///
/// NOT an ACP permission request. Those stay part 1's attention rows answered
/// through `fleet/action` with fingerprint staleness.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FleetConfirm {
    /// Daemon-minted stable card identity.
    pub confirm_id: String,
    /// Scope the card belongs to, normally the copilot `channel:<id>`.
    pub scope_key: String,
    /// The MCP tool the copilot asked to run.
    pub tool: String,
    /// The tool arguments, PROJECTED to the keys the tool's schema declares.
    ///
    /// The classifier ignores unknown argument keys by contract, which protects
    /// the machine verdict and leaves the human's unprotected: this value is
    /// rendered on the operator's confirm card, so a model-authored
    /// `justification` / `reason` / `operator_approved` key riding along would
    /// be arguing its own case to the person approving a destructive action.
    ///
    /// The daemon's obligation, enforced where the card is minted: project
    /// through `ainb_fleet_tools::server::project_arguments` (a filter over the
    /// tool table's declared `properties`) BEFORE persisting, so an undeclared
    /// key never reaches a card at all.
    pub arguments: serde_json::Value,
    /// The session the tool would act on, when it names one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_session_key: Option<String>,
    /// Card lifecycle.
    pub state: FleetConfirmState,
    /// Creation time in epoch milliseconds.
    pub created_at: i64,
    /// Server-side expiry in epoch milliseconds; strictly shorter than part
    /// 1's per-turn deadline so the deadline never converges a turn out from
    /// under a pending card.
    pub expires_at: i64,
}

/// Parameters for `fleet/confirm_list`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FleetConfirmListParams {
    /// Optional exact scope filter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope_key: Option<String>,
}

/// Result for `fleet/confirm_list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FleetConfirmListResult {
    /// Open cards, oldest first.
    pub confirms: Vec<FleetConfirm>,
}

/// The answer to a confirm card, internally tagged like [`ControlAction`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "answer", rename_all = "snake_case")]
pub enum FleetConfirmAnswer {
    /// Run the tool with the arguments as proposed.
    Approve,
    /// Refuse; the suspended tool result resolves as denied.
    Deny,
    /// Run the tool with operator-edited arguments.
    Edit {
        /// The arguments to run INSTEAD of the proposed ones.
        arguments: serde_json::Value,
    },
}

/// Parameters for `fleet/confirm_answer`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FleetConfirmAnswerParams {
    /// The card being answered.
    pub confirm_id: String,
    /// Approve, deny, or approve with edited arguments.
    #[serde(flatten)]
    pub answer: FleetConfirmAnswer,
}

/// Result for `fleet/confirm_answer`.
///
/// A card is SINGLE-USE: answering an already-answered or already-expired
/// `confirm_id` is a typed error, never a second execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FleetConfirmAnswerResult {
    /// The card that was answered.
    pub confirm_id: String,
    /// Its terminal state.
    pub state: FleetConfirmState,
}

/// Payload of the `fleet/confirm_event` notification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FleetConfirmEventParams {
    /// The card at its new state (opened, answered, or expired).
    pub confirm: FleetConfirm,
}

/// Confirm-card lifetime, in milliseconds.
///
/// Here rather than in the daemon because BOTH ends of `fleet/copilot_gate`
/// have to agree on it: the daemon expires the card at this age, and the tool
/// server's client bound has to sit outside it, or a live card would come back
/// to the copilot as a transport timeout and be retried into a second card.
///
/// Strictly shorter than part 1's 30-minute per-turn deadline, which is its
/// whole justification: the card holds the copilot's ACP turn open, so a card
/// that outlived the deadline would have the deadline converge the turn out
/// from under a dialog the operator is still looking at.
pub const FLEET_CONFIRM_TTL_MS: u64 = 10 * 60 * 1000;

/// Parameters for `fleet/copilot_gate`: one tool call offered to the guardrail.
///
/// Deliberately carries NO scope, NO named-session set and NO class hint. The
/// caller is the copilot's MCP tool server, which is downstream of every
/// transcript the copilot has read, so anything it could put on this wire is
/// model-reachable. The daemon resolves the scope from its own copilot channel
/// and pins the turn state itself; all the caller gets to say is which tool the
/// model asked for and with what arguments.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FleetCopilotGateParams {
    /// The MCP tool name the model invoked.
    pub tool: String,
    /// The arguments the model supplied, verbatim and unprojected.
    ///
    /// Unprojected because projection is the DAEMON's obligation: it happens
    /// once, immediately before a card is persisted, so there is exactly one
    /// place that has to be right.
    #[serde(default)]
    pub arguments: serde_json::Map<String, serde_json::Value>,
}

/// What the guardrail decided about one tool call.
///
/// The three non-`run` variants are all "do not execute", kept distinct because
/// the copilot should be able to tell "a human said no" from "nobody looked"
/// from "that call was never executable", and an operator reading the activity
/// feed should too.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FleetGateVerdict {
    /// Execute the tool with the returned arguments.
    Run,
    /// A human denied the confirm card.
    Denied,
    /// The confirm card reached its expiry unanswered.
    Expired,
    /// The call is not executable at all (unknown tool, malformed arguments).
    Refused,
}

/// Result for `fleet/copilot_gate`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FleetCopilotGateResult {
    /// The verdict.
    pub verdict: FleetGateVerdict,
    /// The arguments to execute with, only meaningful for
    /// [`FleetGateVerdict::Run`].
    ///
    /// NOT an echo of the request: for a card answered `edit` these are the
    /// OPERATOR's arguments, so the caller must execute THESE and never the
    /// ones it sent.
    #[serde(default)]
    pub arguments: serde_json::Map<String, serde_json::Value>,
    /// Why, for a refusal. Never model-authored prose: it is the classifier's
    /// own token plus its detail.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Guardrail class of one copilot tool invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FleetActivityClass {
    /// Reads fleet state; runs automatically.
    Read,
    /// Writes to a session; runs automatically and always logs a row.
    Write,
    /// Interrupt / kill / archive; always confirmed.
    Destructive,
}

/// How one copilot tool invocation ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FleetActivityOutcome {
    /// The tool ran.
    Ok,
    /// An operator denied the confirm card.
    Denied,
    /// The confirm card expired unanswered.
    Expired,
    /// The tool ran and failed.
    Error,
}

/// One append-only copilot activity row.
///
/// `seq` is the commit-ordered cursor SQLite assigns inside the write
/// transaction, exactly as `fleet_message.seq` is; `id` is stable external
/// identity and is never an ordering key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FleetActivityRow {
    /// Commit-ordered cursor.
    pub seq: i64,
    /// Daemon-minted stable row identity.
    pub id: String,
    /// Scope the action was taken in.
    pub scope_key: String,
    /// The MCP tool invoked.
    pub tool: String,
    /// Guardrail class the classifier assigned.
    pub class: FleetActivityClass,
    /// The session acted on, when the tool named one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_session_key: Option<String>,
    /// Outcome.
    pub outcome: FleetActivityOutcome,
    /// Short human detail; never the model's justification text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Creation time in epoch milliseconds.
    pub created_at: i64,
}

/// Parameters for `fleet/activity_list`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FleetActivityListParams {
    /// Optional exact scope filter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope_key: Option<String>,
    /// Return rows strictly after this commit-ordered `seq`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after_seq: Option<i64>,
    /// Requested row count, clamped to [`FLEET_ACTIVITY_LIST_MAX`].
    pub limit: u32,
}

/// Result for `fleet/activity_list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FleetActivityListResult {
    /// Rows in ascending commit order.
    pub activities: Vec<FleetActivityRow>,
    /// Cursor for the next page, or `null` when this page is empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_after_seq: Option<i64>,
}

/// Payload of the `fleet/activity_event` notification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FleetActivityEventParams {
    /// The committed row.
    pub activity: FleetActivityRow,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A session with no observed model, in the shape every roster row starts
    /// in.
    fn session_without_model() -> FleetSession {
        FleetSession {
            session_key: "claude:s-1".to_string(),
            provider: FleetProvider::Claude,
            provider_session_id: Some("s-1".to_string()),
            tmux_target: None,
            process_start_fingerprint: None,
            cwd: "/repo".to_string(),
            display_name: None,
            lifecycle: LifecycleState::Running,
            active_work_count: 0,
            attention: AttentionState::None,
            current_request_fingerprint: None,
            current_request: None,
            management: ManagementState::Managed,
            transport_health: TransportHealth::Healthy,
            capabilities: FleetCapabilities::default(),
            provenance: FleetProvenance::Authoritative,
            confidence: FleetConfidence::High,
            discovered_at: 1,
            last_observed_at: 2,
            lifecycle_updated_at: 2,
            attention_updated_at: 1,
            model: None,
            reasoning_effort: None,
            model_updated_at: 0,
            version: 1,
            updated_revision: 3,
        }
    }

    /// Absence renders as ABSENCE. An unobserved model must omit its keys, not
    /// emit `null`: an explicit null is a value a decoder can round-trip back
    /// into the object, which would break the Swift decode-then-re-encode
    /// equality gate the moment a full-session sample joins the canonical
    /// fixtures.
    #[test]
    fn fleet_session_omits_absent_model_keys() {
        let session = session_without_model();
        let encoded = serde_json::to_value(&session).unwrap();
        let object = encoded.as_object().expect("a session encodes as an object");
        assert!(
            !object.contains_key("model"),
            "an unobserved model must be absent, not null: {encoded}"
        );
        assert!(
            !object.contains_key("reasoning_effort"),
            "an unobserved effort must be absent, not null: {encoded}"
        );
        assert_eq!(
            object.get("model_updated_at"),
            Some(&serde_json::json!(0)),
            "the group clock is always present; 0 means never observed"
        );

        let decoded: FleetSession = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded, session, "the absent shape must round-trip");
    }

    /// The observed shape carries both keys verbatim and round-trips.
    #[test]
    fn fleet_session_carries_observed_model_keys() {
        let session = FleetSession {
            model: Some("claude-opus-5".to_string()),
            reasoning_effort: Some("high".to_string()),
            model_updated_at: 1_700,
            ..session_without_model()
        };
        let encoded = serde_json::to_value(&session).unwrap();
        assert_eq!(encoded["model"], "claude-opus-5");
        assert_eq!(encoded["reasoning_effort"], "high");
        assert_eq!(encoded["model_updated_at"], 1_700);

        let decoded: FleetSession = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded, session);
    }

    /// A snapshot minted before 0084 has none of the three keys. It must still
    /// decode, to the never-observed shape.
    #[test]
    fn fleet_session_decodes_a_payload_without_model_keys() {
        let mut encoded = serde_json::to_value(session_without_model()).unwrap();
        encoded
            .as_object_mut()
            .expect("object")
            .remove("model_updated_at")
            .expect("the clock is present before removal");

        let decoded: FleetSession = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded.model, None);
        assert_eq!(decoded.reasoning_effort, None);
        assert_eq!(decoded.model_updated_at, 0);
    }

    #[test]
    fn lifecycle_and_attention_serialize_independently() {
        let value = serde_json::json!({
            "lifecycle": LifecycleState::Running,
            "attention": AttentionState::Ask,
        });
        assert_eq!(value["lifecycle"], "RUNNING");
        assert_eq!(value["attention"], "ASK");
    }

    #[test]
    fn structured_answer_preserves_exact_request_identity() {
        let action = ControlAction::StructuredAnswer {
            request_fingerprint: "sha256:request".to_string(),
            request_identity: Some(FleetRequestIdentity {
                request_id: serde_json::json!(41),
                thread_id: "thread-1".to_string(),
                turn_id: "turn-2".to_string(),
                item_id: "item-3".to_string(),
            }),
            answers: vec![FleetQuestionAnswer {
                question_id: "q-1".to_string(),
                selected_options: vec!["first".to_string(), "third".to_string()],
                text: None,
            }],
        };
        let encoded = serde_json::to_value(&action).unwrap();
        assert_eq!(encoded["action"], "structured_answer");
        assert_eq!(encoded["request_fingerprint"], "sha256:request");
        assert_eq!(encoded["request_identity"]["request_id"], 41);
        assert_eq!(encoded["answers"][0]["question_id"], "q-1");
        assert_eq!(action.kind(), "structured_answer");
    }

    #[test]
    fn structured_dismiss_preserves_exact_request_identity() {
        let action = ControlAction::DismissStructured {
            request_fingerprint: "sha256:request".to_string(),
            request_identity: Some(FleetRequestIdentity {
                request_id: serde_json::json!(41),
                thread_id: "thread-1".to_string(),
                turn_id: "turn-2".to_string(),
                item_id: "item-3".to_string(),
            }),
        };
        let encoded = serde_json::to_value(&action).unwrap();
        assert_eq!(action.kind(), "dismiss_structured");
        assert_eq!(encoded["action"], "dismiss_structured");
        assert_eq!(encoded["request_fingerprint"], "sha256:request");
        assert_eq!(encoded["request_identity"]["request_id"], 41);
    }

    #[test]
    fn verified_picker_is_typed_and_request_scoped() {
        let action = ControlAction::VerifiedPicker {
            request_fingerprint: "sha256:request".to_string(),
            key: "1".to_string(),
        };
        let encoded = serde_json::to_value(&action).unwrap();
        assert_eq!(action.kind(), "verified_picker");
        assert_eq!(encoded["action"], "verified_picker");
        assert_eq!(encoded["request_fingerprint"], "sha256:request");
        assert_eq!(encoded["key"], "1");
    }

    #[test]
    fn capabilities_default_to_disabled() {
        let capabilities: FleetCapabilities = serde_json::from_str("{}").unwrap();
        assert!(!capabilities.structured_answer);
        assert!(!capabilities.kill);
        assert!(!capabilities.tmux_text);
    }

    #[test]
    fn acp_provider_uses_snake_case_wire_token() {
        assert_eq!(serde_json::json!(FleetProvider::Acp), "acp");
        let decoded: FleetProvider = serde_json::from_str("\"acp\"").unwrap();
        assert_eq!(decoded, FleetProvider::Acp);
    }

    #[test]
    fn antigravity_provider_uses_snake_case_wire_token() {
        assert_eq!(serde_json::json!(FleetProvider::Antigravity), "antigravity");
        let decoded: FleetProvider = serde_json::from_str("\"antigravity\"").unwrap();
        assert_eq!(decoded, FleetProvider::Antigravity);
    }

    #[test]
    fn v2_capability_consts_are_advertised_exactly_with_their_dispatch_arms() {
        // Advertisement lands with each capability's dispatch arms (message /
        // transcript in Phase 3, acp.spawn in Phase 5); a daemon built between
        // phases must never advertise methods that answer -32601.
        for id in [
            FLEET_CAPABILITY_MESSAGE_SEND,
            FLEET_CAPABILITY_MESSAGE_READ,
            FLEET_CAPABILITY_TRANSCRIPT_READ,
            // Phase 5 landed `fleet/acp_session_create`'s dispatch arm, so its
            // capability is advertised in the SAME change. Before that arm
            // existed this assertion ran the other way round, which is the
            // point: the catalogue never advertises a -32601 method.
            FLEET_CAPABILITY_ACP_SPAWN,
            FLEET_CAPABILITY_USAGE_READ,
            FLEET_CAPABILITY_QUOTA_READ,
            FLEET_CAPABILITY_RUNTIME_READ,
            FLEET_CAPABILITY_DASHBOARD_READ,
        ] {
            assert!(
                FLEET_PROTOCOL_CAPABILITY_IDS.contains(&id),
                "{id:?} has dispatch arms but is not advertised"
            );
        }
        // Part 2 phase A2 landed all six of its dispatch arms, so all four of
        // its capabilities moved into the catalogue in that same change. The
        // rule is unchanged and still runs both ways: a capability is
        // advertised WITH its handler, never before it.
        for id in [
            FLEET_CAPABILITY_CHAT_WRITE,
            FLEET_CAPABILITY_CHAT_READ,
            FLEET_CAPABILITY_COPILOT_CONFIGURE,
            FLEET_CAPABILITY_CONFIRM_ANSWER,
            // The producer arm, landed with the tool server's live gate. Same
            // rule: advertised WITH its handler, never before it.
            FLEET_CAPABILITY_COPILOT_GATE,
        ] {
            assert!(
                FLEET_PROTOCOL_CAPABILITY_IDS.contains(&id),
                "{id:?} has dispatch arms but is not advertised"
            );
        }
    }

    /// The scope grammar, including the channel prefix part 2 mints.
    ///
    /// `fleet/message_send` fails CLOSED on a prefix it cannot parse, so this
    /// is the deliberate admission part 1's refusal predicted: a channel scope
    /// is legal grammar, and the recipients-are-members check belongs to the
    /// handler that can read the membership.
    #[test]
    fn scope_grammar_parses_channel_and_refuses_the_unknown() {
        assert_eq!(
            FleetScope::parse("session:acp:01J0KEY"),
            Some(FleetScope::Session("acp:01J0KEY"))
        );
        assert_eq!(
            FleetScope::parse("broadcast:01J0BCAST"),
            Some(FleetScope::Broadcast("01J0BCAST"))
        );
        assert_eq!(
            FleetScope::parse("channel:copilot"),
            Some(FleetScope::Channel("copilot"))
        );
        assert_eq!(
            FleetScope::parse("  channel:01J0CH  "),
            Some(FleetScope::Channel("01J0CH"))
        );
        // Fail closed: an unknown prefix, a bare word, an empty tail.
        assert_eq!(FleetScope::parse("thread:01J0"), None);
        assert_eq!(FleetScope::parse("copilot"), None);
        assert_eq!(FleetScope::parse("channel:"), None);
        assert_eq!(FleetScope::parse(""), None);
    }

    fn round_trip<T>(value: &T)
    where
        T: Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
    {
        let encoded = serde_json::to_value(value).unwrap();
        let decoded: T = serde_json::from_value(encoded).unwrap();
        assert_eq!(&decoded, value);
    }

    fn sample_message() -> FleetMessage {
        FleetMessage {
            id: "01J0MSG".to_string(),
            scope_key: "session:acp:01J0KEY".to_string(),
            origin_message_id: Some("01J0ORIGIN".to_string()),
            sender: "operator".to_string(),
            kind: FleetMessageKind::User,
            body: "hello".to_string(),
            created_at: 1_700_000_000_000,
        }
    }

    fn sample_chunk() -> FleetTranscriptChunk {
        FleetTranscriptChunk {
            ingest_order: 41,
            event_id: "01J0CHUNK".to_string(),
            session_key: "acp:01J0KEY".to_string(),
            event_type: "acp.message".to_string(),
            payload: serde_json::json!({ "text": "thinking" }),
            observed_at: 1_700_000_000_100,
        }
    }

    #[test]
    fn message_family_params_and_results_round_trip() {
        round_trip(&FleetAcpSessionCreateParams {
            provider: "claude-agent-acp".to_string(),
            cwd: "/repo".to_string(),
            scope_key: None,
        });
        round_trip(&FleetAcpSessionCreateResult {
            session_key: "acp:01J0KEY".to_string(),
            scope_key: "session:acp:01J0KEY".to_string(),
        });
        round_trip(&FleetMessageSendParams {
            scope_key: None,
            actor: None,
            targets: vec!["acp:01J0KEY".to_string()],
            origin_message_id: None,
            text: "hello".to_string(),
            request_id: "req-1".to_string(),
        });
        round_trip(&FleetMessageSendParams {
            scope_key: None,
            actor: Some("copilot".to_string()),
            targets: vec!["acp:01J0KEY".to_string()],
            origin_message_id: Some("01J0ORIGIN".to_string()),
            text: "hello".to_string(),
            request_id: "req-1".to_string(),
        });
        // An older client omits the key entirely; that must still decode, and
        // it must mean the operator rather than failing the frame.
        let legacy: FleetMessageSendParams = serde_json::from_value(serde_json::json!({
            "targets": ["acp:01J0KEY"],
            "text": "hello",
            "request_id": "req-1"
        }))
        .expect("a pre-actor frame still decodes");
        assert!(legacy.actor.is_none());
        assert!(
            legacy.origin_message_id.is_none(),
            "an unthreaded send is the default, not a decode failure"
        );
        round_trip(&FleetMessageSendResult {
            message_id: "01J0MSG".to_string(),
            deliveries: vec![FleetMessageDelivery {
                session_key: "acp:01J0KEY".to_string(),
                state: ActionReceiptStatus::Pending,
                detail: None,
            }],
        });
        round_trip(&FleetMessageListParams {
            scope_key: Some("session:acp:01J0KEY".to_string()),
            origin_id: None,
            after_id: Some("01J0MSG".to_string()),
            limit: FLEET_MESSAGE_LIST_MAX,
        });
        round_trip(&FleetMessageListResult {
            messages: vec![sample_message()],
            next_after_id: Some("01J0MSG".to_string()),
        });
        round_trip(&FleetMessageSubscribeParams { after_id: None });
        round_trip(&FleetMessageSubscribeResult {
            head_id: Some("01J0MSG".to_string()),
        });
        round_trip(&FleetMessageEventParams {
            message: sample_message(),
        });
    }

    #[test]
    fn transcript_family_params_and_results_round_trip() {
        round_trip(&FleetTranscriptListParams {
            session_key: "acp:01J0KEY".to_string(),
            after_order: Some(7),
            limit: FLEET_TRANSCRIPT_LIST_MAX,
        });
        round_trip(&FleetTranscriptListResult {
            chunks: vec![sample_chunk()],
            next_after_order: Some(41),
        });
        round_trip(&FleetTranscriptSubscribeParams {
            session_key: "acp:01J0KEY".to_string(),
            after_order: None,
        });
        round_trip(&FleetTranscriptSubscribeResult { head_order: None });
        round_trip(&FleetTranscriptEventParams {
            chunk: sample_chunk(),
        });
    }

    #[test]
    fn usage_summary_wire_contract_round_trips_with_optional_costs() {
        assert_eq!(
            serde_json::to_value(FleetUsagePeriod::Trailing7Days).unwrap(),
            "trailing_7_days"
        );
        assert_eq!(
            serde_json::to_value(FleetUsageSummaryState::Partial).unwrap(),
            "partial"
        );
        round_trip(&FleetUsageSummaryParams {
            period: FleetUsagePeriod::Trailing30Days,
        });
        round_trip(&FleetUsageSummaryResult {
            state: FleetUsageSummaryState::Partial,
            generated_at: Some(1_700_000_000_000),
            start_at: Some(1_699_395_200_000),
            end_at: Some(1_700_000_000_000),
            totals: Some(FleetUsageBucket {
                input_tokens: 100,
                cache_creation_tokens: 20,
                cache_read_tokens: 30,
                output_tokens: 40,
                reasoning_tokens: 50,
                call_count: 2,
                session_count: 1,
                project_count: 1,
                cost_usd: None,
            }),
            daily: vec![FleetUsageDailyBucket {
                date: "2026-08-06".to_string(),
                bucket: FleetUsageBucket {
                    input_tokens: 100,
                    cache_creation_tokens: 20,
                    cache_read_tokens: 30,
                    output_tokens: 40,
                    reasoning_tokens: 50,
                    call_count: 2,
                    session_count: 1,
                    project_count: 1,
                    cost_usd: Some(1.25),
                },
            }],
            providers: vec![FleetUsageProviderBucket {
                provider: "claude".to_string(),
                bucket: FleetUsageBucket::default(),
            }],
            models: vec![FleetUsageModelBucket {
                model: "claude-sonnet-4-5".to_string(),
                bucket: FleetUsageBucket::default(),
            }],
            projects: vec![FleetUsageProjectBucket {
                project: "owner/repo".to_string(),
                repo: Some("owner/repo".to_string()),
                bucket: FleetUsageBucket::default(),
            }],
            detail: Some("copilot shutdown metrics unavailable".to_string()),
        });
        round_trip(&FleetQuotaSummaryResult {
            state: FleetUsageSummaryState::Partial,
            generated_at: Some(1_700_000_000_000),
            providers: vec![FleetQuotaProvider {
                provider: "claude".to_string(),
                five_hour: Some(FleetQuotaWindow {
                    used_percent: 42,
                    resets_at: Some(1_700_018_000_000),
                    estimated: false,
                }),
                seven_day: None,
                plan_type: None,
                updated_at: Some(1_700_000_000_000),
            }],
            detail: Some("Codex quota unavailable".to_string()),
        });
        round_trip(&FleetQuotaSummaryParams {});
    }

    #[test]
    fn message_wire_fields_use_stable_snake_case_names() {
        let encoded = serde_json::to_value(sample_message()).unwrap();
        assert_eq!(encoded["kind"], "user");
        assert_eq!(encoded["origin_message_id"], "01J0ORIGIN");
        let delivery = serde_json::to_value(FleetMessageDelivery {
            session_key: "acp:01J0KEY".to_string(),
            state: ActionReceiptStatus::Rejected,
            detail: Some("target_not_running".to_string()),
        })
        .unwrap();
        // Delivery states reuse the durable receipt vocabulary verbatim.
        assert_eq!(delivery["state"], "REJECTED");
        // And the REASON rides with the state: a rejected leg with no reason is
        // a receipt an operator cannot act on.
        assert_eq!(delivery["detail"], "target_not_running");
        // A leg with nothing to explain omits the key rather than sending null,
        // so a pre-detail client sees exactly the frame it always saw.
        let quiet = serde_json::to_value(FleetMessageDelivery {
            session_key: "acp:01J0KEY".to_string(),
            state: ActionReceiptStatus::Delivered,
            detail: None,
        })
        .unwrap();
        assert!(quiet.get("detail").is_none(), "{quiet}");
    }

    #[test]
    fn usage_dashboard_wire_contract_round_trips() {
        round_trip(&FleetUsageDashboardParams {});
        round_trip(&FleetUsageDashboardResult {
            state: FleetUsageSummaryState::Ready,
            generated_at: Some(1_700_000_000_000),
            start_at: Some(1_668_000_000_000),
            end_at: Some(1_700_000_000_000),
            cost_complete: false,
            totals: Some(FleetUsageBucket {
                input_tokens: 500_000,
                cache_creation_tokens: 10_000,
                cache_read_tokens: 20_000,
                output_tokens: 100_000,
                reasoning_tokens: 5_000,
                call_count: 42,
                session_count: 7,
                project_count: 3,
                cost_usd: Some(12.50),
            }),
            weekly: vec![FleetUsageWeeklyBucket {
                week_start: "2026-07-28".to_string(),
                bucket: FleetUsageBucket::default(),
            }],
            heatmap: vec![FleetHeatmapCell {
                date: "2026-08-06".to_string(),
                call_count: 15,
                cost_usd: Some(2.30),
            }],
            forecast: Some(FleetUsageForecast {
                projected_30d_cost_usd: Some(125.00),
                projected_30d_tokens: 5_000_000,
                avg_daily_cost_usd: Some(4.17),
                avg_daily_tokens: 166_667,
                sample_days: 7,
            }),
            providers: vec![],
            models: vec![],
            projects: vec![],
            sessions: vec![FleetUsageSessionBucket {
                session_id: "claude:myproject:sess-1".to_string(),
                provider: "claude".to_string(),
                project: "myproject".to_string(),
                bucket: FleetUsageBucket::default(),
            }],
            branches: vec![FleetUsageBranchBucket {
                branch: "feat/my-feature".to_string(),
                bucket: FleetUsageBucket::default(),
            }],
            tools: vec![FleetUsageNamedBucket {
                name: "Edit".to_string(),
                call_count: 100,
            }],
            mcp_servers: vec![],
            shell_commands: vec![FleetUsageNamedBucket {
                name: "cargo test".to_string(),
                call_count: 30,
            }],
            detail: None,
        });
    }
}
