//! Upgrade-from-populated test for migration 0063 (`workspace_invitation` —
//! multica parity #18).
//!
//! Fresh-database coverage lives in `tripwire_migrations_apply.rs`. This file
//! proves the migration on a REAL populated database:
//!
//! 1. apply only the PRIOR migrations (0001..0062),
//! 2. seed a workspace with members,
//! 3. apply the embedded migrations (which adds 0063),
//! 4. assert the seeded rows survive, the table and indexes exist, and the
//!    PARTIAL unique index actually bites — two `pending` rows for the same
//!    (workspace, email) must fail, while `(pending, accepted)` for the same
//!    pair must succeed (otherwise a re-invite after expiry/decline would be
//!    permanently blocked).

use ainb_hangar_store::apply_migrations;
use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};

/// `sqlx` version number of the migration under test
/// (`0063_workspace_invitation.sql`).
const NEW_MIGRATION_VERSION: i64 = 63;

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

/// The pre-0063 world: a workspace with an owner and a second member.
async fn seed_populated(pool: &SqlitePool) {
    for sql in [
        "INSERT INTO workspace (id, slug, name, created_at) VALUES ('ws-1','alpha','Alpha',0)",
        "INSERT INTO user (id, email, created_at) VALUES ('u-amy','amy@x.io',10)",
        "INSERT INTO user (id, email, created_at) VALUES ('u-bob','bob@x.io',20)",
        "INSERT INTO member (workspace_id, user_id, role) VALUES ('ws-1','u-amy','owner')",
        "INSERT INTO member (workspace_id, user_id, role) VALUES ('ws-1','u-bob','member')",
    ] {
        sqlx::query(sql).execute(pool).await.expect(sql);
    }
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

/// Insert one invitation row directly (bypassing the repo) so the test exercises
/// the DDL, not the application-level guard.
async fn insert_invite(pool: &SqlitePool, id: &str, email: &str, status: &str) -> bool {
    sqlx::query(
        "INSERT INTO workspace_invitation \
         (id, workspace_id, inviter_id, invitee_email, role, status, \
          created_at, updated_at, expires_at) \
         VALUES (?, 'ws-1', 'u-amy', ?, 'member', ?, 100, 100, 999999999999)",
    )
    .bind(id)
    .bind(email)
    .bind(status)
    .execute(pool)
    .await
    .is_ok()
}

#[tokio::test]
async fn migration_0063_adds_workspace_invitation_and_keeps_members() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = pool_at_prior_schema(dir.path()).await;
    seed_populated(&pool).await;

    assert!(
        !object_exists(&pool, "table", "workspace_invitation").await,
        "workspace_invitation must not exist before 0063"
    );

    apply_migrations(&pool).await.expect("upgrade applies 0063");
    let recorded: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM _sqlx_migrations WHERE version = ?")
            .bind(NEW_MIGRATION_VERSION)
            .fetch_one(&pool)
            .await
            .expect("read migration version");
    assert_eq!(recorded, 1, "0063 recorded as applied");

    // (a) The seeded data survived the upgrade untouched.
    let members: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM member WHERE workspace_id = 'ws-1'")
            .fetch_one(&pool)
            .await
            .expect("count members");
    assert_eq!(members, 2, "the upgrade preserved existing memberships");

    // (b) The table and every declared index landed.
    assert!(object_exists(&pool, "table", "workspace_invitation").await);
    for idx in [
        "idx_invitation_unique_pending",
        "idx_invitation_invitee_email",
        "idx_invitation_invitee_user",
        "idx_invitation_workspace_status",
    ] {
        assert!(
            object_exists(&pool, "index", idx).await,
            "missing index {idx}"
        );
    }

    // (c) The partial unique index bites on two LIVE pending rows...
    assert!(insert_invite(&pool, "inv-1", "dana@example.com", "pending").await);
    assert!(
        !insert_invite(&pool, "inv-2", "dana@example.com", "pending").await,
        "a second pending invite for the same (workspace, email) must be rejected"
    );

    // ...and NOT on a (pending, accepted) pair — status-scoped, so a re-invite
    // after a closed invitation is always possible.
    assert!(
        insert_invite(&pool, "inv-3", "eve@example.com", "accepted").await,
        "an accepted row coexists"
    );
    assert!(
        insert_invite(&pool, "inv-4", "eve@example.com", "pending").await,
        "an accepted invite must not block a fresh pending one"
    );

    // (d) The status CHECK is engine-enforced.
    assert!(
        !insert_invite(&pool, "inv-5", "zed@example.com", "revoked").await,
        "status is CHECK-constrained to the closed set"
    );

    // (e) Re-applying is a ledgered no-op.
    apply_migrations(&pool).await.expect("second apply is a no-op");
}
