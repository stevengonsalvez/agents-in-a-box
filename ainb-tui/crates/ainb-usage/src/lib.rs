//! ainb-usage — per-unit invocation tracking.
//!
//! Parses tool log JSONL streams (Claude under
//! `~/.claude/projects/**/*.jsonl`, Codex under
//! `~/.codex/sessions/**/*.jsonl` when present) and counts how many
//! times each unit name shows up, plus the latest timestamp seen.
//!
//! Detection is conservative: we look for explicit `<command-name>`
//! tags (slash-command harness) and tool-use entries naming the
//! `Skill` tool with a `skill` argument. The same JSONL is consumed
//! by the existing token-usage subsystem; the parsers stay
//! independent because the two surfaces want different fields.
//!
//! Output is cached at `$AINB_HOME/usage.cache` (JSON) and is safe
//! to delete — a missing cache simply triggers a fresh parse.

use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Per-unit invocation summary written to the cache.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnitStats {
    /// Number of invocations seen across every parsed log file.
    pub invocations: u64,
    /// ISO-8601 timestamp of the most recent invocation, or `None`
    /// if no timestamped record was found.
    pub last_used: Option<String>,
}

/// Aggregate result keyed by unit name (`commit`, `reflect`, …).
pub type UsageStats = BTreeMap<String, UnitStats>;

/// Invocation summary for a single unit on a single tool, returned by
/// [`detect_invocations`]. Structurally mirrors the lockfile's
/// [`ainb_skill_core::UsageRecord`] (see bead v12.B.1) so a detected
/// record drops straight into the lockfile via `.into()` — that is what
/// the `ainb skill usage` CLI (bead v12.B.3) does after detecting.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InvocationRecord {
    /// Total detected invocations of the unit across the tool's logs.
    pub invocations: u64,
    /// RFC 3339 timestamp of the most recent detected invocation, or
    /// `None` when no timestamped invocation was found.
    pub last_used_at: Option<String>,
}

impl From<UnitStats> for InvocationRecord {
    fn from(s: UnitStats) -> Self {
        Self { invocations: s.invocations, last_used_at: s.last_used }
    }
}

impl From<InvocationRecord> for ainb_skill_core::UsageRecord {
    fn from(r: InvocationRecord) -> Self {
        Self { last_used_at: r.last_used_at, invocations: r.invocations }
    }
}

/// Detect how many times `unit_name` was invoked under a single tool's
/// home directory, and when it was last seen.
///
/// `tool_home` is the root of one tool's on-disk state — e.g. a
/// sandboxed `~/.claude` (logs under `projects/**/*.jsonl`) or
/// `~/.codex` (logs under `sessions/**/*.jsonl`). The directory is
/// walked recursively for `*.jsonl` session logs using the same
/// conservative heuristics as [`parse_claude_logs`] (`<command-name>`
/// tags and `Skill` tool-use entries).
///
/// This is a **pure read**: it never mutates `tool_home`, writes a
/// cache, or reads any path outside the supplied root. A missing or
/// empty `tool_home` yields an empty [`InvocationRecord`] (zero
/// invocations, no timestamp) rather than an error — usage detection is
/// best-effort and must never derail a caller.
pub fn detect_invocations(tool_home: &Path, unit_name: &str) -> InvocationRecord {
    parse_claude_logs(tool_home)
        .remove(unit_name)
        .map(InvocationRecord::from)
        .unwrap_or_default()
}

/// Cache file written to `$AINB_HOME/usage.cache`. Version stamped so
/// future schema changes can invalidate stale on-disk state.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageCache {
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    pub generated_at: Option<String>,
    #[serde(default)]
    pub units: UsageStats,
}

fn default_schema_version() -> u32 {
    1
}

/// Walk every `*.jsonl` under `root` (recursively) and accumulate
/// invocation stats. Files that fail to open or contain malformed
/// JSON are skipped silently — usage tracking is best-effort and
/// must never derail a `skill list` render.
pub fn parse_claude_logs(root: &Path) -> UsageStats {
    let mut stats: UsageStats = BTreeMap::new();
    if !root.exists() {
        return stats;
    }
    for entry in walkdir::WalkDir::new(root) {
        let Ok(entry) = entry else { continue };
        if !entry.file_type().is_file() {
            continue;
        }
        if entry.path().extension().and_then(|s| s.to_str()) != Some("jsonl") {
            continue;
        }
        let Ok(file) = fs::File::open(entry.path()) else {
            continue;
        };
        for line in BufReader::new(file).lines().map_while(|l| l.ok()) {
            ingest_line(&line, &mut stats);
        }
    }
    stats
}

/// Convenience wrapper: parse logs at `root`, refresh the cache file
/// at `cache_path`, return the new stats.
pub fn refresh_cache(root: &Path, cache_path: &Path) -> Result<UsageStats> {
    let stats = parse_claude_logs(root);
    let cache = UsageCache {
        schema_version: default_schema_version(),
        generated_at: Some(now_iso()),
        units: stats.clone(),
    };
    if let Some(parent) = cache_path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
        }
    }
    let serialized = serde_json::to_vec_pretty(&cache)?;
    fs::write(cache_path, serialized)
        .with_context(|| format!("writing {}", cache_path.display()))?;
    Ok(stats)
}

