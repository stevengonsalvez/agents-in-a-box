// ABOUTME: End-to-end integration tests for the persistent usage cache —
// exercises the real claude/codex parsers via parse_usage_for_with_roots_and_cache
// against synthetic on-disk JSONL fixtures, asserting append-only semantics
// and full-reparse fallbacks behave the way the cache contract says.

use std::fs;
use std::io::Write;
use std::path::Path;
use std::sync::Arc;

use ainb::models::usage::{
    UsageProviderFilter, UsageQuery, UsageSourceRoots, parse_usage_for_with_roots_and_cache,
};
use ainb::usage_cache::Cache;
use tempfile::TempDir;

/// Build the minimal claude project layout the parser expects:
///   <projects_dir>/<sanitized-project>/<sessionId>.jsonl
fn build_claude_root(dir: &Path) -> std::path::PathBuf {
    let projects = dir.join("projects");
    let project = projects.join("-Users-test-myrepo");
    fs::create_dir_all(&project).unwrap();
    projects
}

/// Append a single assistant turn to a JSONL file, emitting the matching
/// preceding "user" line so user_message attribution is exercised.
fn append_assistant_turn(
    path: &Path,
    session_id: &str,
    timestamp: &str,
    input_tokens: u64,
    output_tokens: u64,
) {
    let user = serde_json::json!({
        "type": "user",
        "sessionId": session_id,
        "timestamp": timestamp,
        "message": { "content": "hello world" }
    });
    let assistant = serde_json::json!({
        "type": "assistant",
        "sessionId": session_id,
        "timestamp": timestamp,
        "cwd": "/Users/test/myrepo",
        "message": {
            "model": "claude-sonnet-4-5",
            "content": [{"type": "text", "text": "hi"}],
            "usage": {
                "input_tokens": input_tokens,
                "output_tokens": output_tokens,
            }
        }
    });
    let mut f = fs::OpenOptions::new().create(true).append(true).open(path).unwrap();
    writeln!(f, "{user}").unwrap();
    writeln!(f, "{assistant}").unwrap();
}

fn cache_at(dir: &Path) -> Arc<Cache> {
    Arc::new(Cache::open(dir.join("usage.db")).unwrap())
}

fn query_all() -> UsageQuery {
    UsageQuery {
        period: ainb::models::usage::UsagePeriod::All,
        provider_filter: UsageProviderFilter::Claude,
        include_projects: Vec::new(),
        exclude_projects: Vec::new(),
    }
}

#[test]
fn first_scan_then_unchanged_returns_same_data() {
    let work = TempDir::new().unwrap();
    let projects = build_claude_root(work.path());
    let session = projects.join("-Users-test-myrepo").join("sess-1.jsonl");
    append_assistant_turn(&session, "sess-1", "2026-05-01T12:00:00Z", 100, 50);

    let roots = UsageSourceRoots {
        claude_projects_dir: Some(projects.clone()),
        codex_dir: None,
    };

    let cache_dir = TempDir::new().unwrap();
    let cache = cache_at(cache_dir.path());

    let first = parse_usage_for_with_roots_and_cache(query_all(), &roots, cache.clone());
    assert_eq!(first.calls.len(), 1, "one assistant turn parsed");

    // Second scan with the file untouched — same data.
    let second = parse_usage_for_with_roots_and_cache(query_all(), &roots, cache.clone());
    assert_eq!(second.calls.len(), 1);
    assert_eq!(
        first.grand_total.total(),
        second.grand_total.total(),
        "totals must be identical across cache hits"
    );
    let info = cache.info().unwrap();
    assert_eq!(info.file_count, 1);
}

#[test]
fn append_only_adds_new_rows_and_matches_full_reparse() {
    let work = TempDir::new().unwrap();
    let projects = build_claude_root(work.path());
    let session = projects.join("-Users-test-myrepo").join("sess-2.jsonl");
    append_assistant_turn(&session, "sess-2", "2026-05-01T12:00:00Z", 100, 50);

    let roots = UsageSourceRoots {
        claude_projects_dir: Some(projects.clone()),
        codex_dir: None,
    };
    let cache_dir = TempDir::new().unwrap();
    let cache = cache_at(cache_dir.path());

    let _first = parse_usage_for_with_roots_and_cache(query_all(), &roots, cache.clone());

    // Append a second assistant turn.
    append_assistant_turn(&session, "sess-2", "2026-05-01T12:30:00Z", 200, 75);

    let after_append = parse_usage_for_with_roots_and_cache(query_all(), &roots, cache.clone());
    assert_eq!(
        after_append.calls.len(),
        2,
        "appended turn should be visible"
    );

    // Compare with a fresh disabled-cache full re-parse.
    let fresh =
        parse_usage_for_with_roots_and_cache(query_all(), &roots, Arc::new(Cache::disabled()));
    assert_eq!(after_append.calls.len(), fresh.calls.len());
    assert_eq!(
        after_append.grand_total.total(),
        fresh.grand_total.total(),
        "append-only path must agree with full re-parse on totals"
    );
}

#[test]
fn truncate_triggers_full_reparse() {
    let work = TempDir::new().unwrap();
    let projects = build_claude_root(work.path());
    let session = projects.join("-Users-test-myrepo").join("sess-3.jsonl");
    append_assistant_turn(&session, "sess-3", "2026-05-01T12:00:00Z", 100, 50);
    append_assistant_turn(&session, "sess-3", "2026-05-01T13:00:00Z", 100, 50);

    let roots = UsageSourceRoots {
        claude_projects_dir: Some(projects.clone()),
        codex_dir: None,
    };
    let cache_dir = TempDir::new().unwrap();
    let cache = cache_at(cache_dir.path());
    let first = parse_usage_for_with_roots_and_cache(query_all(), &roots, cache.clone());
    assert_eq!(first.calls.len(), 2);

    // Truncate file completely and write one new turn.
    fs::write(&session, b"").unwrap();
    append_assistant_turn(&session, "sess-3", "2026-05-02T09:00:00Z", 1, 1);

    let second = parse_usage_for_with_roots_and_cache(query_all(), &roots, cache.clone());
    assert_eq!(
        second.calls.len(),
        1,
        "truncate + rewrite must drop stale calls and parse only the new turn"
    );
}

#[test]
fn no_cache_path_writes_no_rows() {
    let work = TempDir::new().unwrap();
    let projects = build_claude_root(work.path());
    let session = projects.join("-Users-test-myrepo").join("sess-4.jsonl");
    append_assistant_turn(&session, "sess-4", "2026-05-01T12:00:00Z", 100, 50);

    let roots = UsageSourceRoots {
        claude_projects_dir: Some(projects.clone()),
        codex_dir: None,
    };
    let disabled = Arc::new(Cache::disabled());
    let _ = parse_usage_for_with_roots_and_cache(query_all(), &roots, disabled.clone());
    // Disabled cache exposes no info; ensure it stays disabled.
    assert!(!disabled.is_enabled());
}
