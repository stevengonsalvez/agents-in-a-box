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
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let days = secs / 86_400;
    let rem = secs % 86_400;
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let z = days as i64 + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y_long = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let mo = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    let y = if mo <= 2 { y_long + 1 } else { y_long };
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
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
}
