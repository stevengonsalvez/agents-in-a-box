//! Parity #30 ACCEPTANCE SWEEP: a per-agent env value never lands anywhere it
//! can be read back.
//!
//! ```text
//!   agent(agent_env = {SECRET_TOKEN: sk-live-DEADBEEF01})
//!        │
//!        ▼  one REAL dispatch through the real daemon binary
//!   fake codex ──▶ writes $HOME/child-saw-secret  (proves the child GOT it)
//!        │
//!        ▼  sweep for the literal `sk-live-DEADBEEF01`
//!   ┌───────────────────────────────────────────────────────────┐
//!   │ every TEXT column of every hangar.db table  … EXCEPT      │
//!   │   agent.agent_env (the authoritative store — deviation D2)│
//!   │ the daemon log files                                      │
//!   │ every file under the task's logs dir (post-teardown)      │
//!   │ `ainb hangar agent list --format json` stdout             │
//!   │ `ainb hangar agent env <id>` stdout                       │
//!   └───────────────────────────────────────────────────────────┘
//!   all must be CLEAN.
//! ```
//!
//! This is the test the acceptance sentence rests on: it FAILS on `main` (the
//! `agent list --format json` arm printed the plaintext value outright) and
//! passes after the change, while the `child-saw-secret` marker proves the
//! redaction was not achieved by breaking dispatch.
//!
//! Skips cleanly (never fails) when tmux or the `ainb` binary is unavailable, so
//! a CI image lacking either does not red the suite.

#![allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]

mod tripwire_support;

use std::path::{Path, PathBuf};
use std::time::Duration;

use sqlx::sqlite::SqlitePoolOptions;
use sqlx::{Row, SqlitePool};
use tripwire_support::{
    DaemonSession, ainb_bin, daemon_bin, seed_codex_agent, seed_world, wait_for_db,
};

/// The canary. Every assertion below is "this literal is absent".
const SECRET: &str = "sk-live-DEADBEEF01";

