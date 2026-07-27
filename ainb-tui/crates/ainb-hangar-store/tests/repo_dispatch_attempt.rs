//! The `dispatch_attempt` audit repo (multica parity #12, migration 0058).
//!
//! Before 0058 an admission refusal was ephemeral: an RPC error string, a debug
//! log line, or (on the CLI assign path) a bare `Ok(None)`. These tests pin the
//! persistence half of the fix against real sqlite — record/read round-trip with
//! the code decoding back through `DispatchReason::parse`, the
//! bounded-by-construction trim, and tenant scoping.

use ainb_hangar_core::dispatch_reason::{DispatchReason, DispatchSource};
use ainb_hangar_store::Store;
use ainb_hangar_store::repo::dispatch_attempt::{
    DISPATCH_ATTEMPT_KEEP, DispatchAttemptRepo, NewDispatchAttempt,
};

/// Seed two workspaces so tenant scoping is assertable.
async fn seed(store: &Store) {
    let pool = store.pool();
    for sql in [
        "INSERT INTO workspace (id, slug, name, created_at) VALUES ('ws-1','alpha','Alpha',0)",
        "INSERT INTO workspace (id, slug, name, created_at) VALUES ('ws-2','beta','Beta',0)",
    ] {
        sqlx::query(sql).execute(pool).await.expect(sql);
    }
}

/// A migrated store in a tempdir. The `TempDir` is returned so the caller keeps
/// it alive for the test's duration.
async fn open() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    seed(&store).await;
    (dir, store)
}

async fn record(
    store: &Store,
    id: &str,
    workspace_id: &str,
    issue_id: Option<&str>,
    reason: DispatchReason,
    detail: Option<&str>,
    created_at: i64,
) {
    DispatchAttemptRepo::record(
        store.pool(),
        id,
        &NewDispatchAttempt {
            workspace_id,
            issue_id,
            agent_id: None,
            runtime_id: None,
            task_id: None,
            reason,
            detail,
            source: DispatchSource::Manual,
            created_at,
        },
    )
    .await
    .expect("record attempt");
}

#[tokio::test]
async fn record_then_latest_for_issue_round_trips_the_code() {
    let (_dir, store) = open().await;
    record(
        &store,
        "att-1",
        "ws-1",
        Some("iss-1"),
        DispatchReason::TargetUnavailable,
        Some("no agent in this workspace to run on"),
        1_000,
    )
    .await;

    let latest = DispatchAttemptRepo::latest_for_issue(store.pool(), "iss-1")
        .await
        .expect("query")
        .expect("one attempt recorded");

    assert_eq!(latest.id, "att-1");
    assert_eq!(latest.reason, "target_unavailable");
    // The stored token decodes back through the shared vocabulary — the whole
    // point of one enum for the decider and the serializer.
    assert_eq!(latest.code(), Some(DispatchReason::TargetUnavailable));
    assert_eq!(latest.source_code(), Some(DispatchSource::Manual));
    assert!(!latest.is_dispatched());
    assert_eq!(
        latest.detail.as_deref(),
        Some("no agent in this workspace to run on")
    );
}

#[tokio::test]
async fn newest_first_and_queued_reads_as_dispatched() {
    let (_dir, store) = open().await;
    record(
        &store,
        "att-old",
        "ws-1",
        Some("iss-1"),
        DispatchReason::AlreadyActive,
        None,
        1_000,
    )
    .await;
    record(
        &store,
        "att-new",
        "ws-1",
        Some("iss-1"),
        DispatchReason::Queued,
        Some("task t-1"),
        2_000,
    )
    .await;

    let rows = DispatchAttemptRepo::list_for_issue(store.pool(), "iss-1", 10)
        .await
        .expect("list");
    let ids: Vec<&str> = rows.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(ids, ["att-new", "att-old"], "newest first");
    assert!(rows[0].is_dispatched());
    assert!(!rows[1].is_dispatched());

    let latest = DispatchAttemptRepo::latest_for_issue(store.pool(), "iss-1")
        .await
        .expect("query")
        .expect("present");
    assert_eq!(latest.id, "att-new");
}

