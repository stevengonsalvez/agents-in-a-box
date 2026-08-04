//! The `roles covered` light: the one health signal a stalled pipeline gives.
//!
//! A role-gated column whose role NO agent holds is the pipeline's silent
//! failure. Nothing errors, no task row is written, no retry fires — the cards
//! just sit there. This is the regression that matters, so it gets the test:
//! RED with no holder, GREEN the moment an agent holds the role, through the
//! same fold the CLI strip and the Boards screen both render.
//!
//! Built from raw SQL against the migrated schema (the `pull_role_gate.rs`
//! convention) so the assertion is on the fold's own predicate and cannot be
//! masked by a repo-level guard.

use ainb_hangar_core::ids::WorkspaceId;
use ainb_hangar_store::Store;
use ainb_hangar_store::service::pipeline_health;
use sqlx::{Row, SqlitePool};

/// Frozen "now" so the stuck light is deterministic.
const NOW_MS: i64 = 1_700_000_500_000;

fn ws() -> WorkspaceId {
    WorkspaceId::from_str("ws-1".to_string()).expect("workspace id")
}

async fn seed_world(pool: &SqlitePool) {
    sqlx::query("INSERT INTO workspace (id, slug, name, created_at) VALUES ('ws-1','a','A',0)")
        .execute(pool)
        .await
        .expect("workspace");
    sqlx::query("INSERT INTO user (id, email, created_at) VALUES ('u-1','a@x.dev',0)")
        .execute(pool)
        .await
        .expect("user");
    sqlx::query(
        "INSERT INTO agent_runtime (id, workspace_id, daemon_id, provider, runtime_mode) \
         VALUES ('rt-1','ws-1','d-1','claude','local')",
    )
    .execute(pool)
    .await
    .expect("runtime");
    sqlx::query(
        "INSERT INTO board (id, workspace_id, name, created_at) VALUES ('b-1','ws-1','B',0)",
    )
    .execute(pool)
    .await
    .expect("board");
    sqlx::query(
        "INSERT INTO squad (id, workspace_id, name, leader_type, leader_id, created_at) \
         VALUES ('sq-1','ws-1','team1','agent','ag-lead',0)",
    )
    .execute(pool)
    .await
    .expect("squad");
}

async fn add_agent(pool: &SqlitePool, id: &str, roles: &str, max_concurrent: i64) {
    sqlx::query(
        "INSERT INTO agent \
         (id, workspace_id, name, runtime_id, visibility, owner_id, max_concurrent_tasks) \
         VALUES (?1,'ws-1',?1,'rt-1','workspace','u-1',?2)",
    )
    .bind(id)
    .bind(max_concurrent)
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
    .expect("squad member");
}

async fn add_column(pool: &SqlitePool, id: &str, ord: i64, role: Option<&str>, wip: Option<i64>) {
    sqlx::query(
        "INSERT INTO board_column \
         (id, board_id, ord, name, fsm_state, auto_move, services_role, wip_limit, \
          excludes_prior_agent) \
         VALUES (?1,'b-1',?2,?1,NULL,1,?3,?4,0)",
    )
    .bind(id)
    .bind(ord)
    .bind(role)
    .bind(wip)
    .execute(pool)
    .await
    .expect("column");
}

async fn snapshot(pool: &SqlitePool) -> pipeline_health::PipelineHealth {
    pipeline_health::snapshot(pool, &ws(), "b-1", NOW_MS)
        .await
        .expect("health snapshot")
}

