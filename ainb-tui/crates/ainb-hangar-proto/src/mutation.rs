//! The mutation envelope: one op id, one fence, one vocabulary (D18).
//!
//! Every mutation on this wire carries a client-minted opaque op id and, where
//! the mutation has a thing it is racing against, a fence naming the state the
//! client believed it was acting on. The daemon commits, THEN replies, and the
//! reply says which of three things happened to the operation and which of
//! three things happened to the request:
//!
//! ```text
//! op id ──▶ ledger lookup ─┬─ miss  ──▶ execute ──▶ commit ──▶ created
//!                          ├─ hit, same body ─────────────────▶ replayed
//!                          ├─ hit, other body ────────────────▶ rejected{already_answered_by}
//!                          └─ hit, other principal ───────────▶ rejected{op_id_foreign}
//! ```
//!
//! ## Why a client-minted id and not a server one
//!
//! The failure this exists for is a LOST REPLY: the daemon committed, the
//! socket died, the client retried. A server-minted id is not in the client's
//! hand at retry time, so it cannot name the operation it is retrying. A
//! client-minted opaque id can, and is the only thing that can.
//!
//! ## Two tiers, deliberately
//!
//! 1. [`MutationTier::Dedupe`] — every mutation. The ledger stores the
//!    serialized reply; a replay returns it byte-for-byte. This is enough
//!    whenever the only cost of a double execution is a duplicated row.
//! 2. [`MutationTier::Receipt`] — the PTY-effecting mutations only. These type
//!    into somebody's terminal, so a double execution is a second `yes` on an
//!    agent about to run a command. They carry a receipt whose lifecycle is
//!    written in the SAME `SQLite` transaction as the state flip, never through
//!    the event outbox (which has a documented crash loss window).
//!
//! Adding a handler to tier 2 is a checklist item, not the default.
//!
//! ## Op ids are opaque
//!
//! 128 bits of client CSPRNG, conventionally rendered as 32 lowercase hex
//! characters. The daemon NEVER parses one: no timestamp is read out of it, so
//! a phone with a wrong clock is never rejected for skew, and no freshness rule
//! can be invented later without changing this type. The fleet family's
//! existing `request_id` IS the op id for that family
//! (`FleetActionParams::request_id`, `FleetMessageSendParams::request_id`,
//! `FleetStartParams::request_id`); W0-wire renames nothing on the wire and
//! accepts `op_id` as the alias.

use serde::{Deserialize, Serialize};

/// JSON-RPC error code for a mutation the daemon REFUSED on its own terms: a
/// foreign op id, a stale fence, a body that disagrees with a committed one.
///
/// Distinct from `INVALID_PARAMS`: the request was well-formed and the caller
/// was allowed to make it. What failed is the concurrency claim, and the
/// remedy is to re-read the state, never to re-send the same frame.
pub const MUTATION_REJECTED: i32 = -32008;

/// JSON-RPC error code for a mutation whose EFFECT the daemon cannot establish.
///
/// The honest third answer. A `writing` receipt found after a crash, or a
/// replay of an op id whose ledger row has been evicted: the daemon will not
/// claim the effect happened and will not claim it did not. The client says
/// "could not confirm, check the session".
pub const MUTATION_UNKNOWN: i32 = -32009;

/// Reason: the ledger holds this op id under a DIFFERENT principal.
///
/// Two devices minting the same 128-bit value is not a collision worth
/// modelling; a device replaying another device's id is. The ledger key is
/// `(host, principal, op_id)`, so the foreign row is invisible to this caller
/// and re-executing would be a second effect under a first caller's id.
pub const REASON_OP_ID_FOREIGN: &str = "op_id_foreign";
/// Reason: the ledger row for this op id has aged out of retention.
pub const REASON_OP_EXPIRED: &str = "op_expired";
/// Reason: the attention row was already answered, by somebody else or with
/// different text.
pub const REASON_ALREADY_ANSWERED_BY: &str = "already_answered_by";
/// Reason: the agent moved on since the client observed it
/// (`fleet_session.lifecycle_updated_at` advanced).
pub const REASON_TURN_ADVANCED: &str = "turn_advanced";
/// Reason: a different process now owns the session name.
pub const REASON_INCARNATION_MISMATCH: &str = "incarnation_mismatch";
/// Reason: a concurrent admin edit moved the registry version.
pub const REASON_CONFLICT: &str = "conflict";
/// Reason: the daemon died between `writing` and the reply, so the bytes may
/// or may not have reached the PTY.
pub const REASON_EFFECTS_AMBIGUOUS: &str = "effects_ambiguous";

/// The reserved key under which the daemon attaches a [`MutationAck`] to an
/// object-shaped mutation result.
///
/// Additive rather than a new result envelope: an N-1 client deserializes its
/// own result type and ignores this key, so the dedupe layer costs nothing on
/// the skew matrix. A client that knows the key reads what happened to its op
/// id without a second round trip.
pub const ACK_KEY: &str = "mutation";

/// A client-minted opaque operation id.
///
/// Deliberately a newtype over `String`, not over `u128`: the fleet family's
/// `request_id` values predate this type and are not hex, and the contract is
/// that the daemon treats the value as OPAQUE. Comparing, storing and echoing
/// is everything it may do with one.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct OpId(String);

/// The longest op id the ledger will store. 32 hex characters is the minted
/// shape; the cap exists so a peer cannot make the primary key unbounded.
pub const OP_ID_MAX_LEN: usize = 128;

impl OpId {
    /// Wrap an already-minted id, rejecting empty and over-long values.
    ///
    /// # Errors
    ///
    /// Returns the reason the value cannot be an op id.
    pub fn parse(value: impl Into<String>) -> Result<Self, String> {
        let value = value.into();
        if value.is_empty() {
            return Err("an op id must not be empty".to_string());
        }
        if value.len() > OP_ID_MAX_LEN {
            return Err(format!(
                "an op id must be at most {OP_ID_MAX_LEN} bytes, got {}",
                value.len()
            ));
        }
        Ok(Self(value))
    }

    /// Render 128 bits as the conventional 32 lowercase hex characters.
    ///
    /// The minting side is the CLIENT's: this crate has no RNG (and must keep
    /// none, per its no-host-deps discipline), so the caller supplies the
    /// bytes from its own CSPRNG.
    #[must_use]
    pub fn from_bytes(bytes: [u8; 16]) -> Self {
        use std::fmt::Write as _;
        let mut out = String::with_capacity(32);
        for byte in bytes {
            let _ = write!(out, "{byte:02x}");
        }
        Self(out)
    }

