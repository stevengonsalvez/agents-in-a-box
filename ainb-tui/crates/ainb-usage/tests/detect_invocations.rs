//! Integration tests for `detect_invocations` (bead v12.B.2).
//!
//! Every test builds a fixture *tool home* under a `tempfile` tempdir
//! that mimics the on-disk layout of a real tool (e.g. Claude's
//! `~/.claude/projects/**/*.jsonl`). No test touches a real `~/.claude`
//! or `~/.codex` — `detect_invocations` takes the tool home as an
//! explicit path, so the fixtures are fully sandboxed.

use std::fs;
use std::path::Path;

use ainb_usage::{detect_invocations, InvocationRecord};

/// Write `contents` to `<tool_home>/<rel>`, creating parent dirs.
fn seed(tool_home: &Path, rel: &str, contents: &str) {
    let path = tool_home.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

#[test]
fn counts_unit_across_nested_session_logs() {
    let home = tempfile::tempdir().unwrap();
    // Two project dirs, mimicking ~/.claude/projects/<slug>/*.jsonl.
    seed(
        home.path(),
        "projects/proj-a/2026-01.jsonl",
        "{\"timestamp\":\"2026-01-15T10:00:00Z\",\"content\":\"<command-name>commit</command-name>\"}\n\
         {\"timestamp\":\"2026-01-16T10:00:00Z\",\"content\":\"<command-name>commit</command-name>\"}\n\
         {\"timestamp\":\"2026-01-16T10:00:00Z\",\"content\":\"<command-name>reflect</command-name>\"}\n",
    );
    seed(
        home.path(),
        "projects/proj-b/2026-02.jsonl",
        "{\"type\":\"tool_use\",\"name\":\"Skill\",\"input\":{\"skill\":\"commit\"},\"timestamp\":\"2026-02-01T09:30:00Z\"}\n",
    );

    let rec = detect_invocations(home.path(), "commit");
    assert_eq!(rec.invocations, 3, "two command-name + one Skill tool_use");
    assert_eq!(
        rec.last_used_at.as_deref(),
        Some("2026-02-01T09:30:00Z"),
        "latest timestamp across all logs"
    );
}

#[test]
fn unknown_unit_returns_empty_record() {
    let home = tempfile::tempdir().unwrap();
    seed(
        home.path(),
        "projects/p/log.jsonl",
        "{\"timestamp\":\"2026-01-15T10:00:00Z\",\"content\":\"<command-name>commit</command-name>\"}\n",
    );

    let rec = detect_invocations(home.path(), "does-not-exist");
    assert_eq!(rec, InvocationRecord::default());
    assert_eq!(rec.invocations, 0);
    assert_eq!(rec.last_used_at, None);
}

#[test]
fn missing_tool_home_is_empty_not_panic() {
    let home = tempfile::tempdir().unwrap();
    let absent = home.path().join("nope-no-such-dir");
    let rec = detect_invocations(&absent, "commit");
    assert_eq!(rec, InvocationRecord::default());
}

#[test]
fn ignores_non_jsonl_and_unrelated_lines() {
    let home = tempfile::tempdir().unwrap();
    // A non-jsonl file carrying a *real* `<command-name>` marker must be
    // excluded by extension alone — proving the filter, not the heuristic.
    seed(
        home.path(),
        "projects/p/notes.md",
        "{\"timestamp\":\"2026-01-15T10:00:00Z\",\"content\":\"<command-name>commit</command-name>\"}\n",
    );
    // A jsonl line with no command/skill marker must NOT be counted.
    seed(
        home.path(),
        "projects/p/log.jsonl",
        "{\"timestamp\":\"2026-01-15T10:00:00Z\",\"content\":\"plain text mentioning commit\"}\n",
    );
    let rec = detect_invocations(home.path(), "commit");
    assert_eq!(rec.invocations, 0);
    assert_eq!(rec.last_used_at, None);
}

#[test]
fn record_converts_into_lockfile_usage_record() {
    // B.3 (the CLI writer) needs to drop a detected record straight into
    // the lockfile's UsageRecord, so the conversion must preserve both
    // fields verbatim.
    let rec = InvocationRecord {
        invocations: 7,
        last_used_at: Some("2026-05-01T00:00:00Z".into()),
    };
    let usage: ainb_skill_core::UsageRecord = rec.clone().into();
    assert_eq!(usage.invocations, 7);
    assert_eq!(usage.last_used_at.as_deref(), Some("2026-05-01T00:00:00Z"));

    // Empty detection maps to an empty (omitted) usage record.
    let empty: ainb_skill_core::UsageRecord = InvocationRecord::default().into();
    assert!(empty.is_empty());
}
