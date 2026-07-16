//! Regression tripwire: a provider whose binary cannot be spawned (ENOENT) must
//! drive the task to `failed` WITHIN the dispatch budget — never leave it frozen
//! `running` until the multi-hour running-TTL sweep.
//!
//! ```text
//!  seed DB (ws+user+rt+agent)        spawn daemon in tmux
//!         │                       HANGAR_CLAUDE_PATH = <non-existent path>
//!         ▼                                  │
//!  INSERT queued task ──▶ claim ─▶ dispatched ─▶ running ─▶ (spawn ENOENT)
//!                                                              │
//!                                                              ▼
//!                                        finalize_failure ─▶ failed
//!                                        failure_reason = "spawn_error"
//! ```
//!
//! # The bug this pins
//!
//! When `execute_claimed` reached the provider spawn and it returned `Err`
//! (`claude` / `codex` path does not resolve → ENOENT), the claim loop only
//! logged `task execution errored` and CONTINUED — the task was already
//! `running`, so it sat `running` forever until the reference running-TTL (2.5h)
//! sweep. A user saw a task that had already died sitting `running` indefinitely,
//! well past any dispatch budget. The fix converts the provider spawn `Err` into
//! a terminal FAILED outcome routed through the SAME `finalize_failure` seam the
//! agent-error path uses, so the row reaches `failed` (reason `spawn_error`)
//! immediately. The running-TTL sweeper stays as the backstop for a crash that
//! never reaches this arm.
//!
//! # Why no real provider is needed
//!
//! A NON-EXISTENT binary path IS the whole test — there is nothing to run, so it
//! needs no auth, no spend, and no `live-e2e` gate. It exercises the GENUINE
//! `execute_claimed` → provider spawn → `finalize_failure` path in the real
//! daemon binary (no mock of the loop), and asserts the DB task ROW transitions
//! to `failed` — not merely that a log line was emitted.
//!
//! # Sandbox
//!
//! Sandbox is OFF (`HANGAR_DAEMON_DISABLE_SANDBOX=1`) so the daemon spawns the
//! provider path DIRECTLY. With the sandbox ON the program spawned is the
//! (existing) `sandbox-exec` wrapper, which would spawn fine and the missing
//! `claude` would surface as a non-zero EXIT rather than the spawn `Err` this
//! test pins.

#![allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]

mod tripwire_support;

use std::time::Duration;

use sqlx::sqlite::SqlitePoolOptions;
use sqlx::{Row, SqlitePool};
use tripwire_support::{DaemonSession, daemon_bin, seed_world, wait_for_db};

#[tokio::test]
async fn provider_spawn_enoent_fails_task_within_budget() {
    // Skip cleanly when tmux is unavailable (CI image lacking tmux), so the
    // tripwire never produces a false failure on a host without the tool.
    if !tripwire_support::tmux_available() {
        eprintln!("SKIPPED: tmux not available; skipping spawn-error tripwire");
        return;
    }

    let home = tempfile::tempdir().expect("tempdir home");
    let db_path = home.path().join("hangar.db");

    // 1. Bring the DB up to schema and seed the minimal (claude-backed) world.
    let pool = open_pool(&db_path).await;
    ainb_hangar_store::apply_migrations(&pool).await.expect("migrate");
    let ids = seed_world(&pool).await;

    // 2. Point the daemon's `claude` at a path that DOES NOT EXIST, so the direct
    //    (sandbox-off) spawn hits ENOENT — the exact production stranding trigger.
    let bogus_claude = home.path().join("no_such_claude_binary");
    assert!(
        !bogus_claude.exists(),
        "the bogus claude path must not exist"
    );

    // 3. Spawn the real daemon binary in a uniquely-named tmux session, sandbox
    //    OFF so the bogus path is spawned directly (see module docs).
    let session = DaemonSession::spawn(
        &daemon_bin(),
        home.path(),
        &[
            ("AINB_HANGAR_HOME", home.path().to_str().unwrap()),
            ("HANGAR_DAEMON_RUNTIME_ID", &ids.runtime_id),
            ("HANGAR_CLAUDE_PATH", bogus_claude.to_str().unwrap()),
            ("HANGAR_DAEMON_DISABLE_SANDBOX", "1"),
            ("HANGAR_DAEMON_POLL_MS", "200"),
        ],
    );

    // 4. Enqueue a queued task via direct SQL (the daemon is the only claimer).
    //    `created_at` must be ~now so the queued-TTL sweeper does not reap it
    //    before the claim loop sees it.
    let task_id = "task-spawn-err-1";
    sqlx::query(
        "INSERT INTO agent_task_queue (id, workspace_id, runtime_id, agent_id, created_at) \
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(task_id)
    .bind(&ids.workspace_id)
    .bind(&ids.runtime_id)
    .bind(&ids.agent_id)
    .bind(tripwire_support::now_ms())
    .execute(&pool)
    .await
    .expect("enqueue task");

    // 5. Poll for `failed` within a bounded DISPATCH budget. Before the fix this
    //    times out (task stranded `running`) and `wait_for_db` panics loudly with
    //    `last=Some(("running", None))`; after the fix it returns the failed row.
    //    30s is far below the 2.5h running-TTL, so a pass here can only come from
    //    the finalize-on-spawn-error path, never the sweeper backstop.
    let row = wait_for_db(&pool, task_id, "failed", Duration::from_secs(30)).await;
    drop(session); // kill the tmux session by exact name before assertions

    let status: String = row.get("status");
    assert_eq!(
        status, "failed",
        "spawn ENOENT must finalize the task to failed"
    );

    // The reason is captured on the row (not just logged): the operator sees a
    // provider that could not be spawned, distinct from an agent that gave up.
    let reason: Option<String> = row.get("failure_reason");
    assert_eq!(
        reason.as_deref(),
        Some("spawn_error"),
        "failed row must carry the spawn_error reason"
    );

    // `finished_at` is stamped by the finalize seam — the row is genuinely
    // terminal, not merely relabelled.
    let finished_at: Option<i64> = row.get("finished_at");
    assert!(
        finished_at.is_some(),
        "a finalized failed row stamps finished_at"
    );
}

/// Open a `SQLite` WAL pool at `db_path` (creating the file if absent).
async fn open_pool(db_path: &std::path::Path) -> SqlitePool {
    let opts = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(db_path)
        .create_if_missing(true)
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal);
    SqlitePoolOptions::new().connect_with(opts).await.expect("open pool")
}
