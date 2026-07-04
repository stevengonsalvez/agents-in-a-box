//! a54 tripwire: ONE daemon runs an INTERACTIVE task and a HEADLESS task
//! CONCURRENTLY — the claim loop keeps claiming while a long-lived
//! (interactive) run is in flight, instead of blocking on it.
//!
//! ```text
//!  seed db (ws+user+rt+agent, cap=5)
//!         │  enqueue A = interactive (blocks on a sentinel, stays running)
//!         │  enqueue B = headless    (claimed later, completes fast)
//!         ▼
//!  spawn ONE claim-enabled daemon (isolated $HOME)
//!         │  A claimed first → dispatched→running → REAL tmux session, BLOCKED
//!         │  B claimed on the NEXT poll while A is still running
//!         ▼
//!  (POSITIVE) B reaches `done` WHILE A is still `running`
//!  (NEGATIVE) A is NOT terminal at that instant — the loop did not serialise
//!         │  release the sentinel
//!         ▼
//!  A finishes → session reaped → A `done`
//! ```
//!
//! ## Why this proves the a54 fix
//!
//! Before a54 the claim loop awaited `execute_claimed(...)` INLINE, and an
//! interactive run awaits its tmux session's completion inline inside that — so
//! a single open interactive session wedged EVERY further claim (a headless
//! task behind it would sit `queued` until the human closed the session). A
//! single daemon could never run two tasks at once. This tripwire fails on that
//! old shape: B would never leave `queued` while A blocks, so the `B == done`
//! poll would time out. It passes only when claimed executions run
//! concurrently.
//!
//! The SAME `HANGAR_CLAUDE_PATH` binary backs both tasks:
//! [`fake_claude_tty_branch`] blocks when its stdout is a tmux pty
//! (interactive) and completes when its stdout is a captured pipe (headless).
//!
//! SKIPs cleanly when tmux / the built daemon are absent. Follows the
//! `tmux-ui-tripwire` HARD RULES: exact-name kills only, deadline-bounded
//! polls, POSITIVE + NEGATIVE assertions.

#![allow(clippy::duration_suboptimal_units)]

mod tripwire_support;

use std::path::Path;
use std::time::{Duration, Instant};

use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use tripwire_support::{
    DaemonProcess, fake_claude_tty_branch, now_ms, seed_world, tmux_available, tmux_kill_session,
    tmux_session_live,
};

/// The interactive task id — its tmux session name is `tmux_hangar-<id>`.
const INTERACTIVE_TASK: &str = "task-conc-int";
/// The headless task id — the one that must complete WHILE the interactive
/// runs.
const HEADLESS_TASK: &str = "task-conc-hl";
/// The release sentinel the blocked interactive stub waits on (`$HOME/<name>`).
const SENTINEL: &str = "conc-release";