/// Bounded by construction: 25 records leave exactly the newest 20, so a hot
/// auto-run cascade cannot grow the table without bound (migration decision 3).
#[tokio::test]
async fn record_trims_to_the_newest_keep_rows_per_issue() {
    let (_dir, store) = open().await;
    for i in 0..25_i64 {
        record(
            &store,
            &format!("att-{i:02}"),
            "ws-1",
            Some("iss-1"),
            DispatchReason::AlreadyActive,
            None,
            1_000 + i,
        )
        .await;
    }

    let rows = DispatchAttemptRepo::list_for_issue(store.pool(), "iss-1", 100)
        .await
        .expect("list");
    assert_eq!(rows.len(), usize::try_from(DISPATCH_ATTEMPT_KEEP).unwrap());
    // …and they are the NEWEST 20, not the first 20.
    assert_eq!(rows.first().expect("head").id, "att-24");
    assert_eq!(rows.last().expect("tail").id, "att-05");
}

/// The trim is per-issue: a sibling card's history is untouched.
#[tokio::test]
async fn trim_is_scoped_to_one_issue() {
    let (_dir, store) = open().await;
    record(
        &store,
        "other-1",
        "ws-1",
        Some("iss-2"),
        DispatchReason::Deferred,
        None,
        1,
    )
    .await;
    for i in 0..25_i64 {
        record(
            &store,
            &format!("att-{i:02}"),
            "ws-1",
            Some("iss-1"),
            DispatchReason::AlreadyActive,
            None,
            1_000 + i,
        )
        .await;
    }

    let other = DispatchAttemptRepo::list_for_issue(store.pool(), "iss-2", 100)
        .await
        .expect("list");
    assert_eq!(other.len(), 1, "sibling card's history must survive the trim");
}

/// Mirrors `workspace_data_isolation.rs`: a sibling tenant's attempts are never
/// returned by the workspace feed.
#[tokio::test]
async fn list_by_workspace_never_leaks_a_sibling_tenant() {
    let (_dir, store) = open().await;
    record(
        &store,
        "mine",
        "ws-1",
        Some("iss-1"),
        DispatchReason::RuntimeOffline,
        None,
        1_000,
    )
    .await;
    record(
        &store,
        "theirs",
        "ws-2",
        Some("iss-9"),
        DispatchReason::RuntimeOffline,
        None,
        2_000,
    )
    .await;

    let mine = DispatchAttemptRepo::list_by_workspace(store.pool(), "ws-1", 50)
        .await
        .expect("list");
    let ids: Vec<&str> = mine.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(ids, ["mine"]);

    let theirs = DispatchAttemptRepo::list_by_workspace(store.pool(), "ws-2", 50)
        .await
        .expect("list");
    assert_eq!(theirs.len(), 1);
    assert_eq!(theirs[0].id, "theirs");

    // A workspace with nothing recorded is empty, not an error.
    let empty = DispatchAttemptRepo::list_by_workspace(store.pool(), "ws-absent", 50)
        .await
        .expect("list");
    assert!(empty.is_empty());
}

#[tokio::test]
async fn list_honours_its_limit() {
    let (_dir, store) = open().await;
    for i in 0..5_i64 {
        record(
            &store,
            &format!("att-{i}"),
            "ws-1",
            Some("iss-1"),
            DispatchReason::AlreadyActive,
            None,
            1_000 + i,
        )
        .await;
    }
    let capped = DispatchAttemptRepo::list_for_issue(store.pool(), "iss-1", 2)
        .await
        .expect("list");
    assert_eq!(capped.len(), 2);
    assert_eq!(capped[0].id, "att-4");

    let ws_capped = DispatchAttemptRepo::list_by_workspace(store.pool(), "ws-1", 3)
        .await
        .expect("list");
    assert_eq!(ws_capped.len(), 3);
}

/// A token this binary does not know decodes to `None` and reads as raw text —
/// an older binary never fails a read against a newer daemon's vocabulary.
#[tokio::test]
async fn unknown_stored_code_reads_as_raw_text_not_an_error() {
    let (_dir, store) = open().await;
    sqlx::query(
        "INSERT INTO dispatch_attempt \
         (id, workspace_id, issue_id, reason, source, created_at) \
         VALUES ('att-future','ws-1','iss-1','nonsense_code','nonsense_source',1)",
    )
    .execute(store.pool())
    .await
    .expect("insert future token");

    let latest = DispatchAttemptRepo::latest_for_issue(store.pool(), "iss-1")
        .await
        .expect("query")
        .expect("present");
    assert_eq!(latest.reason, "nonsense_code");
    assert_eq!(latest.code(), None);
    assert_eq!(latest.source_code(), None);
    // Unknown => surfaced (NOT hidden as "dispatched").
    assert!(!latest.is_dispatched());
}
