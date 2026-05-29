//! Typed repository round-trips for `issue` rows, proving the polymorphic
//! `(actor_type, actor_id)` columns work through the [`IssueRepo`] layer.
//!
//! The interesting property is that the same repo API stores both a `member`
//! and an `agent` assignee (the FK-less polymorphic actor pattern, per Multica
//! architecture review §7), and that an out-of-set actor type is rejected by the
//! `CHECK` constraint at the SQL boundary.

use ainb_hangar_core::actor::{ActorKind, ActorRef};
use ainb_hangar_store::repo::issue::{Issue, IssueRepo, NewIssue};
use ainb_hangar_store::Store;

/// Seed the single `workspace` row that every `issue` row references.
async fn seed_workspace(store: &Store) -> String {
    let ws = "ws-1";
    sqlx::query("INSERT INTO workspace (id, slug, name, created_at) VALUES (?, ?, ?, ?)")
        .bind(ws)
        .bind("alpha")
        .bind("Alpha")
        .bind(0_i64)
        .execute(store.pool())
        .await
        .expect("insert workspace");
    ws.to_string()
}

#[tokio::test]
async fn insert_issue_with_member_assignee_roundtrips() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    let workspace_id = seed_workspace(&store).await;

    let new = NewIssue {
        id: "issue-1".to_string(),
        workspace_id,
        title: "Fix the thing".to_string(),
        description: Some("It is broken.".to_string()),
        state: "open".to_string(),
        assignee: Some(ActorRef::new(ActorKind::Member, "user-1").expect("actor")),
        creator: ActorRef::new(ActorKind::Member, "user-2").expect("actor"),
        created_at: 1_700_000_000_000,
    };

    IssueRepo::insert(store.pool(), &new)
        .await
        .expect("insert issue");

    let got: Issue = IssueRepo::get_by_id(store.pool(), "issue-1")
        .await
        .expect("get issue")
        .expect("issue present");

    assert_eq!(got.id, "issue-1");
    assert_eq!(got.title, "Fix the thing");
    assert_eq!(
        got.assignee,
        Some(ActorRef::new(ActorKind::Member, "user-1").expect("actor"))
    );
    assert_eq!(
        got.creator,
        ActorRef::new(ActorKind::Member, "user-2").expect("actor")
    );
    assert_eq!(got.created_at, 1_700_000_000_000);
}

#[tokio::test]
async fn insert_issue_with_agent_assignee_roundtrips() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    let workspace_id = seed_workspace(&store).await;

    let new = NewIssue {
        id: "issue-2".to_string(),
        workspace_id: workspace_id.clone(),
        title: "Agent owns this".to_string(),
        description: None,
        state: "open".to_string(),
        assignee: Some(ActorRef::new(ActorKind::Agent, "agent-1").expect("actor")),
        creator: ActorRef::new(ActorKind::Agent, "agent-2").expect("actor"),
        created_at: 1_700_000_000_001,
    };

    IssueRepo::insert(store.pool(), &new)
        .await
        .expect("insert issue");

    let got = IssueRepo::get_by_id(store.pool(), "issue-2")
        .await
        .expect("get issue")
        .expect("issue present");

    assert_eq!(
        got.assignee,
        Some(ActorRef::new(ActorKind::Agent, "agent-1").expect("actor")),
        "agent assignee must round-trip — proves polymorphism"
    );
    assert_eq!(
        got.creator,
        ActorRef::new(ActorKind::Agent, "agent-2").expect("actor")
    );

    // list_by_workspace_state finds it under the open state.
    let open = IssueRepo::list_by_workspace_state(store.pool(), &workspace_id, "open")
        .await
        .expect("list issues");
    assert!(open.iter().any(|i| i.id == "issue-2"));
}

#[tokio::test]
async fn update_state_overwrites_lifecycle_state() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    let workspace_id = seed_workspace(&store).await;

    let new = NewIssue {
        id: "issue-upd".to_string(),
        workspace_id,
        title: "Move me".to_string(),
        description: None,
        state: "open".to_string(),
        assignee: None,
        creator: ActorRef::new(ActorKind::Member, "user-1").expect("actor"),
        created_at: 1_700_000_000_000,
    };
    IssueRepo::insert(store.pool(), &new)
        .await
        .expect("insert issue");

    IssueRepo::update_state(store.pool(), "issue-upd", "done")
        .await
        .expect("update state");

    let got = IssueRepo::get_by_id(store.pool(), "issue-upd")
        .await
        .expect("get issue")
        .expect("issue present");
    assert_eq!(got.state, "done");

    // Idempotent: a second update to the same state is a no-op, not an error.
    IssueRepo::update_state(store.pool(), "issue-upd", "done")
        .await
        .expect("update state idempotent");
    // Updating an absent id affects zero rows but does not error.
    IssueRepo::update_state(store.pool(), "no-such-issue", "done")
        .await
        .expect("update absent id is not an error");
}

#[tokio::test]
async fn insert_issue_with_invalid_assignee_type_fails_at_check_constraint() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    let workspace_id = seed_workspace(&store).await;

    // Bypass the typed ActorRef and write a bogus assignee_type directly, so we
    // prove the database CHECK constraint (not just the Rust type) enforces the
    // allowed set.
    let res = sqlx::query(
        "INSERT INTO issue \
         (id, workspace_id, title, description, state, \
          assignee_type, assignee_id, creator_type, creator_id, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind("issue-bad")
    .bind(&workspace_id)
    .bind("Bad assignee")
    .bind(Option::<String>::None)
    .bind("open")
    .bind("frog") // invalid assignee_type
    .bind("x-1")
    .bind("member")
    .bind("user-1")
    .bind(0_i64)
    .execute(store.pool())
    .await;

    let err = res.expect_err("invalid assignee_type must violate CHECK constraint");
    let msg = err.to_string().to_lowercase();
    assert!(
        msg.contains("check") || msg.contains("constraint"),
        "expected a CHECK constraint failure, got: {err}"
    );
}
