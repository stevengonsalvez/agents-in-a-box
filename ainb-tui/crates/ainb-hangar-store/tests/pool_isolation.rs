//! Pool wiring + isolation tests for [`ainb_hangar_store::Store`].
//!
//! Each test uses a real on-disk `tempdir/hangar.db` (not `:memory:`) so the
//! `WAL` journal mode is exercised exactly as it is in production, and two
//! stores in two distinct tempdirs are provably independent.

use ainb_hangar_store::Store;
use sqlx::Row;

#[tokio::test]
async fn open_pool_in_tempdir_creates_db_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");

    let db_path = dir.path().join("hangar.db");
    assert!(db_path.exists(), "hangar.db should exist at {db_path:?}");

    // Pool is live and usable.
    let one: i64 = sqlx::query("SELECT 1")
        .fetch_one(store.pool())
        .await
        .expect("SELECT 1")
        .get(0);
    assert_eq!(one, 1);
}

#[tokio::test]
async fn two_pools_in_separate_tempdirs_do_not_collide() {
    let dir_a = tempfile::tempdir().expect("tempdir a");
    let dir_b = tempfile::tempdir().expect("tempdir b");

    let store_a = Store::open_in(dir_a.path()).await.expect("open a");
    let store_b = Store::open_in(dir_b.path()).await.expect("open b");

    insert_workspace(&store_a, "ws-a", "alpha").await;
    insert_workspace(&store_b, "ws-b", "beta").await;

    // Each pool only sees its own row.
    assert_eq!(workspace_slugs(&store_a).await, vec!["alpha".to_string()]);
    assert_eq!(workspace_slugs(&store_b).await, vec!["beta".to_string()]);
}

/// Insert one workspace row into `store`.
async fn insert_workspace(store: &Store, id: &str, slug: &str) {
    sqlx::query("INSERT INTO workspace (id, slug, name, created_at) VALUES (?, ?, ?, ?)")
        .bind(id)
        .bind(slug)
        .bind(slug)
        .bind(0_i64)
        .execute(store.pool())
        .await
        .expect("insert workspace");
}

/// Return all workspace slugs visible to `store`.
async fn workspace_slugs(store: &Store) -> Vec<String> {
    sqlx::query("SELECT slug FROM workspace ORDER BY slug")
        .fetch_all(store.pool())
        .await
        .expect("select slugs")
        .into_iter()
        .map(|r| r.get::<String, _>("slug"))
        .collect()
}