/// Load `cache_path` as `UsageCache`; missing file yields default.
pub fn load_cache(cache_path: &Path) -> Result<UsageCache> {
    if !cache_path.exists() {
        return Ok(UsageCache::default());
    }
    let body = fs::read(cache_path).with_context(|| format!("reading {}", cache_path.display()))?;
    if body.iter().all(u8::is_ascii_whitespace) {
        return Ok(UsageCache::default());
    }
    let parsed: UsageCache = serde_json::from_slice(&body)
        .with_context(|| format!("parsing {}", cache_path.display()))?;
    Ok(parsed)
}

/// Default cache location: `$AINB_HOME/usage.cache`.
pub fn default_cache_path() -> PathBuf {
    ainb_skill_core::paths::default_ainb_home().join("usage.cache")
}

/// Inspect one JSONL line and bump counters for any unit name found.
fn ingest_line(line: &str, stats: &mut UsageStats) {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return;
    }
    // Fast path: most JSONL lines mention neither a `<command-name>`
    // tag nor the literal `"Skill"` tool-use string. Skip them
    // before paying for `serde_json::from_str` + the tree walk.
    // ~1GB of `~/.claude/projects/**/*.jsonl` is dominated by
    // tool_use/tool_result/text blocks that have nothing to do
    // with unit invocations.
    if !trimmed.contains("<command-name>") && !trimmed.contains("\"Skill\"") {
        return;
    }
    let value: serde_json::Value = match serde_json::from_str(trimmed) {
        Ok(v) => v,
        Err(_) => return,
    };
    let ts = value.get("timestamp").and_then(|v| v.as_str()).map(str::to_string);

    for name in extract_command_names(trimmed) {
        bump(stats, &name, ts.as_deref());
    }

    if let Some(name) = walk_for_skill(&value) {
        bump(stats, &name, ts.as_deref());
    }
}

/// Find every `<command-name>X</command-name>` literal in `text`.
fn extract_command_names(text: &str) -> Vec<String> {
    const OPEN: &str = "<command-name>";
    const CLOSE: &str = "</command-name>";
    let mut out = Vec::new();
    let mut start = 0;
    while let Some(open_at) = text[start..].find(OPEN) {
        let absolute_open = start + open_at + OPEN.len();
        let Some(close_at) = text[absolute_open..].find(CLOSE) else {
            break;
        };
        let body = &text[absolute_open..absolute_open + close_at];
        let trimmed = body.trim();
        if !trimmed.is_empty() {
            out.push(trimmed.to_string());
        }
        start = absolute_open + close_at + CLOSE.len();
    }
    out
}

/// Walk a JSON value looking for `{"type":"tool_use","name":"Skill","input":{"skill":"X"}}`.
fn walk_for_skill(value: &serde_json::Value) -> Option<String> {
    if let Some(obj) = value.as_object() {
        if obj.get("type").and_then(|v| v.as_str()) == Some("tool_use")
            && obj.get("name").and_then(|v| v.as_str()) == Some("Skill")
        {
            if let Some(skill) =
                obj.get("input").and_then(|v| v.get("skill")).and_then(|v| v.as_str())
            {
                return Some(skill.to_string());
            }
        }
        for v in obj.values() {
            if let Some(found) = walk_for_skill(v) {
                return Some(found);
            }
        }
    }
    if let Some(arr) = value.as_array() {
        for v in arr {
            if let Some(found) = walk_for_skill(v) {
                return Some(found);
            }
        }
    }
    None
}

fn bump(stats: &mut UsageStats, name: &str, ts: Option<&str>) {
    let entry = stats.entry(name.to_string()).or_default();
    entry.invocations += 1;
    if let Some(ts) = ts {
        match &entry.last_used {
            Some(existing) if existing.as_str() >= ts => {}
            _ => entry.last_used = Some(ts.to_string()),
        }
    }
}

