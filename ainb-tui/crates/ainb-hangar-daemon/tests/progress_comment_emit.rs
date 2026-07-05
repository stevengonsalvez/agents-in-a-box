//! e38.6 integration: the run-loop progress-comment seam writes durable,
//! agent-authored comments to a task's issue at the lifecycle checkpoints.
//!
//! ```text
//!  seed ws + issue + agent          emit_checkpoint(...)
//!         │                                 │
//!         ▼                                 ▼
//!  task(issue_id=Some) ─▶ Started ─▶ comment("started")
//!                       ─▶ Succeeded/Failed ─▶ comment("done"/"blocker")
//!  task(issue_id=None)  ─▶ Started ─▶ (no comment — chat task)
//! ```
//!
//! This is the store-level user-visible proof: rather than spin a tmux daemon
//! (the macOS CI runner is weak), the test drives the public
//! [`progress_comment::emit_checkpoint`] seam directly against a real `SQLite`
//! store and asserts the `comment` table gains the rows by querying
//! [`CommentRepo::list_by_issue`].

use ainb_hangar_core::actor::{ActorKind, ActorRef};
use ainb_hangar_core::clock::FixedClock;
use ainb_hangar_core::idgen::SystemIdGen;
use ainb_hangar_daemon::progress_comment::{Checkpoint, emit_checkpoint};
use ainb_hangar_store::Store;
use ainb_hangar_store::repo::comment::CommentRepo;
use ainb_hangar_store::repo::task::{NewTask, Task, TaskRepo};
use ainb_hangar_store::service::fail::FailureReason;
use sqlx::SqlitePool;

const WS: &str = "ws-e38-6";
const ISSUE: &str = "issue-e38-6";
const AGENT: &str = "agent-e38-6";
const RUNTIME: &str = "rt-e38-6";

