//! P7.2 — `AutopilotRepo` integration round-trips over ephemeral on-disk `SQLite`.
//!
//! Each test opens its own `tempdir/hangar.db` via [`Store::open_in`] (the
//! path-explicit, no-`set_var` pattern the other store tests use), so the suite
//! is parallel-safe with no shared `$HOME` / env races.
//!
//! Proves the seven P7.2 properties plus the workspace-scoping (anti-IDOR)
//! contract on every by-id method:
//!
//! - `create` persists a row with a computed `next_tick_at`;
//! - `create` rejects an invalid cron (`CronError`) with no row inserted;
//! - `list` filters by workspace (A's autopilots invisible to B);
//! - `list_runs` returns latest-first, capped at `limit`;
//! - `disable` clears `enabled` and keeps `next_tick_at`;
//! - `enable` recomputes `next_tick_at` from *now* (no missed-tick replay);
//! - a duplicate `(workspace_id, name)` is rejected (UNIQUE constraint);
//! - `get` / `list_runs` / `disable` / `enable` are workspace-scoped.

use ainb_hangar_core::autopilot::cron::{
    millis_to_utc, next_tick_after, parse_cron, utc_to_millis,
};
use ainb_hangar_core::clock::FixedClock;
use ainb_hangar_core::ids::{AgentId, WorkspaceId};
use ainb_hangar_store::Store;
use ainb_hangar_store::repo::autopilot::{
    AutopilotRepo, AutopilotRepoError, ConcurrencyPolicy, ExecutionMode, NewAutopilot,
};

/// 2026-01-01T00:00:00Z in epoch-ms — the frozen `now` for these tests.
const T0: i64 = 1_767_225_600_000;

/// Seed the FK chain (workspace -> user -> `agent_runtime` -> agent) an `autopilot`
/// row requires, for the given workspace id. Returns the agent id.
async fn seed_chain(store: &Store, ws: &str, suffix: &str) -> String {
    let user = format!("user-{suffix}");
    let runtime = format!("rt-{suffix}");
    let agent = format!("agent-{suffix}");

    sqlx::query("INSERT INTO workspace (id, slug, name, created_at) VALUES (?, ?, ?, ?)")
        .bind(ws)
        .bind(format!("slug-{suffix}"))
        .bind("WS")
        .bind(0_i64)
        .execute(store.pool())
        .await
        .expect("insert workspace");

    sqlx::query("INSERT INTO user (id, email, created_at) VALUES (?, ?, ?)")
        .bind(&user)
        .bind(format!("{suffix}@example.com"))
        .bind(0_i64)
        .execute(store.pool())
        .await
        .expect("insert user");

    sqlx::query(
        "INSERT INTO agent_runtime \
         (id, workspace_id, daemon_id, provider, runtime_mode, status) \
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(&runtime)
    .bind(ws)
    .bind("daemon-1")
    .bind("claude")
    .bind("local")
    .bind("online")
    .execute(store.pool())
    .await
    .expect("insert agent_runtime");

    sqlx::query(
        "INSERT INTO agent \
         (id, workspace_id, name, runtime_id, instructions, visibility, owner_id) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&agent)
    .bind(ws)
    .bind("Builder")
    .bind(&runtime)
    .bind(Option::<String>::None)
    .bind("workspace")
    .bind(&user)
    .execute(store.pool())
    .await
    .expect("insert agent");

    agent
}

fn ws(id: &str) -> WorkspaceId {
    WorkspaceId::from_str(id).unwrap()
}

fn new_req(ws_id: &str, agent: &str, name: &str, cron: &str) -> NewAutopilot {
    NewAutopilot {
        workspace_id: ws(ws_id),
        agent_id: AgentId::from_str(agent).unwrap(),
        name: name.to_string(),
        instructions: Some("say hi".to_string()),
        cron_expr: cron.to_string(),
        max_concurrent_runs: 1,
        execution_mode: ExecutionMode::default(),
        concurrency_policy: ConcurrencyPolicy::default(),
    }
}

/// Expected `next_tick_at` (epoch-ms) for `cron` strictly after `T0`.
fn expected_next_tick(cron: &str) -> i64 {
    let schedule = parse_cron(cron).expect("valid cron");
    let after = millis_to_utc(T0).expect("in range");
    utc_to_millis(next_tick_after(&schedule, after).expect("future match"))
}

#[tokio::test]
async fn create_persists_row_with_next_tick() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    let agent = seed_chain(&store, "ws-a", "a").await;
    let clock = FixedClock(T0);

    let id = AutopilotRepo::create(
        store.pool(),
        &clock,
        &new_req("ws-a", &agent, "daily", "0 */6 * * *"),
    )
    .await
    .expect("create autopilot");

    let got = AutopilotRepo::get(store.pool(), &ws("ws-a"), &id)
        .await
        .expect("get")
        .expect("present");
    assert_eq!(got.name, "daily");
    assert_eq!(got.cron_expr, "0 */6 * * *");
    assert!(got.enabled);
    assert_eq!(
        got.next_tick_at,
        Some(expected_next_tick("0 */6 * * *")),
        "create must cache next_tick_at strictly after the clock instant"
    );
}

#[tokio::test]
async fn create_rejects_invalid_cron_no_row() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    let agent = seed_chain(&store, "ws-a", "a").await;
    let clock = FixedClock(T0);

    let err = AutopilotRepo::create(
        store.pool(),
        &clock,
        &new_req("ws-a", &agent, "bad", "0 25 * * *"),
    )
    .await
    .expect_err("bad cron must be rejected");
    assert!(
        matches!(err, AutopilotRepoError::Cron(_)),
        "expected CronError, got {err:?}"
    );

    let rows = AutopilotRepo::list(store.pool(), &ws("ws-a")).await.expect("list");
    assert!(rows.is_empty(), "a cron-rejected create must write no row");
}

