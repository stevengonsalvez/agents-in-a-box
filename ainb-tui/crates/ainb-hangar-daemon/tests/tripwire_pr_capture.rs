//! P9.1 e2e tripwire: a real `ainb-hangar-daemon` binary claims a task whose
//! fake-`claude` agent shells out to a fake `gh pr create`; the daemon captures
//! the printed PR URL into `agent_task_queue.result.pr_url` — no mocks, no real
//! `gh`, no network.
//!
//! ```text
//!  seed DB + fake gh on PATH        spawn daemon in tmux
//!         │                                  │
//!         ▼                                  ▼
//!  INSERT queued task ───────▶ claim ─▶ running ─▶ fake-claude.sh
//!                                                      │
//!                                            `gh pr create` → prints PR URL
//!                                                      │
//!                                   done · result->>'pr_url' = the URL
//! ```
//!
//! Mirrors the P1.7 happy-path harness (`tripwire_task_happy_path_claude_provider`):
//! the daemon is the genuine release binary, configured via env vars, and we
//! prepend a tempdir holding a fake `gh` to its `PATH` so the agent's
//! `gh pr create` resolves to the stand-in. The tmux session is killed by exact
//! name on drop (per the `tmux_protection` global rule).

#![allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]

mod tripwire_support;

use std::time::Duration;

use sqlx::sqlite::SqlitePoolOptions;
use sqlx::{Row, SqlitePool};
use tripwire_support::{
    DaemonSession, daemon_bin, fake_claude_runs_gh, fake_gh_pr_create, fake_gh_pr_create_fails,
    seed_world, wait_for_db,
};

const FAKE_PR_URL: &str = "https://github.com/test/repo/pull/42";

/// TRIPWIRE 1: the agent runs `gh pr create`; the daemon stamps the printed PR
/// URL into `result.pr_url`.
#[tokio::test]
async fn agent_gh_pr_create_url_is_captured_into_result() {
    if !tripwire_support::tmux_available() {
        eprintln!("tmux not available; skipping e2e tripwire");
        return;
    }

    let home = tempfile::tempdir().expect("tempdir home");
    let db_path = home.path().join("hangar.db");

    let pool = open_pool(&db_path).await;
    ainb_hangar_store::apply_migrations(&pool).await.expect("migrate");
    let ids = seed_world(&pool).await;

    // Fake `gh` (prints the PR URL on `pr create`) + a fake claude that runs it.
    let fake_bin = fake_gh_pr_create(home.path(), FAKE_PR_URL);
    let fake_claude = fake_claude_runs_gh(home.path(), "trip-pr-1");
    let path = prepend_path(&fake_bin);

    let session = DaemonSession::spawn(
        &daemon_bin(),
        home.path(),
        &[
            ("AINB_HANGAR_HOME", home.path().to_str().unwrap()),
            ("HANGAR_DAEMON_RUNTIME_ID", &ids.runtime_id),
            ("HANGAR_CLAUDE_PATH", fake_claude.to_str().unwrap()),
            ("HANGAR_DAEMON_POLL_MS", "200"),
            // Prepend the fakebin dir so the agent's `gh` resolves to the
            // stand-in. PATH is in the runner's env allowlist, so it flows to
            // the agent subprocess.
            ("PATH", &path),
        ],
    );

    let task_id = "task-pr-1";
    enqueue_task(&pool, task_id, &ids).await;

    let row = wait_for_db(&pool, task_id, "done", Duration::from_secs(30)).await;
    drop(session);

    let status: String = row.get("status");
    assert_eq!(status, "done", "task should reach done");

    let captured: Option<String> = pr_url_of(&pool, task_id).await;
    assert_eq!(
        captured.as_deref(),
        Some(FAKE_PR_URL),
        "result.pr_url should hold the gh-printed URL"
    );
}

