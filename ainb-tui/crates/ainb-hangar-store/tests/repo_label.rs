//! Typed repository tests for [`LabelRepo`] (e38.10): attach/detach over the
//! `label` + `issue_label` tables, the `issue.labels` JSON-cache sync, attach
//! idempotency, and the workspace-scoping (no cross-tenant attach) guard.
//!
//! The interesting properties are that the `label` table is the source of truth
//! while the `issue.labels` JSON column trails it as a cache the mutations keep
//! in sync, and that an issue id from another tenant can never have a label
//! attached through a foreign workspace.

use ainb_hangar_core::ids::WorkspaceId;
use ainb_hangar_store::Store;
use ainb_hangar_store::repo::label::{LabelRepo, LabelRepoError};

/// Seed a workspace + one open issue in it, returning `(workspace, issue_id)`.
async fn seed_workspace_issue(store: &Store, ws: &str, issue_id: &str) -> WorkspaceId {
    sqlx::query("INSERT INTO workspace (id, slug, name, created_at) VALUES (?, ?, ?, ?)")
        .bind(ws)
        .bind(ws)
        .bind(ws)
        .bind(0_i64)
        .execute(store.pool())
        .await
        .expect("insert workspace");
    sqlx::query(
        "INSERT INTO issue \
         (id, workspace_id, title, state, creator_type, creator_id, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(issue_id)
    .bind(ws)
    .bind("Refactor API")
    .bind("open")
    .bind("member")
    .bind("user-1")
    .bind(0_i64)
    .execute(store.pool())
    .await
    .expect("insert issue");
    WorkspaceId::from_str(ws).expect("non-empty workspace id")
}

/// Read the raw `issue.labels` JSON cache for an issue.
async fn labels_cache(store: &Store, issue_id: &str) -> String {
    sqlx::query_scalar("SELECT labels FROM issue WHERE id = ?")
        .bind(issue_id)
        .fetch_one(store.pool())
        .await
        .expect("read labels cache")
}

#[tokio::test]
async fn attach_adds_label_and_syncs_json_cache() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    let ws = seed_workspace_issue(&store, "ws-1", "issue-1").await;

    LabelRepo::attach(store.pool(), &ws, "issue-1", "bug", Some("#ff0000"))
        .await
        .expect("attach bug");
    LabelRepo::attach(store.pool(), &ws, "issue-1", "p1", None)
        .await
        .expect("attach p1");

    // The join is the source of truth: both labels are attached, name-ordered.
    let names = LabelRepo::labels_for_issue(store.pool(), &ws, "issue-1")
        .await
        .expect("list labels");
    assert_eq!(names, vec!["bug".to_string(), "p1".to_string()]);

    // The JSON cache the issues_list snapshot reads is kept in sync (name-ordered).
    assert_eq!(labels_cache(&store, "issue-1").await, r#"["bug","p1"]"#);

    // The label definition carries the colour supplied on first attach.
    let defs = LabelRepo::list(store.pool(), &ws).await.expect("list defs");
    let bug = defs.iter().find(|l| l.name == "bug").expect("bug present");
    assert_eq!(bug.color.as_deref(), Some("#ff0000"));
}

#[tokio::test]
async fn attach_is_idempotent() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    let ws = seed_workspace_issue(&store, "ws-1", "issue-1").await;

    LabelRepo::attach(store.pool(), &ws, "issue-1", "bug", None)
        .await
        .expect("attach once");
    LabelRepo::attach(store.pool(), &ws, "issue-1", "bug", None)
        .await
        .expect("attach twice (idempotent)");

    let names = LabelRepo::labels_for_issue(store.pool(), &ws, "issue-1")
        .await
        .expect("list labels");
    assert_eq!(names, vec!["bug".to_string()], "exactly one attach");
    // Only one label definition exists too (the second attach reused it).
    let defs = LabelRepo::list(store.pool(), &ws).await.expect("list defs");
    assert_eq!(defs.len(), 1, "label reused, not duplicated");
}

#[tokio::test]
async fn detach_removes_label_and_syncs_cache_idempotently() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    let ws = seed_workspace_issue(&store, "ws-1", "issue-1").await;

    LabelRepo::attach(store.pool(), &ws, "issue-1", "bug", None)
        .await
        .expect("attach bug");
    LabelRepo::attach(store.pool(), &ws, "issue-1", "p1", None)
        .await
        .expect("attach p1");

    LabelRepo::detach(store.pool(), &ws, "issue-1", "bug")
        .await
        .expect("detach bug");

    let names = LabelRepo::labels_for_issue(store.pool(), &ws, "issue-1")
        .await
        .expect("list labels");
    assert_eq!(names, vec!["p1".to_string()], "only p1 remains");
    assert_eq!(
        labels_cache(&store, "issue-1").await,
        r#"["p1"]"#,
        "cache synced"
    );

    // The label definition survives a detach (it can be shared / re-attached).
    let defs = LabelRepo::list(store.pool(), &ws).await.expect("list defs");
    assert!(defs.iter().any(|l| l.name == "bug"), "definition kept");

    // Detaching an absent / never-attached label is a no-op, not an error.
    LabelRepo::detach(store.pool(), &ws, "issue-1", "bug")
        .await
        .expect("detach absent is idempotent");
    LabelRepo::detach(store.pool(), &ws, "issue-1", "never-existed")
        .await
        .expect("detach unknown name is idempotent");
    assert_eq!(
        LabelRepo::labels_for_issue(store.pool(), &ws, "issue-1")
            .await
            .expect("list labels"),
        vec!["p1".to_string()],
        "no-op detaches left p1 in place"
    );
}

#[tokio::test]
async fn attach_rejects_cross_tenant_issue() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    // issue-1 lives in ws-1; ws-2 is a real second tenant that does NOT own it.
    let _ws1 = seed_workspace_issue(&store, "ws-1", "issue-1").await;
    sqlx::query("INSERT INTO workspace (id, slug, name, created_at) VALUES (?, ?, ?, ?)")
        .bind("ws-2")
        .bind("ws-2")
        .bind("ws-2")
        .bind(0_i64)
        .execute(store.pool())
        .await
        .expect("insert ws-2");
    let ws2 = WorkspaceId::from_str("ws-2").expect("non-empty");

    // Aim issue-1 (owned by ws-1) through ws-2: rejected, nothing written.
    let err = LabelRepo::attach(store.pool(), &ws2, "issue-1", "bug", None)
        .await
        .expect_err("cross-tenant attach must be rejected");
    assert!(
        matches!(err, LabelRepoError::IssueNotFound),
        "cross-tenant attach is an issue-not-found rejection, got {err:?}"
    );

    // issue-1 carries no labels and its cache is untouched.
    let ws1 = WorkspaceId::from_str("ws-1").expect("non-empty");
    assert!(
        LabelRepo::labels_for_issue(store.pool(), &ws1, "issue-1")
            .await
            .expect("list labels")
            .is_empty(),
        "cross-tenant attach must not add a label"
    );
    assert_eq!(
        labels_cache(&store, "issue-1").await,
        "[]",
        "cache untouched"
    );

    // No label definition leaked into ws-2 either.
    assert!(
        LabelRepo::list(store.pool(), &ws2).await.expect("list ws-2").is_empty(),
        "no label definition created on a rejected attach"
    );
}
