//! REGRESSION: dispatching an issue to a squad must produce exactly ONE run,
//! never one per member (goal success criterion 2).
//!
//! # The defect this pins
//!
//! Issue `01KY7SHDMWVMHE218DV5TQRN3R` in the operator's live database produced
//! FOUR runs. Three of them landed within two seconds of each other, on agents
//! `claude`, `test` and `devops`, which is the issue's assignee plus BOTH members
//! of squad `team1`. `SquadAssignService::assign_fanout` wrote the leader brief
//! plus one task per distinct `agent` member, each stamped with the card's repo
//! so each provisioned its OWN worktree: one issue, N agents, N worktrees, all
//! doing the same work and racing each other to the same branch.
//!
//! The fix is not a smaller fan-out, it is a different model: the card in a
//! role-gated column IS the queue, and exactly one eligible agent pulls it.
//!
//! # These tests fail against the pre-change code
//!
//! `three_member_squad_yields_exactly_one_run` asserts `COUNT(*) == 1`. The old
//! code wrote 1 leader + 3 members = 4, so it fails with `4 != 1`.
//! `broadcast_is_gone_even_without_a_pipeline` asserts the same for a workspace
//! with no pipeline provisioned, where the old code wrote 1 + 3 = 4 as well.

use ainb_hangar_core::clock::FixedClock;
use ainb_hangar_core::idgen::SystemIdGen;
use ainb_hangar_core::ids::WorkspaceId;
use ainb_hangar_store::Store;
use ainb_hangar_store::service::pipeline::PipelineService;
use ainb_hangar_store::service::squad_assign::{SquadAssignRequest, SquadAssignService};
use sqlx::SqlitePool;

const NOW_MS: i64 = 1_700_000_500_000;

fn ws() -> WorkspaceId {
    WorkspaceId::from_str("ws-1".to_string()).expect("workspace id")
}

/// A workspace with a squad whose leader is `ag-lead` and which has THREE agent
/// members holding pipeline roles. Under the old code this is a 4-way broadcast.
async fn seed_squad_of_three(pool: &SqlitePool) {
    sqlx::query("INSERT INTO workspace (id, slug, name, created_at) VALUES ('ws-1','a','A',0)")
        .execute(pool)
        .await
        .expect("workspace");
    sqlx::query("INSERT INTO user (id, email, created_at) VALUES ('u-1','a@x.dev',0)")
        .execute(pool)
        .await
        .expect("user");
    // The gap #8 invocation gate defaults the invoker to the workspace OWNER, so
    // the dispatch is refused outright without this membership.
    sqlx::query("INSERT INTO member (workspace_id, user_id, role) VALUES ('ws-1','u-1','owner')")
        .execute(pool)
        .await
        .expect("owner membership");
    sqlx::query(
        "INSERT INTO agent_runtime (id, workspace_id, daemon_id, provider, runtime_mode) \
         VALUES ('rt-1','ws-1','d-1','claude','local')",
    )
    .execute(pool)
    .await
    .expect("runtime");
    // The squad row must precede its members: `squad_member.squad_id` is an FK.
    sqlx::query(
        "INSERT INTO squad (id, workspace_id, name, leader_type, leader_id, created_at) \
         VALUES ('sq-1','ws-1','team1','agent','ag-lead',0)",
    )
    .execute(pool)
    .await
    .expect("squad");
    for (id, roles) in [
        ("ag-lead", "triager,implementer"),
        ("ag-a", "implementer"),
        ("ag-b", "reviewer"),
        ("ag-c", "tester"),
    ] {
        sqlx::query(
            "INSERT INTO agent \
             (id, workspace_id, name, runtime_id, visibility, owner_id, max_concurrent_tasks) \
             VALUES (?1,'ws-1',?1,'rt-1','workspace','u-1',5)",
        )
        .bind(id)
        .execute(pool)
        .await
        .expect("agent");
        sqlx::query(
            "INSERT INTO squad_member (squad_id, member_type, member_id, role) \
             VALUES ('sq-1','agent',?1,?2)",
        )
        .bind(id)
        .bind(roles)
        .execute(pool)
        .await
        .expect("member");
    }
    sqlx::query(
        "INSERT INTO issue \
         (id, workspace_id, title, state, creator_type, creator_id, created_at) \
         VALUES ('i-1','ws-1','Ship it','open','member','u-1',0)",
    )
    .execute(pool)
    .await
    .expect("issue");
}