    /// The id as it travels on the wire.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The state a client believed it was mutating.
///
/// One variant per row of D18's fences table. A single `expected_version`
/// integer would have been one word for four different facts, and a kill
/// fenced on the wrong one kills a reused tmux session name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Fence {
    /// `attention/answer`: the attention row's `version`, and that it was open.
    AttentionVersion {
        /// The row version the client read.
        version: i64,
    },
    /// `fleet/message_send` and prompt send: the lifecycle clock the client
    /// observed. A later value means the agent moved on.
    LifecycleUpdatedAt {
        /// The `fleet_session.lifecycle_updated_at` the client read.
        lifecycle_updated_at: i64,
    },
    /// `fleet/action{cancel, kill}`: the incarnation of the process the client
    /// meant. A different one means a new process owns the name.
    SessionIncarnation {
        /// The incarnation fingerprint the client read.
        session_incarnation: String,
    },
    /// `device_revoke` (R1): the device registry version.
    RegistryVersion {
        /// The registry version the client read.
        version: i64,
    },
}

/// Which fence a mutating method is fenced on.
///
/// Carried in [`MutatingMethod`] so the fence table is a committed artifact and
/// not a comment. [`Self::None`] is honest: most mutations are last-write-wins
/// rows where a fence would be ceremony.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FenceKind {
    /// No fence: dedupe by op id is the whole guarantee.
    None,
    /// Fenced on the attention row's version and open state.
    AttentionVersion,
    /// Fenced on the session's observed lifecycle clock.
    LifecycleUpdatedAt,
    /// Fenced on the session incarnation.
    SessionIncarnation,
    /// Fenced on the device registry version.
    RegistryVersion,
}

/// The two tiers of guarantee (D18).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MutationTier {
    /// Generic dedupe at dispatch, keyed by the ledger row holding the
    /// serialized reply.
    Dedupe,
    /// Dedupe PLUS a transactional receipt with a `writing` boundary, because
    /// the mutation puts bytes into somebody's terminal.
    Receipt,
}

/// What happened to the OPERATION.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MutationOutcome {
    /// First sighting of this op id: the daemon executed it.
    Created,
    /// A committed row for this exact op id; the stored reply is returned
    /// verbatim and nothing ran again.
    Replayed,
}

/// What happened to the REQUEST.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MutationStatus {
    /// The mutation was applied (now or on an earlier attempt).
    Accepted,
    /// The daemon refused: a foreign op id, a stale fence, a conflicting body.
    Rejected,
    /// The daemon cannot establish whether the effect happened.
    Unknown,
}

/// The receipt lifecycle for a PTY-effecting mutation.
///
/// Written in the same transaction as the state flip. [`Self::Writing`] is set
/// immediately before the first byte reaches the PTY, which is what makes a
/// crash distinguishable from a refusal: a row still in `writing` at boot
/// becomes [`Self::Unknown`] and surfaces as a `delivery_unconfirmed` attention
/// row. It is NOT reopened (a retry could double-type) and NOT closed silently.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReceiptState {
    /// The mutation won its claim; nothing has been written yet.
    Claimed,
    /// Bytes are about to reach (or have reached) the PTY.
    Writing,
    /// The provider confirmed delivery.
    Delivered,
    /// The provider confirmed failure; no bytes landed.
    Failed,
    /// The effect cannot be established.
    Unknown,
}

impl ReceiptState {
    /// The stable operator-facing token. One spelling, defined once, so the
    /// daemon, the CLI and three GUIs cannot drift into `WRITING` vs `writing`.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Claimed => "claimed",
            Self::Writing => "writing",
            Self::Delivered => "delivered",
            Self::Failed => "failed",
            Self::Unknown => "unknown",
        }
    }

    /// Parse a stored token back. `None` for anything this build does not know.
    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        match token {
            "claimed" => Some(Self::Claimed),
            "writing" => Some(Self::Writing),
            "delivered" => Some(Self::Delivered),
            "failed" => Some(Self::Failed),
            "unknown" => Some(Self::Unknown),
            _ => None,
        }
    }
}

/// The envelope every mutating params struct embeds, flattened onto the params
/// object so the wire shape stays `{ ..method fields.., op_id?, fence? }`.
///
/// Both members are optional and default-absent: an N-1 client that sends
/// neither is served exactly as it is today, which is what keeps the skew
/// matrix green while the ledger rolls out.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MutationEnvelope {
    /// The client-minted opaque op id. Absent means "no dedupe for this call".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub op_id: Option<OpId>,
    /// The state the client believed it was acting on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fence: Option<Fence>,
}

impl MutationEnvelope {
    /// An envelope carrying only an op id.
    #[must_use]
    pub const fn with_op_id(op_id: OpId) -> Self {
        Self {
            op_id: Some(op_id),
            fence: None,
        }
    }

    /// An envelope carrying an op id and a fence.
    #[must_use]
    pub const fn fenced(op_id: OpId, fence: Fence) -> Self {
        Self {
            op_id: Some(op_id),
            fence: Some(fence),
        }
    }
}

/// What the daemon says happened to an op id, attached to the result object
/// under [`ACK_KEY`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MutationAck {
    /// What happened to the operation, when the operation ran at all.
    ///
    /// ABSENT for a refusal the ledger made before any handler was reached — a
    /// foreign op id, an aged-out row, an attempt still in flight. Naming one
    /// of the three outcomes there would be a lie in the direction that matters
    /// most: a client reading `replayed` concludes its earlier attempt
    /// committed, when in fact nothing of its own has ever run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<MutationOutcome>,
    /// What happened to the request.
    pub status: MutationStatus,
    /// The machine-readable reason, for a non-accepted status.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The receipt state, for a [`MutationTier::Receipt`] mutation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt: Option<ReceiptState>,
}

impl MutationAck {
    /// The ack for a mutation this daemon has just executed.
    #[must_use]
    pub const fn created() -> Self {
        Self {
            outcome: Some(MutationOutcome::Created),
            status: MutationStatus::Accepted,
            reason: None,
            receipt: None,
        }
    }

