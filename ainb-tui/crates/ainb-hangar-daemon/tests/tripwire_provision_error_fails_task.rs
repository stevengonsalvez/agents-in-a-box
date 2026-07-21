//! Regression tripwire: a task whose PRE-RUN provisioning faults (a `repo_ref`
//! that cannot be worktree-added) must drive the row to `failed` WITHIN the
//! dispatch budget — never loop `dispatched -> reclaim -> re-dispatch` invisibly.
//!
//! ```text
//!  seed DB (ws+user+rt+agent)             spawn daemon in tmux
//!         │                          repo_ref = <non-existent repo path>
//!         ▼                                       │
//!  INSERT queued task ──▶ claim ─▶ dispatched ─▶ provision(repo_ref) → Err
//!   (repo_ref = bad path)                              │
//!                                                      ▼
//!                              finalize_setup_failure ─▶ failed
//!                              failure_reason = "provision_error"
//!                              result.content = the real git error
//! ```
//!
//! # The bug this pins
//!
//! The daemon claims a task (`queued -> dispatched`) and provisions its working
//! directory from the card's `repo_ref` BEFORE the `dispatched -> running` start
//! transition. When provisioning faulted (e.g. the repo path could not be
//! `git worktree add`-ed), `execute_claimed` propagated the error with the row
//! left `dispatched`. The claim loop only logged `task execution errored` and
//! continued; the stale-dispatch sweeper then reclaimed the row past the 90s
//! window and re-dispatched it into the SAME fault, looping — the board card
//! never left Todo and the task-detail never showed an error — until the 5min
//! dispatch TTL relabelled it `timeout` with NO cause recorded. The fix
//! terminalises the still-`dispatched` row to `failed` AT ONCE via
//! `finalize_setup_failure`, recording the real error (reason `provision_error`,
//! message on the `result` column) so the failure is terminal AND visible.
//!
//! # Why no real provider / repo is needed
//!
//! A NON-EXISTENT repo path IS the whole test — provisioning fails before any
//! provider is reached, so it needs no auth, no spend, and no `live-e2e` gate. It
//! exercises the GENUINE `execute_claimed` -> `provision` -> `finalize_setup_failure`
//! path in the real daemon binary (no mock of the loop), and asserts the DB task
//! ROW transitions to `failed` within a bounded DISPATCH budget — not merely that
//! a log line was emitted, and far below the 5min dispatch TTL so a pass can only
//! come from the terminalise-on-setup-error path, never the sweeper backstop.

#![allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]

mod tripwire_support;

use std::time::Duration;

use sqlx::sqlite::SqlitePoolOptions;
use sqlx::{Row, SqlitePool};
use tripwire_support::{DaemonSession, daemon_bin, seed_world, wait_for_db};

#[tokio::test]
async fn provision_fault_fails_task_within_budget() {
    // Skip cleanly when tmux is unavailable (CI image lacking tmux), so the
    // tripwire never produces a false failure on a host without the tool.
    if !tripwire_support::tmux_available() {
        eprintln!("SKIPPED: tmux not available; skipping provision-error tripwire");
        return;
    }

    let home = tempfile::tempdir().expect("tempdir home");
    let db_path = home.path().join("hangar.db");

    // 1. Bring the DB up to schema and seed the minimal (claude-backed) world.
    let pool = open_pool(&db_path).await;
    ainb_hangar_store::apply_migrations(&pool).await.expect("migrate");
    let ids = seed_world(&pool).await;

    // 2. Spawn the real daemon binary in a uniquely-named tmux session, claiming
    //    for the seeded runtime. No provider path override is needed — the run
    //    fails during provisioning, before any provider is reached.
    let session = DaemonSession::spawn(
        &daemon_bin(),
        home.path(),
        &[
            ("AINB_HANGAR_HOME", home.path().to_str().unwrap()),
            ("HANGAR_DAEMON_RUNTIME_ID", &ids.runtime_id),
            ("HANGAR_DAEMON_POLL_MS", "200"),
        ],
    );

    // 3. Enqueue a queued task whose `repo_ref` names a path that is NOT a git
    //    repo, so `provision` -> `git worktree add` fails deterministically while
    //    the row is still `dispatched`. `created_at` is ~now so the queued-TTL
    //    sweeper does not reap it before the claim loop sees it.
    let bad_repo = home.path().join("no_such_repo");
    assert!(!bad_repo.exists(), "the bogus repo path must not exist");
    let task_id = "task-provision-err-1";
    sqlx::query(
        "INSERT INTO agent_task_queue \
         (id, workspace_id, runtime_id, agent_id, repo_ref, created_at) \
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(task_id)
    .bind(&ids.workspace_id)
    .bind(&ids.runtime_id)
    .bind(&ids.agent_id)
    .bind(bad_repo.to_str().unwrap())
    .bind(tripwire_support::now_ms())
    .execute(&pool)
    .await
    .expect("enqueue task");

    // 4. Poll for `failed` within a bounded DISPATCH budget. Before the fix this
    //    times out (task loops `dispatched`) and `wait_for_db` panics loudly with
    //    `last=Some(("dispatched", ...))`; after the fix it returns the failed row.
    //    30s is far below the 5min dispatch TTL, so a pass here can only come from
    //    the terminalise-on-setup-error path, never the sweeper timeout backstop.
    let row = wait_for_db(&pool, task_id, "failed", Duration::from_secs(30)).await;
    drop(session); // kill the tmux session by exact name before assertions

    let status: String = row.get("status");
    assert_eq!(
        status, "failed",
        "a provisioning fault must finalize the task to failed"
    );

    // The reason is captured on the row (not just logged): the operator sees a run
    // that could not be set up, distinct from a spawn error or an agent give-up.
    let reason: Option<String> = row.get("failure_reason");
    assert_eq!(
        reason.as_deref(),
        Some("provision_error"),
        "failed row must carry the provision_error reason"
    );

    // The real error is persisted into `result` (the detail surface renders it),
    // so the failure is attributed, not opaque.
    let result: Option<String> = row.get("result");
    let result = result.expect("a terminalised setup failure persists a result blob");
    assert!(
        result.contains("run setup failed before the agent started"),
        "result must carry the human-readable setup error, got {result:?}"
    );

    // `finished_at` is stamped by the finalize seam — the row is genuinely
    // terminal, not merely relabelled.
    let finished_at: Option<i64> = row.get("finished_at");
    assert!(
        finished_at.is_some(),
        "a finalized failed row stamps finished_at"
    );

    // The attempt counter never advanced past the first claim: the fix terminalises
    // on the FIRST fault rather than looping reclaim/re-dispatch (which is what left
    // the old behaviour invisible — no retry increment, no terminal state).
    let attempt: i64 = row.get("attempt");
    assert_eq!(
        attempt, 1,
        "a terminalised setup failure never re-dispatches"
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
