//! The autopilot COLLABORATOR + SUBSCRIBER sets at the store layer (multica
//! parity #27, migration 0064).
//!
//! The headline test is the item's acceptance sentence, proved the only way
//! that means anything: the grant is written, the pool is **dropped**, a NEW
//! pool is opened on the SAME file, and the row is still there. An in-memory
//! assert would pass even if the write never reached disk.
//!
//! The rest pin the contract the set-membership shape implies: a re-add is
//! inert and keeps the FIRST grant (so `set_role` is the only role mutator), a
//! foreign tenant's write lands nothing rather than erroring, and remove is
//! idempotent.

use ainb_hangar_core::actor::{ActorKind, ActorRef};
use ainb_hangar_core::clock::FixedClock;
use ainb_hangar_core::ids::{AgentId, WorkspaceId};
use ainb_hangar_store::Store;
use ainb_hangar_store::repo::autopilot::{
    AutopilotRepo, ConcurrencyPolicy, ExecutionMode, NewAutopilot,
};
use ainb_hangar_store::repo::autopilot_access::{
    AutopilotCollaboratorRepo, AutopilotSubscriberRepo, CollaboratorRole,
};

/// Fixed clock instant (epoch-ms, 2026-01-01T00:00:00Z).
const T0: i64 = 1_767_225_600_000;

fn ws1() -> WorkspaceId {
    WorkspaceId::from_str("ws-1").unwrap()
}
fn ws2() -> WorkspaceId {
    WorkspaceId::from_str("ws-2").unwrap()
}
fn bob() -> ActorRef {
    ActorRef::new(ActorKind::Member, "user-bob").unwrap()
}
fn amy() -> ActorRef {
    ActorRef::new(ActorKind::Member, "user-amy").unwrap()
}

async fn seed_graph(store: &Store) {
    let pool = store.pool();
    for sql in [
        "INSERT INTO workspace (id, slug, name, created_at) VALUES ('ws-1','alpha','Alpha',0)",
        "INSERT INTO workspace (id, slug, name, created_at) VALUES ('ws-2','beta','Beta',0)",
        "INSERT INTO user (id, email, created_at) VALUES ('user-amy','amy@example.com',0)",
        "INSERT INTO user (id, email, created_at) VALUES ('user-bob','bob@example.com',0)",
        "INSERT INTO agent_runtime (id, workspace_id, daemon_id, provider, runtime_mode) \
         VALUES ('rt-1','ws-1','daemon-1','claude','local')",
        "INSERT INTO agent (id, workspace_id, name, runtime_id, visibility, owner_id) \
         VALUES ('agent-1','ws-1','Agent','rt-1','workspace','user-amy')",
    ] {
        sqlx::query(sql).execute(pool).await.expect(sql);
    }
}

fn new_autopilot(name: &str) -> NewAutopilot {
    NewAutopilot {
        workspace_id: ws1(),
        agent_id: AgentId::from_str("agent-1").unwrap(),
        name: name.to_string(),
        instructions: Some("nightly sweep".to_string()),
        cron_expr: "0 3 * * *".to_string(),
        max_concurrent_runs: 1,
        execution_mode: ExecutionMode::RunOnly,
        concurrency_policy: ConcurrencyPolicy::Skip,
        api_trigger_enabled: false,
    }
}

/// THE ACCEPTANCE TEST: a collaborator can be added to an autopilot, and it
/// persists to sqlite — read back by a pool that did not write it.
#[tokio::test]
async fn add_then_reopen_pool_still_reads_the_row() {
    let dir = tempfile::tempdir().expect("tempdir");
    let clock = FixedClock(T0);

    let autopilot_id = {
        let store = Store::open_in(dir.path()).await.expect("open store");
        seed_graph(&store).await;
        let id = AutopilotRepo::create(store.pool(), &clock, &new_autopilot("nightly"))
            .await
            .expect("create autopilot");

        let landed = AutopilotCollaboratorRepo::add(
            store.pool(),
            ws1().as_str(),
            id.as_str(),
            &bob(),
            CollaboratorRole::Editor,
            Some(&amy()),
            T0,
        )
        .await
        .expect("add collaborator");
        assert!(landed, "the grant landed");

        // Drop the pool: everything below must come off DISK.
        store.pool().close().await;
        id
    };

    let reopened = Store::open_in(dir.path()).await.expect("reopen store");
    let rows = AutopilotCollaboratorRepo::list(reopened.pool(), autopilot_id.as_str())
        .await
        .expect("list after reopen");
    assert_eq!(rows.len(), 1, "exactly the one grant survived the reopen");
    let row = &rows[0];
    assert_eq!(row.actor, bob());
    assert_eq!(row.role, Some(CollaboratorRole::Editor));
    assert_eq!(row.role_raw, "editor");
    assert_eq!(
        row.created_by,
        Some(amy()),
        "the granting human is attributed"
    );
    assert_eq!(row.created_at, T0);
}