    /// The ack for a replay served from the ledger.
    #[must_use]
    pub const fn replayed(receipt: Option<ReceiptState>) -> Self {
        Self {
            outcome: Some(MutationOutcome::Replayed),
            status: MutationStatus::Accepted,
            reason: None,
            receipt,
        }
    }

    /// The ack for a refusal the ledger made before any handler ran.
    ///
    /// No outcome: this caller's operation did not execute, and will not.
    #[must_use]
    pub fn rejected(reason: &str) -> Self {
        Self {
            outcome: None,
            status: MutationStatus::Rejected,
            reason: Some(reason.to_string()),
            receipt: None,
        }
    }

    /// The ack for a refusal of an operation that DID run and was refused on
    /// its own terms, so the refusal is this attempt's answer.
    #[must_use]
    pub fn refused(outcome: MutationOutcome, reason: &str) -> Self {
        Self {
            outcome: Some(outcome),
            status: MutationStatus::Rejected,
            reason: Some(reason.to_string()),
            receipt: None,
        }
    }

    /// The ack for an effect the daemon cannot establish.
    ///
    /// No outcome either: "unknown" is precisely the statement that the daemon
    /// cannot say which of the three happened.
    #[must_use]
    pub fn unknown(reason: &str, receipt: Option<ReceiptState>) -> Self {
        Self {
            outcome: None,
            status: MutationStatus::Unknown,
            reason: Some(reason.to_string()),
            receipt,
        }
    }
}

/// One row of the committed mutation registry.
///
/// The registry is the artifact D18's gate is written against: a proto test
/// walks it and proves each params struct embeds a [`MutationEnvelope`], and a
/// daemon test walks it and replays every method twice.
pub struct MutatingMethod {
    /// The wire method name (a `crate::methods` const).
    pub method: &'static str,
    /// The params type's name, for diagnostics.
    pub params_type: &'static str,
    /// Which guarantee this method gets.
    pub tier: MutationTier,
    /// What this method is fenced on.
    pub fence: FenceKind,
    /// A minimal, VALID params object for this method, as JSON.
    ///
    /// Committed rather than generated: it is what both the proto embed test
    /// and the daemon replay test feed in, so it has to be readable next to
    /// the method it belongs to.
    pub sample_params: &'static str,
    /// Parse a params value through the method's OWN typed struct and hand
    /// back the envelope it embedded.
    ///
    /// This function pointer is the proof. It can only be written for a struct
    /// that actually has the field, so a method in this list whose params
    /// struct lost its envelope does not compile.
    pub envelope_of: fn(&serde_json::Value) -> Result<MutationEnvelope, String>,
}

impl std::fmt::Debug for MutatingMethod {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MutatingMethod")
            .field("method", &self.method)
            .field("params_type", &self.params_type)
            .field("tier", &self.tier)
            .field("fence", &self.fence)
            .finish_non_exhaustive()
    }
}

/// Look one method up in the registry.
#[must_use]
pub fn mutating(method: &str) -> Option<&'static MutatingMethod> {
    MUTATING_METHODS.iter().find(|m| m.method == method)
}

/// Whether `method` mutates state.
#[must_use]
pub fn is_mutating(method: &str) -> bool {
    mutating(method).is_some()
}

macro_rules! mutating_method {
    ($method:expr, $ty:ty, $tier:expr, $fence:expr, $sample:expr) => {
        MutatingMethod {
            method: $method,
            params_type: stringify!($ty),
            tier: $tier,
            fence: $fence,
            sample_params: $sample,
            envelope_of: {
                fn envelope_of(value: &serde_json::Value) -> Result<MutationEnvelope, String> {
                    serde_json::from_value::<$ty>(value.clone())
                        .map(|params| params.mutation)
                        .map_err(|e| e.to_string())
                }
                envelope_of
            },
        }
    };
}

// Shorthands used by the registry below; the table is long enough without the
// fully-qualified paths repeated 96 times.
use crate::fleet as f;
use crate::methods as m;
use crate::snapshots as s;
use FenceKind as Fk;
use MutationTier::{Dedupe, Receipt};

