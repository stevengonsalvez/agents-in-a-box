//! e38.27 tripwire: a real, claim-enabled daemon fleet honours an agent's
//! `max_concurrent_tasks` cap under LIVE contention — no over-dispatch, no
//! double-claim.
//!
//! ```text
//!  seed db (ws+user+rt+agent, cap=3)
//!         │  enqueue 10 queued tasks (NULL issue_id) for agent-1/runtime-1
//!         ▼
//!  spawn FIVE claim-enabled daemons, all bound to runtime-1, same $HOME db
//!         │   each: claim(queued→dispatched) ─▶ start(→running) ─▶ fake-claude
//!         │        (sleeps ~0.4s) ─▶ complete(→done)
//!         ▼
//!  sampler thread polls COUNT(status='running' for agent-1) every ~20ms while
//!  the queue drains:
//!    (NEGATIVE) the running count for agent-1 NEVER exceeds the cap (3) AND
//!    (POSITIVE) all 10 tasks eventually reach a terminal state AND
//!    (POSITIVE) every task was dispatched exactly once (no row claimed twice:
//!               no duplicate started_at-bearing run; 10 distinct terminal rows).
//! ```
//!
//! ## Why a daemon FLEET, and why the cap still binds after a54
//!
//! Since a54 a SINGLE daemon runs claimed executions concurrently (spawned onto
//! a `JoinSet`), so one daemon alone can already hold several of an agent's
//! tasks in flight. A five-daemon fleet against a cap of 3 pushes that
//! contention harder still, from multiple processes at once — the per-agent
//! `max_concurrent_tasks` guard (`claim.rs` `CLAIM_SQL`, a correlated COUNT of
//! the agent's live `dispatched`+`running` rows) is the ONLY thing holding the
//! in-flight count at 3, whether the contenders are threads in one daemon or a
//! fleet of five. This is the daemon-level concurrency the store-layer
//! `claim_task_integration.rs` `tokio::join!` test cannot reach: it proves the
//! *atomic claim statement* is race-safe, but never spawns processes that drive
//! the full claim→start→run→complete FSM concurrently against one db.
//!
//! ## Why this is a distinct gap from the daemon-health tripwire
//!
//! `tripwire_daemon_health_sparkline` runs ONE claim-enabled daemon and merely
//! drains a queue to populate the throughput ring — it never spawns a fleet and
//! never samples the live `running` count, so it cannot catch a cap breach.
//!
//! ## Harness reuse + HARD RULES
//!
//! Reuses `tripwire_support` (the P1.7 daemon-spawn helpers): `seed_world`,
//! `daemon_bin`, the `DaemonSession` RAII tmux wrapper (exact-name kill only),
//! and the wall-clock `now_ms`. The fleet + the live `running`-count sampler
//! are the new bits here. SKIP-not-fail when tmux is missing;
//! `--test-threads=1` (the runner enforces it) so the five daemon processes
//! never race a sibling tripwire's tempdir. Each `DaemonSession` kills ONLY its
//! own exact-named tmux session on drop — never `pkill`/`killall`/wildcard.

#![allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
// `Duration::from_secs(N)` reads fine as a poll budget; `from_mins` is unstable.
#![allow(clippy::duration_suboptimal_units)]

mod tripwire_support;

use std::time::{Duration, Instant};

use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use tripwire_support::{DaemonSession, daemon_bin, now_ms, seed_world};

