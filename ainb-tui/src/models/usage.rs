// ABOUTME: Token usage data model and local session parsers for usage analytics.
// Parses Claude and Codex JSONL histories into reusable aggregates for TUI and CLI views.

use chrono::{DateTime, Datelike, Duration, Local, NaiveDate, TimeZone};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracing::{debug, warn};

use crate::usage_cache::{Cache, ParseHint, ParseResult};

/// Usage period selector shared by TUI and CLI report queries.
///
/// Variants beyond the original `Today/Week/ThirtyDays/Month/All/Custom`
/// set are introduced for the date-picker UX:
/// - `LastNDays(n)` — generic "last n days" used for 90d (and exposed
///   over the CLI as `--last-n-days N`).
/// - `SpecificMonth(date)` — calendar month containing `date` (day is
///   ignored; canonicalised to day=1 by callers).
/// - `SpecificQuarter(year, q)` — calendar quarter `q` of `year`,
///   `q ∈ 1..=4`.
/// - `YearToDate` — Jan 1 of the current local year through today.
///
/// `Week` and `ThirtyDays` are retained (rather than collapsing into
/// `LastNDays`) to keep the existing CLI `PeriodArg` enum, JSON
/// serialisation, and downstream tests stable.
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UsagePeriod {
    Today,
    Week,
    ThirtyDays,
    /// Generic "last N days" — used for 90d and the `--last-n-days N`
    /// CLI flag. `n=0` is treated as 1 day (today only) by
    /// `date_range_for_period` to avoid empty ranges.
    LastNDays(u32),
    /// Calendar month of the given date. Callers should canonicalise
    /// the day to 1 — the renderer pretty-prints as "Apr 2026".
    SpecificMonth(NaiveDate),
    /// Calendar quarter `q ∈ 1..=4` of `year`. Renders as "Q2 2026".
    SpecificQuarter(i32, u8),
    /// Jan 1 of the current local year through today.
    YearToDate,
    Month,
    All,
    Custom { from: NaiveDate, to: NaiveDate },
}

impl Default for UsagePeriod {
    fn default() -> Self {
        Self::Week
    }
}

/// Provider filter shared by TUI and CLI report queries.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageProviderFilter {
    #[default]
    All,
    Claude,
    Codex,
}

impl UsageProviderFilter {
    fn includes(self, provider: &str) -> bool {
        match self {
            Self::All => true,
            Self::Claude => provider == "claude",
            Self::Codex => provider == "codex",
        }
    }
}

/// Deterministic activity categories. Phase 2 fills these from turn classification.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityCategory {
    Coding,
    Debugging,
    Feature,
    Refactoring,
    Testing,
    Exploration,
    Planning,
    Delegation,
    Git,
    BuildDeploy,
    Brainstorming,
    Conversation,
    General,
}

impl ActivityCategory {
    pub fn label(self) -> &'static str {
        match self {
            Self::Coding => "Coding",
            Self::Debugging => "Debugging",
            Self::Feature => "Feature",
            Self::Refactoring => "Refactoring",
            Self::Testing => "Testing",
            Self::Exploration => "Exploration",
            Self::Planning => "Planning",
            Self::Delegation => "Delegation",
            Self::Git => "Git",
            Self::BuildDeploy => "Build/Deploy",
            Self::Brainstorming => "Brainstorming",
            Self::Conversation => "Conversation",
            Self::General => "General",
        }
    }
}

/// Query used to parse and aggregate usage.
#[derive(Debug, Clone, Default)]
pub struct UsageQuery {
    pub period: UsagePeriod,
    pub provider_filter: UsageProviderFilter,
    pub include_projects: Vec<String>,
    pub exclude_projects: Vec<String>,
    /// Cross-filters (exact-match drill-downs) layered on top of the
    /// substring `include_projects` / `exclude_projects` globs.
    pub filters: UsageFilters,
}

/// Exact-match drill-down filters set by the dashboard cross-filter
/// (Grafana-style click-to-pivot) and the `--project / --model /
/// --activity / --session` CLI flags.
///
/// All filter sets are AND-combined with each other and with the existing
/// `include_projects` / `exclude_projects` globs. Each filter list is
/// internally OR-combined: `--project a --project b` matches calls in
/// either project.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UsageFilters {
    pub project: Vec<String>,
    pub model: Vec<String>,
    pub activity: Vec<String>,
    pub session: Vec<String>,
}

impl UsageFilters {
    pub fn is_empty(&self) -> bool {
        self.project.is_empty()
            && self.model.is_empty()
            && self.activity.is_empty()
            && self.session.is_empty()
    }

    /// True if any cross-filter is set. Mirror of `!is_empty()` for sites
    /// where the positive form reads better.
    pub fn any(&self) -> bool {
        !self.is_empty()
    }

    /// Pop the most-recently-added filter chip. Removal order: session →
    /// activity → model → project (matches the chip-strip render order so
    /// `Esc` removes the visually rightmost chip first).
    pub fn pop_last(&mut self) -> Option<UsageFilterChip> {
        if let Some(value) = self.session.pop() {
            return Some(UsageFilterChip::Session(value));
        }
        if let Some(value) = self.activity.pop() {
            return Some(UsageFilterChip::Activity(value));
        }
        if let Some(value) = self.model.pop() {
            return Some(UsageFilterChip::Model(value));
        }
        if let Some(value) = self.project.pop() {
            return Some(UsageFilterChip::Project(value));
        }
        None
    }

    pub fn clear(&mut self) {
        self.project.clear();
        self.model.clear();
        self.activity.clear();
        self.session.clear();
    }

    fn matches(&self, call: &ProviderCall, category: ActivityCategory) -> bool {
        if !self.project.is_empty() && !self.project.iter().any(|p| p == &call.project) {
            return false;
        }
        if !self.model.is_empty() && !self.model.iter().any(|m| m == &call.model) {
            return false;
        }
        if !self.session.is_empty() && !self.session.iter().any(|s| s == &call.session_id) {
            return false;
        }
        if !self.activity.is_empty() {
            let label = category.label();
            if !self.activity.iter().any(|a| a == label) {
                return false;
            }
        }
        true
    }
}

/// One filter chip — used by the chip-strip widget and `pop_last`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UsageFilterChip {
    Project(String),
    Model(String),
    Activity(String),
    Session(String),
}

impl UsageFilterChip {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Project(_) => "project",
            Self::Model(_) => "model",
            Self::Activity(_) => "activity",
            Self::Session(_) => "session",
        }
    }

    pub fn value(&self) -> &str {
        match self {
            Self::Project(v) | Self::Model(v) | Self::Activity(v) | Self::Session(v) => v,
        }
    }
}

/// Token counts for a single usage bucket, provider call, or aggregate row.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct TokenBucket {
    pub input_tokens: u64,
    pub cache_creation_tokens: u64,
    pub cache_read_tokens: u64,
    pub output_tokens: u64,
    pub reasoning_tokens: u64,
    pub session_count: usize,
    pub project_count: usize,
    pub call_count: usize,
    pub cost_usd: Option<f64>,
}

impl TokenBucket {
    pub fn total(&self) -> u64 {
        self.input_tokens
            + self.cache_creation_tokens
            + self.cache_read_tokens
            + self.output_tokens
            + self.reasoning_tokens
    }

    fn merge(&mut self, other: &TokenBucket) {
        self.input_tokens += other.input_tokens;
        self.cache_creation_tokens += other.cache_creation_tokens;
        self.cache_read_tokens += other.cache_read_tokens;
        self.output_tokens += other.output_tokens;
        self.reasoning_tokens += other.reasoning_tokens;
        self.call_count += other.call_count;
        self.cost_usd = merge_cost(self.cost_usd, other.cost_usd);
    }
}

/// Per-provider API call parsed from a local session file.
///
/// `Deserialize` is required for bincode round-trip in the persistent
/// usage cache (`usage_cache::store`).
///
/// **WARNING — bincode layout stability.** Bincode v1 with default options
/// encodes positionally with no field tags. Any change to the field set or
/// order of this struct (or any nested type it owns) silently invalidates
/// every cached blob written under the current `BLOB_FORMAT_BINCODE_V1`.
/// Wrong-shape decodes can either panic (caught — falls through to a full
/// re-parse) or, much worse, succeed with mis-aligned bytes and return
/// wrong analytics from the cache.
///
/// **If you change this struct or any nested type, you MUST:**
/// 1. Bump `usage_cache::db::BLOB_FORMAT_BINCODE_V1` to a new value, and
/// 2. Update the layout-stability tripwire test in `usage_cache::tests`.
///
/// The tripwire test asserts a fixed serialized byte length for a known
/// fixture and will fail CI if the layout drifts without an explicit bump.
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderCall {
    pub provider: String,
    pub model: String,
    pub session_id: String,
    pub project: String,
    pub project_path: String,
    pub timestamp: DateTime<Local>,
    pub input_tokens: u64,
    pub cache_creation_tokens: u64,
    pub cache_read_tokens: u64,
    pub output_tokens: u64,
    pub reasoning_tokens: u64,
    pub cost_usd: Option<f64>,
    pub tools: Vec<String>,
    pub bash_commands: Vec<String>,
    pub user_message: String,
}

impl ProviderCall {
    fn bucket(&self) -> TokenBucket {
        TokenBucket {
            input_tokens: self.input_tokens,
            cache_creation_tokens: self.cache_creation_tokens,
            cache_read_tokens: self.cache_read_tokens,
            output_tokens: self.output_tokens,
            reasoning_tokens: self.reasoning_tokens,
            session_count: 0,
            project_count: 0,
            call_count: 1,
            cost_usd: self.cost_usd,
        }
    }
}

/// Daily dashboard row.
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize)]
pub struct DailyUsage {
    pub date: NaiveDate,
    pub bucket: TokenBucket,
}

/// Per-project summary.
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize)]
pub struct ProjectUsage {
    pub name: String,
    pub path: String,
    pub bucket: TokenBucket,
}

/// Per-session summary.
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize)]
pub struct SessionUsage {
    pub provider: String,
    pub project: String,
    pub session_id: String,
    pub first_timestamp: DateTime<Local>,
    pub last_timestamp: DateTime<Local>,
    pub bucket: TokenBucket,
}

/// Per-model summary.
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize)]
pub struct ModelUsage {
    pub model: String,
    pub bucket: TokenBucket,
}

/// Activity summary with classified turns and retry counts.
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize)]
pub struct ActivityUsage {
    pub category: ActivityCategory,
    pub bucket: TokenBucket,
    pub turns: usize,
    pub retries: usize,
    pub edit_turns: usize,
    pub one_shot_turns: usize,
}

/// Tool/MCP/shell breakdown row.
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize)]
pub struct NamedUsage {
    pub name: String,
    pub calls: usize,
}

/// Complete parsed usage data.
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize)]
pub struct UsageData {
    pub daily: Vec<(NaiveDate, TokenBucket)>,
    pub weekly: Vec<(NaiveDate, TokenBucket)>,
    pub projects: Vec<ProjectUsage>,
    pub grand_total: TokenBucket,
    pub calls: Vec<ProviderCall>,
    pub sessions: Vec<SessionUsage>,
    pub models: Vec<ModelUsage>,
    pub activities: Vec<ActivityUsage>,
    pub tools: Vec<NamedUsage>,
    pub mcp_servers: Vec<NamedUsage>,
    pub shell_commands: Vec<NamedUsage>,
}

