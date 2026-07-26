//! Upgrade-from-populated test for migration 0051 (`agent_skill.enabled` +
//! `agent.disabled_runtime_skills`, multica gap #24).
//!
//! Fresh-database column coverage lives in `tripwire_migrations_apply.rs`. This
//! file proves the migration is safe on a REAL populated database — the state
//! every upgrading install carries:
//!
//! 1. apply only the PRIOR migrations (0001..0050),
//! 2. seed an agent with an already-attached skill (a raw `agent_skill` row that
//!    predates the `enabled` column entirely),
//! 3. apply the embedded migrations (which adds 0051),
//! 4. assert the pre-existing link reads `enabled = 1` and STILL materialises
//!    through `skills_for_agent` — the "no silent behaviour change on upgrade"
//!    proof — and that the `CHECK` now rejects a non-boolean.

use ainb_hangar_core::ids::{AgentId, WorkspaceId};
use ainb_hangar_store::apply_migrations;
use ainb_hangar_store::repo::skill::SkillRepo;
use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};

/// `sqlx` version number of the migration under test
/// (`0051_agent_skill_enabled.sql`).
const NEW_MIGRATION_VERSION: i64 = 51;

/// Open a fresh on-disk WAL pool in `dir` and apply only the migrations PRIOR to
/// [`NEW_MIGRATION_VERSION`], reproducing the schema an existing install runs
/// before upgrading.
async fn pool_at_prior_schema(dir: &std::path::Path) -> SqlitePool {
    let db_path = dir.join("hangar.db");
    let opts = SqliteConnectOptions::new()
        .filename(&db_path)
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal);
    let pool = SqlitePoolOptions::new().connect_with(opts).await.expect("open pool");

    let mut migrator = sqlx::migrate::Migrator::new(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations"),
    )
    .await
    .expect("load migrations directory");
    migrator.migrations.to_mut().retain(|m| m.version < NEW_MIGRATION_VERSION);
    assert!(
        !migrator.migrations.is_empty(),
        "prior-migration set must not be empty"
    );
    migrator.run(&pool).await.expect("prior migrations apply");
    pool
}

/// Seed workspace → user → runtime → agent → skill and attach the skill through
/// a RAW insert, i.e. exactly the row shape a pre-0051 install holds.
async fn seed_attached(pool: &SqlitePool) {
    sqlx::query(
        "INSERT INTO workspace (id, slug, name, created_at) VALUES ('ws-1','ws-1','ws-1',0)",
    )
    .execute(pool)
    .await
    .expect("insert workspace");
    sqlx::query("INSERT INTO user (id, email, created_at) VALUES ('user-1','a@x.com',0)")
        .execute(pool)
        .await
        .expect("insert user");
    sqlx::query(
        "INSERT INTO agent_runtime \
         (id, workspace_id, daemon_id, provider, runtime_mode, last_seen_at, status) \
         VALUES ('rt-1','ws-1','d-1','claude','local',0,'online')",
    )
    .execute(pool)
    .await
    .expect("insert runtime");
    sqlx::query(
        "INSERT INTO agent (id, workspace_id, name, runtime_id, visibility, owner_id) \
         VALUES ('ag-1','ws-1','Tester','rt-1','workspace','user-1')",
    )
    .execute(pool)
    .await
    .expect("insert agent");
    sqlx::query(
        "INSERT INTO skill (id, workspace_id, name, description, content) \
         VALUES ('sk-1','ws-1','commit',NULL,'# commit')",
    )
    .execute(pool)
    .await
    .expect("insert skill");
    // The pre-0051 junction shape: no `enabled` column exists yet.
    sqlx::query("INSERT INTO agent_skill (agent_id, skill_id) VALUES ('ag-1','sk-1')")
        .execute(pool)
        .await
        .expect("insert agent_skill");
}

/// A brand-new database defaults every attachment to enabled and every agent to
/// an empty suppression list.
#[tokio::test]
async fn applies_to_fresh_db_and_defaults_enabled_true() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = ainb_hangar_store::Store::open_in(dir.path()).await.expect("open store");
    let pool = store.pool();

    seed_attached(pool).await;

    let enabled: i64 = sqlx::query_scalar("SELECT enabled FROM agent_skill WHERE agent_id='ag-1'")
        .fetch_one(pool)
        .await
        .expect("read enabled");
    assert_eq!(enabled, 1, "a fresh attachment is enabled by default");

    let suppressed: String =
        sqlx::query_scalar("SELECT disabled_runtime_skills FROM agent WHERE id='ag-1'")
            .fetch_one(pool)
            .await
            .expect("read disabled_runtime_skills");
    assert_eq!(
        suppressed, "[]",
        "no runtime skill is suppressed by default"
    );
}

/// The load-bearing upgrade proof: a link written BEFORE the column existed
/// keeps working afterwards.
#[tokio::test]
async fn upgrades_populated_db_without_disabling_anything() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = pool_at_prior_schema(dir.path()).await;
    seed_attached(&pool).await;

    // The column genuinely does not exist yet at the prior schema.
    let pre = sqlx::query_scalar::<_, i64>("SELECT enabled FROM agent_skill")
        .fetch_one(&pool)
        .await;
    assert!(
        pre.is_err(),
        "agent_skill.enabled must not exist before 0051 — otherwise this test proves nothing"
    );

    apply_migrations(&pool).await.expect("upgrade applies 0051");

    let enabled: i64 = sqlx::query_scalar("SELECT enabled FROM agent_skill WHERE agent_id='ag-1'")
        .fetch_one(&pool)
        .await
        .expect("read enabled");
    assert_eq!(
        enabled, 1,
        "an attachment predating the column must backfill to ENABLED, not disabled"
    );

    // …and it still materialises, which is what an operator actually observes.
    let ws = WorkspaceId::from_str("ws-1").unwrap();
    let agent = AgentId::from_str("ag-1").unwrap();
    let skills = SkillRepo::skills_for_agent(&pool, &ws, &agent).await.expect("skills_for_agent");
    assert_eq!(
        skills.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
        vec!["commit"],
        "the pre-existing link still reaches the materialiser after the upgrade"
    );

    // Re-applying is a no-op (the migrator records 0051 as applied).
    apply_migrations(&pool).await.expect("double-apply is a no-op");
}

/// `enabled` is a 0/1 INTEGER, guarded by a `CHECK` — a stray 2 is refused at
/// the storage boundary rather than silently meaning "truthy".
#[tokio::test]
async fn check_constraint_rejects_non_boolean() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = ainb_hangar_store::Store::open_in(dir.path()).await.expect("open store");
    let pool = store.pool();
    seed_attached(pool).await;

    let err = sqlx::query("UPDATE agent_skill SET enabled = 2 WHERE agent_id='ag-1'")
        .execute(pool)
        .await
        .expect_err("CHECK (enabled IN (0,1)) must reject 2");
    assert!(
        err.to_string().to_uppercase().contains("CHECK"),
        "expected a CHECK-constraint failure, got: {err}"
    );
}
