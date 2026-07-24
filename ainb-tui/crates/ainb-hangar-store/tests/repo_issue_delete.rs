//! Behavioural tests for [`IssueRepo::delete_cascade`] + [`IssueRepo::delete_preview`]:
//! the one-transaction issue delete removes every dependent row in FK order
//! (comments, label links, board placements, dependency edges, terminal tasks +
//! their usage rows), preserves `run_history` (task link nulled — cost
//! accounting never shrinks), refuses while a task is ACTIVE, and is
//! workspace-scoped (a foreign-tenant id deletes nothing).

use ainb_hangar_core::actor::{ActorKind, ActorRef};
use ainb_hangar_store::Store;
use ainb_hangar_store::repo::issue::{IssueDeleteError, IssueRepo, NewIssue};
use ainb_hangar_store::repo::task::{NewTask, TaskRepo};
use sqlx::SqlitePool;

/// Seed a workspace with the runtime + user + agent a task row references.
async fn seed_workspace(pool: &SqlitePool, ws: &str) {
    sqlx::query("INSERT INTO workspace (id, slug, name, created_at) VALUES (?, ?, ?, 0)")
        .bind(ws)
        .bind(ws)
        .bind(ws)
        .execute(pool)
        .await
        .expect("workspace");
    sqlx::query(
        "INSERT INTO agent_runtime (id, workspace_id, daemon_id, provider, runtime_mode, status) \
         VALUES (?, ?, 'd', 'claude', 'local', 'online')",
    )
    .bind(format!("rt-{ws}"))
    .bind(ws)
    .execute(pool)
    .await
    .expect("runtime");
    sqlx::query("INSERT INTO user (id, email, created_at) VALUES (?, ?, 0)")
        .bind(format!("user-{ws}"))
        .bind(format!("{ws}@x.test"))
        .execute(pool)
        .await
        .expect("user");
    sqlx::query(
        "INSERT INTO agent (id, workspace_id, name, runtime_id, visibility, owner_id) \
         VALUES (?, ?, ?, ?, 'workspace', ?)",
    )
    .bind(format!("agent-{ws}"))
    .bind(ws)
    .bind(format!("agent-{ws}"))
    .bind(format!("rt-{ws}"))
    .bind(format!("user-{ws}"))
    .execute(pool)
    .await
    .expect("agent");
}

async fn seed_issue(pool: &SqlitePool, ws: &str, id: &str) {
    IssueRepo::insert(
        pool,
        &NewIssue {
            id: id.into(),
            workspace_id: ws.into(),
            title: format!("issue {id}"),
            description: None,
            state: "open".into(),
            assignee: None,
            creator: ActorRef::new(ActorKind::Member, "user-1").expect("actor"),
            created_at: 1_000,
            priority: 0,
            due_date: None,
            labels: Vec::new(),
            parent_issue_id: None,
            stage: None,
        },
    )
    .await
    .expect("insert issue");
}

/// Insert a task for `issue` and force it into `status`.
async fn seed_task(pool: &SqlitePool, ws: &str, id: &str, issue: &str, status: &str) {
    TaskRepo::insert(
        pool,
        &NewTask {
            id: id.into(),
            workspace_id: ws.into(),
            runtime_id: format!("rt-{ws}"),
            agent_id: format!("agent-{ws}"),
            issue_id: Some(issue.into()),
            work_dir: None,
            priority: 0,
            created_at: 1_000,
            autopilot_run_id: None,
            generation: 0,
        },
    )
    .await
    .expect("insert task");
    sqlx::query("UPDATE agent_task_queue SET status = ? WHERE id = ?")
        .bind(status)
        .bind(id)
        .execute(pool)
        .await
        .expect("set status");
}

async fn count(pool: &SqlitePool, sql: &str, bind: &str) -> i64 {
    sqlx::query_scalar(sql).bind(bind).fetch_one(pool).await.expect("count")
}

