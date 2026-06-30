//! P8.1 observability tripwire: the daemon installs a `tracing` subscriber that
//! writes structured JSON lines to `~/.agents-in-a-box/hangar/logs/daemon.jsonl`.
//!
//! Spawns the `ainb-hangar-daemon` binary in one-shot mode against an isolated
//! `$AINB_HANGAR_HOME` tempdir, with `RUST_LOG=info` and a test-only env seam
//! (`AINB_HANGAR_BOOT_TASK_ID`) that makes `boot()` emit exactly one
//! `tracing::info!(task_id = …, "boot")` event. It then asserts the rolling
//! JSONL sink laid down a `daemon.jsonl` file containing exactly one line whose
//! `task_id` matches and whose `level` is `INFO`, and that the line is valid
//! JSON (the CLI/TUI logs-tail surfaces in P8.6 parse it back).
//!
//! Before the subscriber is wired this test is RED: no `daemon.jsonl` is ever
//! written, so the file-exists assertion fails.

use std::path::Path;
use std::process::Command;

use ainb_hangar_store::test_support::with_isolated_home;
use assert_cmd::prelude::*;

/// The rolling appender writes `daemon.<YYYY-MM-DD>` files under the log dir.
/// The first segment is the prefix the test greps for; the daily suffix is
/// resolved at runtime, so the test globs the directory rather than guessing the
/// date (and stays correct across a midnight rollover during the run).
const LOG_PREFIX: &str = "daemon";

#[test]
fn subscriber_writes_one_json_line_per_event() {
    with_isolated_home(|home: &Path| {
        let output = Command::cargo_bin("ainb-hangar-daemon")
            .expect("binary builds")
            .arg("--once")
            .env("AINB_HANGAR_HOME", home)
            .env("RUST_LOG", "info")
            .env("AINB_HANGAR_BOOT_TASK_ID", "t-1")
            .output()
            .expect("run --once");

        assert!(
            output.status.success(),
            "--once should exit 0, stderr={:?}",
            String::from_utf8_lossy(&output.stderr)
        );

        let log_dir = home.join("hangar").join("logs");
        let log_file = find_log_file(&log_dir);

        let contents =
            std::fs::read_to_string(&log_file).unwrap_or_else(|e| panic!("read {log_file:?}: {e}"));

        let boot_lines: Vec<serde_json::Value> = contents
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| {
                serde_json::from_str::<serde_json::Value>(l)
                    .unwrap_or_else(|e| panic!("line is not valid JSON ({e}): {l:?}"))
            })
            .filter(|v| {
                v.pointer("/fields/task_id").and_then(serde_json::Value::as_str) == Some("t-1")
            })
            .collect();

        assert_eq!(
            boot_lines.len(),
            1,
            "expected exactly one JSON line with task_id == \"t-1\", file contents:\n{contents}"
        );

        let line = &boot_lines[0];
        assert_eq!(
            line.get("level").and_then(serde_json::Value::as_str),
            Some("INFO"),
            "boot event level should be INFO, got {line:?}"
        );
        assert_eq!(
            line.pointer("/fields/message").and_then(serde_json::Value::as_str),
            Some("boot"),
            "boot event message should be \"boot\", got {line:?}"
        );
    });
}

/// Locate the rolling appender's current file under `log_dir`.
///
/// `tracing_appender::rolling` writes `daemon.<date>` (daily rotation), so the
/// exact filename is date-dependent. Returns the single matching file, panicking
/// with the directory listing if zero or many are found.
fn find_log_file(log_dir: &Path) -> std::path::PathBuf {
    let dir_display = log_dir.display();
    let entries: Vec<std::path::PathBuf> = std::fs::read_dir(log_dir)
        .unwrap_or_else(|e| panic!("log dir {dir_display} not created by daemon: {e}"))
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with(LOG_PREFIX))
        })
        .collect();

    assert_eq!(
        entries.len(),
        1,
        "expected exactly one {LOG_PREFIX}.* log file in {dir_display}, found {entries:?}"
    );
    entries.into_iter().next().expect("checked len == 1")
}
