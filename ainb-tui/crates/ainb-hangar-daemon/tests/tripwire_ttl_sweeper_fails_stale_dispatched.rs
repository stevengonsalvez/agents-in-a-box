//! P1.7 e2e tripwire: the live daemon's dispatched-TTL sweeper fails a stale
//! `dispatched` task.
//!
//! ```text
//!  seed dispatched task (dispatched_at = past TTL)
//!         │
//!         ▼   daemon (claim disabled, tiny dispatch TTL) sweeps
//!  dispatched ──────────────────────────────▶ failed (failure_reason=timeout)
//! ```
//!
//! Models the spec's scenario without a clock-injection seam into the spawned
//! binary: instead of advancing a `HangarClock`, we (a) seed the dispatch with
//! `dispatched_at` already older than the configured TTL, (b) shrink the
//! dispatch TTL + sweep interval via env so the real-clock sweeper fires
//! immediately, and (c) disable the claim loop so nothing else touches the row —
//! exactly the state the spec describes ("kill the fake claude before it can
//! call /start"). The sweeper then fails the row with `failure_reason='timeout'`
//! and emits the `sweeper_stale_dispatched`/`sweeper_swept` tracing line.

#![allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]

mod tripwire_support;

use std::time::Duration;

use sqlx::sqlite::SqlitePoolOptions;
use sqlx::{Row, SqlitePool};
use tripwire_support::{daemon_bin, now_ms, seed_world, wait_for_db, DaemonSession};

#[tokio::test]
async fn ttl_sweeper_fails_stale_dispatched() {
    if !tripwire_support::tmux_available() {
        eprintln!("tmux not available; skipping e2e tripwire");
        return;
    }

    let home = tempfile::tempdir().expect("tempdir home");
    let db_path = home.path().join("hangar.db");

    let pool = open_pool(&db_path).await;
    ainb_hangar_store::apply_migrations(&pool).await.expect("migrate");
    let ids = seed_world(&pool).await;

    // Seed a task already `dispatched`, with dispatched_at well past the tiny TTL
    // we configure below (and never started), so the sweeper fails it (not
    // reclaim — its age exceeds the TTL).
    let task_id = "task-stale-dispatch";
    let now = now_ms();
    sqlx::query(
        "INSERT INTO agent_task_queue \
         (id, workspace_id, runtime_id, agent_id, status, created_at, dispatched_at) \
         VALUES (?, ?, ?, ?, 'dispatched', ?, ?)",
    )
    .bind(task_id)
    .bind(&ids.workspace_id)
    .bind(&ids.runtime_id)
    .bind(&ids.agent_id)
    .bind(now) // created recently so the queued-TTL sweep ignores it
    .bind(now - 5_000) // dispatched 5s ago — far past the 200ms dispatch TTL
    .execute(&pool)
    .await
    .expect("seed dispatched task");

    // Daemon: claim disabled, dispatch TTL + sweep interval shrunk to 200ms so
    // the real-clock sweeper fires within the poll budget.
    let session = DaemonSession::spawn(
        &daemon_bin(),
        home.path(),
        &[
            ("AINB_HANGAR_HOME", home.path().to_str().unwrap()),
            ("HANGAR_DAEMON_DISABLE_CLAIM", "1"),
            ("HANGAR_SWEEP_INTERVAL_MS", "200"),
            ("HANGAR_SWEEP_DISPATCHED_TTL_MS", "200"),
            ("RUST_LOG", "info"),
        ],
    );

    let row = wait_for_db(&pool, task_id, "failed", Duration::from_secs(30)).await;

    // Capture the pane to prove the sweeper logged the stale-dispatch event
    // before tearing the session down.
    let pane = session.capture_pane();
    drop(session);

    let status: String = row.get("status");
    assert_eq!(status, "failed", "stale dispatch should be failed by the sweeper");
    let reason: Option<String> = row.get("failure_reason");
    assert_eq!(reason.as_deref(), Some("timeout"), "failure_reason should be timeout");

    assert!(
        pane.contains("sweeper_stale_dispatched") || pane.contains("sweeper_swept"),
        "daemon log should show a stale-dispatch sweep event; pane=\n{pane}"
    );
}

/// Open a `SQLite` WAL pool at `db_path` (creating the file if absent).
async fn open_pool(db_path: &std::path::Path) -> SqlitePool {
    let opts = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(db_path)
        .create_if_missing(true)
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal);
    SqlitePoolOptions::new()
        .connect_with(opts)
        .await
        .expect("open pool")
}
