//! End-to-end integration tests for the `ainb hangar <verb>` CLI namespace.
//!
//! Drives the real `ainb` binary (located via `CARGO_BIN_EXE_ainb`) against an
//! isolated `$AINB_HANGAR_HOME` so each test owns an ephemeral `hangar.db`. The
//! store resolves `$AINB_HANGAR_HOME` as the directory that DIRECTLY holds the
//! database (no `.ainb` segment), so a per-test tempdir keeps the real
//! `~/.ainb/hangar.db` untouched.
//!
//! Covered:
//! * `ainb hangar --help` lists every wired noun group.
//! * `ainb hangar issue create` then `ainb hangar issue list` shows the issue
//!   (round-trip through a fresh, auto-bootstrapped workspace + migrated db).
//! * `ainb hangar issue show <id>` round-trips the created issue.
//! * `ainb hangar daemon status` reports the database reachable.
//! * `ainb hangar task list` on an empty db is a clean no-op (exit 0).

use std::path::PathBuf;
use std::process::Command;

fn ainb_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ainb"))
}

/// Run `ainb <args>` with an isolated hangar home. Returns (success, stdout).
fn run(home: &std::path::Path, args: &[&str]) -> (bool, String) {
    let out = Command::new(ainb_bin())
        .args(args)
        .env("AINB_HANGAR_HOME", home)
        // Keep any HOME-scoped lookups off the real home too.
        .env("HOME", home)
        .output()
        .expect("spawn ainb");
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    (out.status.success(), format!("{stdout}{stderr}"))
}

#[test]
fn hangar_help_lists_wired_verbs() {
    let tmp = tempfile::tempdir().unwrap();
    let (ok, out) = run(tmp.path(), &["hangar", "--help"]);
    assert!(ok, "hangar --help should exit 0; out={out}");
    for verb in ["issue", "task", "beads", "daemon"] {
        assert!(out.contains(verb), "hangar --help missing '{verb}':\n{out}");
    }
}

#[test]
fn issue_create_then_list_shows_it() {
    let tmp = tempfile::tempdir().unwrap();

    let (ok, out) = run(
        tmp.path(),
        &["hangar", "issue", "create", "--title", "Tripwire issue"],
    );
    assert!(ok, "issue create should exit 0; out={out}");
    assert!(out.contains("created issue"), "missing create ack:\n{out}");

    let (ok, out) = run(tmp.path(), &["hangar", "issue", "list"]);
    assert!(ok, "issue list should exit 0; out={out}");
    assert!(
        out.contains("Tripwire issue"),
        "created issue not in list:\n{out}"
    );
}

#[test]
fn issue_show_round_trips_created_issue() {
    let tmp = tempfile::tempdir().unwrap();

    let (ok, create_out) = run(
        tmp.path(),
        &["hangar", "issue", "create", "--title", "Showable"],
    );
    assert!(ok, "create failed: {create_out}");
    // "created issue <id>" — pull the id token.
    let id = create_out
        .lines()
        .find_map(|l| l.strip_prefix("created issue "))
        .map(|s| s.trim().to_string())
        .expect("create output carries an id");

    let (ok, out) = run(tmp.path(), &["hangar", "issue", "show", &id]);
    assert!(ok, "issue show should exit 0; out={out}");
    assert!(out.contains(&id), "show output missing id:\n{out}");
    assert!(out.contains("Showable"), "show output missing title:\n{out}");
}

#[test]
fn issue_list_json_format_emits_array() {
    let tmp = tempfile::tempdir().unwrap();
    run(
        tmp.path(),
        &["hangar", "issue", "create", "--title", "Jsonable"],
    );
    let (ok, out) = run(tmp.path(), &["--format", "json", "hangar", "issue", "list"]);
    assert!(ok, "json issue list should exit 0; out={out}");
    assert!(out.trim_start().starts_with('['), "expected JSON array:\n{out}");
    assert!(out.contains("\"title\":\"Jsonable\""), "json missing title:\n{out}");
}

#[test]
fn daemon_status_reports_reachable() {
    let tmp = tempfile::tempdir().unwrap();
    let (ok, out) = run(tmp.path(), &["hangar", "daemon", "status"]);
    assert!(ok, "daemon status should exit 0; out={out}");
    assert!(out.contains("reachable"), "status not reachable:\n{out}");
}

#[test]
fn task_list_on_empty_db_is_clean_noop() {
    let tmp = tempfile::tempdir().unwrap();
    let (ok, out) = run(tmp.path(), &["hangar", "task", "list"]);
    assert!(ok, "task list on empty db should exit 0; out={out}");
    assert!(out.contains("no pending tasks"), "expected empty marker:\n{out}");
}

#[test]
fn issue_list_all_four_formats_are_distinct() {
    let tmp = tempfile::tempdir().unwrap();
    // Title carries a comma so CSV quoting is exercised.
    let (ok, _) = run(
        tmp.path(),
        &["hangar", "issue", "create", "--title", "Wire, up payments"],
    );
    assert!(ok, "create should exit 0");

    let text = run(tmp.path(), &["hangar", "issue", "list", "--format", "text"]).1;
    let json = run(tmp.path(), &["hangar", "issue", "list", "--format", "json"]).1;
    let csv = run(tmp.path(), &["hangar", "issue", "list", "--format", "csv"]).1;
    let md = run(tmp.path(), &["hangar", "issue", "list", "--format", "markdown"]).1;

    // Every format must produce DISTINCT output (the regression that let
    // csv/markdown silently alias text through a `_ =>` catch-all).
    let all = [&text, &json, &csv, &md];
    for (a, b) in [(0, 1), (0, 2), (0, 3), (1, 2), (1, 3), (2, 3)] {
        assert_ne!(all[a], all[b], "formats {a} and {b} produced identical output");
    }

    // Format-specific markers.
    assert!(json.trim_start().starts_with('['), "json not an array:\n{json}");
    assert!(
        csv.contains("id,state,title,description,created_at"),
        "csv header missing:\n{csv}"
    );
    assert!(
        csv.contains("\"Wire, up payments\""),
        "csv must quote the comma-bearing title:\n{csv}"
    );
    assert!(md.contains("| --- |"), "markdown separator row missing:\n{md}");
    assert!(md.contains("| state |"), "markdown header missing:\n{md}");
}
