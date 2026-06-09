//! CLI surface test for the `ainb hangar autopilot` verbs (P7.6, second leg).
//!
//! Drives the real `ainb` binary (via `CARGO_BIN_EXE_ainb`) against an isolated
//! `$AINB_HANGAR_HOME` tempdir, mirroring `hangar_cli_integration.rs`. This is
//! the cheap CLI leg of the P7.6 e2e (per `feedback_cli_surface_as_plugin_test_leg`):
//! it proves the user-visible control-plane flow — create → list → disable →
//! list (disabled badge) → run (fires a task) — without any tmux keystrokes.
//!
//! The autopilot `agent_id` is an FK into the `agent` table, so the test seeds a
//! workspace + user + runtime + agent directly into the tempdir's `hangar.db`
//! (the same shape `scheduler_loop.rs` seeds) before invoking the binary, then
//! passes that agent id to `autopilot create`.

use std::path::PathBuf;
use std::process::Command;

use ainb_hangar_store::Store;
use sqlx::SqlitePool;

fn ainb_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ainb"))
}

/// Run `ainb <args>` with an isolated hangar home. Returns (success, output).
fn run(home: &std::path::Path, args: &[&str]) -> (bool, String) {
    let out = Command::new(ainb_bin())
        .args(args)
        .env("AINB_HANGAR_HOME", home)
        .env("HOME", home)
        .output()
        .expect("spawn ainb");
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    (out.status.success(), format!("{stdout}{stderr}"))
}

/// Seed the workspace + user + runtime + agent the autopilot FK requires,
/// matching the slug the CLI bootstraps (`default`) so `--workspace` defaulting
/// resolves to this row.
async fn seed_agent(pool: &SqlitePool) {
    sqlx::query(
        "INSERT INTO workspace (id, slug, name, created_at) \
         VALUES ('ws-1', 'default', 'Default Workspace', 0)",
    )
    .execute(pool)
    .await
    .expect("seed workspace");
    sqlx::query("INSERT INTO user (id, email, created_at) VALUES ('u-1', 'a@b.c', 0)")
        .execute(pool)
        .await
        .expect("seed user");
    sqlx::query("INSERT INTO member (workspace_id, user_id, role) VALUES ('ws-1', 'u-1', 'owner')")
        .execute(pool)
        .await
        .expect("seed member");
    sqlx::query(
        "INSERT INTO agent_runtime (id, workspace_id, daemon_id, provider, runtime_mode, status) \
         VALUES ('rt-1', 'ws-1', 'd-1', 'claude', 'local', 'online')",
    )
    .execute(pool)
    .await
    .expect("seed runtime");
    sqlx::query(
        "INSERT INTO agent (id, workspace_id, name, runtime_id, visibility, owner_id) \
         VALUES ('ag-1', 'ws-1', 'Tester', 'rt-1', 'workspace', 'u-1')",
    )
    .execute(pool)
    .await
    .expect("seed agent");
}

#[tokio::test]
async fn cli_autopilot_create_list_disable_run() {
    let tmp = tempfile::tempdir().unwrap();
    // Open (and migrate) the db at the same location the binary will, then seed
    // the agent the autopilot will bind to.
    {
        let store = Store::open_in(tmp.path()).await.expect("open store");
        seed_agent(store.pool()).await;
    }

    // create — a valid daily cron, bound to the seeded agent.
    let (ok, out) = run(
        tmp.path(),
        &[
            "hangar",
            "autopilot",
            "create",
            "--name",
            "smoke",
            "--cron",
            "0 9 * * *",
            "--agent",
            "ag-1",
        ],
    );
    assert!(ok, "autopilot create should exit 0; out={out}");
    assert!(
        out.contains("created autopilot"),
        "missing create ack:\n{out}"
    );
    // Pull the minted autopilot id: "created autopilot <id> `smoke` ...".
    let id = out
        .split_whitespace()
        .nth(2)
        .map(str::to_string)
        .expect("create output carries an id");

    // list — table contains the name + the cron expr, with an enabled badge.
    let (ok, out) = run(tmp.path(), &["hangar", "autopilot", "list"]);
    assert!(ok, "autopilot list should exit 0; out={out}");
    assert!(out.contains("smoke"), "list missing name:\n{out}");
    assert!(out.contains("0 9 * * *"), "list missing cron:\n{out}");
    assert!(
        out.contains("enabled"),
        "list missing enabled badge:\n{out}"
    );

    // disable — then list shows the disabled badge.
    let (ok, out) = run(tmp.path(), &["hangar", "autopilot", "disable", &id]);
    assert!(ok, "autopilot disable should exit 0; out={out}");

    let (ok, out) = run(tmp.path(), &["hangar", "autopilot", "list"]);
    assert!(ok, "autopilot list (post-disable) should exit 0; out={out}");
    assert!(
        out.contains("disabled"),
        "list missing disabled badge:\n{out}"
    );

    // run — fires a task immediately (manual run, bypassing the schedule).
    let (ok, out) = run(tmp.path(), &["hangar", "autopilot", "run", &id]);
    assert!(ok, "autopilot run should exit 0; out={out}");
    assert!(out.contains("fired autopilot"), "missing fire ack:\n{out}");

    // Verify the manual run actually enqueued a task + a run row in the db.
    let store = Store::open_in(tmp.path()).await.expect("reopen store");
    let task_count: i64 = sqlx::query_scalar("SELECT count(*) FROM agent_task_queue")
        .fetch_one(store.pool())
        .await
        .expect("count tasks");
    assert_eq!(task_count, 1, "manual run must enqueue exactly one task");
    let run_count: i64 = sqlx::query_scalar("SELECT count(*) FROM autopilot_run")
        .fetch_one(store.pool())
        .await
        .expect("count runs");
    assert_eq!(
        run_count, 1,
        "manual run must create exactly one autopilot_run"
    );
}

#[tokio::test]
async fn cli_autopilot_create_rejects_invalid_cron_before_insert() {
    let tmp = tempfile::tempdir().unwrap();
    {
        let store = Store::open_in(tmp.path()).await.expect("open store");
        seed_agent(store.pool()).await;
    }

    let (ok, out) = run(
        tmp.path(),
        &[
            "hangar",
            "autopilot",
            "create",
            "--name",
            "bad",
            "--cron",
            "not a cron",
            "--agent",
            "ag-1",
        ],
    );
    assert!(!ok, "create with a malformed cron must fail; out={out}");

    // No row was written — the cron is rejected before the insert.
    let store = Store::open_in(tmp.path()).await.expect("reopen store");
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM autopilot")
        .fetch_one(store.pool())
        .await
        .expect("count autopilots");
    assert_eq!(count, 0, "a malformed cron must leave zero autopilot rows");
}
