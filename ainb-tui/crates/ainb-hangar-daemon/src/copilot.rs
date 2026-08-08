//! The copilot service: the guardrail gate every copilot tool call passes
//! through, and the confirm cards it parks on (buzz-port part 2, phase A2).
//!
//! ```text
//!   tool call ─▶ Guardrail::classify ─┬─ Auto ──────▶ activity row ─▶ Run
//!   (tool + args, never prose)        │
//!                                     ├─ Refused ───▶ activity row ─▶ Refused
//!                                     │
//!                                     └─ Confirm ──▶ project_arguments
//!                                                     ─▶ persist card (open)
//!                                                     ─▶ fleet/confirm_event
//!                                                     ─▶ await, BOUNDED
//!                                        ┌────────────────┴───────────────┐
//!                                   answer arrives                  nothing does
//!                                   approve/edit/deny            expires (10 min)
//!                                        │                              │
//!                                    Run / Denied                    Expired
//! ```
//!
//! Four properties are load-bearing here:
//!
//! * **The park is BOUNDED.** A card nobody answers expires at
//!   [`confirm_ttl`], which is deliberately far shorter than part 1's
//!   30-minute per-turn deadline. A suspended tool result holds the copilot's
//!   ACP turn open, and that turn holds its scope's FIFO queue, so an unbounded
//!   park would wedge the channel behind one unanswered dialog. The expiry
//!   resolves the tool as DENIED, which is the fail-closed direction.
//! * **The card carries PROJECTED arguments.** [`ainb_fleet_tools::server::project_arguments`]
//!   runs before the row is persisted, so a model-authored `justification` or
//!   `operator_approved` key never reaches the human's approve dialog. The
//!   classifier already ignores undeclared keys; this is the same fence for the
//!   human verdict.
//! * **A card is single-use.** The store resolves under `WHERE state = 'open'`,
//!   so an answer racing the expiry has exactly one winner and the loser gets a
//!   typed error rather than a second execution.
//! * **Every copilot action lands an activity row.** Including the refusals and
//!   the expiries, because "zero unlogged copilot writes" is only checkable if
//!   the log covers the calls that did NOT happen too.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

use ainb_fleet_tools::guardrail::{self, ConfirmReason, Guardrail, Refusal, Verdict};
use ainb_fleet_tools::server::project_arguments;
use ainb_hangar_core::clock::{HangarClock, SystemClock};
use ainb_hangar_core::idgen::{IdGen, SystemIdGen};
use ainb_hangar_proto::fleet::{
    FleetActivityClass, FleetActivityEventParams, FleetActivityOutcome, FleetActivityRow,
    FleetConfirm, FleetConfirmEventParams, FleetConfirmState,
};
use ainb_hangar_store::repo::fleet_chat::{
    FleetActivityRepo, FleetConfirmRepo, FleetConfirmRow, NewFleetActivity,
};
use ainb_hangar_store::repo::fleet_message::{FleetMessageRepo, NewFleetMessage};
use serde_json::{Map, Value};
use sqlx::SqlitePool;
use tokio::sync::oneshot;

use crate::events::EventSink;

/// The `sender` every copilot-authored chat row carries.
///
/// NEVER `"operator"`. A copilot write wearing the operator's name is a
/// privilege escalation by proxy: the receiving agent's re-prime header tells
/// it the operator's message is the one to act on, so a copilot that can forge
/// that name never needs the destructive tools at all — it can ask another
/// agent to do the thing instead.
pub const COPILOT_ACTOR: &str = "copilot";

/// Default confirm-card lifetime.
///
/// STRICTLY shorter than part 1's default 30-minute turn deadline, which is the
/// whole point: the card holds a turn open, so if the card outlived the
/// deadline the deadline would converge the turn out from under a dialog the
/// operator is still looking at.
const CONFIRM_TTL_DEFAULT: Duration = Duration::from_mins(10);

/// The confirm-card lifetime a production copilot turn parks under.
///
/// A CONST rather than a config knob: the value's whole justification is its
/// relationship to part 1's turn deadline, and a knob that can be turned past
/// the deadline is a knob that converges a turn out from under a live dialog.
/// [`gate`] takes the lifetime as an argument so a test can shrink it without
/// process-global state; nothing else should pass anything but this.
#[must_use]
pub const fn confirm_ttl() -> Duration {
    CONFIRM_TTL_DEFAULT
}

