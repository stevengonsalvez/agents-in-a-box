//! e38.28 tripwire: the retry chain (F06) + timeout enforcement (F43) driven
//! end-to-end against a REAL claim-enabled daemon, not just the store layer.
//!
//! ```text
//!  seed db ──▶ claim-enabled daemon (HANGAR_CLAUDE_PATH = configurable fake)
//!                 │ claim queued task ──▶ running ──▶ provider exec
//!                 ▼
//!   ┌─ RETRY ─────────────────────────────────────────────────────────┐
//!   │ fake exits 75 (EX_TEMPFAIL = infra/retryable) on attempt 1, then  │
//!   │ exits 0 on attempt 2. The original row → failed (retryable        │
//!   │ reason), a CHILD row carrying parent_task_id is spawned, claimed, │
//!   │ and ultimately reaches `done`.                                    │
//!   ├─ NO-CHILD-ON-AGENT-ERROR ────────────────────────────────────────┤
//!   │ fake exits 1 (plain agent_error, non-retryable). The row → failed │
//!   │ and NO child with parent_task_id is ever created.                 │
//!   ├─ TIMEOUT ────────────────────────────────────────────────────────┤
//!   │ fake sleeps past the configured HANGAR provider deadline. The     │
//!   │ daemon kills it and the row → failed with reason = `timeout`,     │
//!   │ within a bounded budget.                                          │
//!   └──────────────────────────────────────────────────────────────────┘
//! ```
//!
//! ## Why this is a distinct gap from `retry_chain.rs`
//!
//! `crates/ainb-hangar-store/tests/retry_chain.rs` proves
//! [`RetryService::maybe_retry_failed`](ainb_hangar_store::service::retry) spawns
//! a `parent_task_id`-chained child *given an already-failed parent Task in hand*.
//! It never spawns a daemon, never runs a provider, and never proves the daemon's
//! claim loop actually CALLS the retry service after a real provider failure —
//! nor that a real non-zero/timeout provider exit is classified into the right
//! [`FailureReason`]. This tripwire drives the whole loop: a configurable fake
//! `claude`, a claim-enabled daemon, and assertions on the resulting rows.
//!
//! ## Harness reuse + HARD RULES
//!
//! The gating (`can_run_tripwire`) and `daemon_bin` locator are reused from
//! `tripwire_p4_common` (the P4.9 shared harness). The daemon-spawn-as-`Child`
//! (so the EXACT pid is captured + killed via `nix`, never `pkill`/`killall`),
//! the configurable fake-`claude` writer, and the seed/enqueue helpers mirror
//! `tripwire_daemon_crash_recovery`. SKIP-not-fail when tmux/binaries are
//! missing; `--test-threads=1` (the runner enforces it) so the daemon processes
//! never race a sibling tripwire's tempdir.

#![allow(clippy::duration_suboptimal_units)] // `from_secs(N)` reads fine as a poll budget.

// Reuse the P4.9 harness for gating + the daemon-binary locator. `#[path]`
// (not `mod`) so Cargo does not compile the helper file as its own test binary.
#[path = "tripwire_p4_common.rs"]
mod common;

use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use common::{can_run_tripwire, daemon_bin, skip};
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;

/// The seeded runtime the daemon claims for (`seed_p4_fixture`).
const RUNTIME_ID: &str = "runtime-1";

/// A claim-enabled daemon child + the isolated `$HOME` it serves. Kills the
/// daemon by its EXACT captured pid on drop (never a wildcard / by-name kill).
struct Daemon {
    home: tempfile::TempDir,
    child: Child,
}

impl Daemon {
    fn hangar_dir(&self) -> PathBuf {
        self.home.path().join(".agents-in-a-box")
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        // Kill only this exact daemon child by its captured pid.
        let pid = self.child.id();
        let _ = kill(
            Pid::from_raw(i32::try_from(pid).unwrap_or(0)),
            Signal::SIGKILL,
        );
        let _ = self.child.wait();
    }
}

