//! P1.7 e2e tripwire: a real `ainb-hangar-daemon` binary, spawned in a real
//! tmux session, claims and executes a fake-`claude` task end-to-end against a
//! real `SQLite` WAL database — no mocks.
//!
//! ```text
//!  seed DB (ws+user+rt+agent)        spawn daemon in tmux
//!         │                                  │
//!         ▼                                  ▼
//!  INSERT queued task ───────▶ claim ─▶ dispatched ─▶ running ─▶ done
//!                                              │
//!                                       fake-claude.sh
//!                                       (emits session_id + result)
//! ```
//!
//! The daemon is the genuine release binary (`assert_cmd`'s `cargo_bin`),
//! configured entirely through env vars (`HANGAR_DAEMON_RUNTIME_ID`,
//! `HANGAR_CLAUDE_PATH`, `HANGAR_DAEMON_POLL_MS`). We poll the database for the
//! terminal state with a 30s budget. The tmux session is killed by exact name on
//! drop (per the `tmux_protection` global rule — never a bulk/wildcard kill).

#![allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]

mod tripwire_support;

use std::time::Duration;

use sqlx::sqlite::SqlitePoolOptions;
use sqlx::{Row, SqlitePool};
use tripwire_support::{
    DaemonSession, daemon_bin, fake_claude_happy, fake_claude_with_usage, seed_world, wait_for_db,
};

#[tokio::test]
async fn happy_path_claude_provider_walks_fsm_to_done() {
    // Skip cleanly when tmux is unavailable (CI image lacking tmux), so the
    // tripwire never produces a false failure on a host without the tool.
    if !tripwire_support::tmux_available() {
        eprintln!("tmux not available; skipping e2e tripwire");
        return;
    }

    let home = tempfile::tempdir().expect("tempdir home");
    let db_path = home.path().join("hangar.db");

    // 1. Bring the DB up to schema and seed the minimal world.
    let pool = open_pool(&db_path).await;
    ainb_hangar_store::apply_migrations(&pool).await.expect("migrate");
    let ids = seed_world(&pool).await;

    // 2. A fake claude that pins session_id="trip-1" then a result line.
    let fake_claude = fake_claude_happy(home.path(), "trip-1");

    // 3. Spawn the real daemon binary in a uniquely-named tmux session.
    let session = DaemonSession::spawn(
        &daemon_bin(),
        home.path(),
        &[
            ("AINB_HANGAR_HOME", home.path().to_str().unwrap()),
            ("HANGAR_DAEMON_RUNTIME_ID", &ids.runtime_id),
            ("HANGAR_CLAUDE_PATH", fake_claude.to_str().unwrap()),
            ("HANGAR_DAEMON_POLL_MS", "200"),
        ],
    );

    // 4. Enqueue a queued task via direct SQL (the daemon is the only claimer).
    //    `created_at` must be ~now: the daemon runs the real SystemClock, and a
    //    1970-epoch timestamp would be instantly swept by the 2h queued-TTL
    //    sweeper before the claim loop ever sees it.
    let task_id = "task-trip-1";
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

    // 5. Poll for the terminal state (30s budget).
    let row = wait_for_db(&pool, task_id, "done", Duration::from_secs(30)).await;
    drop(session); // kill the tmux session by exact name before assertions

    let status: String = row.get("status");
    assert_eq!(status, "done", "task should reach done");

    let result: Option<String> = row.get("result");
    let result = result.expect("result JSON populated");
    assert!(
        result.contains("ok"),
        "result should contain provider output 'ok', got {result}"
    );

    let session_id: Option<String> = row.get("session_id");
    assert_eq!(
        session_id.as_deref(),
        Some("trip-1"),
        "session_id pinned from system line"
    );

    let work_dir: Option<String> = row.get("work_dir");
    let work_dir = work_dir.expect("work_dir populated");
    let expected_root = home
        .path()
        .join(".agents-in-a-box")
        .join("hangar")
        .join("workspaces")
        .join(&ids.workspace_slug);
    assert!(
        work_dir.contains(expected_root.to_str().unwrap()),
        "work_dir {work_dir} should be under {expected_root:?}"
    );

    // 6. claude.jsonl in the task's logs/ has the 2 provider lines.
    // work_dir is `<root>/<shortID>/workdir`; the log is its sibling logs/.
    let workdir = std::path::PathBuf::from(&work_dir);
    let logs = workdir.parent().expect("shortID root").join("logs").join("claude.jsonl");
    let jsonl = std::fs::read_to_string(&logs).unwrap_or_else(|e| panic!("read {logs:?}: {e}"));
    let lines = jsonl.lines().filter(|l| !l.trim().is_empty()).count();
    assert_eq!(
        lines, 2,
        "claude.jsonl should have 2 lines, got {lines}: {jsonl}"
    );
}