async fn store() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    (dir, store)
}

/// Count every task row on the issue, in ANY status. Deliberately unfiltered:
/// the defect was N rows written at once, so any filter could hide it.
async fn task_count(pool: &SqlitePool) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM agent_task_queue WHERE issue_id = 'i-1'")
        .fetch_one(pool)
        .await
        .expect("count tasks")
}

/// THE REGRESSION. A squad with a leader and three agent members yields exactly
/// ONE run. The old code wrote four.
#[tokio::test]
async fn three_member_squad_yields_exactly_one_run() {
    let (_d, s) = store().await;
    let pool = s.pool();
    seed_squad_of_three(pool).await;
    PipelineService::provision_default(pool, &ws(), &SystemIdGen, &FixedClock(NOW_MS))
        .await
        .expect("provision pipeline");

    let out = SquadAssignService::assign_fanout(
        pool,
        &ws(),
        "sq-1",
        &SquadAssignRequest {
            issue_id: Some("i-1"),
            ..SquadAssignRequest::default()
        },
        &SystemIdGen,
        &FixedClock(NOW_MS),
    )
    .await
    .expect("fanout");

    assert_eq!(
        task_count(pool).await,
        1,
        "a squad of N members must yield exactly ONE run, not one per member"
    );
    assert!(
        out.members.is_empty(),
        "there are no member dispatches under pull"
    );

    // The one run is owned by an agent that HOLDS the first stage's role
    // (`triager`), which is the leader here, and it is a real task row.
    assert!(
        !out.leader.task_id.is_empty(),
        "the pull produced a real task"
    );
    let (owner, count): (String, i64) = sqlx::query_as(
        "SELECT agent_id, COUNT(*) FROM agent_task_queue WHERE issue_id='i-1' GROUP BY agent_id",
    )
    .fetch_one(pool)
    .await
    .expect("single owner");
    assert_eq!(count, 1);
    assert_eq!(
        owner, "ag-lead",
        "only the triager could take the Triage stage"
    );
}

/// The card lands in the FIRST ROLE-GATED column (Triage), not in Backlog and not
/// spread across the board.
#[tokio::test]
async fn dispatch_places_the_card_in_the_first_role_gated_stage() {
    let (_d, s) = store().await;
    let pool = s.pool();
    seed_squad_of_three(pool).await;
    PipelineService::provision_default(pool, &ws(), &SystemIdGen, &FixedClock(NOW_MS))
        .await
        .expect("provision");

    SquadAssignService::assign_fanout(
        pool,
        &ws(),
        "sq-1",
        &SquadAssignRequest {
            issue_id: Some("i-1"),
            ..SquadAssignRequest::default()
        },
        &SystemIdGen,
        &FixedClock(NOW_MS),
    )
    .await
    .expect("fanout");

    let (name, role): (String, Option<String>) = sqlx::query_as(
        "SELECT col.name, col.services_role FROM board_card AS bc \
           JOIN board_column AS col ON col.id = bc.column_id \
          WHERE bc.issue_id = 'i-1'",
    )
    .fetch_one(pool)
    .await
    .expect("card is on the board");
    assert_eq!(name, "Triage");
    assert_eq!(role.as_deref(), Some("triager"));

    let cards: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM board_card WHERE issue_id='i-1'")
        .fetch_one(pool)
        .await
        .expect("count cards");
    assert_eq!(cards, 1, "one card, one place on the board");
}