// ---------------------------------------------------------------------------
// RETRY (POSITIVE): a retryable provider failure spawns a child that succeeds.
// ---------------------------------------------------------------------------

#[test]
fn retryable_failure_spawns_child_with_parent_task_id_that_succeeds() {
    if !can_run_tripwire() {
        skip("retry/timeout e2e tripwire (retry leg)");
        return;
    }

    let daemon = spawn_seeded_daemon(
        // Attempt 1 exits 75 (EX_TEMPFAIL → infra/retryable); attempt 2 exits 0.
        &write_retry_fake_claude,
        &[],
    );
    let hangar_dir = daemon.hangar_dir();
    let task_id = "retry-e2e-task";
    enqueue_queued_task(&hangar_dir, task_id);

    // The original row must reach `failed` for a RETRYABLE reason (NOT
    // agent_error): that is what makes the daemon spawn a retry child.
    let parent_failed = poll(
        &hangar_dir,
        Instant::now() + Duration::from_secs(60),
        |pool_dir| {
            let (status, reason) = status_and_reason(pool_dir, task_id);
            (status.as_deref() == Some("failed")).then_some((status, reason))
        },
    );
    let (_, parent_reason) = parent_failed.unwrap_or_else(|| {
        panic!(
            "original task never reached `failed`; last = {:?}",
            status_and_reason(&hangar_dir, task_id)
        )
    });
    assert!(
        matches!(
            parent_reason.as_deref(),
            Some("runtime_offline" | "runtime_recovery")
        ),
        "the original task must fail for a RETRYABLE (infra) reason, got {parent_reason:?}"
    );

    // POSITIVE: a child row carrying parent_task_id = the original is created AND
    // ultimately reaches `done` (attempt 2 of the fake exits 0).
    let child = poll(
        &hangar_dir,
        Instant::now() + Duration::from_secs(60),
        |pool_dir| {
            let child = child_of(pool_dir, task_id)?;
            (child.1.as_str() == "done").then_some(child)
        },
    );
    let (child_id, child_status, child_attempt) = child.unwrap_or_else(|| {
        panic!(
            "no child task with parent_task_id={task_id:?} ever reached `done`; \
             child now = {:?}",
            child_of(&hangar_dir, task_id)
        )
    });
    assert_eq!(child_status, "done", "the retry child must succeed");
    assert_eq!(
        child_attempt, 2,
        "the child is the parent's attempt + 1 (parent was attempt 1)"
    );
    assert_ne!(
        child_id, task_id,
        "the child is a distinct row from the parent"
    );

    // NEGATIVE (paired): the parent itself stayed `failed`, never silently
    // morphed to `done` (the retry created a NEW row, it did not resurrect the
    // original).
    assert_eq!(
        read_status(&hangar_dir, task_id).as_deref(),
        Some("failed"),
        "the original retried row stays `failed` for audit"
    );
}

// ---------------------------------------------------------------------------
// NO-CHILD-ON-AGENT-ERROR (NEGATIVE): a plain agent_error never retries.
// ---------------------------------------------------------------------------

#[test]
fn agent_error_failure_does_not_spawn_a_child() {
    if !can_run_tripwire() {
        skip("retry/timeout e2e tripwire (agent-error leg)");
        return;
    }

    let daemon = spawn_seeded_daemon(&write_agent_error_fake_claude, &[]);
    let hangar_dir = daemon.hangar_dir();
    let task_id = "agent-error-e2e-task";
    enqueue_queued_task(&hangar_dir, task_id);

    // Wait for the row to fail with reason = agent_error.
    let failed = poll(
        &hangar_dir,
        Instant::now() + Duration::from_secs(60),
        |pool_dir| {
            let (status, reason) = status_and_reason(pool_dir, task_id);
            (status.as_deref() == Some("failed")).then_some(reason)
        },
    );
    let reason = failed.unwrap_or_else(|| {
        panic!(
            "agent_error task never reached `failed`; last = {:?}",
            status_and_reason(&hangar_dir, task_id)
        )
    });
    assert_eq!(
        reason.as_deref(),
        Some("agent_error"),
        "a plain non-zero exit must classify as agent_error (non-retryable)"
    );

    // Give the claim loop ample slack to (wrongly) spawn + claim a child, then
    // assert NO child with parent_task_id = this task exists. Settle by waiting
    // past several poll intervals so a missing child is a real negative, not a
    // not-yet-spawned race.
    settle(Duration::from_secs(3));
    assert!(
        child_of(&hangar_dir, task_id).is_none(),
        "agent_error must NOT spawn a retry child, but one exists: {:?}",
        child_of(&hangar_dir, task_id)
    );
}