/// e38.35: a real daemon run whose fake-claude reports token/cost usage on its
/// `result` line lands a `task_usage` row at the finalize seam — the runner→store
/// persist wiring the usage dashboard rolls up. End-to-end through the genuine
/// daemon binary (no mocks): seed → enqueue → claim → run → done → usage persisted.
#[tokio::test]
async fn finalized_task_persists_provider_usage_row() {
    if !tripwire_support::tmux_available() {
        eprintln!("tmux not available; skipping e2e tripwire");
        return;
    }

    let home = tempfile::tempdir().expect("tempdir home");
    let db_path = home.path().join("hangar.db");
    let pool = open_pool(&db_path).await;
    ainb_hangar_store::apply_migrations(&pool).await.expect("migrate");
    let ids = seed_world(&pool).await;

    // A fake claude that pins a session id then reports usage on its result line.
    let fake_claude = fake_claude_with_usage(home.path(), "usage-1", 1200, 340, 0.0231);

    let session = DaemonSession::spawn(
        &daemon_bin(),
        home.path(),
        &[
            ("AINB_HANGAR_HOME", home.path().to_str().unwrap()),
            ("HANGAR_DAEMON_RUNTIME_ID", &ids.runtime_id),
            ("HANGAR_CLAUDE_PATH", fake_claude.to_str().unwrap()),
            ("HANGAR_DAEMON_POLL_MS", "200"),
        ],
    );

    let task_id = "task-usage-1";
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

    let _ = wait_for_db(&pool, task_id, "done", Duration::from_secs(30)).await;
    drop(session); // kill the tmux session by exact name before assertions

    // The finalize seam recorded this run's usage into task_usage.
    let row = sqlx::query(
        "SELECT workspace_id, agent_id, input_tokens, output_tokens, cost_usd \
         FROM task_usage WHERE task_id = ?",
    )
    .bind(task_id)
    .fetch_one(&pool)
    .await
    .expect("a task_usage row was persisted at finalize");

    assert_eq!(row.get::<String, _>("workspace_id"), ids.workspace_id);
    assert_eq!(row.get::<String, _>("agent_id"), ids.agent_id);
    assert_eq!(row.get::<i64, _>("input_tokens"), 1200);
    assert_eq!(row.get::<i64, _>("output_tokens"), 340);
    assert!((row.get::<f64, _>("cost_usd") - 0.0231).abs() < 1e-9);
}

/// P10 / D19: a real daemon run also appends a durable `run_history` row at the
/// finalize seam — the observability timeline the History view reads. End-to-end
/// through the genuine daemon binary (no mocks): seed → enqueue → claim → run →
/// done → history appended, carrying provider / outcome / session / token-cost.
#[tokio::test]
async fn finalized_task_appends_run_history_row() {
    if !tripwire_support::tmux_available() {
        eprintln!("tmux not available; skipping e2e tripwire");
        return;
    }

    let home = tempfile::tempdir().expect("tempdir home");
    let db_path = home.path().join("hangar.db");
    let pool = open_pool(&db_path).await;
    ainb_hangar_store::apply_migrations(&pool).await.expect("migrate");
    let ids = seed_world(&pool).await;

    // A fake claude that pins a session id then reports usage on its result line.
    let fake_claude = fake_claude_with_usage(home.path(), "history-sess-1", 900, 210, 0.0177);

    let session = DaemonSession::spawn(
        &daemon_bin(),
        home.path(),
        &[
            ("AINB_HANGAR_HOME", home.path().to_str().unwrap()),
            ("HANGAR_DAEMON_RUNTIME_ID", &ids.runtime_id),
            ("HANGAR_CLAUDE_PATH", fake_claude.to_str().unwrap()),
            ("HANGAR_DAEMON_POLL_MS", "200"),
        ],
    );

    let task_id = "task-history-1";
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

    let _ = wait_for_db(&pool, task_id, "done", Duration::from_secs(30)).await;
    drop(session); // kill the tmux session by exact name before assertions

    // The finalize seam appended a run_history row for this run.
    let row = sqlx::query(
        "SELECT workspace_id, provider, outcome, session_id, input_tokens, output_tokens, \
                cost_usd, started_at, finished_at \
         FROM run_history WHERE task_id = ?",
    )
    .bind(task_id)
    .fetch_one(&pool)
    .await
    .expect("a run_history row was appended at finalize");

    assert_eq!(row.get::<String, _>("workspace_id"), ids.workspace_id);
    assert_eq!(row.get::<String, _>("provider"), "claude");
    assert_eq!(row.get::<String, _>("outcome"), "success");
    assert_eq!(row.get::<String, _>("session_id"), "history-sess-1");
    assert_eq!(row.get::<i64, _>("input_tokens"), 900);
    assert_eq!(row.get::<i64, _>("output_tokens"), 210);
    assert!((row.get::<f64, _>("cost_usd") - 0.0177).abs() < 1e-9);
    // The run recorded a start + finish, so a positive duration is derivable.
    let started = row.get::<Option<i64>, _>("started_at").expect("started_at set");
    let finished = row.get::<i64, _>("finished_at");
    assert!(finished >= started, "finished_at must not precede started_at");
}

/// Open a `SQLite` WAL pool at `db_path` (creating the file if absent).
async fn open_pool(db_path: &std::path::Path) -> SqlitePool {
    let opts = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(db_path)
        .create_if_missing(true)
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal);
    SqlitePoolOptions::new().connect_with(opts).await.expect("open pool")
}
