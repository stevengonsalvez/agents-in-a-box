// ABOUTME: Token usage data model and JSONL parser for Claude Code session analytics.
// Parses ~/.claude/projects/**/*.jsonl to extract per-day, per-week, per-project token usage.

use chrono::{Datelike, NaiveDate};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tracing::{debug, warn};

/// Token counts for a single usage bucket (day, week, project, etc.)
#[derive(Debug, Clone, Default)]
pub struct TokenBucket {
    pub input_tokens: u64,
    pub cache_creation_tokens: u64,
    pub cache_read_tokens: u64,
    pub output_tokens: u64,
    pub session_count: usize,
    pub project_count: usize,
}

impl TokenBucket {
    pub fn total(&self) -> u64 {
        self.input_tokens + self.cache_creation_tokens + self.cache_read_tokens + self.output_tokens
    }

    fn merge(&mut self, other: &TokenBucket) {
        self.input_tokens += other.input_tokens;
        self.cache_creation_tokens += other.cache_creation_tokens;
        self.cache_read_tokens += other.cache_read_tokens;
        self.output_tokens += other.output_tokens;
    }
}

/// Per-project summary
#[derive(Debug, Clone)]
pub struct ProjectUsage {
    pub name: String,
    pub bucket: TokenBucket,
}

/// Complete parsed usage data
#[derive(Debug, Clone)]
pub struct UsageData {
    pub daily: Vec<(NaiveDate, TokenBucket)>,
    pub weekly: Vec<(NaiveDate, TokenBucket)>,
    pub projects: Vec<ProjectUsage>,
    pub grand_total: TokenBucket,
}

impl Default for UsageData {
    fn default() -> Self {
        Self {
            daily: Vec::new(),
            weekly: Vec::new(),
            projects: Vec::new(),
            grand_total: TokenBucket::default(),
        }
    }
}

// Minimal JSON structures we need from the JSONL files
#[derive(Deserialize)]
struct JLine {
    #[serde(rename = "type")]
    msg_type: Option<String>,
    timestamp: Option<String>,
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
    message: Option<JMessage>,
}

#[derive(Deserialize)]
struct JMessage {
    usage: Option<JUsage>,
}

#[derive(Deserialize)]
struct JUsage {
    input_tokens: Option<u64>,
    cache_creation_input_tokens: Option<u64>,
    cache_read_input_tokens: Option<u64>,
    output_tokens: Option<u64>,
}

/// Parse all Claude Code JSONL session files and return aggregated usage data.
/// This is designed to run in a background thread (blocking I/O).
pub fn parse_usage() -> UsageData {
    let projects_dir = dirs::home_dir()
        .map(|h| h.join(".claude").join("projects"))
        .unwrap_or_default();

    if !projects_dir.is_dir() {
        warn!("Claude projects directory not found: {:?}", projects_dir);
        return UsageData::default();
    }

    // day -> (project_set, session_set, bucket)
    let mut daily_map: HashMap<NaiveDate, (std::collections::HashSet<String>, std::collections::HashSet<String>, TokenBucket)> = HashMap::new();
    // project -> bucket
    let mut project_map: HashMap<String, (std::collections::HashSet<String>, TokenBucket)> = HashMap::new();

    let project_dirs: Vec<_> = match std::fs::read_dir(&projects_dir) {
        Ok(rd) => rd.filter_map(|e| e.ok()).collect(),
        Err(e) => {
            warn!("Cannot read projects dir: {}", e);
            return UsageData::default();
        }
    };

    let username = whoami_username();

    for entry in project_dirs {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let project_name = clean_project_name(path.file_name().unwrap_or_default().to_str().unwrap_or(""), &username);

        let jsonl_files: Vec<PathBuf> = match std::fs::read_dir(&path) {
            Ok(rd) => rd
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.extension().map_or(false, |ext| ext == "jsonl"))
                .collect(),
            Err(_) => continue,
        };

        for jsonl_path in &jsonl_files {
            parse_jsonl_file(jsonl_path, &project_name, &mut daily_map, &mut project_map);
        }
    }

    // Build daily sorted vec
    let mut daily: Vec<(NaiveDate, TokenBucket)> = daily_map
        .into_iter()
        .map(|(date, (projects, sessions, mut bucket))| {
            bucket.session_count = sessions.len();
            bucket.project_count = projects.len();
            (date, bucket)
        })
        .collect();
    daily.sort_by_key(|(d, _)| *d);

    // Build weekly from daily
    let mut weekly_map: HashMap<NaiveDate, TokenBucket> = HashMap::new();
    let mut weekly_sessions: HashMap<NaiveDate, usize> = HashMap::new();
    let mut weekly_projects: HashMap<NaiveDate, usize> = HashMap::new();
    for (date, bucket) in &daily {
        let week_start = week_start_date(*date);
        let w = weekly_map.entry(week_start).or_default();
        w.merge(bucket);
        *weekly_sessions.entry(week_start).or_default() += bucket.session_count;
        *weekly_projects.entry(week_start).or_default() += bucket.project_count;
    }
    let mut weekly: Vec<(NaiveDate, TokenBucket)> = weekly_map
        .into_iter()
        .map(|(date, mut bucket)| {
            bucket.session_count = weekly_sessions.get(&date).copied().unwrap_or(0);
            bucket.project_count = weekly_projects.get(&date).copied().unwrap_or(0);
            (date, bucket)
        })
        .collect();
    weekly.sort_by_key(|(d, _)| *d);

    // Build projects sorted by total desc
    let mut projects: Vec<ProjectUsage> = project_map
        .into_iter()
        .map(|(name, (sessions, mut bucket))| {
            bucket.session_count = sessions.len();
            ProjectUsage { name, bucket }
        })
        .collect();
    projects.sort_by(|a, b| b.bucket.total().cmp(&a.bucket.total()));

    // Grand total
    let mut grand_total = TokenBucket::default();
    for (_, bucket) in &daily {
        grand_total.merge(bucket);
        grand_total.session_count += bucket.session_count;
    }
    grand_total.project_count = projects.len();

    debug!(
        "Parsed usage: {} days, {} weeks, {} projects, {} total tokens",
        daily.len(),
        weekly.len(),
        projects.len(),
        grand_total.total()
    );

    UsageData {
        daily,
        weekly,
        projects,
        grand_total,
    }
}