/// The full cascade: comments, label links, board placement, dependency edges,
/// terminal tasks + usage all go; run_history survives with its task link
/// nulled; the issue row itself is gone; the summary counts what fell.
#[tokio::test]
async fn delete_cascade_removes_dependents_and_keeps_run_history() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("store");
    let pool = store.pool();
    seed_workspace(pool, "ws-a").await;
    seed_issue(pool, "ws-a", "i-1").await;
    seed_issue(pool, "ws-a", "i-2").await;

    // Two terminal tasks: a retry child chained to its parent (the self-FK).
    seed_task(pool, "ws-a", "t-1", "i-1", "failed").await;
    seed_task(pool, "ws-a", "t-2", "i-1", "done").await;
    sqlx::query("UPDATE agent_task_queue SET parent_task_id = 't-1' WHERE id = 't-2'")
        .execute(pool)
        .await
        .expect("chain retry");
    // Usage on one task (FK: task_usage.task_id → agent_task_queue.id).
    sqlx::query(
        "INSERT INTO task_usage (task_id, workspace_id, agent_id, created_at) \
         VALUES ('t-2', 'ws-a', 'agent-ws-a', 1000)",
    )
    .execute(pool)
    .await
    .expect("usage");
    // A finalized run in the cost history (FK: run_history.task_id).
    sqlx::query(
        "INSERT INTO run_history (run_id, task_id, workspace_id, provider, finished_at, outcome) \
         VALUES ('r-1', 't-2', 'ws-a', 'claude', 2000, 'completed')",
    )
    .execute(pool)
    .await
    .expect("run history");
    // Two comments.
    for c in ["c-1", "c-2"] {
        sqlx::query(
            "INSERT INTO comment (id, issue_id, author_type, author_id, body, created_at) \
             VALUES (?, 'i-1', 'member', 'user-1', 'hi', 1000)",
        )
        .bind(c)
        .execute(pool)
        .await
        .expect("comment");
    }
    // A label link (FK: issue_label.issue_id / .label_id).
    sqlx::query("INSERT INTO label (id, workspace_id, name) VALUES ('l-1', 'ws-a', 'bug')")
        .execute(pool)
        .await
        .expect("label");
    sqlx::query("INSERT INTO issue_label (issue_id, label_id) VALUES ('i-1', 'l-1')")
        .execute(pool)
        .await
        .expect("label link");
    // A board placement + a dependency edge in each direction.
    sqlx::query(
        "INSERT INTO board (id, workspace_id, name, created_at) VALUES ('b-1', 'ws-a', 'B', 0)",
    )
    .execute(pool)
    .await
    .expect("board");
    sqlx::query("INSERT INTO board_card (board_id, issue_id, added_at) VALUES ('b-1', 'i-1', 0)")
        .execute(pool)
        .await
        .expect("card");
    sqlx::query(
        "INSERT INTO card_dependency (workspace_id, dependent_issue_id, blocker_issue_id, created_at) \
         VALUES ('ws-a', 'i-1', 'i-2', 0), ('ws-a', 'i-2', 'i-1', 0)",
    )
    .execute(pool)
    .await
    .expect("deps");
    // A beads correlation row for the issue.
    sqlx::query(
        "INSERT INTO beads_mapping (hangar_id, bd_id, hangar_kind, bd_kind, last_synced, source) \
         VALUES ('i-1', 'bd-9', 'issue', 'issue', 0, 'hangar')",
    )
    .execute(pool)
    .await
    .expect("beads mapping");
    // Inbox notifications about the issue, one of its comments, and one of its
    // tasks (by-value subject refs, migration 0021) — all must fall with the
    // issue so no notification deep-links to nothing. Plus one about the SIBLING
    // issue i-2, which must survive.
    sqlx::query(
        "INSERT INTO inbox_entry (id, workspace_id, kind, event, subject_id, summary, created_at) \
         VALUES ('n-issue',   'ws-a', 'issue',   'issue_updated', 'i-1', 's', 0), \
                ('n-comment', 'ws-a', 'comment', 'comment_added', 'c-1', 's', 0), \
                ('n-task',    'ws-a', 'task',    'task_finished', 't-2', 's', 0), \
                ('n-sibling', 'ws-a', 'issue',   'issue_updated', 'i-2', 's', 0)",
    )
    .execute(pool)
    .await
    .expect("inbox entries");

    // Preview agrees with what the cascade is about to do.
    let preview = IssueRepo::delete_preview(pool, "ws-a", "i-1")
        .await
        .expect("preview")
        .expect("issue present");
    assert_eq!(preview.title, "issue i-1");
    assert_eq!(preview.summary.comments, 2);
    assert_eq!(preview.summary.tasks, 2);
    assert_eq!(preview.summary.placements, 1);
    assert_eq!(preview.active_tasks, 0, "both tasks are terminal");

    let summary = IssueRepo::delete_cascade(pool, "ws-a", "i-1").await.expect("delete");
    assert_eq!(summary.comments, 2);
    assert_eq!(summary.tasks, 2);
    assert_eq!(summary.placements, 1);

    // The issue and every dependent row are gone.
    assert!(IssueRepo::get_by_id(pool, "i-1").await.expect("get").is_none());
    for (sql, what) in [
        (
            "SELECT COUNT(*) FROM comment WHERE issue_id = ?",
            "comments",
        ),
        (
            "SELECT COUNT(*) FROM agent_task_queue WHERE issue_id = ?",
            "tasks",
        ),
        (
            "SELECT COUNT(*) FROM issue_label WHERE issue_id = ?",
            "label links",
        ),
        (
            "SELECT COUNT(*) FROM board_card WHERE issue_id = ?",
            "placements",
        ),
        (
            "SELECT COUNT(*) FROM card_dependency WHERE dependent_issue_id = ? OR blocker_issue_id = ?",
            "dependency edges",
        ),
        (
            "SELECT COUNT(*) FROM beads_mapping WHERE hangar_id = ?",
            "beads mapping",
        ),
    ] {
        let n = if what == "dependency edges" {
            sqlx::query_scalar::<_, i64>(sql)
                .bind("i-1")
                .bind("i-1")
                .fetch_one(pool)
                .await
                .expect("count")
        } else {
            count(pool, sql, "i-1").await
        };
        assert_eq!(n, 0, "{what} cascaded");
    }
    assert_eq!(
        count(
            pool,
            "SELECT COUNT(*) FROM task_usage WHERE task_id = ?",
            "t-2"
        )
        .await,
        0,
        "usage rows follow their task"
    );
    // The issue's inbox notifications fell (issue + comment + task subjects) —
    // no notification deep-links to a deleted entity — while the sibling
    // issue's entry survives.
    let orphaned: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM inbox_entry WHERE id IN ('n-issue', 'n-comment', 'n-task')",
    )
    .fetch_one(pool)
    .await
    .expect("inbox count");
    assert_eq!(orphaned, 0, "issue/comment/task inbox entries cascaded");
    assert_eq!(
        count(
            pool,
            "SELECT COUNT(*) FROM inbox_entry WHERE subject_id = ?",
            "i-2"
        )
        .await,
        1,
        "the sibling issue's inbox entry survives"
    );
    // Cost history survives, detached from the vanished task.
    let (runs, linked): (i64, i64) = (
        count(
            pool,
            "SELECT COUNT(*) FROM run_history WHERE run_id = ?",
            "r-1",
        )
        .await,
        count(
            pool,
            "SELECT COUNT(*) FROM run_history WHERE run_id = ? AND task_id IS NOT NULL",
            "r-1",
        )
        .await,
    );
    assert_eq!(runs, 1, "run_history row kept");
    assert_eq!(linked, 0, "task link nulled");
    // The sibling issue is untouched.
    assert!(IssueRepo::get_by_id(pool, "i-2").await.expect("get").is_some());
}