#[derive(Debug, Clone, Serialize)]
pub struct UsageOverview {
    pub calls: usize,
    pub sessions: usize,
    pub projects: usize,
    pub tokens: u64,
    pub cost_usd: Option<f64>,
}

impl UsageData {
    pub fn overview(&self) -> UsageOverview {
        UsageOverview {
            calls: self.grand_total.call_count,
            sessions: self.grand_total.session_count,
            projects: self.grand_total.project_count,
            tokens: self.grand_total.total(),
            cost_usd: self.grand_total.cost_usd,
        }
    }
}

impl Default for UsageData {
    fn default() -> Self {
        Self {
            daily: Vec::new(),
            weekly: Vec::new(),
            projects: Vec::new(),
            grand_total: TokenBucket::default(),
            calls: Vec::new(),
            sessions: Vec::new(),
            models: Vec::new(),
            activities: Vec::new(),
            tools: Vec::new(),
            mcp_servers: Vec::new(),
            shell_commands: Vec::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct UsageSourceRoots {
    pub claude_projects_dir: Option<PathBuf>,
    pub codex_dir: Option<PathBuf>,
}

impl Default for UsageSourceRoots {
    fn default() -> Self {
        let home = dirs::home_dir();
        let claude_projects_dir = std::env::var_os("CLAUDE_CONFIG_DIR")
            .map(PathBuf::from)
            .map(|path| path.join("projects"))
            .or_else(|| home.as_ref().map(|path| path.join(".claude").join("projects")));

        let codex_dir = std::env::var_os("CODEX_HOME")
            .map(PathBuf::from)
            .or_else(|| home.as_ref().map(|path| path.join(".codex")));

        Self {
            claude_projects_dir,
            codex_dir,
        }
    }
}

#[derive(Deserialize)]
struct ClaudeLine {
    #[serde(rename = "type")]
    msg_type: Option<String>,
    timestamp: Option<String>,
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
    cwd: Option<String>,
    message: Option<ClaudeMessage>,
}

#[derive(Deserialize)]
struct ClaudeMessage {
    content: Option<Value>,
    model: Option<String>,
    usage: Option<ClaudeUsage>,
}

#[derive(Deserialize)]
struct ClaudeUsage {
    input_tokens: Option<u64>,
    cache_creation_input_tokens: Option<u64>,
    cache_read_input_tokens: Option<u64>,
    output_tokens: Option<u64>,
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
    role: Option<String>,
    cwd: Option<String>,
    originator: Option<String>,
    session_id: Option<String>,
    model: Option<String>,
    name: Option<String>,
    content: Option<Vec<CodexContent>>,
    info: Option<CodexInfo>,
}

#[derive(Deserialize)]
struct CodexContent {
    #[serde(rename = "type")]
    content_type: Option<String>,
    text: Option<String>,
}

#[derive(Deserialize)]
struct CodexInfo {
    model: Option<String>,
    model_name: Option<String>,
    last_token_usage: Option<CodexTokenUsage>,
    total_token_usage: Option<CodexTokenUsage>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize)]
struct CodexTokenUsage {
    input_tokens: Option<u64>,
    cached_input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    reasoning_output_tokens: Option<u64>,
    total_tokens: Option<u64>,
}

/// Parse all known local session files using the legacy Claude-only query.
/// This is designed to run in a background thread.
pub fn parse_usage() -> UsageData {
    parse_usage_for(UsageQuery {
        provider_filter: UsageProviderFilter::Claude,
        period: UsagePeriod::All,
        include_projects: Vec::new(),
        exclude_projects: Vec::new(),
        filters: UsageFilters::default(),
    })
}

/// Parse local session files for a query using default user data roots.
pub fn parse_usage_for(query: UsageQuery) -> UsageData {
    parse_usage_for_with_roots(query, &UsageSourceRoots::default())
}

/// Process-wide singleton cache handle. Constructed lazily; on open
/// failure (eg. read-only home dir) we fall back to a disabled cache so
/// analytics remain functional.
fn default_cache() -> Arc<Cache> {
    use std::sync::OnceLock;
    static CACHE: OnceLock<Arc<Cache>> = OnceLock::new();
    CACHE
        .get_or_init(|| {
            let Some(path) = crate::usage_cache::store::default_db_path() else {
                debug!("usage_cache: no home dir; running with cache disabled");
                return Arc::new(Cache::disabled());
            };
            match Cache::open(path.clone()) {
                Ok(c) => Arc::new(c),
                Err(err) => {
                    warn!(
                        "usage_cache: failed to open {:?} ({err}); cache disabled",
                        path
                    );
                    Arc::new(Cache::disabled())
                }
            }
        })
        .clone()
}

/// Public helper for callers (CLI `--no-cache`, tests) to get a fresh
/// disabled cache without going through the singleton.
pub fn disabled_cache() -> Arc<Cache> {
    Arc::new(Cache::disabled())
}

/// Public helper for callers that want the process-wide cache (eg. CLI
/// `cache info` / `cache clear`).
pub fn shared_cache() -> Arc<Cache> {
    default_cache()
}

/// Parse local session files for a query using explicit roots.
pub fn parse_usage_for_with_roots(query: UsageQuery, roots: &UsageSourceRoots) -> UsageData {
    parse_usage_for_with_roots_and_cache(query, roots, default_cache())
}

/// Parse local session files for a query, using an explicit cache. Pass
/// `Cache::disabled()` to bypass the cache entirely.
pub fn parse_usage_for_with_roots_and_cache(
    query: UsageQuery,
    roots: &UsageSourceRoots,
    cache: Arc<Cache>,
) -> UsageData {
    let range = date_range_for_period(&query.period);
    let mut calls = Vec::new();

    if query.provider_filter.includes("claude") {
        calls.extend(parse_claude_sources(
            roots.claude_projects_dir.as_deref(),
            cache.as_ref(),
        ));
    }
    if query.provider_filter.includes("codex") {
        calls.extend(parse_codex_sources(
            roots.codex_dir.as_deref(),
            cache.as_ref(),
        ));
    }

    let filtered: Vec<ProviderCall> = calls
        .into_iter()
        .filter(|call| {
            range.as_ref().map_or(true, |(start, end)| {
                call.timestamp >= *start && call.timestamp <= *end
            })
        })
        .filter(|call| project_matches(call, &query.include_projects, &query.exclude_projects))
        .collect();

    let aggregated = aggregate_calls(filtered);
    if query.filters.is_empty() {
        aggregated
    } else {
        filter_usage_data(&aggregated, &query.filters)
    }
}

fn parse_claude_sources(projects_dir: Option<&Path>, cache: &Cache) -> Vec<ProviderCall> {
    let Some(projects_dir) = projects_dir else {
        return Vec::new();
    };
    if !projects_dir.is_dir() {
        warn!("Claude projects directory not found: {:?}", projects_dir);
        return Vec::new();
    }

    let username = whoami_username();
    let mut calls = Vec::new();
    let Ok(project_dirs) = std::fs::read_dir(projects_dir) else {
        return calls;
    };

    for entry in project_dirs.filter_map(Result::ok) {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        let raw_name = path.file_name().and_then(|name| name.to_str()).unwrap_or("");
        let project = clean_project_name(raw_name, &username);
        let project_path = unsanitize_project_path(raw_name);

        for jsonl_path in collect_claude_jsonl_files(&path) {
            let project = project.clone();
            let project_path = project_path.clone();
            let parsed = cache.get_or_parse(&jsonl_path, |path, hint| match hint {
                ParseHint::Full => parse_claude_source_full(path, &project, &project_path),
                ParseHint::Append {
                    from_offset,
                    existing,
                } => {
                    parse_claude_source_append(path, &project, &project_path, from_offset, existing)
                }
            });
            calls.extend(parsed);
        }
    }

    calls
}

fn collect_claude_jsonl_files(project_dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let Ok(entries) = std::fs::read_dir(project_dir) else {
        return files;
    };

    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "jsonl") {
            files.push(path);
            continue;
        }

        let subagents = path.join("subagents");
        let Ok(sub_entries) = std::fs::read_dir(subagents) else {
            continue;
        };
        for sub_entry in sub_entries.filter_map(Result::ok) {
            let sub_path = sub_entry.path();
            if sub_path.extension().is_some_and(|ext| ext == "jsonl") {
                files.push(sub_path);
            }
        }
    }

    files
}

/// Full parse of a Claude JSONL session file. Returns the complete
/// `Vec<ProviderCall>` and the byte offset where parsing stopped (typically
/// the file size at open time — note this is best-effort: if the file is
/// being appended to concurrently we may stop short, which the cache will
/// recover from on the next call via append-only).
fn parse_claude_source_full(path: &Path, project: &str, project_path: &str) -> ParseResult {
    let mut file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => {
            return ParseResult {
                calls: Vec::new(),
                end_offset: 0,
            };
        }
    };
    let mut content = String::new();
    if file.read_to_string(&mut content).is_err() {
        return ParseResult {
            calls: Vec::new(),
            end_offset: 0,
        };
    }
    let end_offset = content.len() as u64;

    let mut calls = Vec::new();
    let mut current_user_message = String::new();
    for line in content.lines() {
        if let Some(call) =
            parse_claude_line(line, path, project, project_path, &mut current_user_message)
        {
            calls.push(call);
        }
    }
    ParseResult { calls, end_offset }
}

/// Append-only parse: re-open the file, seek to `from_offset`, and parse
/// only the new bytes. The returned `Vec<ProviderCall>` is `existing`
/// concatenated with the new calls, so the cache can persist a complete
/// blob for the file.
///
/// Trade-off: `current_user_message` state from before `from_offset` is
/// not restored. New assistant rows that appear *before* the next user
/// line in the appended tail will get an empty `user_message`. This is
/// rare in practice and cheap to fix in a follow-up by persisting parser
/// state alongside the row.
fn parse_claude_source_append(
    path: &Path,
    project: &str,
    project_path: &str,
    from_offset: u64,
    existing: &[ProviderCall],
) -> ParseResult {
    let mut file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => {
            return ParseResult {
                calls: existing.to_vec(),
                end_offset: from_offset,
            };
        }
    };
    let total_len = file.metadata().map(|m| m.len()).unwrap_or(from_offset);
    if file.seek(SeekFrom::Start(from_offset)).is_err() {
        return ParseResult {
            calls: existing.to_vec(),
            end_offset: from_offset,
        };
    }
    let reader = BufReader::new(file);
    let mut calls = existing.to_vec();
    let mut current_user_message = String::new();
    for line in reader.lines() {
        let Ok(line) = line else { break };
        if let Some(call) = parse_claude_line(
            &line,
            path,
            project,
            project_path,
            &mut current_user_message,
        ) {
            calls.push(call);
        }
    }
    ParseResult {
        calls,
        end_offset: total_len,
    }
}