/// Per-agent concurrency cap under test. Five daemons contend; only this many
/// of the agent's tasks may be `running` at once.
const CAP: i64 = 3;
/// How many daemons contend for the single runtime's queue. STRICTLY GREATER
/// than [`CAP`] so the cap — not the daemon count — is the binding constraint.
const DAEMONS: usize = 5;
/// How many queued tasks to drain through the fleet.
const TASKS: usize = 10;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_fleet_honours_concurrency_cap_no_double_claim() {
    // SKIP cleanly when tmux is unavailable so the tripwire never false-fails on
    // a host without the tool.
    if !tripwire_support::tmux_available() {
        eprintln!("SKIP: daemon concurrent-cap tripwire (need tmux + built `ainb-hangar-daemon`)");
        return;
    }

    // Isolated $HOME-style root; the db lives at `{root}/hangar.db` and every
    // daemon resolves it via AINB_HANGAR_HOME (matching the P1.7 tripwires).
    let home = tempfile::tempdir().expect("tempdir home");
    let db_path = home.path().join("hangar.db");

    // Bring the db up to schema, seed the world, and pin the agent's cap.
    let pool = open_pool(&db_path).await;
    ainb_hangar_store::apply_migrations(&pool).await.expect("migrate");
    let ids = seed_world(&pool).await;
    set_cap(&pool, &ids.agent_id, CAP).await;

    // A fake `claude` that holds each task `running` ~0.4s — long enough for the
    // sampler to observe concurrent `running` rows, short enough that 10 tasks
    // drain through a 3-wide cap well inside the poll budget.
    let fake_claude = fake_claude_sleeps(home.path(), 0.4);

    // Enqueue 10 queued tasks (NULL issue_id → the per-(issue,agent) guard is
    // bypassed, so ONLY the max_concurrent_tasks cap can bound in-flight work).
    // `created_at` is wall-clock "now" so the queued-TTL sweeper never reaps them.
    let now = now_ms();
    for i in 0..TASKS {
        sqlx::query(
            "INSERT INTO agent_task_queue \
             (id, workspace_id, runtime_id, agent_id, issue_id, status, created_at) \
             VALUES (?, ?, ?, ?, NULL, 'queued', ?)",
        )
        .bind(format!("cap-task-{i}"))
        .bind(&ids.workspace_id)
        .bind(&ids.runtime_id)
        .bind(&ids.agent_id)
        .bind(now)
        .execute(&pool)
        .await
        .unwrap_or_else(|e| panic!("enqueue cap-task-{i}: {e}"));
    }

    // Spawn the daemon FLEET: five claim-enabled daemons, all bound to the same
    // runtime, all against the same db. A fast poll maximises contention so any
    // cap-breaching race surfaces. They are dropped (killed by exact name) at
    // the end of the test.
    let mut daemons = Vec::with_capacity(DAEMONS);
    let home_str = home.path().to_str().expect("utf-8 home");
    let claude_str = fake_claude.to_str().expect("utf-8 fake-claude");
    for _ in 0..DAEMONS {
        daemons.push(DaemonSession::spawn(
            &daemon_bin(),
            home.path(),
            &[
                ("AINB_HANGAR_HOME", home_str),
                ("HANGAR_DAEMON_RUNTIME_ID", &ids.runtime_id),
                ("HANGAR_CLAUDE_PATH", claude_str),
                ("HANGAR_DAEMON_POLL_MS", "30"),
                // Keep the sweepers calm so they don't reclaim/fail rows
                // mid-flight and muddy the cap accounting.
                ("HANGAR_SWEEP_INTERVAL_MS", "60000"),
            ],
        ));
    }

    // Drive the live sampler + the terminal-drain wait concurrently on the same
    // pool, then assert on the collected evidence.
    let deadline = Instant::now() + Duration::from_secs(60);
    let max_running = sample_max_running_until_drained(&pool, &ids.agent_id, deadline).await;

    // Tear the fleet down (exact-name kills) before the final assertions so no
    // daemon mutates the db while we read the terminal accounting.
    drop(daemons);

    // --- NEGATIVE: the cap was NEVER breached under live contention -----------
    // `<= CAP` is the invariant (the cap must never be exceeded). In practice
    // the five-daemon fleet saturates the 3-wide cap — the sampler routinely
    // observes exactly 3 concurrent `running` rows — so this also proves the
    // contention is real and not trivially serialised down to 1.
    assert!(
        max_running <= CAP,
        "max_concurrent_tasks cap BREACHED under live daemon contention: \
         observed {max_running} concurrent `running` rows for agent {} (cap = {CAP})",
        ids.agent_id
    );

    // --- POSITIVE: all 10 tasks reached a terminal state ----------------------
    let terminal = count_terminal(&pool, &ids.agent_id).await;
    assert_eq!(
        terminal, TASKS as i64,
        "all {TASKS} tasks should reach a terminal state; only {terminal} did \
         (the fleet stalled or the cap starved the queue)"
    );

    // --- POSITIVE: no row was double-claimed (each ran exactly once) ----------
    // Every task that started carries a non-null `started_at`. A row claimed +
    // started by two daemons would surface as a `StartTaskService` AlreadyStarted
    // error (so it would NOT be `done`) — so a clean count of TASKS distinct
    // `done` rows, each with a single `started_at`, proves single dispatch.
    let distinct_started = count_distinct_started(&pool, &ids.agent_id).await;
    assert_eq!(
        distinct_started, TASKS as i64,
        "every task should be started exactly once; {distinct_started} distinct \
         started rows seen (a double-claim would lose or duplicate a run)"
    );
    let done = count_done(&pool, &ids.agent_id).await;
    assert_eq!(
        done, TASKS as i64,
        "all {TASKS} tasks should complete `done` (a double-claimed row would \
         fail its second start and not finish cleanly); {done} done"
    );
}

