//! Generic mutation dedupe at dispatch, and the receipt claim underneath it
//! (spec D18, critique amendments 15-19).
//!
//! Every mutating method passes through here on its way to its handler, so the
//! guarantee is a property of the DISPATCHER rather than of ~96 hand-written
//! transactions:
//!
//! ```text
//! request ──▶ op id? ─no─▶ handler (today's behaviour, no ledger row)
//!               │yes
//!               ▼
//!          claim (host, principal, op_id)
//!               ├─ foreign      ──▶ rejected{op_id_foreign}
//!               ├─ other body   ──▶ rejected{already_answered_by}
//!               ├─ expired      ──▶ unknown{op_expired}
//!               ├─ in flight    ──▶ unknown{effects_ambiguous} + receipt state
//!               ├─ committed    ──▶ the stored reply, verbatim
//!               └─ fresh        ──▶ handler ──▶ record the reply
//! ```
//!
//! ## Why the reply is stored and not recomputed
//!
//! The retry exists because the FIRST reply was lost. Recomputing it would mean
//! running the handler again, which is the thing the ledger exists to prevent.
//! So the serialized reply is the ledger's payload, and a replay is a read.
//!
//! ## Which failures are recorded
//!
//! Everything deterministic, including `INVALID_PARAMS`: the same op id with
//! the same body gets the same answer, and a client that fixes its params is
//! sending a different body, which the fingerprint catches. Only `SQLite`
//! contention and internal faults ABANDON the claim, because those are exactly
//! the cases where the caller is supposed to retry and get a different answer.

use ainb_hangar_proto::mutation::{
    ACK_KEY, MutatingMethod, MutationAck, MutationStatus, MutationTier, REASON_ALREADY_ANSWERED_BY,
    REASON_EFFECTS_AMBIGUOUS, REASON_OP_EXPIRED, REASON_OP_ID_FOREIGN, ReceiptState,
};
use ainb_hangar_proto::{RpcError, RpcRequest, methods};
use ainb_hangar_store::repo::mutation_ledger::{
    ClaimOutcome, LOCAL_PRINCIPAL, LedgerKey, LedgerRow, MutationLedgerRepo, TIER_DEDUPE,
    TIER_RECEIPT,
};
use serde_json::Value;
use sqlx::SqlitePool;

use super::auth::Caller;

// The op-id-bearing mutation the current task is executing.
//
// A task-local rather than a threaded parameter because the receipt tier needs
// the ledger key ~six frames below the dispatcher, inside handlers whose
// signatures are shared with every non-mutating method. Threading it would mean
// an `Option<&LedgerKey>` on dozens of functions that can never use one. One
// request is handled start to finish on one task, so the scope is exact.
tokio::task_local! {
    static ACTIVE: MutationContext;
}

/// What the dispatcher claimed for the request the current task is serving.
#[derive(Debug, Clone)]
pub struct MutationContext {
    /// The ledger row this request owns.
    pub key: LedgerKey,
    /// Which guarantee the method gets.
    pub tier: MutationTier,
}

/// The mutation the current task owns, if any.
///
/// `None` for a read, for a mutation with no op id (a pre-W0-wire client), and
/// for any call made outside the socket dispatcher.
#[must_use]
pub fn active() -> Option<MutationContext> {
    ACTIVE.try_with(Clone::clone).ok()
}

/// Move the CURRENT task's receipt on, if it owns one.
///
/// The tier-2 write boundary for handlers that do not own their own store
/// transaction. Best-effort on a store fault: a receipt the daemon could not
/// advance is one the boot sweep resolves as `unknown`, which is the honest
/// answer and strictly better than failing a delivery that already happened.
pub async fn mark_active_receipt(
    pool: &SqlitePool,
    state: ReceiptState,
    detail: Option<&str>,
    now_ms: i64,
) {
    let Some(context) = active() else {
        return;
    };
    if !matches!(context.tier, MutationTier::Receipt) {
        return;
    }
    if let Err(e) =
        MutationLedgerRepo::set_receipt(pool, &context.key, state.token(), detail, now_ms).await
    {
        tracing::warn!(
            error = %e,
            op_id = %context.key.op_id,
            state = state.token(),
            "could not advance the mutation receipt"
        );
    }
}

/// The reply shape stored in the ledger.
///
/// Tagged rather than "the result value or null", because a rejection is a
/// legitimate terminal outcome that a replay has to reproduce exactly — a
/// client retrying a refused mutation must get the refusal again, not a second
/// execution that might now succeed.
mod stored {
    use super::{RpcError, Value};

