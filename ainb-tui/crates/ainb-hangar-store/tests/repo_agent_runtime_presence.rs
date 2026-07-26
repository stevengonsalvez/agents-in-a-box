//! Integration tests for the runtime-presence WRITER (multica gap #6, the
//! availability half): [`AgentRuntimeRepo::heartbeat`] and
//! [`AgentRuntimeRepo::sweep_presence_by_age`].
//!
//! These prove the persisted `agent_runtime.status` actually MOVES — a read-side
//! derivation alone would leave every other reader (CLI, squads, a future web
//! surface) looking at a row frozen at `online` forever. The properties under
//! test are the two age bands, band exclusivity, idempotency, the `NULL`
//! `last_seen_at` immunity, and heartbeat self-healing.

use ainb_hangar_store::Store;
use ainb_hangar_store::repo::agent::AgentRepo;
use ainb_hangar_store::repo::agent_runtime::AgentRuntimeRepo;

const WS: &str = "ws-a";
const UNSTABLE_AFTER: i64 = 5 * 60 * 1000;
const OFFLINE_AFTER: i64 = 10 * 60 * 1000;
const T0: i64 = 1_700_000_000_000;

/// Seed the workspace + user FK chain the runtime/agent rows hang off.
async fn seed_workspace(store: &Store) {
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
}

/// Insert one runtime with an explicit `(last_seen_at, status)` pair.
async fn seed_runtime(store: &Store, id: &str, last_seen_at: Option<i64>, status: &str) {
    sqlx::query(
        "INSERT INTO agent_runtime \
         (id, workspace_id, daemon_id, provider, runtime_mode, last_seen_at, status) \
         VALUES (?, ?, ?, ?, 'local', ?, ?)",
    )
    .bind(id)
    .bind(WS)
    // The unique index is (workspace_id, daemon_id, provider): key both on the
    // runtime id so several runtimes coexist in one workspace.
    .bind(format!("daemon-{id}"))
    .bind(format!("provider-{id}"))
    .bind(last_seen_at)
    .bind(status)
    .execute(store.pool())
    .await
    .expect("runtime");
}

/// The persisted status of one runtime.
async fn status_of(store: &Store, id: &str) -> String {
    AgentRuntimeRepo::get(store.pool(), id)
        .await
        .expect("get")
        .expect("runtime present")
        .status
}

#[tokio::test]
async fn sweep_walks_online_through_unstable_to_offline_and_is_idempotent() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("store");
    seed_workspace(&store).await;
    seed_runtime(&store, "rt", Some(T0), "online").await;

    // Fresh: inside the unstable threshold, nothing moves.
    let sweep = AgentRuntimeRepo::sweep_presence_by_age(
        store.pool(),
        T0 + 60_000,
        UNSTABLE_AFTER,
        OFFLINE_AFTER,
    )
    .await
    .expect("sweep");
    assert!(sweep.is_empty(), "a fresh heartbeat moves nothing");
    assert_eq!(status_of(&store, "rt").await, "online");

    // Past the unstable threshold: amber, persisted.
    let sweep = AgentRuntimeRepo::sweep_presence_by_age(
        store.pool(),
        T0 + 6 * 60_000,
        UNSTABLE_AFTER,
        OFFLINE_AFTER,
    )
    .await
    .expect("sweep");
    assert_eq!(
        sweep.to_unstable.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
        vec!["rt"],
    );
    assert!(sweep.to_offline.is_empty(), "still inside the grace window");
    assert_eq!(
        sweep.to_unstable[0].workspace_id, WS,
        "workspace carried for the event fan-out"
    );
    assert_eq!(
        status_of(&store, "rt").await,
        "unstable",
        "the row itself moved"
    );

    // A second pass at the same instant is a no-op (source-status constrained).
    let again = AgentRuntimeRepo::sweep_presence_by_age(
        store.pool(),
        T0 + 6 * 60_000,
        UNSTABLE_AFTER,
        OFFLINE_AFTER,
    )
    .await
    .expect("sweep");
    assert!(again.is_empty(), "idempotent: the backlog already moved");

    // Past the grace window: offline, from unstable.
    let sweep = AgentRuntimeRepo::sweep_presence_by_age(
        store.pool(),
        T0 + 11 * 60_000,
        UNSTABLE_AFTER,
        OFFLINE_AFTER,
    )
    .await
    .expect("sweep");
    assert_eq!(
        sweep.to_offline.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
        vec!["rt"],
    );
    assert_eq!(status_of(&store, "rt").await, "offline");

    // And once offline it stays put.
    let again = AgentRuntimeRepo::sweep_presence_by_age(
        store.pool(),
        T0 + 60 * 60_000,
        UNSTABLE_AFTER,
        OFFLINE_AFTER,
    )
    .await
    .expect("sweep");
    assert!(again.is_empty(), "terminal band is idempotent too");
}