/// Seed the minimal graph (workspace + member + runtime + agent + open issue)
/// the comment write path FKs / joins against.
async fn seed(pool: &SqlitePool) {
    sqlx::query("INSERT INTO workspace (id, slug, name, created_at) VALUES (?, ?, ?, ?)")
        .bind(WS)
        .bind("e38-6")
        .bind("E38.6")
        .bind(0_i64)
        .execute(pool)
        .await
        .expect("insert workspace");
    sqlx::query("INSERT INTO user (id, email, created_at) VALUES (?, ?, ?)")
        .bind("u-e38-6")
        .bind("e386@example.com")
        .bind(0_i64)
        .execute(pool)
        .await
        .expect("insert user");
    sqlx::query(
        "INSERT INTO agent_runtime (id, workspace_id, daemon_id, provider, runtime_mode) \
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(RUNTIME)
    .bind(WS)
    .bind("daemon-e38-6")
    .bind("claude")
    .bind("local")
    .execute(pool)
    .await
    .expect("insert runtime");
    sqlx::query(
        "INSERT INTO agent \
         (id, workspace_id, name, runtime_id, visibility, owner_id, max_concurrent_tasks) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(AGENT)
    .bind(WS)
    .bind("Agent")
    .bind(RUNTIME)
    .bind("workspace")
    .bind("u-e38-6")
    .bind(5_i64)
    .execute(pool)
    .await
    .expect("insert agent");
    sqlx::query(
        "INSERT INTO issue (id, workspace_id, title, state, creator_type, creator_id, created_at) \
         VALUES (?, ?, 'Build it', 'open', 'member', 'u-e38-6', 0)",
    )
    .bind(ISSUE)
    .bind(WS)
    .execute(pool)
    .await
    .expect("insert issue");
}

/// Enqueue a task with the given `issue_id` and read it straight back as a
/// fully-materialised [`Task`] (the seam takes a `&Task`).
async fn seed_task(pool: &SqlitePool, id: &str, issue_id: Option<&str>) -> Task {
    TaskRepo::insert(
        pool,
        &NewTask {
            id: id.to_string(),
            workspace_id: WS.to_string(),
            runtime_id: RUNTIME.to_string(),
            agent_id: AGENT.to_string(),
            issue_id: issue_id.map(str::to_string),
            work_dir: None,
            priority: 0,
            created_at: 0,
            autopilot_run_id: None,
            generation: 0,
        },
    )
    .await
    .expect("enqueue task");
    TaskRepo::get_by_id(pool, id).await.expect("read task").expect("task present")
}

#[tokio::test]
async fn started_and_success_checkpoints_write_agent_comments_to_the_issue() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(dir.path()).await.unwrap();
    seed(store.pool()).await;
    let task = seed_task(store.pool(), "task-success", Some(ISSUE)).await;

    let idgen = SystemIdGen;
    // Distinct timestamps: the run loop separates these two checkpoints by the
    // whole provider run (real wall-clock ms apart), so the thread orders
    // started-before-terminal. Two `FixedClock`s reproduce that ordering
    // deterministically without leaning on a same-millisecond ULID tie-break.
    emit_checkpoint(
        store.pool(),
        &idgen,
        &FixedClock(1_000),
        &task,
        Checkpoint::Started,
    )
    .await;
    emit_checkpoint(
        store.pool(),
        &idgen,
        &FixedClock(2_000),
        &task,
        Checkpoint::Succeeded,
    )
    .await;

    let comments = CommentRepo::list_by_issue(store.pool(), WS, ISSUE).await.unwrap();
    assert_eq!(
        comments.len(),
        2,
        "started + terminal comments must both land"
    );

    // Both authored by the running AGENT, not a member.
    let expected_author = ActorRef::new(ActorKind::Agent, AGENT).unwrap();
    for c in &comments {
        assert_eq!(
            c.author, expected_author,
            "comment author is the running agent"
        );
    }
    // Oldest-first: started before terminal.
    assert!(
        comments[0].body.to_lowercase().contains("start"),
        "first comment announces the run start, got {:?}",
        comments[0].body
    );
    assert!(
        comments[1].body.to_lowercase().contains("complete")
            || comments[1].body.to_lowercase().contains("done")
            || comments[1].body.to_lowercase().contains("success"),
        "terminal comment announces success, got {:?}",
        comments[1].body
    );
}

#[tokio::test]
async fn failure_checkpoint_writes_a_blocker_comment_carrying_the_reason() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(dir.path()).await.unwrap();
    seed(store.pool()).await;
    let task = seed_task(store.pool(), "task-failed", Some(ISSUE)).await;

    let idgen = SystemIdGen;
    emit_checkpoint(
        store.pool(),
        &idgen,
        &FixedClock(1_000),
        &task,
        Checkpoint::Started,
    )
    .await;
    emit_checkpoint(
        store.pool(),
        &idgen,
        &FixedClock(2_000),
        &task,
        Checkpoint::Failed {
            reason: FailureReason::Timeout,
        },
    )
    .await;

    let comments = CommentRepo::list_by_issue(store.pool(), WS, ISSUE).await.unwrap();
    assert_eq!(comments.len(), 2, "started + blocker comments");
    let blocker = &comments[1];
    assert_eq!(
        blocker.author,
        ActorRef::new(ActorKind::Agent, AGENT).unwrap()
    );
    // The terminal comment must carry the failure reason so the issue thread
    // records WHY the run stopped, not just that it did.
    assert!(
        blocker.body.contains(FailureReason::Timeout.as_db_str()),
        "blocker comment must carry the failure reason {:?}, got {:?}",
        FailureReason::Timeout.as_db_str(),
        blocker.body
    );
}

#[tokio::test]
async fn null_issue_task_writes_no_comment() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(dir.path()).await.unwrap();
    seed(store.pool()).await;
    // A chat / autopilot placeholder task carries no issue.
    let task = seed_task(store.pool(), "task-chat", None).await;

    let idgen = SystemIdGen;
    emit_checkpoint(
        store.pool(),
        &idgen,
        &FixedClock(1_000),
        &task,
        Checkpoint::Started,
    )
    .await;
    emit_checkpoint(
        store.pool(),
        &idgen,
        &FixedClock(2_000),
        &task,
        Checkpoint::Succeeded,
    )
    .await;

    // No issue exists for this task, so the issue's comment thread stays empty.
    let comments = CommentRepo::list_by_issue(store.pool(), WS, ISSUE).await.unwrap();
    assert!(
        comments.is_empty(),
        "a NULL-issue task must write no comment, got {comments:?}"
    );
}