    pub const OK: &str = "ok";
    pub const ERR: &str = "err";

    pub fn encode_ok(value: &Value) -> String {
        serde_json::json!({ OK: value }).to_string()
    }

    pub fn encode_err(error: &RpcError) -> String {
        serde_json::json!({
            ERR: { "code": error.code, "message": error.message, "data": error.data }
        })
        .to_string()
    }

    pub fn decode(raw: &str) -> Option<Result<Value, RpcError>> {
        let parsed: Value = serde_json::from_str(raw).ok()?;
        let object = parsed.as_object()?;
        if let Some(value) = object.get(OK) {
            return Some(Ok(value.clone()));
        }
        let err = object.get(ERR)?.as_object()?;
        Some(Err(RpcError {
            code: i32::try_from(err.get("code")?.as_i64()?).ok()?,
            message: err.get("message")?.as_str()?.to_string(),
            data: err.get("data").cloned().filter(|d| !d.is_null()),
        }))
    }
}

/// The principal this caller's op ids are keyed under (amendment 15).
///
/// `local` for the operator on the unix leg. Pal gets its OWN principal
/// because it is a different actor holding a different credential: an op id it
/// mints must never reach the operator's row, and vice versa. R1 adds the
/// `device:<id>` arm when a paired device can dial in.
#[must_use]
pub fn principal_of(caller: &Caller) -> String {
    match caller {
        Caller::Operator => LOCAL_PRINCIPAL.to_string(),
        Caller::Pal { scope_key } => format!("pal:{scope_key}"),
    }
}

/// The op id for this request, accepting each family's existing spelling.
///
/// Amendment 19: `FleetActionParams::request_id` IS the op id for the fleet
/// family and `fleet_action_receipt` IS its ledger. W0-wire renames nothing on
/// the wire, so the envelope's `op_id` is an ALIAS — a client that already
/// sends `request_id` is already deduplicated, without changing a byte.
#[must_use]
pub fn op_id_of(method: &str, params: &Value) -> Option<String> {
    let object = params.as_object()?;
    let named = |key: &str| {
        object
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    named("op_id").or_else(|| match method {
        methods::FLEET_ACTION | methods::FLEET_MESSAGE_SEND | methods::FLEET_START => {
            named("request_id")
        }
        methods::FLEET_BROADCAST => named("idempotency_key"),
        _ => None,
    })
}

/// Attach the ack to an object-shaped result.
///
/// Additive, under one reserved key: an N-1 client deserializes its own result
/// type and never sees it, which is what keeps the skew matrix green while the
/// ledger rolls out. A non-object result (no current method has one) is
/// returned untouched rather than wrapped, because wrapping would be the
/// breaking change the whole design avoids.
fn with_ack(mut value: Value, ack: &MutationAck) -> Value {
    if let Some(object) = value.as_object_mut() {
        if let Ok(encoded) = serde_json::to_value(ack) {
            object.insert(ACK_KEY.to_string(), encoded);
        }
    }
    value
}

/// An error envelope carrying the ack in `data`.
fn ack_error(code: i32, message: String, ack: &MutationAck) -> RpcError {
    with_error_ack(
        RpcError {
            code,
            message,
            data: None,
        },
        ack,
    )
}

/// Attach the ack to an error WITHOUT losing what the handler already put in
/// `data`.
///
/// A refusal's own detail is what a UI renders; replacing it with the ledger's
/// bookkeeping would trade a message the operator can act on for one only the
/// protocol cares about. So the ack is merged under the same reserved key it
/// uses on the success side, and a non-object `data` (no current handler
/// produces one) is left exactly as it is.
fn with_error_ack(mut error: RpcError, ack: &MutationAck) -> RpcError {
    let Ok(encoded) = serde_json::to_value(ack) else {
        return error;
    };
    match error.data.take() {
        None => {
            error.data = Some(serde_json::json!({ ACK_KEY: encoded }));
        }
        Some(Value::Object(mut map)) => {
            map.insert(ACK_KEY.to_string(), encoded);
            error.data = Some(Value::Object(map));
        }
        Some(other) => error.data = Some(other),
    }
    error
}

/// Map a store fault onto the wire, matching the dispatcher's own mapping.
fn store_error(error: &sqlx::Error) -> RpcError {
    super::store_err(error)
}

/// Whether an error means "nothing happened, ask again" rather than "this is
/// your answer".
///
/// Only two codes qualify. Recording a lock-contention failure would pin a
/// transient fault to an op id forever; recording an `INVALID_PARAMS` is
/// correct, because the same body really does deserve the same answer.
const fn is_transient(code: i32) -> bool {
    code == super::STORE_UNAVAILABLE || code == super::INTERNAL_ERROR
}

/// The ledger tier token for a registry entry.
const fn tier_token(tier: MutationTier) -> &'static str {
    match tier {
        MutationTier::Dedupe => TIER_DEDUPE,
        MutationTier::Receipt => TIER_RECEIPT,
    }
}

