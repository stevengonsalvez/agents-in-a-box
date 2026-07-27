//! Issue emoji reactions (multica parity #22, migration 0062).
//!
//! Pins the reference's `AddIssueReaction` / `RemoveIssueReaction` semantics:
//! unique per `(issue, actor, emoji)` so reacting twice is a no-op rather than
//! an error, a required-emoji guard at the repo boundary (the reference's
//! `400 "emoji is required"`), aggregation into per-emoji tallies, and reaping
//! on issue delete.

use ainb_hangar_core::actor::{ActorKind, ActorRef};
use ainb_hangar_store::Store;
use ainb_hangar_store::repo::issue::IssueRepo;
use ainb_hangar_store::repo::issue_reaction::{IssueReactionError, IssueReactionRepo};
use sqlx::SqlitePool;

fn member(id: &str) -> ActorRef {
    ActorRef::new(ActorKind::Member, id).unwrap()
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

async fn setup() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(dir.path()).await.unwrap();
    seed_ws(store.pool(), "ws-a").await;
    sqlx::query(
        "INSERT INTO issue (id, workspace_id, title, creator_type, creator_id, created_at) \
         VALUES ('iss-1','ws-a','t','member','m1',0)",
    )
    .execute(store.pool())
    .await
    .unwrap();
    (dir, store)
}

#[tokio::test]
async fn reacting_twice_is_one_row() {
    let (_dir, store) = setup().await;
    let me = member("me");
    assert!(
        IssueReactionRepo::add(store.pool(), "ws-a", "iss-1", &me, "👍", "r1", 1).await.unwrap()
    );
    assert!(
        !IssueReactionRepo::add(store.pool(), "ws-a", "iss-1", &me, "👍", "r2", 2).await.unwrap(),
        "the UNIQUE triple makes a repeat a no-op, not an error"
    );
    let tallies = IssueReactionRepo::tallies(store.pool(), "iss-1").await.unwrap();
    assert_eq!(tallies.len(), 1);
    assert_eq!(tallies[0].emoji, "👍");
    assert_eq!(tallies[0].count, 1);
}

#[tokio::test]
async fn two_actors_same_emoji_tally_to_two() {
    let (_dir, store) = setup().await;
    IssueReactionRepo::add(store.pool(), "ws-a", "iss-1", &member("a"), "🎉", "r1", 1)
        .await
        .unwrap();
    IssueReactionRepo::add(store.pool(), "ws-a", "iss-1", &member("b"), "🎉", "r2", 2)
        .await
        .unwrap();

    let tallies = IssueReactionRepo::tallies(store.pool(), "iss-1").await.unwrap();
    assert_eq!(tallies.len(), 1);
    assert_eq!(tallies[0].count, 2);
    assert!(tallies[0].actors.contains(&member("a")));
    assert!(tallies[0].actors.contains(&member("b")));
}

#[tokio::test]
async fn remove_reaction_is_idempotent() {
    let (_dir, store) = setup().await;
    let me = member("me");
    IssueReactionRepo::add(store.pool(), "ws-a", "iss-1", &me, "🚀", "r1", 1).await.unwrap();
    assert!(IssueReactionRepo::remove(store.pool(), "ws-a", "iss-1", &me, "🚀").await.unwrap());
    assert!(!IssueReactionRepo::remove(store.pool(), "ws-a", "iss-1", &me, "🚀").await.unwrap());
    assert!(IssueReactionRepo::tallies(store.pool(), "iss-1").await.unwrap().is_empty());
}

#[tokio::test]
async fn blank_emoji_is_rejected() {
    let (_dir, store) = setup().await;
    let err = IssueReactionRepo::add(store.pool(), "ws-a", "iss-1", &member("me"), "  ", "r1", 1)
        .await
        .unwrap_err();
    assert!(matches!(err, IssueReactionError::EmptyEmoji));
}

#[tokio::test]
async fn foreign_workspace_writes_nothing() {
    let (_dir, store) = setup().await;
    seed_ws(store.pool(), "ws-b").await;
    assert!(
        !IssueReactionRepo::add(store.pool(), "ws-b", "iss-1", &member("x"), "👀", "r1", 1)
            .await
            .unwrap()
    );
    assert!(IssueReactionRepo::list(store.pool(), "iss-1").await.unwrap().is_empty());
}

#[tokio::test]
async fn delete_cascade_reaps_reactions() {
    let (_dir, store) = setup().await;
    IssueReactionRepo::add(store.pool(), "ws-a", "iss-1", &member("me"), "❤️", "r1", 1)
        .await
        .unwrap();
    IssueRepo::delete_cascade(store.pool(), "ws-a", "iss-1").await.unwrap();
    assert!(IssueReactionRepo::list(store.pool(), "iss-1").await.unwrap().is_empty());
}