// ---------------------------------------------------------------------------
// TIMEOUT (POSITIVE): a provider that sleeps past the deadline is killed and the
// row is failed with reason = timeout, within a bounded budget.
// ---------------------------------------------------------------------------

#[test]
fn provider_past_deadline_is_killed_and_failed_with_reason_timeout() {
    if !can_run_tripwire() {
        skip("retry/timeout e2e tripwire (timeout leg)");
        return;
    }

    // Configure a TINY provider deadline so the fake's `sleep 120` blows it fast.
    let daemon = spawn_seeded_daemon(
        &write_blocking_fake_claude,
        &[("HANGAR_PROVIDER_MAX_RUNTIME_MS", "1500")],
    );
    let hangar_dir = daemon.hangar_dir();
    let task_id = "timeout-e2e-task";
    enqueue_queued_task(&hangar_dir, task_id);

    // The fake blocks far past the 1.5s deadline; the daemon must kill it and
    // fail the row with reason = timeout well within this budget (deadline +
    // claim/poll slack ≪ the fake's 120s sleep).
    let failed = poll(
        &hangar_dir,
        Instant::now() + Duration::from_secs(30),
        |pool_dir| {
            let (status, reason) = status_and_reason(pool_dir, task_id);
            (status.as_deref() == Some("failed")).then_some(reason)
        },
    );
    let reason = failed.unwrap_or_else(|| {
        panic!(
            "task past the provider deadline never reached `failed` within budget; \
             last = {:?}",
            status_and_reason(&hangar_dir, task_id)
        )
    });

    // POSITIVE: failed specifically for timeout.
    assert_eq!(
        reason.as_deref(),
        Some("timeout"),
        "a provider that overran the deadline must fail with reason = timeout"
    );
    // NEGATIVE (paired): NOT agent_error — the deadline-kill path is distinct
    // from a non-zero agent exit.
    assert_ne!(
        reason.as_deref(),
        Some("agent_error"),
        "the timeout-kill path must not be mislabelled agent_error"
    );
}

// ===========================================================================
// Harness
// ===========================================================================

/// Spawn a claim-enabled daemon over a freshly-seeded isolated `$HOME`, with the
/// fake-`claude` written by `write_fake` and `extra_env` layered on top.
///
/// Polls for the RPC socket so the daemon has booted past migrations + bind
/// before the caller enqueues. The fake-claude is written into the same `$HOME`
/// (the runner forwards `$HOME` to the provider, so its counter file is stable).
fn spawn_seeded_daemon(
    write_fake: &dyn Fn(&Path) -> PathBuf,
    extra_env: &[(&str, &str)],
) -> Daemon {
    let home = tempfile::tempdir().expect("isolated HOME tempdir");
    let hangar_dir = home.path().join(".agents-in-a-box");
    std::fs::create_dir_all(&hangar_dir).expect("create ~/.agents-in-a-box");

    seed_and_free_slot(&hangar_dir);
    let fake_claude = write_fake(home.path());

    let bin = daemon_bin().expect("gated by can_run_tripwire");
    let mut cmd = Command::new(bin);
    cmd.env("HOME", home.path())
        .env_remove("AINB_HANGAR_HOME")
        .env("HANGAR_DAEMON_RUNTIME_ID", RUNTIME_ID)
        .env("HANGAR_CLAUDE_PATH", &fake_claude)
        // Fast claim cadence so child re-dispatch + each leg's terminal state
        // land inside the poll budgets; tight sweep so any backstop fires fast.
        .env("HANGAR_DAEMON_POLL_MS", "200")
        .env("HANGAR_SWEEP_INTERVAL_MS", "300")
        // Run the provider UNCONFINED: the tripwire's fake-claude is a `/bin/sh`
        // script that reads/writes a counter file under `$HOME`, outside the
        // task's sandboxed roots — the OS sandbox would (correctly) deny it.
        .env("HANGAR_DAEMON_DISABLE_SANDBOX", "1")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    let child = cmd.spawn().expect("spawn ainb-hangar-daemon");

    let socket = hangar_dir.join("hangar.sock");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !socket.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }

    Daemon { home, child }
}