#[tokio::test]
async fn agent_env_value_leaks_into_no_persisted_or_rendered_artefact() {
    if !tripwire_support::tmux_available() {
        eprintln!("tmux not available; skipping agent-env leak sweep");
        return;
    }
    let Some(ainb) = ainb_bin() else {
        eprintln!("ainb binary not built; skipping agent-env leak sweep");
        return;
    };

    let home = tempfile::tempdir().expect("tempdir home");
    let db_path = home.path().join("hangar.db");

    let pool = open_pool(&db_path).await;
    ainb_hangar_store::apply_migrations(&pool).await.expect("migrate");
    let ids = seed_world(&pool).await;
    let (codex_runtime_id, codex_agent_id) = seed_codex_agent(&pool, &ids).await;

    // The agent carries the secret in its per-agent env — the ONE place it is
    // allowed to be at rest (deviation D-2).
    sqlx::query("UPDATE agent SET agent_env = ? WHERE id = ?")
        .bind(format!(r#"{{"SECRET_TOKEN":"{SECRET}"}}"#))
        .bind(&codex_agent_id)
        .execute(&pool)
        .await
        .expect("set agent_env");

    // A fake codex that PROVES it received the value without ever printing it:
    // printing would be the child's own leak, and would land in codex.jsonl —
    // a genuine artefact this sweep is right to fail on.
    let marker = home.path().join("child-saw-secret");
    let fake_codex = write_executable(
        home.path(),
        "fake-codex.sh",
        &format!(
            "#!/bin/sh\n\
             if [ \"${{SECRET_TOKEN}}\" = '{SECRET}' ]; then : > '{}'; fi\n\
             echo '{{\"type\":\"system\",\"session_id\":\"codex-sid\"}}'\n\
             echo '{{\"type\":\"result\",\"content\":\"codex-ok\"}}'\n\
             exit 0\n",
            marker.display()
        ),
    );

    let session = DaemonSession::spawn(
        &daemon_bin(),
        home.path(),
        &[
            ("AINB_HANGAR_HOME", home.path().to_str().unwrap()),
            ("HANGAR_DAEMON_RUNTIME_ID", &codex_runtime_id),
            ("HANGAR_CODEX_PATH", fake_codex.to_str().unwrap()),
            ("HANGAR_DAEMON_POLL_MS", "200"),
        ],
    );

    let task_id = "task-env-leak";
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
    .expect("enqueue task");

    let row = wait_for_db(&pool, task_id, "done", Duration::from_secs(30)).await;
    drop(session);

    // ── PRECONDITION: dispatch really did carry the plaintext to the child ──
    assert!(
        marker.exists(),
        "the provider child never received the agent_env value — a leak sweep \
         over a broken dispatch proves nothing"
    );

    // ── (a) every TEXT column of every table, except agent.agent_env ────────
    let tables: Vec<String> =
        sqlx::query_scalar("SELECT name FROM sqlite_master WHERE type = 'table'")
            .fetch_all(&pool)
            .await
            .expect("list tables");
    for table in &tables {
        if table.starts_with("sqlite_") || table.starts_with("_sqlx") {
            continue;
        }
        let cols: Vec<String> = sqlx::query(&format!("PRAGMA table_info({table})"))
            .fetch_all(&pool)
            .await
            .expect("table_info")
            .into_iter()
            .map(|r| r.get::<String, _>("name"))
            .collect();
        for col in cols {
            // The authoritative store of record: plaintext here is the design
            // (deviation D-2), and `repo_agent_env_redaction` pins its bytes.
            if table == "agent" && col == "agent_env" {
                continue;
            }
            let hits: i64 = sqlx::query_scalar(&format!(
                "SELECT COUNT(*) FROM \"{table}\" WHERE CAST(\"{col}\" AS TEXT) LIKE ?"
            ))
            .bind(format!("%{SECRET}%"))
            .fetch_one(&pool)
            .await
            .unwrap_or(0);
            assert_eq!(hits, 0, "the env value leaked into {table}.{col}");
        }
    }

    // ── (b) the daemon's own log files ─────────────────────────────────────
    let logs_root = home.path().join("hangar").join("logs");
    assert_clean_tree(&logs_root, "the daemon log directory");

    // ── (c) every file under the task's logs dir, AFTER teardown ───────────
    // This is what proves the interactive wrapper unlink and that no provider
    // log captured the value.
    let work_dir: String = row.get::<Option<String>, _>("work_dir").expect("work_dir populated");
    let task_logs = PathBuf::from(&work_dir).parent().expect("shortID root").join("logs");
    assert_clean_tree(&task_logs, "the task logs directory");

    // ── (d) `ainb hangar agent list --format json` stdout ───────────────────
    let listed = run_ainb(
        &ainb,
        home.path(),
        &["hangar", "agent", "list", "--format", "json"],
    );
    assert!(
        !listed.contains(SECRET),
        "agent list --format json leaked the value:\n{listed}"
    );
    assert!(
        listed.contains(r#""SECRET_TOKEN":"****""#),
        "agent list --format json must keep the KEY and mask the value:\n{listed}"
    );

    // ── (e) `ainb hangar agent env <id>` stdout ─────────────────────────────
    let env_out = run_ainb(
        &ainb,
        home.path(),
        &["hangar", "agent", "env", &codex_agent_id],
    );
    assert!(
        !env_out.contains(SECRET),
        "agent env leaked the value:\n{env_out}"
    );
    assert!(
        env_out.contains("SECRET_TOKEN=****"),
        "agent env must print the masked pair:\n{env_out}"
    );
    assert!(
        env_out.contains("1 keys (values hidden)"),
        "agent env must report the count:\n{env_out}"
    );
}

/// Assert no file anywhere under `root` contains the canary. A missing tree is
/// vacuously clean (a run that produced no logs cannot have leaked into them).
fn assert_clean_tree(root: &Path, what: &str) {
    if !root.exists() {
        return;
    }
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if let Ok(body) = std::fs::read(&path) {
                assert!(
                    !String::from_utf8_lossy(&body).contains(SECRET),
                    "the env value leaked into {what}: {}",
                    path.display()
                );
            }
        }
    }
}

/// Run the real `ainb` CLI against the temp Hangar home and return its stdout.
fn run_ainb(bin: &Path, home: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new(bin)
        .args(args)
        .env("AINB_HANGAR_HOME", home)
        .output()
        .unwrap_or_else(|e| panic!("run ainb {args:?}: {e}"));
    assert!(
        out.status.success(),
        "ainb {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).to_string()
}

/// Write an executable shell script under `dir`.
fn write_executable(dir: &Path, name: &str, body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt as _;
    let path = dir.join(name);
    std::fs::write(&path, body).expect("write script");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    path
}

/// Open a pool against the tripwire's temp DB file.
async fn open_pool(db_path: &Path) -> SqlitePool {
    SqlitePoolOptions::new()
        .max_connections(4)
        .connect(&format!("sqlite://{}?mode=rwc", db_path.display()))
        .await
        .expect("open pool")
}
