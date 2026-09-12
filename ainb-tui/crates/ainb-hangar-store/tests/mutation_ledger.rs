//! Mutation-ledger integration tests (migration 0097, spec D18).
//!
//! Every case here is a failure mode the ledger exists for, driven against a
//! real ephemeral `SQLite` WAL database rather than a mock: the claim path is a
//! conditional INSERT and a conditional UPDATE, and those only mean anything
//! when SQLite is the one serialising them.

use ainb_hangar_store::Store;
use ainb_hangar_store::repo::mutation_ledger::{
    ClaimOutcome, LOCAL_HOST_ID, LedgerKey, MutationLedgerRepo, RetentionPolicy, STATUS_ACCEPTED,
    TIER_DEDUPE, TIER_RECEIPT,
};

const NOW: i64 = 1_700_000_000_000;

fn body() -> serde_json::Value {
    serde_json::json!({ "attention_id": "att-1", "answer": "yes", "answered_by": "tui" })
}

/// The core promise: the second arrival of an op id does not execute, it
/// replays the reply the first one committed, byte for byte.
#[tokio::test]
async fn a_replayed_op_id_returns_the_stored_reply() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(dir.path()).await.unwrap();
    let pool = store.pool();
    let key = LedgerKey::local("op-1");
    let fp = MutationLedgerRepo::fingerprint("attention/answer", &body());

    let first = MutationLedgerRepo::claim(pool, &key, "attention/answer", &fp, TIER_DEDUPE, NOW)
        .await
        .unwrap();
    assert_eq!(first, ClaimOutcome::Fresh);
    MutationLedgerRepo::record_reply(
        pool,
        &key,
        STATUS_ACCEPTED,
        None,
        Some(r#"{"outcome":"delivered","via":"tmux (s1)"}"#),
        NOW + 5,
    )
    .await
    .unwrap();

    let second =
        MutationLedgerRepo::claim(pool, &key, "attention/answer", &fp, TIER_DEDUPE, NOW + 9)
            .await
            .unwrap();
    let ClaimOutcome::Replay(row) = second else {
        panic!("a committed op id must replay, got {second:?}");
    };
    assert_eq!(
        row.reply.as_deref(),
        Some(r#"{"outcome":"delivered","via":"tmux (s1)"}"#)
    );
    assert_eq!(row.status, STATUS_ACCEPTED);
}

/// Amendment 15: the ledger key carries the principal, so a foreign row would
/// NOT collide on insert. Without the explicit lookup this caller would quietly
/// execute a second time under somebody else's op id.
#[tokio::test]
async fn a_second_principal_replaying_an_op_id_is_foreign() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(dir.path()).await.unwrap();
    let pool = store.pool();
    let fp = MutationLedgerRepo::fingerprint("attention/answer", &body());

    let mine = LedgerKey::local("op-shared");
    MutationLedgerRepo::claim(pool, &mine, "attention/answer", &fp, TIER_DEDUPE, NOW)
        .await
        .unwrap();
    MutationLedgerRepo::record_reply(pool, &mine, STATUS_ACCEPTED, None, Some("{}"), NOW)
        .await
        .unwrap();

    let theirs = LedgerKey::device("phone-7", "op-shared");
    let outcome =
        MutationLedgerRepo::claim(pool, &theirs, "attention/answer", &fp, TIER_DEDUPE, NOW + 1)
            .await
            .unwrap();
    assert_eq!(
        outcome,
        ClaimOutcome::Foreign {
            principal: "local".to_string()
        }
    );
    assert!(
        MutationLedgerRepo::get(pool, &theirs).await.unwrap().is_none(),
        "a foreign claim must not mint a row"
    );
}

