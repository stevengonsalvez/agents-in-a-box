//! Regression tripwire: a PRE-START setup fault (a bad / unclonable `repo_ref`
//! that faults `workdir_provision::provision`) must drive the task chain TERMINAL
//! (`failed`, bounded by `max_attempts`) — never leave it reclaiming forever.
//!
//! ```text
//!  seed DB (ws+user+rt+agent)          spawn daemon in tmux
//!         │                                    │
//!         ▼                                    ▼
//!  INSERT queued task ──▶ claim ─▶ dispatched ─▶ provision(bad repo_ref) ─▶ Err
//!  (repo_ref = non-git path)                          │
//!                                                      ▼
//!                           finalize_pre_start_failure ─▶ running ─▶ failed
//!                           reason = "setup_error"          │
//!                                                           ▼
//!                              maybe_spawn_retry ─▶ child (attempt+1) … until
//!                              attempt == max_attempts ─▶ chain terminal, STOP
//! ```
//!
//! # The bug this pins (zombie-dispatch)
//!
//! When `execute_claimed` hit a pre-start setup fault (`prepare_env` /
//! `workdir_provision::provision`) it returned the `Err` via `?`. The claim loop
//! only logged `task execution errored`; the row stayed `dispatched`. The
//! stale-dispatch sweeper then redelivered it every reclaim window (~90s) with a
//! FRESH `dispatched_at` and an UNCHANGED `attempt` — so it never aged past the
//! dispatch TTL for the fail step, never consumed `max_attempts`, and reclaimed
//! FOREVER. A user saw a task frozen `dispatched`, `attempt` stuck at 1, its card
//! stuck in Todo with no failure badge, cycle after cycle.
//!
//! The fix finalises a pre-start fault terminal (a `setup_error`) through the
//! shared fail seam, so the F06 retry chain bounds it by `max_attempts` and the
//! chain lands `failed` on exhaustion.
//!
//! # Why no real provider / repo is needed
//!
//! A NON-GIT `repo_ref` path IS the whole test — `provision` faults before any
//! provider is spawned, so it needs no auth, no spend, no clone, and no
//! `live-e2e` gate. It exercises the GENUINE `execute_claimed` → provision-fault
//! → finalize path in the real daemon binary, and asserts the DB task ROWS reach
//! terminal `failed` — not merely that a log line was emitted.

#![allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]

mod tripwire_support;

use std::time::{Duration, Instant};

use sqlx::sqlite::SqlitePoolOptions;
use sqlx::{Row, SqlitePool};
use tripwire_support::{DaemonSession, daemon_bin, seed_world, wait_for_db};