fn parse_jsonl_file(
    path: &Path,
    project_name: &str,
    daily_map: &mut HashMap<NaiveDate, (std::collections::HashSet<String>, std::collections::HashSet<String>, TokenBucket)>,
    project_map: &mut HashMap<String, (std::collections::HashSet<String>, TokenBucket)>,
) {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return,
    };

    for line in content.lines() {
        let jline: JLine = match serde_json::from_str(line) {
            Ok(j) => j,
            Err(_) => continue,
        };

        if jline.msg_type.as_deref() != Some("assistant") {
            continue;
        }

        let usage = match jline.message.and_then(|m| m.usage) {
            Some(u) => u,
            None => continue,
        };

        let ts = match &jline.timestamp {
            Some(t) if t.len() >= 10 => t,
            _ => continue,
        };

        let date = match NaiveDate::parse_from_str(&ts[..10], "%Y-%m-%d") {
            Ok(d) => d,
            Err(_) => continue,
        };

        let session_id = jline.session_id.unwrap_or_default();

        let input = usage.input_tokens.unwrap_or(0);
        let cache_create = usage.cache_creation_input_tokens.unwrap_or(0);
        let cache_read = usage.cache_read_input_tokens.unwrap_or(0);
        let output = usage.output_tokens.unwrap_or(0);

        // Daily
        let daily_entry = daily_map.entry(date).or_default();
        daily_entry.0.insert(project_name.to_string());
        daily_entry.1.insert(session_id.clone());
        daily_entry.2.input_tokens += input;
        daily_entry.2.cache_creation_tokens += cache_create;
        daily_entry.2.cache_read_tokens += cache_read;
        daily_entry.2.output_tokens += output;

        // Project
        let proj_entry = project_map.entry(project_name.to_string()).or_default();
        proj_entry.0.insert(session_id);
        proj_entry.1.input_tokens += input;
        proj_entry.1.cache_creation_tokens += cache_create;
        proj_entry.1.cache_read_tokens += cache_read;
        proj_entry.1.output_tokens += output;
    }
}

fn clean_project_name(raw: &str, username: &str) -> String {
    let prefixes = [
        format!("-Users-{}-", username),
        format!("Users-{}-", username),
    ];
    let mut name = raw.to_string();
    for p in &prefixes {
        if let Some(stripped) = name.strip_prefix(p.as_str()) {
            name = stripped.to_string();
            break;
        }
    }
    // Shorten long worktree names
    if name.starts_with("-agents-in-a-box-worktrees-") {
        name = name.replacen("-agents-in-a-box-worktrees-", "worktree/", 1);
    }
    if name.is_empty() {
        raw.to_string()
    } else {
        name
    }
}

fn week_start_date(date: NaiveDate) -> NaiveDate {
    let days_since_monday = date.weekday().num_days_from_monday();
    date - chrono::Duration::days(i64::from(days_since_monday))
}

fn whoami_username() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "unknown".to_string())
}

/// Format a token count in human-readable form (e.g., "1.2B", "456M", "12K")
pub fn format_tokens_short(n: u64) -> String {
    if n >= 1_000_000_000 {
        format!("{:.1}B", n as f64 / 1_000_000_000.0)
    } else if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.0}K", n as f64 / 1_000.0)
    } else {
        format!("{n}")
    }
}

/// Format a token count with commas
pub fn format_tokens_commas(n: u64) -> String {
    let s = n.to_string();
    let mut result = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            result.push(',');
        }
        result.push(c);
    }
    result.chars().rev().collect()
}