/// Every mutation this wire carries, with its tier, its fence, and a sample.
///
/// APPEND-ONLY and exhaustive: "generic dedupe at dispatch for every mutation"
/// is only true if this list is every mutation, so a new mutating method that
/// is not appended here is a hole in the guarantee, not a smaller feature.
pub static MUTATING_METHODS: &[MutatingMethod] = &[
    // ── attention ────────────────────────────────────────────────────────
    mutating_method!(
        m::ATTENTION_ANSWER,
        s::AnswerParams,
        Receipt,
        Fk::AttentionVersion,
        r#"{"attention_id":"att-sample","answer":"yes","answered_by":"harness"}"#
    ),
    // ── fleet control plane ──────────────────────────────────────────────
    // FenceKind::None, not SessionIncarnation, and the difference is honesty:
    // the verified send lives in `fleet.rs`, which another lane owns, so no
    // handler reads an incarnation fence today. Declaring one here would tell a
    // client reading the contract that it holds a stale-kill guard it does not
    // have. The row flips to `SessionIncarnation` in the change that enforces
    // it, and `receipt_tier_mutations_are_all_fenced` is the test that has to
    // be relaxed to allow this — deliberately, so the gap is visible.
    mutating_method!(
        m::FLEET_ACTION,
        f::FleetActionParams,
        Receipt,
        Fk::None,
        r#"{"session_key":"fleet-sample","expected_version":1,"request_id":"op-fleet-action","action":{"action":"kill"}}"#
    ),
    // FenceKind::None for the same reason as `fleet/action` above: the
    // lifecycle fence is specified and unenforced, so it is not claimed.
    mutating_method!(
        m::FLEET_MESSAGE_SEND,
        f::FleetMessageSendParams,
        Receipt,
        Fk::None,
        r#"{"targets":["fleet-sample"],"text":"hello","request_id":"op-fleet-message"}"#
    ),
    mutating_method!(
        m::FLEET_BROADCAST,
        f::FleetBroadcastParams,
        Dedupe,
        Fk::None,
        r#"{"target_keys":["fleet-sample"],"text":"hello","idempotency_key":"op-fleet-broadcast"}"#
    ),
    mutating_method!(
        m::FLEET_START,
        f::FleetStartParams,
        Dedupe,
        Fk::None,
        r#"{"request_id":"op-fleet-start","provider":"claude","cwd":"/nonexistent/harness"}"#
    ),
    mutating_method!(
        m::FLEET_ACP_SESSION_CREATE,
        f::FleetAcpSessionCreateParams,
        Dedupe,
        Fk::None,
        r#"{"provider":"claude","cwd":"/nonexistent/harness"}"#
    ),
    mutating_method!(
        m::FLEET_TRANSCRIPT_PRUNE,
        f::FleetTranscriptPruneParams,
        Dedupe,
        Fk::None,
        r#"{"session_key":"fleet-sample","before_order":1,"no_export":true}"#
    ),
    mutating_method!(
        m::FLEET_CHANNEL_CREATE,
        f::FleetChannelCreateParams,
        Dedupe,
        Fk::None,
        r#"{"kind":"broadcast","name":"harness"}"#
    ),
    mutating_method!(
        m::FLEET_CONFIRM_ANSWER,
        f::FleetConfirmAnswerParams,
        Dedupe,
        Fk::None,
        r#"{"confirm_id":"confirm-sample","answer":"deny"}"#
    ),
    mutating_method!(
        m::FLEET_PAL_CONFIGURE,
        f::FleetPalConfigureParams,
        Dedupe,
        Fk::None,
        r#"{"provider":"claude"}"#
    ),
    mutating_method!(
        m::FLEET_PAL_GATE,
        f::FleetPalGateParams,
        Dedupe,
        Fk::None,
        r#"{"tool":"harness_tool","arguments":{}}"#
    ),
    mutating_method!(
        m::CODEX_SESSION_ENSURE,
        f::CodexSessionEnsureParams,
        Dedupe,
        Fk::None,
        r#"{"session_id":"codex-sample","cwd":"/nonexistent/harness"}"#
    ),
    mutating_method!(
        m::CODEX_SESSION_DISCARD,
        f::CodexSessionDiscardParams,
        Dedupe,
        Fk::None,
        r#"{"session_id":"codex-sample"}"#
    ),
    // ── ATC registry ─────────────────────────────────────────────────────
    mutating_method!(
        m::ATC_REGISTER,
        s::AtcRegisterParams,
        Dedupe,
        Fk::None,
        r#"{"name":"harness-atc"}"#
    ),
    mutating_method!(
        m::ATC_UNREGISTER,
        s::AtcUnregisterParams,
        Dedupe,
        Fk::None,
        r#"{"name":"harness-atc"}"#
    ),
    mutating_method!(
        m::ATC_ESCALATE,
        s::AtcEscalateParams,
        Dedupe,
        Fk::None,
        r#"{"instance_name":"harness-atc","session_id":"s1","reason":"harness"}"#
    ),
    // ── issues ───────────────────────────────────────────────────────────
    mutating_method!(
        m::HANGAR_ISSUE_UPDATE,
        s::IssueUpdateParams,
        Dedupe,
        Fk::None,
        r#"{"workspace_id":"ws-sample","issue_id":"iss-sample","state":"todo"}"#
    ),
    mutating_method!(
        m::HANGAR_ISSUES_BATCH_UPDATE,
        s::IssuesBatchUpdateParams,
        Dedupe,
        Fk::None,
        r#"{"workspace_id":"ws-sample","issue_ids":["iss-sample"],"state":"todo"}"#
    ),
    mutating_method!(
        m::HANGAR_ISSUE_DELETE,
        s::IssueDeleteParams,
        Dedupe,
        Fk::None,
        r#"{"workspace_id":"ws-sample","issue_id":"iss-sample"}"#
    ),
    mutating_method!(
        m::HANGAR_ISSUE_CANCEL_ACTIVE,
        s::IssueCancelActiveParams,
        Dedupe,
        Fk::None,
        r#"{"workspace_id":"ws-sample","issue_id":"iss-sample"}"#
    ),
    mutating_method!(
        m::HANGAR_ISSUE_CRITERION_SET,
        s::IssueCriterionSetParams,
        Dedupe,
        Fk::None,
        r#"{"workspace_id":"ws-sample","issue_id":"iss-sample","criterion":"c","checked":true}"#
    ),
    mutating_method!(
        m::HANGAR_ISSUE_LABEL_ATTACH,
        s::IssueLabelParams,
        Dedupe,
        Fk::None,
        r#"{"workspace_id":"ws-sample","issue_id":"iss-sample","name":"bug"}"#
    ),
    mutating_method!(
        m::HANGAR_ISSUE_LABEL_DETACH,
        s::IssueLabelParams,
        Dedupe,
        Fk::None,
        r#"{"workspace_id":"ws-sample","issue_id":"iss-sample","name":"bug"}"#
    ),
    mutating_method!(
        m::HANGAR_ISSUE_LINK_ADD,
        s::IssueLinkParams,
        Dedupe,
        Fk::None,
        r#"{"workspace_id":"ws-sample","issue_id":"iss-sample","other_issue_id":"iss-other"}"#
    ),
    mutating_method!(
        m::HANGAR_ISSUE_LINK_REMOVE,
        s::IssueLinkParams,
        Dedupe,
        Fk::None,
        r#"{"workspace_id":"ws-sample","issue_id":"iss-sample","other_issue_id":"iss-other"}"#
    ),
    mutating_method!(
        m::HANGAR_ISSUE_SUBSCRIBE,
        s::IssueSubscribeParams,
        Dedupe,
        Fk::None,
        r#"{"workspace_id":"ws-sample","issue_id":"iss-sample"}"#
    ),
    mutating_method!(
        m::HANGAR_ISSUE_UNSUBSCRIBE,
        s::IssueSubscribeParams,
        Dedupe,
        Fk::None,
        r#"{"workspace_id":"ws-sample","issue_id":"iss-sample"}"#
    ),
    mutating_method!(
        m::HANGAR_ISSUE_REACTION_ADD,
        s::IssueReactionParams,
        Dedupe,
        Fk::None,
        r#"{"workspace_id":"ws-sample","issue_id":"iss-sample","emoji":"+1"}"#
    ),
    mutating_method!(
        m::HANGAR_ISSUE_REACTION_REMOVE,
        s::IssueReactionParams,
        Dedupe,
        Fk::None,
        r#"{"workspace_id":"ws-sample","issue_id":"iss-sample","emoji":"+1"}"#
    ),
    mutating_method!(
        m::HANGAR_ISSUE_PROPERTY_SET,
        s::IssuePropertySetParams,
        Dedupe,
        Fk::None,
        r#"{"workspace_id":"ws-sample","issue_id":"iss-sample","key":"team","value":"core"}"#
    ),
    mutating_method!(
        m::HANGAR_ISSUE_PROPERTY_CLEAR,
        s::IssuePropertyClearParams,
        Dedupe,
        Fk::None,
        r#"{"workspace_id":"ws-sample","issue_id":"iss-sample","key":"team"}"#
    ),
    mutating_method!(
        m::HANGAR_ISSUE_METADATA_SET,
        s::IssueMetadataParams,
        Dedupe,
        Fk::None,
        r#"{"workspace_id":"ws-sample","issue_id":"iss-sample","key":"k","value":"v"}"#
    ),
    mutating_method!(
        m::HANGAR_ISSUE_METADATA_DELETE,
        s::IssueMetadataParams,
        Dedupe,
        Fk::None,
        r#"{"workspace_id":"ws-sample","issue_id":"iss-sample","key":"k"}"#
    ),
    mutating_method!(
        m::HANGAR_COMMENT_ADD,
        s::CommentAddParams,
        Dedupe,
        Fk::None,
        r#"{"workspace_id":"ws-sample","issue_id":"iss-sample","author":"harness","body":"hi"}"#
    ),
    mutating_method!(
        m::HANGAR_PR_STATUS_REFRESH,
        s::PrStatusRefreshParams,
        Dedupe,
        Fk::None,
        r#"{"workspace_id":"ws-sample","issue_id":"iss-sample"}"#
    ),
    // ── properties ───────────────────────────────────────────────────────
    mutating_method!(
        m::HANGAR_PROPERTY_DEFINE,
        s::PropertyDefineParams,
        Dedupe,
        Fk::None,
        r#"{"workspace_id":"ws-sample","key":"team"}"#
    ),
    mutating_method!(
        m::HANGAR_PROPERTY_ARCHIVE,
        s::PropertyArchiveParams,
        Dedupe,
        Fk::None,
        r#"{"workspace_id":"ws-sample","key":"team"}"#
    ),
    // ── boards ───────────────────────────────────────────────────────────
    mutating_method!(
        m::HANGAR_BOARD_CREATE,
        s::BoardCreateParams,
        Dedupe,
        Fk::None,
        r#"{"workspace_id":"ws-sample","name":"harness"}"#
    ),
    mutating_method!(
        m::HANGAR_BOARD_UPDATE,
        s::BoardUpdateParams,
        Dedupe,
        Fk::None,
        r#"{"workspace_id":"ws-sample","board_id":"board-sample","name":"harness"}"#
    ),
    mutating_method!(
        m::HANGAR_BOARD_DELETE,
        s::BoardIdParams,
        Dedupe,
        Fk::None,
        r#"{"workspace_id":"ws-sample","board_id":"board-sample"}"#
    ),
    mutating_method!(
        m::HANGAR_BOARD_COLUMN_ADD,
        s::BoardColumnAddParams,
        Dedupe,
        Fk::None,
        r#"{"workspace_id":"ws-sample","board_id":"board-sample","name":"todo"}"#
    ),
    mutating_method!(
        m::HANGAR_BOARD_COLUMN_UPDATE,
        s::BoardColumnUpdateParams,
        Dedupe,
        Fk::None,
        r#"{"workspace_id":"ws-sample","board_id":"board-sample","column_id":"col-sample","name":"todo"}"#
    ),
    mutating_method!(
        m::HANGAR_BOARD_COLUMN_DELETE,
        s::BoardColumnDeleteParams,
        Dedupe,
        Fk::None,
        r#"{"workspace_id":"ws-sample","board_id":"board-sample","column_id":"col-sample"}"#
    ),
    mutating_method!(
        m::HANGAR_BOARD_COLUMN_REORDER,
        s::BoardColumnReorderParams,
        Dedupe,
        Fk::None,
        r#"{"workspace_id":"ws-sample","board_id":"board-sample","column_ids":["col-sample"]}"#
    ),
    mutating_method!(
        m::HANGAR_BOARD_CARD_ADD,
        s::BoardCardParams,
        Dedupe,
        Fk::None,
        r#"{"workspace_id":"ws-sample","board_id":"board-sample","issue_id":"iss-sample"}"#
    ),
    mutating_method!(
        m::HANGAR_BOARD_CARD_MOVE,
        s::BoardCardParams,
        Dedupe,
        Fk::None,
        r#"{"workspace_id":"ws-sample","board_id":"board-sample","issue_id":"iss-sample","column_id":"col-sample"}"#
    ),
    mutating_method!(
        m::HANGAR_BOARD_CARD_REMOVE,
        s::BoardCardParams,
        Dedupe,
        Fk::None,
        r#"{"workspace_id":"ws-sample","board_id":"board-sample","issue_id":"iss-sample"}"#
    ),
    mutating_method!(
        m::HANGAR_BOARD_CARD_CREATE,
        s::BoardCardCreateParams,
        Dedupe,
        Fk::None,
        r#"{"workspace_id":"ws-sample","board_id":"board-sample","title":"harness"}"#
    ),
    mutating_method!(
        m::HANGAR_BOARD_CARD_RUN,
        s::BoardCardRunParams,
        Dedupe,
        Fk::None,
        r#"{"workspace_id":"ws-sample","board_id":"board-sample","issue_id":"iss-sample","mode":"plan"}"#
    ),
    mutating_method!(
        m::HANGAR_BOARD_CARD_CANCEL,
        s::BoardCardCancelParams,
        Dedupe,
        Fk::None,
        r#"{"workspace_id":"ws-sample","board_id":"board-sample","issue_id":"iss-sample"}"#
    ),
    mutating_method!(
        m::HANGAR_BOARD_CARD_REORDER,
        s::BoardCardReorderParams,
        Dedupe,
        Fk::None,
        r#"{"workspace_id":"ws-sample","board_id":"board-sample","issue_ids":["iss-sample"]}"#
    ),
    mutating_method!(
        m::HANGAR_BOARD_CARD_ASSIGN_SQUAD,
        s::BoardCardAssignSquadParams,
        Dedupe,
        Fk::None,
        r#"{"workspace_id":"ws-sample","board_id":"board-sample","issue_id":"iss-sample"}"#
    ),
    mutating_method!(
        m::HANGAR_BOARD_CARD_DEP_ADD,
        s::BoardCardDepParams,
        Dedupe,
        Fk::None,
        r#"{"workspace_id":"ws-sample","board_id":"board-sample","dependent_issue_id":"iss-sample","blocker_issue_id":"iss-other"}"#
    ),
    mutating_method!(
        m::HANGAR_BOARD_CARD_DEP_REMOVE,
        s::BoardCardDepParams,
        Dedupe,
        Fk::None,
        r#"{"workspace_id":"ws-sample","board_id":"board-sample","dependent_issue_id":"iss-sample","blocker_issue_id":"iss-other"}"#
    ),
    mutating_method!(
        m::HANGAR_BOARD_CARD_SET_AUTO_RUN,
        s::BoardCardAutoRunParams,
        Dedupe,
        Fk::None,
        r#"{"workspace_id":"ws-sample","board_id":"board-sample","issue_id":"iss-sample","auto_run":true}"#
    ),
    // ── agents ───────────────────────────────────────────────────────────
    mutating_method!(
        m::HANGAR_AGENT_CREATE,
        s::AgentCreateParams,
        Dedupe,
        Fk::None,
        r#"{"workspace_id":"ws-sample","name":"harness-agent"}"#
    ),
    mutating_method!(
        m::HANGAR_AGENT_UPDATE,
        s::AgentUpdateParams,
        Dedupe,
        Fk::None,
        r#"{"workspace_id":"ws-sample","agent_id":"agent-sample","name":"harness-agent"}"#
    ),
    mutating_method!(
        m::HANGAR_AGENT_ARCHIVE,
        s::AgentArchiveParams,
        Dedupe,
        Fk::None,
        r#"{"workspace_id":"ws-sample","agent_id":"agent-sample","archived":true}"#
    ),
    mutating_method!(
        m::HANGAR_AGENT_DELETE,
        s::AgentDeleteParams,
        Dedupe,
        Fk::None,
        r#"{"workspace_id":"ws-sample","agent_id":"agent-sample"}"#
    ),
    // ── skills ───────────────────────────────────────────────────────────
    mutating_method!(
        m::HANGAR_SKILLS_SYNC,
        s::SkillsSyncParams,
        Dedupe,
        Fk::None,
        r#"{"workspace_id":"ws-sample"}"#
    ),
    mutating_method!(
        m::HANGAR_SKILL_ATTACH,
        s::SkillAttachParams,
        Dedupe,
        Fk::None,
        r#"{"workspace_id":"ws-sample","agent_id":"agent-sample","skill_id":"skill-sample"}"#
    ),
    mutating_method!(
        m::HANGAR_SKILL_DETACH,
        s::SkillAttachParams,
        Dedupe,
        Fk::None,
        r#"{"workspace_id":"ws-sample","agent_id":"agent-sample","skill_id":"skill-sample"}"#
    ),
    mutating_method!(
        m::HANGAR_SKILL_SET_ENABLED,
        s::SkillSetEnabledParams,
        Dedupe,
        Fk::None,
        r#"{"workspace_id":"ws-sample","agent_id":"agent-sample","skill_id":"skill-sample","enabled":true}"#
    ),
    // ── autopilots ───────────────────────────────────────────────────────
    mutating_method!(
        m::HANGAR_AUTOPILOT_UPDATE,
        s::AutopilotUpdateParams,
        Dedupe,
        Fk::None,
        r#"{"workspace_id":"ws-sample","autopilot_id":"auto-sample","name":"harness"}"#
    ),
    mutating_method!(
        m::HANGAR_AUTOPILOT_SET_ENABLED,
        s::AutopilotSetEnabledParams,
        Dedupe,
        Fk::None,
        r#"{"workspace_id":"ws-sample","autopilot_id":"auto-sample","enabled":true}"#
    ),
    mutating_method!(
        m::HANGAR_AUTOPILOT_FIRE_NOW,
        s::AutopilotFireNowParams,
        Dedupe,
        Fk::None,
        r#"{"workspace_id":"ws-sample","autopilot_id":"auto-sample"}"#
    ),
    mutating_method!(
        m::HANGAR_AUTOPILOT_TRIGGER_API,
        s::AutopilotTriggerApiParams,
        Dedupe,
        Fk::None,
        r#"{"workspace_id":"ws-sample","autopilot_id":"auto-sample"}"#
    ),
    mutating_method!(
        m::HANGAR_AUTOPILOT_SET_API_TRIGGER,
        s::AutopilotSetApiTriggerParams,
        Dedupe,
        Fk::None,
        r#"{"workspace_id":"ws-sample","autopilot_id":"auto-sample","enabled":true}"#
    ),
    mutating_method!(
        m::HANGAR_AUTOPILOT_SET_ACCESS_MODE,
        s::AutopilotSetAccessModeParams,
        Dedupe,
        Fk::None,
        r#"{"workspace_id":"ws-sample","autopilot_id":"auto-sample","access_mode":"open"}"#
    ),
    mutating_method!(
        m::HANGAR_AUTOPILOT_COLLABORATOR_ADD,
        s::AutopilotActorParams,
        Dedupe,
        Fk::None,
        r#"{"workspace_id":"ws-sample","autopilot_id":"auto-sample","actor":"harness"}"#
    ),
    mutating_method!(
        m::HANGAR_AUTOPILOT_COLLABORATOR_REMOVE,
        s::AutopilotActorParams,
        Dedupe,
        Fk::None,
        r#"{"workspace_id":"ws-sample","autopilot_id":"auto-sample","actor":"harness"}"#
    ),
    mutating_method!(
        m::HANGAR_AUTOPILOT_SUBSCRIBER_ADD,
        s::AutopilotActorParams,
        Dedupe,
        Fk::None,
        r#"{"workspace_id":"ws-sample","autopilot_id":"auto-sample","actor":"harness"}"#
    ),
    mutating_method!(
        m::HANGAR_AUTOPILOT_SUBSCRIBER_REMOVE,
        s::AutopilotActorParams,
        Dedupe,
        Fk::None,
        r#"{"workspace_id":"ws-sample","autopilot_id":"auto-sample","actor":"harness"}"#
    ),
    // ── squads ───────────────────────────────────────────────────────────
    mutating_method!(
        m::HANGAR_SQUAD_CREATE,
        s::SquadCreateParams,
        Dedupe,
        Fk::None,
        r#"{"workspace_id":"ws-sample","name":"harness","leader":"agent-sample"}"#
    ),
    mutating_method!(
        m::HANGAR_SQUAD_MEMBER_ADD,
        s::SquadMemberParams,
        Dedupe,
        Fk::None,
        r#"{"workspace_id":"ws-sample","squad_id":"squad-sample","member":"agent-sample"}"#
    ),
    mutating_method!(
        m::HANGAR_SQUAD_MEMBER_REMOVE,
        s::SquadMemberParams,
        Dedupe,
        Fk::None,
        r#"{"workspace_id":"ws-sample","squad_id":"squad-sample","member":"agent-sample"}"#
    ),
    mutating_method!(
        m::HANGAR_SQUAD_MEMBER_ROLE_SET,
        s::SquadMemberRoleParams,
        Dedupe,
        Fk::None,
        r#"{"workspace_id":"ws-sample","squad_id":"squad-sample","member":"agent-sample","role":"member"}"#
    ),
    mutating_method!(
        m::HANGAR_SQUAD_ASSIGN,
        s::SquadAssignParams,
        Dedupe,
        Fk::None,
        r#"{"workspace_id":"ws-sample","squad_id":"squad-sample"}"#
    ),
    mutating_method!(
        m::HANGAR_SQUAD_FANOUT,
        s::SquadAssignParams,
        Dedupe,
        Fk::None,
        r#"{"workspace_id":"ws-sample","squad_id":"squad-sample"}"#
    ),
    mutating_method!(
        m::HANGAR_SQUAD_ARCHIVE,
        s::SquadArchiveParams,
        Dedupe,
        Fk::None,
        r#"{"workspace_id":"ws-sample","squad_id":"squad-sample","archived":true}"#
    ),
    mutating_method!(
        m::HANGAR_SQUAD_INSTRUCTIONS_SET,
        s::SquadInstructionsParams,
        Dedupe,
        Fk::None,
        r#"{"workspace_id":"ws-sample","squad_id":"squad-sample","instructions":"harness"}"#
    ),
    // ── membership ───────────────────────────────────────────────────────
    mutating_method!(
        m::HANGAR_MEMBER_SET_ROLE,
        s::MemberSetRoleParams,
        Dedupe,
        Fk::None,
        r#"{"workspace_id":"ws-sample","user_id":"user-sample","role":"member"}"#
    ),
    mutating_method!(
        m::HANGAR_MEMBER_REMOVE,
        s::MemberRemoveParams,
        Dedupe,
        Fk::None,
        r#"{"workspace_id":"ws-sample","user_id":"user-sample"}"#
    ),
    mutating_method!(
        m::HANGAR_INVITE_CREATE,
        s::InviteCreateParams,
        Dedupe,
        Fk::None,
        r#"{"workspace_id":"ws-sample","inviter_user_id":"user-sample","invitee_email":"a@example.com","role":"member"}"#
    ),
    mutating_method!(
        m::HANGAR_INVITE_ACCEPT,
        s::InviteActParams,
        Dedupe,
        Fk::None,
        r#"{"workspace_id":"ws-sample","invitation_id":"inv-sample","actor_email":"a@example.com"}"#
    ),
    mutating_method!(
        m::HANGAR_INVITE_DECLINE,
        s::InviteActParams,
        Dedupe,
        Fk::None,
        r#"{"workspace_id":"ws-sample","invitation_id":"inv-sample","actor_email":"a@example.com"}"#
    ),
    mutating_method!(
        m::HANGAR_INVITE_REVOKE,
        s::InviteRevokeParams,
        Dedupe,
        Fk::None,
        r#"{"workspace_id":"ws-sample","invitation_id":"inv-sample"}"#
    ),
    // ── tasks, inbox, config, profiles ───────────────────────────────────
    mutating_method!(
        m::HANGAR_TASK_TRANSITION,
        s::TaskTransitionParams,
        Dedupe,
        Fk::None,
        r#"{"workspace_id":"ws-sample","task_id":"task-sample","to_status":"running"}"#
    ),
    mutating_method!(
        m::HANGAR_TASK_RETRY,
        s::TaskRetryParams,
        Dedupe,
        Fk::None,
        r#"{"workspace_id":"ws-sample","task_id":"task-sample"}"#
    ),
    mutating_method!(
        m::HANGAR_INBOX_MARK_READ,
        s::InboxScopedParams,
        Dedupe,
        Fk::None,
        r#"{"workspace_id":"ws-sample"}"#
    ),
    mutating_method!(
        m::HANGAR_NOTIFY_RULE_SET,
        s::NotifyRuleSetParams,
        Dedupe,
        Fk::None,
        r#"{"kind":"attention_raised","channels":[]}"#
    ),
    mutating_method!(
        m::HANGAR_DAEMON_CONFIG_SET,
        s::DaemonConfigSetParams,
        Dedupe,
        Fk::None,
        r#"{"key":"harness.key","value":"harness"}"#
    ),
    mutating_method!(
        m::PROFILE_UPSERT,
        s::ProfileUpsertParams,
        Dedupe,
        Fk::None,
        r#"{"slug":"harness","tier":"standard"}"#
    ),
];

