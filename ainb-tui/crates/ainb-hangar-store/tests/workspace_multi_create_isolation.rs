//! Multica gap #4 acceptance — create → isolation → delete at the store seam.
//!
//! The tmux acceptance ("create a second workspace in the TUI, switch to it, its
//! issues/agents are isolated") reduces, at the authoritative data layer, to:
//! after [`WorkspaceRepo::create`] mints a second workspace, its issues are
//! scoped away from the default's (a fresh workspace is empty), and
//! [`WorkspaceRepo::delete`] tears the workspace's rows down while the sibling
//! survives. This proves that acceptance deterministically — no PTY, no flake —
//! against the same store the daemon (id-or-slug scoped) and the host mutator
//! both drive.
//!
//! The complementary `workspace_data_isolation.rs` proves list queries scope by
//! row id, not slug. This file proves the create/delete lifecycle that gap #4
//! adds actually produces (and removes) those isolated tenants.

use ainb_hangar_core::actor::{ActorKind, ActorRef};
use ainb_hangar_core::ids::WorkspaceId;
use ainb_hangar_store::Store;
use ainb_hangar_store::bootstrap::ensure_default_workspace;
use ainb_hangar_store::repo::issue::{IssueRepo, NewIssue};
use ainb_hangar_store::repo::workspace::{WorkspaceRepo, WorkspaceRepoError};

const OPEN: &str = "open";
/// The sentinel issue seeded into the DEFAULT workspace — must NEVER surface in
/// the freshly-created `acme` workspace.
const DEFAULT_SENTINEL: &str = "DEFAULT-ONLY sentinel issue";

fn wsid(id: &str) -> WorkspaceId {
    WorkspaceId::from_str(id.to_string()).expect("workspace id")
}

/// Seed one open issue titled `title` into `workspace_id`, returning its id.
async fn seed_issue(store: &Store, workspace_id: &str, title: &str) -> String {
    let id = format!("issue-{}", title.replace(' ', "-"));
    let creator = ActorRef::new(ActorKind::Member, "u-creator").expect("creator ref");
    IssueRepo::insert(
        store.pool(),
        &NewIssue {
            id: id.clone(),
            workspace_id: workspace_id.to_string(),
            title: title.to_string(),
            description: None,
            state: OPEN.to_string(),
            assignee: None,
            creator,
            created_at: 0,
            priority: 0,
            due_date: None,
            labels: Vec::new(),
            parent_issue_id: None,
            stage: None,
        },
    )
    .await
    .expect("insert issue");
    id
}

/// Create a second workspace, prove its issue list is isolated from the default,
/// and that the default's sentinel never leaks into it.
#[tokio::test]
async fn created_workspace_is_isolated_from_the_default() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    let default_id = ensure_default_workspace(store.pool()).await.expect("bootstrap default");

    // Seed the default with a distinguishing sentinel issue.
    seed_issue(&store, &default_id, DEFAULT_SENTINEL).await;

    // Create the second workspace (the gap #4 path).
    let acme = WorkspaceRepo::create(store.pool(), "acme", "Acme", None)
        .await
        .expect("create acme");
    assert_ne!(acme.id, default_id, "a fresh workspace has its own ULID id");

    // The default sees its sentinel; acme sees NOTHING (isolation).
    let default_issues = IssueRepo::list_by_workspace_state(store.pool(), &default_id, OPEN)
        .await
        .unwrap();
    assert!(
        default_issues.iter().any(|i| i.title == DEFAULT_SENTINEL),
        "default must still see its sentinel: {default_issues:?}"
    );
    let acme_issues =
        IssueRepo::list_by_workspace_state(store.pool(), &acme.id, OPEN).await.unwrap();
    assert!(
        acme_issues.is_empty(),
        "the freshly-created workspace must be empty (isolated): {acme_issues:?}"
    );

    // An issue created in acme stays in acme; the default never sees it.
    seed_issue(&store, &acme.id, "ACME work item").await;
    let acme_issues =
        IssueRepo::list_by_workspace_state(store.pool(), &acme.id, OPEN).await.unwrap();
    assert!(acme_issues.iter().all(|i| i.workspace_id == acme.id));
    let default_issues = IssueRepo::list_by_workspace_state(store.pool(), &default_id, OPEN)
        .await
        .unwrap();
    assert!(
        !default_issues.iter().any(|i| i.title == "ACME work item"),
        "acme's issue must never leak into the default: {default_issues:?}"
    );

    // Two workspaces, one owner member each (sharing the single bootstrap user).
    let ws_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM workspace")
        .fetch_one(store.pool())
        .await
        .unwrap();
    assert_eq!(ws_count, 2);
    let owners: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM member WHERE role = 'owner'")
        .fetch_one(store.pool())
        .await
        .unwrap();
    assert_eq!(owners, 2, "each workspace has its own owner member row");
}

/// Delete removes the created workspace and its issues; the default's sentinel
/// survives, and the last remaining workspace cannot be deleted.
#[tokio::test]
async fn delete_removes_the_created_workspace_but_spares_the_default() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    let default_id = ensure_default_workspace(store.pool()).await.expect("bootstrap default");
    seed_issue(&store, &default_id, DEFAULT_SENTINEL).await;

    let acme = WorkspaceRepo::create(store.pool(), "acme", "Acme", None).await.expect("create");
    let acme_issue = seed_issue(&store, &acme.id, "ACME work item").await;

    WorkspaceRepo::delete(store.pool(), &wsid(&acme.id)).await.expect("delete acme");

    // acme + its issue gone; default + its sentinel survive.
    let ws: Vec<String> = sqlx::query_scalar("SELECT slug FROM workspace ORDER BY created_at")
        .fetch_all(store.pool())
        .await
        .unwrap();
    assert_eq!(ws, vec!["default"], "only the default remains");
    let gone: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM issue WHERE id = ?")
        .bind(&acme_issue)
        .fetch_one(store.pool())
        .await
        .unwrap();
    assert_eq!(gone, 0, "acme's issue was torn down");
    let default_issues = IssueRepo::list_by_workspace_state(store.pool(), &default_id, OPEN)
        .await
        .unwrap();
    assert!(
        default_issues.iter().any(|i| i.title == DEFAULT_SENTINEL),
        "the default sentinel survives a sibling delete: {default_issues:?}"
    );

    // The last remaining workspace cannot be deleted.
    let err = WorkspaceRepo::delete(store.pool(), &wsid(&default_id)).await.unwrap_err();
    assert!(
        matches!(err, WorkspaceRepoError::LastWorkspace),
        "got {err:?}"
    );
}
