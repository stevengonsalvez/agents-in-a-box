//! a54 tripwire: a graceful daemon shutdown REAPS its in-flight interactive
//! tmux sessions instead of orphaning them.
//!
//! ```text
//!  enqueue interactive task ─▶ daemon claims ─▶ REAL tmux session, BLOCKED
//!         │  ── tripwire asserts the session is LIVE ──
//!         ▼
//!  SIGINT the daemon (graceful Ctrl-C, NOT SIGKILL)
//!         │  run loop's shutdown arm kills each live interactive session by name
//!         ▼
//!  (POSITIVE) the tmux session is GONE within the deadline (no orphan pane)
//! ```
//!
//! ## Why this matters (a54)
//!
//! An interactive run is a DETACHED tmux session — it survives the daemon
//! exiting, and (now that runs are spawned onto a JoinSet) aborting the
//! in-flight `wait` future does NOT kill it. So without an explicit shutdown
//! reap, every open interactive session would be orphaned on daemon shutdown.
//! The fix tracks live session names and kills each by EXACT name on `Ctrl-C`.
//! This tripwire drives a real SIGINT and asserts the session is reaped — the
//! sentinel is never touched, so the ONLY way the session dies within the
//! window is the shutdown reap (the stub self-exits only after ~60s, far past
//! the deadline).
//!
//! SKIPs cleanly when tmux / the built daemon are absent. Exact-name kills
//! only.

#![allow(clippy::duration_suboptimal_units)]

mod tripwire_support;

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use tripwire_support::{
    DaemonProcess, fake_claude_tty_branch, now_ms, seed_world, tmux_available, tmux_kill_session,
    tmux_session_live,
};

/// The interactive task id — its tmux session is `tmux_hangar-<id>`.
const TASK_ID: &str = "task-shutdown-int";
/// A sentinel that is NEVER written here — the session must die via the reap.
const SENTINEL: &str = "never-released";

#[tokio::test]
async fn graceful_shutdown_reaps_in_flight_interactive_session() {
    if !tmux_available() {
        eprintln!("SKIP: a54 shutdown-reap tripwire (need tmux + built ainb-hangar-daemon)");
        return;
    }

    let home = tempfile::tempdir().expect("tempdir home");
    let hangar = home.path().join(".agents-in-a-box");
    std::fs::create_dir_all(&hangar).expect("create hangar dir");
    let db_path = hangar.join("hangar.db");

    let pool = open_pool(&db_path).await;
    ainb_hangar_store::apply_migrations(&pool).await.expect("migrate");
    let ids = seed_world(&pool).await;

    let fake = fake_claude_tty_branch(home.path(), "shutdown-sid", SENTINEL);
    let daemon = DaemonProcess::spawn(
        home.path(),
        &[
            ("HANGAR_DAEMON_RUNTIME_ID", &ids.runtime_id),
            ("HANGAR_CLAUDE_PATH", fake.to_str().unwrap()),
            ("HANGAR_DAEMON_POLL_MS", "150"),
            ("HANGAR_SWEEP_INTERVAL_MS", "60000"),
        ],
    );

    sqlx::query(
        "INSERT INTO agent_task_queue (id, workspace_id, runtime_id, agent_id, mode, created_at) \
         VALUES (?, ?, ?, ?, 'interactive', ?)",
    )
    .bind(TASK_ID)
    .bind(&ids.workspace_id)
    .bind(&ids.runtime_id)
    .bind(&ids.agent_id)
    .bind(now_ms())
    .execute(&pool)
    .await
    .expect("enqueue interactive task");

    // Wait for the interactive session to be LIVE (the in-flight run to reap).
    let session_name = format!("tmux_hangar-{TASK_ID}");
    let live_deadline = Instant::now() + Duration::from_secs(30);
    let mut saw_live = false;
    while Instant::now() < live_deadline {
        if tmux_session_live(&session_name) {
            saw_live = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        saw_live,
        "the interactive runner must spawn a live tmux session `{session_name}`"
    );

    // Settle so the runner has recorded the session in the shutdown-reap set
    // (registration happens microseconds after the session goes live; this margin
    // closes the theoretical spawn→register gap deterministically).
    tokio::time::sleep(Duration::from_millis(600)).await;

    // Graceful shutdown: SIGINT (NOT the SIGKILL that Drop uses), so the daemon's
    // Ctrl-C reap path runs.
    let pid = daemon.pid();
    let sent = Command::new("kill")
        .args(["-INT", &pid.to_string()])
        .status()
        .expect("send SIGINT to daemon");
    assert!(sent.success(), "kill -INT {pid} failed");

    // POSITIVE: the session is reaped by the shutdown path (the sentinel is never
    // written, so a still-live session past this deadline is an ORPHAN — the a54
    // bug this guards).
    let reap_deadline = Instant::now() + Duration::from_secs(15);
    let mut reaped = false;
    while Instant::now() < reap_deadline {
        if !tmux_session_live(&session_name) {
            reaped = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }

    // Teardown (exact-name) before the assertion so a failure never leaks a pane.
    drop(daemon);
    tmux_kill_session(&session_name);

    assert!(
        reaped,
        "graceful shutdown must reap the in-flight interactive session `{session_name}`, \
         not orphan it"
    );
}

/// Open a WAL sqlite pool at `db_path` (matches the daemon's connection mode).
async fn open_pool(db_path: &Path) -> SqlitePool {
    let opts = SqliteConnectOptions::new()
        .filename(db_path)
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal);
    SqlitePoolOptions::new().connect_with(opts).await.expect("open pool")
}