#[tokio::test]
async fn pre_start_setup_fault_fails_chain_within_max_attempts() {
    // Skip cleanly when tmux is unavailable, so the tripwire never produces a
    // false failure on a host without the tool.
    if !tripwire_support::tmux_available() {
        eprintln!("SKIPPED: tmux not available; skipping setup-fault tripwire");
        return;
    }

    let home = tempfile::tempdir().expect("tempdir home");
    let db_path = home.path().join("hangar.db");

    // 1. Bring the DB up to schema and seed the minimal (claude-backed) world.
    let pool = open_pool(&db_path).await;
    ainb_hangar_store::apply_migrations(&pool).await.expect("migrate");
    let ids = seed_world(&pool).await;

    // 2. A `repo_ref` that points at a path that is NOT a git repo, so
    //    `workdir_provision::provision` faults BEFORE any provider spawn — the
    //    exact pre-start stranding trigger the fix must finalise.
    let bogus_repo = home.path().join("not_a_git_repo");
    assert!(!bogus_repo.exists(), "the bogus repo path must not exist");

    // 3. Spawn the real daemon binary in a uniquely-named tmux session. Sandbox
    //    off + a bogus claude path are harmless (the provider is never reached);
    //    fast poll so the claim + retry chain settle quickly. Sweeper timing is
    //    left at defaults — the fix makes the row terminal in `execute_claimed`,
    //    so this test must pass WITHOUT relying on any sweeper pass.
    let session = DaemonSession::spawn(
        &daemon_bin(),
        home.path(),
        &[
            ("AINB_HANGAR_HOME", home.path().to_str().unwrap()),
            ("HANGAR_DAEMON_RUNTIME_ID", &ids.runtime_id),
            ("HANGAR_CLAUDE_PATH", "/nonexistent/claude"),
            ("HANGAR_DAEMON_DISABLE_SANDBOX", "1"),
            ("HANGAR_DAEMON_POLL_MS", "200"),
        ],
    );

    // 4. Enqueue a queued task with the bad `repo_ref` and `max_attempts = 2`.
    //    `created_at` ~now so the queued-TTL sweeper does not reap it first.
    let task_id = "task-setup-fault-1";
    let max_attempts: i64 = 2;
    sqlx::query(
        "INSERT INTO agent_task_queue \
         (id, workspace_id, runtime_id, agent_id, repo_ref, max_attempts, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(task_id)
    .bind(&ids.workspace_id)
    .bind(&ids.runtime_id)
    .bind(&ids.agent_id)
    .bind(bogus_repo.to_str().unwrap())
    .bind(max_attempts)
    .bind(tripwire_support::now_ms())
    .execute(&pool)
    .await
    .expect("enqueue task");

    // 5. The ORIGINAL (attempt 1) row must reach `failed` fast — far below any
    //    sweeper cadence, so a pass here can only come from the finalize-on-
    //    setup-fault path, never a sweeper backstop. Before the fix this times out
    //    with `last=Some(("dispatched", None))` (the zombie state).
    let row = wait_for_db(&pool, task_id, "failed", Duration::from_secs(30)).await;
    let reason: Option<String> = row.get("failure_reason");
    assert_eq!(
        reason.as_deref(),
        Some("setup_error"),
        "the failed row must carry the setup_error reason, not sit reason-less"
    );
    let finished_at: Option<i64> = row.get("finished_at");
    assert!(
        finished_at.is_some(),
        "a finalized failed row stamps finished_at"
    );
    let attempt: i64 = row.get("attempt");
    assert_eq!(attempt, 1, "the original row is attempt 1");

    // 6. The CHAIN must settle terminal: the F06 retry bounds the setup fault by
    //    `max_attempts`, so within a bounded budget there is a `failed` row at
    //    `attempt == max_attempts` AND no row for this agent is left `dispatched`
    //    or `queued` (the proof there is no infinite reclaim loop).
    let settled =
        wait_for_chain_settled(&pool, &ids.agent_id, max_attempts, Duration::from_secs(30)).await;

    // Kill the tmux session by exact name before the final assertions so the
    // daemon cannot mutate rows underneath them.
    drop(session);

    assert!(
        settled.terminal_at_max,
        "expected a failed row at attempt=={max_attempts}; rows={:?}",
        settled.rows
    );
    assert_eq!(
        settled.non_terminal, 0,
        "no row may be left dispatched/queued (would be an infinite reclaim loop); rows={:?}",
        settled.rows
    );
    // Bounded: exactly `max_attempts` rows (attempt 1 + one retry child), never an
    // unbounded pile of reclaim-spawned children.
    assert_eq!(
        settled.rows.len() as i64,
        max_attempts,
        "the chain must be bounded by max_attempts; rows={:?}",
        settled.rows
    );
    // Every terminal row carries the setup reason (no reason-less zombie left).
    for (_id, status, r_attempt, r_reason) in &settled.rows {
        assert_eq!(status, "failed", "every chain row must be terminal failed");
        assert_eq!(
            r_reason.as_deref(),
            Some("setup_error"),
            "chain row (attempt {r_attempt}) must carry setup_error"
        );
    }
}

/// The observed state of a task chain (all rows sharing one agent).
struct ChainState {
    /// `(id, status, attempt, failure_reason)` for every chain row.
    rows: Vec<(String, String, i64, Option<String>)>,
    /// Rows still `dispatched` or `queued` (a non-zero count is the zombie bug).
    non_terminal: usize,
    /// Whether some `failed` row reached `attempt == max_attempts`.
    terminal_at_max: bool,
}

/// Poll the agent's task rows until the chain has settled — no `dispatched` /
/// `queued` row remains AND a `failed` row reached `attempt == max_attempts` — or
/// the budget elapses (returning the last-seen state either way, so the caller's
/// assertions render the failure).
async fn wait_for_chain_settled(
    pool: &SqlitePool,
    agent_id: &str,
    max_attempts: i64,
    budget: Duration,
) -> ChainState {
    let deadline = Instant::now() + budget;
    loop {
        let state = fetch_chain(pool, agent_id, max_attempts).await;
        let settled = state.non_terminal == 0 && state.terminal_at_max;
        if settled || Instant::now() >= deadline {
            return state;
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
}

/// Snapshot every task row for `agent_id` and summarise the chain state.
async fn fetch_chain(pool: &SqlitePool, agent_id: &str, max_attempts: i64) -> ChainState {
    let rows = sqlx::query(
        "SELECT id, status, attempt, failure_reason FROM agent_task_queue \
         WHERE agent_id = ? ORDER BY attempt",
    )
    .bind(agent_id)
    .fetch_all(pool)
    .await
    .expect("query chain");

    let mut out = Vec::new();
    let mut non_terminal = 0usize;
    let mut terminal_at_max = false;
    for row in rows {
        let id: String = row.get("id");
        let status: String = row.get("status");
        let attempt: i64 = row.get("attempt");
        let reason: Option<String> = row.get("failure_reason");
        if status == "dispatched" || status == "queued" {
            non_terminal += 1;
        }
        if status == "failed" && attempt == max_attempts {
            terminal_at_max = true;
        }
        out.push((id, status, attempt, reason));
    }
    ChainState {
        rows: out,
        non_terminal,
        terminal_at_max,
    }
}

/// Open a `SQLite` WAL pool at `db_path` (creating the file if absent).
async fn open_pool(db_path: &std::path::Path) -> SqlitePool {
    let opts = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(db_path)
        .create_if_missing(true)
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal);
    SqlitePoolOptions::new().connect_with(opts).await.expect("open pool")
}
