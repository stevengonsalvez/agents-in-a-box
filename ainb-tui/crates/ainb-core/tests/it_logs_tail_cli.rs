//! CLI surface test for `ainb hangar logs tail` (P8.6, CLI leg).
//!
//! Drives the real `ainb` binary (via `CARGO_BIN_EXE_ainb`) against an isolated
//! `$AINB_HANGAR_HOME` tempdir. Seeds the daemon's P8.1-shaped structured log
//! file at `<home>/hangar/logs/daemon.<date>` with three known JSON lines (the
//! `tracing_subscriber::fmt().json()` wire shape: top-level `timestamp`/`level`/
//! `target`, event fields nested under `fields`), runs
//! `ainb hangar logs tail --no-follow`, and asserts every line's message +
//! fields surface in the pretty-printed stdout.
//!
//! Per the P8.6 reconciliation: the file is `daemon.<date>` (daily rotation),
//! **never** a literal `daemon.jsonl`. `--no-follow` bounds the run so the test
//! never hangs on a tail-follow loop.

use std::path::{Path, PathBuf};
use std::process::Command;

fn ainb_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ainb"))
}

/// Seed three P8.1-shaped JSON log lines into `<home>/hangar/logs/daemon.<date>`.
///
/// The filename uses a fixed date suffix (matching the daily-rotation prefix
/// `daemon`); the reader globs `daemon.*` so the exact date is irrelevant.
fn seed_logs(home: &Path) -> PathBuf {
    let log_dir = home.join("hangar").join("logs");
    std::fs::create_dir_all(&log_dir).expect("create log dir");
    let file = log_dir.join("daemon.2026-05-31");
    let lines = [
        r#"{"timestamp":"2026-05-31T12:00:00.000001Z","level":"INFO","target":"ainb_hangar_daemon","fields":{"message":"daemon ready","task_id":"t-aaa"}}"#,
        r#"{"timestamp":"2026-05-31T12:00:01.000002Z","level":"WARN","target":"ainb_hangar_daemon::run_loop","fields":{"message":"claim slot retry","attempts":2}}"#,
        r#"{"timestamp":"2026-05-31T12:00:02.000003Z","level":"ERROR","target":"ainb_hangar_daemon::runner","fields":{"message":"provider exited nonzero","code":7}}"#,
    ];
    std::fs::write(&file, format!("{}\n", lines.join("\n"))).expect("seed log file");
    file
}

fn run(home: &Path, args: &[&str]) -> (bool, String) {
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

#[test]
fn logs_tail_no_follow_prints_all_three_lines() {
    let tmp = tempfile::tempdir().unwrap();
    seed_logs(tmp.path());

    let (ok, out) = run(tmp.path(), &["hangar", "logs", "tail", "--no-follow"]);
    assert!(ok, "logs tail --no-follow should exit 0, output:\n{out}");

    // Every seeded event's message surfaces, pretty-printed.
    assert!(
        out.contains("daemon ready"),
        "line 1 message missing:\n{out}"
    );
    assert!(
        out.contains("claim slot retry"),
        "line 2 message missing:\n{out}"
    );
    assert!(
        out.contains("provider exited nonzero"),
        "line 3 message missing:\n{out}"
    );

    // Level tokens are surfaced.
    assert!(out.contains("INFO"), "INFO level missing:\n{out}");
    assert!(out.contains("WARN"), "WARN level missing:\n{out}");
    assert!(out.contains("ERROR"), "ERROR level missing:\n{out}");

    // The k=v field tail is rendered for at least one custom field.
    assert!(out.contains("task_id=t-aaa"), "field tail missing:\n{out}");
    assert!(
        out.contains("attempts=2"),
        "numeric field tail missing:\n{out}"
    );
}

#[test]
fn logs_tail_level_filter_drops_below_floor() {
    let tmp = tempfile::tempdir().unwrap();
    seed_logs(tmp.path());

    // Floor at WARN: the INFO line must drop, WARN + ERROR remain.
    let (ok, out) = run(
        tmp.path(),
        &["hangar", "logs", "tail", "--no-follow", "--level", "warn"],
    );
    assert!(ok, "logs tail --level warn should exit 0, output:\n{out}");
    assert!(
        !out.contains("daemon ready"),
        "INFO line should be filtered out at --level warn:\n{out}"
    );
    assert!(
        out.contains("claim slot retry"),
        "WARN line should remain:\n{out}"
    );
    assert!(
        out.contains("provider exited nonzero"),
        "ERROR line should remain:\n{out}"
    );
}

#[test]
fn logs_tail_missing_log_dir_is_not_an_error() {
    // No logs seeded at all: a fresh install with no daemon run yet.
    let tmp = tempfile::tempdir().unwrap();
    let (ok, out) = run(tmp.path(), &["hangar", "logs", "tail", "--no-follow"]);
    assert!(
        ok,
        "logs tail with no log dir should exit 0 (empty), output:\n{out}"
    );
}

#[test]
fn logs_tail_lines_bounds_output() {
    let tmp = tempfile::tempdir().unwrap();
    seed_logs(tmp.path());

    // --lines 1 keeps only the last (ERROR) line.
    let (ok, out) = run(
        tmp.path(),
        &["hangar", "logs", "tail", "--no-follow", "--lines", "1"],
    );
    assert!(ok, "logs tail --lines 1 should exit 0, output:\n{out}");
    assert!(
        out.contains("provider exited nonzero"),
        "last line should be kept at --lines 1:\n{out}"
    );
    assert!(
        !out.contains("daemon ready"),
        "earlier line should be dropped at --lines 1:\n{out}"
    );
}
