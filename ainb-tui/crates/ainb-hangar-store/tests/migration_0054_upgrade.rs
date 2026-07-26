//! Upgrade-from-populated test for migration 0054 (`issue.acceptance_criteria`
//! normalised from a flat string array to structured criterion objects — multica
//! gap #11-rest).
//!
//! Fresh-database coverage lives in `tripwire_migrations_apply.rs`. This file
//! proves the migration is safe on a REAL populated database — the state every
//! upgrading install carries:
//!
//! 1. apply only the PRIOR migrations (0001..0053),
//! 2. seed issues whose `acceptance_criteria` holds the pre-0054 flat string
//!    array, plus an already-structured row and an empty row,
//! 3. apply the embedded migrations (which adds 0054),
//! 4. assert the legacy rows are now object arrays with distinct minted `ac-`
//!    ids and the SAME texts in the SAME order, the already-structured row is
//!    byte-identical (checked state preserved), and the empty row stays `[]`.

use ainb_hangar_core::acceptance::criteria_from_json;
use ainb_hangar_store::apply_migrations;
use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};

/// `sqlx` version number of the migration under test
/// (`0054_acceptance_criterion_state.sql`).
const NEW_MIGRATION_VERSION: i64 = 54;

/// Open a fresh on-disk WAL pool in `dir` and apply only the migrations PRIOR to
/// [`NEW_MIGRATION_VERSION`].
async fn pool_at_prior_schema(dir: &std::path::Path) -> SqlitePool {
    let db_path = dir.join("hangar.db");
    let opts = SqliteConnectOptions::new()
        .filename(&db_path)
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal);
    let pool = SqlitePoolOptions::new()
        .connect_with(opts)
        .await
        .expect("open pool");

    let mut migrator = sqlx::migrate::Migrator::new(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations"),
    )
    .await
    .expect("load migrations directory");
    migrator
        .migrations
        .to_mut()
        .retain(|m| m.version < NEW_MIGRATION_VERSION);
    assert!(
        !migrator.migrations.is_empty(),
        "prior-migration set must not be empty"
    );
    migrator.run(&pool).await.expect("prior migrations apply");
    pool
}

/// Seed a workspace and four issues covering every shape the column can hold.
async fn seed_legacy_issues(pool: &SqlitePool) {
    sqlx::query(
        "INSERT INTO workspace (id, slug, name, created_at) VALUES ('ws-1','ws-1','ws-1',0)",
    )
    .execute(pool)
    .await
    .expect("insert workspace");
    for (id, acceptance) in [
        ("i-legacy", r#"["builds green","tests pass"]"#),
        ("i-empty", "[]"),
        (
            "i-structured",
            r#"[{"id":"ac-keep","text":"already done","checked":true,"checked_at":42,"checked_by":"agent:builder"}]"#,
        ),
        ("i-single", r#"["only one"]"#),
    ] {
        sqlx::query(
            "INSERT INTO issue \
             (id, workspace_id, title, state, creator_type, creator_id, created_at, \
              labels, acceptance_criteria, context_refs) \
             VALUES (?, 'ws-1', 'T', 'open', 'member', 'user-1', 0, '[]', ?, '[]')",
        )
        .bind(id)
        .bind(acceptance)
        .execute(pool)
        .await
        .expect("insert issue");
    }
}

async fn acceptance_of(pool: &SqlitePool, id: &str) -> String {
    sqlx::query_scalar::<_, String>("SELECT acceptance_criteria FROM issue WHERE id = ?")
        .bind(id)
        .fetch_one(pool)
        .await
        .expect("read acceptance_criteria")
}

/// The load-bearing upgrade proof.
#[tokio::test]
async fn upgrades_populated_db_flat_strings_to_structured_objects() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = pool_at_prior_schema(dir.path()).await;
    seed_legacy_issues(&pool).await;

    // Pre-condition: the legacy row genuinely holds bare strings today.
    let before = acceptance_of(&pool, "i-legacy").await;
    assert_eq!(before, r#"["builds green","tests pass"]"#);

    apply_migrations(&pool).await.expect("0054 applies");

    // The legacy row is now an OBJECT array, same texts, same order, unchecked,
    // with distinct minted `ac-` ids.
    let raw = acceptance_of(&pool, "i-legacy").await;
    assert!(
        raw.starts_with("[{"),
        "legacy row must become an object array, got {raw}"
    );
    let items = criteria_from_json(&raw).expect("decodes");
    assert_eq!(
        items.iter().map(|c| c.text.as_str()).collect::<Vec<_>>(),
        vec!["builds green", "tests pass"],
        "texts + order preserved"
    );
    assert!(items.iter().all(|c| !c.checked), "backfilled as unchecked");
    assert!(
        items.iter().all(|c| c.id.starts_with("ac-")),
        "ids are minted with the ac- prefix: {items:?}"
    );
    assert_ne!(items[0].id, items[1].id, "ids are distinct per criterion");
    assert!(
        items.iter().all(|c| !c.id.starts_with("ac-legacy-")),
        "on-disk ids are minted, not read-time placeholders: {items:?}"
    );

    // The single-element legacy row also converts.
    let single = criteria_from_json(&acceptance_of(&pool, "i-single").await).expect("decodes");
    assert_eq!(single.len(), 1);
    assert_eq!(single[0].text, "only one");

    // The empty default is untouched.
    assert_eq!(acceptance_of(&pool, "i-empty").await, "[]");

    // An already-structured row is left byte-identical — checked state and
    // provenance survive.
    let structured = acceptance_of(&pool, "i-structured").await;
    let kept = criteria_from_json(&structured).expect("decodes");
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].id, "ac-keep");
    assert!(kept[0].checked, "checked bit preserved");
    assert_eq!(kept[0].checked_at, Some(42));
    assert_eq!(kept[0].checked_by.as_deref(), Some("agent:builder"));
}

/// The JSON1 functions the migration leans on (`json_valid`,
/// `json_array_length`, `json_type`, `json_each`, `json_group_array`,
/// `json_object`) are compiled into the bundled SQLite. If this goes red the
/// migration body must degrade to a no-op and the Rust decoder carries the
/// whole upgrade path.
#[tokio::test]
async fn json1_functions_are_available() {
    let pool = SqlitePool::connect("sqlite::memory:")
        .await
        .expect("open memory pool");
    let valid: i64 = sqlx::query_scalar("SELECT json_valid('[\"a\"]')")
        .fetch_one(&pool)
        .await
        .expect("json_valid");
    assert_eq!(valid, 1);
    let len: i64 = sqlx::query_scalar("SELECT json_array_length('[\"a\",\"b\"]')")
        .fetch_one(&pool)
        .await
        .expect("json_array_length");
    assert_eq!(len, 2);
    let ty: String = sqlx::query_scalar("SELECT json_type('[\"a\"]', '$[0]')")
        .fetch_one(&pool)
        .await
        .expect("json_type");
    assert_eq!(ty, "text");
    let built: String = sqlx::query_scalar(
        "SELECT json_group_array(json_object('text', je.value, 'checked', json('false'))) \
         FROM json_each('[\"a\",\"b\"]') AS je",
    )
    .fetch_one(&pool)
    .await
    .expect("json_group_array + json_each + json_object");
    assert_eq!(
        built,
        r#"[{"text":"a","checked":false},{"text":"b","checked":false}]"#
    );
}
