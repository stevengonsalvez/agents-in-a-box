//! Issue subscribers (multica parity #22, migration 0062).
//!
//! The acceptance sentence for #22 is *"an actor can subscribe to an issue;
//! persists (sqlite)"*. These tests pin that at the store layer, plus the four
//! semantics the reference's `AddIssueSubscriber` / `RemoveIssueSubscriber`
//! carry: first-reason-wins idempotency, idempotent removal, tenant isolation
//! through the join to `issue`, and reaping on issue delete.

use ainb_hangar_core::actor::{ActorKind, ActorRef};
use ainb_hangar_store::Store;
use ainb_hangar_store::repo::issue::IssueRepo;
use ainb_hangar_store::repo::issue_subscriber::{IssueSubscriberRepo, SubscribeReason};
use sqlx::SqlitePool;

fn member(id: &str) -> ActorRef {
    ActorRef::new(ActorKind::Member, id).unwrap()
}

fn agent(id: &str) -> ActorRef {
    ActorRef::new(ActorKind::Agent, id).unwrap()
}

async fn seed_ws(pool: &SqlitePool, id: &str) {
    sqlx::query("INSERT INTO workspace (id, slug, name, created_at) VALUES (?, ?, ?, 0)")
        .bind(id)
        .bind(id)
        .bind(id)
        .execute(pool)
        .await
        .unwrap();
}

async fn seed_issue(pool: &SqlitePool, ws: &str, id: &str) {
    sqlx::query(
        "INSERT INTO issue (id, workspace_id, title, creator_type, creator_id, created_at) \
         VALUES (?, ?, ?, 'member', 'm1', 0)",
    )
    .bind(id)
    .bind(ws)
    .bind(id)
    .execute(pool)
    .await
    .unwrap();
}

async fn setup() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(dir.path()).await.unwrap();
    seed_ws(store.pool(), "ws-a").await;
    seed_issue(store.pool(), "ws-a", "iss-1").await;
    (dir, store)
}

/// THE ACCEPTANCE, at the unit layer: subscribe, then re-open the SAME database
/// file from a fresh pool and read the row back.
#[tokio::test]
async fn subscribe_persists_and_reads_back() {
    let (dir, store) = setup().await;
    assert!(
        IssueSubscriberRepo::add(
            store.pool(),
            "ws-a",
            "iss-1",
            &member("me"),
            SubscribeReason::Manual,
            123,
        )
        .await
        .unwrap()
    );
    drop(store);

    // A brand-new process would see exactly this.
    let reopened = Store::open_in(dir.path()).await.unwrap();
    let subs = IssueSubscriberRepo::list(reopened.pool(), "iss-1").await.unwrap();
    assert_eq!(subs.len(), 1, "the subscription survives a pool reopen");
    assert_eq!(subs[0].actor, member("me"));
    assert_eq!(subs[0].reason, Some(SubscribeReason::Manual));
    assert_eq!(subs[0].reason_raw, "manual");
}

/// `reason` is provenance, not state: the FIRST reason wins, exactly as the
/// reference's `ON CONFLICT DO NOTHING`.
#[tokio::test]
async fn first_reason_wins_on_repeat_subscribe() {
    let (_dir, store) = setup().await;
    let me = member("me");
    assert!(
        IssueSubscriberRepo::add(
            store.pool(),
            "ws-a",
            "iss-1",
            &me,
            SubscribeReason::Creator,
            1
        )
        .await
        .unwrap()
    );
    assert!(
        !IssueSubscriberRepo::add(
            store.pool(),
            "ws-a",
            "iss-1",
            &me,
            SubscribeReason::Commenter,
            2
        )
        .await
        .unwrap(),
        "a repeat subscribe lands no second row"
    );

    let subs = IssueSubscriberRepo::list(store.pool(), "iss-1").await.unwrap();
    assert_eq!(subs.len(), 1);
    assert_eq!(subs[0].reason, Some(SubscribeReason::Creator));
}

#[tokio::test]
async fn unsubscribe_removes_and_is_idempotent() {
    let (_dir, store) = setup().await;
    let me = member("me");
    IssueSubscriberRepo::add(
        store.pool(),
        "ws-a",
        "iss-1",
        &me,
        SubscribeReason::Manual,
        1,
    )
    .await
    .unwrap();

    assert!(IssueSubscriberRepo::remove(store.pool(), "ws-a", "iss-1", &me).await.unwrap());
    assert!(!IssueSubscriberRepo::is_subscribed(store.pool(), "iss-1", &me).await.unwrap());
    assert!(
        !IssueSubscriberRepo::remove(store.pool(), "ws-a", "iss-1", &me).await.unwrap(),
        "removing an absent subscription is a no-op, not an error"
    );
}

/// The [`CommentRepo::insert`] tenant rule: a write scoped to the WRONG
/// workspace lands nothing and does not error.
#[tokio::test]
async fn foreign_workspace_writes_nothing() {
    let (_dir, store) = setup().await;
    seed_ws(store.pool(), "ws-b").await;

    assert!(
        !IssueSubscriberRepo::add(
            store.pool(),
            "ws-b",
            "iss-1",
            &member("intruder"),
            SubscribeReason::Manual,
            1,
        )
        .await
        .unwrap()
    );
    assert_eq!(
        IssueSubscriberRepo::count(store.pool(), "iss-1").await.unwrap(),
        0
    );
}

#[tokio::test]
async fn delete_cascade_reaps_subscribers() {
    let (_dir, store) = setup().await;
    IssueSubscriberRepo::add(
        store.pool(),
        "ws-a",
        "iss-1",
        &member("me"),
        SubscribeReason::Manual,
        1,
    )
    .await
    .unwrap();

    IssueRepo::delete_cascade(store.pool(), "ws-a", "iss-1").await.unwrap();
    assert_eq!(
        IssueSubscriberRepo::count(store.pool(), "iss-1").await.unwrap(),
        0
    );
}

/// A member and an agent are DISTINCT subscribers even with the same id half.
#[tokio::test]
async fn distinct_actors_accumulate() {
    let (_dir, store) = setup().await;
    for a in [member("x"), agent("x")] {
        IssueSubscriberRepo::add(
            store.pool(),
            "ws-a",
            "iss-1",
            &a,
            SubscribeReason::Manual,
            1,
        )
        .await
        .unwrap();
    }
    assert_eq!(
        IssueSubscriberRepo::count(store.pool(), "iss-1").await.unwrap(),
        2
    );
    let actors = IssueSubscriberRepo::actors(store.pool(), "iss-1").await.unwrap();
    assert!(actors.contains(&member("x")));
    assert!(actors.contains(&agent("x")));
}