#[cfg(test)]
mod tests {
    use super::*;

    /// The gate D18 names: walk the committed list and prove each params
    /// struct embeds a [`MutationEnvelope`] that actually round-trips.
    ///
    /// The sample is parsed twice: once bare (an N-1 client sends no envelope,
    /// and must still decode) and once with an op id and a fence spliced in at
    /// the TOP level, which is the flattened shape. A struct that dropped the
    /// field would fail the second parse by answering `None` for an op id the
    /// wire carried.
    #[test]
    fn every_mutating_method_embeds_the_envelope() {
        let op = OpId::from_bytes([0xab; 16]);
        for entry in MUTATING_METHODS {
            let mut value: serde_json::Value = serde_json::from_str(entry.sample_params)
                .unwrap_or_else(|e| panic!("{}: sample is not JSON: {e}", entry.method));
            let bare = (entry.envelope_of)(&value)
                .unwrap_or_else(|e| panic!("{} ({}): {e}", entry.method, entry.params_type));
            assert_eq!(
                bare,
                MutationEnvelope::default(),
                "{}: a params object with no envelope members must decode to the default",
                entry.method
            );

            let object = value
                .as_object_mut()
                .unwrap_or_else(|| panic!("{}: sample must be a JSON object", entry.method));
            object.insert("op_id".to_string(), serde_json::json!(op.as_str()));
            object.insert(
                "fence".to_string(),
                serde_json::json!({"kind": "attention_version", "version": 7}),
            );
            let carried = (entry.envelope_of)(&value)
                .unwrap_or_else(|e| panic!("{} ({}): {e}", entry.method, entry.params_type));
            assert_eq!(
                carried,
                MutationEnvelope::fenced(op.clone(), Fence::AttentionVersion { version: 7 }),
                "{} ({}) does not embed MutationEnvelope on the wire",
                entry.method,
                entry.params_type
            );
        }
    }

