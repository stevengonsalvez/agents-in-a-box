//! Integration tests for the workload dimension's store backing (multica gap #6):
//! [`TaskRepo::live_workload_by_workspace`] (the batch map) and
//! [`TaskRepo::live_workload_for_agent`] (the single-row CRUD backing). The
//! property under test is that the counts fold LIVE task counts only — terminal
//! statuses (`done`/`failed`/`cancelled`) never count — and that
//! [`Workload::derive`] over them walks `Idle → Queued → Working → Idle` as a
//! task moves through its real lifecycle via [`TaskRepo::transition_status`] (the
//! same call the Kanban card-move uses).

use ainb_hangar_core::task_status::TaskStatus;
use ainb_hangar_proto::events::Workload;
use ainb_hangar_store::Store;
use ainb_hangar_store::repo::task::{NewTask, TaskRepo};

const WS: &str = "ws-a";
const RT: &str = "rt-a";
const AGENT: &str = "agent-a";

/// Seed the minimal FK chain (workspace / user / online runtime / agent) so task
/// rows insert cleanly and the agent reads as `online` (availability), leaving
/// workload the only dimension under test.
async fn seed(store: &Store) {
    sqlx::query("INSERT INTO workspace (id, slug, name, created_at) VALUES (?, ?, ?, 0)")
        .bind(WS)
        .bind(WS)
        .bind(WS)
        .execute(store.pool())
        .await
        .expect("ws");
    sqlx::query("INSERT INTO user (id, email, created_at) VALUES ('u', 'u@x.test', 0)")
        .execute(store.pool())
        .await
        .expect("user");
    sqlx::query(
        "INSERT INTO agent_runtime (id, workspace_id, daemon_id, provider, runtime_mode, status) \
         VALUES (?, ?, 'd', 'claude', 'local', 'online')",
    )
    .bind(RT)
    .bind(WS)
    .execute(store.pool())
    .await
    .expect("runtime");
    sqlx::query(
        "INSERT INTO agent (id, workspace_id, name, runtime_id, visibility, owner_id) \
         VALUES (?, ?, ?, ?, 'workspace', 'u')",
    )
    .bind(AGENT)
    .bind(WS)
    .bind(AGENT)
    .bind(RT)
    .execute(store.pool())
    .await
    .expect("agent");
}

fn new_task(id: &str, created_at: i64) -> NewTask {
    NewTask {
        id: id.into(),
        workspace_id: WS.into(),
        runtime_id: RT.into(),
        agent_id: AGENT.into(),
        issue_id: None,
        work_dir: None,
        priority: 0,
        created_at,
        autopilot_run_id: None,
        generation: 0,
    }
}

/// The workload derived for `AGENT` from the batch map (defaulting a missing
/// agent to `(0, 0)` = `Idle`, the daemon's contract).
async fn agent_workload(store: &Store) -> Workload {
    let map = TaskRepo::live_workload_by_workspace(store.pool(), WS)
        .await
        .expect("batch workload");
    let (running, queued) = map.get(AGENT).copied().unwrap_or((0, 0));
    Workload::derive(running, queued)
}

/// A task walking its real lifecycle drives the agent's workload
/// `Idle → Queued → Working → Idle`, and the single-agent path agrees with the
/// batch path at every step. Terminal `done` returns to `Idle` (history excluded).
#[tokio::test]
async fn workload_tracks_live_task_lifecycle() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("store");
    seed(&store).await;

    // (a) No tasks → absent from the batch map → Idle.
    assert_eq!(agent_workload(&store).await, Workload::Idle);
    assert_eq!(
        TaskRepo::live_workload_for_agent(store.pool(), AGENT).await.unwrap(),
        (0, 0),
        "no live tasks"
    );

    // (b) One queued task (insert defaults to `queued`) → Queued.
    TaskRepo::insert(store.pool(), &new_task("t-1", 100)).await.unwrap();
    assert_eq!(agent_workload(&store).await, Workload::Queued);
    assert_eq!(
        TaskRepo::live_workload_for_agent(store.pool(), AGENT).await.unwrap(),
        (0, 1),
    );

    // (c) Transition it to running → Working.
    assert!(
        TaskRepo::transition_status(store.pool(), WS, "t-1", TaskStatus::Running, 200)
            .await
            .unwrap()
    );
    assert_eq!(agent_workload(&store).await, Workload::Working);
    assert_eq!(
        TaskRepo::live_workload_for_agent(store.pool(), AGENT).await.unwrap(),
        (1, 0),
    );

    // (d) Transition it to done → Idle (terminal rows never count).
    assert!(
        TaskRepo::transition_status(store.pool(), WS, "t-1", TaskStatus::Done, 300)
            .await
            .unwrap()
    );
    assert_eq!(agent_workload(&store).await, Workload::Idle);
    assert_eq!(
        TaskRepo::live_workload_for_agent(store.pool(), AGENT).await.unwrap(),
        (0, 0),
        "a terminal task is excluded from the live counts",
    );
}

/// `dispatched` (claimed-but-not-yet-running) folds into the `queued` count, and
/// a `running` task alongside a queued one still reads `Working` (running wins).
#[tokio::test]
async fn dispatched_counts_as_queued_and_running_wins() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("store");
    seed(&store).await;

    TaskRepo::insert(store.pool(), &new_task("t-run", 100)).await.unwrap();
    TaskRepo::insert(store.pool(), &new_task("t-disp", 101)).await.unwrap();
    // Move one to running, one to dispatched.
    TaskRepo::transition_status(store.pool(), WS, "t-run", TaskStatus::Running, 200)
        .await
        .unwrap();
    TaskRepo::transition_status(store.pool(), WS, "t-disp", TaskStatus::Dispatched, 200)
        .await
        .unwrap();

    let (running, queued) = TaskRepo::live_workload_for_agent(store.pool(), AGENT).await.unwrap();
    assert_eq!(
        (running, queued),
        (1, 1),
        "dispatched folds into the queued count",
    );
    assert_eq!(
        Workload::derive(running, queued),
        Workload::Working,
        "a running task outranks a queued one",
    );
}

/// Terminal statuses (`failed`, `cancelled`) never contribute to the live counts.
#[tokio::test]
async fn terminal_statuses_never_count() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("store");
    seed(&store).await;

    for (id, status) in [
        ("t-fail", TaskStatus::Failed),
        ("t-cancel", TaskStatus::Cancelled),
    ] {
        TaskRepo::insert(store.pool(), &new_task(id, 100)).await.unwrap();
        TaskRepo::transition_status(store.pool(), WS, id, status, 200).await.unwrap();
    }

    assert_eq!(
        TaskRepo::live_workload_for_agent(store.pool(), AGENT).await.unwrap(),
        (0, 0),
        "failed + cancelled are terminal — excluded from the live dot",
    );
    assert!(
        TaskRepo::live_workload_by_workspace(store.pool(), WS).await.unwrap().is_empty(),
        "no agent has a live task",
    );
}
