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
use ainb_hangar_daemon::seed::{WS_ID, seed_p4_fixture};
use ainb_hangar_store::Store;

/// Overwrite `task-1`'s row to a completed task carrying `result.pr_url`.
async fn complete_task_with_pr(
    pool: &sqlx::SqlitePool,
    task_id: &str,
    pr_url: &str,
    finished_at: i64,
) {
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

    complete_task_with_pr(
        store.pool(),
        "task-1",
        "https://example.com/pr/1",
        1_700_000_100_000,
    )
    .await;

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
    complete_task_with_pr(
        store.pool(),
        "task-1",
        "https://example.com/pr/1",
        1_700_000_100_000,
    )
    .await;

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

#[tokio::test]
async fn issues_list_scopes_pr_url_and_branch_to_the_latest_generation() {
    // Regression guard (migration 0039 x the branch/pr surface): a rerun mints a
    // fresh run generation. When the LATEST generation opened no PR and committed
    // no branch, the issue row must surface None — never the superseded prior
    // generation's stale PR/branch, which recency-ordering alone would leak
    // because the newer, value-less row is skipped by the `IS NOT NULL` filter.
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(dir.path()).await.unwrap();
    seed_p4_fixture(store.pool()).await.unwrap();

    // Generation 0 (a prior run) produced a PR AND a branch on issue-1.
    let g0 = serde_json::json!({ "content": "done", "exit_code": 0, "pr_url": "https://example.com/pr/OLD" });
    sqlx::query(
        "UPDATE agent_task_queue \
         SET status = 'done', result = ?, branch = 'ainb/old', finished_at = ?, generation = 0 \
         WHERE id = 'task-1'",
    )
    .bind(g0.to_string())
    .bind(1_700_000_100_000i64)
    .execute(store.pool())
    .await
    .unwrap();

    // Generation 1 (the rerun, the LATEST) finished later but opened no PR and
    // committed no branch — the current run produced neither.
    let g1 = serde_json::json!({ "content": "done", "exit_code": 0 });
    sqlx::query(
        "INSERT INTO agent_task_queue \
         (id, workspace_id, runtime_id, agent_id, issue_id, status, result, created_at, finished_at, generation) \
         VALUES ('task-1-g1', ?, 'runtime-1', 'agent-1', 'issue-1', 'done', ?, ?, ?, 1)",
    )
    .bind(WS_ID)
    .bind(g1.to_string())
    .bind(1_700_000_200_000i64)
    .bind(1_700_000_300_000i64)
    .execute(store.pool())
    .await
    .unwrap();

    let issues = issues_list(store.pool(), WS_ID).await.unwrap();
    let issue_1 = issues.iter().find(|i| i.id.as_str() == "issue-1").expect("issue-1 present");
    assert_eq!(
        issue_1.pr_url, None,
        "the superseded generation-0 PR must not leak onto the latest run"
    );
    assert_eq!(
        issue_1.branch, None,
        "the superseded generation-0 branch must not leak onto the latest run"
    );
}

#[tokio::test]
async fn issues_list_surfaces_issue_priority_due_date_and_labels() {
    // e38.9 guard: the snapshot mapper must thread the issue's priority,
    // due_date, and labels onto the wire IssueRow. A regression that emitted the
    // schema defaults (priority 0 / None / empty) would otherwise stay green —
    // the other tests here only assert pr_url.
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(dir.path()).await.unwrap();
    seed_p4_fixture(store.pool()).await.unwrap();

    // Set the three e38.9 fields on issue-1 (the fixture seeds them at default).
    sqlx::query("UPDATE issue SET priority = 3, due_date = ?, labels = ? WHERE id = 'issue-1'")
        .bind(1_782_777_600_000i64)
        .bind(r#"["bug","p0"]"#)
        .execute(store.pool())
        .await
        .unwrap();

    let issues = issues_list(store.pool(), WS_ID).await.unwrap();

    // POSITIVE: the per-row values reach the wire IssueRow.
    let issue_1 = issues.iter().find(|i| i.id.as_str() == "issue-1").expect("issue-1 present");
    assert_eq!(
        issue_1.priority, 3,
        "issues_list must surface issue priority"
    );
    assert_eq!(
        issue_1.due_date,
        Some(1_782_777_600_000),
        "issues_list must surface the due_date"
    );
    assert_eq!(
        issue_1.labels,
        vec!["bug".to_string(), "p0".to_string()],
        "issues_list must surface labels"
    );

    // NEGATIVE: an untouched issue keeps the schema defaults — proving the
    // mapper reads each row, not a constant.
    let issue_2 = issues.iter().find(|i| i.id.as_str() == "issue-2").expect("issue-2 present");
    assert_eq!(issue_2.priority, 0, "issue-2 keeps the default priority");
    assert_eq!(issue_2.due_date, None, "issue-2 has no due date");
    assert!(issue_2.labels.is_empty(), "issue-2 has no labels");
}