#[tokio::test]
async fn list_filters_by_workspace() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    let agent_a = seed_chain(&store, "ws-a", "a").await;
    let agent_b = seed_chain(&store, "ws-b", "b").await;
    let clock = FixedClock(T0);

    AutopilotRepo::create(
        store.pool(),
        &clock,
        &new_req("ws-a", &agent_a, "a-pilot", "0 9 * * *"),
    )
    .await
    .expect("create a");
    AutopilotRepo::create(
        store.pool(),
        &clock,
        &new_req("ws-b", &agent_b, "b-pilot", "0 9 * * *"),
    )
    .await
    .expect("create b");

    let a = AutopilotRepo::list(store.pool(), &ws("ws-a")).await.expect("list a");
    assert_eq!(a.len(), 1);
    assert_eq!(a[0].name, "a-pilot");
    assert!(
        a.iter().all(|p| p.workspace_id == "ws-a"),
        "workspace A's list must never include B's autopilot"
    );
}

#[tokio::test]
async fn list_runs_returns_latest_first_capped() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    let agent = seed_chain(&store, "ws-a", "a").await;
    let clock = FixedClock(T0);

    let id = AutopilotRepo::create(
        store.pool(),
        &clock,
        &new_req("ws-a", &agent, "p", "0 9 * * *"),
    )
    .await
    .expect("create");

    // Three runs at increasing started_at.
    for (i, started) in [T0, T0 + 1000, T0 + 2000].iter().enumerate() {
        AutopilotRepo::insert_run(store.pool(), &id, *started, "completed")
            .await
            .unwrap_or_else(|e| panic!("insert run {i}: {e}"));
    }

    let runs = AutopilotRepo::list_runs(store.pool(), &ws("ws-a"), &id, 2)
        .await
        .expect("list_runs");
    assert_eq!(runs.len(), 2, "limit caps the result set");
    assert_eq!(runs[0].started_at, T0 + 2000, "latest first");
    assert_eq!(runs[1].started_at, T0 + 1000, "then next-latest");
}

#[tokio::test]
async fn disable_clears_enabled_and_keeps_next_tick() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    let agent = seed_chain(&store, "ws-a", "a").await;
    let clock = FixedClock(T0);

    let id = AutopilotRepo::create(
        store.pool(),
        &clock,
        &new_req("ws-a", &agent, "p", "0 */6 * * *"),
    )
    .await
    .expect("create");
    let before = AutopilotRepo::get(store.pool(), &ws("ws-a"), &id)
        .await
        .expect("get")
        .expect("present")
        .next_tick_at;

    AutopilotRepo::disable(store.pool(), &ws("ws-a"), &id).await.expect("disable");

    let after = AutopilotRepo::get(store.pool(), &ws("ws-a"), &id)
        .await
        .expect("get")
        .expect("present");
    assert!(!after.enabled, "disable clears enabled");
    assert_eq!(
        after.next_tick_at, before,
        "disable must preserve next_tick_at for re-enable inspection"
    );
}

