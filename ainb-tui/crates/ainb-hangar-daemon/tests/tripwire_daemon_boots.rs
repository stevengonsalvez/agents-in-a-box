//! Boot tripwire for the `ainb-hangar-daemon` binary.
//!
//! Proves the P0 user-visible end state: `--version` prints the expected
//! string, and `--once` boots the persistence layer (applying every migration)
//! against an isolated `$HOME`, leaves a `hangar.db` containing all 14 v1
//! tables, and exits 0.
//!
//! Both tests redirect `AINB_HANGAR_HOME` to a tempdir via the store's
//! `with_isolated_home` helper, holding `ENV_LOCK` for the duration so the suite
//! stays correct under parallel `cargo test`.

use std::path::Path;
use std::process::Command;

use ainb_hangar_store::test_support::with_isolated_home;
use assert_cmd::prelude::*;

/// The 14 v1 tables every later phase plugs into. Order is the migration order;
/// the assertion is set-membership, not order-sensitive.
const EXPECTED_TABLES: &[&str] = &[
    "workspace",
    "user",
    "member",
    "agent",
    "agent_runtime",
    "issue",
    "comment",
    "agent_task_queue",
    "skill",
    "skill_file",
    "agent_skill",
    "pat",
    "daemon_token",
    "beads_mapping",
];

#[test]
fn daemon_version_flag_prints_expected_string() {
    let output = Command::cargo_bin("ainb-hangar-daemon")
        .expect("binary builds")
        .arg("--version")
        .output()
        .expect("run --version");

    assert!(
        output.status.success(),
        "--version should exit 0, got {:?}",
        output.status
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let expected = format!("ainb-hangar-daemon {}", env!("CARGO_PKG_VERSION"));
    assert!(
        stdout.contains(&expected),
        "expected stdout to contain {expected:?}, got {stdout:?}"
    );
}

#[test]
fn daemon_boots_to_idle_and_creates_db_file() {
    with_isolated_home(|home: &Path| {
        let output = Command::cargo_bin("ainb-hangar-daemon")
            .expect("binary builds")
            .arg("--once")
            .env("AINB_HANGAR_HOME", home)
            .output()
            .expect("run --once");

        assert!(
            output.status.success(),
            "--once should exit 0, stderr={:?}",
            String::from_utf8_lossy(&output.stderr)
        );

        let db_path = home.join("hangar.db");
        assert!(
            db_path.exists(),
            "expected hangar.db at {db_path:?} after boot"
        );

        let names = table_names(&db_path);
        for table in EXPECTED_TABLES {
            assert!(
                names.iter().any(|n| n == table),
                "expected table {table:?} in schema, got {names:?}"
            );
        }
    });
}

/// Read every user-table name from a `SQLite` file via the `sqlite3` CLI.
///
/// Uses the CLI rather than a second sqlx pool so the assertion observes exactly
/// what an operator running `sqlite3 hangar.db ".schema"` would see (the
/// acceptance-criteria proof in `docs/hangar/phases/P0.md`).
fn table_names(db_path: &Path) -> Vec<String> {
    let output = Command::new("sqlite3")
        .arg(db_path)
        .arg("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' AND name NOT LIKE '\\_sqlx%' ESCAPE '\\';")
        .output()
        .expect("run sqlite3");
    assert!(
        output.status.success(),
        "sqlite3 query failed: {:?}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).lines().map(str::to_string).collect()
}
