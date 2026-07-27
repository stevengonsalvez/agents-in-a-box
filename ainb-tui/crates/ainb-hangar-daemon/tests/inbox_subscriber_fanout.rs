//! The inbox fan-out reads the REAL subscriber set (multica parity #22).
//!
//! Before migration 0062 `inbox_aggregator::recipients_for` approximated "who is
//! notified" as the issue's PARTICIPANTS (creator + assignee) minus the actor,
//! and its own doc comment said so. This file is the proof that the table is not
//! decorative:
//!
//! 1. `a_manual_subscriber_who_is_neither_creator_nor_assignee_gets_the_comment_entry`
//!    FAILS on the participant approximation — the watcher is neither endpoint,
//!    so the old derivation could never reach them. It passes only once
//!    `recipients_for` reads `issue_subscriber`.
//! 2. `the_comment_author_is_not_notified_of_their_own_comment` pins the half of
//!    the reference's rule that must SURVIVE the conversion.
//! 3. `an_issue_with_no_subscriber_rows_still_notifies_its_participants` pins the
//!    fallback, so no upgrade path can silence a notification.

use std::time::Duration;

use ainb_hangar_core::actor::{ActorKind, ActorRef};
use ainb_hangar_core::ids::{CommentId, IssueId};
use ainb_hangar_daemon::events::EventBroker;
use ainb_hangar_daemon::seed::{self, WS_ID};
use ainb_hangar_proto::events::{CommentRow, HangarEvent};
use ainb_hangar_store::Store;
use ainb_hangar_store::repo::issue_subscriber::{IssueSubscriberRepo, SubscribeReason};

fn member(id: &str) -> ActorRef {
    ActorRef::new(ActorKind::Member, id).unwrap()
}

/// Boot a seeded store with the REAL aggregator draining a shared broker — the
/// same wiring `boot()` uses — and hand back the emit sink.
async fn boot() -> (
    tempfile::TempDir,
    Store,
    ainb_hangar_daemon::events::EventSink,
) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(dir.path()).await.unwrap();
    seed::seed_p4_fixture(store.pool()).await.unwrap();
    let broker = EventBroker::new();
    drop(ainb_hangar_daemon::inbox_aggregator::spawn(
        store.pool().clone(),
        broker.subscribe(),
    ));
    let sink = broker.sink();
    (dir, store, sink)
}

/// Insert one issue with an explicit creator + assignee, bypassing the repo so
/// the test controls the subscriber set exactly (the repo auto-subscribes both).
async fn seed_issue(store: &Store, id: &str, creator: &str, assignee: Option<&str>) {
    sqlx::query(
        "INSERT INTO issue \
         (id, workspace_id, title, state, creator_type, creator_id, \
          assignee_type, assignee_id, created_at) \
         VALUES (?, ?, 'fanout fixture', 'open', 'member', ?, ?, ?, 0)",
    )
    .bind(id)
    .bind(WS_ID)
    .bind(creator)
    .bind(assignee.map(|_| "member"))
    .bind(assignee)
    .execute(store.pool())
    .await
    .expect("seed issue");
}

/// Emit one `CommentAdded` for `issue_id` authored by `author`.
fn emit_comment(
    sink: &ainb_hangar_daemon::events::EventSink,
    issue_id: &str,
    comment_id: &str,
    author: &str,
) {
    sink.emit(
        WS_ID,
        HangarEvent::CommentAdded(CommentRow {
            id: CommentId::from_str(comment_id.to_string()).unwrap(),
            issue_id: IssueId::from_str(issue_id.to_string()).unwrap(),
            author: author.to_string(),
            body: "look at this".into(),
            created_at: 1,
        }),
    );
}

