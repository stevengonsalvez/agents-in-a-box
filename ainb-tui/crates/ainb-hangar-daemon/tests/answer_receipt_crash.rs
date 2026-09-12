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

async fn seed_open_row(pool: &sqlx::SqlitePool) {
    AttentionRepo::insert(
        pool,
        &NewAttention {
            id: "att-crash".into(),
            session_id: SESSION.into(),
            cwd: CWD.into(),
            workspace_id: None,
            kind: AttentionKind::Approval,
            payload: r#"{"kind":"APPROVAL","id":"att-crash"}"#.into(),
            degraded: false,
            created_at: 1_000,
            raise_transcript: None,
            channels: ainb_hangar_core::channel::ChannelSet::NONE,
        },
    )
    .await
    .unwrap();
}

fn answer_request() -> RpcRequest {
    RpcRequest {
        jsonrpc: ainb_hangar_proto::jsonrpc_version(),
        id: RpcId::Number(1),
        method: methods::ATTENTION_ANSWER.to_string(),
        params: serde_json::json!({
            "attention_id": "att-crash",
            "answer": "approve",
            "answered_by": "tui",
            "op_id": OP_ID,
        }),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_crash_between_claim_and_send_keys_is_surfaced_not_retried() {
    let dir = tempfile::tempdir().unwrap();
    let fake = install_fake_ainb(dir.path());
    // Single-test binary: no sibling test can observe this mutation.
    std::env::set_var("AINB_BIN", &fake);

    let store = Store::open_in(dir.path()).await.unwrap();
    seed_open_row(store.pool()).await;
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
            &answer_request(),
            &health(),
            &sink,
            &Caller::Operator,
        )
        .await
    });

    let key = LedgerKey::local(OP_ID);
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
    let claimed = AttentionRepo::get(store.pool(), "att-crash").await.unwrap().unwrap();
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
    assert_eq!(payload["op_id"], OP_ID);
    assert_eq!(
        unconfirmed[0].state, "open",
        "only an operator closes it — the daemon never does"
    );

    // ── the retry ───────────────────────────────────────────────────────────
    let response = rpc::dispatch_as(
        store.pool(),
        &answer_request(),
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
