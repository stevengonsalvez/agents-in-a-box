//! The `api` trigger + the `skipped` run status at the store layer (multica
//! parity item 15).
//!
//! Before migration 0057, a declined dispatch was an EVENT only: the scheduler
//! logged `tick_skipped` and wrote no row, so no read path could ever tell a
//! declined dispatch from a tick that never came due. These tests pin the two
//! halves of the fix against real sqlite:
//!
//! 1. [`record_skipped_run`] persists a TERMINAL `skipped` run (its
//!    `completed_at` stamped, so it never inflates the in-flight count),
//! 2. [`dispatch_with_admission`] — the shared gate every trigger surface calls —
//!    applies the SAME concurrency policy the scheduler always did, now writing
//!    a `skipped` row on the `skip` branch and stamping `source` on every run.

use ainb_hangar_core::clock::FixedClock;
use ainb_hangar_core::ids::{AgentId, AutopilotId, WorkspaceId};
use ainb_hangar_store::Store;
use ainb_hangar_store::repo::autopilot::{
    Autopilot, AutopilotRepo, ConcurrencyPolicy, ExecutionMode, NewAutopilot,
};
use ainb_hangar_store::repo::autopilot_run::{
    DispatchOutcome, RunSource, count_in_flight, dispatch_with_admission, fire_autopilot_tick,
    fire_autopilot_tick_with_source, record_skipped_run,
};
use sqlx::Row;

/// Fixed clock instant all tests fire at (epoch-ms, 2026-01-01T00:00:00Z).
const T0: i64 = 1_767_225_600_000;

/// Seed the workspace + user + runtime + agent FK chain every autopilot needs.
async fn seed_graph(store: &Store) {
    let pool = store.pool();
    for sql in [
        "INSERT INTO workspace (id, slug, name, created_at) VALUES ('ws-1','alpha','Alpha',0)",
        "INSERT INTO workspace (id, slug, name, created_at) VALUES ('ws-2','beta','Beta',0)",
        "INSERT INTO user (id, email, created_at) VALUES ('user-1','a@example.com',0)",
        "INSERT INTO agent_runtime (id, workspace_id, daemon_id, provider, runtime_mode) \
         VALUES ('rt-1','ws-1','daemon-1','claude','local')",
        "INSERT INTO agent (id, workspace_id, name, runtime_id, visibility, owner_id) \
         VALUES ('agent-1','ws-1','Agent','rt-1','workspace','user-1')",
    ] {
        sqlx::query(sql).execute(pool).await.expect(sql);
    }
}

/// Create one autopilot with the given policy/limit and read it back.
async fn seed_autopilot(
    store: &Store,
    policy: ConcurrencyPolicy,
    max_concurrent_runs: i64,
) -> Autopilot {
    let clock = FixedClock(T0);
    let id = AutopilotRepo::create(
        store.pool(),
        &clock,
        &NewAutopilot {
            workspace_id: WorkspaceId::from_str("ws-1").unwrap(),
            agent_id: AgentId::from_str("agent-1").unwrap(),
            name: "daily".to_string(),
            instructions: Some("do the thing".to_string()),
            cron_expr: "0 9 * * *".to_string(),
            max_concurrent_runs,
            execution_mode: ExecutionMode::default(),
            concurrency_policy: policy,
            api_trigger_enabled: false,
        },
    )
    .await
    .expect("create autopilot");
    reload(store, &id).await
}

async fn reload(store: &Store, id: &AutopilotId) -> Autopilot {
    AutopilotRepo::get(store.pool(), &WorkspaceId::from_str("ws-1").unwrap(), id)
        .await
        .expect("get autopilot")
        .expect("autopilot present")
}

async fn count(store: &Store, sql: &str) -> i64 {
    sqlx::query_scalar::<_, i64>(sql)
        .fetch_one(store.pool())
        .await
        .unwrap_or_else(|e| panic!("{sql}: {e}"))
}

async fn open(dir: &tempfile::TempDir) -> Store {
    let store = Store::open_in(dir.path()).await.expect("open store");
    seed_graph(&store).await;
    store
}