/// Parse a single Claude JSONL line. Returns `Some(call)` for assistant
/// lines that carry usage data; mutates `current_user_message` when a
/// user line is encountered. None for unrecognized / unparseable lines.
fn parse_claude_line(
    line: &str,
    path: &Path,
    project: &str,
    project_path: &str,
    current_user_message: &mut String,
) -> Option<ProviderCall> {
    let parsed: ClaudeLine = serde_json::from_str(line).ok()?;
    match parsed.msg_type.as_deref() {
        Some("user") => {
            if let Some(message) = parsed.message {
                *current_user_message = extract_claude_user_text(message.content.as_ref());
            }
            None
        }
        Some("assistant") => {
            let message = parsed.message?;
            let usage = message.usage?;
            let timestamp = parsed.timestamp.as_deref().and_then(parse_timestamp)?;

            let model = message.model.unwrap_or_else(|| "claude-unknown".to_string());
            let tools = extract_claude_tools(message.content.as_ref());
            let bash_commands = extract_bash_commands_from_claude_content(message.content.as_ref());
            let input_tokens = usage.input_tokens.unwrap_or(0);
            let output_tokens = usage.output_tokens.unwrap_or(0);
            let cache_creation_tokens = usage.cache_creation_input_tokens.unwrap_or(0);
            let cache_read_tokens = usage.cache_read_input_tokens.unwrap_or(0);
            let cost_usd = estimate_cost_usd(
                &model,
                input_tokens,
                output_tokens,
                cache_creation_tokens,
                cache_read_tokens,
                0,
            );

            Some(ProviderCall {
                provider: "claude".to_string(),
                model,
                session_id: parsed.session_id.unwrap_or_else(|| {
                    path.file_stem().and_then(|stem| stem.to_str()).unwrap_or("unknown").to_string()
                }),
                project: project.to_string(),
                project_path: parsed.cwd.unwrap_or_else(|| project_path.to_string()),
                timestamp,
                input_tokens,
                cache_creation_tokens,
                cache_read_tokens,
                output_tokens,
                reasoning_tokens: 0,
                cost_usd,
                tools,
                bash_commands,
                user_message: current_user_message.clone(),
            })
        }
        _ => None,
    }
}

fn parse_codex_sources(codex_dir: Option<&Path>, cache: &Cache) -> Vec<ProviderCall> {
    let Some(codex_dir) = codex_dir else {
        return Vec::new();
    };
    let sessions_dir = codex_dir.join("sessions");
    if !sessions_dir.is_dir() {
        return Vec::new();
    }

    let mut calls = Vec::new();
    for file in discover_codex_sources(&sessions_dir) {
        // Codex sessions accumulate cumulative token totals across the file,
        // so an append-only parse would need replayed state. For PR-A we
        // ignore the append hint and always full-parse on change. The cache
        // still serves the unchanged-fingerprint path.
        let parsed = cache.get_or_parse(&file, |path, _hint| {
            let calls = parse_codex_source(path);
            let end_offset = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
            ParseResult { calls, end_offset }
        });
        calls.extend(parsed);
    }
    calls
}

fn discover_codex_sources(sessions_dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let Ok(years) = std::fs::read_dir(sessions_dir) else {
        return files;
    };

    for year in years.filter_map(Result::ok) {
        let year_path = year.path();
        if !year_path.is_dir() || !is_date_component(&year_path, 4) {
            continue;
        }
        let Ok(months) = std::fs::read_dir(year_path) else {
            continue;
        };
        for month in months.filter_map(Result::ok) {
            let month_path = month.path();
            if !month_path.is_dir() || !is_date_component(&month_path, 2) {
                continue;
            }
            let Ok(days) = std::fs::read_dir(month_path) else {
                continue;
            };
            for day in days.filter_map(Result::ok) {
                let day_path = day.path();
                if !day_path.is_dir() || !is_date_component(&day_path, 2) {
                    continue;
                }
                let Ok(entries) = std::fs::read_dir(day_path) else {
                    continue;
                };
                for entry in entries.filter_map(Result::ok) {
                    let path = entry.path();
                    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                        continue;
                    };
                    if name.starts_with("rollout-")
                        && name.ends_with(".jsonl")
                        && is_valid_codex_session(&path)
                    {
                        files.push(path);
                    }
                }
            }
        }
    }

    files
}

