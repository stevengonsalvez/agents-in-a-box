//! Typed repository tests for the Kanban board surface of [`TaskRepo`] (P8.4):
//! [`TaskRepo::list_by_workspace`] (every status, oldest-first) and
//! [`TaskRepo::transition_status`] (workspace-scoped card-move with timestamp
//! coherence). The tenant guard is the property under test — a foreign workspace
//! id touches no row.

use ainb_hangar_core::task_status::TaskStatus;
use ainb_hangar_store::Store;
use ainb_hangar_store::repo::task::{NewTask, TaskRepo};

/// Seed two workspaces, each with a runtime + agent, so the tenant-scope tests
/// can prove a transition in workspace A never touches workspace B.
async fn seed_two_workspaces(store: &Store) {
    for (ws, slug, rt, agent) in [
        ("ws-a", "alpha", "rt-a", "agent-a"),
        ("ws-b", "beta", "rt-b", "agent-b"),
    ] {
        sqlx::query("INSERT INTO workspace (id, slug, name, created_at) VALUES (?, ?, ?, ?)")
            .bind(ws)
            .bind(slug)
            .bind(slug)
            .bind(0_i64)
            .execute(store.pool())
            .await
            .expect("ws");
        sqlx::query(
            "INSERT INTO agent_runtime (id, workspace_id, daemon_id, provider, runtime_mode, status) \
             VALUES (?, ?, 'd', 'claude', 'local', 'online')",
        )
        .bind(rt)
        .bind(ws)
        .execute(store.pool())
        .await
        .expect("runtime");
        sqlx::query("INSERT INTO user (id, email, created_at) VALUES (?, ?, 0)")
            .bind(format!("user-{ws}"))
            .bind(format!("{slug}@x.test"))
            .execute(store.pool())
            .await
            .expect("user");
        sqlx::query(
            "INSERT INTO agent (id, workspace_id, name, runtime_id, visibility, owner_id) \
             VALUES (?, ?, ?, ?, 'workspace', ?)",
        )
        .bind(agent)
        .bind(ws)
        .bind(agent)
        .bind(rt)
        .bind(format!("user-{ws}"))
        .execute(store.pool())
        .await
        .expect("agent");
    }
}

fn new_task(id: &str, ws: &str, rt: &str, agent: &str, created_at: i64) -> NewTask {
    NewTask {
        id: id.into(),
        workspace_id: ws.into(),
        runtime_id: rt.into(),
        agent_id: agent.into(),
        issue_id: None,
        work_dir: None,
        priority: 0,
        created_at,
        autopilot_run_id: None,
    }
}

/// `list_by_workspace` returns every task in the workspace (terminal included),
/// oldest-first, and excludes other workspaces' tasks.
#[tokio::test]
async fn list_by_workspace_returns_all_statuses_scoped() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("store");
    seed_two_workspaces(&store).await;

    TaskRepo::insert(
        store.pool(),
        &new_task("a-1", "ws-a", "rt-a", "agent-a", 100),
    )
    .await
    .unwrap();
    TaskRepo::insert(
        store.pool(),
        &new_task("a-2", "ws-a", "rt-a", "agent-a", 50),
    )
    .await
    .unwrap();
    TaskRepo::insert(
        store.pool(),
        &new_task("b-1", "ws-b", "rt-b", "agent-b", 10),
    )
    .await
    .unwrap();
    // Move a-1 to a terminal status so the list proves it still surfaces it.
    assert!(
        TaskRepo::transition_status(store.pool(), "ws-a", "a-1", TaskStatus::Done, 200)
            .await
            .unwrap()
    );

    let tasks = TaskRepo::list_by_workspace(store.pool(), "ws-a").await.unwrap();
    assert_eq!(
        tasks.len(),
        2,
        "only ws-a's tasks, including the terminal one"
    );
    // Oldest-first: a-2 (created 50) precedes a-1 (created 100).
    assert_eq!(tasks[0].id, "a-2");
    assert_eq!(tasks[1].id, "a-1");
    assert_eq!(tasks[1].status, "done");
}