/// Seed the P4 fixture into `{hangar_dir}/hangar.db`, free the fixture's single
/// running slot (`task-1` → done), and raise `agent-1`'s concurrency so the
/// claim loop has slots for both the seeded task AND its retry child.
fn seed_and_free_slot(hangar_dir: &Path) {
    block_on(async {
        let store = ainb_hangar_store::Store::open_in(hangar_dir).await.expect("open seed store");
        let pool = store.pool();
        ainb_hangar_daemon::seed::seed_p4_fixture(pool).await.expect("seed P4 fixture");
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
    });
}

/// Enqueue one `queued` task on the seeded runtime/agent/workspace that the
/// claim-enabled daemon will pick up. `created_at` is wall-clock so the
/// queued-TTL sweeper never reaps it; no `issue_id` so the partial-unique
/// pending index can't collide with the retry child.
fn enqueue_queued_task(hangar_dir: &Path, task_id: &str) {
    block_on(async {
        let store =
            ainb_hangar_store::Store::open_in(hangar_dir).await.expect("open enqueue store");
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
        .bind(task_id)
        .bind(ainb_hangar_daemon::seed::WS_ID)
        .bind(RUNTIME_ID)
        .bind("agent-1")
        .bind(now_ms)
        .execute(store.pool())
        .await
        .expect("enqueue task");
    });
}

/// Write an executable fake `claude` that fails RETRYABLY on attempt 1 (exit 75
/// = `EX_TEMPFAIL`, the POSIX "temporary failure, retry later" code → the daemon
/// classifies it as an infra/retryable [`FailureReason::RuntimeOffline`]), then
/// succeeds on attempt 2 (exit 0).
///
/// The attempt counter lives in `$HOME/.fake-claude-count` — `$HOME` is the
/// daemon's isolated tempdir and the only stable env the runner forwards.
fn write_retry_fake_claude(dir: &Path) -> PathBuf {
    write_executable(
        dir,
        "fake-claude-retry.sh",
        "#!/bin/sh\n\
         COUNT_FILE=\"$HOME/.fake-claude-count\"\n\
         n=$(cat \"$COUNT_FILE\" 2>/dev/null || echo 0)\n\
         n=$((n + 1))\n\
         echo \"$n\" > \"$COUNT_FILE\"\n\
         echo '{\"type\":\"system\",\"session_id\":\"retry-'\"$n\"'\"}'\n\
         if [ \"$n\" -eq 1 ]; then\n\
         \techo '{\"type\":\"result\",\"content\":\"runtime offline\"}'\n\
         \texit 75\n\
         fi\n\
         echo '{\"type\":\"result\",\"content\":\"ok\"}'\n\
         exit 0\n",
    )
}