/// Amendment 18: `adopted` requires the body fingerprint to match. Two clients
/// answering the same question with different text are not the same operation,
/// even if a client reused its op id.
#[tokio::test]
async fn a_different_body_under_the_same_op_id_is_a_mismatch() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(dir.path()).await.unwrap();
    let pool = store.pool();
    let key = LedgerKey::local("op-2");
    let yes = MutationLedgerRepo::fingerprint("attention/answer", &body());
    let no = MutationLedgerRepo::fingerprint(
        "attention/answer",
        &serde_json::json!({ "attention_id": "att-1", "answer": "no", "answered_by": "tui" }),
    );
    assert_ne!(yes, no);

    MutationLedgerRepo::claim(pool, &key, "attention/answer", &yes, TIER_DEDUPE, NOW)
        .await
        .unwrap();
    MutationLedgerRepo::record_reply(pool, &key, STATUS_ACCEPTED, None, Some("{}"), NOW)
        .await
        .unwrap();

    let outcome = MutationLedgerRepo::claim(pool, &key, "attention/answer", &no, TIER_DEDUPE, NOW)
        .await
        .unwrap();
    assert!(
        matches!(outcome, ClaimOutcome::BodyMismatch(_)),
        "{outcome:?}"
    );
}

/// The fingerprint covers the REQUEST, not the envelope: a retry that refreshes
/// its fence is still the same answer, and must adopt rather than be refused.
#[tokio::test]
async fn the_fingerprint_ignores_the_envelope() {
    let bare = MutationLedgerRepo::fingerprint("attention/answer", &body());
    let mut enveloped = body();
    let object = enveloped.as_object_mut().unwrap();
    object.insert("op_id".to_string(), serde_json::json!("op-3"));
    object.insert(
        "fence".to_string(),
        serde_json::json!({"kind":"attention_version","version":4}),
    );
    assert_eq!(
        bare,
        MutationLedgerRepo::fingerprint("attention/answer", &enveloped)
    );

    // Key order must not matter either: two clients serializing the same answer
    // differently are answering the same question.
    let reordered =
        serde_json::json!({ "answered_by": "tui", "answer": "yes", "attention_id": "att-1" });
    assert_eq!(
        bare,
        MutationLedgerRepo::fingerprint("attention/answer", &reordered)
    );
    // The method is part of the identity: the same body under a different verb
    // is a different operation.
    assert_ne!(
        bare,
        MutationLedgerRepo::fingerprint("fleet/action", &body())
    );
}

/// A claim whose handler never answered is in flight, not replayable: the
/// daemon may have committed, so the honest answer is "I cannot tell you".
#[tokio::test]
async fn an_unanswered_claim_reads_as_in_flight() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(dir.path()).await.unwrap();
    let pool = store.pool();
    let key = LedgerKey::local("op-4");
    let fp = MutationLedgerRepo::fingerprint("attention/answer", &body());

    MutationLedgerRepo::claim(pool, &key, "attention/answer", &fp, TIER_RECEIPT, NOW)
        .await
        .unwrap();
    let again = MutationLedgerRepo::claim(pool, &key, "attention/answer", &fp, TIER_RECEIPT, NOW)
        .await
        .unwrap();
    let ClaimOutcome::InFlight(row) = again else {
        panic!("an unanswered claim must read in flight, got {again:?}");
    };
    // Tier 2 opens at `claimed`, so a retry can say WHICH state it is stuck in.
    assert_eq!(row.receipt_state.as_deref(), Some("claimed"));
}