    /// Every registered method is a real method name, and no method is
    /// registered twice under the same tier by accident.
    #[test]
    fn the_registry_names_real_methods() {
        assert!(
            MUTATING_METHODS.len() >= 90,
            "the registry lost entries: {} left",
            MUTATING_METHODS.len()
        );
        let mut seen = std::collections::HashSet::new();
        for entry in MUTATING_METHODS {
            assert!(
                crate::methods::ALL_METHODS.contains(&entry.method),
                "{} is not in ALL_METHODS",
                entry.method
            );
            assert!(
                seen.insert(entry.method),
                "{} is registered twice",
                entry.method
            );
        }
    }

    /// Tier 2 is exactly the PTY-effecting set D18 names. `terminal/input` is
    /// absent because the method does not exist yet (R2 adds it); when it
    /// does, this assertion is where it gets added, deliberately.
    #[test]
    fn the_receipt_tier_is_the_pty_effecting_set() {
        let receipts: Vec<&str> = MUTATING_METHODS
            .iter()
            .filter(|m| matches!(m.tier, MutationTier::Receipt))
            .map(|m| m.method)
            .collect();
        assert_eq!(
            receipts,
            vec![
                crate::methods::ATTENTION_ANSWER,
                crate::methods::FLEET_ACTION,
                crate::methods::FLEET_MESSAGE_SEND,
            ],
            "adding a handler to the receipt tier is a checklist item, not a default"
        );
    }