/// Write an executable fake `claude` that always exits 1 (a plain agent error —
/// the LLM mis-tooled / gave up), the canonical NON-retryable failure.
fn write_agent_error_fake_claude(dir: &Path) -> PathBuf {
    write_executable(
        dir,
        "fake-claude-agent-error.sh",
        "#!/bin/sh\n\
         echo '{\"type\":\"system\",\"session_id\":\"agent-err\"}'\n\
         echo '{\"type\":\"result\",\"content\":\"i give up\"}'\n\
         exit 1\n",
    )
}

/// Write an executable fake `claude` that echoes a system line then blocks well
/// past any test-sane provider deadline, so the daemon must kill it on timeout.
fn write_blocking_fake_claude(dir: &Path) -> PathBuf {
    write_executable(
        dir,
        "fake-claude-block.sh",
        "#!/bin/sh\n\
         echo '{\"type\":\"system\",\"session_id\":\"timeout-block\"}'\n\
         sleep 120\n",
    )
}

/// Write `body` to `{dir}/{name}`, chmod 0755, and return its path.
fn write_executable(dir: &Path, name: &str, body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join(name);
    std::fs::write(&path, body).expect("write fake-claude script");
    let mut perms = std::fs::metadata(&path).expect("stat script").permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&path, perms).expect("chmod script");
    path
}

/// Read the `status` of `task_id` from `{hangar_dir}/hangar.db`, or `None` if
/// the row is absent. Opens a fresh pool each call.
fn read_status(hangar_dir: &Path, task_id: &str) -> Option<String> {
    block_on(async {
        let store = ainb_hangar_store::Store::open_in(hangar_dir).await.expect("open status store");
        sqlx::query_scalar::<_, String>("SELECT status FROM agent_task_queue WHERE id = ?")
            .bind(task_id)
            .fetch_optional(store.pool())
            .await
            .expect("query task status")
    })
}

/// Read `(status, failure_reason)` for `task_id`, or `(None, None)` if absent.
fn status_and_reason(hangar_dir: &Path, task_id: &str) -> (Option<String>, Option<String>) {
    block_on(async {
        let store = ainb_hangar_store::Store::open_in(hangar_dir).await.expect("open status store");
        let row = sqlx::query_as::<_, (String, Option<String>)>(
            "SELECT status, failure_reason FROM agent_task_queue WHERE id = ?",
        )
        .bind(task_id)
        .fetch_optional(store.pool())
        .await
        .expect("query task status+reason");
        row.map_or((None, None), |(s, r)| (Some(s), r))
    })
}

/// Find the single child row whose `parent_task_id = parent_id`, returning
/// `(child_id, status, attempt)`, or `None` if none exists.
fn child_of(hangar_dir: &Path, parent_id: &str) -> Option<(String, String, i64)> {
    block_on(async {
        let store = ainb_hangar_store::Store::open_in(hangar_dir).await.expect("open child store");
        sqlx::query_as::<_, (String, String, i64)>(
            "SELECT id, status, attempt FROM agent_task_queue WHERE parent_task_id = ? LIMIT 1",
        )
        .bind(parent_id)
        .fetch_optional(store.pool())
        .await
        .expect("query child task")
    })
}

/// Poll `{hangar_dir}/hangar.db` until `pred` returns `Some(_)` or `deadline`
/// passes. No bare sleep before the first read; a 150ms inter-poll gap,
/// deadline-bounded.
fn poll<T>(hangar_dir: &Path, deadline: Instant, pred: impl Fn(&Path) -> Option<T>) -> Option<T> {
    loop {
        if let Some(v) = pred(hangar_dir) {
            return Some(v);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(150));
    }
}

/// Block for `dur` to let the claim loop settle before asserting a NEGATIVE
/// (no-child) — a deliberate wait *after* a terminal state is observed, not a
/// bare sleep before a read.
fn settle(dur: Duration) {
    std::thread::sleep(dur);
}

/// Run `fut` to completion on a one-shot current-thread tokio runtime.
fn block_on<F: std::future::Future>(fut: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("one-shot runtime")
        .block_on(fut)
}