/// Even with NO pipeline provisioned, a squad dispatch is ONE task. The
/// no-pipeline fallback briefs the leader alone: the member broadcast is gone
/// outright, not merely bypassed when a pipeline happens to exist.
#[tokio::test]
async fn broadcast_is_gone_even_without_a_pipeline() {
    let (_d, s) = store().await;
    let pool = s.pool();
    seed_squad_of_three(pool).await;

    let out = SquadAssignService::assign_fanout(
        pool,
        &ws(),
        "sq-1",
        &SquadAssignRequest {
            issue_id: Some("i-1"),
            ..SquadAssignRequest::default()
        },
        &SystemIdGen,
        &FixedClock(NOW_MS),
    )
    .await
    .expect("fanout");

    assert_eq!(
        task_count(pool).await,
        1,
        "no pipeline is still ONE task, never four"
    );
    assert!(out.members.is_empty());
    assert_eq!(out.leader.leader_agent_id, "ag-lead");
}

/// A dangling member ref STILL rejects the whole dispatch. Members are no longer
/// dispatched to, but they are still resolved and gated, so removing the
/// broadcast did not quietly remove the tenant / invocation safety checks with
/// it, and no partial state is left behind.
#[tokio::test]
async fn a_dangling_member_still_rejects_the_dispatch_with_zero_rows() {
    let (_d, s) = store().await;
    let pool = s.pool();
    seed_squad_of_three(pool).await;
    PipelineService::provision_default(pool, &ws(), &SystemIdGen, &FixedClock(NOW_MS))
        .await
        .expect("provision");
    sqlx::query(
        "INSERT INTO squad_member (squad_id, member_type, member_id, role) \
         VALUES ('sq-1','agent','ag-ghost','implementer')",
    )
    .execute(pool)
    .await
    .expect("dangling member");

    let err = SquadAssignService::assign_fanout(
        pool,
        &ws(),
        "sq-1",
        &SquadAssignRequest {
            issue_id: Some("i-1"),
            ..SquadAssignRequest::default()
        },
        &SystemIdGen,
        &FixedClock(NOW_MS),
    )
    .await;
    assert!(
        err.is_err(),
        "a dangling member ref must still reject the dispatch"
    );
    assert_eq!(
        task_count(pool).await,
        0,
        "a rejected dispatch writes nothing"
    );
    let cards: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM board_card WHERE issue_id='i-1'")
        .fetch_one(pool)
        .await
        .expect("count cards");
    assert_eq!(cards, 0, "and places no card either");
}

/// `--redundant N` is the SURVIVING form of intentional parallelism: it writes N
/// runs on one card, all sharing a `run_group`, so a deliberate cluster stays
/// distinguishable from the accidental broadcast that was removed.
#[tokio::test]
async fn redundant_opt_in_writes_n_runs_sharing_one_run_group() {
    let (_d, s) = store().await;
    let pool = s.pool();
    seed_squad_of_three(pool).await;

    let out = SquadAssignService::assign_redundant(
        pool,
        &ws(),
        "sq-1",
        &SquadAssignRequest {
            issue_id: Some("i-1"),
            ..SquadAssignRequest::default()
        },
        3,
        &SystemIdGen,
        &FixedClock(NOW_MS),
    )
    .await
    .expect("redundant dispatch");

    assert_eq!(task_count(pool).await, 3, "three deliberate runs");
    assert_eq!(
        out.members.len(),
        2,
        "first copy in leader, the rest in members"
    );

    // ONE shared run_group across all three, and it is not NULL.
    let groups: Vec<Option<String>> = sqlx::query_scalar(
        "SELECT run_group FROM agent_task_queue WHERE issue_id='i-1' ORDER BY id",
    )
    .fetch_all(pool)
    .await
    .expect("read run_group");
    assert_eq!(groups.len(), 3);
    let first = groups[0].clone().expect("run_group is stamped, not NULL");
    assert!(
        groups.iter().all(|g| g.as_deref() == Some(first.as_str())),
        "every copy of one deliberate fan-out shares a run_group: {groups:?}"
    );

    // Three DISTINCT agents, so the copies are genuinely independent attempts.
    let distinct: i64 = sqlx::query_scalar(
        "SELECT COUNT(DISTINCT agent_id) FROM agent_task_queue WHERE issue_id='i-1'",
    )
    .fetch_one(pool)
    .await
    .expect("count distinct agents");
    assert_eq!(distinct, 3);
}

