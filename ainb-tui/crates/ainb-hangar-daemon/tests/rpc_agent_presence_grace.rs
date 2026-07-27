//! Integration: availability decays with the runtime heartbeat (multica gap #6,
//! the availability half — the sibling of `rpc_agent_workload.rs`).
//!
//! Two layers are proven independently because both are load-bearing:
//!
//! * the READ fold — `agents_list` derives `ActorRow.presence` from the runtime's
//!   `last_seen_at` age, so the Agents screen is correct on any snapshot pull
//!   even with no live daemon and no tick latency;
//! * the WRITER — `sweep_runtime_presence` moves the persisted
//!   `agent_runtime.status` (and beats for the daemon's own runtime first), so
//!   every other reader sees the truth and the TUI gets an `AgentPresence` event
//!   to re-render on.
//!
//! A read-only fold would pass the snapshot assertions and silently fail the
//! stored-status ones, which is why both are here.

use ainb_hangar_core::clock::FixedClock;
use ainb_hangar_daemon::events::EventBroker;
use ainb_hangar_daemon::rpc::snapshots;
use ainb_hangar_daemon::sweeper::sweep_runtime_presence;
use ainb_hangar_proto::events::{HangarEvent, PresenceState, Workload};
use ainb_hangar_store::Store;
use ainb_hangar_store::repo::agent_runtime::AgentRuntimeRepo;

const WS: &str = "ws-a";
const RT: &str = "rt-a";
const AGENT: &str = "agent-a";
/// The instant the seeded runtime last beat.
const T: i64 = 1_700_000_000_000;
const MIN: i64 = 60_000;

/// Seed a workspace with one runtime that beat at `T` and one agent on it.
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
        "INSERT INTO agent_runtime \
         (id, workspace_id, daemon_id, provider, runtime_mode, last_seen_at, status) \
         VALUES (?, ?, 'd', 'claude', 'local', ?, 'online')",
    )
    .bind(RT)
    .bind(WS)
    .bind(T)
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

/// The agent's row in an `agents_list` snapshot taken at `now_ms`.
async fn agent_row(store: &Store, now_ms: i64) -> ainb_hangar_proto::events::ActorRow {
    snapshots::agents_list(store.pool(), WS, now_ms)
        .await
        .expect("agents_list")
        .into_iter()
        .find(|a| a.actor_ref == format!("agent:{AGENT}"))
        .expect("agent row present")
}

/// The runtime's persisted liveness status.
async fn stored_status(store: &Store) -> String {
    AgentRuntimeRepo::get(store.pool(), RT)
        .await
        .expect("get")
        .expect("runtime present")
        .status
}

#[tokio::test]
async fn agents_list_presence_decays_with_the_heartbeat_age() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("store");
    seed(&store).await;

    let row = agent_row(&store, T + MIN).await;
    assert_eq!(
        row.presence,
        PresenceState::Online,
        "a fresh beat is online"
    );
    assert_eq!(
        row.workload,
        Workload::Idle,
        "availability and workload stay independent dimensions",
    );

    assert_eq!(
        agent_row(&store, T + 6 * MIN).await.presence,
        PresenceState::Unstable,
        "past the 5min threshold the dot goes amber",
    );
    assert_eq!(
        agent_row(&store, T + 11 * MIN).await.presence,
        PresenceState::Offline,
        "past the 10min grace window the agent is gone",
    );

    assert_eq!(
        stored_status(&store).await,
        "online",
        "the read fold is pure: reading a snapshot never writes",
    );
}

#[tokio::test]
async fn sweep_writes_the_decayed_status_and_is_idempotent() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("store");
    seed(&store).await;

    let sweep = sweep_runtime_presence(store.pool(), &FixedClock(T + 6 * MIN), None)
        .await
        .expect("sweep");
    assert_eq!(
        sweep.to_unstable.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
        vec![RT],
    );
    assert_eq!(
        stored_status(&store).await,
        "unstable",
        "the WRITER moved the row in sqlite, not just the snapshot",
    );

    let sweep = sweep_runtime_presence(store.pool(), &FixedClock(T + 11 * MIN), None)
        .await
        .expect("sweep");
    assert_eq!(
        sweep.to_offline.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
        vec![RT],
    );
    assert_eq!(stored_status(&store).await, "offline");

    let again = sweep_runtime_presence(store.pool(), &FixedClock(T + 11 * MIN), None)
        .await
        .expect("sweep");
    assert!(
        again.is_empty(),
        "a repeat pass over the same backlog moves nothing"
    );
}

#[tokio::test]
async fn a_returning_daemon_heals_its_runtime_via_the_heartbeat() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("store");
    seed(&store).await;

    // Decay it all the way out first.
    sweep_runtime_presence(store.pool(), &FixedClock(T + 11 * MIN), None)
        .await
        .expect("sweep");
    assert_eq!(stored_status(&store).await, "offline");

    AgentRuntimeRepo::heartbeat(store.pool(), RT, T + 12 * MIN)
        .await
        .expect("heartbeat");
    assert_eq!(stored_status(&store).await, "online");
    assert_eq!(
        agent_row(&store, T + 12 * MIN + 1_000).await.presence,
        PresenceState::Online,
        "the snapshot follows the fresh beat straight back to online",
    );
}

#[tokio::test]
async fn a_daemon_never_sweeps_its_own_runtime() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("store");
    seed(&store).await;

    // Far past the grace window — but this pass owns RT, so it beats first.
    let far_future = T + 24 * 60 * MIN;
    let sweep = sweep_runtime_presence(store.pool(), &FixedClock(far_future), Some(RT))
        .await
        .expect("sweep");
    assert!(
        sweep.is_empty(),
        "our own runtime was beaten before the sweep saw it"
    );
    assert_eq!(stored_status(&store).await, "online");
    assert_eq!(
        agent_row(&store, far_future).await.presence,
        PresenceState::Online,
    );
}

#[tokio::test]
async fn a_sweep_pushes_agent_presence_on_the_workspace_stream() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("store");
    seed(&store).await;

    let broker = EventBroker::new();
    let mut rx = broker.subscribe();
    let handle = ainb_hangar_daemon::run_loop::spawn_runtime_presence(
        store.pool().clone(),
        None,
        std::time::Duration::from_millis(20),
        std::sync::Arc::new(FixedClock(T + 6 * MIN)),
        broker.sink(),
    );

    let scoped = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
        .await
        .expect("an AgentPresence event within the budget")
        .expect("event channel open");
    handle.abort();

    assert_eq!(scoped.workspace_id, WS, "scoped to the runtime's workspace");
    match scoped.event {
        HangarEvent::AgentPresence { agent_id, state } => {
            assert_eq!(agent_id.as_str(), AGENT);
            assert_eq!(state, PresenceState::Unstable);
        }
        other => panic!("expected AgentPresence, got {other:?}"),
    }
    assert_eq!(
        stored_status(&store).await,
        "unstable",
        "the loop wrote the row it announced",
    );
}