/// A skipped run is a real row, and it is TERMINAL: it never counts as in flight,
/// so a later dispatch still fires.
#[tokio::test]
async fn record_skipped_run_writes_one_terminal_row_and_does_not_block_the_next_fire() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = open(&dir).await;
    let autopilot = seed_autopilot(&store, ConcurrencyPolicy::Skip, 1).await;
    let clock = FixedClock(T0);

    let run_id = record_skipped_run(
        store.pool(),
        &clock,
        &autopilot,
        RunSource::Api,
        "concurrency limit: 1/1 in flight",
    )
    .await
    .expect("record skipped run");

    let row = sqlx::query(
        "SELECT started_at, completed_at, status, source, failure_reason \
         FROM autopilot_run WHERE id = ?",
    )
    .bind(run_id.as_str())
    .fetch_one(store.pool())
    .await
    .expect("skipped run row present");
    assert_eq!(row.get::<String, _>("status"), "skipped");
    assert_eq!(row.get::<String, _>("source"), "api");
    assert_eq!(
        row.get::<Option<String>, _>("failure_reason").as_deref(),
        Some("concurrency limit: 1/1 in flight")
    );
    assert_eq!(row.get::<i64, _>("started_at"), T0);
    assert_eq!(
        row.get::<Option<i64>, _>("completed_at"),
        Some(T0),
        "a skipped run MUST stamp completed_at or it wedges the in-flight count"
    );

    // Exactly one row, and it enqueued no work.
    assert_eq!(count(&store, "SELECT count(*) FROM autopilot_run").await, 1);
    assert_eq!(
        count(&store, "SELECT count(*) FROM agent_task_queue").await,
        0
    );

    // The in-flight count is unchanged, so the gate still admits the next fire.
    assert_eq!(
        count_in_flight(store.pool(), &autopilot.id).await.expect("count in flight"),
        0
    );
    let outcome = dispatch_with_admission(store.pool(), &clock, &autopilot, RunSource::Api)
        .await
        .expect("dispatch after a skip");
    assert!(matches!(outcome, DispatchOutcome::Fired { .. }));
}

/// At the limit under `skip`, an api dispatch is declined AND recorded — the one
/// path that proves both halves of parity item 15 at once.
#[tokio::test]
async fn dispatch_at_limit_with_skip_records_a_skipped_api_run_and_no_task() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = open(&dir).await;
    let autopilot = seed_autopilot(&store, ConcurrencyPolicy::Skip, 1).await;
    let clock = FixedClock(T0);

    let first = dispatch_with_admission(store.pool(), &clock, &autopilot, RunSource::Api)
        .await
        .expect("first dispatch");
    assert!(matches!(first, DispatchOutcome::Fired { .. }));

    let second = dispatch_with_admission(store.pool(), &clock, &autopilot, RunSource::Api)
        .await
        .expect("second dispatch");
    let DispatchOutcome::Skipped {
        reason, in_flight, ..
    } = second
    else {
        panic!("at the limit under `skip` the dispatch must be declined, got {second:?}");
    };
    assert_eq!(in_flight, 1);
    assert!(
        reason.starts_with("concurrency limit"),
        "reason names the admission gate: {reason}"
    );

    // One task (the skip enqueued nothing) …
    assert_eq!(
        count(&store, "SELECT count(*) FROM agent_task_queue").await,
        1,
        "a skip must enqueue no work"
    );
    // … one live run …
    assert_eq!(
        count(
            &store,
            "SELECT count(*) FROM autopilot_run WHERE status <> 'skipped'"
        )
        .await,
        1
    );
    // … and exactly one recorded skip, stamped `api`, terminal, with a reason.
    assert_eq!(
        count(
            &store,
            "SELECT count(*) FROM autopilot_run \
             WHERE status = 'skipped' AND source = 'api' \
               AND completed_at IS NOT NULL \
               AND failure_reason LIKE 'concurrency limit%'"
        )
        .await,
        1,
        "the declined dispatch must be persisted, not just logged"
    );
}

/// `queue` still fires at the limit (regression guard on the lifted logic).
#[tokio::test]
async fn dispatch_at_limit_with_queue_fires_anyway() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = open(&dir).await;
    let autopilot = seed_autopilot(&store, ConcurrencyPolicy::Queue, 1).await;
    let clock = FixedClock(T0);

    for _ in 0..2 {
        let outcome = dispatch_with_admission(store.pool(), &clock, &autopilot, RunSource::Api)
            .await
            .expect("dispatch");
        assert!(
            matches!(outcome, DispatchOutcome::Fired { superseded: 0, .. }),
            "queue fires without superseding, got {outcome:?}"
        );
    }
    assert_eq!(
        count(&store, "SELECT count(*) FROM agent_task_queue").await,
        2
    );
    assert_eq!(
        count(
            &store,
            "SELECT count(*) FROM autopilot_run WHERE status = 'skipped'"
        )
        .await,
        0,
        "queue never records a skip"
    );
}

/// `replace` supersedes the in-flight run then fires (regression guard).
#[tokio::test]
async fn dispatch_at_limit_with_replace_supersedes_then_fires() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = open(&dir).await;
    let autopilot = seed_autopilot(&store, ConcurrencyPolicy::Replace, 1).await;
    let clock = FixedClock(T0);

    let first = dispatch_with_admission(store.pool(), &clock, &autopilot, RunSource::Api)
        .await
        .expect("first dispatch");
    let DispatchOutcome::Fired {
        run_id: first_run, ..
    } = first
    else {
        panic!("under the limit every policy fires, got {first:?}");
    };

    let second = dispatch_with_admission(store.pool(), &clock, &autopilot, RunSource::Api)
        .await
        .expect("second dispatch");
    assert!(
        matches!(second, DispatchOutcome::Fired { superseded: 1, .. }),
        "replace supersedes exactly the one in-flight run, got {second:?}"
    );

    let status: String = sqlx::query_scalar("SELECT status FROM autopilot_run WHERE id = ?")
        .bind(first_run.as_str())
        .fetch_one(store.pool())
        .await
        .expect("read the superseded run");
    assert_eq!(status, "cancelled");
    assert_eq!(
        count(
            &store,
            "SELECT count(*) FROM autopilot_run WHERE status = 'skipped'"
        )
        .await,
        0,
        "replace never records a skip"
    );
}

