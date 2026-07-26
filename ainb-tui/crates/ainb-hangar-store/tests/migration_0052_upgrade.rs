//! Upgrade-from-populated test for migration 0052 (the archive AUDIT trail on
//! `agent` + `squad`, multica gap #26).
//!
//! Fresh-database column coverage lives in `tripwire_migrations_apply.rs`. This
//! file proves the migration is safe on a REAL populated database — the state
//! every upgrading install carries:
//!
//! 1. apply only the PRIOR migrations (0001..0051),
//! 2. seed an agent that is ALREADY archived and a squad, through raw inserts
//!    that predate the audit columns entirely,
//! 3. apply the embedded migrations (which adds 0052),
//! 4. assert the legacy archived agent STILL reads `archived = true` with NULL
//!    audit columns (no fabricated stamp, no silent reclassification), the
//!    legacy squad reads `archived = false` and is still listed, and a fresh
//!    archive then stamps both columns.

use ainb_hangar_core::actor::{ActorKind, ActorRef};
use ainb_hangar_core::ids::WorkspaceId;
use ainb_hangar_store::apply_migrations;
use ainb_hangar_store::repo::agent::AgentRepo;
use ainb_hangar_store::repo::squad::SquadRepo;
use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};

/// `sqlx` version number of the migration under test (`0052_archive_audit.sql`).
const NEW_MIGRATION_VERSION: i64 = 52;

/// A fixed epoch-ms stamp so the assertions are exact rather than "roughly now".
const STAMP_MS: i64 = 1_700_000_000_000;

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

/// Seed workspace → user → runtime → an ALREADY-ARCHIVED agent → a squad,
/// through raw inserts, i.e. exactly the row shape a pre-0052 install holds.
async fn seed_legacy_archived(pool: &SqlitePool) {
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
        "INSERT INTO agent (id, workspace_id, name, runtime_id, visibility, owner_id, archived) \
         VALUES ('ag-1','ws-1','Retired','rt-1','workspace','user-1',1)",
    )
    .execute(pool)
    .await
    .expect("insert archived agent");
    sqlx::query(
        "INSERT INTO squad (id, workspace_id, name, leader_type, leader_id, created_at) \
         VALUES ('sq-1','ws-1','alpha','agent','ag-1',0)",
    )
    .execute(pool)
    .await
    .expect("insert squad");
}

fn ws() -> WorkspaceId {
    WorkspaceId::from_str("ws-1".to_string()).unwrap()
}

/// The load-bearing upgrade proof: rows written BEFORE the audit columns existed
/// keep their meaning afterwards, and nothing is fabricated for them.
#[tokio::test]
async fn upgrades_populated_db_without_fabricating_an_audit_stamp() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = pool_at_prior_schema(dir.path()).await;
    seed_legacy_archived(&pool).await;

    // The columns genuinely do not exist yet at the prior schema.
    let pre = sqlx::query_scalar::<_, Option<i64>>("SELECT archived_at FROM agent")
        .fetch_one(&pool)
        .await;
    assert!(
        pre.is_err(),
        "agent.archived_at must not exist before 0052 — otherwise this test proves nothing"
    );

    apply_migrations(&pool).await.expect("upgrade applies 0052");

    // 1. The legacy archived agent is STILL archived, with an honest unknown
    //    audit pair — the "no silent reclassification, no fabricated stamp" proof.
    let agent = AgentRepo::get(&pool, "ag-1").await.expect("get agent").expect("agent present");
    assert!(
        agent.archived,
        "a pre-0052 archived agent must still read as archived"
    );
    assert_eq!(
        agent.archived_at, None,
        "a historical archive has no honest timestamp — it must stay NULL"
    );
    assert_eq!(
        agent.archived_by, None,
        "a historical archive has no honest actor — it must stay NULL"
    );

    // 2. The legacy squad defaults to ACTIVE and is still listed.
    let squads = SquadRepo::list(&pool, &ws()).await.expect("list squads");
    assert_eq!(
        squads.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
        vec!["sq-1"],
        "a pre-0052 squad defaults to active and stays in the list"
    );
    assert!(!squads[0].archived, "default archived = false");
    assert_eq!(squads[0].archived_at, None);
    assert_eq!(squads[0].archived_by, None);

    // 3. A FRESH archive through the repo now stamps both columns.
    let by = ActorRef::new(ActorKind::Member, "user-1").unwrap();
    assert!(
        AgentRepo::set_archived(&pool, "ws-1", "ag-1", true, Some(&by), STAMP_MS)
            .await
            .expect("re-archive"),
        "re-archiving an already-archived agent still matches its row"
    );
    let agent = AgentRepo::get(&pool, "ag-1").await.expect("get agent").expect("agent present");
    assert_eq!(agent.archived_at, Some(STAMP_MS));
    assert_eq!(agent.archived_by, Some(by.clone()));

    SquadRepo::set_archived(&pool, &ws(), "sq-1", true, Some(&by), STAMP_MS)
        .await
        .expect("archive squad");
    let all = SquadRepo::list_including_archived(&pool, &ws()).await.expect("list all");
    assert_eq!(all.len(), 1);
    assert!(all[0].archived);
    assert_eq!(all[0].archived_at, Some(STAMP_MS));
    assert_eq!(all[0].archived_by, Some(by));

    // Re-applying is a no-op (the migrator records 0052 as applied).
    apply_migrations(&pool).await.expect("double-apply is a no-op");
}

/// `squad.archived` is a 0/1 INTEGER guarded by a `CHECK` — a stray 2 is refused
/// at the storage boundary rather than silently meaning "truthy".
#[tokio::test]
async fn squad_archived_check_rejects_non_boolean() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = ainb_hangar_store::Store::open_in(dir.path()).await.expect("open store");
    let pool = store.pool();
    seed_legacy_archived(pool).await;

    let err = sqlx::query("UPDATE squad SET archived = 2 WHERE id='sq-1'")
        .execute(pool)
        .await
        .expect_err("CHECK (archived IN (0,1)) must reject 2");
    assert!(
        err.to_string().to_uppercase().contains("CHECK"),
        "expected a CHECK-constraint failure, got: {err}"
    );
}

/// There is deliberately NO constraint tying `archived = 1` to a non-null
/// `archived_at`: legacy rows would violate it. A raw archived-without-stamp row
/// must remain writable.
#[tokio::test]
async fn archived_without_a_stamp_is_still_a_legal_row() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = ainb_hangar_store::Store::open_in(dir.path()).await.expect("open store");
    let pool = store.pool();
    seed_legacy_archived(pool).await;

    sqlx::query("UPDATE squad SET archived = 1 WHERE id='sq-1'")
        .execute(pool)
        .await
        .expect("archived = 1 with NULL audit columns must be accepted");
    let stamp: Option<i64> = sqlx::query_scalar("SELECT archived_at FROM squad WHERE id='sq-1'")
        .fetch_one(pool)
        .await
        .expect("read stamp");
    assert_eq!(stamp, None);
}
