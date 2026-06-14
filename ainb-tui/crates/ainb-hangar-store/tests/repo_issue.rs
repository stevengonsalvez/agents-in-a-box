//! Typed repository round-trips for `issue` rows, proving the polymorphic
//! `(actor_type, actor_id)` columns work through the [`IssueRepo`] layer.
//!
//! The interesting property is that the same repo API stores both a `member`
//! and an `agent` assignee (the FK-less polymorphic actor pattern, per the
//! reference architecture review §7), and that an out-of-set actor type is rejected by the
//! `CHECK` constraint at the SQL boundary.

use ainb_hangar_core::actor::{ActorKind, ActorRef};
use ainb_hangar_store::Store;
use ainb_hangar_store::repo::issue::{Issue, IssueRepo, NewIssue};

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
        priority: 0,
        due_date: None,
        labels: Vec::new(),
    };

    IssueRepo::insert(store.pool(), &new).await.expect("insert issue");

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
        priority: 0,
        due_date: None,
        labels: Vec::new(),
    };

    IssueRepo::insert(store.pool(), &new).await.expect("insert issue");

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
async fn insert_issue_roundtrips_priority_due_date_and_labels() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    let workspace_id = seed_workspace(&store).await;

    // An issue created with an explicit urgency, a due date, and two labels —
    // the three attributes the create flow now persists (migration 0014).
    let new = NewIssue {
        id: "issue-attrs".to_string(),
        workspace_id,
        title: "Urgent thing".to_string(),
        description: None,
        state: "open".to_string(),
        assignee: None,
        creator: ActorRef::new(ActorKind::Member, "user-1").expect("actor"),
        created_at: 1_700_000_000_000,
        priority: 3,
        due_date: Some(1_700_000_500_000),
        labels: vec!["bug".to_string(), "p0".to_string()],
    };
    IssueRepo::insert(store.pool(), &new).await.expect("insert issue");

    let got: Issue = IssueRepo::get_by_id(store.pool(), "issue-attrs")
        .await
        .expect("get issue")
        .expect("issue present");
    assert_eq!(got.priority, 3, "priority round-trips");
    assert_eq!(
        got.due_date,
        Some(1_700_000_500_000),
        "due_date round-trips"
    );
    assert_eq!(
        got.labels,
        vec!["bug".to_string(), "p0".to_string()],
        "labels round-trip through the JSON column"
    );
}