#[tokio::test]
async fn sweep_bands_are_exclusive_and_null_heartbeat_is_immune() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("store");
    seed_workspace(&store).await;
    seed_runtime(&store, "fresh", Some(T0), "online").await;
    seed_runtime(&store, "amber", Some(T0 - 6 * 60_000), "online").await;
    seed_runtime(&store, "gone", Some(T0 - 20 * 60_000), "online").await;
    seed_runtime(&store, "legacy", None, "online").await;

    let sweep =
        AgentRuntimeRepo::sweep_presence_by_age(store.pool(), T0, UNSTABLE_AFTER, OFFLINE_AFTER)
            .await
            .expect("sweep");

    let unstable: Vec<&str> = sweep.to_unstable.iter().map(|r| r.id.as_str()).collect();
    let offline: Vec<&str> = sweep.to_offline.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(unstable, vec!["amber"], "only the mid-band row is amber");
    assert_eq!(
        offline,
        vec!["gone"],
        "a row past the grace window skips amber entirely"
    );

    assert_eq!(status_of(&store, "fresh").await, "online");
    assert_eq!(status_of(&store, "amber").await, "unstable");
    assert_eq!(status_of(&store, "gone").await, "offline");
    assert_eq!(
        status_of(&store, "legacy").await,
        "online",
        "a NULL last_seen_at carries no liveness signal and must never decay",
    );
}

#[tokio::test]
async fn heartbeat_stamps_last_seen_and_heals_a_decayed_runtime() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("store");
    seed_workspace(&store).await;
    seed_runtime(&store, "rt", Some(T0 - 30 * 60_000), "offline").await;
    seed_runtime(&store, "other", Some(T0 - 30 * 60_000), "offline").await;

    assert!(
        AgentRuntimeRepo::heartbeat(store.pool(), "rt", T0).await.expect("heartbeat"),
        "an existing runtime reports the update",
    );
    let row = AgentRuntimeRepo::get(store.pool(), "rt").await.expect("get").expect("present");
    assert_eq!(row.status, "online", "a beat heals a decayed runtime");
    assert_eq!(row.last_seen_at, Some(T0));

    assert_eq!(
        status_of(&store, "other").await,
        "offline",
        "a beat is targeted by id and never revives a foreign runtime",
    );

    assert!(
        !AgentRuntimeRepo::heartbeat(store.pool(), "no-such-runtime", T0)
            .await
            .expect("heartbeat"),
        "an unknown id updates nothing",
    );
}

#[tokio::test]
async fn list_ids_by_runtime_returns_active_agents_of_that_runtime_only() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("store");
    seed_workspace(&store).await;
    seed_runtime(&store, "rt", Some(T0), "online").await;
    seed_runtime(&store, "rt2", Some(T0), "online").await;
    for (id, runtime, archived) in [
        ("a-live", "rt", 0),
        ("a-archived", "rt", 1),
        ("a-elsewhere", "rt2", 0),
    ] {
        sqlx::query(
            "INSERT INTO agent (id, workspace_id, name, runtime_id, visibility, owner_id, archived) \
             VALUES (?, ?, ?, ?, 'workspace', 'u', ?)",
        )
        .bind(id)
        .bind(WS)
        .bind(id)
        .bind(runtime)
        .bind(archived)
        .execute(store.pool())
        .await
        .expect("agent");
    }

    let ids = AgentRepo::list_ids_by_runtime(store.pool(), "rt").await.expect("list");
    assert_eq!(ids, vec!["a-live".to_string()]);
}
