//! First-answer-wins survives the receipt lifecycle (spec P2, D18).
//!
//! The claim moved into a transaction that also writes a mutation receipt, and
//! a conditional UPDATE that grew a second statement beside it is exactly the
//! shape that quietly stops being atomic. So the race is driven for real: two
//! surfaces answer one open row concurrently, and the database is left to
//! serialise them.
//!
//! ```text
//! surface A ─┐                       ┌─▶ Delivered,      receipt delivered
//!            ├─▶ one open row ──────▶┤
//! surface B ─┘                       └─▶ AlreadyAnswered, no receipt, no send
//! ```
//!
//! Single-test binary: the forced-delivery seam is process-global.

use std::time::Instant;

use ainb_hangar_daemon::events::EventBroker;
use ainb_hangar_daemon::rpc::{self, DaemonHealth, auth::Caller};
use ainb_hangar_proto::{RpcId, RpcRequest, methods};
use ainb_hangar_store::Store;
use ainb_hangar_store::repo::attention::{AttentionKind, AttentionRepo, NewAttention};
use ainb_hangar_store::repo::mutation_ledger::{LedgerKey, MutationLedgerRepo};

/// The forced-delivery seam and `$AINB_BIN` are process-global, and these two
/// tests would otherwise disarm each other's seam mid-flight — which is exactly
/// the flake it looks like: one test reporting `delivery_failed` because a
/// sibling finished first.
static SEAM: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

const SESSION: &str = "sess-race";
const CWD: &str = "/work/race";

fn health() -> DaemonHealth {
    DaemonHealth {
        socket_path: "/tmp/answer-race.sock".to_string(),
        pid: std::process::id(),
        started_at: Instant::now(),
        version: "0.1.0".into(),
        stats: std::sync::Arc::new(ainb_hangar_daemon::health_stats::HealthStats::default()),
    }
}