    /// A declared fence must be an ENFORCED fence.
    ///
    /// The earlier shape of this test asserted that every tier-2 mutation names
    /// a fence, which the table satisfied by naming two that no handler reads.
    /// That is the worse failure: a client that reads the contract and sends a
    /// lifecycle fence believes it holds a stale-send guard that does not
    /// exist. So the list of fenced methods is pinned exactly, and adding a row
    /// to it means adding the enforcement in the same change.
    #[test]
    fn only_enforced_fences_are_declared() {
        let fenced: Vec<(&str, FenceKind)> = MUTATING_METHODS
            .iter()
            .filter(|m| m.fence != FenceKind::None)
            .map(|m| (m.method, m.fence))
            .collect();
        assert_eq!(
            fenced,
            vec![(
                crate::methods::ATTENTION_ANSWER,
                FenceKind::AttentionVersion
            )],
            "`fleet/action` (session_incarnation) and `fleet/message_send` \
             (lifecycle_updated_at) are specified in D18 and enforced nowhere: \
             their executor lives in a file another lane owns. Flip the registry \
             row in the change that reads the fence, not before."
        );
    }

    /// Op ids are opaque: bounded, non-empty, never parsed.
    #[test]
    fn op_id_parsing_bounds_the_key() {
        assert!(OpId::parse("").is_err());
        assert!(OpId::parse("x".repeat(OP_ID_MAX_LEN + 1)).is_err());
        assert_eq!(
            OpId::parse("action-request-001").unwrap().as_str(),
            "action-request-001"
        );
        assert_eq!(
            OpId::from_bytes([
                0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd,
                0xee, 0xff
            ])
            .as_str(),
            "00112233445566778899aabbccddeeff"
        );
    }