fn parse_codex_source(path: &Path) -> Vec<ProviderCall> {
    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(_) => return Vec::new(),
    };

    let mut calls = Vec::new();
    let mut session_id =
        path.file_stem().and_then(|stem| stem.to_str()).unwrap_or("unknown").to_string();
    let mut session_model: Option<String> = None;
    let mut cwd = "unknown".to_string();
    let mut project = "unknown".to_string();
    let mut previous_cumulative_total = 0_u64;
    let mut previous_input = 0_u64;
    let mut previous_cached = 0_u64;
    let mut previous_output = 0_u64;
    let mut previous_reasoning = 0_u64;
    let mut pending_tools: Vec<String> = Vec::new();
    let mut pending_user_message = String::new();

    for line in content.lines() {
        let entry: CodexEntry = match serde_json::from_str(line) {
            Ok(entry) => entry,
            Err(_) => continue,
        };

        let payload = entry.payload.as_ref();

        if entry.entry_type == "session_meta" {
            if let Some(payload) = payload {
                if let Some(id) = &payload.session_id {
                    session_id = id.clone();
                }
                if let Some(model) = &payload.model {
                    session_model = Some(model.clone());
                }
                if let Some(session_cwd) = &payload.cwd {
                    cwd = session_cwd.clone();
                    project = sanitize_codex_project(session_cwd);
                }
            }
            continue;
        }

        if entry.entry_type == "turn_context" {
            if let Some(model) = payload.and_then(|payload| payload.model.as_ref()) {
                session_model = Some(model.clone());
            }
            continue;
        }

        if entry.entry_type == "response_item"
            && payload.and_then(|p| p.payload_type.as_deref()) == Some("function_call")
        {
            if let Some(raw_name) = payload.and_then(|p| p.name.as_deref()) {
                pending_tools.push(normalize_codex_tool(raw_name).to_string());
            }
            continue;
        }

        if entry.entry_type == "event_msg"
            && payload.and_then(|p| p.payload_type.as_deref()) == Some("patch_apply_end")
        {
            pending_tools.push("Edit".to_string());
            continue;
        }

        if entry.entry_type == "response_item"
            && payload.and_then(|p| p.payload_type.as_deref()) == Some("message")
            && payload.and_then(|p| p.role.as_deref()) == Some("user")
        {
            let texts = payload
                .and_then(|p| p.content.as_ref())
                .map(|content| {
                    content
                        .iter()
                        .filter(|item| item.content_type.as_deref() == Some("input_text"))
                        .filter_map(|item| item.text.as_deref())
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .unwrap_or_default();
            if !texts.is_empty() {
                pending_user_message = texts;
            }
            continue;
        }

        if entry.entry_type != "event_msg"
            || payload.and_then(|p| p.payload_type.as_deref()) != Some("token_count")
        {
            continue;
        }

        let Some(info) = payload.and_then(|p| p.info.as_ref()) else {
            continue;
        };

        let cumulative_total =
            info.total_token_usage.and_then(|usage| usage.total_tokens).unwrap_or(0);
        if cumulative_total > 0 && cumulative_total == previous_cumulative_total {
            continue;
        }
        previous_cumulative_total = cumulative_total;

        let (input_tokens, cached_tokens, output_tokens, reasoning_tokens) =
            if let Some(last) = info.last_token_usage {
                let input = last.input_tokens.unwrap_or(0);
                let cached = last.cached_input_tokens.unwrap_or(0);
                let output = last.output_tokens.unwrap_or(0);
                let reasoning = last.reasoning_output_tokens.unwrap_or(0);

                if let Some(total) = info.total_token_usage {
                    previous_input = total.input_tokens.unwrap_or(previous_input + input);
                    previous_cached = total.cached_input_tokens.unwrap_or(previous_cached + cached);
                    previous_output = total.output_tokens.unwrap_or(previous_output + output);
                    previous_reasoning =
                        total.reasoning_output_tokens.unwrap_or(previous_reasoning + reasoning);
                } else {
                    previous_input += input;
                    previous_cached += cached;
                    previous_output += output;
                    previous_reasoning += reasoning;
                }

                (input, cached, output, reasoning)
            } else if let Some(total) = info.total_token_usage {
                let input = total.input_tokens.unwrap_or(0).saturating_sub(previous_input);
                let cached = total.cached_input_tokens.unwrap_or(0).saturating_sub(previous_cached);
                let output = total.output_tokens.unwrap_or(0).saturating_sub(previous_output);
                let reasoning =
                    total.reasoning_output_tokens.unwrap_or(0).saturating_sub(previous_reasoning);

                previous_input = total.input_tokens.unwrap_or(0);
                previous_cached = total.cached_input_tokens.unwrap_or(0);
                previous_output = total.output_tokens.unwrap_or(0);
                previous_reasoning = total.reasoning_output_tokens.unwrap_or(0);

                (input, cached, output, reasoning)
            } else {
                continue;
            };

        if input_tokens + cached_tokens + output_tokens + reasoning_tokens == 0 {
            continue;
        }

        let Some(timestamp) = entry.timestamp.as_deref().and_then(parse_timestamp) else {
            continue;
        };

        let uncached_input_tokens = input_tokens.saturating_sub(cached_tokens);
        let model = resolve_codex_model(payload, session_model.as_deref());
        let cost_usd = estimate_cost_usd(
            &model,
            uncached_input_tokens,
            output_tokens + reasoning_tokens,
            0,
            cached_tokens,
            0,
        );

        calls.push(ProviderCall {
            provider: "codex".to_string(),
            model,
            session_id: session_id.clone(),
            project: project.clone(),
            project_path: cwd.clone(),
            timestamp,
            input_tokens: uncached_input_tokens,
            cache_creation_tokens: 0,
            cache_read_tokens: cached_tokens,
            output_tokens,
            reasoning_tokens,
            cost_usd,
            tools: std::mem::take(&mut pending_tools),
            bash_commands: Vec::new(),
            user_message: std::mem::take(&mut pending_user_message),
        });
    }

    calls
}

fn aggregate_calls(mut calls: Vec<ProviderCall>) -> UsageData {
    if calls.is_empty() {
        return UsageData::default();
    }

    calls.sort_by_key(|call| call.timestamp);
    let turn_analysis = analyze_turns(&calls);

    let mut daily_map: HashMap<NaiveDate, (HashSet<String>, HashSet<String>, TokenBucket)> =
        HashMap::new();
    let mut project_map: HashMap<String, (String, HashSet<String>, TokenBucket)> = HashMap::new();
    let mut session_map: HashMap<String, SessionUsageAccumulator> = HashMap::new();
    let mut model_map: HashMap<String, TokenBucket> = HashMap::new();
    let mut activity_map: HashMap<ActivityCategory, ActivityAccumulator> = HashMap::new();
    let mut tool_map: HashMap<String, usize> = HashMap::new();
    let mut mcp_map: HashMap<String, usize> = HashMap::new();
    let mut shell_map: HashMap<String, usize> = HashMap::new();

    for (idx, call) in calls.iter().enumerate() {
        let bucket = call.bucket();
        let day = call.timestamp.date_naive();
        let session_key = format!("{}:{}:{}", call.provider, call.project, call.session_id);

        let daily = daily_map.entry(day).or_default();
        daily.0.insert(call.project.clone());
        daily.1.insert(session_key.clone());
        daily.2.merge(&bucket);

        let project = project_map.entry(call.project.clone()).or_insert_with(|| {
            (
                call.project_path.clone(),
                HashSet::new(),
                TokenBucket::default(),
            )
        });
        project.0 = call.project_path.clone();
        project.1.insert(session_key.clone());
        project.2.merge(&bucket);

        let session = session_map.entry(session_key).or_insert_with(|| SessionUsageAccumulator {
            provider: call.provider.clone(),
            project: call.project.clone(),
            session_id: call.session_id.clone(),
            first_timestamp: call.timestamp,
            last_timestamp: call.timestamp,
            bucket: TokenBucket::default(),
        });
        if call.timestamp < session.first_timestamp {
            session.first_timestamp = call.timestamp;
        }
        if call.timestamp > session.last_timestamp {
            session.last_timestamp = call.timestamp;
        }
        session.bucket.merge(&bucket);

        model_map.entry(call.model.clone()).or_default().merge(&bucket);

        let analysis = turn_analysis.get(&idx).copied().unwrap_or_else(|| TurnAnalysis {
            category: classify_activity(call),
            retries: 0,
            has_edits: has_edit_tool(&call.tools),
        });
        let activity = activity_map.entry(analysis.category).or_default();
        activity.bucket.merge(&bucket);
        activity.turns += 1;
        activity.retries += analysis.retries;
        if analysis.has_edits {
            activity.edit_turns += 1;
            if analysis.retries == 0 {
                activity.one_shot_turns += 1;
            }
        }

        for tool in &call.tools {
            if let Some(server) =
                tool.strip_prefix("mcp__").and_then(|rest| rest.split("__").next())
            {
                *mcp_map.entry(server.to_string()).or_default() += 1;
            } else {
                *tool_map.entry(tool.clone()).or_default() += 1;
            }
        }
        for command in &call.bash_commands {
            *shell_map.entry(command.clone()).or_default() += 1;
        }
    }

    let mut daily: Vec<_> = daily_map
        .into_iter()
        .map(|(date, (projects, sessions, mut bucket))| {
            bucket.project_count = projects.len();
            bucket.session_count = sessions.len();
            (date, bucket)
        })
        .collect();
    daily.sort_by_key(|(date, _)| *date);

    let mut weekly = aggregate_weekly(&daily);

    let mut projects: Vec<_> = project_map
        .into_iter()
        .map(|(name, (path, sessions, mut bucket))| {
            bucket.session_count = sessions.len();
            ProjectUsage { name, path, bucket }
        })
        .collect();
    projects.sort_by(|a, b| bucket_sort_value(&b.bucket).total_cmp(&bucket_sort_value(&a.bucket)));

    let mut sessions: Vec<_> =
        session_map.into_values().map(SessionUsageAccumulator::into_usage).collect();
    sessions.sort_by(|a, b| bucket_sort_value(&b.bucket).total_cmp(&bucket_sort_value(&a.bucket)));

    let mut models: Vec<_> = model_map
        .into_iter()
        .map(|(model, bucket)| ModelUsage { model, bucket })
        .collect();
    models.sort_by(|a, b| bucket_sort_value(&b.bucket).total_cmp(&bucket_sort_value(&a.bucket)));

    let tools = sorted_named_usage(tool_map);
    let mcp_servers = sorted_named_usage(mcp_map);
    let shell_commands = sorted_named_usage(shell_map);
    let activities = sorted_activities(activity_map);

    let mut grand_total = TokenBucket::default();
    for (_, bucket) in &daily {
        grand_total.merge(bucket);
    }
    grand_total.session_count = sessions.len();
    grand_total.project_count = projects.len();

    debug!(
        "Parsed usage: {} days, {} weeks, {} projects, {} calls, {} total tokens",
        daily.len(),
        weekly.len(),
        projects.len(),
        calls.len(),
        grand_total.total()
    );

    UsageData {
        daily,
        weekly: {
            weekly.sort_by_key(|(date, _)| *date);
            weekly
        },
        projects,
        grand_total,
        calls,
        sessions,
        models,
        activities,
        tools,
        mcp_servers,
        shell_commands,
    }
}

/// Filter an already-parsed `UsageData` by exact-match cross-filters.
///
/// Returns a new `UsageData` with `calls`, `daily`, `weekly`, `projects`,
/// `sessions`, `models`, `activities`, `tools`, `mcp_servers`,
/// `shell_commands`, and `grand_total` re-aggregated from the filtered
/// call set. If no filters are active the original data is returned
/// unchanged via clone.
///
/// This is the in-memory pivot used by:
/// - the TUI cross-filter (Grafana-style click-to-pivot on rows), and
/// - the CLI `--project / --model / --activity / --session` flags after
///   the period+provider+include/exclude pre-pass.
///
/// The activity filter compares against the per-call classified category
/// label (see `ActivityCategory::label()`), case-sensitive.
pub fn filter_usage_data(data: &UsageData, filters: &UsageFilters) -> UsageData {
    if filters.is_empty() {
        return data.clone();
    }
    let filtered_calls: Vec<ProviderCall> = data
        .calls
        .iter()
        .filter(|call| filters.matches(call, classify_activity(call)))
        .cloned()
        .collect();
    aggregate_calls(filtered_calls)
}

#[derive(Debug, Clone, Copy)]
struct TurnAnalysis {
    category: ActivityCategory,
    retries: usize,
    has_edits: bool,
}

#[derive(Debug, Default)]
struct ActivityAccumulator {
    bucket: TokenBucket,
    turns: usize,
    retries: usize,
    edit_turns: usize,
    one_shot_turns: usize,
}

fn analyze_turns(calls: &[ProviderCall]) -> HashMap<usize, TurnAnalysis> {
    let mut sessions: HashMap<String, Vec<usize>> = HashMap::new();
    for (idx, call) in calls.iter().enumerate() {
        let key = format!("{}:{}:{}", call.provider, call.project, call.session_id);
        sessions.entry(key).or_default().push(idx);
    }

    let mut analysis = HashMap::new();
    for mut indices in sessions.into_values() {
        indices.sort_by_key(|idx| calls[*idx].timestamp);

        let mut edit_seen = false;
        let mut bash_after_edit = false;
        for idx in indices {
            let call = &calls[idx];
            let has_edits = has_edit_tool(&call.tools);
            let has_bash = has_bash_tool(call);
            let retries = usize::from(has_edits && bash_after_edit);

            if has_edits {
                edit_seen = true;
                bash_after_edit = false;
            }
            if has_bash && edit_seen {
                bash_after_edit = true;
            }

            analysis.insert(
                idx,
                TurnAnalysis {
                    category: classify_activity(call),
                    retries,
                    has_edits,
                },
            );
        }
    }

    analysis
}

fn classify_activity(call: &ProviderCall) -> ActivityCategory {
    let base = if has_tool(&call.tools, &["EnterPlanMode", "TodoWrite", "Plan"]) {
        ActivityCategory::Planning
    } else if has_tool(&call.tools, &["Agent", "Task"]) {
        ActivityCategory::Delegation
    } else if call.bash_commands.iter().any(|command| command_matches(command, &["git"])) {
        ActivityCategory::Git
    } else if call.bash_commands.iter().any(|command| {
        command_matches(
            command,
            &["cargo", "npm", "pnpm", "yarn", "docker", "kubectl", "make"],
        )
    }) {
        ActivityCategory::BuildDeploy
    } else if has_edit_tool(&call.tools) {
        ActivityCategory::Coding
    } else if has_tool(
        &call.tools,
        &[
            "Read",
            "Glob",
            "Grep",
            "LS",
            "Search",
            "WebSearch",
            "Fetch",
            "ReadFile",
        ],
    ) || call.tools.iter().any(|tool| tool.starts_with("mcp__"))
    {
        ActivityCategory::Exploration
    } else if call.user_message.trim().is_empty() && !has_any_tool(call) {
        ActivityCategory::General
    } else {
        ActivityCategory::Conversation
    };

    refine_activity_with_message(base, &call.user_message)
}

fn refine_activity_with_message(base: ActivityCategory, message: &str) -> ActivityCategory {
    let text = message.to_lowercase();
    if contains_any(&text, &["debug", "bug", "error", "fail", "failing", "fix"]) {
        ActivityCategory::Debugging
    } else if contains_any(&text, &["test", "spec", "coverage", "assert"]) {
        ActivityCategory::Testing
    } else if contains_any(&text, &["refactor", "cleanup", "rename", "simplify"]) {
        ActivityCategory::Refactoring
    } else if contains_any(&text, &["feature", "implement", "add support", "build"]) {
        ActivityCategory::Feature
    } else if contains_any(&text, &["brainstorm", "ideas", "options"]) {
        ActivityCategory::Brainstorming
    } else if contains_any(&text, &["research", "investigate", "look into", "explore"]) {
        ActivityCategory::Exploration
    } else {
        base
    }
}

fn has_any_tool(call: &ProviderCall) -> bool {
    !call.tools.is_empty() || !call.bash_commands.is_empty()
}

fn has_edit_tool(tools: &[String]) -> bool {
    has_tool(
        tools,
        &[
            "Edit",
            "Write",
            "MultiEdit",
            "NotebookEdit",
            "FileEditTool",
            "FileWriteTool",
        ],
    )
}

fn has_bash_tool(call: &ProviderCall) -> bool {
    has_tool(&call.tools, &["Bash", "Shell", "exec_command"]) || !call.bash_commands.is_empty()
}

fn has_tool(tools: &[String], names: &[&str]) -> bool {
    tools.iter().any(|tool| names.iter().any(|name| tool == name))
}

fn command_matches(command: &str, names: &[&str]) -> bool {
    let trimmed = command.trim_start();
    names
        .iter()
        .any(|name| trimmed == *name || trimmed.starts_with(&format!("{name} ")))
}

fn contains_any(text: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| text.contains(needle))
}

fn sorted_activities(map: HashMap<ActivityCategory, ActivityAccumulator>) -> Vec<ActivityUsage> {
    let mut rows: Vec<_> = map
        .into_iter()
        .map(|(category, activity)| ActivityUsage {
            category,
            bucket: activity.bucket,
            turns: activity.turns,
            retries: activity.retries,
            edit_turns: activity.edit_turns,
            one_shot_turns: activity.one_shot_turns,
        })
        .collect();
    rows.sort_by(|a, b| {
        bucket_sort_value(&b.bucket)
            .total_cmp(&bucket_sort_value(&a.bucket))
            .then_with(|| activity_rank(a.category).cmp(&activity_rank(b.category)))
    });
    rows
}

