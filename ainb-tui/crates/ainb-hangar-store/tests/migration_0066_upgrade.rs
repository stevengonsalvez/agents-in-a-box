//! Upgrade-from-populated test for migration 0066 (`issue_properties_metadata`
//! — the custom-property catalog + the per-issue metadata scratch bag, multica
//! parity #17).
//!
//! Fresh-database coverage lives in `tripwire_migrations_apply.rs`. This file
//! proves the migration on a REAL populated database:
//!
//! 1. apply only the PRIOR migrations (0001..0065),
//! 2. seed a workspace + an issue that already carries labels, acceptance
//!    criteria and a comment (the pre-0066 world),
//! 3. apply the embedded migrations (which adds 0066),
//! 4. assert the catalog table + BOTH indexes exist, the pre-existing issue's
//!    two new bags defaulted to `{}`, its labels / acceptance criteria survived
//!    byte-identical, and the `(workspace_id, key)` UNIQUE index actually bites.

use ainb_hangar_store::apply_migrations;
use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};

/// `sqlx` version number of the migration under test
/// (`0066_issue_properties_metadata.sql`).
const NEW_MIGRATION_VERSION: i64 = 66;

/// The exact JSON the seeded issue's `acceptance_criteria` column holds before
/// the upgrade — re-read verbatim afterwards, so any accidental rewrite shows.
const SEEDED_CRITERIA: &str = r#"[{"id":"ac-1","text":"builds green","checked":true}]"#;
/// Same, for `labels`.
const SEEDED_LABELS: &str = r#"["bug","p0"]"#;

async fn pool_at_prior_schema(dir: &std::path::Path) -> SqlitePool {
    let opts = SqliteConnectOptions::new()
        .filename(dir.join("hangar.db"))
        .create_if_missing(true)
        .foreign_keys(true)
        .journal_mode(SqliteJournalMode::Wal);
    let pool = SqlitePoolOptions::new().connect_with(opts).await.expect("open pool");

    let mut migrator = sqlx::migrate::Migrator::new(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations"),
    )
    .await
    .expect("load migrations directory");
    migrator.migrations.to_mut().retain(|m| m.version < NEW_MIGRATION_VERSION);
    assert!(!migrator.migrations.is_empty());
    migrator.run(&pool).await.expect("prior migrations apply");
    pool
}

/// The pre-0066 world: an issue that already carries every other JSON-column
/// payload the schema knows about, so the ALTER TABLE has something to disturb.
async fn seed_populated(pool: &SqlitePool) {
    sqlx::query("INSERT INTO workspace (id, slug, name, created_at) VALUES ('ws-1','a','A',0)")
        .execute(pool)
        .await
        .expect("seed workspace");
    sqlx::query(
        "INSERT INTO issue \
         (id, workspace_id, title, state, creator_type, creator_id, created_at, \
          labels, acceptance_criteria) \
         VALUES ('i-1','ws-1','Ship #17','open','member','u-amy',10, ?, ?)",
    )
    .bind(SEEDED_LABELS)
    .bind(SEEDED_CRITERIA)
    .execute(pool)
    .await
    .expect("seed issue");
    sqlx::query(
        "INSERT INTO comment (id, issue_id, author_type, author_id, body, created_at) \
         VALUES ('cm-1','i-1','member','u-amy','looks good',11)",
    )
    .execute(pool)
    .await
    .expect("seed comment");
}

async fn object_exists(pool: &SqlitePool, kind: &str, name: &str) -> bool {
    sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM sqlite_master WHERE type = ? AND name = ?")
        .bind(kind)
        .bind(name)
        .fetch_one(pool)
        .await
        .expect("sqlite_master")
        > 0
}

#[tokio::test]
async fn migration_0066_adds_the_catalog_and_two_empty_bags() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = pool_at_prior_schema(dir.path()).await;
    seed_populated(&pool).await;

    assert!(
        !object_exists(&pool, "table", "issue_property").await,
        "issue_property must not exist before 0066"
    );

    apply_migrations(&pool).await.expect("upgrade applies 0066");
    let recorded: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM _sqlx_migrations WHERE version = ?")
            .bind(NEW_MIGRATION_VERSION)
            .fetch_one(&pool)
            .await
            .expect("read migration version");
    assert_eq!(recorded, 1, "0066 recorded as applied");

    // (a) The catalog table and BOTH indexes landed.
    assert!(object_exists(&pool, "table", "issue_property").await);
    assert!(object_exists(&pool, "index", "idx_issue_property_workspace_key").await);
    assert!(object_exists(&pool, "index", "idx_issue_property_workspace_active").await);

    // (b) NO BACKFILL: the pre-existing issue's two new bags are the schema
    //     default, not NULL and not invented content.
    let (properties, metadata): (String, String) =
        sqlx::query_as("SELECT properties, metadata FROM issue WHERE id = 'i-1'")
            .fetch_one(&pool)
            .await
            .expect("read new columns");
    assert_eq!(properties, "{}");
    assert_eq!(metadata, "{}");

    // (c) The pre-existing JSON payloads survived BYTE-IDENTICAL — an ALTER
    //     TABLE that rewrote a sibling column would show up here.
    let (labels, criteria): (String, String) =
        sqlx::query_as("SELECT labels, acceptance_criteria FROM issue WHERE id = 'i-1'")
            .fetch_one(&pool)
            .await
            .expect("read preserved columns");
    assert_eq!(labels, SEEDED_LABELS);
    assert_eq!(criteria, SEEDED_CRITERIA);
    let body: String = sqlx::query_scalar("SELECT body FROM comment WHERE id = 'cm-1'")
        .fetch_one(&pool)
        .await
        .expect("comment survives");
    assert_eq!(body, "looks good");

    // (d) The (workspace_id, key) UNIQUE index BITES — it is what makes
    //     `define` an idempotent resolve-or-update rather than a duplicate mint.
    let insert = "INSERT INTO issue_property \
         (id, workspace_id, key, name, kind, options, position, created_at) \
         VALUES (?,'ws-1','sprint','Sprint','select','[\"S1\"]',0,20)";
    sqlx::query(insert)
        .bind("prop-1")
        .execute(&pool)
        .await
        .expect("first definition wins");
    assert!(
        sqlx::query(insert).bind("prop-2").execute(&pool).await.is_err(),
        "a second (workspace_id, key) pair must violate the UNIQUE index"
    );
    // The SAME key in a DIFFERENT workspace is a distinct definition.
    sqlx::query("INSERT INTO workspace (id, slug, name, created_at) VALUES ('ws-2','b','B',0)")
        .execute(&pool)
        .await
        .expect("second workspace");
    sqlx::query(
        "INSERT INTO issue_property \
         (id, workspace_id, key, name, kind, options, position, created_at) \
         VALUES ('prop-3','ws-2','sprint','Sprint','select','[\"S1\"]',0,21)",
    )
    .execute(&pool)
    .await
    .expect("per-workspace uniqueness, not global");

    // (e) Re-applying is a ledgered no-op.
    apply_migrations(&pool).await.expect("second apply is a no-op");
}