/// `--redundant 1` is the ordinary single-owner dispatch, and stamps NO
/// `run_group`: an unclustered row means "nobody asked for parallelism here".
#[tokio::test]
async fn redundant_one_is_an_ordinary_single_owner_dispatch() {
    let (_d, s) = store().await;
    let pool = s.pool();
    seed_squad_of_three(pool).await;
    PipelineService::provision_default(pool, &ws(), &SystemIdGen, &FixedClock(NOW_MS))
        .await
        .expect("provision");

    SquadAssignService::assign_redundant(
        pool,
        &ws(),
        "sq-1",
        &SquadAssignRequest {
            issue_id: Some("i-1"),
            ..SquadAssignRequest::default()
        },
        1,
        &SystemIdGen,
        &FixedClock(NOW_MS),
    )
    .await
    .expect("dispatch");

    assert_eq!(task_count(pool).await, 1);
    let group: Option<String> =
        sqlx::query_scalar("SELECT run_group FROM agent_task_queue WHERE issue_id='i-1'")
            .fetch_one(pool)
            .await
            .expect("read run_group");
    assert_eq!(group, None, "an ordinary pull belongs to no cluster");
}

/// Redundancy is a CEILING, not a quota: asking for more copies than there are
/// eligible agents dispatches to all of them rather than failing.
#[tokio::test]
async fn redundant_more_than_the_roster_dispatches_to_everyone() {
    let (_d, s) = store().await;
    let pool = s.pool();
    seed_squad_of_three(pool).await;

    SquadAssignService::assign_redundant(
        pool,
        &ws(),
        "sq-1",
        &SquadAssignRequest {
            issue_id: Some("i-1"),
            ..SquadAssignRequest::default()
        },
        99,
        &SystemIdGen,
        &FixedClock(NOW_MS),
    )
    .await
    .expect("dispatch");

    assert_eq!(
        task_count(pool).await,
        4,
        "leader plus three members, capped by the roster"
    );
}