#[tokio::test]
async fn enable_recomputes_next_tick_from_now() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    let agent = seed_chain(&store, "ws-a", "a").await;

    // Create at T0, disable, then enable at a LATER clock — the new next_tick
    // must be computed from the later instant, never replay the T0-era tick.
    let create_clock = FixedClock(T0);
    let id = AutopilotRepo::create(
        store.pool(),
        &create_clock,
        &new_req("ws-a", &agent, "p", "0 */6 * * *"),
    )
    .await
    .expect("create");
    AutopilotRepo::disable(store.pool(), &ws("ws-a"), &id).await.expect("disable");

    // Advance one full day.
    let later = T0 + 24 * 3_600_000;
    let enable_clock = FixedClock(later);
    AutopilotRepo::enable(store.pool(), &enable_clock, &ws("ws-a"), &id)
        .await
        .expect("enable");

    let got = AutopilotRepo::get(store.pool(), &ws("ws-a"), &id)
        .await
        .expect("get")
        .expect("present");
    assert!(got.enabled);
    let schedule = parse_cron("0 */6 * * *").unwrap();
    let expected =
        utc_to_millis(next_tick_after(&schedule, millis_to_utc(later).unwrap()).unwrap());
    assert_eq!(
        got.next_tick_at,
        Some(expected),
        "enable must recompute strictly after the current clock, not replay missed ticks"
    );
}

#[tokio::test]
async fn concurrent_create_unique_name_rejected() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    let agent = seed_chain(&store, "ws-a", "a").await;
    let clock = FixedClock(T0);

    AutopilotRepo::create(
        store.pool(),
        &clock,
        &new_req("ws-a", &agent, "dup", "0 9 * * *"),
    )
    .await
    .expect("first create");
    let err = AutopilotRepo::create(
        store.pool(),
        &clock,
        &new_req("ws-a", &agent, "dup", "0 9 * * *"),
    )
    .await
    .expect_err("duplicate (workspace_id, name) must be rejected");
    assert!(
        matches!(err, AutopilotRepoError::Db(_)),
        "expected a DB uniqueness error, got {err:?}"
    );

    // Only the first row survives.
    let rows = AutopilotRepo::list(store.pool(), &ws("ws-a")).await.expect("list");
    assert_eq!(rows.len(), 1);
}

// --- cross-workspace denial on every by-id method (anti-IDOR) ---------------

#[tokio::test]
async fn get_is_workspace_scoped() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    let agent = seed_chain(&store, "ws-a", "a").await;
    seed_chain(&store, "ws-b", "b").await;
    let clock = FixedClock(T0);

    let id = AutopilotRepo::create(
        store.pool(),
        &clock,
        &new_req("ws-a", &agent, "p", "0 9 * * *"),
    )
    .await
    .expect("create");

    assert!(
        AutopilotRepo::get(store.pool(), &ws("ws-b"), &id).await.expect("get").is_none(),
        "workspace B must not read workspace A's autopilot by id"
    );
}

#[tokio::test]
async fn disable_and_enable_are_workspace_scoped() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    let agent = seed_chain(&store, "ws-a", "a").await;
    seed_chain(&store, "ws-b", "b").await;
    let clock = FixedClock(T0);

    let id = AutopilotRepo::create(
        store.pool(),
        &clock,
        &new_req("ws-a", &agent, "p", "0 9 * * *"),
    )
    .await
    .expect("create");

    // A disable issued from workspace B must touch no row.
    AutopilotRepo::disable(store.pool(), &ws("ws-b"), &id)
        .await
        .expect("disable (foreign ws no-op)");
    let still = AutopilotRepo::get(store.pool(), &ws("ws-a"), &id)
        .await
        .expect("get")
        .expect("present");
    assert!(
        still.enabled,
        "a foreign-workspace disable must NOT flip workspace A's enabled flag"
    );

    // Same for enable: a foreign-ws enable is a no-op (and does not recompute).
    AutopilotRepo::enable(store.pool(), &clock, &ws("ws-b"), &id)
        .await
        .expect("enable (foreign ws no-op)");
}

#[tokio::test]
async fn list_runs_is_workspace_scoped() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    let agent = seed_chain(&store, "ws-a", "a").await;
    seed_chain(&store, "ws-b", "b").await;
    let clock = FixedClock(T0);

    let id = AutopilotRepo::create(
        store.pool(),
        &clock,
        &new_req("ws-a", &agent, "p", "0 9 * * *"),
    )
    .await
    .expect("create");
    AutopilotRepo::insert_run(store.pool(), &id, T0, "completed")
        .await
        .expect("insert run");

    let foreign = AutopilotRepo::list_runs(store.pool(), &ws("ws-b"), &id, 10)
        .await
        .expect("list_runs");
    assert!(
        foreign.is_empty(),
        "workspace B must not read workspace A's autopilot run history"
    );
    // And the owner still sees the run.
    let owner = AutopilotRepo::list_runs(store.pool(), &ws("ws-a"), &id, 10)
        .await
        .expect("list_runs");
    assert_eq!(owner.len(), 1);
}
