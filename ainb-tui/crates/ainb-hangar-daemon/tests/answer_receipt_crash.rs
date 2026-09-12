//! A crash between the claim and `send-keys` (spec D18, critique amendment 17).
//!
//! This is the case the whole receipt lifecycle exists for, and it is the one
//! case that cannot be simulated by returning an error: an error is RECORDED as
//! that op id's answer, whereas a killed daemon records nothing. So the test
//! parks the answer at its `writing` boundary, aborts the task at exactly that
//! instant — leaving the durable state a SIGKILL leaves — and then boots a
//! fresh daemon over the same database.
//!
//! ```text
//! claim ──▶ receipt=claimed (same txn as the flip)
//!        ──▶ receipt=writing   ◀── committed BEFORE any byte reaches the PTY
//!        ──▶ ✖ daemon dies here
//!  boot  ──▶ writing ──▶ unknown{effects_ambiguous}
//!        ──▶ attention row kind=delivery_unconfirmed, naming the answer
//!  retry ──▶ replayed, status unknown, receipt unknown  (never a second type)
//! ```
//!
//! Single-test binary: `$AINB_BIN` and the write-boundary stall are
//! process-global, and no sibling test in this file may observe either.

use std::time::{Duration, Instant};

use ainb_hangar_daemon::events::EventBroker;
use ainb_hangar_daemon::rpc::{self, DaemonHealth, auth::Caller};
use ainb_hangar_proto::mutation::ACK_KEY;
use ainb_hangar_proto::{RpcId, RpcRequest, methods};
use ainb_hangar_store::Store;
use ainb_hangar_store::repo::attention::{AttentionKind, AttentionRepo, NewAttention};
use ainb_hangar_store::repo::mutation_ledger::{LedgerKey, MutationLedgerRepo};

const OP_ID: &str = "op-crash-between-claim-and-send";

/// The write-boundary stall, the post-delivery stall and `$AINB_BIN` are all
/// process-global, so the tests in this file must not overlap: one arming a
/// stall the next is waiting past is indistinguishable from a hang.
static SEAM: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
const SESSION: &str = "sess-crash";
const CWD: &str = "/work/crash";

fn health() -> DaemonHealth {
    DaemonHealth {
        socket_path: "/tmp/answer-receipt-crash.sock".to_string(),
        pid: std::process::id(),
        started_at: Instant::now(),
        version: "0.1.0".into(),
        stats: std::sync::Arc::new(ainb_hangar_daemon::health_stats::HealthStats::default()),
    }
}

/// A fake `ainb list --format json` that reports one running session in `CWD`,
/// so the C1 target resolution finds an unambiguous target and the answer
/// reaches its claim. Discovery shells out by design; this is the seam it
/// documents (`$AINB_BIN`).
fn install_fake_ainb(dir: &std::path::Path) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt as _;
    let rows = serde_json::json!([{
        "session_id": SESSION,
        "tmux_session_name": "crash-pane",
        "workspace_name": "crash",
        "worktree_path": CWD,
        "created_at": "2026-09-12T00:00:00Z",
        "is_running": true,
        "claude_active": true,
    }])
    .to_string();
    let script = dir.join("fake-ainb");
    std::fs::write(&script, format!("#!/bin/sh\ncat <<'JSON'\n{rows}\nJSON\n")).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    script
}

