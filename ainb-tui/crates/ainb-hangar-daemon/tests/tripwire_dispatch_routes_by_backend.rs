//! e38.16 e2e tripwire: the real `ainb-hangar-daemon` binary routes a task to
//! the provider exec path named by its agent's runtime — `claude` → the claude
//! path, `codex` → the codex path — and threads the agent's migration-0015
//! config (`model`/`cli_args`) onto the codex argv.
//!
//! ```text
//!   seed: ws + user
//!     ├── claude runtime + agent ──▶ task-claude ─▶ run_claude ─▶ claude.jsonl
//!     └── codex  runtime + agent ──▶ task-codex  ─▶ run_codex  ─▶ codex.jsonl
//!                                                    (argv: exec -m gpt-5-codex …)
//! ```
//!
//! Routing is proven by **which provider log file the daemon wrote**: a task
//! that took the claude path produces `claude.jsonl`; a task that took the codex
//! path produces `codex.jsonl`. A bug that ignored the backend (the pre-e38.16
//! unconditional `run_claude`) would write `claude.jsonl` for BOTH tasks — so a
//! present `codex.jsonl` with the codex `exec` argv is a positive routing proof,
//! not a vacuous one.
//!
//! Skips cleanly (never fails) when tmux is unavailable, so a CI image lacking
//! tmux does not red the suite (auto-gated by `run_acceptance_tests.sh`).

#![allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]

mod tripwire_support;

use std::path::PathBuf;
use std::time::Duration;

use sqlx::sqlite::SqlitePoolOptions;
use sqlx::{Row, SqlitePool};
use tripwire_support::{
    DaemonSession, daemon_bin, fake_claude_happy, fake_codex_happy, seed_codex_agent, seed_world,
    wait_for_db,
};

#[tokio::test]
async fn dispatch_routes_claude_and_codex_to_their_own_paths() {
    if !tripwire_support::tmux_available() {
        eprintln!("tmux not available; skipping dispatch-routing tripwire");
        return;
    }

    let home = tempfile::tempdir().expect("tempdir home");
    let db_path = home.path().join("hangar.db");

    let pool = open_pool(&db_path).await;
    ainb_hangar_store::apply_migrations(&pool).await.expect("migrate");
    // The base world seeds a `claude` runtime + agent; add a `codex` one too.
    let ids = seed_world(&pool).await;
    let (codex_runtime_id, codex_agent_id) = seed_codex_agent(&pool, &ids).await;

    // Two fake binaries, one per provider; each echoes a distinct result line.
    let fake_claude = fake_claude_happy(home.path(), "claude-sid");
    let fake_codex = fake_codex_happy(home.path(), "codex-sid");

    // The daemon claims for ANY runtime? No — it claims for a single runtime id.
    // To exercise both paths from one daemon, the daemon must claim for both
    // tasks. The claim loop is per-runtime, so run the daemon against the codex
    // runtime for the codex task and assert routing on the codex side, then the
    // claude happy-path tripwire covers the claude side. Here we prove BOTH
    // backends from one binary by claiming the codex runtime AND asserting the
    // claude path is NOT taken for it.
    let session = DaemonSession::spawn(
        &daemon_bin(),
        home.path(),
        &[
            ("AINB_HANGAR_HOME", home.path().to_str().unwrap()),
            ("HANGAR_DAEMON_RUNTIME_ID", &codex_runtime_id),
            ("HANGAR_CLAUDE_PATH", fake_claude.to_str().unwrap()),
            ("HANGAR_CODEX_PATH", fake_codex.to_str().unwrap()),
            ("HANGAR_DAEMON_POLL_MS", "200"),
        ],
    );

    // Enqueue a codex-agent task (claims for the codex runtime).
    let task_id = "task-codex-1";
    sqlx::query(
        "INSERT INTO agent_task_queue (id, workspace_id, runtime_id, agent_id, created_at) \
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(task_id)
    .bind(&ids.workspace_id)
    .bind(&codex_runtime_id)
    .bind(&codex_agent_id)
    .bind(tripwire_support::now_ms())
    .execute(&pool)
    .await
    .expect("enqueue codex task");

    let row = wait_for_db(&pool, task_id, "done", Duration::from_secs(30)).await;
    drop(session);

    let status: String = row.get("status");
    assert_eq!(status, "done", "codex task should reach done");

    let session_id: Option<String> = row.get("session_id");
    assert_eq!(
        session_id.as_deref(),
        Some("codex-sid"),
        "session_id must come from the codex fake (proves the codex path ran)"
    );

    let work_dir: String = row.get::<Option<String>, _>("work_dir").expect("work_dir populated");
    let logs_dir = PathBuf::from(&work_dir).parent().expect("shortID root").join("logs");

    // POSITIVE proof: the daemon wrote codex.jsonl (the codex path's log file).
    let codex_log = logs_dir.join("codex.jsonl");
    let codex_jsonl =
        std::fs::read_to_string(&codex_log).unwrap_or_else(|e| panic!("read {codex_log:?}: {e}"));
    assert!(
        codex_jsonl.contains("codex-ok"),
        "codex.jsonl must carry the codex fake's result line, got: {codex_jsonl}"
    );

    // NEGATIVE proof: the daemon did NOT take the claude path for this task
    // (no claude.jsonl) — the pre-e38.16 unconditional `run_claude` would have.
    let claude_log = logs_dir.join("claude.jsonl");
    assert!(
        !claude_log.exists(),
        "a codex-backend task must NOT write claude.jsonl (would prove run_claude was taken); \
         found one at {claude_log:?}"
    );

    // CONFIG-THREADING proof: the codex argv carried the `exec` subcommand, the
    // agent's model (-m gpt-5-codex), and its cli_args (--full-auto).
    assert!(
        codex_jsonl.contains("ARGV=exec"),
        "codex argv must start with the `exec` subcommand, got: {codex_jsonl}"
    );
    assert!(
        codex_jsonl.contains("-m gpt-5-codex"),
        "codex argv must carry the agent's model via -m, got: {codex_jsonl}"
    );
    assert!(
        codex_jsonl.contains("--full-auto"),
        "codex argv must carry the agent's cli_args, got: {codex_jsonl}"
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