/// Provenance: the sourced fire stamps `api`; the legacy delegate still stamps
/// `schedule`, so no existing caller silently changes meaning.
#[tokio::test]
async fn fire_stamps_the_source_and_the_legacy_entry_point_stays_schedule() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = open(&dir).await;
    let autopilot = seed_autopilot(&store, ConcurrencyPolicy::Queue, 5).await;
    let clock = FixedClock(T0);

    let (api_run, _) =
        fire_autopilot_tick_with_source(store.pool(), &clock, &autopilot, RunSource::Api)
            .await
            .expect("sourced fire");
    let (legacy_run, _) = fire_autopilot_tick(store.pool(), &clock, &autopilot)
        .await
        .expect("legacy fire");

    async fn source_of(store: &Store, id: &str) -> String {
        sqlx::query_scalar::<_, String>("SELECT source FROM autopilot_run WHERE id = ?")
            .bind(id)
            .fetch_one(store.pool())
            .await
            .expect("read source")
    }
    assert_eq!(source_of(&store, api_run.as_str()).await, "api");
    assert_eq!(source_of(&store, legacy_run.as_str()).await, "schedule");
}

/// `set_api_trigger_enabled` is workspace-scoped: a foreign workspace touches no
/// row, exactly like every other by-id mutation on this repo.
#[tokio::test]
async fn set_api_trigger_enabled_is_workspace_scoped() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = open(&dir).await;
    let autopilot = seed_autopilot(&store, ConcurrencyPolicy::Skip, 1).await;
    let id = AutopilotId::from_str(autopilot.id.clone()).unwrap();
    assert!(
        !autopilot.api_trigger_enabled,
        "the api trigger starts disarmed"
    );

    // A foreign workspace: no row touched, no arm.
    let updated = AutopilotRepo::set_api_trigger_enabled(
        store.pool(),
        &WorkspaceId::from_str("ws-2").unwrap(),
        &id,
        true,
    )
    .await
    .expect("foreign-workspace update");
    assert!(!updated, "a foreign workspace must touch no row");
    assert!(!reload(&store, &id).await.api_trigger_enabled);

    // The owning workspace arms it, and can disarm it again.
    let updated = AutopilotRepo::set_api_trigger_enabled(
        store.pool(),
        &WorkspaceId::from_str("ws-1").unwrap(),
        &id,
        true,
    )
    .await
    .expect("owner update");
    assert!(updated);
    assert!(reload(&store, &id).await.api_trigger_enabled);

    AutopilotRepo::set_api_trigger_enabled(
        store.pool(),
        &WorkspaceId::from_str("ws-1").unwrap(),
        &id,
        false,
    )
    .await
    .expect("disarm");
    assert!(!reload(&store, &id).await.api_trigger_enabled);
}

/// The run-history read surfaces the new columns (the CLI + plugin read path).
#[tokio::test]
async fn list_runs_surfaces_source_and_failure_reason() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = open(&dir).await;
    let autopilot = seed_autopilot(&store, ConcurrencyPolicy::Skip, 1).await;
    let id = AutopilotId::from_str(autopilot.id.clone()).unwrap();
    let clock = FixedClock(T0);

    dispatch_with_admission(store.pool(), &clock, &autopilot, RunSource::Api)
        .await
        .expect("fire");
    dispatch_with_admission(store.pool(), &clock, &autopilot, RunSource::Api)
        .await
        .expect("skip");

    let runs = AutopilotRepo::list_runs(
        store.pool(),
        &WorkspaceId::from_str("ws-1").unwrap(),
        &id,
        10,
    )
    .await
    .expect("list runs");
    assert_eq!(runs.len(), 2);
    assert!(
        runs.iter().all(|r| r.source == "api"),
        "both runs carry their api provenance: {runs:?}"
    );
    let skipped = runs
        .iter()
        .find(|r| r.status == "skipped")
        .expect("the skipped run is readable through list_runs");
    assert!(
        skipped
            .failure_reason
            .as_deref()
            .is_some_and(|r| r.starts_with("concurrency limit")),
        "the admission reason is readable: {skipped:?}"
    );
}

/// `RunSource::from_db_str` is tolerant: unknown values fall back to the column
/// default rather than erroring a whole read.
#[test]
fn run_source_from_db_str_falls_back_to_schedule() {
    assert_eq!(RunSource::from_db_str("api"), RunSource::Api);
    assert_eq!(RunSource::from_db_str("manual"), RunSource::Manual);
    assert_eq!(RunSource::from_db_str("webhook"), RunSource::Webhook);
    assert_eq!(RunSource::from_db_str("schedule"), RunSource::Schedule);
    assert_eq!(
        RunSource::from_db_str("from-the-future"),
        RunSource::Schedule
    );
}
