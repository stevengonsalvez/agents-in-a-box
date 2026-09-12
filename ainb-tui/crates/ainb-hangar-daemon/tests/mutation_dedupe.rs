//! The D18 gate: every mutation is deduplicated at dispatch.
//!
//! Three properties, each a failure this ledger exists to stop:
//!
//! 1. **Replay is a read.** Every method in the committed `MUTATING_METHODS`
//!    registry is dispatched twice with one op id. The first execution is
//!    `created`; the second is `replayed` and returns the first reply verbatim.
//! 2. **An op id belongs to its principal.** A second principal presenting the
//!    same op id is `rejected{op_id_foreign}` — never a second execution under
//!    somebody else's identity.
//! 3. **A different body is a different operation.** The same op id with a
//!    changed body is `rejected{already_answered_by}`, not a silent overwrite.
//!
//! The registry walk is the point: a mutating method added without a ledger
//! entry is a hole in "generic dedupe at dispatch for every mutation", and this
//! test is what turns that hole into a failure.

use std::time::{Duration, Instant};

use ainb_hangar_daemon::events::EventBroker;
use ainb_hangar_daemon::rpc::{self, DaemonHealth, auth::Caller};
use ainb_hangar_proto::mutation::{ACK_KEY, MUTATING_METHODS};
use ainb_hangar_proto::{RpcId, RpcRequest};
use ainb_hangar_store::Store;

/// No handler in the registry should take anywhere near this. A method that
/// does is a method that would wedge a real client, and the test says so.
const PER_CALL: Duration = Duration::from_secs(30);

fn health() -> DaemonHealth {
    DaemonHealth {
        socket_path: "/tmp/mutation-dedupe.sock".to_string(),
        pid: std::process::id(),
        started_at: Instant::now(),
        version: "0.1.0".into(),
        stats: std::sync::Arc::new(ainb_hangar_daemon::health_stats::HealthStats::default()),
    }
}

fn request(method: &str, params: serde_json::Value) -> RpcRequest {
    RpcRequest {
        jsonrpc: ainb_hangar_proto::jsonrpc_version(),
        id: RpcId::Number(1),
        method: method.to_string(),
        params,
    }
}

/// The ack the daemon attached, from either half of the envelope.
///
/// A mutation that succeeded carries it beside its result; one that was refused
/// carries it in `error.data`. Both are the same contract — "what happened to
/// your op id" — so the assertions read the same either way.
fn ack(response: &serde_json::Value) -> serde_json::Value {
    if let Some(found) = response.get("result").and_then(|r| r.get(ACK_KEY)) {
        return found.clone();
    }
    response
        .get("error")
        .and_then(|e| e.get("data"))
        .and_then(|d| d.get(ACK_KEY))
        .cloned()
        .unwrap_or(serde_json::Value::Null)
}

/// Strip the ack so two replies can be compared for "is this the same answer".
fn without_ack(mut response: serde_json::Value) -> serde_json::Value {
    if let Some(result) = response.get_mut("result").and_then(|r| r.as_object_mut()) {
        result.remove(ACK_KEY);
    }
    if let Some(data) = response
        .get_mut("error")
        .and_then(|e| e.get_mut("data"))
        .and_then(|d| d.as_object_mut())
    {
        data.remove(ACK_KEY);
    }
    response
}

async fn dispatch(
    store: &Store,
    events: &ainb_hangar_daemon::events::EventSink,
    caller: &Caller,
    method: &str,
    params: serde_json::Value,
) -> serde_json::Value {
    let req = request(method, params);
    let response = tokio::time::timeout(
        PER_CALL,
        rpc::dispatch_as(store.pool(), &req, &health(), events, caller),
    )
    .await
    .unwrap_or_else(|_| panic!("{method} did not answer within {PER_CALL:?}"));
    serde_json::to_value(response).unwrap()
}

