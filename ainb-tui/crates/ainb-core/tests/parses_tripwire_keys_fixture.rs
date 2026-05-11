//! Smoke test for the deterministic tripwire_keys fixture
//! (plan §Phase 7).
//!
//! Walks the on-disk fixture, parses claude / codex JSONL with the
//! same field shapes the production parsers consume, and asserts
//! documented per-project per-period token totals. Bucketing happens
//! against FIXTURE_NOW in pure UTC so the test stays timezone-stable
//! independent of the host clock.
//!
//! If you intentionally regenerate the fixture and the schema drifts,
//! re-run the constant-discovery procedure: replace every `EXPECTED_*`
//! with `u64::MAX`, run the test, copy the actual numbers from the
//! failure output, then lock the new constants. Do not hand-edit
//! constants without a generator run behind them.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use serde::Deserialize;

const SCHEMA_VERSION: u32 = 1;
const FIXTURE_NOW_RFC3339: &str = "2026-05-11T00:00:00Z";

// Per-project token totals by period (sum of `input_tokens` across
// claude+codex sessions inside the period window). Derived from the
// generator design — re-verified by running the test once with
// `u64::MAX` placeholders. Each row is monotonically increasing.
//
// alpha=scale 1, beta=2, gamma=3. Project rows differ by a constant
// factor so chip values are byte-diffable across projects too.
struct ExpectedTotals {
    today: u64,
    week: u64,
    month: u64,
    quarter: u64,
    ytd: u64,
    all: u64,
}

const ALPHA_TOTALS: ExpectedTotals = ExpectedTotals {
    today: 400,
    week: 1200,
    month: 2400,
    quarter: 3200,
    ytd: 3600,
    all: 4000,
};
const BETA_TOTALS: ExpectedTotals = ExpectedTotals {
    today: 800,
    week: 2400,
    month: 4800,
    quarter: 6400,
    ytd: 7200,
    all: 8000,
};
const GAMMA_TOTALS: ExpectedTotals = ExpectedTotals {
    today: 1200,
    week: 3600,
    month: 7200,
    quarter: 9600,
    ytd: 10800,
    all: 12000,
};

const EXPECTED_CLAUDE_FILES: usize = 60;
const EXPECTED_CODEX_FILES: usize = 30;

#[derive(Debug, Clone, Copy)]
struct Call {
    project: &'static str,
    timestamp: DateTime<Utc>,
    input_tokens: u64,
}

fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("tripwire_keys")
}

fn fixture_now() -> DateTime<Utc> {
    let raw = fs::read_to_string(fixture_root().join("FIXTURE_NOW.txt"))
        .expect("FIXTURE_NOW.txt present");
    DateTime::parse_from_rfc3339(raw.trim())
        .expect("FIXTURE_NOW parses as RFC 3339")
        .with_timezone(&Utc)
}

fn project_from_dirname(dirname: &str) -> &'static str {
    // Dir names are `-Users-test-proj-<project>`. Map back to the
    // canonical alpha/beta/gamma identifier so the test constants stay
    // readable.
    match dirname {
        "-Users-test-proj-alpha" => "alpha",
        "-Users-test-proj-beta" => "beta",
        "-Users-test-proj-gamma" => "gamma",
        _ => panic!("unexpected claude project dir: {dirname}"),
    }
}

fn project_from_cwd(cwd: &str) -> &'static str {
    match cwd {
        "/Users/test/proj-alpha" => "alpha",
        "/Users/test/proj-beta" => "beta",
        "/Users/test/proj-gamma" => "gamma",
        _ => panic!("unexpected codex cwd: {cwd}"),
    }
}

#[derive(Deserialize)]
struct ClaudeLine {
    #[serde(rename = "type")]
    msg_type: String,
    timestamp: Option<String>,
    message: Option<ClaudeMessage>,
}

#[derive(Deserialize)]
struct ClaudeMessage {
    usage: Option<ClaudeUsage>,
}

#[derive(Deserialize)]
struct ClaudeUsage {
    input_tokens: Option<u64>,
}