/// An ACTIVE task (queued / dispatched / running) blocks the delete — nothing
/// is removed, and the error says to cancel the run first.
#[tokio::test]
async fn delete_cascade_refuses_while_a_task_is_active() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("store");
    let pool = store.pool();
    seed_workspace(pool, "ws-a").await;
    seed_issue(pool, "ws-a", "i-1").await;
    seed_task(pool, "ws-a", "t-done", "i-1", "done").await;
    seed_task(pool, "ws-a", "t-live", "i-1", "running").await;

    let err = IssueRepo::delete_cascade(pool, "ws-a", "i-1").await.expect_err("refused");
    assert!(
        matches!(err, IssueDeleteError::ActiveTasks(1)),
        "active task refusal, got {err:?}"
    );
    assert!(err.to_string().contains("cancel the run first"));

    // Nothing was removed — the terminal sibling task included.
    assert!(IssueRepo::get_by_id(pool, "i-1").await.expect("get").is_some());
    assert_eq!(
        count(
            pool,
            "SELECT COUNT(*) FROM agent_task_queue WHERE issue_id = ?",
            "i-1"
        )
        .await,
        2,
        "both task rows intact"
    );
}

/// The delete is workspace-scoped: a foreign-tenant `(id, workspace)` pair is
/// NotFound and removes nothing.
#[tokio::test]
async fn delete_cascade_is_workspace_scoped() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("store");
    let pool = store.pool();
    seed_workspace(pool, "ws-a").await;
    seed_workspace(pool, "ws-b").await;
    seed_issue(pool, "ws-a", "i-1").await;

    let err = IssueRepo::delete_cascade(pool, "ws-b", "i-1")
        .await
        .expect_err("foreign tenant");
    assert!(matches!(err, IssueDeleteError::NotFound), "got {err:?}");
    assert!(
        IssueRepo::get_by_id(pool, "i-1").await.expect("get").is_some(),
        "foreign-tenant delete touched nothing"
    );
    assert!(
        IssueRepo::delete_preview(pool, "ws-b", "i-1").await.expect("preview").is_none(),
        "preview is tenant-scoped too"
    );
}
