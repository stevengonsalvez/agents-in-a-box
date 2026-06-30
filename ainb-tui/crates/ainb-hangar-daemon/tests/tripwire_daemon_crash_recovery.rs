//! e38.25 tripwire: kill -9 the running daemon mid-task and restart it against
//! the SAME `hangar.db`.
//!
//! ```text
//!  seed db ──▶ claim-enabled daemon (PID A) ──claims──▶ task RUNNING
//!                       │ kill -9 PID A (unclean crash, started_at stamped)
//!                       ▼
//!  task stuck `running` in hangar.db (WAL left mid-write)
//!                       │ respawn daemon (PID B) against the SAME $HOME db
//!                       ▼
//!  (a) WAL db opens cleanly (a query succeeds) AND
//!  (b) the orphaned row reaches a recovered/terminal state within budget
//!      (reclaimed→queued / re-dispatched / swept→failed), NOT still `running`.
//! ```
//!
//! ## Why this is a distinct gap from the TTL sweeper tripwires
//!
//! `tripwire_ttl_sweeper_fails_stale_dispatched` proves a *stale dispatch*
//! (`started_at IS NULL`) is reclaimed/failed. It never kills the daemon and
//! never exercises a `running` row whose runtime crashed *after* `started_at`
//! was stamped. A crashed `running` row is the real-world daemon-restart case:
//! the agent's provider was launched, the daemon died with `kill -9` before it
//! could finalise the row, and the WAL was left mid-write. On restart that row
//! must not stay `running` forever (the default running TTL is 2.5h — no bounded
//! test recovery), it must be recovered promptly so the work is re-dispatched.
//!
//! ## Harness reuse + HARD RULES
//!
//! Gating (`can_run_tripwire`/`skip`) and the `daemon_bin` locator are reused
//! from `tripwire_p4_common` (the P4.9 shared seed/spawn harness). The
//! crash-specific lifecycle — spawn-as-`Child` so the EXACT pid is captured,
//! `kill -9` that exact pid via `nix` (never `pkill`/`killall`/by-name), respawn
//! against the same db — lives here. SKIP-not-fail when tmux/binaries are
//! missing; `--test-threads=1` (the runner already enforces it) so the two
//! daemon processes never race a sibling tripwire's tempdir.

#![allow(clippy::duration_suboptimal_units)] // `from_secs(N)` reads fine as a poll budget.

// Reuse the P4.9 harness for gating + the daemon-binary locator. `#[path]`
// (not `mod`) so Cargo does not compile the helper file as its own test binary.
#[path = "tripwire_p4_common.rs"]
mod common;

use std::path::Path;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use common::{can_run_tripwire, daemon_bin, skip};
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;

/// The seeded runtime the daemon claims for (`seed_p4_fixture`).
const RUNTIME_ID: &str = "runtime-1";
/// The enqueued task whose row we crash mid-`running` and recover.
const TASK_ID: &str = "crash-recovery-task";