/// What the claim decided, for a caller that is not going to run the handler.
fn settled(entry: &MutatingMethod, outcome: &ClaimOutcome) -> Option<Result<Value, RpcError>> {
    match outcome {
        ClaimOutcome::Fresh => None,
        ClaimOutcome::Foreign { principal } => {
            let ack = MutationAck::rejected(REASON_OP_ID_FOREIGN);
            Some(Err(ack_error(
                ainb_hangar_proto::mutation::MUTATION_REJECTED,
                format!(
                    "this op id already belongs to {principal}; mint a fresh one for {}",
                    entry.method
                ),
                &ack,
            )))
        }
        ClaimOutcome::BodyMismatch(row) => {
            let ack = MutationAck::rejected(REASON_ALREADY_ANSWERED_BY);
            Some(Err(ack_error(
                ainb_hangar_proto::mutation::MUTATION_REJECTED,
                format!(
                    "op id {} already committed a different {} body",
                    row.key.op_id, row.method
                ),
                &ack,
            )))
        }
        ClaimOutcome::Expired(_) => {
            let ack = MutationAck::unknown(REASON_OP_EXPIRED, None);
            Some(Err(ack_error(
                ainb_hangar_proto::mutation::MUTATION_UNKNOWN,
                "this op id has aged out of the ledger; could not confirm, check the session"
                    .to_string(),
                &ack,
            )))
        }
        ClaimOutcome::InFlight(row) => {
            let receipt = row.receipt_state.as_deref().and_then(ReceiptState::from_token);
            // `replayed`, and the receipt state, is exactly what D18 says a
            // retry against a `claimed` or `writing` receipt gets: this op id
            // IS this caller's, and an earlier attempt of it is in flight. The
            // status is `unknown` because the EFFECT is unknown, not because
            // the operation is a stranger.
            let ack = MutationAck {
                outcome: Some(ainb_hangar_proto::mutation::MutationOutcome::Replayed),
                status: MutationStatus::Unknown,
                reason: Some(REASON_EFFECTS_AMBIGUOUS.to_string()),
                receipt,
            };
            Some(Err(ack_error(
                ainb_hangar_proto::mutation::MUTATION_UNKNOWN,
                "an earlier attempt at this op id has not answered; could not confirm, \
                 check the session"
                    .to_string(),
                &ack,
            )))
        }
        ClaimOutcome::Replay(row) => Some(replay(row)),
    }
}

/// Serve a committed row: the stored reply, tagged `replayed`.
///
/// An `unknown` row is answered from its STATUS, not from its (absent) reply.
/// That row is a mutation the boot sweep resolved after a crash: there is no
/// reply because nothing ever answered, and reporting it as an aged-out op id
/// would tell the client the wrong thing about a very specific, very bad
/// moment — bytes that may or may not have reached a terminal.
fn replay(row: &LedgerRow) -> Result<Value, RpcError> {
    let receipt = row.receipt_state.as_deref().and_then(ReceiptState::from_token);
    if row.status == ainb_hangar_store::repo::mutation_ledger::STATUS_UNKNOWN {
        let reason = row.reason.as_deref().unwrap_or(REASON_EFFECTS_AMBIGUOUS);
        return Err(ack_error(
            ainb_hangar_proto::mutation::MUTATION_UNKNOWN,
            "this mutation's effect could not be established; could not confirm, \
             check the session"
                .to_string(),
            &MutationAck {
                outcome: Some(ainb_hangar_proto::mutation::MutationOutcome::Replayed),
                status: MutationStatus::Unknown,
                reason: Some(reason.to_string()),
                receipt,
            },
        ));
    }
    // A stored rejection IS this op id's answer, produced by an earlier attempt
    // of this caller's own operation, so the replay names the attempt it came
    // from. That is the opposite case from a refusal the ledger makes before
    // any handler runs, which names no outcome at all.
    let ack = if row.status == ainb_hangar_store::repo::mutation_ledger::STATUS_REJECTED {
        MutationAck::refused(
            ainb_hangar_proto::mutation::MutationOutcome::Replayed,
            row.reason.as_deref().unwrap_or(REASON_ALREADY_ANSWERED_BY),
        )
    } else {
        MutationAck::replayed(receipt)
    };
    match row.reply.as_deref().and_then(stored::decode) {
        Some(Ok(value)) => Ok(with_ack(value, &ack)),
        Some(Err(error)) => Err(with_error_ack(error, &ack)),
        None => Err(ack_error(
            ainb_hangar_proto::mutation::MUTATION_UNKNOWN,
            "this op id committed but its reply is no longer stored; could not confirm, \
             check the session"
                .to_string(),
            &MutationAck::unknown(REASON_OP_EXPIRED, receipt),
        )),
    }
}

