//! Upgrade-from-populated test for migration 0053 (`squad_member.role` +
//! `squad.instructions`, multica gap #25).
//!
//! Fresh-database column coverage lives in `tripwire_migrations_apply.rs`. This
//! file proves the migration is safe on a REAL populated database — the state
//! every upgrading install carries:
//!
//! 1. apply only the PRIOR migrations (0001..0052),
//! 2. seed a squad plus two `squad_member` rows through raw inserts that predate
//!    both new columns entirely,
//! 3. apply the embedded migrations (which adds 0053),
//! 4. assert the legacy squad reads `instructions == ""`, both legacy
//!    memberships read `role == ""`, the squad is still listed, and a subsequent
//!    `set_member_role` / `set_instructions` persists and round-trips through
//!    BOTH `list` and `get`.

use ainb_hangar_core::actor::{ActorKind, ActorRef};
use ainb_hangar_core::ids::WorkspaceId;
use ainb_hangar_store::apply_migrations;
use ainb_hangar_store::repo::squad::SquadRepo;
use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};

/// `sqlx` version number of the migration under test
/// (`0053_squad_role_instructions.sql`).
const NEW_MIGRATION_VERSION: i64 = 53;

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

/// Seed workspace → user → runtime → agents → a squad with two memberships,
/// through raw inserts that name only the pre-0053 columns.
async fn seed_legacy_squad(pool: &SqlitePool) {
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
    for (id, name) in [("ag-lead", "Captain"), ("ag-1", "Scout")] {
        sqlx::query(
            "INSERT INTO agent (id, workspace_id, name, runtime_id, visibility, owner_id) \
             VALUES (?,'ws-1',?, 'rt-1','workspace','user-1')",
        )
        .bind(id)
        .bind(name)
        .execute(pool)
        .await
        .expect("insert agent");
    }
    sqlx::query(
        "INSERT INTO squad (id, workspace_id, name, leader_type, leader_id, created_at) \
         VALUES ('sq-1','ws-1','alpha','agent','ag-lead',0)",
    )
    .execute(pool)
    .await
    .expect("insert squad");
    for (kind, id) in [("agent", "ag-1"), ("member", "user-1")] {
        sqlx::query("INSERT INTO squad_member (squad_id, member_type, member_id) VALUES ('sq-1',?,?)")
            .bind(kind)
            .bind(id)
            .execute(pool)
            .await
            .expect("insert membership");
    }
}

fn ws() -> WorkspaceId {
    WorkspaceId::from_str("ws-1".to_string()).unwrap()
}

/// The load-bearing upgrade proof: memberships and squads written BEFORE the two
/// columns existed read as the empty defaults afterwards (so the leader briefing
/// renders byte-identically), and the new levers work on those legacy rows.
#[tokio::test]
async fn upgrades_populated_db_to_empty_role_and_instructions() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = pool_at_prior_schema(dir.path()).await;
    seed_legacy_squad(&pool).await;

    // The columns genuinely do not exist yet at the prior schema.
    assert!(
        sqlx::query_scalar::<_, String>("SELECT role FROM squad_member")
            .fetch_one(&pool)
            .await
            .is_err(),
        "squad_member.role must not exist before 0053 — otherwise this test proves nothing"
    );
    assert!(
        sqlx::query_scalar::<_, String>("SELECT instructions FROM squad")
            .fetch_one(&pool)
            .await
            .is_err(),
        "squad.instructions must not exist before 0053"
    );

    apply_migrations(&pool).await.expect("upgrade applies 0053");

    // 1. The legacy squad is still listed, with empty instructions and roleless
    //    memberships.
    let squads = SquadRepo::list(&pool, &ws()).await.expect("list squads");
    assert_eq!(
        squads.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
        vec!["sq-1"],
        "a pre-0053 squad stays in the list"
    );
    assert_eq!(squads[0].instructions, "", "legacy squad has no instructions");
    assert_eq!(squads[0].members.len(), 2);
    for m in &squads[0].members {
        assert_eq!(m.role, "", "legacy membership {:?} has no role", m.actor);
    }

    // 2. The new levers work on those LEGACY rows and round-trip through both
    //    read paths.
    let scout = ActorRef::new(ActorKind::Agent, "ag-1").unwrap();
    assert!(
        SquadRepo::set_member_role(&pool, &ws(), "sq-1", &scout, "owns the migrations")
            .await
            .expect("set role"),
        "an existing membership is updated"
    );
    SquadRepo::set_instructions(&pool, &ws(), "sq-1", "Route schema work to the DB owner.")
        .await
        .expect("set instructions");

    for squad in [
        SquadRepo::list(&pool, &ws()).await.expect("list").remove(0),
        SquadRepo::get(&pool, &ws(), "sq-1").await.expect("get").expect("present"),
    ] {
        assert_eq!(squad.instructions, "Route schema work to the DB owner.");
        let roled = squad.members.iter().find(|m| m.actor == scout).expect("scout present");
        assert_eq!(roled.role, "owns the migrations");
        let human = squad.members.iter().find(|m| m.actor != scout).expect("human present");
        assert_eq!(human.role, "", "the untouched membership keeps no role");
    }

    // Re-applying is a no-op (the migrator records 0053 as applied).
    apply_migrations(&pool).await.expect("double-apply is a no-op");
}