fn parse_claude(root: &Path) -> (usize, Vec<Call>) {
    let mut files = 0;
    let mut calls = Vec::new();
    for project_entry in fs::read_dir(root).expect("claude projects root") {
        let project_entry = project_entry.unwrap();
        if !project_entry.file_type().unwrap().is_dir() {
            continue;
        }
        let dirname = project_entry.file_name().into_string().unwrap();
        let project = project_from_dirname(&dirname);
        for session_entry in fs::read_dir(project_entry.path()).unwrap() {
            let session_entry = session_entry.unwrap();
            let path = session_entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("jsonl") {
                continue;
            }
            files += 1;
            let content = fs::read_to_string(&path).unwrap();
            for line in content.lines() {
                if line.is_empty() {
                    continue;
                }
                let parsed: ClaudeLine = serde_json::from_str(line)
                    .unwrap_or_else(|e| panic!("invalid claude jsonl line in {path:?}: {e}"));
                if parsed.msg_type != "assistant" {
                    continue;
                }
                let usage =
                    parsed.message.and_then(|m| m.usage).expect("assistant line carries usage");
                let input = usage.input_tokens.expect("usage.input_tokens present");
                let ts = parse_ts(&parsed.timestamp.expect("assistant has timestamp"));
                calls.push(Call {
                    project,
                    timestamp: ts,
                    input_tokens: input,
                });
            }
        }
    }
    (files, calls)
}

#[derive(Deserialize)]
struct CodexEntry {
    #[serde(rename = "type")]
    entry_type: String,
    timestamp: Option<String>,
    payload: Option<CodexPayload>,
}

#[derive(Deserialize)]
struct CodexPayload {
    #[serde(rename = "type")]
    payload_type: Option<String>,
    originator: Option<String>,
    cwd: Option<String>,
    info: Option<CodexInfo>,
}

#[derive(Deserialize)]
struct CodexInfo {
    last_token_usage: Option<CodexTokens>,
}

#[derive(Deserialize)]
struct CodexTokens {
    input_tokens: Option<u64>,
}