/// How a guarded tool call ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateOutcome {
    /// Execute the tool with these arguments. For a confirm card answered
    /// `edit`, these are the OPERATOR's arguments, not the model's.
    Run(Map<String, Value>),
    /// A human said no.
    Denied,
    /// The card reached its expiry unanswered; the tool resolves as denied.
    Expired,
    /// The call is not executable at all (unknown tool, malformed arguments).
    Refused(String),
}

/// Answering a confirm card failed.
#[derive(Debug, thiserror::Error)]
pub enum ConfirmError {
    /// No card with that id.
    #[error("confirm card {confirm_id:?} was not found")]
    NotFound {
        /// The unknown card.
        confirm_id: String,
    },
    /// The card is already answered or already expired. SINGLE-USE: this is a
    /// typed error rather than a second execution.
    #[error("confirm card {confirm_id:?} is already {state}")]
    AlreadyResolved {
        /// The card.
        confirm_id: String,
        /// Its terminal state.
        state: String,
    },
    /// The store failed.
    #[error(transparent)]
    Sql(#[from] sqlx::Error),
}

/// The parked tool calls awaiting an operator, keyed by `confirm_id`.
///
/// Process-global because the two halves live on different tasks: the gate
/// parks on a copilot turn, and `fleet/confirm_answer` arrives on some other
/// connection entirely. A `oneshot` per card, so a resolved card cannot be
/// resumed twice even if the store guard were ever loosened.
static WAITERS: LazyLock<Mutex<HashMap<String, oneshot::Sender<Answer>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

#[derive(Debug, Clone, PartialEq, Eq)]
enum Answer {
    Approved(Map<String, Value>),
    Denied,
}

/// Classify one copilot tool call, park it if a human is required, and log it.
///
/// `guardrail` is the state the DAEMON pinned for this turn (the sessions the
/// operator's message named, plus the operator's per-tool overrides). It is
/// never derived from anything the model wrote.
pub async fn gate(
    pool: &SqlitePool,
    events: &EventSink,
    scope_key: &str,
    tool: &str,
    arguments: &Map<String, Value>,
    guardrail: &Guardrail,
    ttl: Duration,
) -> GateOutcome {
    let target = target_of(arguments);
    match guardrail.classify(tool, arguments) {
        Verdict::Refused(refusal) => {
            let detail = match &refusal {
                Refusal::UnknownTool(name) => format!("unknown_tool; {name}"),
                Refusal::BadArguments(detail) => format!("bad_arguments; {detail}"),
            };
            record_activity(
                pool,
                events,
                scope_key,
                tool,
                target.as_deref(),
                FleetActivityOutcome::Error,
                Some(&detail),
            )
            .await;
            GateOutcome::Refused(detail)
        }
        Verdict::Auto => {
            record_activity(
                pool,
                events,
                scope_key,
                tool,
                target.as_deref(),
                FleetActivityOutcome::Ok,
                None,
            )
            .await;
            GateOutcome::Run(arguments.clone())
        }
        Verdict::Confirm(reason) => {
            park(pool, events, scope_key, tool, arguments, reason, ttl).await
        }
    }
}

/// Mint a confirm card, announce it, and wait for a human, BOUNDED by
/// [`confirm_ttl`].
async fn park(
    pool: &SqlitePool,
    events: &EventSink,
    scope_key: &str,
    tool: &str,
    arguments: &Map<String, Value>,
    reason: ConfirmReason,
    ttl: Duration,
) -> GateOutcome {
    let target = target_of(arguments);
    // BEFORE the insert, never after: the persisted row is what the operator's
    // dialog renders, so an undeclared key must not exist in it at all.
    let projected = project_arguments(tool, arguments);
    let now = SystemClock.now_ms();
    let expires_at = now.saturating_add(i64::try_from(ttl.as_millis()).unwrap_or(i64::MAX));
    let confirm_id = SystemIdGen.new_ulid();
    let card = FleetConfirmRow {
        confirm_id: confirm_id.clone(),
        scope_key: scope_key.to_string(),
        tool: tool.to_string(),
        arguments: Value::Object(projected.clone()).to_string(),
        target_session_key: target.clone(),
        state: "open".to_string(),
        edited_arguments: None,
        created_at: now,
        expires_at,
        answered_at: None,
    };
    if let Err(error) = FleetConfirmRepo::insert(pool, &card).await {
        tracing::error!(%error, tool, "could not persist a confirm card; refusing the call");
        return GateOutcome::Refused(format!("store_error; {error}"));
    }
    let (tx, rx) = oneshot::channel();
    if let Ok(mut waiters) = WAITERS.lock() {
        waiters.insert(confirm_id.clone(), tx);
    }
    tracing::info!(
        %confirm_id,
        tool,
        reason = confirm_reason_token(reason),
        "copilot tool call parked on a confirm card"
    );
    emit_confirm(events, &card, FleetConfirmState::Open);

    let answer = tokio::time::timeout(ttl, rx).await;
    // Whatever happened, this card is done waiting; a stale sender left behind
    // would keep a dead oneshot alive for the life of the daemon.
    if let Ok(mut waiters) = WAITERS.lock() {
        waiters.remove(&confirm_id);
    }

    match answer {
        Ok(Ok(Answer::Approved(arguments))) => {
            record_activity(
                pool,
                events,
                scope_key,
                tool,
                target.as_deref(),
                FleetActivityOutcome::Ok,
                Some("confirm_approved"),
            )
            .await;
            GateOutcome::Run(arguments)
        }
        Ok(Ok(Answer::Denied)) => {
            record_activity(
                pool,
                events,
                scope_key,
                tool,
                target.as_deref(),
                FleetActivityOutcome::Denied,
                Some("confirm_denied"),
            )
            .await;
            GateOutcome::Denied
        }
        // The sender was dropped without an answer (a daemon shutdown, or an
        // answer path that failed after taking the waiter). Fail CLOSED.
        Ok(Err(_)) | Err(_) => {
            expire(pool, events, &confirm_id).await;
            record_activity(
                pool,
                events,
                scope_key,
                tool,
                target.as_deref(),
                FleetActivityOutcome::Expired,
                Some("confirm_expired"),
            )
            .await;
            GateOutcome::Expired
        }
    }
}

/// Resolve an open card as expired and announce it. A no-op when an answer won
/// the race, which is the single-use guard doing its job.
async fn expire(pool: &SqlitePool, events: &EventSink, confirm_id: &str) {
    match FleetConfirmRepo::expire(pool, confirm_id, SystemClock.now_ms()).await {
        Ok(Some(card)) => {
            tracing::info!(%confirm_id, "confirm card expired unanswered; resolving the tool denied");
            emit_confirm(events, &card, FleetConfirmState::Expired);
        }
        Ok(None) => {}
        Err(error) => tracing::error!(%error, %confirm_id, "could not expire a confirm card"),
    }
}

/// Answer one confirm card: the write half of `fleet/confirm_answer`.
///
/// Resolves the durable row FIRST (single-use is the store's guard, not this
/// function's), then wakes the parked tool call. A card whose waiter is gone —
/// the daemon restarted, or the turn already converged — still resolves, so the
/// operator's answer is never silently lost.
pub async fn answer(
    pool: &SqlitePool,
    events: &EventSink,
    confirm_id: &str,
    approve: bool,
    edited: Option<Map<String, Value>>,
) -> Result<FleetConfirm, ConfirmError> {
    let existing =
        FleetConfirmRepo::get(pool, confirm_id)
            .await?
            .ok_or_else(|| ConfirmError::NotFound {
                confirm_id: confirm_id.to_string(),
            })?;
    let state = if approve { "approved" } else { "denied" };
    // An `edit` answer is projected too: the operator's arguments go to the
    // tool, but an operator UI relaying a model-supplied blob unchanged would
    // otherwise be a way back in for the keys the card just dropped.
    let edited = edited.map(|arguments| project_arguments(&existing.tool, &arguments));
    let edited_json = edited.as_ref().map(|arguments| Value::Object(arguments.clone()).to_string());
    let now = SystemClock.now_ms();
    let resolved =
        match FleetConfirmRepo::resolve(pool, confirm_id, state, edited_json.as_deref(), now)
            .await?
        {
            Some(resolved) => resolved,
            // A row still reading `open` that refuses the write is one whose TTL
            // lapsed with no process left watching it (a restart between the park
            // and the answer). Fail CLOSED and record the lapse, so the card stops
            // claiming to be answerable instead of collecting another approve.
            None if existing.state == "open" => {
                FleetConfirmRepo::expire(pool, confirm_id, now).await?;
                return Err(ConfirmError::AlreadyResolved {
                    confirm_id: confirm_id.to_string(),
                    state: "expired".to_string(),
                });
            }
            None => {
                return Err(ConfirmError::AlreadyResolved {
                    confirm_id: confirm_id.to_string(),
                    state: existing.state.clone(),
                });
            }
        };

    let waiter = WAITERS.lock().ok().and_then(|mut waiters| waiters.remove(confirm_id));
    if let Some(waiter) = waiter {
        let answer = if approve {
            let arguments = edited.unwrap_or_else(|| {
                serde_json::from_str::<Map<String, Value>>(&resolved.arguments).unwrap_or_default()
            });
            Answer::Approved(arguments)
        } else {
            Answer::Denied
        };
        let _ = waiter.send(answer);
    }
    let card = wire_confirm(&resolved);
    events.emit_fleet_notification(
        ainb_hangar_proto::methods::FLEET_CONFIRM_EVENT,
        serde_json::to_value(FleetConfirmEventParams {
            confirm: card.clone(),
        })
        .unwrap_or(Value::Null),
    );
    Ok(card)
}

/// Post one copilot-authored line into a channel timeline.
///
/// The `sender` is [`COPILOT_ACTOR`] and never the operator: see that const.
pub async fn post_channel_message(
    pool: &SqlitePool,
    events: &EventSink,
    scope_key: &str,
    body: &str,
) -> Result<String, sqlx::Error> {
    let row = FleetMessageRepo::insert_message(
        pool,
        &NewFleetMessage {
            id: SystemIdGen.new_ulid(),
            request_id: None,
            request_fingerprint: None,
            scope_key: scope_key.to_string(),
            origin_message_id: None,
            sender: COPILOT_ACTOR.to_string(),
            kind: "agent".to_string(),
            body: body.to_string(),
            created_at: SystemClock.now_ms(),
        },
    )
    .await
    .map_err(|error| match error {
        ainb_hangar_store::repo::fleet_message::FleetMessageError::Sql(error) => error,
        other => sqlx::Error::Protocol(other.to_string()),
    })?;
    events.emit_message_seq(row.seq);
    Ok(row.id)
}

/// Log one `fleet/copilot_configure` write to the activity feed.
///
/// The persona is a privileged field (a system prompt for an agent holding
/// destructive tools), so every change to it is visible to anyone reviewing
/// what the copilot has been doing. `detail` says WHETHER a persona is set,
/// never what it says: this feed is readable with `fleet.chat.read`, and the
/// persona is gated behind `fleet.copilot.configure`.
pub async fn record_configure(
    pool: &SqlitePool,
    events: &EventSink,
    scope_key: &str,
    detail: &str,
) {
    record_activity(
        pool,
        events,
        scope_key,
        "copilot_configure",
        None,
        FleetActivityOutcome::Ok,
        Some(detail),
    )
    .await;
}

/// Append one activity row and announce it. Best-effort: a copilot action is
/// never failed because its audit row could not be written, but the failure is
/// logged loudly, because an unlogged write is exactly the thing this feed
/// exists to make impossible.
async fn record_activity(
    pool: &SqlitePool,
    events: &EventSink,
    scope_key: &str,
    tool: &str,
    target: Option<&str>,
    outcome: FleetActivityOutcome,
    detail: Option<&str>,
) {
    let class = class_of(tool);
    let row = NewFleetActivity {
        id: SystemIdGen.new_ulid(),
        scope_key: scope_key.to_string(),
        tool: tool.to_string(),
        class: activity_class_token(class).to_string(),
        target_session_key: target.map(ToString::to_string),
        outcome: activity_outcome_token(outcome).to_string(),
        detail: detail.map(ToString::to_string),
        created_at: SystemClock.now_ms(),
    };
    match FleetActivityRepo::insert(pool, &row).await {
        Ok(row) => {
            events.emit_fleet_notification(
                ainb_hangar_proto::methods::FLEET_ACTIVITY_EVENT,
                serde_json::to_value(FleetActivityEventParams {
                    activity: wire_activity(&row),
                })
                .unwrap_or(Value::Null),
            );
        }
        Err(error) => {
            tracing::error!(%error, tool, "could not persist a copilot activity row");
        }
    }
}

/// The session a tool call names, when it names one. The ONE reading of the
/// target, so a card and its activity row can never disagree about who was
/// acted on.
fn target_of(arguments: &Map<String, Value>) -> Option<String> {
    arguments.get("session").and_then(Value::as_str).map(ToString::to_string)
}

/// The guardrail class of one tool, from the classifier's own tables.
#[must_use]
pub fn class_of(tool: &str) -> FleetActivityClass {
    if guardrail::READ_TOOLS.contains(&tool) {
        FleetActivityClass::Read
    } else if guardrail::CONFIRM_TOOLS.contains(&tool) {
        FleetActivityClass::Destructive
    } else {
        FleetActivityClass::Write
    }
}

const fn confirm_reason_token(reason: ConfirmReason) -> &'static str {
    match reason {
        ConfirmReason::DestructiveTool => "destructive_tool",
        ConfirmReason::SessionNotNamedByOperator => "session_not_named_by_operator",
    }
}