/// Amendment 17: a receipt still `writing` at boot, and any claim that never
/// reached a terminal status, both resolve to `unknown`, and neither is
/// re-executed.
#[tokio::test]
async fn the_boot_sweep_finds_writing_and_in_flight_rows() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(dir.path()).await.unwrap();
    let pool = store.pool();
    let fp = MutationLedgerRepo::fingerprint("attention/answer", &body());

    let writing = LedgerKey::local("op-writing");
    MutationLedgerRepo::claim(pool, &writing, "attention/answer", &fp, TIER_RECEIPT, NOW)
        .await
        .unwrap();
    MutationLedgerRepo::set_receipt(pool, &writing, "writing", None, NOW)
        .await
        .unwrap();

    let stalled = LedgerKey::local("op-stalled");
    MutationLedgerRepo::claim(pool, &stalled, "hangar/issue_update", &fp, TIER_DEDUPE, NOW)
        .await
        .unwrap();

    let settled = LedgerKey::local("op-settled");
    MutationLedgerRepo::claim(pool, &settled, "hangar/issue_update", &fp, TIER_DEDUPE, NOW)
        .await
        .unwrap();
    MutationLedgerRepo::record_reply(pool, &settled, STATUS_ACCEPTED, None, Some("{}"), NOW)
        .await
        .unwrap();

    let unresolved = MutationLedgerRepo::unresolved_at_boot(pool, LOCAL_HOST_ID).await.unwrap();
    let ids: Vec<&str> = unresolved.iter().map(|r| r.key.op_id.as_str()).collect();
    assert_eq!(
        ids,
        vec!["op-stalled", "op-writing"],
        "oldest first, then op id"
    );

    for row in &unresolved {
        MutationLedgerRepo::resolve_unknown(pool, &row.key, "effects_ambiguous", None, NOW + 1)
            .await
            .unwrap();
    }
    let after = MutationLedgerRepo::get(pool, &writing).await.unwrap().unwrap();
    assert_eq!(after.status, "unknown");
    assert_eq!(after.receipt_state.as_deref(), Some("unknown"));
    let after = MutationLedgerRepo::get(pool, &stalled).await.unwrap().unwrap();
    assert_eq!(after.status, "unknown");
    assert_eq!(
        after.receipt_state, None,
        "a dedupe-tier row has no receipt to resolve"
    );
    assert!(
        MutationLedgerRepo::unresolved_at_boot(pool, LOCAL_HOST_ID)
            .await
            .unwrap()
            .is_empty(),
        "the sweep must be idempotent"
    );
}

/// Retention is two-stage on purpose: stage one drops the bulk (the reply) but
/// keeps the key, so a retry that arrives late is answered `op_expired` instead
/// of executed a second time. Stage two removes the tombstone.
#[tokio::test]
async fn retention_expires_the_reply_before_it_deletes_the_key() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(dir.path()).await.unwrap();
    let pool = store.pool();
    let fp = MutationLedgerRepo::fingerprint("attention/answer", &body());
    let key = LedgerKey::local("op-old");

    MutationLedgerRepo::claim(pool, &key, "attention/answer", &fp, TIER_DEDUPE, NOW)
        .await
        .unwrap();
    MutationLedgerRepo::record_reply(pool, &key, STATUS_ACCEPTED, None, Some("{}"), NOW)
        .await
        .unwrap();

    let policy = RetentionPolicy::default();
    let eight_days = NOW + 8 * 24 * 60 * 60 * 1000;
    let report = MutationLedgerRepo::retain(pool, LOCAL_HOST_ID, eight_days, policy)
        .await
        .unwrap();
    assert_eq!(report.expired, 1);
    assert_eq!(report.deleted, 0);

    let row = MutationLedgerRepo::get(pool, &key).await.unwrap().unwrap();
    assert!(row.expired, "the key must survive the reply");
    assert_eq!(row.reply, None);

    let outcome =
        MutationLedgerRepo::claim(pool, &key, "attention/answer", &fp, TIER_DEDUPE, eight_days)
            .await
            .unwrap();
    assert!(
        matches!(outcome, ClaimOutcome::Expired(_)),
        "a retry after eviction must not re-execute, got {outcome:?}"
    );

    let fifteen_days = NOW + 15 * 24 * 60 * 60 * 1000;
    let report = MutationLedgerRepo::retain(pool, LOCAL_HOST_ID, fifteen_days, policy)
        .await
        .unwrap();
    assert_eq!(report.deleted, 1);
    assert!(MutationLedgerRepo::get(pool, &key).await.unwrap().is_none());
}