fn parse_codex(root: &Path) -> (usize, Vec<Call>) {
    let mut files = 0;
    let mut calls = Vec::new();
    let years = match fs::read_dir(root) {
        Ok(d) => d,
        Err(_) => return (0, calls),
    };
    for year in years.flatten() {
        for month in fs::read_dir(year.path()).unwrap().flatten() {
            for day in fs::read_dir(month.path()).unwrap().flatten() {
                for file in fs::read_dir(day.path()).unwrap().flatten() {
                    let path = file.path();
                    let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
                    if !name.starts_with("rollout-") || !name.ends_with(".jsonl") {
                        continue;
                    }
                    files += 1;
                    let content = fs::read_to_string(&path).unwrap();
                    let mut session_project: Option<&'static str> = None;
                    for line in content.lines() {
                        if line.is_empty() {
                            continue;
                        }
                        let entry: CodexEntry = serde_json::from_str(line)
                            .unwrap_or_else(|e| panic!("invalid codex jsonl in {path:?}: {e}"));
                        match entry.entry_type.as_str() {
                            "session_meta" => {
                                let payload = entry.payload.expect("session_meta has payload");
                                assert_eq!(
                                    payload.originator.as_deref(),
                                    Some("codex"),
                                    "codex session_meta originator"
                                );
                                let cwd = payload.cwd.expect("session_meta has cwd");
                                session_project = Some(project_from_cwd(&cwd));
                            }
                            "event_msg" => {
                                let Some(payload) = entry.payload else {
                                    continue;
                                };
                                if payload.payload_type.as_deref() != Some("token_count") {
                                    continue;
                                }
                                let Some(info) = payload.info else { continue };
                                let Some(last) = info.last_token_usage else {
                                    continue;
                                };
                                let input = last.input_tokens.unwrap_or(0);
                                if input == 0 {
                                    continue;
                                }
                                let ts =
                                    parse_ts(&entry.timestamp.expect("event_msg has timestamp"));
                                let project = session_project
                                    .expect("token_count appears after session_meta");
                                calls.push(Call {
                                    project,
                                    timestamp: ts,
                                    input_tokens: input,
                                });
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
    }
    (files, calls)
}

fn parse_ts(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s)
        .unwrap_or_else(|e| panic!("bad timestamp {s:?}: {e}"))
        .with_timezone(&Utc)
}

fn bucket_totals(now: DateTime<Utc>, calls: &[Call]) -> BTreeMap<&'static str, ExpectedTotals> {
    let today = now.date_naive();
    let mut totals: BTreeMap<&'static str, ExpectedTotals> = BTreeMap::new();
    for project in ["alpha", "beta", "gamma"] {
        totals.insert(
            project,
            ExpectedTotals {
                today: 0,
                week: 0,
                month: 0,
                quarter: 0,
                ytd: 0,
                all: 0,
            },
        );
    }
    let jan1 = NaiveDate::from_ymd_opt(today.year_using_chrono(), 1, 1).unwrap();
    for call in calls {
        let date = call.timestamp.date_naive();
        let days_back = (today - date).num_days();
        let bucket = totals.get_mut(call.project).expect("known project");
        bucket.all += call.input_tokens;
        if date >= jan1 {
            bucket.ytd += call.input_tokens;
        }
        if (0..90).contains(&days_back) {
            bucket.quarter += call.input_tokens;
        }
        if (0..30).contains(&days_back) {
            bucket.month += call.input_tokens;
        }
        if (0..7).contains(&days_back) {
            bucket.week += call.input_tokens;
        }
        if days_back == 0 {
            bucket.today += call.input_tokens;
        }
    }
    totals
}

// Local trait-alike to avoid a `chrono::Datelike` import collision with
// `Datelike` already brought into scope. Inline helper keeps the test
// file dependency surface tiny.
trait YearShim {
    fn year_using_chrono(&self) -> i32;
}
impl YearShim for NaiveDate {
    fn year_using_chrono(&self) -> i32 {
        use chrono::Datelike;
        self.year()
    }
}

fn assert_totals_eq(
    label: &str,
    project: &str,
    actual: &ExpectedTotals,
    expected: &ExpectedTotals,
) {
    assert_eq!(actual.today, expected.today, "{label} {project} today");
    assert_eq!(actual.week, expected.week, "{label} {project} week");
    assert_eq!(actual.month, expected.month, "{label} {project} month");
    assert_eq!(
        actual.quarter, expected.quarter,
        "{label} {project} quarter"
    );
    assert_eq!(actual.ytd, expected.ytd, "{label} {project} ytd");
    assert_eq!(actual.all, expected.all, "{label} {project} all");
}

#[test]
fn fixture_now_pinned_to_2026_05_11() {
    assert_eq!(
        fixture_now(),
        Utc.with_ymd_and_hms(2026, 5, 11, 0, 0, 0).unwrap(),
    );
}

#[test]
fn schema_version_matches_test_expectations() {
    let raw = fs::read_to_string(fixture_root().join("SCHEMA_VERSION.txt"))
        .expect("SCHEMA_VERSION.txt present");
    let actual: u32 = raw.trim().parse().expect("schema version parses");
    assert_eq!(
        actual, SCHEMA_VERSION,
        "Fixture schema drifted — re-verify the test constants and bump SCHEMA_VERSION here."
    );
}

#[test]
fn file_counts_match_design() {
    let claude_root = fixture_root().join("claude").join("projects");
    let codex_root = fixture_root().join("codex").join("sessions");
    let (claude_files, _) = parse_claude(&claude_root);
    let (codex_files, _) = parse_codex(&codex_root);
    assert_eq!(claude_files, EXPECTED_CLAUDE_FILES);
    assert_eq!(codex_files, EXPECTED_CODEX_FILES);
}

#[test]
fn per_project_per_period_totals_match_documented_constants() {
    let claude_root = fixture_root().join("claude").join("projects");
    let codex_root = fixture_root().join("codex").join("sessions");
    let (_, mut calls) = parse_claude(&claude_root);
    let (_, codex_calls) = parse_codex(&codex_root);
    calls.extend(codex_calls);

    let totals = bucket_totals(fixture_now(), &calls);
    assert_totals_eq(
        "alpha-totals",
        "alpha",
        totals.get("alpha").unwrap(),
        &ALPHA_TOTALS,
    );
    assert_totals_eq(
        "beta-totals",
        "beta",
        totals.get("beta").unwrap(),
        &BETA_TOTALS,
    );
    assert_totals_eq(
        "gamma-totals",
        "gamma",
        totals.get("gamma").unwrap(),
        &GAMMA_TOTALS,
    );

    for (project, t) in totals.iter() {
        assert!(t.today < t.week, "{project}: today < 7d");
        assert!(t.week < t.month, "{project}: 7d < 30d");
        assert!(t.month < t.quarter, "{project}: 30d < 90d");
        assert!(t.quarter < t.ytd, "{project}: 90d < ytd");
        assert!(t.ytd < t.all, "{project}: ytd < all");
    }
}

#[test]
fn period_chip_values_distinct_across_projects() {
    // Byte-diffable invariant: no two projects share a chip value, so
    // capture-pane diffs in Phase 8 can attribute the change to the
    // right row.
    let all = [&ALPHA_TOTALS, &BETA_TOTALS, &GAMMA_TOTALS];
    for axis in ["today", "week", "month", "quarter", "ytd", "all"] {
        let values: Vec<u64> = all
            .iter()
            .map(|t| match axis {
                "today" => t.today,
                "week" => t.week,
                "month" => t.month,
                "quarter" => t.quarter,
                "ytd" => t.ytd,
                "all" => t.all,
                _ => unreachable!(),
            })
            .collect();
        let mut sorted = values.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(
            sorted.len(),
            values.len(),
            "period {axis} has duplicate project totals: {values:?}"
        );
    }
}