/// `insert` persists `priority` (0..3 = P3..P0, higher = more urgent) and
/// `get_by_id` reads it back; the helper's default is 0 (P3).
#[tokio::test]
async fn insert_roundtrips_priority() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("store");
    seed_two_workspaces(&store).await;

    let mut urgent = new_task("a-urgent", "ws-a", "rt-a", "agent-a", 100);
    urgent.priority = 3;
    TaskRepo::insert(store.pool(), &urgent).await.unwrap();
    TaskRepo::insert(
        store.pool(),
        &new_task("a-routine", "ws-a", "rt-a", "agent-a", 200),
    )
    .await
    .unwrap();

    let urgent = TaskRepo::get_by_id(store.pool(), "a-urgent").await.unwrap().unwrap();
    assert_eq!(urgent.priority, 3, "explicit priority persists");
    let routine = TaskRepo::get_by_id(store.pool(), "a-routine").await.unwrap().unwrap();
    assert_eq!(routine.priority, 0, "default priority is 0 (P3)");
}

/// `transition_status` is workspace-scoped: a transition keyed on the wrong
/// workspace touches no row and returns `Ok(false)`.
#[tokio::test]
async fn transition_status_is_workspace_scoped() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("store");
    seed_two_workspaces(&store).await;
    TaskRepo::insert(
        store.pool(),
        &new_task("a-1", "ws-a", "rt-a", "agent-a", 100),
    )
    .await
    .unwrap();

    // Wrong workspace → no row moved.
    let moved = TaskRepo::transition_status(store.pool(), "ws-b", "a-1", TaskStatus::Done, 200)
        .await
        .unwrap();
    assert!(
        !moved,
        "a-1 belongs to ws-a; a ws-b transition must touch nothing"
    );
    let task = TaskRepo::get_by_id(store.pool(), "a-1").await.unwrap().unwrap();
    assert_eq!(task.status, "queued", "the task stays queued");

    // Right workspace → moved.
    let moved = TaskRepo::transition_status(store.pool(), "ws-a", "a-1", TaskStatus::Running, 300)
        .await
        .unwrap();
    assert!(moved);
    let task = TaskRepo::get_by_id(store.pool(), "a-1").await.unwrap().unwrap();
    assert_eq!(task.status, "running");
    assert_eq!(task.started_at, Some(300), "running stamps started_at");
}

/// Moving to a terminal status stamps `finished_at`; moving back to queued clears
/// the lifecycle timestamps (so the board's derived age/state stay coherent).
#[tokio::test]
async fn transition_status_stamps_lifecycle_timestamps() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("store");
    seed_two_workspaces(&store).await;
    TaskRepo::insert(
        store.pool(),
        &new_task("a-1", "ws-a", "rt-a", "agent-a", 100),
    )
    .await
    .unwrap();

    TaskRepo::transition_status(store.pool(), "ws-a", "a-1", TaskStatus::Running, 200)
        .await
        .unwrap();
    TaskRepo::transition_status(store.pool(), "ws-a", "a-1", TaskStatus::Done, 300)
        .await
        .unwrap();
    let task = TaskRepo::get_by_id(store.pool(), "a-1").await.unwrap().unwrap();
    assert_eq!(task.finished_at, Some(300), "terminal stamps finished_at");
    assert_eq!(task.started_at, Some(200), "running start preserved");

    // Re-queue clears the lifecycle stamps.
    TaskRepo::transition_status(store.pool(), "ws-a", "a-1", TaskStatus::Queued, 400)
        .await
        .unwrap();
    let task = TaskRepo::get_by_id(store.pool(), "a-1").await.unwrap().unwrap();
    assert_eq!(task.status, "queued");
    assert_eq!(task.started_at, None, "re-queue clears started_at");
    assert_eq!(task.finished_at, None, "re-queue clears finished_at");
    assert_eq!(task.dispatched_at, None, "re-queue clears dispatched_at");
}