/// The row cap is the other half of D18's "7 days or 100k rows, whichever
/// first": a burst inside the age window still has to be bounded.
#[tokio::test]
async fn retention_caps_the_live_row_count() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(dir.path()).await.unwrap();
    let pool = store.pool();
    let fp = MutationLedgerRepo::fingerprint("attention/answer", &body());

    for i in 0..10 {
        let key = LedgerKey::local(format!("op-{i:02}"));
        MutationLedgerRepo::claim(pool, &key, "attention/answer", &fp, TIER_DEDUPE, NOW + i)
            .await
            .unwrap();
        MutationLedgerRepo::record_reply(pool, &key, STATUS_ACCEPTED, None, Some("{}"), NOW + i)
            .await
            .unwrap();
    }

    let policy = RetentionPolicy {
        max_live_rows: 4,
        ..RetentionPolicy::default()
    };
    let report = MutationLedgerRepo::retain(pool, LOCAL_HOST_ID, NOW + 10, policy).await.unwrap();
    assert_eq!(report.expired, 6, "the six oldest lose their replies");

    // The four newest keep theirs.
    for i in 6..10 {
        let row = MutationLedgerRepo::get(pool, &LedgerKey::local(format!("op-{i:02}")))
            .await
            .unwrap()
            .unwrap();
        assert!(!row.expired, "op-{i:02} should still be live");
    }
    let row = MutationLedgerRepo::get(pool, &LedgerKey::local("op-00"))
        .await
        .unwrap()
        .unwrap();
    assert!(row.expired);
}

/// The receipt lifecycle and the state flip commit together or not at all.
/// A rolled-back transaction must leave no trace of either.
#[tokio::test]
async fn a_rolled_back_transaction_leaves_no_receipt() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(dir.path()).await.unwrap();
    let pool = store.pool();
    let key = LedgerKey::local("op-rollback");
    let fp = MutationLedgerRepo::fingerprint("attention/answer", &body());

    let mut tx = pool.begin().await.unwrap();
    let outcome =
        MutationLedgerRepo::claim_in_tx(&mut tx, &key, "attention/answer", &fp, TIER_RECEIPT, NOW)
            .await
            .unwrap();
    assert_eq!(outcome, ClaimOutcome::Fresh);
    MutationLedgerRepo::set_receipt_in_tx(&mut tx, &key, "writing", None, NOW)
        .await
        .unwrap();
    tx.rollback().await.unwrap();

    assert!(
        MutationLedgerRepo::get(pool, &key).await.unwrap().is_none(),
        "a rolled-back claim must not leave a ledger row"
    );
}