/// Criterion 1: replay every mutating method twice, one `created` and one
/// `replayed` each.
#[tokio::test]
async fn every_mutating_method_replays_exactly_once() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(dir.path()).await.unwrap();
    let broker = EventBroker::new();
    let events = broker.sink();
    let mut deduplicated = 0usize;
    let mut transient: Vec<&str> = Vec::new();
    let mut refused: Vec<&str> = Vec::new();

    for (index, entry) in MUTATING_METHODS.iter().enumerate() {
        let mut params: serde_json::Value = serde_json::from_str(entry.sample_params)
            .unwrap_or_else(|e| panic!("{}: sample is not JSON: {e}", entry.method));
        let op_id = format!("op-dedupe-{index:03}");
        params
            .as_object_mut()
            .unwrap()
            .insert("op_id".to_string(), serde_json::json!(op_id));

        let first = dispatch(
            &store,
            &events,
            &Caller::Operator,
            entry.method,
            params.clone(),
        )
        .await;
        let second = dispatch(&store, &events, &Caller::Operator, entry.method, params).await;

        if refusal(&first).is_some() {
            // A handler that REFUSED did not mutate, so the ledger keeps no row
            // and the SAME op id stays free to deliver on a later attempt. Both
            // dispatches must therefore look identical and neither may claim a
            // replay — the opposite of the dedupe contract, and correct.
            assert_eq!(
                ack(&first)["status"],
                "rejected",
                "{} refused and must say so: {first}",
                entry.method
            );
            assert!(
                ack(&second)["outcome"] != "replayed",
                "{} refused, so nothing may be replayed: {second}",
                entry.method
            );
            assert_eq!(
                without_ack(second.clone()),
                without_ack(first.clone()),
                "{} did not answer its retry the same way",
                entry.method
            );
            refused.push(entry.method);
            continue;
        }

        if is_transient(&first) {
            // The OTHER half of the contract, asserted just as hard. A store
            // fault or an unavailable service means nothing ran, so the claim
            // is abandoned and the retry the caller is about to make is a real
            // retry — pinning a transient failure to an op id forever would be
            // the worse bug. Both dispatches must therefore look identical and
            // carry no ack at all.
            assert_eq!(
                ack(&first),
                serde_json::Value::Null,
                "{} answered transiently and must not have recorded an ack: {first}",
                entry.method
            );
            assert_eq!(
                without_ack(second.clone()),
                without_ack(first.clone()),
                "{} did not answer its retry the same way",
                entry.method
            );
            transient.push(entry.method);
            continue;
        }

        assert_eq!(
            ack(&first)["outcome"],
            "created",
            "{} first dispatch must be created: {first}",
            entry.method
        );
        assert_eq!(
            ack(&second)["outcome"],
            "replayed",
            "{} second dispatch must be replayed: {second}",
            entry.method
        );
        assert_eq!(
            without_ack(second),
            without_ack(first),
            "{} replayed a DIFFERENT answer",
            entry.method
        );
        deduplicated += 1;
    }

    // The gate is the COVERAGE, not just the per-method assertion: a registry
    // that quietly shrank, or a daemon fixture in which everything answered
    // transiently, would otherwise pass with nothing proven. The floor is
    // len() - 1 because exactly ONE method answers transiently in this fixture
    // (`codex/session_ensure`, whose remote control is deliberately never
    // started). A second silent hole fails the gate.
    eprintln!(
        "deduplicated {deduplicated} of {} mutating methods; \
         transient: {transient:?}; refused: {refused:?}",
        MUTATING_METHODS.len()
    );
    // NAMED, not counted. A count leaves room for one hole to close while
    // another opens; naming the exceptions means a new one fails the gate and
    // has to be justified in the same change.
    assert_eq!(
        transient,
        vec!["codex/session_ensure"],
        "the only method allowed to answer transiently is the one whose remote \
         control this fixture deliberately never starts"
    );
    assert_eq!(
        refused,
        vec!["attention/answer"],
        "the only method allowed to refuse here is the one with no live session \
         to answer into; a refusal is not recorded, by design"
    );
    assert_eq!(
        deduplicated,
        MUTATING_METHODS.len() - transient.len() - refused.len(),
        "every other method must reach the ledger"
    );
}

/// Whether this response is a refusal the ledger deliberately did NOT record.
///
/// Keyed on the D18 reason vocabulary, not on `status: rejected` alone: a
/// RECORDED rejection (an `INVALID_PARAMS` stored as that op id's answer)
/// is also `rejected`, and it replays — which is the opposite contract.
fn refusal(response: &serde_json::Value) -> Option<String> {
    let reason = ack(response)["reason"].as_str()?.to_string();
    [
        ainb_hangar_proto::mutation::REASON_ALREADY_ANSWERED_BY,
        ainb_hangar_proto::mutation::REASON_NOT_DELIVERED,
        ainb_hangar_proto::mutation::REASON_NO_TARGET,
    ]
    .contains(&reason.as_str())
    .then_some(reason)
}

