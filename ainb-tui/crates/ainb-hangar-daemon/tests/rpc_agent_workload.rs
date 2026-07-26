//! Integration: the `hangar/agents_list` snapshot carries the two-dimensional
//! presence (multica gap #6) — an agent's `ActorRow.workload` is derived from its
//! LIVE task counts, orthogonal to its `presence` availability. As a task on the
//! agent walks its lifecycle the workload flips `Idle → Queued → Working → Idle`
//! while `presence` stays `Online` throughout (proving the dimensions are
//! independent), and the single-row `agent_update` response carries the SAME
//! workload as the batch `agents_list` row for that agent.

use ainb_hangar_core::task_status::TaskStatus;
use ainb_hangar_daemon::rpc::snapshots;
use ainb_hangar_proto::events::{PresenceState, Workload};
use ainb_hangar_store::Store;
use ainb_hangar_store::repo::agent::AgentConfigUpdate;
use ainb_hangar_store::repo::task::{NewTask, TaskRepo};

const WS: &str = "ws-a";
const RT: &str = "rt-a";
const AGENT: &str = "agent-a";

/// Seed a workspace with an ONLINE runtime + one agent, so availability reads
/// `online` and workload is the only dimension that moves.
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

fn new_task(id: &str) -> NewTask {
    NewTask {
        id: id.into(),
        workspace_id: WS.into(),
        runtime_id: RT.into(),
        agent_id: AGENT.into(),
        issue_id: None,
        work_dir: None,
        priority: 0,
        created_at: 100,
        autopilot_run_id: None,
        generation: 0,
    }
}

/// A fixed "now" for the snapshot reads. The seeded runtime carries a NULL
/// `last_seen_at`, so the availability fold reads its status verbatim and this
/// value never moves presence — workload stays the only dimension under test.
const NOW: i64 = 1_700_000_000_000;

/// The agent row's workload in the current `agents_list` snapshot.
async fn agent_row(store: &Store) -> ainb_hangar_proto::events::ActorRow {
    snapshots::agents_list(store.pool(), WS, NOW)
        .await
        .expect("agents_list")
        .into_iter()
        .find(|a| a.actor_ref == format!("agent:{AGENT}"))
        .expect("agent row present")
}

#[tokio::test]
async fn agents_list_workload_tracks_lifecycle_and_agent_update_agrees() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("store");
    seed(&store).await;

    // Baseline: no tasks → Idle, availability online.
    let row = agent_row(&store).await;
    assert_eq!(row.presence, PresenceState::Online);
    assert_eq!(row.workload, Workload::Idle, "no live tasks → idle");

    // Enqueue → Queued (availability unchanged).
    TaskRepo::insert(store.pool(), &new_task("t-1")).await.unwrap();
    let row = agent_row(&store).await;
    assert_eq!(
        row.presence,
        PresenceState::Online,
        "availability unchanged"
    );
    assert_eq!(row.workload, Workload::Queued);

    // Running → Working.
    TaskRepo::transition_status(store.pool(), WS, "t-1", TaskStatus::Running, 200)
        .await
        .unwrap();
    let row = agent_row(&store).await;
    assert_eq!(row.presence, PresenceState::Online);
    assert_eq!(row.workload, Workload::Working);

    // The single-row agent_update response carries the SAME workload as the
    // batch agents_list row (a no-op rename to the same name still re-reads it).
    let updated = snapshots::agent_update(
        store.pool(),
        WS,
        AGENT,
        &AgentConfigUpdate {
            name: Some(AGENT.to_string()),
            ..AgentConfigUpdate::default()
        },
        NOW,
    )
    .await
    .expect("agent_update")
    .expect("row present");
    assert_eq!(
        updated.workload,
        Workload::Working,
        "single-row CRUD workload matches the batch snapshot",
    );

    // Done → back to Idle (terminal excluded), availability still online.
    TaskRepo::transition_status(store.pool(), WS, "t-1", TaskStatus::Done, 300)
        .await
        .unwrap();
    let row = agent_row(&store).await;
    assert_eq!(row.presence, PresenceState::Online);
    assert_eq!(row.workload, Workload::Idle, "terminal task → idle");
}