#[tokio::test]
async fn insert_issue_defaults_priority_due_date_and_labels() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    let workspace_id = seed_workspace(&store).await;

    // The routine create path: no urgency, no due date, no labels. The defaults
    // (priority 0, due_date None, empty labels) must round-trip.
    let new = NewIssue {
        id: "issue-default".to_string(),
        workspace_id,
        title: "Routine thing".to_string(),
        description: None,
        state: "open".to_string(),
        assignee: None,
        creator: ActorRef::new(ActorKind::Member, "user-1").expect("actor"),
        created_at: 1_700_000_000_000,
        priority: 0,
        due_date: None,
        labels: Vec::new(),
    };
    IssueRepo::insert(store.pool(), &new).await.expect("insert issue");

    let got = IssueRepo::get_by_id(store.pool(), "issue-default")
        .await
        .expect("get issue")
        .expect("issue present");
    assert_eq!(got.priority, 0, "default priority is 0");
    assert_eq!(got.due_date, None, "default due_date is None");
    assert!(got.labels.is_empty(), "default labels is empty");
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
        priority: 0,
        due_date: None,
        labels: Vec::new(),
    };
    IssueRepo::insert(store.pool(), &new).await.expect("insert issue");

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
async fn update_fields_edits_state_assignee_priority_and_due_date() {
    use ainb_hangar_store::repo::issue::IssueFieldUpdate;

    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    let workspace_id = seed_workspace(&store).await;

    let new = NewIssue {
        id: "issue-edit".to_string(),
        workspace_id: workspace_id.clone(),
        title: "Edit me".to_string(),
        description: None,
        state: "open".to_string(),
        assignee: None,
        creator: ActorRef::new(ActorKind::Member, "user-1").expect("actor"),
        created_at: 1_700_000_000_000,
        priority: 0,
        due_date: None,
        labels: Vec::new(),
    };
    IssueRepo::insert(store.pool(), &new).await.expect("insert issue");

    // A full edit: change state, assign an agent, bump priority, set a due date.
    let touched = IssueRepo::update_fields(
        store.pool(),
        &workspace_id,
        "issue-edit",
        &IssueFieldUpdate {
            state: Some("in_progress".to_string()),
            assignee: Some(Some(
                ActorRef::new(ActorKind::Agent, "agent-7").expect("actor"),
            )),
            priority: Some(3),
            due_date: Some(Some(1_700_000_900_000)),
        },
    )
    .await
    .expect("update fields");
    assert!(
        touched,
        "an in-workspace issue id must edit exactly one row"
    );

    let got: Issue = IssueRepo::get_by_id(store.pool(), "issue-edit")
        .await
        .expect("get issue")
        .expect("issue present");
    assert_eq!(got.state, "in_progress", "state edited");
    assert_eq!(
        got.assignee,
        Some(ActorRef::new(ActorKind::Agent, "agent-7").expect("actor")),
        "assignee set to an agent"
    );
    assert_eq!(got.priority, 3, "priority edited");
    assert_eq!(got.due_date, Some(1_700_000_900_000), "due_date set");

    // A partial edit (only priority) leaves every other field untouched.
    IssueRepo::update_fields(
        store.pool(),
        &workspace_id,
        "issue-edit",
        &IssueFieldUpdate {
            priority: Some(1),
            ..Default::default()
        },
    )
    .await
    .expect("partial update");
    let got = IssueRepo::get_by_id(store.pool(), "issue-edit")
        .await
        .expect("get issue")
        .expect("issue present");
    assert_eq!(got.priority, 1, "priority re-edited");
    assert_eq!(
        got.state, "in_progress",
        "state left unchanged by partial edit"
    );
    assert_eq!(
        got.assignee,
        Some(ActorRef::new(ActorKind::Agent, "agent-7").expect("actor")),
        "assignee left unchanged by partial edit"
    );
    assert_eq!(
        got.due_date,
        Some(1_700_000_900_000),
        "due_date left unchanged by partial edit"
    );

    // Clearing nullable fields (explicit Some(None)) sets them back to NULL.
    IssueRepo::update_fields(
        store.pool(),
        &workspace_id,
        "issue-edit",
        &IssueFieldUpdate {
            assignee: Some(None),
            due_date: Some(None),
            ..Default::default()
        },
    )
    .await
    .expect("clear nullable fields");
    let got = IssueRepo::get_by_id(store.pool(), "issue-edit")
        .await
        .expect("get issue")
        .expect("issue present");
    assert_eq!(got.assignee, None, "assignee cleared to NULL");
    assert_eq!(got.due_date, None, "due_date cleared to NULL");
}

#[tokio::test]
async fn update_fields_is_workspace_scoped_no_cross_tenant_edit() {
    use ainb_hangar_store::repo::issue::IssueFieldUpdate;

    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    let workspace_id = seed_workspace(&store).await;

    // A second tenant workspace.
    sqlx::query("INSERT INTO workspace (id, slug, name, created_at) VALUES (?, ?, ?, ?)")
        .bind("ws-2")
        .bind("beta")
        .bind("Beta")
        .bind(0_i64)
        .execute(store.pool())
        .await
        .expect("insert second workspace");

    let new = NewIssue {
        id: "issue-tenant".to_string(),
        workspace_id, // belongs to ws-1
        title: "Tenant A's issue".to_string(),
        description: None,
        state: "open".to_string(),
        assignee: None,
        creator: ActorRef::new(ActorKind::Member, "user-1").expect("actor"),
        created_at: 1_700_000_000_000,
        priority: 0,
        due_date: None,
        labels: Vec::new(),
    };
    IssueRepo::insert(store.pool(), &new).await.expect("insert issue");

    // Editing the issue scoped to the WRONG workspace must touch no row.
    let touched = IssueRepo::update_fields(
        store.pool(),
        "ws-2",
        "issue-tenant",
        &IssueFieldUpdate {
            state: Some("done".to_string()),
            ..Default::default()
        },
    )
    .await
    .expect("cross-tenant update is not an error, just a no-op");
    assert!(!touched, "a cross-tenant edit must touch zero rows");

    // The issue is untouched: still open in its real workspace.
    let got = IssueRepo::get_by_id(store.pool(), "issue-tenant")
        .await
        .expect("get issue")
        .expect("issue present");
    assert_eq!(got.state, "open", "cross-tenant edit must not change state");
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
