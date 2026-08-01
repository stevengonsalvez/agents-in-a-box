//! 0075 prunes the tmux transport flip-flop artefacts without stranding a
//! replay cursor.
//!
//! The churn bug (fixed in PR #554) wrote one `tmux_missing` and one
//! `tmux_available` per orphaned row per three-second tick. On a real profile
//! that was 39,961 of 46,002 `fleet_event` rows. The prune must remove that
//! noise while preserving two invariants clients depend on:
//!
//!   - the ledger HEAD is never deleted, so no cursor can point past the end;
//!   - each session keeps its NEWEST transport event, so provenance survives.
//!
//! `revision` is `INTEGER PRIMARY KEY AUTOINCREMENT`, so deleted revisions are
//! never reused and every replay path (`revision > ?`) simply skips the holes.

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};

use ainb_hangar_store::apply_migrations;

/// The migration under test; the pool is seeded at the version before it.
const PRUNE_VERSION: i64 = 75;

fn migrations_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations")
}

/// Apply every migration BELOW [`PRUNE_VERSION`], so rows can be seeded into the
/// schema exactly as it stood before the prune shipped.
async fn pool_before_prune(dir: &std::path::Path) -> SqlitePool {
    let opts = SqliteConnectOptions::new()
        .filename(dir.join("hangar.db"))
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal);
    let pool = SqlitePoolOptions::new().connect_with(opts).await.expect("open pool");

    let mut migrator =
        sqlx::migrate::Migrator::new(migrations_dir()).await.expect("load migrations");
    migrator.migrations.to_mut().retain(|m| m.version < PRUNE_VERSION);
    migrator.run(&pool).await.expect("pre-prune migrations apply");
    pool
}

async fn seed_session(pool: &SqlitePool, key: &str) {
    sqlx::query(
        "INSERT INTO fleet_session (session_key, discovered_at, last_observed_at) VALUES (?, 1, 1)",
    )
    .bind(key)
    .execute(pool)
    .await
    .expect("seed fleet_session");
}

async fn seed_event(pool: &SqlitePool, key: &str, event_id: &str, event_type: &str) -> i64 {
    sqlx::query(
        "INSERT INTO fleet_event \
         (event_id, session_key, observed_at, authority, event_type, payload, session_version, applied) \
         VALUES (?, ?, 1, 'authoritative', ?, '{}', 1, 1)",
    )
    .bind(event_id)
    .bind(key)
    .bind(event_type)
    .execute(pool)
    .await
    .expect("seed fleet_event");
    sqlx::query("SELECT MAX(revision) AS r FROM fleet_event")
        .fetch_one(pool)
        .await
        .expect("head")
        .get::<i64, _>("r")
}

async fn count(pool: &SqlitePool, sql: &str) -> i64 {
    sqlx::query(sql).fetch_one(pool).await.expect("count").get::<i64, _>(0)
}

#[tokio::test]
async fn prune_removes_churn_but_keeps_the_head_and_one_transport_event_per_session() {
    let dir = tempfile::tempdir().expect("tmpdir");
    let pool = pool_before_prune(dir.path()).await;

    seed_session(&pool, "sess-a").await;
    seed_session(&pool, "sess-b").await;

    // Two sessions flip-flopping, exactly the shape the churn bug produced.
    for i in 0..20 {
        seed_event(&pool, "sess-a", &format!("a-miss-{i}"), "tmux_missing").await;
        seed_event(&pool, "sess-a", &format!("a-avail-{i}"), "tmux_available").await;
        seed_event(&pool, "sess-b", &format!("b-miss-{i}"), "tmux_missing").await;
    }
    // A real transition that must survive untouched, and then the ledger HEAD —
    // deliberately a transport event, so the "never delete the head" clause is
    // actually exercised rather than trivially satisfied.
    seed_event(&pool, "sess-a", "real-stop", "Stop").await;
    let head = seed_event(&pool, "sess-b", "b-head", "tmux_available").await;

    let before = count(&pool, "SELECT COUNT(*) FROM fleet_event").await;
    assert_eq!(before, 62, "20*3 churn + 1 real + 1 head");

    apply_migrations(&pool).await.expect("prune migration applies");

    // The head is untouched, so no client cursor can point past the end.
    assert_eq!(
        count(&pool, "SELECT MAX(revision) FROM fleet_event").await,
        head,
        "the ledger head must never be deleted"
    );
    assert_eq!(
        count(
            &pool,
            "SELECT COUNT(*) FROM fleet_event WHERE event_id = 'b-head'"
        )
        .await,
        1,
        "the head row itself must survive even though it is a transport event"
    );

    // The real transition is not churn and must survive.
    assert_eq!(
        count(
            &pool,
            "SELECT COUNT(*) FROM fleet_event WHERE event_type = 'Stop'"
        )
        .await,
        1,
        "a real transition must never be pruned"
    );

    // Exactly one transport event remains per session: sess-a's newest, and
    // sess-b's newest (which is the head).
    for key in ["sess-a", "sess-b"] {
        let remaining = count(
            &pool,
            &format!(
                "SELECT COUNT(*) FROM fleet_event WHERE session_key = '{key}' \
                 AND event_type IN ('tmux_missing','tmux_available')"
            ),
        )
        .await;
        assert_eq!(remaining, 1, "{key} must keep exactly one transport event");
    }

    assert_eq!(
        count(&pool, "SELECT COUNT(*) FROM fleet_event").await,
        3,
        "one transport event per session plus the real transition"
    );
}