/// One running session in `CWD`, so the C1 target resolution is unambiguous.
fn install_fake_ainb(dir: &std::path::Path) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt as _;
    let rows = serde_json::json!([{
        "session_id": SESSION,
        "tmux_session_name": "race-pane",
        "workspace_name": "race",
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

fn answer_request(op_id: &str, by: &str) -> RpcRequest {
    RpcRequest {
        jsonrpc: ainb_hangar_proto::jsonrpc_version(),
        id: RpcId::Number(1),
        method: methods::ATTENTION_ANSWER.to_string(),
        params: serde_json::json!({
            "attention_id": "att-race",
            "answer": "approve",
            "answered_by": by,
            "op_id": op_id,
        }),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_surfaces_answering_one_row_yield_one_delivered_and_one_already_answered() {
    let _seam = SEAM.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let fake = install_fake_ainb(dir.path());
    // Single-test binary: no sibling test can observe these mutations.
    std::env::set_var("AINB_BIN", &fake);
    ainb_hangar_daemon::answer::set_forced_delivery_for_test(true);

    let store = Store::open_in(dir.path()).await.unwrap();
    AttentionRepo::insert(
        store.pool(),
        &NewAttention {
            id: "att-race".into(),
            session_id: SESSION.into(),
            cwd: CWD.into(),
            workspace_id: None,
            kind: AttentionKind::Approval,
            payload: r#"{"kind":"APPROVAL","id":"att-race"}"#.into(),
            degraded: false,
            created_at: 1_000,
            raise_transcript: None,
            channels: ainb_hangar_core::channel::ChannelSet::NONE,
        },
    )
    .await
    .unwrap();

    let broker = EventBroker::new();
    let (a, b) = (store.pool().clone(), store.pool().clone());
    let (sink_a, sink_b) = (broker.sink(), broker.sink());
    let left = tokio::spawn(async move {
        rpc::dispatch_as(
            &a,
            &answer_request("op-race-a", "tui"),
            &health(),
            &sink_a,
            &Caller::Operator,
        )
        .await
    });
    let right = tokio::spawn(async move {
        rpc::dispatch_as(
            &b,
            &answer_request("op-race-b", "web"),
            &health(),
            &sink_b,
            &Caller::Operator,
        )
        .await
    });
    let outcomes: Vec<serde_json::Value> = vec![
        serde_json::to_value(left.await.unwrap()).unwrap(),
        serde_json::to_value(right.await.unwrap()).unwrap(),
    ];
    ainb_hangar_daemon::answer::set_forced_delivery_for_test(false);

    let tags: Vec<&str> = outcomes
        .iter()
        .map(|o| o["result"]["outcome"].as_str().unwrap_or("<error>"))
        .collect();
    assert_eq!(
        tags.iter().filter(|t| **t == "delivered").count(),
        1,
        "exactly one surface may deliver: {outcomes:?}"
    );
    assert_eq!(
        tags.iter().filter(|t| **t == "already_answered").count(),
        1,
        "the loser must be told who won, and deliver nothing: {outcomes:?}"
    );

    // The receipt lifecycle agrees with the race: one delivered, one that never
    // left `claimed` because its claim lost.
    let a = MutationLedgerRepo::get(store.pool(), &LedgerKey::local("op-race-a"))
        .await
        .unwrap();
    let b = MutationLedgerRepo::get(store.pool(), &LedgerKey::local("op-race-b"))
        .await
        .unwrap();
    let states: Vec<Option<&str>> = vec![
        a.as_ref().and_then(|r| r.receipt_state.as_deref()),
        b.as_ref().and_then(|r| r.receipt_state.as_deref()),
    ];
    assert_eq!(
        states.iter().filter(|s| **s == Some("delivered")).count(),
        1,
        "exactly one receipt may reach delivered: {states:?}"
    );
    // The loser has NO ledger row at all. Losing the race is a refusal, not an
    // applied mutation, so recording it would both mis-report `accepted` and
    // pin that refusal to the op id forever.
    assert_eq!(
        [a.is_none(), b.is_none()].iter().filter(|gone| **gone).count(),
        1,
        "the loser's claim must be abandoned, not recorded: a={a:?} b={b:?}"
    );

    // And the row itself is answered exactly once, by the winner.
    let row = AttentionRepo::get(store.pool(), "att-race").await.unwrap().unwrap();
    assert_eq!(row.state, "answered");
    assert!(row.answered_by.is_some());
    assert_eq!(row.version, 2, "the fence advanced exactly once");
}

/// The fence: a surface answering the version it read wins; one answering a
/// version that has moved is REFUSED, with the row left open, rather than
/// delivering a second time.
///
/// The refusal is `ambiguous`, not `already_answered`. Those are two different
/// facts and the row proves it: nobody answered this one, and it is still
/// answerable — telling the client "already answered by unknown" would be wrong
/// in both directions at once.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stale_fence_is_refused_with_the_row_still_open() {
    let _seam = SEAM.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let fake = install_fake_ainb(dir.path());
    std::env::set_var("AINB_BIN", &fake);
    ainb_hangar_daemon::answer::set_forced_delivery_for_test(true);

    let store = Store::open_in(dir.path()).await.unwrap();
    AttentionRepo::insert(
        store.pool(),
        &NewAttention {
            id: "att-fence".into(),
            session_id: SESSION.into(),
            cwd: CWD.into(),
            workspace_id: None,
            kind: AttentionKind::Approval,
            payload: r#"{"kind":"APPROVAL","id":"att-fence"}"#.into(),
            degraded: false,
            created_at: 1_000,
            raise_transcript: None,
            channels: ainb_hangar_core::channel::ChannelSet::NONE,
        },
    )
    .await
    .unwrap();
    let broker = EventBroker::new();
    let sink = broker.sink();

    let fenced = |op: &str, version: i64| RpcRequest {
        jsonrpc: ainb_hangar_proto::jsonrpc_version(),
        id: RpcId::Number(1),
        method: methods::ATTENTION_ANSWER.to_string(),
        params: serde_json::json!({
            "attention_id": "att-fence",
            "answer": "approve",
            "answered_by": "tui",
            "op_id": op,
            "fence": { "kind": "attention_version", "version": version },
        }),
    };

    // A fence naming a version the row never had is refused with the row still
    // open — the guard runs before anything is delivered.
    let stale = rpc::dispatch_as(
        store.pool(),
        &fenced("op-fence-stale", 99),
        &health(),
        &sink,
        &Caller::Operator,
    )
    .await;
    let stale = serde_json::to_value(stale).unwrap();
    assert_eq!(stale["result"]["outcome"], "ambiguous", "{stale}");
    assert!(
        stale["result"]["reason"].as_str().is_some_and(|r| r.contains("moved on")),
        "the refusal must say the read was stale, not invent a winner: {stale}"
    );
    let row = AttentionRepo::get(store.pool(), "att-fence").await.unwrap().unwrap();
    assert_eq!(row.state, "open", "a refused fence must not flip the row");
    assert!(row.answered_by.is_none(), "nobody answered it: {row:?}");

    // The ack must say REJECTED. Stamping a refusal `accepted` tells a client
    // branching on `mutation.status` that its answer was applied.
    let ack = &stale["result"][ainb_hangar_proto::mutation::ACK_KEY];
    assert_eq!(ack["status"], "rejected", "{stale}");
    assert_eq!(ack["reason"], "already_answered_by", "{stale}");

    // And the refusal must NOT be cached as this op id's permanent reply. The
    // body fingerprint strips the fence, so a client that re-reads the fence
    // and retries under the same op id would otherwise replay the refusal
    // forever and never deliver.
    assert!(
        MutationLedgerRepo::get(store.pool(), &LedgerKey::local("op-fence-stale"))
            .await
            .unwrap()
            .is_none(),
        "a fence refusal must leave the op id free for the documented retry"
    );
    let retried = rpc::dispatch_as(
        store.pool(),
        &fenced("op-fence-stale", row.version),
        &health(),
        &sink,
        &Caller::Operator,
    )
    .await;
    let retried = serde_json::to_value(retried).unwrap();
    ainb_hangar_daemon::answer::set_forced_delivery_for_test(false);
    // Both halves in one assertion: the version the surface actually read wins,
    // AND the op id that carried the refusal is reusable for that retry.
    assert_eq!(
        retried["result"]["outcome"], "delivered",
        "the same op id must deliver once the client refreshes its fence: {retried}"
    );
    assert_eq!(
        retried["result"][ainb_hangar_proto::mutation::ACK_KEY]["status"],
        "accepted",
        "{retried}"
    );
}