#[tokio::test]
async fn one_daemon_runs_interactive_and_headless_concurrently() {
    if !tmux_available() {
        eprintln!("SKIP: a54 concurrent-runs tripwire (need tmux + built ainb-hangar-daemon)");
        return;
    }

    // Isolated $HOME; db under `<home>/.agents-in-a-box/hangar.db` (the layout the
    // daemon resolves with AINB_HANGAR_HOME removed — see `DaemonProcess`).
    let home = tempfile::tempdir().expect("tempdir home");
    let hangar = home.path().join(".agents-in-a-box");
    std::fs::create_dir_all(&hangar).expect("create hangar dir");
    let db_path = hangar.join("hangar.db");

    let pool = open_pool(&db_path).await;
    ainb_hangar_store::apply_migrations(&pool).await.expect("migrate");
    let ids = seed_world(&pool).await; // agent cap = 5 (comfortably admits 2)

    // One binary, two behaviours (TTY-branching): interactive blocks, headless
    // completes.
    let fake = fake_claude_tty_branch(home.path(), "conc-sid", SENTINEL);

    let daemon = DaemonProcess::spawn(
        home.path(),
        &[
            ("HANGAR_DAEMON_RUNTIME_ID", &ids.runtime_id),
            ("HANGAR_CLAUDE_PATH", fake.to_str().unwrap()),
            ("HANGAR_DAEMON_POLL_MS", "150"),
            // Keep the sweepers calm so a slow interactive block is never reaped
            // as a stale run mid-test.
            ("HANGAR_SWEEP_INTERVAL_MS", "60000"),
        ],
    );

    // Enqueue A (interactive) with an EARLIER created_at so it is claimed FIRST
    // (ORDER BY priority DESC, created_at, id) and becomes the blocking in-flight
    // run; B (headless) is claimed on a later poll.
    let base = now_ms();
    enqueue(&pool, &ids, INTERACTIVE_TASK, "interactive", base - 1_000).await;
    enqueue(&pool, &ids, HEADLESS_TASK, "headless", base).await;

    let session_name = format!("tmux_hangar-{INTERACTIVE_TASK}");

    // The interactive run must reach `running` AND spin up a live tmux session —
    // the in-flight run the loop must NOT block on.
    let live_deadline = Instant::now() + Duration::from_secs(30);
    let mut saw_live = false;
    while Instant::now() < live_deadline {
        if tmux_session_live(&session_name)
            && status(&pool, INTERACTIVE_TASK).await.as_deref() == Some("running")
        {
            saw_live = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        saw_live,
        "the interactive task must reach `running` with a live tmux session `{session_name}`"
    );

    // POSITIVE (the a54 proof): the HEADLESS task completes to `done` WHILE the
    // interactive one is still blocked/running. On the pre-fix serial loop this
    // never happens — B stays `queued` behind the blocked A and this poll times
    // out.
    let done_deadline = Instant::now() + Duration::from_secs(30);
    let mut headless_done = false;
    while Instant::now() < done_deadline {
        if status(&pool, HEADLESS_TASK).await.as_deref() == Some("done") {
            headless_done = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // Read the interactive task's state at the moment the headless one finished —
    // it must still be in flight (the sentinel is untouched), proving the two ran
    // CONCURRENTLY rather than the headless one waiting for the interactive.
    let interactive_state = status(&pool, INTERACTIVE_TASK).await;

    // Defensive exact-name teardown regardless of outcome (before assertions).
    let release_ok = std::fs::write(home.path().join(SENTINEL), "go").is_ok();

    assert!(
        headless_done,
        "the headless task must reach `done` WHILE the interactive run is in flight \
         (a serial claim loop would leave it `queued`); interactive was {interactive_state:?}"
    );
    // NEGATIVE: the interactive run had NOT terminated when the headless one did —
    // they overlapped in flight.
    assert_eq!(
        interactive_state.as_deref(),
        Some("running"),
        "the interactive run must still be `running` when the headless one completed \
         (proving genuine concurrency, not the headless waiting its turn)"
    );
    assert!(
        release_ok,
        "could not write the interactive release sentinel"
    );

    // The released interactive run now finishes: `done` + its session reaped.
    let fin_deadline = Instant::now() + Duration::from_secs(30);
    let mut interactive_done = false;
    while Instant::now() < fin_deadline {
        if status(&pool, INTERACTIVE_TASK).await.as_deref() == Some("done") {
            interactive_done = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }

    let reap_deadline = Instant::now() + Duration::from_secs(15);
    let mut reaped = false;
    while Instant::now() < reap_deadline {
        if !tmux_session_live(&session_name) {
            reaped = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }

    // Teardown: kill the daemon + defensively the interactive session (exact name).
    drop(daemon);
    tmux_kill_session(&session_name);

    assert!(
        interactive_done,
        "the released interactive run must reach `done`"
    );
    assert!(
        reaped,
        "the interactive tmux session `{session_name}` must be reaped after it finishes"
    );
}

/// Enqueue a `queued` task of `mode` for the seeded agent, NULL issue_id (so
/// the per-(issue,agent) guard is bypassed and only the cap could bound
/// in-flight work), stamped at `created_at`.
async fn enqueue(
    pool: &SqlitePool,
    ids: &tripwire_support::SeededIds,
    task_id: &str,
    mode: &str,
    created_at: i64,
) {
    sqlx::query(
        "INSERT INTO agent_task_queue \
         (id, workspace_id, runtime_id, agent_id, issue_id, mode, status, created_at) \
         VALUES (?, ?, ?, ?, NULL, ?, 'queued', ?)",
    )
    .bind(task_id)
    .bind(&ids.workspace_id)
    .bind(&ids.runtime_id)
    .bind(&ids.agent_id)
    .bind(mode)
    .bind(created_at)
    .execute(pool)
    .await
    .unwrap_or_else(|e| panic!("enqueue {task_id}: {e}"));
}

/// The current `status` of `task_id`, or `None` if the row is absent.
async fn status(pool: &SqlitePool, task_id: &str) -> Option<String> {
    sqlx::query_scalar::<_, String>("SELECT status FROM agent_task_queue WHERE id = ?")
        .bind(task_id)
        .fetch_optional(pool)
        .await
        .expect("query task status")
}

/// Open a WAL sqlite pool at `db_path` (matches the daemon's connection mode).
async fn open_pool(db_path: &Path) -> SqlitePool {
    let opts = SqliteConnectOptions::new()
        .filename(db_path)
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal);
    SqlitePoolOptions::new().connect_with(opts).await.expect("open pool")
}
