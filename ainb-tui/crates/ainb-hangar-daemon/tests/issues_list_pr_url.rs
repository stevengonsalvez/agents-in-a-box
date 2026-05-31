//! P9.2 — daemon `hangar/issues_list` surfaces the PR URL captured by P9.1.
//!
//! The `issues_list` snapshot mapper reads `result ->> 'pr_url'` from an issue's
//! latest completed task and threads it onto the wire [`IssueRow`] so the TUI's
//! task-detail screen can badge it. These tests prove:
//!
//! - a completed task whose `result.pr_url` is set surfaces on the issue row;
//! - an issue whose tasks carry NO `pr_url` (the byte-identical pre-P9 `result`
//!   shape) surfaces `pr_url: None`, never an empty string;
//! - the **latest** finished task's URL wins when more than one task ran.

use ainb_hangar_daemon::rpc::snapshots::issues_list;
use ainb_hangar_daemon::seed::{seed_p4_fixture, WS_ID};
use ainb_hangar_store::Store;

/// Overwrite `task-1`'s row to a completed task carrying `result.pr_url`.
async fn complete_task_with_pr(pool: &sqlx::SqlitePool, task_id: &str, pr_url: &str, finished_at: i64) {
    let result = serde_json::json!({ "content": "done", "exit_code": 0, "pr_url": pr_url });
    sqlx::query(
        "UPDATE agent_task_queue SET status = 'done', result = ?, finished_at = ? WHERE id = ?",
    )
    .bind(result.to_string())
    .bind(finished_at)
    .bind(task_id)
    .execute(pool)
    .await
    .expect("complete task with pr");
}

#[tokio::test]
async fn issues_list_surfaces_completed_task_pr_url() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(dir.path()).await.unwrap();
    seed_p4_fixture(store.pool()).await.unwrap();

    complete_task_with_pr(store.pool(), "task-1", "https://example.com/pr/1", 1_700_000_100_000).await;

    let issues = issues_list(store.pool(), WS_ID).await.unwrap();
    let issue_1 = issues.iter().find(|i| i.id.as_str() == "issue-1").expect("issue-1 present");
    assert_eq!(
        issue_1.pr_url.as_deref(),
        Some("https://example.com/pr/1"),
        "issue-1 should surface its completed task's pr_url"
    );

    // An issue with no completed task (issue-2) carries no PR badge.
    let issue_2 = issues.iter().find(|i| i.id.as_str() == "issue-2").expect("issue-2 present");
    assert_eq!(issue_2.pr_url, None, "issue-2 has no PR-bearing task");
}

#[tokio::test]
async fn issues_list_omits_pr_url_when_no_task_opened_a_pr() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(dir.path()).await.unwrap();
    seed_p4_fixture(store.pool()).await.unwrap();

    // Complete task-1 with a pre-P9 result shape (no pr_url key at all).
    let result = serde_json::json!({ "content": "done", "exit_code": 0 });
    sqlx::query("UPDATE agent_task_queue SET status = 'done', result = ?, finished_at = ? WHERE id = 'task-1'")
        .bind(result.to_string())
        .bind(1_700_000_100_000i64)
        .execute(store.pool())
        .await
        .unwrap();

    let issues = issues_list(store.pool(), WS_ID).await.unwrap();
    let issue_1 = issues.iter().find(|i| i.id.as_str() == "issue-1").expect("issue-1 present");
    assert_eq!(
        issue_1.pr_url, None,
        "a result without a pr_url key must surface None, never an empty string"
    );
}

#[tokio::test]
async fn issues_list_takes_the_latest_finished_task_pr_url() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(dir.path()).await.unwrap();
    seed_p4_fixture(store.pool()).await.unwrap();

    // The fixture's task-1 finished earlier with an older PR.
    complete_task_with_pr(store.pool(), "task-1", "https://example.com/pr/1", 1_700_000_100_000).await;

    // A second, later task on the same issue opens a newer PR. Insert it
    // directly (the partial-unique pending index only guards non-terminal rows,
    // so a second `done` task on issue-1 is allowed).
    let result = serde_json::json!({ "content": "done", "exit_code": 0, "pr_url": "https://example.com/pr/2" });
    sqlx::query(
        "INSERT INTO agent_task_queue \
         (id, workspace_id, runtime_id, agent_id, issue_id, status, result, created_at, finished_at) \
         VALUES ('task-1b', ?, 'runtime-1', 'agent-1', 'issue-1', 'done', ?, ?, ?)",
    )
    .bind(WS_ID)
    .bind(result.to_string())
    .bind(1_700_000_200_000i64)
    .bind(1_700_000_300_000i64)
    .execute(store.pool())
    .await
    .unwrap();

    let issues = issues_list(store.pool(), WS_ID).await.unwrap();
    let issue_1 = issues.iter().find(|i| i.id.as_str() == "issue-1").expect("issue-1 present");
    assert_eq!(
        issue_1.pr_url.as_deref(),
        Some("https://example.com/pr/2"),
        "the latest finished task's PR URL should win"
    );
}