#[test]
fn daemon_kill9_mid_task_recovers_orphan_row_against_same_db() {
    if !can_run_tripwire() {
        skip("daemon crash-recovery tripwire");
        return;
    }

    // Isolated $HOME → the daemon resolves `$HOME/.agents-in-a-box/hangar.db` (no
    // AINB_HANGAR_HOME), the same db across both spawns.
    let home = tempfile::tempdir().expect("isolated HOME tempdir");
    let hangar_dir = home.path().join(".agents-in-a-box");
    std::fs::create_dir_all(&hangar_dir).expect("create ~/.agents-in-a-box");

    // Seed the P4 fixture, free the fixture's running slot, and enqueue ONE
    // queued task the claim-enabled daemon will pick up and START (→ running).
    seed_and_enqueue(&hangar_dir);

    // A fake `claude` that BLOCKS for far longer than the test window, so the
    // claimed task stays `running` (started_at stamped) while we capture the
    // daemon pid and kill it. It echoes a system line first so any session-id
    // parsing has something to read, then sleeps — it never exits within budget.
    let fake_claude = write_blocking_fake_claude(home.path());

    // --- spawn #1: claim-enabled daemon, captured EXACT pid ---------------
    let mut daemon_a = spawn_daemon(home.path(), &fake_claude);
    let pid_a = daemon_a.id();
    assert!(pid_a > 0, "spawned daemon must have a real pid");

    // Wait for the enqueued task to reach `running` (started_at stamped) — that
    // is the mid-task state we crash from.
    let reached_running = poll_status(
        &hangar_dir,
        TASK_ID,
        Instant::now() + Duration::from_secs(30),
        |status, _| status == "running",
    );
    assert_eq!(
        reached_running.as_deref(),
        Some("running"),
        "task should reach `running` before the crash (claim+start drove it there)"
    );

    // --- kill -9 the EXACT captured pid (never pkill/killall/by-name) -------
    kill(
        Pid::from_raw(i32::try_from(pid_a).expect("pid fits i32")),
        Signal::SIGKILL,
    )
    .expect("kill -9 the captured daemon pid");
    // Reap the zombie so the OS releases it (we own this child handle).
    let _ = daemon_a.wait();

    // The unclean kill leaves the row `running` and the WAL mid-write.
    let after_kill = read_status(&hangar_dir, TASK_ID);
    assert_eq!(
        after_kill.as_deref(),
        Some("running"),
        "immediately post-crash the row is still `running` (the orphan limbo)"
    );

    // --- (a) WAL db opens cleanly post-unclean-kill: a query succeeds -------
    // A fresh pool open + scalar query against the same file proves the WAL was
    // not corrupted by the mid-write kill (sqlite recovers the WAL on open).
    let total = count_rows(&hangar_dir);
    assert!(
        total >= 1,
        "db opens cleanly post-kill and a query returns rows (got {total})"
    );

    // --- respawn #2 against the SAME db ------------------------------------
    let mut daemon_b = spawn_daemon(home.path(), &fake_claude);

    // (b) the orphaned row reaches a recovered/terminal state within budget:
    // reclaimed→queued, re-dispatched, swept→done, or swept→failed — anything
    // BUT still stuck in the original `running` limbo.
    let recovered = poll_status(
        &hangar_dir,
        TASK_ID,
        Instant::now() + Duration::from_secs(60 * common::budget_scale()),
        |status, _| status != "running",
    );

    // Tear down spawn #2 before asserting (kill the exact captured pid).
    let pid_b = daemon_b.id();
    let _ = kill(
        Pid::from_raw(i32::try_from(pid_b).expect("pid fits i32")),
        Signal::SIGKILL,
    );
    let _ = daemon_b.wait();

    let recovered = recovered.unwrap_or_else(|| {
        panic!(
            "orphaned `running` row never left its limbo after daemon restart \
             (no bounded crash-recovery): last status = {:?}",
            read_status(&hangar_dir, TASK_ID)
        )
    });

    // POSITIVE: the row moved to a recovered/terminal state.
    assert!(
        matches!(
            recovered.as_str(),
            "queued" | "dispatched" | "done" | "failed" | "cancelled"
        ),
        "recovered row should be in a queued/dispatched/terminal state, got {recovered:?}"
    );
    // NEGATIVE (paired): it is NOT still stuck `running`.
    assert_ne!(
        recovered, "running",
        "post-restart the orphaned row must not still be stuck `running`"
    );
}

/// Seed the P4 fixture into `{hangar_dir}/hangar.db`, free the fixture's running
/// slot (`task-1` → done) and raise `agent-1`'s concurrency, then enqueue one
/// fresh `queued` task ([`TASK_ID`]) the claim-enabled daemon will start.
fn seed_and_enqueue(hangar_dir: &Path) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("seed runtime");
    rt.block_on(async {
        let store = ainb_hangar_store::Store::open_in(hangar_dir).await.expect("open seed store");
        let pool = store.pool();
        ainb_hangar_daemon::seed::seed_p4_fixture(pool).await.expect("seed P4 fixture");

        // The fixture seeds `task-1` `running`, consuming `agent-1`'s single
        // slot. Free it (→ done) and raise the cap so the claim loop has a slot
        // for the fresh task.
        sqlx::query(
            "UPDATE agent_task_queue SET status='done', finished_at=created_at WHERE id='task-1'",
        )
        .execute(pool)
        .await
        .expect("free fixture running slot");
        sqlx::query("UPDATE agent SET max_concurrent_tasks = 5 WHERE id = 'agent-1'")
            .execute(pool)
            .await
            .expect("raise agent concurrency");

        // Enqueue one queued task on the seeded runtime/agent/workspace. `now`
        // is wall-clock so the queued-TTL sweeper never reaps it; no `issue_id`
        // so the partial-unique pending index can't collide.
        let now_ms = i64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_millis()),
        )
        .unwrap_or(i64::MAX);
        sqlx::query(
            "INSERT INTO agent_task_queue \
             (id, workspace_id, runtime_id, agent_id, issue_id, status, created_at) \
             VALUES (?, ?, ?, ?, NULL, 'queued', ?)",
        )
        .bind(TASK_ID)
        .bind(ainb_hangar_daemon::seed::WS_ID)
        .bind(RUNTIME_ID)
        .bind("agent-1")
        .bind(now_ms)
        .execute(pool)
        .await
        .expect("enqueue crash-recovery task");
    });
}