/// Run `handler` under the mutation ledger, or answer from it.
///
/// `handler` is the dispatcher's own `handle` future factory. It is only
/// awaited on a fresh claim; every other outcome answers without executing.
///
/// # Errors
///
/// Whatever the handler returns, or the ledger's own refusal.
pub async fn guard<F, Fut>(
    pool: &SqlitePool,
    req: &RpcRequest,
    caller: &Caller,
    now_ms: i64,
    handler: F,
) -> Result<Value, RpcError>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<Value, RpcError>>,
{
    let Some(entry) = ainb_hangar_proto::mutation::mutating(&req.method) else {
        return handler().await;
    };
    let Some(op_id) = op_id_of(&req.method, &req.params) else {
        // No op id: exactly today's behaviour. The ledger is opt-in on the
        // client's side precisely so an N-1 client keeps working unchanged.
        return handler().await;
    };

    let key = LedgerKey {
        host_id: ainb_hangar_store::repo::mutation_ledger::LOCAL_HOST_ID.to_string(),
        principal: principal_of(caller),
        op_id,
    };
    let fingerprint = MutationLedgerRepo::fingerprint(&req.method, &req.params);
    let tier = tier_token(entry.tier);

    let outcome = MutationLedgerRepo::claim(pool, &key, &req.method, &fingerprint, tier, now_ms)
        .await
        .map_err(|e| store_error(&e))?;
    if let Some(answer) = settled(entry, &outcome) {
        return answer;
    }

    let context = MutationContext {
        key: key.clone(),
        tier: entry.tier,
    };
    let result = ACTIVE.scope(context, handler()).await;
    // A handler can run for seconds (a verified tmux send waits on the composer
    // ingest gate), so the reply is stamped with the clock NOW rather than with
    // the claim's. `created_at` is what retention keys on and stays put;
    // `updated_at` is the only honest answer to "when did this op id settle".
    let settled_ms =
        ainb_hangar_core::clock::HangarClock::now_ms(&ainb_hangar_core::clock::SystemClock);

    match &result {
        Ok(value) => {
            let encoded = stored::encode_ok(value);
            MutationLedgerRepo::record_reply(
                pool,
                &key,
                ainb_hangar_store::repo::mutation_ledger::STATUS_ACCEPTED,
                None,
                Some(&encoded),
                settled_ms,
            )
            .await
            .map_err(|e| store_error(&e))?;
            let receipt = MutationLedgerRepo::get(pool, &key)
                .await
                .map_err(|e| store_error(&e))?
                .and_then(|row| row.receipt_state)
                .as_deref()
                .and_then(ReceiptState::from_token);
            let ack = MutationAck {
                outcome: Some(ainb_hangar_proto::mutation::MutationOutcome::Created),
                status: MutationStatus::Accepted,
                reason: None,
                receipt,
            };
            Ok(with_ack(value.clone(), &ack))
        }
        Err(error) if is_transient(error.code) => {
            // Nothing ran to completion, so the retry the caller is about to
            // make should be a real retry — but only if nothing can have left.
            // `abandon` refuses to drop a row whose receipt reached `writing`
            // (bytes may already be in a terminal), and that row is left for the
            // boot sweep to resolve as `unknown` instead.
            match MutationLedgerRepo::abandon(pool, &key).await {
                Ok(0) => tracing::warn!(
                    op_id = %key.op_id,
                    method = %req.method,
                    "a mutation failed transiently after its writing boundary; \
                     the claim is kept and resolves as unknown"
                ),
                Ok(_) | Err(_) => {}
            }
            result
        }
        Err(error) => {
            let encoded = stored::encode_err(error);
            MutationLedgerRepo::record_reply(
                pool,
                &key,
                ainb_hangar_store::repo::mutation_ledger::STATUS_REJECTED,
                Some(&error.code.to_string()),
                Some(&encoded),
                settled_ms,
            )
            .await
            .map_err(|e| store_error(&e))?;
            // The refusal is this op id's answer, and it is answered for the
            // first time: `created` names WHICH attempt produced it, so a
            // client can tell its own refusal from a replayed one.
            Err(with_error_ack(
                error.clone(),
                &MutationAck {
                    outcome: Some(ainb_hangar_proto::mutation::MutationOutcome::Created),
                    status: MutationStatus::Rejected,
                    reason: Some(error.code.to_string()),
                    receipt: None,
                },
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The fleet family's existing spelling is accepted as the op id, because
    /// amendment 19 says W0-wire renames nothing on the wire.
    #[test]
    fn the_legacy_request_id_is_an_op_id_alias() {
        let params = serde_json::json!({ "request_id": "action-request-001" });
        assert_eq!(
            op_id_of(methods::FLEET_ACTION, &params).as_deref(),
            Some("action-request-001")
        );
        let params = serde_json::json!({ "idempotency_key": "broadcast-001" });
        assert_eq!(
            op_id_of(methods::FLEET_BROADCAST, &params).as_deref(),
            Some("broadcast-001")
        );
        // The alias is per-family: a hangar method's `request_id` (there is no
        // such field) must not be invented into an op id.
        let params = serde_json::json!({ "request_id": "nope" });
        assert_eq!(op_id_of(methods::HANGAR_ISSUE_UPDATE, &params), None);
    }

    /// An explicit `op_id` always wins over the legacy alias.
    #[test]
    fn an_explicit_op_id_wins() {
        let params = serde_json::json!({ "op_id": "abc", "request_id": "xyz" });
        assert_eq!(
            op_id_of(methods::FLEET_ACTION, &params).as_deref(),
            Some("abc")
        );
    }

    /// Blank and absent are the same thing: no dedupe, today's behaviour.
    #[test]
    fn a_blank_op_id_is_no_op_id() {
        assert_eq!(
            op_id_of(
                methods::ATTENTION_ANSWER,
                &serde_json::json!({ "op_id": "   " })
            ),
            None
        );
        assert_eq!(
            op_id_of(methods::ATTENTION_ANSWER, &serde_json::json!({})),
            None
        );
        assert_eq!(
            op_id_of(methods::ATTENTION_ANSWER, &serde_json::Value::Null),
            None
        );
    }

    /// Pal is a different principal, so its op ids can never reach the
    /// operator's ledger rows.
    #[test]
    fn pal_has_its_own_principal() {
        assert_eq!(principal_of(&Caller::Operator), "local");
        assert_eq!(
            principal_of(&Caller::Pal {
                scope_key: "channel:01J0".to_string()
            }),
            "pal:channel:01J0"
        );
    }

    /// The stored reply round-trips both terminal shapes, so a replay of a
    /// refusal is the same refusal.
    #[test]
    fn the_stored_reply_round_trips_both_outcomes() {
        let ok = serde_json::json!({ "outcome": "delivered" });
        assert_eq!(
            stored::decode(&stored::encode_ok(&ok)).unwrap().unwrap(),
            ok
        );
        let err = RpcError {
            code: -32602,
            message: "bad".to_string(),
            data: Some(serde_json::json!({ "field": "answer" })),
        };
        let back = stored::decode(&stored::encode_err(&err)).unwrap().unwrap_err();
        assert_eq!(back, err);
    }

    /// The ack rides beside the result, never instead of it: an N-1 client's
    /// own fields are untouched.
    #[test]
    fn the_ack_is_additive() {
        let value = with_ack(
            serde_json::json!({ "outcome": "delivered", "via": "tmux (s1)" }),
            &MutationAck::created(),
        );
        assert_eq!(value["outcome"], "delivered");
        assert_eq!(value["via"], "tmux (s1)");
        assert_eq!(value[ACK_KEY]["outcome"], "created");
        assert_eq!(value[ACK_KEY]["status"], "accepted");
    }

    /// Only the two retry-shaped codes abandon a claim.
    #[test]
    fn transient_codes_are_exactly_the_retryable_two() {
        assert!(is_transient(super::super::STORE_UNAVAILABLE));
        assert!(is_transient(super::super::INTERNAL_ERROR));
        for code in [-32602, -32601, -32000, -32008, -32009] {
            assert!(
                !is_transient(code),
                "{code} must be recorded, not abandoned"
            );
        }
    }
}