/// Amendment 15 under CONCURRENCY, which is the only way it can actually fail.
///
/// Every other test in this file awaits the first claim before starting the
/// second, so the fast-path SELECT always sees the winner. Two principals that
/// truly race never see each other's row: `principal` is inside the primary
/// key, so each insert lands under its own key and both callers are told
/// `Fresh`, and both execute, under one op id, which is the exact double-fire
/// the ledger exists to stop.
///
/// The UNIQUE (host_id, op_id) index is what decides it. Exactly one `Fresh`,
/// and the loser is `Foreign` naming the winner.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_principals_racing_one_op_id_yield_exactly_one_fresh() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(dir.path()).await.unwrap();
    let fp = MutationLedgerRepo::fingerprint("attention/answer", &body());

    // Repeated, because a race that only sometimes interleaves would otherwise
    // pass on the run where it happened not to.
    for round in 0..25 {
        let op = format!("op-raced-{round:02}");
        let mine = LedgerKey::local(&op);
        let theirs = LedgerKey::device("phone-7", &op);
        let (a, b) = (store.pool().clone(), store.pool().clone());
        let (ka, kb) = (mine.clone(), theirs.clone());
        let (fa, fb) = (fp.clone(), fp.clone());

        let (left, right) = tokio::join!(
            tokio::spawn(async move {
                MutationLedgerRepo::claim(&a, &ka, "attention/answer", &fa, TIER_DEDUPE, NOW).await
            }),
            tokio::spawn(async move {
                MutationLedgerRepo::claim(&b, &kb, "attention/answer", &fb, TIER_DEDUPE, NOW).await
            }),
        );
        let outcomes = [left.unwrap().unwrap(), right.unwrap().unwrap()];

        let fresh = outcomes.iter().filter(|o| **o == ClaimOutcome::Fresh).count();
        assert_eq!(
            fresh, 1,
            "round {round}: exactly one principal may execute an op id, got {outcomes:?}"
        );
        let foreign = outcomes.iter().filter(|o| matches!(o, ClaimOutcome::Foreign { .. })).count();
        assert_eq!(
            foreign, 1,
            "round {round}: the loser must be told whose op id it is, got {outcomes:?}"
        );

        // And exactly one row exists for that op id, under whichever principal
        // won, never one per principal.
        let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM mutation_ledger WHERE op_id = ?")
            .bind(&op)
            .fetch_one(store.pool())
            .await
            .unwrap();
        assert_eq!(rows, 1, "round {round}: one op id, one ledger row");
    }
}

/// The ceiling retention cannot provide: a burst INSIDE the hourly window.
///
/// Every deterministic outcome writes a row and its serialized reply, and a
/// fresh op id per call makes every call a new row, so a caller that loops
/// grows the ledger unbounded between sweeps. The cap is per principal, so one
/// runaway credential cannot crowd out the operator's own.
#[tokio::test]
async fn a_principal_past_its_ceiling_is_refused_and_its_neighbour_is_not() {
    use ainb_hangar_store::repo::mutation_ledger::MAX_ROWS_PER_PRINCIPAL;

    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(dir.path()).await.unwrap();
    let pool = store.pool();
    let fp = MutationLedgerRepo::fingerprint("attention/answer", &body());

    // Fill one principal's quota directly: driving 50k claims through the repo
    // would test SQLite's insert rate, not the ceiling.
    for i in 0..MAX_ROWS_PER_PRINCIPAL {
        sqlx::query(
            "INSERT INTO mutation_ledger \
             (host_id, principal, op_id, method, body_fingerprint, tier, status, \
              expired, created_at, updated_at) \
             VALUES ('local', 'pal:channel-1', ?, 'attention/answer', ?, 'dedupe', \
                     'accepted', 0, ?, ?)",
        )
        .bind(format!("op-flood-{i:06}"))
        .bind(&fp)
        .bind(NOW)
        .bind(NOW)
        .execute(pool)
        .await
        .unwrap();
    }

    let flooder = LedgerKey {
        host_id: "local".to_string(),
        principal: "pal:channel-1".to_string(),
        op_id: "op-one-too-many".to_string(),
    };
    let outcome =
        MutationLedgerRepo::claim(pool, &flooder, "attention/answer", &fp, TIER_DEDUPE, NOW)
            .await
            .unwrap();
    assert!(
        matches!(outcome, ClaimOutcome::Saturated { .. }),
        "a principal past its ceiling must be refused, got {outcome:?}"
    );
    assert!(
        MutationLedgerRepo::get(pool, &flooder).await.unwrap().is_none(),
        "a refused claim must not mint the row it was refused for"
    );

    // The operator's own principal is untouched: the cap is per principal
    // precisely so one credential cannot deny service to another.
    let operator = LedgerKey::local("op-operator-still-works");
    assert_eq!(
        MutationLedgerRepo::claim(pool, &operator, "attention/answer", &fp, TIER_DEDUPE, NOW)
            .await
            .unwrap(),
        ClaimOutcome::Fresh
    );
}