    /// The receipt tokens are the five the spec names, and they round-trip.
    #[test]
    fn receipt_tokens_round_trip() {
        for state in [
            ReceiptState::Claimed,
            ReceiptState::Writing,
            ReceiptState::Delivered,
            ReceiptState::Failed,
            ReceiptState::Unknown,
        ] {
            assert_eq!(ReceiptState::from_token(state.token()), Some(state));
        }
        assert_eq!(ReceiptState::from_token("pending"), None);
    }

    /// The ack serializes with the tagged vocabulary a client branches on.
    #[test]
    fn the_ack_is_the_two_axis_vocabulary() {
        let json = serde_json::to_value(MutationAck::created()).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"outcome":"created","status":"accepted"})
        );
        // No `outcome`: a foreign op id is a refusal of something this caller
        // never ran, and claiming `replayed` would tell it the opposite.
        let json = serde_json::to_value(MutationAck::rejected(REASON_OP_ID_FOREIGN)).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"status":"rejected","reason":"op_id_foreign"})
        );
        // A refusal the HANDLER produced does name the attempt that produced it.
        let json = serde_json::to_value(MutationAck::refused(
            MutationOutcome::Created,
            "turn_advanced",
        ))
        .unwrap();
        assert_eq!(
            json,
            serde_json::json!({"outcome":"created","status":"rejected","reason":"turn_advanced"})
        );
        let json = serde_json::to_value(MutationAck::unknown(
            REASON_EFFECTS_AMBIGUOUS,
            Some(ReceiptState::Unknown),
        ))
        .unwrap();
        assert_eq!(
            json,
            serde_json::json!({"status":"unknown","reason":"effects_ambiguous","receipt":"unknown"})
        );
    }

    /// The two mutation codes sit in the server-error range and collide with
    /// nothing else this crate defines.
    #[test]
    fn mutation_codes_are_distinct_server_errors() {
        for code in [MUTATION_REJECTED, MUTATION_UNKNOWN] {
            assert!((-32099..=-32000).contains(&code));
            assert_ne!(code, crate::auth::UNAUTHORIZED);
            assert_ne!(code, crate::STORE_UNAVAILABLE);
            assert_ne!(code, crate::protocol::PROTOCOL_INCOMPATIBLE);
        }
        assert_ne!(MUTATION_REJECTED, MUTATION_UNKNOWN);
    }
}