/// FANOUT-SEMANTICS, preserved for the surviving multi-task shape: a dependent
/// stays blocked until the blocker's WHOLE cluster has drained with a success.
///
/// This property used to be covered end-to-end by
/// `tripwire_tcp_card_dependency_chain_e2e`, which built its multi-task blocker
/// out of the squad BROADCAST. With the broadcast gone a squad blocker holds one
/// task, so that tripwire can no longer express the case, and the coverage is
/// re-homed here against `--redundant`, which is now the only way one card
/// carries several concurrent runs.
///
/// The three states that must each keep the dependent blocked: any sibling still
/// active, all siblings terminal but NONE succeeded, and a success that arrived
/// in an older generation.
#[tokio::test]
async fn dependent_waits_for_the_whole_redundant_cluster_to_drain() {
    use ainb_hangar_store::repo::card_dependency::CardDependencyRepo;

    let (_d, s) = store().await;
    let pool = s.pool();
    seed_squad_of_three(pool).await;
    sqlx::query(
        "INSERT INTO issue \
         (id, workspace_id, title, state, creator_type, creator_id, created_at) \
         VALUES ('i-dep','ws-1','dependent','open','member','u-1',0)",
    )
    .execute(pool)
    .await
    .expect("dependent issue");
    sqlx::query(
        "INSERT INTO card_dependency \
         (workspace_id, dependent_issue_id, blocker_issue_id, created_at, link_type) \
         VALUES ('ws-1','i-dep','i-1',0,'blocked_by')",
    )
    .execute(pool)
    .await
    .expect("dependency");

    // A deliberate cluster of three concurrent runs on the blocker.
    SquadAssignService::assign_redundant(
        pool,
        &ws(),
        "sq-1",
        &SquadAssignRequest {
            issue_id: Some("i-1"),
            ..SquadAssignRequest::default()
        },
        3,
        &SystemIdGen,
        &FixedClock(NOW_MS),
    )
    .await
    .expect("redundant dispatch");

    let ids: Vec<String> =
        sqlx::query_scalar("SELECT id FROM agent_task_queue WHERE issue_id='i-1' ORDER BY id")
            .fetch_all(pool)
            .await
            .expect("cluster ids");
    assert_eq!(ids.len(), 3);

    let blocked = |pool: &sqlx::SqlitePool| {
        let pool = pool.clone();
        async move {
            !CardDependencyRepo::unfinished_blockers_of(&pool, "i-dep")
                .await
                .expect("blockers")
                .is_empty()
        }
    };

    assert!(blocked(pool).await, "all three siblings still active");

    // First sibling home is NOT enough: two are still running.
    sqlx::query("UPDATE agent_task_queue SET status='done' WHERE id=?1")
        .bind(&ids[0])
        .execute(pool)
        .await
        .expect("finish first");
    assert!(
        blocked(pool).await,
        "one done, two still active: still blocked"
    );

    sqlx::query("UPDATE agent_task_queue SET status='failed' WHERE id=?1")
        .bind(&ids[1])
        .execute(pool)
        .await
        .expect("fail second");
    assert!(
        blocked(pool).await,
        "two terminal, one still active: still blocked"
    );

    // The LAST sibling drains the set, and one of them succeeded.
    sqlx::query("UPDATE agent_task_queue SET status='cancelled' WHERE id=?1")
        .bind(&ids[2])
        .execute(pool)
        .await
        .expect("cancel third");
    assert!(
        !blocked(pool).await,
        "the cluster has drained with a success, so the dependent unblocks"
    );
}

/// The other half of the same contract: a cluster that drains with NO success
/// leaves the dependent blocked, rather than unblocking on mere terminality.
#[tokio::test]
async fn a_cluster_that_drains_without_a_success_keeps_the_dependent_blocked() {
    use ainb_hangar_store::repo::card_dependency::CardDependencyRepo;

    let (_d, s) = store().await;
    let pool = s.pool();
    seed_squad_of_three(pool).await;
    sqlx::query(
        "INSERT INTO issue \
         (id, workspace_id, title, state, creator_type, creator_id, created_at) \
         VALUES ('i-dep','ws-1','dependent','open','member','u-1',0)",
    )
    .execute(pool)
    .await
    .expect("dependent issue");
    sqlx::query(
        "INSERT INTO card_dependency \
         (workspace_id, dependent_issue_id, blocker_issue_id, created_at, link_type) \
         VALUES ('ws-1','i-dep','i-1',0,'blocked_by')",
    )
    .execute(pool)
    .await
    .expect("dependency");

    SquadAssignService::assign_redundant(
        pool,
        &ws(),
        "sq-1",
        &SquadAssignRequest {
            issue_id: Some("i-1"),
            ..SquadAssignRequest::default()
        },
        2,
        &SystemIdGen,
        &FixedClock(NOW_MS),
    )
    .await
    .expect("redundant dispatch");

    sqlx::query("UPDATE agent_task_queue SET status='failed' WHERE issue_id='i-1'")
        .execute(pool)
        .await
        .expect("fail the whole cluster");

    assert!(
        !CardDependencyRepo::unfinished_blockers_of(pool, "i-dep")
            .await
            .expect("blockers")
            .is_empty(),
        "a cluster that drained without any success must NOT unblock the dependent"
    );
}