/// FAILURE MODE: `gh pr create` exits non-zero. The agent script swallows that
/// (`|| true`) and still exits 0, so the task completes — but no PR URL was
/// printed, so `result.pr_url` is NULL (no key), never `""`.
#[tokio::test]
async fn gh_failure_leaves_pr_url_null() {
    if !tripwire_support::tmux_available() {
        eprintln!("tmux not available; skipping e2e tripwire");
        return;
    }

    let home = tempfile::tempdir().expect("tempdir home");
    let db_path = home.path().join("hangar.db");

    let pool = open_pool(&db_path).await;
    ainb_hangar_store::apply_migrations(&pool).await.expect("migrate");
    let ids = seed_world(&pool).await;

    // Fake `gh` that FAILS on `pr create` (exit 3, no URL).
    let fake_bin = fake_gh_pr_create_fails(home.path());
    let fake_claude = fake_claude_runs_gh(home.path(), "trip-pr-2");
    let path = prepend_path(&fake_bin);

    let session = DaemonSession::spawn(
        &daemon_bin(),
        home.path(),
        &[
            ("AINB_HANGAR_HOME", home.path().to_str().unwrap()),
            ("HANGAR_DAEMON_RUNTIME_ID", &ids.runtime_id),
            ("HANGAR_CLAUDE_PATH", fake_claude.to_str().unwrap()),
            ("HANGAR_DAEMON_POLL_MS", "200"),
            ("PATH", &path),
        ],
    );

    let task_id = "task-pr-2";
    enqueue_task(&pool, task_id, &ids).await;

    // The agent still exits 0 (it ignored gh's failure), so the task completes.
    let row = wait_for_db(&pool, task_id, "done", Duration::from_secs(30)).await;
    drop(session);

    let status: String = row.get("status");
    assert_eq!(status, "done", "task completes despite gh's non-zero exit");

    let captured: Option<String> = pr_url_of(&pool, task_id).await;
    assert_eq!(
        captured, None,
        "no PR URL printed → result.pr_url is NULL (no key)"
    );

    // And the column must not be the empty string — explicit NULL/absent check.
    let raw: Option<String> = row.get("result");
    let raw = raw.expect("result JSON populated");
    assert!(
        !raw.contains("pr_url"),
        "result JSON must omit pr_url entirely (no empty-string sentinel), got {raw}"
    );
}

/// Enqueue a `queued` task with a ~now `created_at` (so the queued-TTL sweeper
/// does not eat it before the claim loop sees it).
async fn enqueue_task(pool: &SqlitePool, task_id: &str, ids: &tripwire_support::SeededIds) {
    sqlx::query(
        "INSERT INTO agent_task_queue (id, workspace_id, runtime_id, agent_id, created_at) \
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(task_id)
    .bind(&ids.workspace_id)
    .bind(&ids.runtime_id)
    .bind(&ids.agent_id)
    .bind(tripwire_support::now_ms())
    .execute(pool)
    .await
    .expect("enqueue task");
}

/// Read `result->>'pr_url'` for `task_id` via `SQLite` JSON1 (the same extraction
/// the TUI's badge query uses), so this asserts the on-disk JSON shape, not a
/// Rust-side re-parse.
async fn pr_url_of(pool: &SqlitePool, task_id: &str) -> Option<String> {
    sqlx::query("SELECT result ->> 'pr_url' AS pr_url FROM agent_task_queue WHERE id = ?")
        .bind(task_id)
        .fetch_one(pool)
        .await
        .expect("query pr_url")
        .get::<Option<String>, _>("pr_url")
}

/// Prepend `dir` to the current process `PATH` so the daemon (and thus the agent
/// subprocess) resolves the fake `gh` first while keeping system tools (`sh`,
/// `echo`) reachable.
fn prepend_path(dir: &std::path::Path) -> String {
    let existing = std::env::var("PATH").unwrap_or_default();
    format!("{}:{existing}", dir.display())
}

/// Open a `SQLite` WAL pool at `db_path` (creating the file if absent).
async fn open_pool(db_path: &std::path::Path) -> SqlitePool {
    let opts = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(db_path)
        .create_if_missing(true)
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal);
    SqlitePoolOptions::new().connect_with(opts).await.expect("open pool")
}