#[tokio::test]
async fn re_add_is_idempotent_and_keeps_the_first_grant() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    seed_graph(&store).await;
    let clock = FixedClock(T0);
    let id = AutopilotRepo::create(store.pool(), &clock, &new_autopilot("nightly"))
        .await
        .expect("create");

    assert!(
        AutopilotCollaboratorRepo::add(
            store.pool(),
            ws1().as_str(),
            id.as_str(),
            &bob(),
            CollaboratorRole::Editor,
            None,
            T0,
        )
        .await
        .expect("first add")
    );

    // A second add, with a DIFFERENT role and a LATER timestamp, must not
    // silently downgrade the grant nor re-stamp created_at.
    assert!(
        !AutopilotCollaboratorRepo::add(
            store.pool(),
            ws1().as_str(),
            id.as_str(),
            &bob(),
            CollaboratorRole::Viewer,
            None,
            T0 + 9_000,
        )
        .await
        .expect("second add"),
        "re-adding an existing collaborator reports no new row"
    );

    let row = AutopilotCollaboratorRepo::get(store.pool(), id.as_str(), &bob())
        .await
        .expect("get")
        .expect("grant present");
    assert_eq!(row.role, Some(CollaboratorRole::Editor), "first grant wins");
    assert_eq!(row.created_at, T0, "created_at was not re-stamped");

    // set_role is the ONLY role mutator.
    assert!(
        AutopilotCollaboratorRepo::set_role(
            store.pool(),
            ws1().as_str(),
            id.as_str(),
            &bob(),
            CollaboratorRole::Viewer,
        )
        .await
        .expect("set_role")
    );
    let row = AutopilotCollaboratorRepo::get(store.pool(), id.as_str(), &bob())
        .await
        .expect("get")
        .expect("grant present");
    assert_eq!(row.role, Some(CollaboratorRole::Viewer));
    assert_eq!(row.created_at, T0, "a role change is not a new grant");
}

#[tokio::test]
async fn foreign_workspace_add_writes_nothing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    seed_graph(&store).await;
    let clock = FixedClock(T0);
    let id = AutopilotRepo::create(store.pool(), &clock, &new_autopilot("nightly"))
        .await
        .expect("create");

    // ws-2 exists, but this autopilot is not in it.
    assert!(
        !AutopilotCollaboratorRepo::add(
            store.pool(),
            ws2().as_str(),
            id.as_str(),
            &bob(),
            CollaboratorRole::Editor,
            None,
            T0,
        )
        .await
        .expect("foreign add is not an error"),
        "a foreign tenant's write reports no row"
    );
    assert_eq!(
        AutopilotCollaboratorRepo::count(store.pool(), id.as_str())
            .await
            .expect("count"),
        0,
        "and wrote nothing"
    );
}

#[tokio::test]
async fn remove_is_idempotent() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    seed_graph(&store).await;
    let clock = FixedClock(T0);
    let id = AutopilotRepo::create(store.pool(), &clock, &new_autopilot("nightly"))
        .await
        .expect("create");

    AutopilotCollaboratorRepo::add(
        store.pool(),
        ws1().as_str(),
        id.as_str(),
        &bob(),
        CollaboratorRole::Editor,
        None,
        T0,
    )
    .await
    .expect("add");

    assert!(
        AutopilotCollaboratorRepo::remove(store.pool(), ws1().as_str(), id.as_str(), &bob())
            .await
            .expect("first remove")
    );
    assert!(
        !AutopilotCollaboratorRepo::remove(store.pool(), ws1().as_str(), id.as_str(), &bob())
            .await
            .expect("second remove"),
        "removing an absent grant is an idempotent no-op"
    );
}

#[tokio::test]
async fn subscribers_persist_and_count_per_workspace() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    seed_graph(&store).await;
    let clock = FixedClock(T0);
    let a = AutopilotRepo::create(store.pool(), &clock, &new_autopilot("nightly"))
        .await
        .expect("create a");
    let b = AutopilotRepo::create(store.pool(), &clock, &new_autopilot("weekly"))
        .await
        .expect("create b");

    for (id, actor, at) in [(&a, amy(), T0), (&a, bob(), T0 + 1), (&b, bob(), T0 + 2)] {
        assert!(
            AutopilotSubscriberRepo::add(
                store.pool(),
                ws1().as_str(),
                id.as_str(),
                &actor,
                None,
                at,
            )
            .await
            .expect("subscribe")
        );
    }

    assert_eq!(
        AutopilotSubscriberRepo::actors(store.pool(), a.as_str()).await.expect("actors"),
        vec![amy(), bob()],
        "oldest-first, deterministic within a millisecond"
    );

    let mut counts = AutopilotSubscriberRepo::counts_by_autopilot(store.pool(), ws1().as_str())
        .await
        .expect("counts");
    counts.sort();
    let mut expected = vec![(a.as_str().to_string(), 2), (b.as_str().to_string(), 1)];
    expected.sort();
    assert_eq!(
        counts, expected,
        "one GROUP BY covers the whole list screen"
    );
}