/// Poll until `issue_id` has at least `want` inbox entries, then return every
/// recipient addressed for it.
async fn recipients_for_issue(store: &Store, issue_id: &str, want: usize) -> Vec<String> {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let rows: Vec<(String, String)> = sqlx::query_as(
            "SELECT recipient_type, recipient_id FROM inbox_entry \
             WHERE subject_id IN (SELECT id FROM comment WHERE issue_id = ?1) OR subject_id = ?1 \
                OR subject_id LIKE ?2",
        )
        .bind(issue_id)
        .bind(format!("{issue_id}%"))
        .fetch_all(store.pool())
        .await
        .expect("read inbox");
        let out: Vec<String> = rows.into_iter().map(|(k, i)| format!("{k}:{i}")).collect();
        if out.len() >= want || std::time::Instant::now() > deadline {
            return out;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// THE PROOF: a watcher who is neither creator nor assignee is notified.
///
/// The participant approximation this replaced could not reach `member:watcher`
/// by construction, so reverting `recipients_for` to `issue_participants` fails
/// this test.
#[tokio::test]
async fn a_manual_subscriber_who_is_neither_creator_nor_assignee_gets_the_comment_entry() {
    let (_dir, store, sink) = boot().await;
    seed_issue(&store, "fanout-1", "alice", Some("bob")).await;
    // Everyone who exists on the issue subscribes, PLUS a third-party watcher.
    for (actor, reason) in [
        ("alice", SubscribeReason::Creator),
        ("bob", SubscribeReason::Assignee),
        ("watcher", SubscribeReason::Manual),
    ] {
        IssueSubscriberRepo::add(store.pool(), WS_ID, "fanout-1", &member(actor), reason, 0)
            .await
            .unwrap();
    }
    sqlx::query(
        "INSERT INTO comment (id, issue_id, author_type, author_id, body, created_at) \
         VALUES ('cm-1','fanout-1','member','carol','look at this',1)",
    )
    .execute(store.pool())
    .await
    .unwrap();

    emit_comment(&sink, "fanout-1", "cm-1", "member:carol");
    let recipients = recipients_for_issue(&store, "fanout-1", 3).await;

    assert!(
        recipients.contains(&"member:watcher".to_string()),
        "a manual subscriber must be notified: {recipients:?}"
    );
    assert!(recipients.contains(&"member:alice".to_string()));
    assert!(recipients.contains(&"member:bob".to_string()));
}

/// The half of the reference's rule that must survive the conversion: you are
/// never notified of your own action.
#[tokio::test]
async fn the_comment_author_is_not_notified_of_their_own_comment() {
    let (_dir, store, sink) = boot().await;
    seed_issue(&store, "fanout-2", "alice", None).await;
    for actor in ["alice", "dave"] {
        IssueSubscriberRepo::add(
            store.pool(),
            WS_ID,
            "fanout-2",
            &member(actor),
            SubscribeReason::Manual,
            0,
        )
        .await
        .unwrap();
    }
    sqlx::query(
        "INSERT INTO comment (id, issue_id, author_type, author_id, body, created_at) \
         VALUES ('cm-2','fanout-2','member','dave','mine',1)",
    )
    .execute(store.pool())
    .await
    .unwrap();

    emit_comment(&sink, "fanout-2", "cm-2", "member:dave");
    let recipients = recipients_for_issue(&store, "fanout-2", 1).await;

    assert!(recipients.contains(&"member:alice".to_string()));
    assert!(
        !recipients.contains(&"member:dave".to_string()),
        "the author must not be notified of their own comment: {recipients:?}"
    );
}

/// The upgrade-safety fallback: an issue with ZERO subscriber rows still
/// notifies its participants, so the conversion cannot silence a notification.
#[tokio::test]
async fn an_issue_with_no_subscriber_rows_still_notifies_its_participants() {
    let (_dir, store, sink) = boot().await;
    seed_issue(&store, "fanout-3", "alice", Some("bob")).await;
    assert_eq!(
        IssueSubscriberRepo::count(store.pool(), "fanout-3").await.unwrap(),
        0,
        "the fixture deliberately has NO subscriber rows"
    );
    sqlx::query(
        "INSERT INTO comment (id, issue_id, author_type, author_id, body, created_at) \
         VALUES ('cm-3','fanout-3','member','carol','hi',1)",
    )
    .execute(store.pool())
    .await
    .unwrap();

    emit_comment(&sink, "fanout-3", "cm-3", "member:carol");
    let recipients = recipients_for_issue(&store, "fanout-3", 2).await;

    assert!(
        recipients.contains(&"member:alice".to_string())
            && recipients.contains(&"member:bob".to_string()),
        "the participant fallback must still fire: {recipients:?}"
    );
}