fn activity_rank(category: ActivityCategory) -> usize {
    match category {
        ActivityCategory::Coding => 0,
        ActivityCategory::Debugging => 1,
        ActivityCategory::Feature => 2,
        ActivityCategory::Refactoring => 3,
        ActivityCategory::Testing => 4,
        ActivityCategory::Exploration => 5,
        ActivityCategory::Planning => 6,
        ActivityCategory::Delegation => 7,
        ActivityCategory::Git => 8,
        ActivityCategory::BuildDeploy => 9,
        ActivityCategory::Brainstorming => 10,
        ActivityCategory::Conversation => 11,
        ActivityCategory::General => 12,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanStatus {
    Under,
    Near,
    Over,
}

#[derive(Debug, Clone, Serialize)]
pub struct PlanProjection {
    pub period_start: NaiveDate,
    pub period_end: NaiveDate,
    pub monthly_usd: f64,
    pub spent_usd: f64,
    pub percent_used: f64,
    pub projected_usd: f64,
    pub status: PlanStatus,
    pub remaining_days: i64,
}

pub fn project_plan_usage(
    data: &UsageData,
    plan: &crate::config::UsagePlan,
    today: NaiveDate,
) -> PlanProjection {
    let (period_start, period_end) = billing_period(today, plan.reset_day);
    let spent_usd = plan_cost_for_range(data, plan.provider, period_start, period_end);
    let percent_used = if plan.monthly_usd > 0.0 {
        spent_usd / plan.monthly_usd
    } else {
        0.0
    };
    let mut last_week: Vec<f64> = (0..7)
        .map(|offset| {
            let date = today - Duration::days(offset);
            plan_cost_for_range(data, plan.provider, date, date)
        })
        .collect();
    last_week.sort_by(f64::total_cmp);
    let median_daily = last_week[last_week.len() / 2];
    let remaining_days = (period_end - today).num_days().max(0);
    let projected_usd = spent_usd + median_daily * remaining_days as f64;
    let status = if percent_used > 1.0 {
        PlanStatus::Over
    } else if percent_used >= 0.8 {
        PlanStatus::Near
    } else {
        PlanStatus::Under
    };

    PlanProjection {
        period_start,
        period_end,
        monthly_usd: plan.monthly_usd,
        spent_usd,
        percent_used,
        projected_usd,
        status,
        remaining_days,
    }
}

fn plan_cost_for_range(
    data: &UsageData,
    provider: crate::config::UsagePlanProvider,
    start: NaiveDate,
    end: NaiveDate,
) -> f64 {
    let call_cost = data
        .calls
        .iter()
        .filter(|call| {
            let date = call.timestamp.date_naive();
            date >= start && date <= end && plan_provider_includes(provider, &call.provider)
        })
        .filter_map(|call| call.cost_usd)
        .sum::<f64>();

    if call_cost > 0.0 || provider != crate::config::UsagePlanProvider::All {
        return call_cost;
    }

    data.daily
        .iter()
        .filter(|(date, _)| *date >= start && *date <= end)
        .filter_map(|(_, bucket)| bucket.cost_usd)
        .sum::<f64>()
}

fn plan_provider_includes(provider: crate::config::UsagePlanProvider, call_provider: &str) -> bool {
    match provider {
        crate::config::UsagePlanProvider::All => true,
        crate::config::UsagePlanProvider::Claude => call_provider == "claude",
        crate::config::UsagePlanProvider::Codex => call_provider == "codex",
        crate::config::UsagePlanProvider::Cursor => call_provider == "cursor",
    }
}

pub fn billing_period(today: NaiveDate, reset_day: u8) -> (NaiveDate, NaiveDate) {
    let reset_day = reset_day.clamp(1, 28) as u32;
    let current_reset =
        NaiveDate::from_ymd_opt(today.year(), today.month(), reset_day).unwrap_or(today);
    let start = if today >= current_reset {
        current_reset
    } else {
        add_months(current_reset, -1)
    };
    let end = add_months(start, 1) - Duration::days(1);
    (start, end)
}

fn add_months(date: NaiveDate, months: i32) -> NaiveDate {
    let month_index = date.year() * 12 + date.month() as i32 - 1 + months;
    let year = month_index.div_euclid(12);
    let month = month_index.rem_euclid(12) as u32 + 1;
    NaiveDate::from_ymd_opt(year, month, date.day()).unwrap_or(date)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Impact {
    High,
    Medium,
    Low,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum HealthGrade {
    A,
    B,
    C,
    D,
    F,
}

#[derive(Debug, Clone, Serialize)]
pub struct WasteAction {
    pub label: String,
    pub command: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WasteFinding {
    pub id: String,
    pub title: String,
    pub impact: Impact,
    pub tokens_saved: u64,
    pub details: String,
    pub actions: Vec<WasteAction>,
}

#[derive(Debug, Clone, Serialize)]
pub struct OptimizeResult {
    pub score: u8,
    pub grade: HealthGrade,
    pub potential_tokens_saved: u64,
    pub findings: Vec<WasteFinding>,
}

pub fn optimize_usage(data: &UsageData) -> OptimizeResult {
    let mut findings = Vec::new();
    let read_calls =
        data.tools.iter().find(|tool| tool.name == "Read").map_or(0, |tool| tool.calls);
    let edit_calls =
        data.tools.iter().find(|tool| tool.name == "Edit").map_or(0, |tool| tool.calls);
    let bash_calls =
        data.tools.iter().find(|tool| tool.name == "Bash").map_or(0, |tool| tool.calls);

    if read_calls > edit_calls.saturating_mul(8).max(8) {
        findings.push(WasteFinding {
            id: "low-read-edit-ratio".to_string(),
            title: "Heavy reading before edits".to_string(),
            impact: Impact::Medium,
            tokens_saved: data.grand_total.input_tokens / 10,
            details: format!("{read_calls} reads for {edit_calls} edits"),
            actions: vec![WasteAction {
                label: "Prefer targeted file reads and ripgrep before broad scans".to_string(),
                command: None,
            }],
        });
    }

    let cache_tokens = data.grand_total.cache_creation_tokens + data.grand_total.cache_read_tokens;
    if cache_tokens > data.grand_total.input_tokens.saturating_mul(2) {
        findings.push(WasteFinding {
            id: "cache-bloat".to_string(),
            title: "Large cache footprint".to_string(),
            impact: Impact::Low,
            tokens_saved: cache_tokens / 20,
            details: format!("{} cached tokens", format_tokens_short(cache_tokens)),
            actions: vec![WasteAction {
                label: "Keep prompts and context focused for long sessions".to_string(),
                command: None,
            }],
        });
    }

    if bash_calls > data.grand_total.call_count.saturating_mul(3).max(10) {
        findings.push(WasteFinding {
            id: "bash-output-bloat".to_string(),
            title: "Frequent shell output".to_string(),
            impact: Impact::Medium,
            tokens_saved: data.grand_total.output_tokens / 12,
            details: format!("{bash_calls} shell calls"),
            actions: vec![WasteAction {
                label: "Pipe large command output through focused filters".to_string(),
                command: Some("rg <pattern> <path>".to_string()),
            }],
        });
    }

    let agent_calls =
        data.tools.iter().find(|tool| tool.name == "Agent").map_or(0, |tool| tool.calls);
    if agent_calls > data.sessions.len().saturating_mul(5).max(5) {
        findings.push(WasteFinding {
            id: "ghost-agents".to_string(),
            title: "High agent delegation count".to_string(),
            impact: Impact::High,
            tokens_saved: data.grand_total.total() / 8,
            details: format!(
                "{agent_calls} agent calls across {} sessions",
                data.sessions.len()
            ),
            actions: vec![WasteAction {
                label: "Close unused agent tasks and keep delegated scope narrow".to_string(),
                command: None,
            }],
        });
    }

    findings.sort_by(|a, b| {
        impact_weight(b.impact)
            .cmp(&impact_weight(a.impact))
            .then_with(|| b.tokens_saved.cmp(&a.tokens_saved))
    });
    let penalties: u8 = findings
        .iter()
        .map(|finding| match finding.impact {
            Impact::High => 15,
            Impact::Medium => 7,
            Impact::Low => 3,
        })
        .sum();
    let score = 100_u8.saturating_sub(penalties).max(20);
    let grade = health_grade(score);
    let potential_tokens_saved = findings.iter().map(|finding| finding.tokens_saved).sum();

    OptimizeResult {
        score,
        grade,
        potential_tokens_saved,
        findings,
    }
}

fn impact_weight(impact: Impact) -> u8 {
    match impact {
        Impact::High => 3,
        Impact::Medium => 2,
        Impact::Low => 1,
    }
}

fn health_grade(score: u8) -> HealthGrade {
    match score {
        90..=100 => HealthGrade::A,
        75..=89 => HealthGrade::B,
        55..=74 => HealthGrade::C,
        30..=54 => HealthGrade::D,
        _ => HealthGrade::F,
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ModelComparison {
    pub model: String,
    pub bucket: TokenBucket,
    pub calls: usize,
    pub edit_turns: usize,
    pub one_shot_turns: usize,
    pub retries: usize,
    pub cost_per_call: Option<f64>,
    pub tokens_per_call: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct CompareResult {
    pub models: Vec<ModelComparison>,
    pub winner: Option<String>,
    pub low_data: bool,
}

pub fn compare_models(data: &UsageData) -> CompareResult {
    let mut rows: Vec<ModelComparison> = data
        .models
        .iter()
        .map(|model| {
            let calls = model.bucket.call_count.max(1);
            let edit_turns = data
                .calls
                .iter()
                .filter(|call| call.model == model.model && has_edit_tool(&call.tools))
                .count();
            let retries = data
                .calls
                .iter()
                .filter(|call| call.model == model.model)
                .filter(|call| call.user_message.to_lowercase().contains("fix"))
                .count();
            let one_shot_turns = edit_turns.saturating_sub(retries);
            ModelComparison {
                model: model.model.clone(),
                bucket: model.bucket.clone(),
                calls,
                edit_turns,
                one_shot_turns,
                retries,
                cost_per_call: model.bucket.cost_usd.map(|cost| cost / calls as f64),
                tokens_per_call: model.bucket.total() / calls as u64,
            }
        })
        .collect();
    rows.sort_by(|a, b| {
        b.one_shot_turns
            .cmp(&a.one_shot_turns)
            .then_with(|| a.retries.cmp(&b.retries))
            .then_with(|| {
                a.cost_per_call
                    .unwrap_or(f64::MAX)
                    .total_cmp(&b.cost_per_call.unwrap_or(f64::MAX))
            })
    });
    let low_data = rows.iter().map(|row| row.calls).sum::<usize>() < 5 || rows.len() < 2;
    let winner = rows.first().map(|row| row.model.clone());
    CompareResult {
        models: rows,
        winner,
        low_data,
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct YieldResult {
    pub productive_usd: f64,
    pub reverted_usd: f64,
    pub abandoned_usd: f64,
    pub productive_sessions: usize,
    pub reverted_sessions: usize,
    pub abandoned_sessions: usize,
}

pub fn analyze_yield(data: &UsageData) -> YieldResult {
    let mut result = YieldResult {
        productive_usd: 0.0,
        reverted_usd: 0.0,
        abandoned_usd: 0.0,
        productive_sessions: 0,
        reverted_sessions: 0,
        abandoned_sessions: 0,
    };

    for session in &data.sessions {
        let cost = session.bucket.cost_usd.unwrap_or(0.0);
        let has_git = data.calls.iter().any(|call| {
            call.session_id == session.session_id
                && call.bash_commands.iter().any(|cmd| cmd.trim_start().starts_with("git "))
        });
        let reverted = data.calls.iter().any(|call| {
            call.session_id == session.session_id
                && call.user_message.to_lowercase().contains("revert")
        });
        if reverted {
            result.reverted_sessions += 1;
            result.reverted_usd += cost;
        } else if has_git {
            result.productive_sessions += 1;
            result.productive_usd += cost;
        } else {
            result.abandoned_sessions += 1;
            result.abandoned_usd += cost;
        }
    }

    result
}

struct SessionUsageAccumulator {
    provider: String,
    project: String,
    session_id: String,
    first_timestamp: DateTime<Local>,
    last_timestamp: DateTime<Local>,
    bucket: TokenBucket,
}

impl SessionUsageAccumulator {
    fn into_usage(mut self) -> SessionUsage {
        self.bucket.session_count = 1;
        SessionUsage {
            provider: self.provider,
            project: self.project,
            session_id: self.session_id,
            first_timestamp: self.first_timestamp,
            last_timestamp: self.last_timestamp,
            bucket: self.bucket,
        }
    }
}

fn aggregate_weekly(daily: &[(NaiveDate, TokenBucket)]) -> Vec<(NaiveDate, TokenBucket)> {
    let mut weekly_map: HashMap<NaiveDate, TokenBucket> = HashMap::new();

    for (date, bucket) in daily {
        let week_start = week_start_date(*date);
        weekly_map.entry(week_start).or_default().merge(bucket);
    }

    weekly_map.into_iter().collect()
}

fn sorted_named_usage(map: HashMap<String, usize>) -> Vec<NamedUsage> {
    let mut rows: Vec<_> =
        map.into_iter().map(|(name, calls)| NamedUsage { name, calls }).collect();
    rows.sort_by(|a, b| b.calls.cmp(&a.calls).then_with(|| a.name.cmp(&b.name)));
    rows
}

fn project_matches(call: &ProviderCall, include: &[String], exclude: &[String]) -> bool {
    let name = call.project.to_lowercase();
    let path = call.project_path.to_lowercase();

    if !include.is_empty() {
        let matched = include
            .iter()
            .map(|pattern| pattern.to_lowercase())
            .any(|pattern| name.contains(&pattern) || path.contains(&pattern));
        if !matched {
            return false;
        }
    }

    if !exclude.is_empty() {
        let matched = exclude
            .iter()
            .map(|pattern| pattern.to_lowercase())
            .any(|pattern| name.contains(&pattern) || path.contains(&pattern));
        if matched {
            return false;
        }
    }

    true
}

fn date_range_for_period(period: &UsagePeriod) -> Option<(DateTime<Local>, DateTime<Local>)> {
    let now = Local::now();
    let today = now.date_naive();

    match period {
        UsagePeriod::All => None,
        UsagePeriod::Today => Some(day_bounds(today)),
        UsagePeriod::Week => Some((start_of_day(today - Duration::days(6)), end_of_day(today))),
        UsagePeriod::ThirtyDays => {
            Some((start_of_day(today - Duration::days(29)), end_of_day(today)))
        }
        UsagePeriod::LastNDays(n) => {
            let n = (*n).max(1);
            Some((
                start_of_day(today - Duration::days(i64::from(n - 1))),
                end_of_day(today),
            ))
        }
        UsagePeriod::Month => {
            let first = NaiveDate::from_ymd_opt(today.year(), today.month(), 1).unwrap_or(today);
            Some((start_of_day(first), end_of_day(today)))
        }
        UsagePeriod::SpecificMonth(anchor) => {
            let first =
                NaiveDate::from_ymd_opt(anchor.year(), anchor.month(), 1).unwrap_or(*anchor);
            let last = last_day_of_month(anchor.year(), anchor.month());
            Some((start_of_day(first), end_of_day(last)))
        }
        UsagePeriod::SpecificQuarter(year, q) => {
            let (first, last) = quarter_bounds(*year, *q);
            Some((start_of_day(first), end_of_day(last)))
        }
        UsagePeriod::YearToDate => {
            let first = NaiveDate::from_ymd_opt(today.year(), 1, 1).unwrap_or(today);
            Some((start_of_day(first), end_of_day(today)))
        }
        UsagePeriod::Custom { from, to } => Some((start_of_day(*from), end_of_day(*to))),
    }
}

/// Last calendar day of a `(year, month)`. Falls back to day 28 only
/// for genuinely impossible chrono inputs (year out of range).
pub fn last_day_of_month(year: i32, month: u32) -> NaiveDate {
    let (next_year, next_month) = if month == 12 {
        (year + 1, 1)
    } else {
        (year, month + 1)
    };
    let next_first = NaiveDate::from_ymd_opt(next_year, next_month, 1)
        .unwrap_or_else(|| NaiveDate::from_ymd_opt(year, month, 28).expect("valid 28th"));
    next_first - Duration::days(1)
}

/// First and last calendar days of `quarter` (1..=4) within `year`.
/// Out-of-range quarters are clamped to Q1.
pub fn quarter_bounds(year: i32, quarter: u8) -> (NaiveDate, NaiveDate) {
    let q = quarter.clamp(1, 4);
    let start_month = (u32::from(q) - 1) * 3 + 1;
    let end_month = start_month + 2;
    let first = NaiveDate::from_ymd_opt(year, start_month, 1)
        .unwrap_or_else(|| NaiveDate::from_ymd_opt(year, 1, 1).expect("valid Jan 1"));
    let last = last_day_of_month(year, end_month);
    (first, last)
}

/// Quarter (1..=4) containing `date`.
pub fn quarter_of(date: NaiveDate) -> u8 {
    ((date.month0() / 3) + 1) as u8
}

fn day_bounds(date: NaiveDate) -> (DateTime<Local>, DateTime<Local>) {
    (start_of_day(date), end_of_day(date))
}

fn start_of_day(date: NaiveDate) -> DateTime<Local> {
    Local
        .from_local_datetime(&date.and_hms_opt(0, 0, 0).expect("valid start of day"))
        .single()
        .unwrap_or_else(|| {
            Local.from_utc_datetime(&date.and_hms_opt(0, 0, 0).expect("valid start of day"))
        })
}

fn end_of_day(date: NaiveDate) -> DateTime<Local> {
    Local
        .from_local_datetime(&date.and_hms_milli_opt(23, 59, 59, 999).expect("valid end of day"))
        .single()
        .unwrap_or_else(|| {
            Local.from_utc_datetime(
                &date.and_hms_milli_opt(23, 59, 59, 999).expect("valid end of day"),
            )
        })
}

fn parse_timestamp(timestamp: &str) -> Option<DateTime<Local>> {
    DateTime::parse_from_rfc3339(timestamp).map(|dt| dt.with_timezone(&Local)).ok()
}

fn extract_claude_user_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(items)) => items
            .iter()
            .filter(|item| item.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|item| item.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join(" "),
        _ => String::new(),
    }
}

fn extract_claude_tools(content: Option<&Value>) -> Vec<String> {
    let Some(Value::Array(items)) = content else {
        return Vec::new();
    };

    items
        .iter()
        .filter(|item| item.get("type").and_then(Value::as_str) == Some("tool_use"))
        .filter_map(|item| item.get("name").and_then(Value::as_str))
        .map(ToString::to_string)
        .collect()
}

fn extract_bash_commands_from_claude_content(content: Option<&Value>) -> Vec<String> {
    let Some(Value::Array(items)) = content else {
        return Vec::new();
    };

    items
        .iter()
        .filter(|item| {
            item.get("type").and_then(Value::as_str) == Some("tool_use")
                && matches!(
                    item.get("name").and_then(Value::as_str),
                    Some("Bash" | "Shell")
                )
        })
        .filter_map(|item| {
            item.get("input").and_then(|input| input.get("command")).and_then(Value::as_str)
        })
        .map(ToString::to_string)
        .collect()
}

fn is_valid_codex_session(path: &Path) -> bool {
    let Ok(content) = std::fs::read_to_string(path) else {
        return false;
    };
    let Some(first_line) = content.lines().find(|line| !line.trim().is_empty()) else {
        return false;
    };
    let Ok(entry) = serde_json::from_str::<CodexEntry>(first_line) else {
        return false;
    };

    entry.entry_type == "session_meta"
        && entry
            .payload
            .and_then(|payload| payload.originator)
            .is_some_and(|originator| originator.to_lowercase().starts_with("codex"))
}

fn is_date_component(path: &Path, len: usize) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.len() == len && name.chars().all(|ch| ch.is_ascii_digit()))
}

fn sanitize_codex_project(cwd: &str) -> String {
    cwd.trim_start_matches('/').replace('/', "-")
}

fn normalize_codex_tool(raw: &str) -> &str {
    match raw {
        "exec_command" => "Bash",
        "read_file" => "Read",
        "write_file" | "apply_diff" | "apply_patch" => "Edit",
        "spawn_agent" | "close_agent" | "wait_agent" => "Agent",
        "read_dir" => "Glob",
        _ => raw,
    }
}

fn resolve_codex_model(payload: Option<&CodexPayload>, session_model: Option<&str>) -> String {
    payload
        .and_then(|payload| payload.model.as_deref())
        .or_else(|| {
            payload
                .and_then(|payload| payload.info.as_ref())
                .and_then(|info| info.model.as_deref())
        })
        .or_else(|| {
            payload
                .and_then(|payload| payload.info.as_ref())
                .and_then(|info| info.model_name.as_deref())
        })
        .or(session_model)
        .unwrap_or("gpt-5")
        .to_string()
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
    if name.starts_with("-agents-in-a-box-worktrees-") {
        name = name.replacen("-agents-in-a-box-worktrees-", "worktree/", 1);
    }
    if name.is_empty() {
        raw.to_string()
    } else {
        name
    }
}

fn unsanitize_project_path(raw: &str) -> String {
    raw.replace('-', "/")
}

fn week_start_date(date: NaiveDate) -> NaiveDate {
    let days_since_monday = date.weekday().num_days_from_monday();
    date - Duration::days(i64::from(days_since_monday))
}

fn whoami_username() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "unknown".to_string())
}

fn merge_cost(left: Option<f64>, right: Option<f64>) -> Option<f64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left + right),
        (Some(left), None) => Some(left),
        (None, Some(right)) => Some(right),
        (None, None) => None,
    }
}

fn bucket_sort_value(bucket: &TokenBucket) -> f64 {
    bucket.cost_usd.unwrap_or(bucket.total() as f64)
}

fn estimate_cost_usd(
    model: &str,
    input_tokens: u64,
    output_tokens: u64,
    cache_creation_tokens: u64,
    cache_read_tokens: u64,
    reasoning_tokens: u64,
) -> Option<f64> {
    let rates = model_rates(model)?;
    Some(
        input_tokens as f64 * rates.input
            + (output_tokens + reasoning_tokens) as f64 * rates.output
            + cache_creation_tokens as f64 * rates.cache_write
            + cache_read_tokens as f64 * rates.cache_read,
    )
}

struct ModelRates {
    input: f64,
    output: f64,
    cache_write: f64,
    cache_read: f64,
}

fn model_rates(model: &str) -> Option<ModelRates> {
    let canonical = canonical_model_name(model);
    let (input_per_million, output_per_million) = if canonical.starts_with("claude-opus") {
        (15.0, 75.0)
    } else if canonical.starts_with("claude-sonnet") || canonical.starts_with("claude-3-5-sonnet") {
        (3.0, 15.0)
    } else if canonical.starts_with("claude-haiku") || canonical.starts_with("claude-3-5-haiku") {
        (0.8, 4.0)
    } else if canonical.starts_with("gpt-5")
        || canonical.starts_with("gpt-4.1")
        || canonical.starts_with("gpt-4o")
    {
        (1.25, 10.0)
    } else {
        return None;
    };

    let input = input_per_million / 1_000_000.0;
    let output = output_per_million / 1_000_000.0;
    Some(ModelRates {
        input,
        output,
        cache_write: input * 1.25,
        cache_read: input * 0.1,
    })
}

fn canonical_model_name(model: &str) -> String {
    let without_prefix = model
        .split('@')
        .next()
        .unwrap_or(model)
        .trim_start_matches("anthropic/")
        .trim_start_matches("openai/")
        .to_string();

    if without_prefix
        .rsplit('-')
        .next()
        .is_some_and(|suffix| suffix.len() == 8 && suffix.chars().all(|ch| ch.is_ascii_digit()))
    {
        without_prefix
            .rsplit_once('-')
            .map_or(without_prefix.clone(), |(name, _)| name.to_string())
    } else {
        without_prefix
    }
}

/// Format a token count in human-readable form (e.g., "1.2B", "456M", "12K").
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

/// Format a token count with commas.
#[allow(dead_code)]
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn write_file(path: &Path, content: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, content).unwrap();
    }

    fn roots(claude_projects_dir: Option<PathBuf>, codex_dir: Option<PathBuf>) -> UsageSourceRoots {
        UsageSourceRoots {
            claude_projects_dir,
            codex_dir,
        }
    }

    fn provider_call(
        provider: &str,
        session_id: &str,
        timestamp: &str,
        message: &str,
        tools: &[&str],
        bash_commands: &[&str],
        input_tokens: u64,
    ) -> ProviderCall {
        ProviderCall {
            provider: provider.to_string(),
            model: if provider == "codex" {
                "gpt-5".to_string()
            } else {
                "claude-sonnet-4-5".to_string()
            },
            session_id: session_id.to_string(),
            project: "alpha".to_string(),
            project_path: "/work/alpha".to_string(),
            timestamp: parse_timestamp(timestamp).unwrap(),
            input_tokens,
            cache_creation_tokens: 0,
            cache_read_tokens: 0,
            output_tokens: 10,
            reasoning_tokens: 0,
            cost_usd: None,
            tools: tools.iter().map(|tool| tool.to_string()).collect(),
            bash_commands: bash_commands.iter().map(|command| command.to_string()).collect(),
            user_message: message.to_string(),
        }
    }

    #[test]
    fn parses_claude_fixture_and_preserves_daily_project_totals() {
        let temp = tempdir().unwrap();
        let claude_projects = temp.path().join(".claude/projects");
        let project_dir = claude_projects.join("-Users-stevie-agents-in-a-box");
        write_file(
            &project_dir.join("session.jsonl"),
            r#"{"type":"user","timestamp":"2026-04-10T09:00:00Z","sessionId":"s1","message":{"role":"user","content":"fix parser"}}
{"type":"assistant","timestamp":"2026-04-10T09:00:05Z","sessionId":"s1","message":{"role":"assistant","model":"claude-sonnet-4-5","content":[{"type":"tool_use","name":"Read","input":{"file_path":"src/lib.rs"}},{"type":"tool_use","name":"Bash","input":{"command":"cargo test"}}],"usage":{"input_tokens":1000,"cache_creation_input_tokens":200,"cache_read_input_tokens":300,"output_tokens":400}}}
"#,
        );

        let data = parse_usage_for_with_roots(
            UsageQuery {
                provider_filter: UsageProviderFilter::Claude,
                period: UsagePeriod::All,
                include_projects: Vec::new(),
                exclude_projects: Vec::new(),
                filters: UsageFilters::default(),
            },
            &roots(Some(claude_projects), None),
        );

        assert_eq!(data.daily.len(), 1);
        assert_eq!(data.projects.len(), 1);
        assert_eq!(data.grand_total.input_tokens, 1000);
        assert_eq!(data.grand_total.cache_creation_tokens, 200);
        assert_eq!(data.grand_total.cache_read_tokens, 300);
        assert_eq!(data.grand_total.output_tokens, 400);
        assert_eq!(data.grand_total.call_count, 1);
        assert_eq!(data.grand_total.session_count, 1);
        assert!(data.tools.iter().any(|tool| tool.name == "Read" && tool.calls == 1));
        assert!(data.shell_commands.iter().any(|cmd| cmd.name == "cargo test" && cmd.calls == 1));
    }

    #[test]
    fn parses_codex_fixture_and_normalizes_cached_input_tokens() {
        let temp = tempdir().unwrap();
        let codex_dir = temp.path().join(".codex");
        let rollout = codex_dir.join("sessions/2026/04/10/rollout-1.jsonl");
        write_file(
            &rollout,
            r#"{"type":"session_meta","payload":{"originator":"codex_cli","session_id":"codex-1","cwd":"/Users/stevie/work/project","model":"gpt-5"}}
{"type":"response_item","timestamp":"2026-04-10T10:00:00Z","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"implement thing"}]}}
{"type":"response_item","timestamp":"2026-04-10T10:00:01Z","payload":{"type":"function_call","name":"exec_command"}}
{"type":"event_msg","timestamp":"2026-04-10T10:00:02Z","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":1000,"cached_input_tokens":400,"output_tokens":200,"reasoning_output_tokens":50},"total_token_usage":{"total_tokens":1650}}}}
"#,
        );

        let data = parse_usage_for_with_roots(
            UsageQuery {
                provider_filter: UsageProviderFilter::Codex,
                period: UsagePeriod::All,
                include_projects: Vec::new(),
                exclude_projects: Vec::new(),
                filters: UsageFilters::default(),
            },
            &roots(None, Some(codex_dir)),
        );

        assert_eq!(data.calls.len(), 1);
        let call = &data.calls[0];
        assert_eq!(call.input_tokens, 600);
        assert_eq!(call.cache_read_tokens, 400);
        assert_eq!(call.output_tokens, 200);
        assert_eq!(call.reasoning_tokens, 50);
        assert_eq!(call.tools, vec!["Bash"]);
        assert_eq!(call.user_message, "implement thing");
    }

    #[test]
    fn codex_last_usage_advances_total_delta_baseline() {
        let temp = tempdir().unwrap();
        let codex_dir = temp.path().join(".codex");
        let rollout = codex_dir.join("sessions/2026/04/10/rollout-1.jsonl");
        write_file(
            &rollout,
            r#"{"type":"session_meta","payload":{"originator":"codex_cli","session_id":"codex-1","cwd":"/Users/stevie/work/project","model":"gpt-5"}}
{"type":"response_item","timestamp":"2026-04-10T10:00:00Z","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"first"}]}}
{"type":"event_msg","timestamp":"2026-04-10T10:00:01Z","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":1000,"cached_input_tokens":400,"output_tokens":200,"reasoning_output_tokens":50},"total_token_usage":{"input_tokens":1000,"cached_input_tokens":400,"output_tokens":200,"reasoning_output_tokens":50,"total_tokens":1650}}}}
{"type":"response_item","timestamp":"2026-04-10T10:00:02Z","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"second"}]}}
{"type":"event_msg","timestamp":"2026-04-10T10:00:03Z","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":1300,"cached_input_tokens":450,"output_tokens":260,"reasoning_output_tokens":70,"total_tokens":2080}}}}
"#,
        );

        let data = parse_usage_for_with_roots(
            UsageQuery {
                provider_filter: UsageProviderFilter::Codex,
                period: UsagePeriod::All,
                include_projects: Vec::new(),
                exclude_projects: Vec::new(),
                filters: UsageFilters::default(),
            },
            &roots(None, Some(codex_dir)),
        );

        assert_eq!(data.calls.len(), 2);
        assert_eq!(data.grand_total.input_tokens, 850);
        assert_eq!(data.grand_total.cache_read_tokens, 450);
        assert_eq!(data.grand_total.output_tokens, 260);
        assert_eq!(data.grand_total.reasoning_tokens, 70);
    }

    #[test]
    fn date_filter_uses_assistant_call_timestamp() {
        let temp = tempdir().unwrap();
        let claude_projects = temp.path().join(".claude/projects");
        let project_dir = claude_projects.join("proj");
        write_file(
            &project_dir.join("session.jsonl"),
            r#"{"type":"user","timestamp":"2026-04-09T23:59:00Z","sessionId":"s1","message":{"role":"user","content":"late ask"}}
{"type":"assistant","timestamp":"2026-04-10T00:00:05Z","sessionId":"s1","message":{"role":"assistant","model":"claude-sonnet-4-5","content":[],"usage":{"input_tokens":1,"output_tokens":2}}}
"#,
        );

        let day = NaiveDate::from_ymd_opt(2026, 4, 10).unwrap();
        let data = parse_usage_for_with_roots(
            UsageQuery {
                provider_filter: UsageProviderFilter::Claude,
                period: UsagePeriod::Custom { from: day, to: day },
                include_projects: Vec::new(),
                exclude_projects: Vec::new(),
                filters: UsageFilters::default(),
            },
            &roots(Some(claude_projects), None),
        );

        assert_eq!(data.grand_total.call_count, 1);
        assert_eq!(data.daily[0].0, day);
    }

    #[test]
    fn custom_ranges_are_inclusive() {
        let temp = tempdir().unwrap();
        let claude_projects = temp.path().join(".claude/projects");
        let project_dir = claude_projects.join("proj");
        write_file(
            &project_dir.join("session.jsonl"),
            r#"{"type":"assistant","timestamp":"2026-04-10T23:59:59+01:00","sessionId":"s1","message":{"role":"assistant","model":"claude-sonnet-4-5","content":[],"usage":{"input_tokens":1,"output_tokens":1}}}
{"type":"assistant","timestamp":"2026-04-11T00:00:00+01:00","sessionId":"s1","message":{"role":"assistant","model":"claude-sonnet-4-5","content":[],"usage":{"input_tokens":10,"output_tokens":10}}}
"#,
        );

        let day = NaiveDate::from_ymd_opt(2026, 4, 10).unwrap();
        let data = parse_usage_for_with_roots(
            UsageQuery {
                provider_filter: UsageProviderFilter::Claude,
                period: UsagePeriod::Custom { from: day, to: day },
                include_projects: Vec::new(),
                exclude_projects: Vec::new(),
                filters: UsageFilters::default(),
            },
            &roots(Some(claude_projects), None),
        );

        assert_eq!(data.grand_total.call_count, 1);
        assert_eq!(data.grand_total.input_tokens, 1);
    }

    #[test]
    fn inverted_custom_ranges_return_no_usage() {
        let temp = tempdir().unwrap();
        let claude_projects = temp.path().join(".claude/projects");
        let project_dir = claude_projects.join("proj");
        write_file(
            &project_dir.join("session.jsonl"),
            r#"{"type":"assistant","timestamp":"2026-04-10T12:00:00+01:00","sessionId":"s1","message":{"role":"assistant","model":"claude-sonnet-4-5","content":[],"usage":{"input_tokens":1,"output_tokens":1}}}
"#,
        );

        let from = NaiveDate::from_ymd_opt(2026, 4, 11).unwrap();
        let to = NaiveDate::from_ymd_opt(2026, 4, 10).unwrap();
        let data = parse_usage_for_with_roots(
            UsageQuery {
                provider_filter: UsageProviderFilter::Claude,
                period: UsagePeriod::Custom { from, to },
                include_projects: Vec::new(),
                exclude_projects: Vec::new(),
                filters: UsageFilters::default(),
            },
            &roots(Some(claude_projects), None),
        );

        assert_eq!(data.grand_total.call_count, 0);
        assert!(data.calls.is_empty());
    }

    #[test]
    fn include_and_exclude_filters_apply_before_aggregation() {
        let temp = tempdir().unwrap();
        let claude_projects = temp.path().join(".claude/projects");
        write_file(
            &claude_projects.join("alpha-keep/session.jsonl"),
            r#"{"type":"assistant","timestamp":"2026-04-10T09:00:00Z","sessionId":"s1","message":{"role":"assistant","model":"claude-sonnet-4-5","content":[],"usage":{"input_tokens":1,"output_tokens":1}}}
"#,
        );
        write_file(
            &claude_projects.join("alpha-scratch/session.jsonl"),
            r#"{"type":"assistant","timestamp":"2026-04-10T09:00:00Z","sessionId":"s2","message":{"role":"assistant","model":"claude-sonnet-4-5","content":[],"usage":{"input_tokens":10,"output_tokens":10}}}
"#,
        );
        write_file(
            &claude_projects.join("beta/session.jsonl"),
            r#"{"type":"assistant","timestamp":"2026-04-10T09:00:00Z","sessionId":"s3","message":{"role":"assistant","model":"claude-sonnet-4-5","content":[],"usage":{"input_tokens":100,"output_tokens":100}}}
"#,
        );

        let data = parse_usage_for_with_roots(
            UsageQuery {
                provider_filter: UsageProviderFilter::Claude,
                period: UsagePeriod::All,
                include_projects: vec!["alpha".to_string()],
                exclude_projects: vec!["scratch".to_string()],
                filters: UsageFilters::default(),
            },
            &roots(Some(claude_projects), None),
        );

        assert_eq!(data.projects.len(), 1);
        assert_eq!(data.projects[0].name, "alpha-keep");
        assert_eq!(data.grand_total.input_tokens, 1);
    }

    #[test]
    fn classifier_covers_core_tool_and_keyword_categories() {
        let cases = [
            (
                provider_call(
                    "claude",
                    "s1",
                    "2026-04-10T09:00:00+01:00",
                    "change code",
                    &["Edit"],
                    &[],
                    1,
                ),
                ActivityCategory::Coding,
            ),
            (
                provider_call(
                    "claude",
                    "s1",
                    "2026-04-10T09:01:00+01:00",
                    "fix parser bug",
                    &["Edit"],
                    &[],
                    1,
                ),
                ActivityCategory::Debugging,
            ),
            (
                provider_call(
                    "claude",
                    "s1",
                    "2026-04-10T09:02:00+01:00",
                    "implement feature",
                    &[],
                    &[],
                    1,
                ),
                ActivityCategory::Feature,
            ),
            (
                provider_call(
                    "claude",
                    "s1",
                    "2026-04-10T09:03:00+01:00",
                    "refactor usage cleanup",
                    &[],
                    &[],
                    1,
                ),
                ActivityCategory::Refactoring,
            ),
            (
                provider_call(
                    "claude",
                    "s1",
                    "2026-04-10T09:04:00+01:00",
                    "add test coverage",
                    &[],
                    &[],
                    1,
                ),
                ActivityCategory::Testing,
            ),
            (
                provider_call(
                    "claude",
                    "s1",
                    "2026-04-10T09:05:00+01:00",
                    "",
                    &["Read"],
                    &[],
                    1,
                ),
                ActivityCategory::Exploration,
            ),
            (
                provider_call(
                    "claude",
                    "s1",
                    "2026-04-10T09:06:00+01:00",
                    "",
                    &["TodoWrite"],
                    &[],
                    1,
                ),
                ActivityCategory::Planning,
            ),
            (
                provider_call(
                    "claude",
                    "s1",
                    "2026-04-10T09:07:00+01:00",
                    "",
                    &["Agent"],
                    &[],
                    1,
                ),
                ActivityCategory::Delegation,
            ),
            (
                provider_call(
                    "claude",
                    "s1",
                    "2026-04-10T09:08:00+01:00",
                    "",
                    &["Bash"],
                    &["git status"],
                    1,
                ),
                ActivityCategory::Git,
            ),
            (
                provider_call(
                    "claude",
                    "s1",
                    "2026-04-10T09:09:00+01:00",
                    "",
                    &["Bash"],
                    &["cargo test usage"],
                    1,
                ),
                ActivityCategory::BuildDeploy,
            ),
            (
                provider_call(
                    "claude",
                    "s1",
                    "2026-04-10T09:10:00+01:00",
                    "brainstorm options",
                    &[],
                    &[],
                    1,
                ),
                ActivityCategory::Brainstorming,
            ),
            (
                provider_call(
                    "claude",
                    "s1",
                    "2026-04-10T09:11:00+01:00",
                    "sounds reasonable",
                    &[],
                    &[],
                    1,
                ),
                ActivityCategory::Conversation,
            ),
            (
                provider_call("claude", "s1", "2026-04-10T09:12:00+01:00", "", &[], &[], 1),
                ActivityCategory::General,
            ),
        ];

        for (call, expected) in cases {
            assert_eq!(classify_activity(&call), expected);
        }
    }

    #[test]
    fn retry_analysis_counts_edit_only_and_edit_bash_edit_cycle() {
        let calls = vec![
            provider_call(
                "claude",
                "s1",
                "2026-04-10T09:00:00+01:00",
                "implement feature",
                &["Edit"],
                &[],
                10,
            ),
            provider_call(
                "claude",
                "s1",
                "2026-04-10T09:01:00+01:00",
                "",
                &["Bash"],
                &["cargo test usage"],
                10,
            ),
            provider_call(
                "claude",
                "s1",
                "2026-04-10T09:02:00+01:00",
                "fix failing test",
                &["Edit"],
                &[],
                10,
            ),
        ];

        let analysis = analyze_turns(&calls);
        assert_eq!(analysis.get(&0).unwrap().retries, 0);
        assert!(analysis.get(&0).unwrap().has_edits);
        assert_eq!(analysis.get(&2).unwrap().retries, 1);

        let data = aggregate_calls(calls);
        let edit_turns: usize = data.activities.iter().map(|activity| activity.edit_turns).sum();
        let retries: usize = data.activities.iter().map(|activity| activity.retries).sum();
        let one_shot_turns: usize =
            data.activities.iter().map(|activity| activity.one_shot_turns).sum();

        assert_eq!(edit_turns, 2);
        assert_eq!(retries, 1);
        assert_eq!(one_shot_turns, 1);
    }

    #[test]
    fn aggregation_tracks_activity_totals_from_mixed_provider_calls() {
        let data = aggregate_calls(vec![
            provider_call(
                "claude",
                "claude-1",
                "2026-04-10T09:00:00+01:00",
                "implement feature",
                &["Edit"],
                &[],
                100,
            ),
            provider_call(
                "codex",
                "codex-1",
                "2026-04-10T10:00:00+01:00",
                "research parser",
                &["Read"],
                &[],
                200,
            ),
        ]);

        assert_eq!(data.grand_total.call_count, 2);
        assert_eq!(data.grand_total.input_tokens, 300);
        assert_eq!(data.models.len(), 2);
        assert!(data.projects.iter().any(|project| project.name == "alpha"));
        assert!(data.tools.iter().any(|tool| tool.name == "Edit"));
        assert!(data.tools.iter().any(|tool| tool.name == "Read"));

        let feature = data
            .activities
            .iter()
            .find(|activity| activity.category == ActivityCategory::Feature)
            .unwrap();
        assert_eq!(feature.turns, 1);
        assert_eq!(feature.bucket.input_tokens, 100);

        let exploration = data
            .activities
            .iter()
            .find(|activity| activity.category == ActivityCategory::Exploration)
            .unwrap();
        assert_eq!(exploration.turns, 1);
        assert_eq!(exploration.bucket.input_tokens, 200);
    }

    #[test]
    fn plan_projection_covers_under_near_and_over_status() {
        let today = NaiveDate::from_ymd_opt(2026, 4, 29).unwrap();
        let mut data = UsageData::default();
        data.daily = vec![(
            today,
            TokenBucket {
                cost_usd: Some(10.0),
                ..TokenBucket::default()
            },
        )];
        let mut plan = crate::config::UsagePlan {
            id: crate::config::UsagePlanId::Custom,
            monthly_usd: 100.0,
            provider: crate::config::UsagePlanProvider::All,
            reset_day: 12,
            set_at: "2026-04-29T00:00:00Z".to_string(),
        };

        assert_eq!(
            project_plan_usage(&data, &plan, today).status,
            PlanStatus::Under
        );

        plan.monthly_usd = 12.0;
        assert_eq!(
            project_plan_usage(&data, &plan, today).status,
            PlanStatus::Near
        );

        plan.monthly_usd = 5.0;
        assert_eq!(
            project_plan_usage(&data, &plan, today).status,
            PlanStatus::Over
        );
    }

    #[test]
    fn plan_projection_scopes_provider_and_counts_missing_days_as_zero() {
        let today = NaiveDate::from_ymd_opt(2026, 4, 29).unwrap();
        let mut data = UsageData::default();
        data.calls = vec![
            ProviderCall {
                provider: "claude".to_string(),
                model: "claude-sonnet-4-5".to_string(),
                session_id: "s1".to_string(),
                project: "alpha".to_string(),
                project_path: "/work/alpha".to_string(),
                timestamp: Local.with_ymd_and_hms(2026, 4, 29, 10, 0, 0).unwrap(),
                cost_usd: Some(70.0),
                ..provider_call(
                    "claude",
                    "s1",
                    "2026-04-29T10:00:00+01:00",
                    "build",
                    &[],
                    &[],
                    1,
                )
            },
            ProviderCall {
                provider: "codex".to_string(),
                model: "gpt-5".to_string(),
                session_id: "s2".to_string(),
                project: "alpha".to_string(),
                project_path: "/work/alpha".to_string(),
                timestamp: Local.with_ymd_and_hms(2026, 4, 29, 11, 0, 0).unwrap(),
                cost_usd: Some(30.0),
                ..provider_call(
                    "codex",
                    "s2",
                    "2026-04-29T11:00:00+01:00",
                    "build",
                    &[],
                    &[],
                    1,
                )
            },
        ];
        let plan = crate::config::UsagePlan {
            id: crate::config::UsagePlanId::Custom,
            monthly_usd: 100.0,
            provider: crate::config::UsagePlanProvider::Claude,
            reset_day: 1,
            set_at: "2026-04-29T00:00:00Z".to_string(),
        };

        let projection = project_plan_usage(&data, &plan, today);

        assert_eq!(projection.spent_usd, 70.0);
        assert_eq!(projection.projected_usd, 70.0);
    }

    #[test]
    fn fixed_periods_cover_labeled_calendar_days() {
        let week = date_range_for_period(&UsagePeriod::Week).unwrap();
        let thirty = date_range_for_period(&UsagePeriod::ThirtyDays).unwrap();

        assert_eq!(
            (week.1.date_naive() - week.0.date_naive()).num_days() + 1,
            7
        );
        assert_eq!(
            (thirty.1.date_naive() - thirty.0.date_naive()).num_days() + 1,
            30
        );
    }

    #[test]
    fn optimize_compare_and_yield_return_stable_summaries() {
        let calls = vec![
            provider_call(
                "claude",
                "s1",
                "2026-04-10T09:00:00+01:00",
                "implement feature",
                &["Read", "Edit", "Bash"],
                &["git status"],
                100,
            ),
            provider_call(
                "codex",
                "s2",
                "2026-04-10T10:00:00+01:00",
                "research parser",
                &["Read"],
                &[],
                200,
            ),
        ];
        let data = aggregate_calls(calls);

        let optimize = optimize_usage(&data);
        assert!(optimize.score <= 100);

        let compare = compare_models(&data);
        assert_eq!(compare.models.len(), 2);

        let yield_result = analyze_yield(&data);
        assert_eq!(yield_result.productive_sessions, 1);
        assert_eq!(yield_result.abandoned_sessions, 1);
    }
}