/// The two codes that mean "nothing happened, ask again": `SQLite` contention and
/// the daemon's internal catch-all (which carries "still starting" for a
/// service this test fixture deliberately never starts).
fn is_transient(response: &serde_json::Value) -> bool {
    let code = response["error"]["code"].as_i64();
    code == Some(i64::from(ainb_hangar_proto::STORE_UNAVAILABLE)) || code == Some(-32603)
}

/// Criterion 1, second half (amendment 15): a second principal replaying the
/// same op id is refused, and nothing runs.
#[tokio::test]
async fn a_second_principal_replaying_an_op_id_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(dir.path()).await.unwrap();
    let broker = EventBroker::new();
    let events = broker.sink();

    // `fleet/message_send`, and the choice is forced from both sides: the test
    // needs a method whose outcome is RECORDED (an answer with no live session
    // is a refusal, which by design leaves no row to collide with) AND one the
    // Pal credential is allowed to call at all. That is the intersection.
    //
    // Its op id rides on `request_id`, which IS the op id for that family
    // (amendment 19), so this also exercises the alias.
    let params = serde_json::json!({
        "targets": ["fleet-sample"],
        "text": "ship it",
        "request_id": "op-shared-across-principals",
    });

    let mine = dispatch(
        &store,
        &events,
        &Caller::Operator,
        ainb_hangar_proto::methods::FLEET_MESSAGE_SEND,
        params.clone(),
    )
    .await;
    assert_eq!(ack(&mine)["outcome"], "created", "{mine}");

    // Pal is a different credential and therefore a different principal.
    let theirs = dispatch(
        &store,
        &events,
        &Caller::Pal {
            scope_key: "channel:01JHARNESS".to_string(),
        },
        ainb_hangar_proto::methods::FLEET_MESSAGE_SEND,
        params,
    )
    .await;
    assert_eq!(
        theirs["error"]["code"],
        ainb_hangar_proto::mutation::MUTATION_REJECTED,
        "{theirs}"
    );
    assert_eq!(ack(&theirs)["status"], "rejected", "{theirs}");
    assert_eq!(ack(&theirs)["reason"], "op_id_foreign", "{theirs}");
}

/// Amendment 18: `adopted` requires the body to match. Two surfaces answering
/// the same question differently are not one operation, even under one op id.
#[tokio::test]
async fn the_same_op_id_with_a_different_body_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(dir.path()).await.unwrap();
    let broker = EventBroker::new();
    let events = broker.sink();
    // Same reasoning as the foreign-principal test: a recorded outcome is the
    // precondition for a body-mismatch collision.
    let method = ainb_hangar_proto::methods::HANGAR_ISSUE_UPDATE;

    let yes = serde_json::json!({
        "workspace_id": "ws-sample",
        "issue_id": "iss-sample",
        "state": "todo",
        "op_id": "op-body-fingerprint",
    });
    let no = serde_json::json!({
        "workspace_id": "ws-sample",
        "issue_id": "iss-sample",
        "state": "doing",
        "op_id": "op-body-fingerprint",
    });

    let first = dispatch(&store, &events, &Caller::Operator, method, yes).await;
    assert_eq!(ack(&first)["outcome"], "created", "{first}");

    let clash = dispatch(&store, &events, &Caller::Operator, method, no).await;
    assert_eq!(
        clash["error"]["code"],
        ainb_hangar_proto::mutation::MUTATION_REJECTED,
        "{clash}"
    );
    assert_eq!(ack(&clash)["reason"], "already_answered_by", "{clash}");
}

/// A client that sends no op id is served exactly as it was before W0-wire.
/// This is the property that keeps the N-1 leg of the skew matrix green.
#[tokio::test]
async fn a_request_without_an_op_id_is_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(dir.path()).await.unwrap();
    let broker = EventBroker::new();
    let events = broker.sink();

    let params = serde_json::json!({
        "attention_id": "att-no-op-id",
        "answer": "yes",
        "answered_by": "tui",
    });
    let first = dispatch(
        &store,
        &events,
        &Caller::Operator,
        ainb_hangar_proto::methods::ATTENTION_ANSWER,
        params.clone(),
    )
    .await;
    let second = dispatch(
        &store,
        &events,
        &Caller::Operator,
        ainb_hangar_proto::methods::ATTENTION_ANSWER,
        params,
    )
    .await;
    assert_eq!(ack(&first), serde_json::Value::Null, "{first}");
    assert_eq!(ack(&second), serde_json::Value::Null, "{second}");
    assert_eq!(
        first, second,
        "an un-deduplicated call must behave as before"
    );
}
