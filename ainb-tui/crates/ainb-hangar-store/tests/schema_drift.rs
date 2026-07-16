//! Tripwire for [`ainb_hangar_store::schema_drift`] — the stale-daemon
//! detector behind the daemon-health pane's red banner.
//!
//! The trap it guards: a stale daemon binary keeps serving a database a NEWER
//! binary has migrated forward. `sqlx` only validates at pool-open, so the old
//! daemon never errors — every pane just renders silent zeros. The probe must
//! flag "applied schema AHEAD of this binary"; deleting the probe (or stubbing
//! it to `None`) turns `applied_ahead_of_binary_is_drift` RED.

use ainb_hangar_store::{apply_migrations, schema_drift};
use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use std::str::FromStr;

/// Open a fresh on-disk `SQLite` pool inside a tempdir and apply all migrations.
async fn fresh_pool(dir: &std::path::Path) -> SqlitePool {
    let db_path = dir.join("hangar.db");
    let opts = SqliteConnectOptions::from_str(&format!("sqlite://{}", db_path.display()))
        .expect("valid sqlite url")
        .create_if_missing(true);
    let pool = SqlitePoolOptions::new().connect_with(opts).await.expect("open pool");
    apply_migrations(&pool).await.expect("migrations apply");
    pool
}

/// A freshly migrated database is exactly at this binary's schema: no drift.
#[tokio::test]
async fn fresh_database_reports_no_drift() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = fresh_pool(dir.path()).await;
    assert_eq!(schema_drift(&pool).await, None);
}

/// A database whose applied `MAX(version)` is AHEAD of this binary's embedded
/// migrations (i.e. a newer binary migrated it) is flagged as drift, naming
/// both versions so the banner tells the operator exactly what happened.
#[tokio::test]
async fn applied_ahead_of_binary_is_drift() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = fresh_pool(dir.path()).await;

    // Simulate a newer binary having migrated the DB forward: record a future
    // migration version directly in the sqlx ledger.
    sqlx::query(
        "INSERT INTO _sqlx_migrations \
         (version, description, installed_on, success, checksum, execution_time) \
         VALUES (999999, 'from_the_future', CURRENT_TIMESTAMP, TRUE, x'00', 0)",
    )
    .execute(&pool)
    .await
    .expect("insert future migration row");

    let drift = schema_drift(&pool).await.expect("future schema must be flagged");
    assert!(
        drift.contains("999999") && drift.contains("AHEAD"),
        "drift message must name the future version: {drift}"
    );
}

/// A dead database (probe query cannot run) is drift too — the banner must
/// fire on outright unreachability, not only on version skew.
#[tokio::test]
async fn missing_ledger_is_drift() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = fresh_pool(dir.path()).await;
    sqlx::query("DROP TABLE _sqlx_migrations")
        .execute(&pool)
        .await
        .expect("drop ledger");
    let drift = schema_drift(&pool).await.expect("dead ledger must be flagged");
    assert!(
        drift.contains("unreachable"),
        "probe failure must read as unreachable: {drift}"
    );
}
