//! e38.20 boot test: a real daemon boot self-registers an `agent_runtime` row.
//!
//! Before e38.20 every `agent_runtime` insert lived only in test fixtures, so a
//! freshly-booted daemon advertised no runtime. This drives the real
//! `ainb-hangar-daemon` binary in one-shot mode (`--once`) against an isolated
//! `$AINB_HANGAR_HOME` that already holds a bootstrapped workspace, with
//! `HANGAR_DAEMON_RUNTIME_ID` set, and asserts the boot path upserted exactly one
//! `agent_runtime` row keyed on that runtime id, marked `online`. A second boot
//! must not duplicate or error (idempotent upsert).
//!
//! Before [`boot`](ainb_hangar_daemon::boot) calls
//! [`register_runtime`](ainb_hangar_daemon::runtime_register::register_runtime)
//! this test is RED: no `agent_runtime` row is ever written, so the row-count
//! assertion fails.

use std::path::Path;
use std::process::Command;

use ainb_hangar_store::Store;
use assert_cmd::prelude::*;

/// Seed a default workspace into the isolated home's `hangar.db` so the
/// self-registered runtime's `workspace_id` FK is satisfiable. Opens the same
/// db file the daemon will open (`$AINB_HANGAR_HOME/hangar.db`).
fn seed_workspace(home: &Path) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let store = Store::open_in(home).await.expect("open hangar db");
        sqlx::query("INSERT INTO workspace (id, slug, name, created_at) VALUES (?, ?, ?, ?)")
            .bind("ws-boot")
            .bind("default")
            .bind("Default")
            .bind(1_i64)
            .execute(store.pool())
            .await
            .expect("seed workspace");
    });
}

/// Count `agent_runtime` rows in the isolated home's db.
fn runtime_count(home: &Path) -> i64 {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let store = Store::open_in(home).await.expect("open hangar db");
        sqlx::query_scalar("SELECT COUNT(*) FROM agent_runtime")
            .fetch_one(store.pool())
            .await
            .expect("count runtimes")
    })
}

/// Run `ainb-hangar-daemon --once` against `home` with the given runtime id.
fn boot_once(home: &Path, runtime_id: &str) {
    let output = Command::cargo_bin("ainb-hangar-daemon")
        .expect("binary builds")
        .arg("--once")
        .env("AINB_HANGAR_HOME", home)
        .env("HANGAR_DAEMON_RUNTIME_ID", runtime_id)
        // Keep the claim loop quiet — `--once` returns before it runs anyway,
        // but be explicit so the boot path is the only thing exercised.
        .env("HANGAR_DAEMON_DISABLE_CLAIM", "1")
        .output()
        .expect("run --once");
    assert!(
        output.status.success(),
        "--once should exit 0, stderr={:?}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn boot_self_registers_runtime_idempotently() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    seed_workspace(home);

    // First boot registers the runtime.
    boot_once(home, "rt-boot");
    assert_eq!(
        runtime_count(home),
        1,
        "boot must self-register exactly one agent_runtime row"
    );

    // The row is keyed on the runtime id and marked online.
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let store = Store::open_in(home).await.unwrap();
        let row =
            ainb_hangar_store::repo::agent_runtime::AgentRuntimeRepo::get(store.pool(), "rt-boot")
                .await
                .unwrap()
                .expect("the self-registered runtime exists");
        assert_eq!(row.status, "online", "a booted runtime is online");
        assert_eq!(row.workspace_id, "ws-boot");
    });

    // A second boot (restart) upserts the same row — no duplicate, no error.
    boot_once(home, "rt-boot");
    assert_eq!(
        runtime_count(home),
        1,
        "a restart must not duplicate the runtime row"
    );
}