/// Wire token for an activity class. Mirrors the proto's `snake_case` rename,
/// and is the value the 0081 CHECK constraint accepts.
#[must_use]
pub const fn activity_class_token(class: FleetActivityClass) -> &'static str {
    match class {
        FleetActivityClass::Read => "read",
        FleetActivityClass::Write => "write",
        FleetActivityClass::Destructive => "destructive",
    }
}

/// Wire token for an activity outcome.
#[must_use]
pub const fn activity_outcome_token(outcome: FleetActivityOutcome) -> &'static str {
    match outcome {
        FleetActivityOutcome::Ok => "ok",
        FleetActivityOutcome::Denied => "denied",
        FleetActivityOutcome::Expired => "expired",
        FleetActivityOutcome::Error => "error",
    }
}

fn emit_confirm(events: &EventSink, card: &FleetConfirmRow, _state: FleetConfirmState) {
    events.emit_fleet_notification(
        ainb_hangar_proto::methods::FLEET_CONFIRM_EVENT,
        serde_json::to_value(FleetConfirmEventParams {
            confirm: wire_confirm(card),
        })
        .unwrap_or(Value::Null),
    );
}

/// Project one persisted card onto its wire shape.
#[must_use]
pub fn wire_confirm(row: &FleetConfirmRow) -> FleetConfirm {
    FleetConfirm {
        confirm_id: row.confirm_id.clone(),
        scope_key: row.scope_key.clone(),
        tool: row.tool.clone(),
        // Stored already projected; a parse failure would be a corrupt row, and
        // an empty object is the fail-closed rendering (nothing to argue with).
        arguments: serde_json::from_str(&row.arguments)
            .unwrap_or_else(|_| Value::Object(Map::new())),
        target_session_key: row.target_session_key.clone(),
        state: confirm_state(&row.state),
        created_at: row.created_at,
        expires_at: row.expires_at,
    }
}

fn confirm_state(token: &str) -> FleetConfirmState {
    match token {
        "approved" => FleetConfirmState::Approved,
        "denied" => FleetConfirmState::Denied,
        "expired" => FleetConfirmState::Expired,
        _ => FleetConfirmState::Open,
    }
}

/// Project one persisted activity row onto its wire shape.
#[must_use]
pub fn wire_activity(
    row: &ainb_hangar_store::repo::fleet_chat::FleetActivityRowRecord,
) -> FleetActivityRow {
    FleetActivityRow {
        seq: row.seq,
        id: row.id.clone(),
        scope_key: row.scope_key.clone(),
        tool: row.tool.clone(),
        class: match row.class.as_str() {
            "read" => FleetActivityClass::Read,
            "destructive" => FleetActivityClass::Destructive,
            _ => FleetActivityClass::Write,
        },
        target_session_key: row.target_session_key.clone(),
        outcome: match row.outcome.as_str() {
            "denied" => FleetActivityOutcome::Denied,
            "expired" => FleetActivityOutcome::Expired,
            "error" => FleetActivityOutcome::Error,
            _ => FleetActivityOutcome::Ok,
        },
        detail: row.detail.clone(),
        created_at: row.created_at,
    }
}
