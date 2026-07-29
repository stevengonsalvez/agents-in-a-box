//! Fleet control-plane wire types.
//!
//! Sessions use stable provider identity, independent lifecycle and attention
//! state, optimistic concurrency versions, and durable global revisions.

use serde::{Deserialize, Serialize};

/// Current Fleet wire protocol version.
pub const FLEET_PROTOCOL_VERSION: u32 = 1;

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

/// Fleet capability identifiers advertised during protocol negotiation.
pub const FLEET_PROTOCOL_CAPABILITY_IDS: &[&str] = &[
    FLEET_CAPABILITY_ACTION_EXECUTE,
    FLEET_CAPABILITY_ATC_READ,
    FLEET_CAPABILITY_BROADCAST_EXECUTE,
    "fleet.protocol.negotiate",
    FLEET_CAPABILITY_RECEIPT_READ,
    "fleet.snapshot.read",
    FLEET_CAPABILITY_START_EXECUTE,
    "fleet.subscription.live",
    "fleet.subscription.replay",
    "fleet.subscription.resync",
    FLEET_CAPABILITY_TIMELINE_READ,
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
    /// Approve or deny exact provider requests.
    pub approvals: bool,
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
    /// Approve exact provider request.
    Approve {
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
            Self::Approve { .. } => "approve",
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
}