/// Open a `SQLite` WAL pool at `db_path` (creating it if absent).
async fn open_pool(db_path: &std::path::Path) -> SqlitePool {
    let opts = SqliteConnectOptions::new()
        .filename(db_path)
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal);
    SqlitePoolOptions::new().connect_with(opts).await.expect("open pool")
}

/// Set `agent.max_concurrent_tasks = cap`.
async fn set_cap(pool: &SqlitePool, agent_id: &str, cap: i64) {
    sqlx::query("UPDATE agent SET max_concurrent_tasks = ? WHERE id = ?")
        .bind(cap)
        .bind(agent_id)
        .execute(pool)
        .await
        .expect("set agent cap");
}

/// Write an executable fake `claude` that emits a `system` line (so session-id
/// parsing has something to read), sleeps `secs` so the claimed task stays
/// `running` long enough to sample, then emits a `result` line and exits 0.
fn fake_claude_sleeps(dir: &std::path::Path, secs: f64) -> std::path::PathBuf {
    let path = dir.join("fake-claude-sleep.sh");
    let body = format!(
        "#!/bin/sh\n\
         echo '{{\"type\":\"system\",\"session_id\":\"cap-'\"$$\"'\"}}'\n\
         sleep {secs}\n\
         echo '{{\"type\":\"result\",\"content\":\"ok\"}}'\n\
         exit 0\n"
    );
    std::fs::write(&path, body).expect("write sleeping fake-claude");
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&path).expect("stat script").permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).expect("chmod script");
    }
    path
}

/// Poll the live `running` count for `agent_id` every ~20ms, tracking the
/// maximum observed, until all [`TASKS`] tasks are terminal OR `deadline`
/// passes. Returns the maximum concurrent `running` count seen.
///
/// No bare sleep before the first read; a tight 20ms gap so a brief cap breach
/// in the `dispatched→running` window is not sampled past. Deadline-bounded.
async fn sample_max_running_until_drained(
    pool: &SqlitePool,
    agent_id: &str,
    deadline: Instant,
) -> i64 {
    let mut max_running = 0_i64;
    loop {
        let running = count_running(pool, agent_id).await;
        max_running = max_running.max(running);

        let terminal = count_terminal(pool, agent_id).await;
        if terminal >= TASKS as i64 {
            // One last sample after the drain finished cannot raise the max
            // (everything is terminal), so we are done.
            return max_running;
        }
        if Instant::now() >= deadline {
            return max_running;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Count `agent_id`'s tasks currently in `running`.
async fn count_running(pool: &SqlitePool, agent_id: &str) -> i64 {
    sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM agent_task_queue WHERE agent_id = ? AND status = 'running'",
    )
    .bind(agent_id)
    .fetch_one(pool)
    .await
    .expect("count running")
}

/// Count `agent_id`'s tasks in a terminal state.
async fn count_terminal(pool: &SqlitePool, agent_id: &str) -> i64 {
    sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM agent_task_queue \
         WHERE agent_id = ? AND status IN ('done','failed','cancelled')",
    )
    .bind(agent_id)
    .fetch_one(pool)
    .await
    .expect("count terminal")
}

/// Count `agent_id`'s tasks that reached `done`.
async fn count_done(pool: &SqlitePool, agent_id: &str) -> i64 {
    sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM agent_task_queue WHERE agent_id = ? AND status = 'done'",
    )
    .bind(agent_id)
    .fetch_one(pool)
    .await
    .expect("count done")
}

/// Count `agent_id`'s tasks with a non-null `started_at` (i.e. every task that
/// was actually dispatched + started). A double-claimed row's second start
/// errors `AlreadyStarted` and never re-stamps, so this is a clean
/// "started exactly once" proxy.
async fn count_distinct_started(pool: &SqlitePool, agent_id: &str) -> i64 {
    sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM agent_task_queue \
         WHERE agent_id = ? AND started_at IS NOT NULL",
    )
    .bind(agent_id)
    .fetch_one(pool)
    .await
    .expect("count started")
}