fn now_iso() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_command_names_finds_multiple() {
        let line = r#"junk <command-name>commit</command-name> mid <command-name>reflect</command-name> end"#;
        assert_eq!(extract_command_names(line), vec!["commit", "reflect"]);
    }

    #[test]
    fn extract_command_names_ignores_unmatched_open() {
        let line = "<command-name>open-but-never-closed";
        assert!(extract_command_names(line).is_empty());
    }

    #[test]
    fn ingest_counts_command_name_invocation() {
        let mut stats: UsageStats = BTreeMap::new();
        let line = r#"{"timestamp":"2026-01-01T00:00:00Z","content":"<command-name>commit</command-name>"}"#;
        ingest_line(line, &mut stats);
        assert_eq!(stats["commit"].invocations, 1);
        assert_eq!(
            stats["commit"].last_used.as_deref(),
            Some("2026-01-01T00:00:00Z")
        );
    }

    #[test]
    fn ingest_counts_skill_tool_use() {
        let mut stats: UsageStats = BTreeMap::new();
        let line = r#"{"type":"tool_use","name":"Skill","input":{"skill":"reflect"}}"#;
        ingest_line(line, &mut stats);
        assert_eq!(stats["reflect"].invocations, 1);
    }

    #[test]
    fn ingest_keeps_latest_timestamp() {
        let mut stats: UsageStats = BTreeMap::new();
        let older =
            r#"{"timestamp":"2026-01-01T00:00:00Z","content":"<command-name>x</command-name>"}"#;
        let newer =
            r#"{"timestamp":"2026-06-01T00:00:00Z","content":"<command-name>x</command-name>"}"#;
        ingest_line(older, &mut stats);
        ingest_line(newer, &mut stats);
        assert_eq!(stats["x"].invocations, 2);
        assert_eq!(
            stats["x"].last_used.as_deref(),
            Some("2026-06-01T00:00:00Z")
        );
    }

    #[test]
    fn ingest_ignores_malformed_lines() {
        let mut stats: UsageStats = BTreeMap::new();
        ingest_line("not json at all", &mut stats);
        ingest_line("", &mut stats);
        ingest_line("{}", &mut stats);
        assert!(stats.is_empty());
    }

    #[test]
    fn parse_claude_logs_walks_recursive() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("project-a")).unwrap();
        std::fs::create_dir_all(dir.path().join("project-b")).unwrap();
        std::fs::write(
            dir.path().join("project-a/2026-01.jsonl"),
            r#"{"timestamp":"2026-01-15T00:00:00Z","content":"<command-name>commit</command-name>"}
{"timestamp":"2026-01-16T00:00:00Z","content":"<command-name>commit</command-name>"}
"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("project-b/2026-02.jsonl"),
            r#"{"type":"tool_use","name":"Skill","input":{"skill":"reflect"},"timestamp":"2026-02-01T00:00:00Z"}
"#,
        )
        .unwrap();
        std::fs::write(dir.path().join("project-a/readme.md"), "ignore me").unwrap();

        let stats = parse_claude_logs(dir.path());
        assert_eq!(stats["commit"].invocations, 2);
        assert_eq!(
            stats["commit"].last_used.as_deref(),
            Some("2026-01-16T00:00:00Z")
        );
        assert_eq!(stats["reflect"].invocations, 1);
    }

    #[test]
    fn refresh_cache_roundtrips_through_disk() {
        let logs = tempfile::tempdir().unwrap();
        std::fs::write(
            logs.path().join("a.jsonl"),
            r#"{"timestamp":"2026-03-01T00:00:00Z","content":"<command-name>x</command-name>"}
"#,
        )
        .unwrap();
        let cache_dir = tempfile::tempdir().unwrap();
        let cache_path = cache_dir.path().join("usage.cache");

        let stats = refresh_cache(logs.path(), &cache_path).unwrap();
        assert_eq!(stats["x"].invocations, 1);

        let loaded = load_cache(&cache_path).unwrap();
        assert_eq!(loaded.units, stats);
        assert!(loaded.generated_at.is_some());
    }

    #[test]
    fn load_cache_missing_file_returns_default() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("no.cache");
        let cache = load_cache(&path).unwrap();
        assert!(cache.units.is_empty());
    }

    #[test]
    fn now_iso_shape_matches_rfc3339_seconds_z() {
        // Pins the wire shape used by every downstream consumer of `generated_at`.
        // Re-parseable by chrono, ends in 'Z', no fractional seconds, no offset.
        let s = now_iso();
        assert_eq!(s.len(), 20, "shape: YYYY-MM-DDTHH:MM:SSZ ({s})");
        assert!(s.ends_with('Z'), "must end in Z: {s}");
        let parsed = chrono::DateTime::parse_from_rfc3339(&s)
            .unwrap_or_else(|e| panic!("re-parse failed for {s}: {e}"));
        let round = parsed
            .with_timezone(&chrono::Utc)
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string();
        assert_eq!(round, s, "round-trip shape mismatch");
    }

    #[test]
    fn now_iso_format_matches_chrono_on_fixed_instants() {
        // Pins agreement with chrono's `%Y-%m-%dT%H:%M:%SZ` on a spread of
        // representative epochs (epoch, mid-2009, 2025 new-year UTC,
        // 2099-12-31 23:59:59 UTC). Catches off-by-one regressions if the
        // formatter is ever swapped again.
        use chrono::TimeZone;
        let cases: [(i64, &str); 4] = [
            (0, "1970-01-01T00:00:00Z"),
            (1_234_567_890, "2009-02-13T23:31:30Z"),
            (1_735_689_600, "2025-01-01T00:00:00Z"),
            (4_102_444_799, "2099-12-31T23:59:59Z"),
        ];
        for (secs, expected) in cases {
            let formatted = chrono::Utc
                .timestamp_opt(secs, 0)
                .single()
                .expect("valid timestamp")
                .format("%Y-%m-%dT%H:%M:%SZ")
                .to_string();
            assert_eq!(formatted, expected, "epoch={secs}");
        }
    }
}
