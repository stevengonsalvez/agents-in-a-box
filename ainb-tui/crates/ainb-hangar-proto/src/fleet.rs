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

/// Fleet capability identifiers advertised during protocol negotiation.
///
/// The chat-bus capability consts above are part of the frozen v2 surface but
/// are appended here only in the phase their dispatch arms land, so no daemon
/// build ever advertises a capability whose methods answer -32601.
pub const FLEET_PROTOCOL_CAPABILITY_IDS: &[&str] = &[
    FLEET_CAPABILITY_ACP_SPAWN,
    FLEET_CAPABILITY_ACTION_EXECUTE,
    FLEET_CAPABILITY_ATC_READ,
    FLEET_CAPABILITY_BROADCAST_EXECUTE,
    FLEET_CAPABILITY_MESSAGE_READ,
    FLEET_CAPABILITY_MESSAGE_SEND,
    "fleet.protocol.negotiate",
    FLEET_CAPABILITY_RECEIPT_READ,
    "fleet.snapshot.read",
    FLEET_CAPABILITY_START_EXECUTE,
    "fleet.subscription.live",
    "fleet.subscription.replay",
    "fleet.subscription.resync",
    FLEET_CAPABILITY_TIMELINE_READ,
    FLEET_CAPABILITY_TRANSCRIPT_READ,
    FLEET_CAPABILITY_RUNTIME_READ,
    FLEET_CAPABILITY_USAGE_READ,
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

/// Bounded runtime health returned only by the public Fleet RPC.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FleetRuntimeStatusResult {
    /// Daemon version serving this connection.
    pub daemon_version: String,
    /// Fleet protocol version serving this connection.
    pub protocol_version: u32,
    /// Supported provider hook states. No filesystem paths are exposed.
    pub hooks: Vec<FleetRuntimeHookStatus>,
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
    /// `operator` or a `session_key`.
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
    /// Recipient session keys; every target must already exist.
    pub targets: Vec<String>,
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

/// Payload of the `fleet/transcript_event` notification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FleetTranscriptEventParams {
    /// The committed chunk.
    pub chunk: FleetTranscriptChunk,
}

#[cfg(test)]
mod tests {
    use super::*;

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
            FLEET_CAPABILITY_RUNTIME_READ,
        ] {
            assert!(
                FLEET_PROTOCOL_CAPABILITY_IDS.contains(&id),
                "{id:?} has dispatch arms but is not advertised"
            );
        }
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
            targets: vec!["acp:01J0KEY".to_string()],
            text: "hello".to_string(),
            request_id: "req-1".to_string(),
        });
        round_trip(&FleetMessageSendResult {
            message_id: "01J0MSG".to_string(),
            deliveries: vec![FleetMessageDelivery {
                session_key: "acp:01J0KEY".to_string(),
                state: ActionReceiptStatus::Pending,
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
    }

    #[test]
    fn message_wire_fields_use_stable_snake_case_names() {
        let encoded = serde_json::to_value(sample_message()).unwrap();
        assert_eq!(encoded["kind"], "user");
        assert_eq!(encoded["origin_message_id"], "01J0ORIGIN");
        let delivery = serde_json::to_value(FleetMessageDelivery {
            session_key: "acp:01J0KEY".to_string(),
            state: ActionReceiptStatus::Rejected,
        })
        .unwrap();
        // Delivery states reuse the durable receipt vocabulary verbatim.
        assert_eq!(delivery["state"], "REJECTED");
    }
}