/// Spawn the daemon as a `Child` (NOT via tmux), so `child.id()` is the EXACT
/// pid we kill. Claim-enabled (binds to [`RUNTIME_ID`]) with a blocking fake
/// `claude`, fast poll, and a tight sweep interval so any restart-time recovery
/// fires within the poll budget. Waits for the socket to appear so the daemon is
/// fully up before we drive it.
fn spawn_daemon(home: &Path, fake_claude: &Path) -> Child {
    let bin = daemon_bin().expect("gated by can_run_tripwire");
    let child = Command::new(bin)
        .env("HOME", home)
        .env_remove("AINB_HANGAR_HOME")
        .env("HANGAR_DAEMON_RUNTIME_ID", RUNTIME_ID)
        .env("HANGAR_CLAUDE_PATH", fake_claude)
        .env("HANGAR_DAEMON_POLL_MS", "200")
        .env("HANGAR_SWEEP_INTERVAL_MS", "300")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn ainb-hangar-daemon");

    // Wait for the RPC socket so the daemon has booted past migrations + bind.
    let socket = home.join(".agents-in-a-box").join("hangar.sock");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !socket.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    child
}

/// Write an executable fake `claude` that echoes a system line then blocks
/// (sleep) well past the test window, so a claimed task stays `running`.
fn write_blocking_fake_claude(dir: &Path) -> std::path::PathBuf {
    let path = dir.join("fake-claude-block.sh");
    let body = "#!/bin/sh\n\
         echo '{\"type\":\"system\",\"session_id\":\"crash-block\"}'\n\
         sleep 120\n";
    std::fs::write(&path, body).expect("write blocking fake-claude");
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&path).expect("stat script").permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).expect("chmod script");
    }
    path
}

/// Read the current `status` of `task_id` from `{hangar_dir}/hangar.db`, or
/// `None` if absent. Opens a fresh pool each call — this is also the (a) clean
/// re-open proof.
fn read_status(hangar_dir: &Path, task_id: &str) -> Option<String> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("status runtime");
    rt.block_on(async {
        let store = ainb_hangar_store::Store::open_in(hangar_dir).await.expect("open status store");
        sqlx::query_scalar::<_, String>("SELECT status FROM agent_task_queue WHERE id = ?")
            .bind(task_id)
            .fetch_optional(store.pool())
            .await
            .expect("query task status")
    })
}

/// Count all task rows — a trivial query that proves the WAL db opens cleanly
/// after the unclean kill.
fn count_rows(hangar_dir: &Path) -> i64 {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("count runtime");
    rt.block_on(async {
        let store = ainb_hangar_store::Store::open_in(hangar_dir).await.expect("open count store");
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM agent_task_queue")
            .fetch_one(store.pool())
            .await
            .expect("count task rows")
    })
}

/// Poll the task `status` until `pred(status, attempt)` holds or `deadline`
/// passes. Returns the matching status, or `None` on timeout. No bare sleep
/// before the first read; a 150ms inter-poll gap, deadline-bounded.
fn poll_status(
    hangar_dir: &Path,
    task_id: &str,
    deadline: Instant,
    pred: impl Fn(&str, i64) -> bool,
) -> Option<String> {
    let mut attempt = 0;
    loop {
        if let Some(status) = read_status(hangar_dir, task_id) {
            if pred(&status, attempt) {
                return Some(status);
            }
        }
        if Instant::now() >= deadline {
            return None;
        }
        attempt += 1;
        std::thread::sleep(Duration::from_millis(150));
    }
}