async fn seed_open_row_as(pool: &sqlx::SqlitePool, attention_id: &str) {
    AttentionRepo::insert(
        pool,
        &NewAttention {
            id: attention_id.into(),
            session_id: SESSION.into(),
            cwd: CWD.into(),
            workspace_id: None,
            kind: AttentionKind::Approval,
            payload: format!(r#"{{"kind":"APPROVAL","id":"{attention_id}"}}"#),
            degraded: false,
            created_at: 1_000,
            raise_transcript: None,
            channels: ainb_hangar_core::channel::ChannelSet::NONE,
        },
    )
    .await
    .unwrap();
}

fn answer_request_for(attention_id: &str, op_id: &str) -> RpcRequest {
    RpcRequest {
        jsonrpc: ainb_hangar_proto::jsonrpc_version(),
        id: RpcId::Number(1),
        method: methods::ATTENTION_ANSWER.to_string(),
        params: serde_json::json!({
            "attention_id": attention_id,
            "answer": "approve",
            "answered_by": "tui",
            "op_id": op_id,
        }),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_crash_between_claim_and_send_keys_is_surfaced_not_retried() {
    const ATT: &str = "att-crash-writing";
    const OP: &str = OP_ID;
    let _seam = SEAM.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let fake = install_fake_ainb(dir.path());
    // Single-test binary: no sibling test can observe this mutation.
    std::env::set_var("AINB_BIN", &fake);

    let store = Store::open_in(dir.path()).await.unwrap();
    seed_open_row_as(store.pool(), ATT).await;
    let broker = EventBroker::new();
    let events = broker.sink();

    // ── the crash ───────────────────────────────────────────────────────────
    ainb_hangar_daemon::answer::set_stall_at_write_boundary_for_test(true);
    let pool = store.pool().clone();
    let task = tokio::spawn(async move {
        let broker = EventBroker::new();
        let sink = broker.sink();
        rpc::dispatch_as(
            &pool,
            &answer_request_for(ATT, OP),
            &health(),
            &sink,
            &Caller::Operator,
        )
        .await
    });

    let key = LedgerKey::local(OP);
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let row = MutationLedgerRepo::get(store.pool(), &key).await.unwrap();
        if row.as_ref().and_then(|r| r.receipt_state.as_deref()) == Some("writing") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the answer never reached its writing boundary: {row:?}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    // The daemon dies HERE: no reply is recorded, exactly as a SIGKILL leaves it.
    task.abort();
    let _ = task.await;
    ainb_hangar_daemon::answer::set_stall_at_write_boundary_for_test(false);

    let mid = MutationLedgerRepo::get(store.pool(), &key).await.unwrap().unwrap();
    assert_eq!(mid.status, "in_flight", "a killed handler records no reply");
    assert_eq!(mid.receipt_state.as_deref(), Some("writing"));
    let claimed = AttentionRepo::get(store.pool(), ATT).await.unwrap().unwrap();
    assert_eq!(claimed.state, "answered", "the claim itself did commit");

    // ── the restart ─────────────────────────────────────────────────────────
    drop(store);
    let store = Store::open_in(dir.path()).await.unwrap();
    let report = ainb_hangar_daemon::receipt_sweep::run(store.pool()).await.unwrap();
    assert_eq!(report.unconfirmed, 1, "{report:?}");

    let rows = AttentionRepo::list_fleet(store.pool()).await.unwrap();
    let unconfirmed: Vec<_> =
        rows.iter().filter(|r| r.kind == AttentionKind::DeliveryUnconfirmed).collect();
    assert_eq!(
        unconfirmed.len(),
        1,
        "a mid-write crash must leave exactly one delivery_unconfirmed row: {rows:?}"
    );
    let payload: serde_json::Value = serde_json::from_str(&unconfirmed[0].payload).unwrap();
    assert_eq!(
        payload["context"]["answer"], "approve",
        "the row must NAME the answer whose delivery is unconfirmed: {payload}"
    );
    assert_eq!(payload["op_id"], OP);
    assert_eq!(
        unconfirmed[0].state, "open",
        "only an operator closes it — the daemon never does"
    );

    // ── the retry ───────────────────────────────────────────────────────────
    let response = rpc::dispatch_as(
        store.pool(),
        &answer_request_for(ATT, OP),
        &health(),
        &events,
        &Caller::Operator,
    )
    .await;
    let response = serde_json::to_value(response).unwrap();
    let ack = &response["error"]["data"][ACK_KEY];
    assert_eq!(
        response["error"]["code"],
        ainb_hangar_proto::mutation::MUTATION_UNKNOWN,
        "{response}"
    );
    assert_eq!(ack["outcome"], "replayed", "{response}");
    assert_eq!(ack["status"], "unknown", "{response}");
    assert_eq!(ack["receipt"], "unknown", "{response}");
    assert_eq!(ack["reason"], "effects_ambiguous", "{response}");

    // And the retry changed nothing: no second answer, no second row.
    let rows = AttentionRepo::list_fleet(store.pool()).await.unwrap();
    assert_eq!(
        rows.iter().filter(|r| r.kind == AttentionKind::DeliveryUnconfirmed).count(),
        1,
        "a retry must not raise a second unconfirmed row"
    );
}

/// The OTHER half of the boot sweep, and the branch a bug had made dead code:
/// a receipt still `claimed` never committed `writing`, so provably no byte
/// reached the PTY — the answer was lost, and the row has to go back on the
/// operator's board.
///
/// `AttentionRepo::reopen` scopes its revert to ONE claim with
/// `answered_by = ? AND answered_at = ?`. The sweep used to bind its own boot
/// clock as the second value, so the UPDATE matched zero rows every time: the
/// request left the inbox while the agent stayed blocked, which is exactly the
/// "close it quietly" outcome the sweep exists to avoid.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_claimed_receipt_at_boot_reopens_its_attention_row() {
    const ATT: &str = "att-crash-claimed";
    const OP: &str = "op-claimed-at-boot";
    let _seam = SEAM.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(dir.path()).await.unwrap();
    seed_open_row_as(store.pool(), ATT).await;

    // The durable state a daemon killed between the claim commit and the
    // `writing` commit leaves behind: the row flipped, the ledger claimed, and
    // no reply recorded.
    let key = LedgerKey::local(OP);
    let fingerprint = MutationLedgerRepo::fingerprint(
        methods::ATTENTION_ANSWER,
        &serde_json::json!({ "attention_id": ATT, "answer": "approve" }),
    );
    MutationLedgerRepo::claim(
        store.pool(),
        &key,
        methods::ATTENTION_ANSWER,
        &fingerprint,
        "receipt",
        5_000,
    )
    .await
    .unwrap();
    MutationLedgerRepo::set_receipt(
        store.pool(),
        &key,
        "claimed",
        Some(&serde_json::json!({ "attention_id": ATT }).to_string()),
        5_000,
    )
    .await
    .unwrap();
    assert_eq!(
        AttentionRepo::mark_answered_if_open(store.pool(), ATT, "tui", "approve", 5_000)
            .await
            .unwrap(),
        1
    );

    // Boot, a good while later: the sweep's own clock is NOT the row's stamp.
    let report = ainb_hangar_daemon::receipt_sweep::run(store.pool()).await.unwrap();
    assert_eq!(report.reopened, 1, "{report:?}");
    assert_eq!(report.unconfirmed, 0, "nothing was mid-write: {report:?}");

    let row = AttentionRepo::get(store.pool(), ATT).await.unwrap().unwrap();
    assert_eq!(row.state, "open", "a lost answer must go back on the board");
    assert!(row.answered_by.is_none());
    assert!(row.answer.is_none());
    assert!(
        row.version >= 3,
        "the fence advanced on the flip AND on the reopen: {row:?}"
    );

    // And the ledger row is resolved, so a retry is never re-executed blindly.
    let ledger = MutationLedgerRepo::get(store.pool(), &key).await.unwrap().unwrap();
    assert_eq!(ledger.status, "unknown");
    assert!(
        ainb_hangar_daemon::receipt_sweep::run(store.pool()).await.unwrap()
            == ainb_hangar_daemon::receipt_sweep::SweepReport::default(),
        "the sweep must be idempotent"
    );
}

/// The narrower crash window, and the one a sweep can get WRONG rather than
/// merely miss: `delivered` is committed by the answer path, and the reply is
/// recorded by the dispatcher several awaits later. A daemon killed in between
/// leaves `status = in_flight` beside `receipt_state = delivered` — an outcome
/// that is known, under a status that says it is not.
///
/// Routing that through the generic arm overwrote a CONFIRMED delivery with
/// `unknown` and told the operator the daemon "stopped before this mutation
/// answered", which is the one thing it demonstrably did not do.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_crash_after_delivery_keeps_the_outcome_its_receipt_recorded() {
    const ATT: &str = "att-crash-delivered";
    const OP: &str = "op-crash-after-delivery";
    let _seam = SEAM.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let fake = install_fake_ainb(dir.path());
    std::env::set_var("AINB_BIN", &fake);

    let store = Store::open_in(dir.path()).await.unwrap();
    seed_open_row_as(store.pool(), ATT).await;
    let broker = EventBroker::new();

    ainb_hangar_daemon::answer::set_forced_delivery_for_test(true);
    ainb_hangar_daemon::answer::set_stall_after_delivery_for_test(true);
    let pool = store.pool().clone();
    let task = tokio::spawn(async move {
        let broker = EventBroker::new();
        let sink = broker.sink();
        rpc::dispatch_as(
            &pool,
            &answer_request_for(ATT, OP),
            &health(),
            &sink,
            &Caller::Operator,
        )
        .await
    });

    let key = LedgerKey::local(OP);
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let row = MutationLedgerRepo::get(store.pool(), &key).await.unwrap();
        if row.as_ref().and_then(|r| r.receipt_state.as_deref()) == Some("delivered") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the answer never reached a delivered receipt: {row:?}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    // The daemon dies HERE: the delivery is durable, the reply is not.
    task.abort();
    let _ = task.await;
    ainb_hangar_daemon::answer::set_stall_after_delivery_for_test(false);
    ainb_hangar_daemon::answer::set_forced_delivery_for_test(false);

    let mid = MutationLedgerRepo::get(store.pool(), &key).await.unwrap().unwrap();
    assert_eq!(mid.status, "in_flight", "no reply was recorded");
    assert_eq!(mid.receipt_state.as_deref(), Some("delivered"));

    drop(store);
    let store = Store::open_in(dir.path()).await.unwrap();
    let report = ainb_hangar_daemon::receipt_sweep::run(store.pool()).await.unwrap();
    assert_eq!(report.settled, 1, "{report:?}");
    assert_eq!(report.unconfirmed, 0, "nothing was mid-write: {report:?}");
    assert_eq!(report.ambiguous, 0, "the outcome was known: {report:?}");

    let after = MutationLedgerRepo::get(store.pool(), &key).await.unwrap().unwrap();
    assert_eq!(
        after.status, "accepted",
        "a confirmed delivery must not be rewritten as unknown: {after:?}"
    );
    assert_eq!(
        after.receipt_state.as_deref(),
        Some("delivered"),
        "the receipt is the evidence and must survive the sweep: {after:?}"
    );

    // No operator alert: nothing is unconfirmed.
    let rows = AttentionRepo::list_fleet(store.pool()).await.unwrap();
    assert!(
        rows.iter().all(|r| r.kind != AttentionKind::DeliveryUnconfirmed),
        "a delivered answer must not raise delivery_unconfirmed: {rows:?}"
    );

    // A retry replays the KNOWN outcome. The body is gone — the daemon died
    // before storing it — but the status axis still says the answer was
    // applied, and the receipt says how. `op_expired` here would tell a client
    // its delivered answer might never have run at all.
    let response = rpc::dispatch_as(
        store.pool(),
        &answer_request_for(ATT, OP),
        &health(),
        &broker.sink(),
        &Caller::Operator,
    )
    .await;
    let response = serde_json::to_value(response).unwrap();
    let ack = &response["error"]["data"][ainb_hangar_proto::mutation::ACK_KEY];
    assert_eq!(ack["outcome"], "replayed", "{response}");
    assert_eq!(ack["status"], "accepted", "{response}");
    assert_eq!(ack["receipt"], "delivered", "{response}");
    assert_eq!(ack["reason"], "reply_lost", "{response}");

    // And nothing was typed a second time: the attention row is untouched.
    let row = AttentionRepo::get(store.pool(), ATT).await.unwrap().unwrap();
    assert_eq!(row.state, "answered", "{row:?}");
}
