//! The issue-activity DIFF engine (multica parity #13).
//!
//! multica writes one `activity_log` row per CHANGED FIELD, not one per update
//! call. These tests drive the real `IssueRepo::update_fields` → re-read →
//! `ActivityService::record_issue_diff` path against sqlite and assert exactly
//! which rows land, in which order, with which details — the contract both the
//! daemon and the CLI issue-update writers share.

use ainb_hangar_core::activity::{ActivityAction, ActivityActor};
use ainb_hangar_core::actor::{ActorKind, ActorRef};
use ainb_hangar_core::clock::HangarClock;
use ainb_hangar_core::idgen::IdGen;
use ainb_hangar_store::Store;
use ainb_hangar_store::repo::activity::ActivityRepo;
use ainb_hangar_store::repo::issue::{Issue, IssueFieldUpdate, IssueRepo, NewIssue};
use ainb_hangar_store::service::activity::ActivityService;

/// A monotonic id source: `act-0000`, `act-0001`, … so ordering assertions are
/// stable without depending on ULID entropy. The counter is process-global so a
/// test that calls the helper twice cannot collide on the primary key (which,
/// under the best-effort contract, would be swallowed rather than raised).
static NEXT_ID: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

struct SeqIds;
impl IdGen for SeqIds {
    fn new_ulid(&self) -> String {
        let n = NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        format!("act-{n:04}")
    }
}

/// A clock that ticks one millisecond per read, so rows written inside a single
/// diff call are still distinguishable by `created_at`.
struct TickClock(std::sync::atomic::AtomicI64);
impl HangarClock for TickClock {
    fn now_ms(&self) -> i64 {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    }
}

fn member(id: &str) -> ActivityActor {
    ActivityActor::Actor(ActorRef::new(ActorKind::Member, id).expect("member"))
}

async fn open() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    sqlx::query("INSERT INTO workspace (id, slug, name, created_at) VALUES ('ws-1','a','A',0)")
        .execute(store.pool())
        .await
        .expect("workspace");
    sqlx::query("INSERT INTO user (id, email, created_at) VALUES ('u1','a@example.com',0)")
        .execute(store.pool())
        .await
        .expect("user");
    (dir, store)
}

async fn seed_issue(store: &Store) -> Issue {
    IssueRepo::insert(
        store.pool(),
        &NewIssue {
            id: "iss-1".into(),
            workspace_id: "ws-1".into(),
            title: "Wire the timeline".into(),
            description: None,
            state: "open".into(),
            assignee: None,
            creator: ActorRef::new(ActorKind::Member, "u1").unwrap(),
            created_at: 1,
            priority: 1,
            due_date: None,
            labels: vec![],
            acceptance_criteria: vec![],
            context_refs: vec![],
            parent_issue_id: None,
            stage: None,
        },
    )
    .await
    .expect("insert issue");
    IssueRepo::get_by_id(store.pool(), "iss-1")
        .await
        .expect("get")
        .expect("issue exists")
}

/// Apply an edit and record the diff, returning the `(action, details)` pairs
/// that landed in `activity_log`, OLDEST FIRST.
async fn edit_and_diff(
    store: &Store,
    update: &IssueFieldUpdate,
) -> Vec<(String, serde_json::Value)> {
    let ids = SeqIds;
    let clock = TickClock(std::sync::atomic::AtomicI64::new(
        NEXT_ID.load(std::sync::atomic::Ordering::SeqCst) as i64 * 100 + 1000,
    ));
    let before = IssueRepo::get_by_id(store.pool(), "iss-1")
        .await
        .expect("get before")
        .expect("issue");
    IssueRepo::update_fields(store.pool(), "ws-1", "iss-1", update)
        .await
        .expect("update");
    let after = IssueRepo::get_by_id(store.pool(), "iss-1")
        .await
        .expect("get after")
        .expect("issue");

    ActivityService::record_issue_diff(
        store.pool(),
        &ids,
        &clock,
        "ws-1",
        &member("u1"),
        &before,
        &after,
    )
    .await;

    let mut rows = ActivityRepo::list_for_issue(store.pool(), "iss-1", 100).await.expect("list");
    rows.reverse(); // repo is newest-first; the timeline reads oldest-first
    rows.into_iter().map(|r| (r.action.clone(), r.details_json())).collect()
}

#[tokio::test]
async fn a_state_only_edit_writes_exactly_one_row() {
    let (_dir, store) = open().await;
    seed_issue(&store).await;

    let rows = edit_and_diff(
        &store,
        &IssueFieldUpdate {
            state: Some("in_progress".into()),
            ..Default::default()
        },
    )
    .await;

    assert_eq!(rows.len(), 1, "one changed field → one row: {rows:?}");
    assert_eq!(rows[0].0, ActivityAction::StatusChanged.as_db_str());
    assert_eq!(
        rows[0].1,
        serde_json::json!({"from": "open", "to": "in_progress"})
    );
}