/// A stage gated on a role NOBODY holds reports RED; hiring one agent that holds
/// it flips the SAME stage GREEN, with no other change to the board.
#[tokio::test]
async fn uncovered_role_reports_red_until_an_agent_holds_it() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("store");
    let pool = store.pool();
    seed_world(pool).await;

    add_column(pool, "Backlog", 0, None, None).await;
    add_column(pool, "Review", 1, Some("reviewer"), Some(3)).await;
    add_column(pool, "QA", 2, Some("tester"), Some(1)).await;
    // A reviewer exists; NO tester does. `implementer,reviewer` also pins the
    // comma-separated token match the pull statement gates on.
    add_agent(pool, "ag-1", "implementer,reviewer", 2).await;

    let health = snapshot(pool).await;
    let uncovered: Vec<&str> =
        health.uncovered().iter().filter_map(|s| s.services_role.as_deref()).collect();
    assert_eq!(uncovered, vec!["tester"], "only the unheld role is red");

    let qa = health.stages.iter().find(|s| s.name == "QA").expect("QA stage");
    assert!(qa.role_uncovered(), "QA has no tester: {qa:?}");
    assert_eq!(qa.role_agents, 0);

    let review = health.stages.iter().find(|s| s.name == "Review").expect("Review stage");
    assert!(
        !review.role_uncovered(),
        "Review has a reviewer: {review:?}"
    );
    assert_eq!(review.role_agents, 1);
    assert_eq!(
        review.role_agents_free, 1,
        "an idle holder can pull right now"
    );

    // An ungated column is not a pull queue and can never be uncovered.
    let backlog = health.stages.iter().find(|s| s.name == "Backlog").expect("Backlog stage");
    assert!(!backlog.is_gated());
    assert!(!backlog.role_uncovered());

    // Hire a tester. Same board, same query — the light must flip.
    add_agent(pool, "ag-2", "tester", 1).await;
    let health = snapshot(pool).await;
    assert!(
        health.uncovered().is_empty(),
        "every role is now held: {:?}",
        health.uncovered()
    );
    let qa = health.stages.iter().find(|s| s.name == "QA").expect("QA stage");
    assert_eq!(qa.role_agents, 1);
    assert_eq!(qa.role_agents_free, 1);

    // An ARCHIVED agent does not cover a role: it can never be dispatched to, so
    // counting it would make the light lie in exactly the case it exists for.
    sqlx::query("UPDATE agent SET archived = 1 WHERE id = 'ag-2'")
        .execute(pool)
        .await
        .expect("archive");
    let health = snapshot(pool).await;
    let uncovered: Vec<&str> =
        health.uncovered().iter().filter_map(|s| s.services_role.as_deref()).collect();
    assert_eq!(
        uncovered,
        vec!["tester"],
        "an archived holder does not cover a role"
    );
}

/// The fold is SKIPPED on a board that gates nothing, and the per-agent
/// active-task count it charges for is index-served.
///
/// `boards_list` re-reads this on every pushed daemon event, for every board in
/// the workspace, and `role_agents_free` runs a correlated count over
/// `agent_task_queue` once per (column x candidate agent). A plain kanban board
/// can never produce a light off that work, so it must not pay for it; and the
/// count that remains must not be a full table scan.
#[tokio::test]
async fn a_board_with_no_role_gate_skips_the_fold_and_the_agent_count_is_indexed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("store");
    let pool = store.pool();
    seed_world(pool).await;
    add_agent(pool, "ag-1", "reviewer", 2).await;

    // A plain kanban board: two columns, neither one a pull queue.
    add_column(pool, "Todo", 0, None, None).await;
    add_column(pool, "Doing", 1, None, None).await;
    assert_eq!(
        pipeline_health::snapshot_if_pipeline(pool, &ws(), "b-1", NOW_MS)
            .await
            .expect("probe"),
        None,
        "a board with no role-gated column is not a pipeline and folds nothing"
    );

    // Gate ONE column and the same board becomes a pipeline, folded in full.
    sqlx::query("UPDATE board_column SET services_role = 'reviewer' WHERE id = 'Doing'")
        .execute(pool)
        .await
        .expect("gate a column");
    let health = pipeline_health::snapshot_if_pipeline(pool, &ws(), "b-1", NOW_MS)
        .await
        .expect("probe")
        .expect("a gated column makes this a pipeline");
    assert_eq!(
        health.stages.len(),
        2,
        "the fold still covers EVERY column, gated or not"
    );
    let doing = health.stages.iter().find(|s| s.name == "Doing").expect("Doing stage");
    assert_eq!(doing.role_agents, 1, "the reviewer covers it");

    // Migration 0077: the per-agent active-task count is answered from an index
    // rather than by scanning `agent_task_queue`, which grows with task history.
    let rows = sqlx::query(
        "EXPLAIN QUERY PLAN SELECT COUNT(*) FROM agent_task_queue r \
          WHERE r.agent_id = 'ag-1' AND r.status IN ('queued','dispatched','running')",
    )
    .fetch_all(pool)
    .await
    .expect("query plan");
    let plan = rows
        .iter()
        .map(|r| r.try_get::<String, _>("detail").unwrap_or_default())
        .collect::<Vec<_>>()
        .join(" | ");
    assert!(
        plan.contains("idx_task_agent_status"),
        "the per-agent active-task count must use idx_task_agent_status, got: {plan}"
    );
    assert!(
        !plan.contains("SCAN agent_task_queue"),
        "and must not scan the table: {plan}"
    );
}
