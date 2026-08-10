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
/// The SIGTERM case's task id. Distinct so the two cases never share a tmux
/// session name when cargo runs them in parallel.
const TASK_TERM: &str = "task-shutdown-term";
/// A sentinel that is NEVER written here — the session must die via the reap.
const SENTINEL: &str = "never-released";

/// SIGINT is the foreground "tear it all down": the attached pane goes with it.
#[tokio::test]
async fn sigint_reaps_in_flight_interactive_session() {
    drive_shutdown("-INT", TASK_ID, "shutdown-sid", Expect::Reaped).await;
}

/// SIGTERM is `hangar daemon stop` / `restart`, which runs after every upgrade.
/// It must stop the daemon WITHOUT killing the operator's attached agent panes —
/// the next boot's tmux reconciler re-adopts them with exact pane identity.
///
/// Before the SIGTERM handler existed this test could not have been written: the
/// process died on the OS default disposition, so nothing about the shutdown was
/// under the daemon's control at all.
#[tokio::test]
async fn sigterm_stops_the_daemon_but_leaves_the_attached_session_running() {
    drive_shutdown("-TERM", TASK_TERM, "shutdown-term-sid", Expect::Survives).await;
}

/// What the interactive session should look like after the signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expect {
    /// The shutdown path killed it by exact name.
    Reaped,
    /// It outlived the daemon, still attachable.
    Survives,
}

async fn drive_shutdown(signal: &str, task_id: &str, sid: &str, expect: Expect) {
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

    let fake = fake_claude_tty_branch(home.path(), sid, SENTINEL);
    let mut daemon = DaemonProcess::spawn(
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
    .bind(task_id)
    .bind(&ids.workspace_id)
    .bind(&ids.runtime_id)
    .bind(&ids.agent_id)
    .bind(now_ms())
    .execute(&pool)
    .await
    .expect("enqueue interactive task");

    // Wait for the interactive session to be LIVE (the in-flight run to reap).
    let session_name = format!("tmux_hangar-{task_id}");
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

    // Graceful shutdown by EXACT pid (never the SIGKILL that Drop uses), so the
    // daemon's own shutdown path runs.
    let pid = daemon.pid();
    let sent = Command::new("kill")
        .args([signal, &pid.to_string()])
        .status()
        .expect("signal daemon");
    assert!(sent.success(), "kill {signal} {pid} failed");

    // The sentinel is never written, so the stub only self-exits after ~60s —
    // far past this deadline. Anything that changes within it is the daemon's
    // shutdown path, not the run finishing.
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut reaped = false;
    while Instant::now() < deadline {
        if !tmux_session_live(&session_name) {
            reaped = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
    // The daemon itself must be gone either way — a handler that hangs is as
    // broken as one that kills the wrong thing. `wait_for_exit` reaps, because
    // this test is the daemon's parent and a zombie answers `kill(pid, 0)`
    // exactly like a live process.
    let daemon_stopped = daemon.wait_for_exit(Duration::from_secs(15));
    let still_live = tmux_session_live(&session_name);

    // Teardown (exact-name) before the assertions so a failure never leaks a pane.
    drop(daemon);
    tmux_kill_session(&session_name);

    assert!(daemon_stopped, "the daemon must exit on {signal}");
    match expect {
        Expect::Reaped => assert!(
            reaped,
            "SIGINT must reap the in-flight interactive session `{session_name}`, \
             not orphan it"
        ),
        Expect::Survives => assert!(
            still_live,
            "SIGTERM must leave the operator's attached session `{session_name}` \
             running — `daemon restart` happens on every upgrade"
        ),
    }
}

/// Open a WAL sqlite pool at `db_path` (matches the daemon's connection mode).
async fn open_pool(db_path: &Path) -> SqlitePool {
    let opts = SqliteConnectOptions::new()
        .filename(db_path)
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal);
    SqlitePoolOptions::new().connect_with(opts).await.expect("open pool")
}