#[tokio::test]
async fn a_three_field_edit_writes_three_rows_in_field_order() {
    let (_dir, store) = open().await;
    seed_issue(&store).await;

    let rows = edit_and_diff(
        &store,
        &IssueFieldUpdate {
            state: Some("in_progress".into()),
            assignee: Some(Some(ActorRef::new(ActorKind::Member, "u1").unwrap())),
            priority: Some(3),
            ..Default::default()
        },
    )
    .await;

    let actions: Vec<&str> = rows.iter().map(|(a, _)| a.as_str()).collect();
    assert_eq!(
        actions,
        ["status_changed", "assignee_changed", "priority_changed"],
        "stable field order: state → assignee → priority → title → due_date"
    );
    // hangar priority is numeric (multica's is a string enum) — DEVIATION.
    assert_eq!(rows[2].1, serde_json::json!({"from": 1, "to": 3}));
}

#[tokio::test]
async fn a_no_op_edit_writes_nothing() {
    let (_dir, store) = open().await;
    seed_issue(&store).await;

    // Setting every field to the value it already holds.
    let rows = edit_and_diff(
        &store,
        &IssueFieldUpdate {
            state: Some("open".into()),
            title: Some("Wire the timeline".into()),
            priority: Some(1),
            ..Default::default()
        },
    )
    .await;

    assert!(rows.is_empty(), "no field changed → no rows: {rows:?}");
}

#[tokio::test]
async fn assignment_and_unassignment_omit_the_absent_side() {
    let (_dir, store) = open().await;
    seed_issue(&store).await;

    // Unassigned → assigned: only the `to_*` keys are present.
    let rows = edit_and_diff(
        &store,
        &IssueFieldUpdate {
            assignee: Some(Some(ActorRef::new(ActorKind::Agent, "agent-7").unwrap())),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0, "assignee_changed");
    assert_eq!(
        rows[0].1,
        serde_json::json!({"to_type": "agent", "to_id": "agent-7"}),
        "the nil FROM side is omitted entirely, not written as null"
    );

    // Assigned → unassigned: only the `from_*` keys are present.
    let rows = edit_and_diff(
        &store,
        &IssueFieldUpdate {
            assignee: Some(None),
            ..Default::default()
        },
    )
    .await;
    let unassign = rows.last().expect("an unassign row");
    assert_eq!(unassign.0, "assignee_changed");
    assert_eq!(
        unassign.1,
        serde_json::json!({"from_type": "agent", "from_id": "agent-7"})
    );
}

#[tokio::test]
async fn clearing_a_due_date_records_a_typed_null() {
    let (_dir, store) = open().await;
    seed_issue(&store).await;

    edit_and_diff(
        &store,
        &IssueFieldUpdate {
            due_date: Some(Some(1_700_000_000_000)),
            ..Default::default()
        },
    )
    .await;
    let rows = edit_and_diff(
        &store,
        &IssueFieldUpdate {
            due_date: Some(None),
            ..Default::default()
        },
    )
    .await;

    let cleared = rows.last().expect("a due-date row");
    assert_eq!(cleared.0, "due_date_changed");
    assert_eq!(
        cleared.1,
        serde_json::json!({"from": 1_700_000_000_000i64, "to": serde_json::Value::Null}),
        "hangar writes a typed null where multica writes an empty string"
    );
}

#[tokio::test]
async fn a_title_edit_records_both_sides() {
    let (_dir, store) = open().await;
    seed_issue(&store).await;

    let rows = edit_and_diff(
        &store,
        &IssueFieldUpdate {
            title: Some("Wire the timeline modal".into()),
            ..Default::default()
        },
    )
    .await;

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0, "title_changed");
    assert_eq!(
        rows[0].1,
        serde_json::json!({"from": "Wire the timeline", "to": "Wire the timeline modal"})
    );
}

/// REGRESSION: a whole diff runs inside one millisecond, and `ulid::Ulid::new()`
/// is not monotonic within a millisecond — so a shared `created_at` let the
/// timeline's `(created_at, id)` sort shuffle the fields arbitrarily. The
/// service stamps row `i` at `base + i`, which is what makes the written field
/// order the read order.
#[tokio::test]
async fn a_frozen_clock_still_preserves_field_order_on_read() {
    struct FrozenClock;
    impl HangarClock for FrozenClock {
        fn now_ms(&self) -> i64 {
            1_700_000_000_000
        }
    }

    let (_dir, store) = open().await;
    seed_issue(&store).await;

    let before = IssueRepo::get_by_id(store.pool(), "iss-1")
        .await
        .expect("get before")
        .expect("issue");
    IssueRepo::update_fields(
        store.pool(),
        "ws-1",
        "iss-1",
        &IssueFieldUpdate {
            state: Some("in_progress".into()),
            priority: Some(3),
            title: Some("Renamed".into()),
            ..Default::default()
        },
    )
    .await
    .expect("update");
    let after = IssueRepo::get_by_id(store.pool(), "iss-1")
        .await
        .expect("get after")
        .expect("issue");

    // Random ULIDs, one frozen clock reading: only the per-row stamp can keep
    // the order stable.
    let written = ActivityService::record_issue_diff(
        store.pool(),
        &ainb_hangar_core::idgen::SystemIdGen,
        &FrozenClock,
        "ws-1",
        &member("u1"),
        &before,
        &after,
    )
    .await;
    assert_eq!(written, 3);

    let mut rows = ActivityRepo::list_for_issue(store.pool(), "iss-1", 100).await.expect("list");
    rows.reverse();
    let actions: Vec<&str> = rows.iter().map(|r| r.action.as_str()).collect();
    assert_eq!(
        actions,
        ["status_changed", "priority_changed", "title_changed"],
        "field order must survive a same-millisecond write"
    );
}
