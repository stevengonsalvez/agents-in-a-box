//! The `activity_log` narrative repo (multica parity #13, migration 0059).
//!
//! Pins the persistence half against real sqlite: record/read round-trip with
//! the action decoding back through `ActivityAction::parse`, the newest-first
//! ordering including the same-millisecond id tiebreak, tenant scoping on the
//! workspace feed, the `system` actor's NULL id, and the tolerant read of a
//! token this binary does not know.

use ainb_hangar_core::activity::{ActivityAction, ActivityActor};
use ainb_hangar_core::actor::{ActorKind, ActorRef};
use ainb_hangar_store::Store;
use ainb_hangar_store::repo::activity::{ActivityRepo, NewActivity};

/// Seed two workspaces so tenant scoping is assertable.
async fn open() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    for sql in [
        "INSERT INTO workspace (id, slug, name, created_at) VALUES ('ws-1','alpha','Alpha',0)",
        "INSERT INTO workspace (id, slug, name, created_at) VALUES ('ws-2','beta','Beta',0)",
    ] {
        sqlx::query(sql).execute(store.pool()).await.expect(sql);
    }
    (dir, store)
}

async fn record(
    store: &Store,
    id: &str,
    workspace_id: &str,
    issue_id: &str,
    actor: &ActivityActor,
    action: ActivityAction,
    details: serde_json::Value,
    created_at: i64,
) {
    ActivityRepo::record(
        store.pool(),
        id,
        &NewActivity {
            workspace_id,
            issue_id: Some(issue_id),
            actor,
            action,
            details,
            created_at,
        },
    )
    .await
    .expect("record activity");
}

fn member(id: &str) -> ActivityActor {
    ActivityActor::Actor(ActorRef::new(ActorKind::Member, id).expect("member"))
}

#[tokio::test]
async fn records_and_reads_back_newest_first_with_id_tiebreak() {
    let (_dir, store) = open().await;
    let m = member("u1");

    record(
        &store,
        "act-1",
        "ws-1",
        "iss-1",
        &m,
        ActivityAction::Created,
        serde_json::json!({}),
        100,
    )
    .await;
    // Two rows in the SAME millisecond: the ULID-shaped id is the tiebreaker.
    record(
        &store,
        "act-2",
        "ws-1",
        "iss-1",
        &m,
        ActivityAction::StatusChanged,
        serde_json::json!({"from": "open", "to": "in_progress"}),
        200,
    )
    .await;
    record(
        &store,
        "act-3",
        "ws-1",
        "iss-1",
        &m,
        ActivityAction::PriorityChanged,
        serde_json::json!({"from": 1, "to": 3}),
        200,
    )
    .await;

    let rows = ActivityRepo::list_for_issue(store.pool(), "iss-1", 50)
        .await
        .expect("list");
    let ids: Vec<&str> = rows.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(ids, ["act-3", "act-2", "act-1"], "newest first, id tiebreak");

    let status = &rows[1];
    assert_eq!(status.code(), Some(ActivityAction::StatusChanged));
    assert_eq!(
        status.details_json(),
        serde_json::json!({"from": "open", "to": "in_progress"})
    );
    assert_eq!(status.actor(), Some(member("u1")));
}

#[tokio::test]
async fn workspace_feed_never_returns_a_sibling_tenants_rows() {
    let (_dir, store) = open().await;
    let m = member("u1");
    record(
        &store,
        "act-mine",
        "ws-1",
        "iss-1",
        &m,
        ActivityAction::Created,
        serde_json::json!({}),
        1,
    )
    .await;
    record(
        &store,
        "act-theirs",
        "ws-2",
        "iss-2",
        &m,
        ActivityAction::Created,
        serde_json::json!({}),
        2,
    )
    .await;

    let mine = ActivityRepo::list_by_workspace(store.pool(), "ws-1", 50)
        .await
        .expect("list ws-1");
    let ids: Vec<&str> = mine.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(ids, ["act-mine"]);
}

#[tokio::test]
async fn system_row_stores_no_actor_id() {
    let (_dir, store) = open().await;
    record(
        &store,
        "act-sys",
        "ws-1",
        "iss-1",
        &ActivityActor::System,
        ActivityAction::StatusChanged,
        serde_json::json!({"from": "open", "to": "done", "via": "pr_merged"}),
        5,
    )
    .await;

    let rows = ActivityRepo::list_for_issue(store.pool(), "iss-1", 10)
        .await
        .expect("list");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].actor_type.as_deref(), Some("system"));
    assert_eq!(rows[0].actor_id, None);
    assert_eq!(rows[0].actor(), Some(ActivityActor::System));
    assert_eq!(
        rows[0].details_json().get("via").and_then(|v| v.as_str()),
        Some("pr_merged")
    );
}

/// The tolerant-read contract: a token written by a newer daemon reads back raw
/// with `code() == None` instead of failing the read.
#[tokio::test]
async fn unknown_action_token_reads_back_raw() {
    let (_dir, store) = open().await;
    sqlx::query(
        "INSERT INTO activity_log (id, workspace_id, issue_id, actor_type, action, created_at) \
         VALUES ('act-future','ws-1','iss-1','system','teleported_from_2027',9)",
    )
    .execute(store.pool())
    .await
    .expect("raw insert");

    let rows = ActivityRepo::list_for_issue(store.pool(), "iss-1", 10)
        .await
        .expect("list");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].action, "teleported_from_2027");
    assert_eq!(rows[0].code(), None);
    // …and the default details blob decodes to an empty object.
    assert_eq!(rows[0].details_json(), serde_json::json!({}));
}
