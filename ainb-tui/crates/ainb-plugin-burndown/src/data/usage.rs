// ABOUTME: Token usage data model and local session parsers for usage analytics.
// Parses Claude and Codex JSONL histories into reusable aggregates for TUI and CLI views.

use chrono::{DateTime, Datelike, Duration, Local, NaiveDate, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracing::{debug, warn};

use crate::data::repo_lookup;
use crate::cache::{Cache, ParseHint, ParseResult};

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
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageProviderFilter {
    #[default]
    All,
    Claude,
    Codex,
    Cursor,
    Copilot,
    Gemini,
}

impl UsageProviderFilter {
    fn includes(self, provider: &str) -> bool {
        match self {
            Self::All => true,
            Self::Claude => provider == "claude",
            Self::Codex => provider == "codex",
            Self::Cursor => provider == "cursor",
            Self::Copilot => provider == "copilot",
            Self::Gemini => provider == "gemini",
        }
    }
}

/// Deterministic activity categories. Phase 2 fills these from turn classification.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
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

    /// Inverse of `label`. Used by the indexed filter path to map a
    /// chip's display label back to the enum so we can hit the
    /// `by_activity` index. Returns `None` for unknown labels —
    /// caller falls back to the linear filter for that pivot.
    pub fn from_label(label: &str) -> Option<Self> {
        match label {
            "Coding" => Some(Self::Coding),
            "Debugging" => Some(Self::Debugging),
            "Feature" => Some(Self::Feature),
            "Refactoring" => Some(Self::Refactoring),
            "Testing" => Some(Self::Testing),
            "Exploration" => Some(Self::Exploration),
            "Planning" => Some(Self::Planning),
            "Delegation" => Some(Self::Delegation),
            "Git" => Some(Self::Git),
            "Build/Deploy" => Some(Self::BuildDeploy),
            "Brainstorming" => Some(Self::Brainstorming),
            "Conversation" => Some(Self::Conversation),
            "General" => Some(Self::General),
            _ => None,
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
/// --activity / --session / --branch` CLI flags.
///
/// All filter sets are AND-combined with each other and with the existing
/// `include_projects` / `exclude_projects` globs. Each filter list is
/// internally OR-combined: `--project a --project b` matches calls in
/// either project.
///
/// Branch filtering matches `call.branch` exactly. Calls with no recorded
/// branch (`branch == None`, eg. codex turns or Claude turns made outside a
/// git repo) are excluded by any non-empty branch filter — there's no way
/// to ask for "untracked branch" via this struct on purpose.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct UsageFilters {
    pub project: Vec<String>,
    pub model: Vec<String>,
    pub activity: Vec<String>,
    pub session: Vec<String>,
    pub branch: Vec<String>,
    /// Negative filters set via the X-on-row picker. A call is rejected
    /// as soon as it matches any value in any of these lists. Mirrors
    /// the include lists above so all five dimensions support exclude.
    pub exclude_project: Vec<String>,
    pub exclude_model: Vec<String>,
    pub exclude_activity: Vec<String>,
    pub exclude_session: Vec<String>,
    pub exclude_branch: Vec<String>,
}

impl UsageFilters {
    pub fn is_empty(&self) -> bool {
        self.project.is_empty()
            && self.model.is_empty()
            && self.activity.is_empty()
            && self.session.is_empty()
            && self.branch.is_empty()
            && self.exclude_project.is_empty()
            && self.exclude_model.is_empty()
            && self.exclude_activity.is_empty()
            && self.exclude_session.is_empty()
            && self.exclude_branch.is_empty()
    }

    /// True if any cross-filter is set. Mirror of `!is_empty()` for sites
    /// where the positive form reads better.
    pub fn any(&self) -> bool {
        !self.is_empty()
    }

    /// Pop the most-recently-added filter chip. Removal order matches
    /// the chip-strip render order so `Esc` removes the visually
    /// rightmost chip first: exclude chips are rendered after include
    /// chips, so they pop first; within each group the order is
    /// branch → session → activity → model → project.
    pub fn pop_last(&mut self) -> Option<UsageFilterChip> {
        if let Some(value) = self.exclude_branch.pop() {
            return Some(UsageFilterChip::ExcludeBranch(value));
        }
        if let Some(value) = self.exclude_session.pop() {
            return Some(UsageFilterChip::ExcludeSession(value));
        }
        if let Some(value) = self.exclude_activity.pop() {
            return Some(UsageFilterChip::ExcludeActivity(value));
        }
        if let Some(value) = self.exclude_model.pop() {
            return Some(UsageFilterChip::ExcludeModel(value));
        }
        if let Some(value) = self.exclude_project.pop() {
            return Some(UsageFilterChip::ExcludeProject(value));
        }
        if let Some(value) = self.branch.pop() {
            return Some(UsageFilterChip::Branch(value));
        }
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
        self.branch.clear();
        self.exclude_project.clear();
        self.exclude_model.clear();
        self.exclude_activity.clear();
        self.exclude_session.clear();
        self.exclude_branch.clear();
    }

    pub(crate) fn matches(&self, call: &ProviderCall, category: ActivityCategory) -> bool {
        self.matches_with_resolved_repo(call, category, None)
    }

    /// Variant of `matches` that lets the caller pass in the
    /// per-call resolved repo id (e.g. `owner/repo` from a git
    /// `origin` lookup on `call.project_path`).
    ///
    /// Why this exists: `aggregate_calls_with_analysis` keys the
    /// `By Project` panel by the **resolved repo id**, so the chip
    /// the user commits via Enter on that panel is `owner/repo`,
    /// not the raw folder name parked on `call.project`. Comparing
    /// the chip only against `call.project` makes the filter no-op
    /// silently — the chip looks active in the strip but matches
    /// zero rows. Passing the same resolved repo here means the
    /// project chip matches calls under any worktree that resolved
    /// to the same upstream, which is the grafana-style pivot the
    /// user is asking for.
    ///
    /// Both `--project owner/repo` from the CLI and the raw folder
    /// chip path keep working: we test the chip against both
    /// `call.project` AND the resolved repo (`None` falls through
    /// to the existing raw-project comparison).
    pub(crate) fn matches_with_resolved_repo(
        &self,
        call: &ProviderCall,
        category: ActivityCategory,
        resolved_repo: Option<&str>,
    ) -> bool {
        let project_matches = |chip: &String| {
            chip == &call.project || resolved_repo.is_some_and(|repo| chip == repo)
        };
        if !self.project.is_empty() && !self.project.iter().any(project_matches) {
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
        if !self.branch.is_empty() {
            let Some(branch) = call.recorded_branch() else {
                return false;
            };
            if !self.branch.iter().any(|b| b == branch) {
                return false;
            }
        }
        // Exclude lists: a call is rejected if it matches any value.
        // Branch exclusions only apply to calls with a recorded branch
        // — calls without a branch are unaffected by branch excludes
        // (mirror of the include-side semantics where `branch == None`
        // means "no branch was recorded for this call").
        if self.exclude_project.iter().any(project_matches) {
            return false;
        }
        if self.exclude_model.iter().any(|m| m == &call.model) {
            return false;
        }
        if self.exclude_session.iter().any(|s| s == &call.session_id) {
            return false;
        }
        if !self.exclude_activity.is_empty() {
            let label = category.label();
            if self.exclude_activity.iter().any(|a| a == label) {
                return false;
            }
        }
        if !self.exclude_branch.is_empty() {
            if let Some(branch) = call.recorded_branch() {
                if self.exclude_branch.iter().any(|b| b == branch) {
                    return false;
                }
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
    Branch(String),
    /// Exclude variants set via the X-on-row picker. The chip strip
    /// renders these with a leading `~` and `pop_last` returns them
    /// before the include variants so Esc walks the visually
    /// rightmost chip first.
    ExcludeProject(String),
    ExcludeModel(String),
    ExcludeActivity(String),
    ExcludeSession(String),
    ExcludeBranch(String),
}

impl UsageFilterChip {
    /// Stable string key for this chip, used by the chip-strip widget,
    /// CLI flag mapping, and filter telemetry. The match below is the
    /// single source of truth — keep it in sync with new variants.
    //
    // NOTE: an earlier cleanup considered switching to strum_macros'
    // AsRefStr. Skipped: adding the strum crate purely for five mappings
    // costs more than the match itself, and a const-slice indexed by
    // discriminant order is harder to read than the match and not type-
    // checked when a variant is added. The match is already canonical.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Project(_) | Self::ExcludeProject(_) => "project",
            Self::Model(_) | Self::ExcludeModel(_) => "model",
            Self::Activity(_) | Self::ExcludeActivity(_) => "activity",
            Self::Session(_) | Self::ExcludeSession(_) => "session",
            Self::Branch(_) | Self::ExcludeBranch(_) => "branch",
        }
    }

    /// True for negative (exclude) variants. Used by the chip strip to
    /// render with a `~` prefix and by callers that want a single test
    /// for "is this an exclusion".
    pub fn is_exclude(&self) -> bool {
        matches!(
            self,
            Self::ExcludeProject(_)
                | Self::ExcludeModel(_)
                | Self::ExcludeActivity(_)
                | Self::ExcludeSession(_)
                | Self::ExcludeBranch(_)
        )
    }

    pub fn value(&self) -> &str {
        match self {
            Self::Project(v)
            | Self::Model(v)
            | Self::Activity(v)
            | Self::Session(v)
            | Self::Branch(v)
            | Self::ExcludeProject(v)
            | Self::ExcludeModel(v)
            | Self::ExcludeActivity(v)
            | Self::ExcludeSession(v)
            | Self::ExcludeBranch(v) => v,
        }
    }
}

/// Token counts for a single usage bucket, provider call, or aggregate row.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
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
/// every cached blob written under the current `BLOB_FORMAT_BINCODE_CURRENT`.
/// Wrong-shape decodes can either panic (caught — falls through to a full
/// re-parse) or, much worse, succeed with mis-aligned bytes and return
/// wrong analytics from the cache.
///
/// **If you change this struct or any nested type, you MUST:**
/// 1. Bump `usage_cache::db::BLOB_FORMAT_BINCODE_CURRENT` to a new value, and
/// 2. Update the layout-stability tripwire test in `usage_cache::tests`.
///
/// The tripwire test asserts a fixed serialized byte length for a known
/// fixture and will fail CI if the layout drifts without an explicit bump.
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderCall {
    /// Stable per-call identifier: `blake3(format!("{path}:{offset}"))`
    /// truncated to a `u64` (first 8 little-endian bytes of the digest).
    /// `path` is the source JSONL file, `offset` is the byte position of
    /// the assistant line that produced this call. The combination is
    /// unique per call across a parse run and stable across cache hits
    /// (the parser feeds the same `(path, offset)` tuple every time the
    /// same line is rewalked), which makes `analyze_turns` results safe
    /// to memoise on the unfiltered call set and look up by id during
    /// chip-pivot re-aggregation in `filter_usage_data`.
    ///
    /// `blake3` is reused here rather than introducing a new hashing
    /// dependency just for an id; the cryptographic strength is overkill
    /// but the API is already in scope (see `usage_cache::fingerprint`).
    pub id: u64,
    pub provider: String,
    pub model: String,
    pub session_id: String,
    pub project: String,
    pub project_path: String,
    /// Stored in UTC so cached blobs are timezone-independent. Render
    /// sites convert to local at the display boundary via
    /// `.with_timezone(&Local)` (see `components/usage.rs`,
    /// `cli/usage.rs`).
    pub timestamp: DateTime<Utc>,
    pub input_tokens: u64,
    pub cache_creation_tokens: u64,
    pub cache_read_tokens: u64,
    pub output_tokens: u64,
    pub reasoning_tokens: u64,
    pub cost_usd: Option<f64>,
    pub tools: Vec<String>,
    pub bash_commands: Vec<String>,
    pub user_message: String,
    /// Git branch the turn was made from, parsed from `gitBranch` on
    /// Claude assistant turns. Codex transcripts don't carry branch state,
    /// so codex calls always have `None` here.
    pub branch: Option<String>,
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

    /// Branch attribution that's safe to display: returns the recorded
    /// branch only when it's `Some(non-empty)`. Empty strings creep in
    /// from downstream branch detection that returns `""` for a non-git
    /// project; the canonical accessor stops them from being aggregated
    /// as a real branch named "" anywhere in the pipeline.
    pub fn recorded_branch(&self) -> Option<&str> {
        self.branch.as_deref().filter(|b| !b.is_empty())
    }
}

/// Daily dashboard row.
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DailyUsage {
    pub date: NaiveDate,
    pub bucket: TokenBucket,
}

/// Per-project summary.
///
/// `name` is the display label and aggregation key. When the project's
/// `cwd` belongs to a git repo with a resolvable `origin` remote,
/// `name` is the upstream identifier (e.g. `"owner/repo"`) and `repo`
/// holds the same value as an explicit "this came from a remote"
/// marker. Otherwise `name` falls back to the local folder/sanitised
/// project name and `repo` is `None`.
///
/// Two worktrees of the same upstream repo collapse into a single
/// `ProjectUsage` row because they share the same `name`. Chip filters
/// match on `name`, so the filter UI follows the same rule.
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectUsage {
    pub name: String,
    pub path: String,
    pub bucket: TokenBucket,
    /// `Some(owner/repo)` when the project's working directory was
    /// successfully resolved to an upstream remote at aggregation
    /// time. `None` for non-git paths or repos without an `origin`.
    pub repo: Option<String>,
}

/// Per-session summary.
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionUsage {
    pub provider: String,
    pub project: String,
    pub session_id: String,
    pub first_timestamp: DateTime<Utc>,
    pub last_timestamp: DateTime<Utc>,
    pub bucket: TokenBucket,
}

/// Per-model summary.
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelUsage {
    pub model: String,
    pub bucket: TokenBucket,
}

/// Per-branch summary. Built only from calls whose `branch` is `Some`;
/// branchless calls (codex, non-git Claude turns) are dropped from this
/// view so the panel never grows a misleading "(no branch)" bucket.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BranchUsage {
    pub branch: String,
    pub bucket: TokenBucket,
}

/// Activity summary with classified turns and retry counts.
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NamedUsage {
    pub name: String,
    pub calls: usize,
}

/// Complete parsed usage data.
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize)]
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
    /// Branch attribution rows, sorted by largest bucket. Only Claude
    /// assistant turns with a non-empty `gitBranch` populate this.
    pub branches: Vec<BranchUsage>,
    /// Precomputed `model -> [(project, call_count), ...]` sorted by
    /// count descending. Built once during `aggregate_calls` so the
    /// render path's "top N projects for model X" lookup is O(1) plus
    /// the constant-N truncation, instead of a full O(N·M) scan of
    /// `data.calls` per render frame.
    pub model_project_counts: HashMap<String, Vec<(String, usize)>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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
            branches: Vec::new(),
            model_project_counts: HashMap::new(),
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
    #[serde(rename = "gitBranch")]
    git_branch: Option<String>,
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
            let Some(path) = crate::cache::store::default_db_path() else {
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

    let filters_active = !query.filters.is_empty();
    // Shared repo cache across the chip-match pass — resolves each
    // distinct project_path at most once. Same purpose as the cache in
    // `filter_usage_data_full`: lets `--project owner/repo` match calls
    // under any worktree of the same upstream, matching the resolved-
    // repo display key used by the aggregator. Only built when the
    // caller actually needs project resolution; other chip dimensions
    // skip the fs lookup entirely.
    let needs_repo_lookup = filters_active
        && (!query.filters.project.is_empty() || !query.filters.exclude_project.is_empty());
    let mut repo_cache: HashMap<String, Option<String>> = HashMap::new();
    let filtered: Vec<ProviderCall> = calls
        .into_iter()
        .filter(|call| {
            range.as_ref().map_or(true, |(start, end)| {
                // Period bounds and call timestamps both live in Utc
                // (period bounds are anchored at local-midnight and
                // converted via `start_of_day` / `end_of_day`).
                call.timestamp >= *start && call.timestamp <= *end
            })
        })
        .filter(|call| project_matches(call, &query.include_projects, &query.exclude_projects))
        // Apply cross-filter chips up-front when present. Saves a full
        // aggregate+re-aggregate round trip on the CLI path: previously
        // we built the full UsageData and then filter_usage_data
        // re-aggregated the filtered subset. The TUI hot path keeps
        // calling filter_usage_data over cached UsageData and is
        // unaffected.
        .filter(|call| {
            if !filters_active {
                return true;
            }
            let resolved_repo = if needs_repo_lookup {
                repo_lookup::resolve_repo(&call.project_path, &mut repo_cache)
            } else {
                None
            };
            query.filters.matches_with_resolved_repo(
                call,
                classify_activity(call),
                resolved_repo.as_deref(),
            )
        })
        .collect();

    aggregate_calls(filtered)
}

// TODO(perf): parallelise per-file cache.get_or_parse with rayon
// (par_iter() over collect_claude_jsonl_files results). SQLite writes
// still serialise on the connection mutex but JSONL parsing +
// fingerprinting can run concurrent. Cache itself is already
// Send+Sync via Mutex<Connection>. Deferred — adding rayon as a
// dependency is the largest unrelated lift in this batch and is
// best landed in its own change with benchmarks attached.
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

/// Collect Claude assistant calls whose timestamp is `>= cutoff`,
/// skipping files whose mtime falls before `cutoff - grace`. This is the
/// hot-path the live-window 5h aggregator relies on — it intentionally
/// does *not* go through the usage cache because we only need a tight
/// time slice, not the full history.
///
/// `grace` widens the mtime filter to forgive clock skew between
/// timestamp comparisons (real-world JSONL files have been seen with
/// mtime trailing the last entry's timestamp by a few seconds).
///
/// Returns an empty vec if the projects dir is missing.
pub(crate) fn collect_recent_claude_calls_within(
    cutoff: DateTime<Utc>,
    grace: Duration,
) -> Vec<ProviderCall> {
    let roots = UsageSourceRoots::default();
    let Some(projects_dir) = roots.claude_projects_dir.as_deref() else {
        return Vec::new();
    };
    collect_recent_claude_calls_within_at(projects_dir, cutoff, grace)
}

/// Test seam for `collect_recent_claude_calls_within` — accepts an
/// explicit `projects_dir` so tests can drive the walker against a
/// tempdir without mutating environment variables.
pub(crate) fn collect_recent_claude_calls_within_at(
    projects_dir: &Path,
    cutoff: DateTime<Utc>,
    grace: Duration,
) -> Vec<ProviderCall> {
    if !projects_dir.is_dir() {
        return Vec::new();
    }
    let username = whoami_username();
    let mtime_floor = std::time::SystemTime::UNIX_EPOCH
        + std::time::Duration::from_secs((cutoff - grace).timestamp().max(0) as u64);

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
            // mtime gate — skip files that haven't been touched in the
            // window. Cheap and avoids opening cold archives.
            if let Ok(meta) = std::fs::metadata(&jsonl_path) {
                if let Ok(mtime) = meta.modified() {
                    if mtime < mtime_floor {
                        continue;
                    }
                }
            }
            parse_claude_jsonl_within(
                &jsonl_path,
                &project,
                &project_path,
                cutoff,
                &mut calls,
            );
        }
    }
    calls
}

/// Stream a single Claude JSONL file and append calls whose timestamp
/// is `>= cutoff` to `out`. JSONL files are append-only with
/// monotonically increasing timestamps, so once we have collected at
/// least one call inside the window and then encounter a call before
/// the window we know the rest of the file is older too — but the
/// converse is not guaranteed (header lines, "user"-typed lines, etc.
/// have no usage data and emit `None`). To stay correct we just walk
/// to EOF and filter; the mtime gate already keeps the per-file work
/// bounded.
fn parse_claude_jsonl_within(
    path: &Path,
    project: &str,
    project_path: &str,
    cutoff: DateTime<Utc>,
    out: &mut Vec<ProviderCall>,
) {
    let Ok(file) = std::fs::File::open(path) else {
        return;
    };
    let reader = BufReader::new(file);
    let mut current_user_message = String::new();
    let mut offset: u64 = 0;
    for line in reader.lines() {
        let Ok(line) = line else { return };
        let line_offset = offset;
        // Reconstruct byte advance: line bytes + the trailing newline
        // we stripped (BufRead::lines drops the terminator).
        offset += line.len() as u64 + 1;
        if let Some(call) = parse_claude_line(
            &line,
            path,
            project,
            project_path,
            &mut current_user_message,
            line_offset,
        ) {
            if call.timestamp >= cutoff {
                out.push(call);
            }
        }
    }
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
    // Walk lines manually rather than using `content.lines()` so each
    // call carries the byte offset of its source line in the file, which
    // feeds the stable `ProviderCall.id`. `split_inclusive` keeps the
    // newline byte counted in the offset advance.
    let mut offset: u64 = 0;
    for chunk in content.split_inclusive('\n') {
        let line_offset = offset;
        offset += chunk.len() as u64;
        let line = chunk.strip_suffix('\n').unwrap_or(chunk);
        if let Some(call) = parse_claude_line(
            line,
            path,
            project,
            project_path,
            &mut current_user_message,
            line_offset,
        ) {
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
/// `current_user_message` state from before `from_offset` is restored
/// by `recover_user_message_before`, which walks the prefix
/// `[0, from_offset)` for the last `"type":"user"` line. Without that,
/// every appended assistant turn would lose user_message attribution
/// across cache hits.
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
    // Recover the user message that was last in scope at `from_offset`
    // by reading the prefix [0, from_offset) and walking it for "type":
    // "user" lines. Without this, every appended assistant turn would
    // carry an empty user_message and lose attribution. Bounded by
    // from_offset; only paid on cache-hit append paths.
    let current_user_message = recover_user_message_before(&mut file, from_offset);
    if file.seek(SeekFrom::Start(from_offset)).is_err() {
        return ParseResult {
            calls: existing.to_vec(),
            end_offset: from_offset,
        };
    }
    let mut reader = BufReader::new(file);
    let mut calls = existing.to_vec();
    let mut current_user_message = current_user_message;
    // Track the running byte offset so each call's stable id matches
    // the full-parse path. `read_line` advances exactly the bytes it
    // consumed (including the trailing newline), so `running_offset`
    // is the offset of the *next* line at any iteration tip — the
    // current line's offset is captured before the read.
    let mut running_offset = from_offset;
    let mut buf = String::new();
    loop {
        buf.clear();
        let line_offset = running_offset;
        let read = match reader.read_line(&mut buf) {
            Ok(0) => break, // EOF
            Ok(n) => n,
            Err(_) => {
                // On any I/O or UTF-8 error we cannot safely advance
                // the cached end_offset — the next run would skip past
                // the unread tail and silently lose data. Roll the
                // cursor back to from_offset so the next scan retries
                // the append. Mirror the Full path's all-or-nothing
                // semantics.
                return ParseResult {
                    calls: existing.to_vec(),
                    end_offset: from_offset,
                };
            }
        };
        running_offset += read as u64;
        let line = buf.strip_suffix('\n').unwrap_or(&buf);
        if let Some(call) = parse_claude_line(
            line,
            path,
            project,
            project_path,
            &mut current_user_message,
            line_offset,
        ) {
            calls.push(call);
        }
    }
    ParseResult {
        calls,
        end_offset: total_len,
    }
}

/// Walk the prefix `[0, from_offset)` of `file` looking for the last
/// `"type":"user"` line, returning its extracted message text. Used by
/// the append parser so user_message attribution survives a cache hit
/// (the user line that primed the message is in the prefix; without
/// recovery, every appended assistant turn ends up with an empty
/// `user_message`).
///
/// Errors are absorbed — if the prefix is unreadable we return an empty
/// string and the next assistant turn's `user_message` is empty (same
/// fallback as before this fix). Leaves the file cursor at an
/// undefined offset; the caller must re-seek to `from_offset`.
fn recover_user_message_before(
    file: &mut std::fs::File,
    from_offset: u64,
) -> String {
    if from_offset == 0 {
        return String::new();
    }
    if file.seek(SeekFrom::Start(0)).is_err() {
        return String::new();
    }
    let limited = file.by_ref().take(from_offset);
    let reader = BufReader::new(limited);
    let mut last = String::new();
    for line in reader.lines() {
        let Ok(line) = line else {
            // Partial scan still useful — keep whatever we'd
            // discovered up to the error.
            break;
        };
        if let Ok(parsed) = serde_json::from_str::<ClaudeLine>(&line) {
            if parsed.msg_type.as_deref() == Some("user") {
                if let Some(message) = parsed.message {
                    last = extract_claude_user_text(message.content.as_ref());
                }
            }
        }
    }
    last
}

/// Parse a single Claude JSONL line. Returns `Some(call)` for assistant
/// lines that carry usage data; mutates `current_user_message` when a
/// user line is encountered. None for unrecognized / unparseable lines.
///
/// `line_offset` is the byte position of `line` in `path` and feeds the
/// stable `ProviderCall.id` derivation. The full and append parsers
/// track the running offset and pass it in.
fn parse_claude_line(
    line: &str,
    path: &Path,
    project: &str,
    project_path: &str,
    current_user_message: &mut String,
    line_offset: u64,
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
                id: provider_call_id(path, line_offset),
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
                branch: parsed.git_branch.filter(|b| !b.is_empty()),
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

    // Mirror the Claude parser's offset-tracking so each codex call
    // carries a stable `(path, offset)` id. `split_inclusive` ensures
    // the trailing newline is counted in the per-line advance.
    let mut offset: u64 = 0;
    for chunk in content.split_inclusive('\n') {
        let line_offset = offset;
        offset += chunk.len() as u64;
        let line = chunk.strip_suffix('\n').unwrap_or(chunk);
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
            id: provider_call_id(path, line_offset),
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
            branch: None,
        });
    }

    calls
}

// TODO(refactor): the function below ingests `calls` into ten parallel
// accumulators (daily, weekly, projects, sessions, models, branches,
// activities, tools, mcp, shell). Extract a per-dimension Accumulator
// trait so each one is unit-testable in isolation; orchestrator
// becomes `for call in calls { for acc in &mut accumulators { acc.ingest(call) } }`.
// Deferred — large refactor with no functional delta. The
// analyze_turns precompute that previously gated this work has landed,
// so a future pass is unblocked.
fn aggregate_calls(calls: Vec<ProviderCall>) -> UsageData {
    aggregate_calls_with_analysis(calls, None)
}

/// Variant of `aggregate_calls` that accepts an optional precomputed
/// `analyze_turns` result keyed by `ProviderCall.id`. When `Some`, the
/// per-call analysis is looked up by id rather than re-walking the
/// session timeline — this is what `filter_usage_data` uses so chip
/// pivots don't pay a fresh O(N) timeline scan on each re-aggregate.
///
/// Fallback semantics: only `None` triggers a local `analyze_turns`
/// recompute. `Some(map)` is trusted as authoritative — calls whose id
/// is missing from the map silently get a default `TurnAnalysis` (zero
/// retries, zero edits). Callers passing `Some` must ensure every
/// `call.id` in the unfiltered superset appears in the precompute.
fn aggregate_calls_with_analysis(
    mut calls: Vec<ProviderCall>,
    precomputed_analysis: Option<&HashMap<u64, TurnAnalysis>>,
) -> UsageData {
    if calls.is_empty() {
        return UsageData::default();
    }

    calls.sort_by_key(|call| call.timestamp);
    // Local fallback only if the caller didn't precompute. Using the
    // precomputed map preserves correctness across filtering: a call's
    // retry count is measured against its *own* session's timeline in
    // the unfiltered set, not against whatever subset survived the
    // filter.
    let local_analysis: HashMap<u64, TurnAnalysis>;
    let turn_analysis: &HashMap<u64, TurnAnalysis> = match precomputed_analysis {
        Some(m) => m,
        None => {
            local_analysis = analyze_turns(&calls);
            &local_analysis
        }
    };

    let mut daily_map: HashMap<NaiveDate, (HashSet<String>, HashSet<String>, TokenBucket)> =
        HashMap::new();
    // Project aggregation key is the *display* name: the upstream repo
    // id (e.g. "owner/repo") when resolvable, otherwise the local
    // folder/sanitised project name. This is what collapses two
    // worktrees of the same repo into one ProjectUsage row.
    //
    // Value tuple: (project_path, session_keys, bucket, repo_marker).
    // `repo_marker` is `Some(owner/repo)` when the key came from the
    // remote and `None` when it fell back to the folder name.
    let mut project_map: HashMap<String, (String, HashSet<String>, TokenBucket, Option<String>)> =
        HashMap::new();
    // Per-cwd repo lookup cache: shared across the loop so each
    // distinct working directory is parsed at most once.
    let mut repo_cache: HashMap<String, Option<String>> = HashMap::new();
    let mut session_map: HashMap<String, SessionUsageAccumulator> = HashMap::new();
    let mut model_map: HashMap<String, TokenBucket> = HashMap::new();
    let mut branch_map: HashMap<String, TokenBucket> = HashMap::new();
    let mut activity_map: HashMap<ActivityCategory, ActivityAccumulator> = HashMap::new();
    let mut tool_map: HashMap<String, usize> = HashMap::new();
    let mut mcp_map: HashMap<String, usize> = HashMap::new();
    let mut shell_map: HashMap<String, usize> = HashMap::new();

    for call in &calls {
        let bucket = call.bucket();
        // Daily bucketing is a user-facing calendar concept: render and
        // the period bounds both treat "a day" as a local-tz day, so the
        // grouping key has to be local. The cached call timestamp is
        // Utc — convert at the boundary.
        let day = call.timestamp.with_timezone(&Local).date_naive();
        let session_key = format!("{}:{}:{}", call.provider, call.project, call.session_id);

        // Resolve the call's working directory to an upstream repo id
        // so worktrees collapse. Falls back to the folder/sanitised
        // project name when the cwd isn't a git repo or has no origin.
        let resolved_repo = repo_lookup::resolve_repo(&call.project_path, &mut repo_cache);
        let project_key = resolved_repo.clone().unwrap_or_else(|| call.project.clone());

        let daily = daily_map.entry(day).or_default();
        daily.0.insert(project_key.clone());
        daily.1.insert(session_key.clone());
        daily.2.merge(&bucket);

        let project = project_map.entry(project_key.clone()).or_insert_with(|| {
            (
                call.project_path.clone(),
                HashSet::new(),
                TokenBucket::default(),
                resolved_repo.clone(),
            )
        });
        project.0 = call.project_path.clone();
        project.1.insert(session_key.clone());
        project.2.merge(&bucket);
        // Once a row is keyed by the upstream repo, keep it that way
        // even if a later call from the same key fails resolution.
        if project.3.is_none() && resolved_repo.is_some() {
            project.3 = resolved_repo.clone();
        }

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

        add_bucket(&mut model_map, call.model.clone(), &bucket);

        if let Some(branch) = call.recorded_branch() {
            add_bucket(&mut branch_map, branch.to_string(), &bucket);
        }

        let analysis = turn_analysis.get(&call.id).copied().unwrap_or_else(|| TurnAnalysis {
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
                bump(&mut mcp_map, server.to_string());
            } else {
                bump(&mut tool_map, tool.clone());
            }
        }
        for command in &call.bash_commands {
            bump(&mut shell_map, command.clone());
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
        .map(|(name, (path, sessions, mut bucket, repo))| {
            bucket.session_count = sessions.len();
            ProjectUsage {
                name,
                path,
                bucket,
                repo,
            }
        })
        .collect();
    sort_by_bucket_desc(&mut projects, |p| &p.bucket);

    let mut sessions: Vec<_> =
        session_map.into_values().map(SessionUsageAccumulator::into_usage).collect();
    sort_by_bucket_desc(&mut sessions, |s| &s.bucket);

    let mut models: Vec<_> = model_map
        .into_iter()
        .map(|(model, bucket)| ModelUsage { model, bucket })
        .collect();
    sort_by_bucket_desc(&mut models, |m| &m.bucket);

    let mut branches: Vec<_> = branch_map
        .into_iter()
        .map(|(branch, bucket)| BranchUsage { branch, bucket })
        .collect();
    sort_by_bucket_desc(&mut branches, |b| &b.bucket);

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

    let model_project_counts = build_model_project_counts(&calls);

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
        branches,
        model_project_counts,
    }
}

/// Build the `model -> [(project, count), ...]` index used by the
/// burndown render path's "top N projects for model X" lookup. Sorted
/// desc by count so the render call is `take(n)` with no extra work.
fn build_model_project_counts(
    calls: &[ProviderCall],
) -> HashMap<String, Vec<(String, usize)>> {
    let mut by_model: HashMap<String, HashMap<String, usize>> = HashMap::new();
    for call in calls {
        *by_model
            .entry(call.model.clone())
            .or_default()
            .entry(call.project.clone())
            .or_insert(0) += 1;
    }
    by_model
        .into_iter()
        .map(|(model, projs)| {
            let mut v: Vec<(String, usize)> = projs.into_iter().collect();
            v.sort_by(|a, b| b.1.cmp(&a.1));
            (model, v)
        })
        .collect()
}

/// Re-derive the `activities`, `mcp_servers`, and `tools` columns of an
/// already-parsed `UsageData` from `data.calls`.
///
/// session-reader publishes empty `activities` and `mcp_servers` arrays
/// on the wire (see the comment in `crates/ainb-plugin-session-reader/
/// src/scanner.rs`: classification is the consumer's job). It also
/// emits raw tool names into `tools` without splitting the `mcp__server__*`
/// prefix into a separate MCP-server row. Burndown owns the richer
/// activity taxonomy (12+ buckets vs. the wire schema's 6) and the
/// mcp/tool split, so we recompute those three columns here using the
/// raw call set the producer ships intact.
///
/// Other columns (daily/weekly/projects/sessions/models/branches/
/// shell_commands/grand_total/model_project_counts) are left as the
/// producer published them — re-aggregating those would risk
/// timezone shifts and other cross-platform drift for no gain.
pub fn rebuild_activity_and_mcp_columns(data: &mut UsageData) {
    let turn_analysis = analyze_turns(&data.calls);
    let mut activity_map: HashMap<ActivityCategory, ActivityAccumulator> = HashMap::new();
    let mut tool_map: HashMap<String, usize> = HashMap::new();
    let mut mcp_map: HashMap<String, usize> = HashMap::new();

    for call in &data.calls {
        let bucket = call.bucket();
        let analysis = turn_analysis.get(&call.id).copied().unwrap_or_else(|| TurnAnalysis {
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
                bump(&mut mcp_map, server.to_string());
            } else {
                bump(&mut tool_map, tool.clone());
            }
        }
    }

    data.activities = sorted_activities(activity_map);
    data.mcp_servers = sorted_named_usage(mcp_map);
    data.tools = sorted_named_usage(tool_map);
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
    filter_usage_data_full(data, filters, &UsagePeriod::All, UsageProviderFilter::All)
}

/// Pre-built lookup indices over a [`UsageData`] call set.
///
/// Each index maps a dimension value (project / model / branch /
/// activity) to the `usize` positions of matching calls in
/// `data.calls`. Used by [`filter_usage_data_indexed`] to short-
/// circuit the linear `O(N)` filter walk to `O(candidates)` when a
/// chip is active — the candidate set is the union of the chip's
/// index entries, then the remaining predicates (period / provider /
/// other-dimension chips) are checked against just those.
///
/// Build once per `data_generation` (see the plugin's `data_generation`
/// counter) and reuse across pivots. Cheap O(N) one-time cost
/// amortized across many chip clicks.
///
/// `by_project_resolved` carries `owner/repo` entries derived via
/// `repo_lookup::resolve_repo`; calls whose `project_path` doesn't
/// resolve to a git origin are absent from this map but still appear
/// in `by_project_raw`.
#[derive(Debug, Clone, Default)]
pub struct UsageIndices {
    by_project_raw: HashMap<String, Vec<usize>>,
    by_project_resolved: HashMap<String, Vec<usize>>,
    by_model: HashMap<String, Vec<usize>>,
    by_branch: HashMap<String, Vec<usize>>,
    by_activity: HashMap<ActivityCategory, Vec<usize>>,
}

impl UsageIndices {
    /// Build the index set from a [`UsageData`]'s `calls` vector.
    /// O(N) over the call set; resolves each distinct project_path at
    /// most once for the project-resolved index.
    pub fn from_usage_data(data: &UsageData) -> Self {
        let mut by_project_raw: HashMap<String, Vec<usize>> = HashMap::new();
        let mut by_project_resolved: HashMap<String, Vec<usize>> = HashMap::new();
        let mut by_model: HashMap<String, Vec<usize>> = HashMap::new();
        let mut by_branch: HashMap<String, Vec<usize>> = HashMap::new();
        let mut by_activity: HashMap<ActivityCategory, Vec<usize>> = HashMap::new();
        let mut repo_cache: HashMap<String, Option<String>> = HashMap::new();
        for (idx, call) in data.calls.iter().enumerate() {
            by_project_raw
                .entry(call.project.clone())
                .or_default()
                .push(idx);
            if let Some(repo) = repo_lookup::resolve_repo(&call.project_path, &mut repo_cache) {
                by_project_resolved.entry(repo).or_default().push(idx);
            }
            by_model.entry(call.model.clone()).or_default().push(idx);
            if let Some(branch) = call.recorded_branch() {
                by_branch.entry(branch.to_string()).or_default().push(idx);
            }
            by_activity
                .entry(classify_activity(call))
                .or_default()
                .push(idx);
        }
        Self {
            by_project_raw,
            by_project_resolved,
            by_model,
            by_branch,
            by_activity,
        }
    }

    /// Resolve a project chip value to the union of its raw-folder and
    /// resolved-repo index entries — the chip might be either form.
    fn project_indices(&self, chip: &str) -> Vec<usize> {
        let mut out = Vec::new();
        if let Some(v) = self.by_project_raw.get(chip) {
            out.extend_from_slice(v);
        }
        if let Some(v) = self.by_project_resolved.get(chip) {
            out.extend_from_slice(v);
        }
        // De-dup in case the chip happens to match both the raw and
        // resolved entries for the same call (rare but possible if a
        // folder is literally named `owner/repo`).
        out.sort_unstable();
        out.dedup();
        out
    }
}

/// Apply the full filter surface — cross-filter chips, period date range,
/// and provider — in a single re-aggregation pass.
///
/// This is the grafana-style pivot the render path uses: period and
/// provider were UI selectors with no data binding pre-PR A (they
/// repainted the chip strip but the dashboard widgets ignored them).
/// Here all three filters compose into the same call-level predicate
/// and the result is re-aggregated once, so every panel reflects the
/// active pivot.
///
/// The no-op early return covers the common idle case (no chips, no
/// period selected beyond `All`, provider = All) and keeps that path
/// at the same cost as the pre-PR-A `filter_usage_data`.
/// Indexed variant of [`filter_usage_data_full`]. When `indices` is
/// `Some` and a single-dimension chip is active (project / model /
/// branch / activity), seeds the candidate set from that chip's
/// pre-built index entries instead of walking every call. Falls back
/// to the linear pass when no indices are supplied, when no chip is
/// active, or when only exclude chips are active (excludes don't
/// shrink the candidate set — they only reject within it).
///
/// Other predicates (period, provider, remaining chips, excludes) are
/// applied to the seeded candidate set.
///
/// Behavioural parity with [`filter_usage_data_full`] is asserted in
/// tests: any (data, filters, period, provider) tuple produces
/// byte-identical output through either entrypoint.
pub fn filter_usage_data_indexed(
    data: &UsageData,
    indices: Option<&UsageIndices>,
    filters: &UsageFilters,
    period: &UsagePeriod,
    provider_filter: UsageProviderFilter,
) -> UsageData {
    let period_range = date_range_for_period(period);
    let provider_active = !matches!(provider_filter, UsageProviderFilter::All);
    if filters.is_empty() && period_range.is_none() && !provider_active {
        return data.clone();
    }

    // Pick the smallest available include-chip dimension as the seed
    // for the candidate walk. Indices only help include filters: an
    // exclude chip can't shrink the candidate set, it only rejects
    // within it.
    let seed = indices.and_then(|idx| select_seed_indices(idx, filters));
    let turn_analysis = analyze_turns(&data.calls);
    let needs_repo_lookup =
        !filters.project.is_empty() || !filters.exclude_project.is_empty();
    let mut repo_cache: HashMap<String, Option<String>> = HashMap::new();

    let filtered_calls: Vec<ProviderCall> = match seed {
        Some(seed_indices) => seed_indices
            .into_iter()
            .filter_map(|i| data.calls.get(i))
            .filter(|call| {
                let resolved_repo = if needs_repo_lookup {
                    repo_lookup::resolve_repo(&call.project_path, &mut repo_cache)
                } else {
                    None
                };
                pass_full_predicate(
                    call,
                    filters,
                    period_range.as_ref(),
                    provider_filter,
                    resolved_repo.as_deref(),
                )
            })
            .cloned()
            .collect(),
        None => data
            .calls
            .iter()
            .filter(|call| {
                let resolved_repo = if needs_repo_lookup {
                    repo_lookup::resolve_repo(&call.project_path, &mut repo_cache)
                } else {
                    None
                };
                pass_full_predicate(
                    call,
                    filters,
                    period_range.as_ref(),
                    provider_filter,
                    resolved_repo.as_deref(),
                )
            })
            .cloned()
            .collect(),
    };
    aggregate_calls_with_analysis(filtered_calls, Some(&turn_analysis))
}

/// Pick the smallest include-chip dimension to seed the indexed walk.
/// Returns `None` when no include chips are active (we don't seed
/// from excludes because they can only reject, not select).
fn select_seed_indices(indices: &UsageIndices, filters: &UsageFilters) -> Option<Vec<usize>> {
    let mut candidates: Vec<Vec<usize>> = Vec::new();
    if !filters.project.is_empty() {
        let mut combined: Vec<usize> = Vec::new();
        for chip in &filters.project {
            combined.extend(indices.project_indices(chip));
        }
        combined.sort_unstable();
        combined.dedup();
        candidates.push(combined);
    }
    if !filters.model.is_empty() {
        let mut combined: Vec<usize> = Vec::new();
        for chip in &filters.model {
            if let Some(v) = indices.by_model.get(chip) {
                combined.extend_from_slice(v);
            }
        }
        combined.sort_unstable();
        combined.dedup();
        candidates.push(combined);
    }
    if !filters.branch.is_empty() {
        let mut combined: Vec<usize> = Vec::new();
        for chip in &filters.branch {
            if let Some(v) = indices.by_branch.get(chip) {
                combined.extend_from_slice(v);
            }
        }
        combined.sort_unstable();
        combined.dedup();
        candidates.push(combined);
    }
    if !filters.activity.is_empty() {
        let mut combined: Vec<usize> = Vec::new();
        for chip in &filters.activity {
            // activity chips store the label string; map back via
            // `ActivityCategory::from_label` for the index lookup.
            if let Some(cat) = ActivityCategory::from_label(chip) {
                if let Some(v) = indices.by_activity.get(&cat) {
                    combined.extend_from_slice(v);
                }
            }
        }
        combined.sort_unstable();
        combined.dedup();
        candidates.push(combined);
    }
    // Pick the smallest dimension as the seed — applying other chip
    // predicates linearly over that set is cheap.
    candidates.into_iter().min_by_key(|v| v.len())
}

/// Centralised per-call predicate used by both the indexed and linear
/// filter paths. Encapsulates chip-match + provider + period so the
/// two paths can't drift in their filter semantics.
fn pass_full_predicate(
    call: &ProviderCall,
    filters: &UsageFilters,
    period_range: Option<&(DateTime<Utc>, DateTime<Utc>)>,
    provider_filter: UsageProviderFilter,
    resolved_repo: Option<&str>,
) -> bool {
    if !filters.matches_with_resolved_repo(call, classify_activity(call), resolved_repo) {
        return false;
    }
    if !provider_filter.includes(&call.provider) {
        return false;
    }
    if let Some((start, end)) = period_range {
        if call.timestamp < *start || call.timestamp > *end {
            return false;
        }
    }
    true
}

pub fn filter_usage_data_full(
    data: &UsageData,
    filters: &UsageFilters,
    period: &UsagePeriod,
    provider_filter: UsageProviderFilter,
) -> UsageData {
    let period_range = date_range_for_period(period);
    let provider_active = !matches!(provider_filter, UsageProviderFilter::All);
    if filters.is_empty() && period_range.is_none() && !provider_active {
        return data.clone();
    }
    // Precompute analyze_turns once on the *unfiltered* call set so
    // each call's retry/has_edits classification reflects its actual
    // session timeline, not the post-filter subset (a retry that
    // happened before a filtered-out edit still counts). The
    // aggregate path looks up by ProviderCall.id, which is stable
    // across the filter-and-sort pipeline.
    let turn_analysis = analyze_turns(&data.calls);
    // Resolve every call's working directory to an upstream `owner/repo`
    // so the project chip — which is keyed on the same resolved repo by
    // `aggregate_calls_with_analysis` — matches calls under any worktree
    // of the same upstream. Cache is local to this filter pass so each
    // distinct project_path resolves at most once. Only pay for the
    // resolution when there's actually a project chip to test against —
    // model/activity/session/branch pivots leave this empty.
    let needs_repo_lookup =
        !filters.project.is_empty() || !filters.exclude_project.is_empty();
    let mut repo_cache: HashMap<String, Option<String>> = HashMap::new();
    let filtered_calls: Vec<ProviderCall> = data
        .calls
        .iter()
        .filter(|call| {
            // Project chip can carry either a raw folder name (legacy /
            // CLI flag input) or a resolved `owner/repo` (TUI Enter on
            // the By Project row); test both in `matches`.
            let resolved_repo = if needs_repo_lookup {
                repo_lookup::resolve_repo(&call.project_path, &mut repo_cache)
            } else {
                None
            };
            // Chip filters (project / model / activity / session / branch).
            if !filters.matches_with_resolved_repo(
                call,
                classify_activity(call),
                resolved_repo.as_deref(),
            ) {
                return false;
            }
            // Provider filter (Right/Left arrow on the TUI).
            if !provider_filter.includes(&call.provider) {
                return false;
            }
            // Period filter (1/2/3/a or specific month/quarter).
            if let Some((start, end)) = period_range {
                if call.timestamp < start || call.timestamp > end {
                    return false;
                }
            }
            true
        })
        .cloned()
        .collect();
    aggregate_calls_with_analysis(filtered_calls, Some(&turn_analysis))
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

/// Walk the per-session timeline and produce a per-call
/// `TurnAnalysis` keyed by `ProviderCall.id`. Idempotent on the same
/// inputs — produced once on the unfiltered call set in
/// `filter_usage_data` and reused across chip-pivot re-aggregates so
/// each chip switch costs an O(1) lookup per call instead of an O(N)
/// timeline rewalk over the filtered subset.
///
/// Keying on `id` (rather than the previous positional `idx`) means the
/// map survives sorting and filtering: the precompute can be done on
/// the unfiltered set and consumed by `aggregate_calls_with_analysis`
/// after `filter_usage_data` has dropped non-matching calls.
fn analyze_turns(calls: &[ProviderCall]) -> HashMap<u64, TurnAnalysis> {
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
                call.id,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanStatus {
    Under,
    Near,
    Over,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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
            // start/end are NaiveDate in the user's local calendar;
            // bucket the Utc timestamp into the same calendar.
            let date = call.timestamp.with_timezone(&Local).date_naive();
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Impact {
    High,
    Medium,
    Low,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HealthGrade {
    A,
    B,
    C,
    D,
    F,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WasteAction {
    pub label: String,
    pub command: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WasteFinding {
    pub id: String,
    pub title: String,
    pub impact: Impact,
    pub tokens_saved: u64,
    pub details: String,
    pub actions: Vec<WasteAction>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
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
    first_timestamp: DateTime<Utc>,
    last_timestamp: DateTime<Utc>,
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

/// "Now" anchor used for period filtering. Reads `AINB_NOW`
/// (RFC 3339, e.g. `2026-05-11T00:00:00Z`) when set so deterministic
/// tripwire fixtures can pin the calendar day; otherwise falls back to
/// the real local clock.
fn local_now() -> DateTime<Local> {
    if let Ok(raw) = std::env::var("AINB_NOW") {
        if let Ok(parsed) = DateTime::parse_from_rfc3339(raw.trim()) {
            return parsed.with_timezone(&Local);
        }
    }
    Local::now()
}

/// Returns the inclusive `(start, end)` instants for `period`, expressed
/// in UTC for direct comparison against `ProviderCall.timestamp`.
///
/// The "day" anchor is the user's local calendar day (so "today" still
/// means the user's wall-clock today across DST and timezone moves) —
/// `start_of_day` / `end_of_day` build a local-midnight / local-23:59
/// datetime first, then convert to UTC for the comparison surface.
fn date_range_for_period(period: &UsagePeriod) -> Option<(DateTime<Utc>, DateTime<Utc>)> {
    let now = local_now();
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
            let last = last_day_of_month(anchor.year(), anchor.month()).unwrap_or(*anchor);
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

/// Last calendar day of a `(year, month)`. Returns `None` if the inputs
/// are out of chrono's representable range (year < -262_144 or > 262_143,
/// month not in 1..=12). Previously this silently returned the 28th,
/// which produced wrong-on-purpose results for callers that didn't
/// notice — `Option` makes the failure explicit.
pub fn last_day_of_month(year: i32, month: u32) -> Option<NaiveDate> {
    // Reject obviously-invalid months up front so month=0 doesn't silently
    // roll into the previous December (which the next-month-minus-one
    // trick below would otherwise compute).
    if !(1..=12).contains(&month) {
        return None;
    }
    let (next_year, next_month) = if month == 12 {
        (year + 1, 1)
    } else {
        (year, month + 1)
    };
    let next_first = NaiveDate::from_ymd_opt(next_year, next_month, 1)?;
    Some(next_first - Duration::days(1))
}

/// First and last calendar days of `quarter` (1..=4) within `year`.
/// Out-of-range quarters are clamped to the nearest valid quarter
/// (`quarter < 1 -> Q1`, `quarter > 4 -> Q4`) via `clamp(1, 4)`.
pub fn quarter_bounds(year: i32, quarter: u8) -> (NaiveDate, NaiveDate) {
    let q = quarter.clamp(1, 4);
    let start_month = (u32::from(q) - 1) * 3 + 1;
    let end_month = start_month + 2;
    let first = NaiveDate::from_ymd_opt(year, start_month, 1)
        .unwrap_or_else(|| NaiveDate::from_ymd_opt(year, 1, 1).expect("valid Jan 1"));
    // Quarter end-month is always 3, 6, 9, or 12 — last_day_of_month can
    // only fail for an out-of-range `year`. In that case fall back to
    // `first` so the bounds collapse to a single day rather than panic.
    let last = last_day_of_month(year, end_month).unwrap_or(first);
    (first, last)
}

/// Quarter (1..=4) containing `date`.
pub fn quarter_of(date: NaiveDate) -> u8 {
    ((date.month0() / 3) + 1) as u8
}

/// First day of the calendar month containing `date`. Falls back to
/// `date` itself if chrono rejects the (year, month, 1) tuple, which is
/// only possible at the extreme edges of the representable range.
pub fn first_of_month(date: NaiveDate) -> NaiveDate {
    NaiveDate::from_ymd_opt(date.year(), date.month(), 1).unwrap_or(date)
}

/// First day of the previous calendar month, wrapping the year at Jan.
pub fn previous_month_first(anchor: NaiveDate) -> NaiveDate {
    let (y, m) = if anchor.month() == 1 {
        (anchor.year() - 1, 12)
    } else {
        (anchor.year(), anchor.month() - 1)
    };
    NaiveDate::from_ymd_opt(y, m, 1).unwrap_or(anchor)
}

/// First day of the next calendar month, wrapping the year at Dec.
pub fn next_month_first(anchor: NaiveDate) -> NaiveDate {
    let (y, m) = if anchor.month() == 12 {
        (anchor.year() + 1, 1)
    } else {
        (anchor.year(), anchor.month() + 1)
    };
    NaiveDate::from_ymd_opt(y, m, 1).unwrap_or(anchor)
}

/// `(year, quarter)` of the previous quarter, wrapping into the prior
/// year at Q1.
pub fn previous_quarter(year: i32, q: u8) -> (i32, u8) {
    if q <= 1 { (year - 1, 4) } else { (year, q - 1) }
}

/// `(year, quarter)` of the next quarter, wrapping into the next year
/// at Q4.
pub fn next_quarter(year: i32, q: u8) -> (i32, u8) {
    if q >= 4 { (year + 1, 1) } else { (year, q + 1) }
}

/// `(year, quarter)` containing `date`.
pub fn current_quarter(date: NaiveDate) -> (i32, u8) {
    (date.year(), quarter_of(date))
}

fn day_bounds(date: NaiveDate) -> (DateTime<Utc>, DateTime<Utc>) {
    (start_of_day(date), end_of_day(date))
}

/// Local-midnight on `date`, converted to UTC. The local interpretation
/// is intentional: `date` originates from the user's calendar (`today`
/// in the period helpers), so the bound has to anchor at local midnight
/// to match how the user experiences "the day". Conversion to UTC
/// happens last so the bound is directly comparable to the
/// Utc-stored `ProviderCall.timestamp`.
fn start_of_day(date: NaiveDate) -> DateTime<Utc> {
    Local
        .from_local_datetime(&date.and_hms_opt(0, 0, 0).expect("valid start of day"))
        .single()
        .unwrap_or_else(|| {
            Local.from_utc_datetime(&date.and_hms_opt(0, 0, 0).expect("valid start of day"))
        })
        .with_timezone(&Utc)
}

/// Local-23:59:59.999 on `date`, converted to UTC. See `start_of_day`.
fn end_of_day(date: NaiveDate) -> DateTime<Utc> {
    Local
        .from_local_datetime(&date.and_hms_milli_opt(23, 59, 59, 999).expect("valid end of day"))
        .single()
        .unwrap_or_else(|| {
            Local.from_utc_datetime(
                &date.and_hms_milli_opt(23, 59, 59, 999).expect("valid end of day"),
            )
        })
        .with_timezone(&Utc)
}

fn parse_timestamp(timestamp: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(timestamp).map(|dt| dt.with_timezone(&Utc)).ok()
}

/// Compute the stable per-call id from `(path, offset)`.
///
/// FNV-1a 64-bit. Stable across runs and platforms, deterministic, and
/// has no external dependency — that last property matters because the
/// plugin's persistent SQLite cache (which originally pulled blake3 in)
/// was retired in Phase 3.
fn provider_call_id(path: &Path, offset: u64) -> u64 {
    let key = format!("{}:{}", path.to_string_lossy(), offset);
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = FNV_OFFSET;
    for byte in key.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
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

/// Sort `rows` in-place by bucket weight (descending), where each row's
/// bucket is extracted via `key`. Centralises the cost-then-tokens
/// ranking used across every usage panel (projects, sessions, models,
/// branches, ...). The closure return type is `&TokenBucket` so callers
/// can point at a field without cloning.
fn sort_by_bucket_desc<T, F>(rows: &mut Vec<T>, key: F)
where
    F: Fn(&T) -> &TokenBucket,
{
    rows.sort_by(|a, b| {
        bucket_sort_value(key(b)).total_cmp(&bucket_sort_value(key(a)))
    });
}

/// Merge `bucket` into the entry at `key` in a `String -> TokenBucket`
/// map, defaulting to an empty bucket when the key is absent. Centralises
/// the model_map / branch_map merge pattern used inside `aggregate_calls`.
fn add_bucket<K>(map: &mut HashMap<K, TokenBucket>, key: K, bucket: &TokenBucket)
where
    K: std::hash::Hash + Eq,
{
    map.entry(key).or_default().merge(bucket);
}

/// Increment the count at `key` in a `String -> usize` map, defaulting to
/// zero when the key is absent. Centralises the tool/mcp/shell counter
/// pattern used inside `aggregate_calls`.
fn bump<K>(map: &mut HashMap<K, usize>, key: K)
where
    K: std::hash::Hash + Eq,
{
    *map.entry(key).or_default() += 1;
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
    use std::sync::Mutex;
    use tempfile::tempdir;

    // Serialise tests that mutate AINB_NOW so parallel runs inside the
    // same process don't read each other's env. Defined here rather
    // than via lazy_static so the lock has no init-order surprises.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn last_day_of_month_handles_leap_years() {
        // Leap year: Feb has 29 days.
        assert_eq!(
            last_day_of_month(2024, 2),
            Some(NaiveDate::from_ymd_opt(2024, 2, 29).unwrap())
        );
        // Non-leap year: Feb has 28 days.
        assert_eq!(
            last_day_of_month(2023, 2),
            Some(NaiveDate::from_ymd_opt(2023, 2, 28).unwrap())
        );
        // Century non-leap (divisible by 100 but not 400).
        assert_eq!(
            last_day_of_month(1900, 2),
            Some(NaiveDate::from_ymd_opt(1900, 2, 28).unwrap())
        );
        // Quad-century leap (divisible by 400).
        assert_eq!(
            last_day_of_month(2000, 2),
            Some(NaiveDate::from_ymd_opt(2000, 2, 29).unwrap())
        );
        // 31-day month.
        assert_eq!(
            last_day_of_month(2024, 1),
            Some(NaiveDate::from_ymd_opt(2024, 1, 31).unwrap())
        );
        // December rollover into next year.
        assert_eq!(
            last_day_of_month(2024, 12),
            Some(NaiveDate::from_ymd_opt(2024, 12, 31).unwrap())
        );
        // Out-of-range month returns None instead of silently wrong data.
        assert_eq!(last_day_of_month(2024, 13), None);
        assert_eq!(last_day_of_month(2024, 0), None);
    }

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

    /// Per-test-process counter for ProviderCall.id. Tests that key
    /// off id (analyze_turns, the precompute lookup) need each call to
    /// have a distinct id; using an atomic counter keeps the helper
    /// stateless from the caller's perspective.
    static TEST_CALL_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

    fn next_test_call_id() -> u64 {
        TEST_CALL_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }

    /// Test-only convenience that wraps the shared `ProviderCallBuilder`
    /// with the parameter shape this module's parser tests expect.
    /// Picks gpt-5 for codex and claude-sonnet-4-5 otherwise; sets
    /// output_tokens to a fixed 10 (the tests rely on that constant).
    /// Each call gets a fresh `id` from `next_test_call_id` so
    /// analyze_turns can key the result map without collisions.
    fn provider_call(
        provider: &str,
        session_id: &str,
        timestamp: &str,
        message: &str,
        tools: &[&str],
        bash_commands: &[&str],
        input_tokens: u64,
    ) -> ProviderCall {
        let model = if provider == "codex" { "gpt-5" } else { "claude-sonnet-4-5" };
        crate::test_support::ProviderCallBuilder::new()
            .with_id(next_test_call_id())
            .with_provider(provider)
            .with_model(model)
            .with_session(session_id)
            .with_timestamp(parse_timestamp(timestamp).unwrap())
            .with_input_tokens(input_tokens)
            .with_output_tokens(10)
            .with_tools(tools)
            .with_bash(bash_commands)
            .with_user_message(message)
            .build()
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
    fn branches_aggregate_only_from_calls_with_recorded_branch() {
        let temp = tempdir().unwrap();
        let claude_projects = temp.path().join(".claude/projects");
        let project_dir = claude_projects.join("-Users-stevie-myrepo");
        write_file(
            &project_dir.join("session.jsonl"),
            r#"{"type":"assistant","timestamp":"2026-04-10T09:00:00Z","sessionId":"s1","gitBranch":"main","message":{"role":"assistant","model":"claude-sonnet-4-5","content":[{"type":"text","text":"a"}],"usage":{"input_tokens":100,"output_tokens":10}}}
{"type":"assistant","timestamp":"2026-04-10T09:00:01Z","sessionId":"s1","gitBranch":"main","message":{"role":"assistant","model":"claude-sonnet-4-5","content":[{"type":"text","text":"b"}],"usage":{"input_tokens":50,"output_tokens":5}}}
{"type":"assistant","timestamp":"2026-04-10T09:00:02Z","sessionId":"s1","gitBranch":"feat/x","message":{"role":"assistant","model":"claude-sonnet-4-5","content":[{"type":"text","text":"c"}],"usage":{"input_tokens":1000,"output_tokens":50}}}
{"type":"assistant","timestamp":"2026-04-10T09:00:03Z","sessionId":"s1","message":{"role":"assistant","model":"claude-sonnet-4-5","content":[{"type":"text","text":"d"}],"usage":{"input_tokens":99999,"output_tokens":99999}}}
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

        // 4 turns parsed but only 3 have a recorded branch — the 4th is
        // missing gitBranch and must be excluded from `branches`. Note its
        // huge token count: if we accidentally bucketed branchless calls
        // under "(none)" it would dominate the sort and the assertion
        // below would fail noisily.
        assert_eq!(data.calls.len(), 4);
        assert_eq!(data.branches.len(), 2, "only main and feat/x are recorded");

        // Sorted largest first by total bucket value.
        assert_eq!(data.branches[0].branch, "feat/x");
        assert_eq!(data.branches[0].bucket.input_tokens, 1000);
        assert_eq!(data.branches[1].branch, "main");
        assert_eq!(data.branches[1].bucket.input_tokens, 150);
    }

    #[test]
    fn claude_assistant_turn_carries_git_branch_through_parser() {
        let temp = tempdir().unwrap();
        let claude_projects = temp.path().join(".claude/projects");
        let project_dir = claude_projects.join("-Users-stevie-myrepo");
        write_file(
            &project_dir.join("session.jsonl"),
            r#"{"type":"user","timestamp":"2026-04-10T09:00:00Z","sessionId":"s1","gitBranch":"feat/x","message":{"role":"user","content":"go"}}
{"type":"assistant","timestamp":"2026-04-10T09:00:05Z","sessionId":"s1","gitBranch":"feat/x","message":{"role":"assistant","model":"claude-sonnet-4-5","content":[{"type":"text","text":"ok"}],"usage":{"input_tokens":10,"output_tokens":5}}}
{"type":"assistant","timestamp":"2026-04-10T09:01:00Z","sessionId":"s1","gitBranch":"","message":{"role":"assistant","model":"claude-sonnet-4-5","content":[{"type":"text","text":"again"}],"usage":{"input_tokens":1,"output_tokens":1}}}
{"type":"assistant","timestamp":"2026-04-10T09:02:00Z","sessionId":"s1","message":{"role":"assistant","model":"claude-sonnet-4-5","content":[{"type":"text","text":"none"}],"usage":{"input_tokens":1,"output_tokens":1}}}
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

        assert_eq!(data.calls.len(), 3);
        assert_eq!(data.calls[0].branch.as_deref(), Some("feat/x"));
        assert_eq!(
            data.calls[1].branch, None,
            "empty gitBranch should normalize to None so empty doesn't leak as a real branch label",
        );
        assert_eq!(data.calls[2].branch, None, "missing gitBranch becomes None");
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
        // Lookups now key on ProviderCall.id (was positional idx); the
        // helper assigns a fresh id per call from an atomic counter so
        // we read the actual id off each call rather than guessing.
        assert_eq!(analysis.get(&calls[0].id).unwrap().retries, 0);
        assert!(analysis.get(&calls[0].id).unwrap().has_edits);
        assert_eq!(analysis.get(&calls[2].id).unwrap().retries, 1);

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
                timestamp: Utc.with_ymd_and_hms(2026, 4, 29, 10, 0, 0).unwrap(),
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
                timestamp: Utc.with_ymd_and_hms(2026, 4, 29, 11, 0, 0).unwrap(),
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

        // Bounds are Utc-stored but anchored at the user's local
        // midnight; convert back to Local before reading the calendar
        // day so the assertion is independent of the host offset.
        let week_start = week.0.with_timezone(&Local).date_naive();
        let week_end = week.1.with_timezone(&Local).date_naive();
        let thirty_start = thirty.0.with_timezone(&Local).date_naive();
        let thirty_end = thirty.1.with_timezone(&Local).date_naive();

        assert_eq!((week_end - week_start).num_days() + 1, 7);
        assert_eq!((thirty_end - thirty_start).num_days() + 1, 30);
    }

    #[test]
    fn last_n_days_period_covers_n_calendar_days() {
        for n in [1u32, 7, 14, 90] {
            let (start, end) =
                date_range_for_period(&UsagePeriod::LastNDays(n)).unwrap();
            let start_d = start.with_timezone(&Local).date_naive();
            let end_d = end.with_timezone(&Local).date_naive();
            assert_eq!(
                (end_d - start_d).num_days() + 1,
                i64::from(n),
                "n={n}"
            );
        }
    }

    #[test]
    fn specific_month_period_starts_on_day_one_and_ends_on_last_day() {
        let anchor = NaiveDate::from_ymd_opt(2026, 4, 17).unwrap();
        let (start, end) =
            date_range_for_period(&UsagePeriod::SpecificMonth(anchor)).unwrap();
        assert_eq!(
            start.with_timezone(&Local).date_naive(),
            NaiveDate::from_ymd_opt(2026, 4, 1).unwrap()
        );
        assert_eq!(
            end.with_timezone(&Local).date_naive(),
            NaiveDate::from_ymd_opt(2026, 4, 30).unwrap()
        );
    }

    #[test]
    fn specific_quarter_q1_to_q4_cover_three_months_each() {
        for q in 1u8..=4 {
            let (start, end) =
                date_range_for_period(&UsagePeriod::SpecificQuarter(2026, q)).unwrap();
            let start_d = start.with_timezone(&Local).date_naive();
            let end_d = end.with_timezone(&Local).date_naive();
            // Q1 = 90 days (Jan 31 + Feb 28 + Mar 31), Q2 = 91, Q3 = 92, Q4 = 92.
            let days = (end_d - start_d).num_days() + 1;
            assert!((90..=92).contains(&days), "q={q} got {days} days");
        }
    }

    #[test]
    fn ytd_period_starts_on_jan_1() {
        let (start, _) = date_range_for_period(&UsagePeriod::YearToDate).unwrap();
        let start_d = start.with_timezone(&Local).date_naive();
        assert_eq!(start_d.month(), 1);
        assert_eq!(start_d.day(), 1);
    }

    #[test]
    fn ainb_now_overrides_today_anchor_for_period_filter() {
        // Pin "now" to a date well outside the natural Local::now() window
        // so the override is unambiguously responsible for the answer.
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let prev = std::env::var("AINB_NOW").ok();
        std::env::set_var("AINB_NOW", "2026-05-11T00:00:00Z");

        let (today_start, today_end) =
            date_range_for_period(&UsagePeriod::Today).expect("today range");
        let today_anchor = today_start.with_timezone(&Local).date_naive();
        assert_eq!(today_anchor.year(), 2026);
        assert_eq!(today_anchor.month(), 5);
        assert_eq!(today_anchor.day(), 11);
        assert_eq!(today_end.with_timezone(&Local).date_naive(), today_anchor);

        let (ytd_start, _) =
            date_range_for_period(&UsagePeriod::YearToDate).expect("ytd range");
        let ytd_start_d = ytd_start.with_timezone(&Local).date_naive();
        assert_eq!(ytd_start_d, NaiveDate::from_ymd_opt(2026, 1, 1).unwrap());

        match prev {
            Some(v) => std::env::set_var("AINB_NOW", v),
            None => std::env::remove_var("AINB_NOW"),
        }
    }

    #[test]
    fn ainb_now_invalid_value_falls_back_to_real_clock() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let prev = std::env::var("AINB_NOW").ok();
        std::env::set_var("AINB_NOW", "not-a-timestamp");

        let (today_start, _) =
            date_range_for_period(&UsagePeriod::Today).expect("today range");
        let today_anchor = today_start.with_timezone(&Local).date_naive();
        // Anchor should be within a day of real now — i.e. the override
        // didn't accidentally swap to epoch or 1970.
        let actual_today = Local::now().date_naive();
        let drift = (today_anchor - actual_today).num_days().abs();
        assert!(drift <= 1, "anchor drifted {drift} days from real today");

        match prev {
            Some(v) => std::env::set_var("AINB_NOW", v),
            None => std::env::remove_var("AINB_NOW"),
        }
    }

    #[test]
    fn quarter_of_dispatches_jan_to_dec_correctly() {
        assert_eq!(quarter_of(NaiveDate::from_ymd_opt(2026, 1, 5).unwrap()), 1);
        assert_eq!(quarter_of(NaiveDate::from_ymd_opt(2026, 3, 31).unwrap()), 1);
        assert_eq!(quarter_of(NaiveDate::from_ymd_opt(2026, 4, 1).unwrap()), 2);
        assert_eq!(quarter_of(NaiveDate::from_ymd_opt(2026, 7, 1).unwrap()), 3);
        assert_eq!(quarter_of(NaiveDate::from_ymd_opt(2026, 12, 31).unwrap()), 4);
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

    /// Two worktrees of the same upstream repo must collapse into a
    /// single ProjectUsage row keyed by `owner/repo`. We materialise a
    /// pair of throwaway repos on disk that share an `origin` so the
    /// resolver attributes both worktrees to the same upstream id.
    #[test]
    fn aggregation_collapses_worktrees_with_shared_origin() {
        let temp = tempdir().unwrap();
        let wt_a = temp.path().join("wt-a");
        let wt_b = temp.path().join("wt-b");

        for wt in [&wt_a, &wt_b] {
            let git_dir = wt.join(".git");
            std::fs::create_dir_all(&git_dir).unwrap();
            std::fs::write(
                git_dir.join("config"),
                "[remote \"origin\"]\n\turl = git@github.com:acme/widget.git\n",
            )
            .unwrap();
        }

        // Distinct sanitised project names — same upstream repo.
        let mut call1 = provider_call(
            "claude",
            "s1",
            "2026-04-10T09:00:00Z",
            "edit",
            &[],
            &[],
            100,
        );
        call1.project = "wt-a-folder".to_string();
        call1.project_path = wt_a.to_string_lossy().into_owned();

        let mut call2 = provider_call(
            "claude",
            "s2",
            "2026-04-10T10:00:00Z",
            "edit",
            &[],
            &[],
            150,
        );
        call2.project = "wt-b-folder".to_string();
        call2.project_path = wt_b.to_string_lossy().into_owned();

        let data = aggregate_calls(vec![call1, call2]);
        assert_eq!(data.projects.len(), 1, "worktrees should collapse");
        let proj = &data.projects[0];
        assert_eq!(proj.name, "acme/widget");
        assert_eq!(proj.repo.as_deref(), Some("acme/widget"));
        assert_eq!(proj.bucket.input_tokens, 250);
    }

    /// When a call's working directory has no resolvable upstream the
    /// row falls back to the sanitised project name and `repo` stays
    /// `None`.
    #[test]
    fn aggregation_falls_back_to_folder_when_no_origin() {
        let temp = tempdir().unwrap();
        let wt = temp.path().join("plain");
        std::fs::create_dir_all(&wt).unwrap();
        // No .git at all.

        let mut call = provider_call(
            "claude",
            "s1",
            "2026-04-10T09:00:00Z",
            "edit",
            &[],
            &[],
            100,
        );
        call.project = "plain-folder".to_string();
        call.project_path = wt.to_string_lossy().into_owned();

        let data = aggregate_calls(vec![call]);
        assert_eq!(data.projects.len(), 1);
        let proj = &data.projects[0];
        assert_eq!(proj.name, "plain-folder");
        assert!(proj.repo.is_none());
    }

    #[test]
    fn collect_recent_within_keeps_only_entries_inside_cutoff() {
        // Two assistant lines: one inside the 5h window, one well
        // outside it. The walker must return only the inside-window
        // call.
        let temp = tempdir().unwrap();
        let claude_projects = temp.path().join("projects");
        let project_dir = claude_projects.join("-Users-stevie-fixture");
        let now = Utc::now();
        let inside = (now - Duration::hours(2)).to_rfc3339();
        let outside = (now - Duration::hours(8)).to_rfc3339();
        let payload = format!(
            "{}\n{}\n",
            format!(
                r#"{{"type":"assistant","timestamp":"{outside}","sessionId":"s1","message":{{"role":"assistant","model":"claude-sonnet-4-5","content":[],"usage":{{"input_tokens":100,"output_tokens":50}}}}}}"#,
            ),
            format!(
                r#"{{"type":"assistant","timestamp":"{inside}","sessionId":"s1","message":{{"role":"assistant","model":"claude-sonnet-4-5","content":[],"usage":{{"input_tokens":200,"output_tokens":75}}}}}}"#,
            ),
        );
        write_file(&project_dir.join("session.jsonl"), &payload);

        let cutoff = now - Duration::hours(5);
        let calls = collect_recent_claude_calls_within_at(
            &claude_projects,
            cutoff,
            Duration::hours(1),
        );

        assert_eq!(calls.len(), 1, "only the in-window call should survive");
        let call = &calls[0];
        assert_eq!(call.input_tokens, 200);
        assert!(
            call.timestamp >= cutoff,
            "kept call must be within cutoff"
        );
    }

    #[test]
    fn collect_recent_within_skips_files_below_mtime_floor() {
        // File with one in-window assistant entry, but its mtime is
        // older than the (cutoff - grace) floor. Expectation: skipped.
        let temp = tempdir().unwrap();
        let claude_projects = temp.path().join("projects");
        let project_dir = claude_projects.join("-Users-stevie-stale");
        let now = Utc::now();
        // Even though the line itself is in-window, an old mtime
        // proves the file hasn't been written to recently — Claude
        // would have updated mtime if it were still appending. The
        // walker treats stale-mtime files as cold archives.
        let in_window = (now - Duration::minutes(30)).to_rfc3339();
        let payload = format!(
            r#"{{"type":"assistant","timestamp":"{in_window}","sessionId":"s1","message":{{"role":"assistant","model":"claude-sonnet-4-5","content":[],"usage":{{"input_tokens":99,"output_tokens":1}}}}}}"#
        );
        let session = project_dir.join("session.jsonl");
        write_file(&session, &payload);
        // Backdate mtime to 24h ago — well past the (5h + 1h) gate.
        let f = std::fs::OpenOptions::new()
            .write(true)
            .open(&session)
            .unwrap();
        let twenty_four_hours_ago = std::time::SystemTime::now()
            - std::time::Duration::from_secs(60 * 60 * 24);
        f.set_modified(twenty_four_hours_ago).unwrap();

        let cutoff = now - Duration::hours(5);
        let calls = collect_recent_claude_calls_within_at(
            &claude_projects,
            cutoff,
            Duration::hours(1),
        );
        assert!(
            calls.is_empty(),
            "files older than (cutoff - grace) should be skipped",
        );
    }

    #[test]
    fn collect_recent_within_returns_empty_when_dir_missing() {
        let temp = tempdir().unwrap();
        let projects = temp.path().join("does-not-exist");
        let calls = collect_recent_claude_calls_within_at(
            &projects,
            Utc::now() - Duration::hours(5),
            Duration::hours(1),
        );
        assert!(calls.is_empty());
    }

    // ────────────────────────────────────────────────────────────────
    // filter_usage_data_full coverage (PR A)
    //
    // Pre-PR-A the period & provider selectors were UI-only — they
    // repainted the chip strip but the dashboard widgets ignored them.
    // These tests pin the new grafana-style global filter behaviour.
    // ────────────────────────────────────────────────────────────────

    use crate::test_support::ProviderCallBuilder;

    fn calls_aggregated(calls: Vec<ProviderCall>) -> UsageData {
        aggregate_calls(calls)
    }

    #[test]
    fn filter_usage_data_full_filters_by_provider_for_codex() {
        let now = Utc::now();
        let claude_call = ProviderCallBuilder::new()
            .with_id(1)
            .with_provider("claude")
            .with_project("alpha")
            .with_timestamp(now)
            .with_input_tokens(100)
            .with_output_tokens(50)
            .build();
        let codex_call = ProviderCallBuilder::new()
            .with_id(2)
            .with_provider("codex")
            .with_project("beta")
            .with_timestamp(now)
            .with_input_tokens(200)
            .with_output_tokens(100)
            .build();
        let data = calls_aggregated(vec![claude_call, codex_call]);
        assert_eq!(data.calls.len(), 2);

        let filtered = filter_usage_data_full(
            &data,
            &UsageFilters::default(),
            &UsagePeriod::All,
            UsageProviderFilter::Codex,
        );
        assert_eq!(filtered.calls.len(), 1, "only codex call should survive");
        assert_eq!(filtered.calls[0].provider, "codex");
        assert_eq!(filtered.projects.len(), 1);
        assert_eq!(filtered.projects[0].name, "beta");
    }

    #[test]
    fn filter_usage_data_full_filters_by_period_today_excludes_old_call() {
        let now = Utc::now();
        let old = now - Duration::days(40);
        let recent_call = ProviderCallBuilder::new()
            .with_id(1)
            .with_timestamp(now)
            .with_input_tokens(100)
            .build();
        let stale_call = ProviderCallBuilder::new()
            .with_id(2)
            .with_timestamp(old)
            .with_input_tokens(999)
            .build();
        let data = calls_aggregated(vec![recent_call, stale_call]);
        assert_eq!(data.calls.len(), 2);

        let filtered = filter_usage_data_full(
            &data,
            &UsageFilters::default(),
            &UsagePeriod::Today,
            UsageProviderFilter::All,
        );
        assert_eq!(filtered.calls.len(), 1, "only today's call should survive");
        assert_eq!(filtered.calls[0].id, 1);
    }

    #[test]
    fn filter_usage_data_full_composes_chip_period_provider_filters() {
        let now = Utc::now();
        let old = now - Duration::days(40);
        // alpha + claude + today — survives all filters
        let keep = ProviderCallBuilder::new()
            .with_id(1)
            .with_provider("claude")
            .with_project("alpha")
            .with_timestamp(now)
            .build();
        // alpha + claude + old — dropped by period
        let drop_period = ProviderCallBuilder::new()
            .with_id(2)
            .with_provider("claude")
            .with_project("alpha")
            .with_timestamp(old)
            .build();
        // beta + claude + today — dropped by project chip
        let drop_chip = ProviderCallBuilder::new()
            .with_id(3)
            .with_provider("claude")
            .with_project("beta")
            .with_timestamp(now)
            .build();
        // alpha + codex + today — dropped by provider filter
        let drop_provider = ProviderCallBuilder::new()
            .with_id(4)
            .with_provider("codex")
            .with_project("alpha")
            .with_timestamp(now)
            .build();
        let data = calls_aggregated(vec![keep, drop_period, drop_chip, drop_provider]);
        assert_eq!(data.calls.len(), 4);

        let mut chip_filters = UsageFilters::default();
        chip_filters.project.push("alpha".to_string());

        let filtered = filter_usage_data_full(
            &data,
            &chip_filters,
            &UsagePeriod::Today,
            UsageProviderFilter::Claude,
        );
        assert_eq!(filtered.calls.len(), 1, "only id=1 satisfies all 3 filters");
        assert_eq!(filtered.calls[0].id, 1);
    }

    #[test]
    fn rebuild_activity_and_mcp_columns_splits_mcp_prefix_from_tools() {
        // Simulate a wire snapshot where the producer (session-reader)
        // shipped raw tool names with the mcp__ prefix unsplit. Burndown's
        // rebuild must route those into mcp_servers and leave only plain
        // tools in `tools`.
        let now = Utc::now();
        let call = ProviderCallBuilder::new()
            .with_id(1)
            .with_timestamp(now)
            .with_input_tokens(10)
            .with_tools(&["Read", "Bash", "mcp__github__create_issue", "mcp__github__list_prs"])
            .build();
        let mut data = UsageData::default();
        data.calls = vec![call];

        rebuild_activity_and_mcp_columns(&mut data);

        let tool_names: Vec<&str> = data.tools.iter().map(|n| n.name.as_str()).collect();
        assert!(tool_names.contains(&"Read"), "Read survives as a plain tool");
        assert!(tool_names.contains(&"Bash"), "Bash survives as a plain tool");
        assert!(
            !tool_names.iter().any(|name| name.starts_with("mcp__")),
            "no mcp__ tools should leak into the tools column: {tool_names:?}"
        );
        let mcp_names: Vec<&str> = data.mcp_servers.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(
            mcp_names,
            vec!["github"],
            "two mcp__github__* tools collapse to a single github server row"
        );
        assert_eq!(data.mcp_servers[0].calls, 2);
    }

    #[test]
    fn rebuild_activity_and_mcp_columns_populates_activities_when_wire_was_empty() {
        // session-reader publishes activities as Vec::new(). After
        // rebuild, the call's tools + message should land in a populated
        // activity row using burndown's richer taxonomy.
        let now = Utc::now();
        let call = ProviderCallBuilder::new()
            .with_id(1)
            .with_timestamp(now)
            .with_input_tokens(50)
            .with_tools(&["Edit", "Write"])
            .with_user_message("fix the bug in the parser")
            .build();
        let mut data = UsageData::default();
        data.calls = vec![call];

        assert!(data.activities.is_empty(), "wire shipped empty activities");
        rebuild_activity_and_mcp_columns(&mut data);

        assert!(
            !data.activities.is_empty(),
            "rebuild should populate at least one activity row"
        );
        let total_turns: usize = data.activities.iter().map(|a| a.turns).sum();
        assert_eq!(total_turns, 1, "single call should contribute one turn");
    }

    #[test]
    fn rebuild_activity_and_mcp_columns_handles_empty_calls() {
        // Defensive: empty calls (no data yet) must not panic and must
        // leave all three columns empty.
        let mut data = UsageData::default();
        rebuild_activity_and_mcp_columns(&mut data);
        assert!(data.activities.is_empty());
        assert!(data.mcp_servers.is_empty());
        assert!(data.tools.is_empty());
    }

    #[test]
    fn filter_usage_data_full_no_op_when_everything_default() {
        // No chips, period=All, provider=All — the function must return
        // the data unchanged (cheap clone path) so empty-state callers
        // hit the fast path.
        let call = ProviderCallBuilder::new()
            .with_id(7)
            .with_timestamp(Utc::now())
            .with_input_tokens(42)
            .build();
        let data = calls_aggregated(vec![call]);
        let filtered = filter_usage_data_full(
            &data,
            &UsageFilters::default(),
            &UsagePeriod::All,
            UsageProviderFilter::All,
        );
        assert_eq!(filtered.calls.len(), 1);
        assert_eq!(filtered.calls[0].id, 7);
    }

    /// Regression for the silent project-filter no-op: the chip carries
    /// the resolved repo (`owner/repo`) because that's how the
    /// By Project panel keys its rows, but the call's raw `project`
    /// field is the local folder name. The chip must still match.
    #[test]
    fn matches_with_resolved_repo_accepts_chip_against_owner_repo() {
        let call = ProviderCallBuilder::new()
            .with_id(1)
            .with_project("worktree-folder-name")
            .build();
        let mut filters = UsageFilters::default();
        filters.project.push("shotclubhouse/shotclubhouse".to_string());

        // No resolved repo → chip looks for raw folder, can't match.
        assert!(
            !filters.matches_with_resolved_repo(&call, ActivityCategory::Conversation, None),
            "without resolved repo, owner/repo chip must not match a raw-folder call.project"
        );

        // With resolved repo matching the chip → match.
        assert!(
            filters.matches_with_resolved_repo(
                &call,
                ActivityCategory::Conversation,
                Some("shotclubhouse/shotclubhouse"),
            ),
            "chip 'owner/repo' must match calls whose project_path resolves to that repo"
        );
    }

    /// The raw-folder chip path (CLI `--project local-folder` or
    /// legacy chip values) must keep working alongside the new
    /// resolved-repo path.
    #[test]
    fn matches_with_resolved_repo_keeps_raw_folder_chip_working() {
        let call = ProviderCallBuilder::new()
            .with_id(1)
            .with_project("local-folder")
            .build();
        let mut filters = UsageFilters::default();
        filters.project.push("local-folder".to_string());

        assert!(filters.matches_with_resolved_repo(
            &call,
            ActivityCategory::Conversation,
            None
        ));
        // And still works when resolved_repo is Some but differs.
        assert!(filters.matches_with_resolved_repo(
            &call,
            ActivityCategory::Conversation,
            Some("some/other-repo"),
        ));
    }

    /// exclude_project must mirror the include path — exclude chip on
    /// `owner/repo` should drop calls under any worktree of that repo.
    #[test]
    fn matches_with_resolved_repo_excludes_by_owner_repo() {
        let call = ProviderCallBuilder::new()
            .with_id(1)
            .with_project("worktree-folder-name")
            .build();
        let mut filters = UsageFilters::default();
        filters
            .exclude_project
            .push("shotclubhouse/shotclubhouse".to_string());

        assert!(
            !filters.matches_with_resolved_repo(
                &call,
                ActivityCategory::Conversation,
                Some("shotclubhouse/shotclubhouse"),
            ),
            "exclude_project on owner/repo must drop calls that resolve to that repo"
        );
        // Without the resolved repo, the call survives (chip can't
        // identify it).
        assert!(filters.matches_with_resolved_repo(
            &call,
            ActivityCategory::Conversation,
            None,
        ));
    }

    // ────────────────────────────────────────────────────────────────
    // PR E: indexed-filter parity + behaviour.
    // The indexed path MUST produce byte-identical output to the
    // linear path for any input — these tests pin that contract so
    // a future optimisation can't silently drift filter semantics.
    // ────────────────────────────────────────────────────────────────

    fn small_indexed_fixture() -> UsageData {
        let now = Utc::now();
        let claude_alpha = ProviderCallBuilder::new()
            .with_id(1)
            .with_provider("claude")
            .with_project("alpha-folder")
            .with_model("claude-sonnet-4-5")
            .with_timestamp(now)
            .with_input_tokens(100)
            .with_output_tokens(50)
            .with_tools(&["Read", "Edit"])
            .build();
        let claude_beta = ProviderCallBuilder::new()
            .with_id(2)
            .with_provider("claude")
            .with_project("beta-folder")
            .with_model("claude-opus-4-7")
            .with_timestamp(now)
            .with_input_tokens(200)
            .with_output_tokens(100)
            .with_tools(&["Bash"])
            .build();
        let codex_alpha = ProviderCallBuilder::new()
            .with_id(3)
            .with_provider("codex")
            .with_project("alpha-folder")
            .with_model("gpt-5")
            .with_timestamp(now)
            .with_input_tokens(300)
            .with_output_tokens(150)
            .with_tools(&["Edit"])
            .build();
        calls_aggregated(vec![claude_alpha, claude_beta, codex_alpha])
    }

    #[test]
    fn indexed_filter_matches_linear_on_project_chip() {
        let data = small_indexed_fixture();
        let indices = UsageIndices::from_usage_data(&data);
        let mut filters = UsageFilters::default();
        filters.project.push("alpha-folder".to_string());

        let linear = filter_usage_data_full(
            &data,
            &filters,
            &UsagePeriod::All,
            UsageProviderFilter::All,
        );
        let indexed = filter_usage_data_indexed(
            &data,
            Some(&indices),
            &filters,
            &UsagePeriod::All,
            UsageProviderFilter::All,
        );

        assert_eq!(
            linear.calls.len(),
            indexed.calls.len(),
            "indexed and linear paths must produce the same call count"
        );
        let mut lin_ids: Vec<u64> = linear.calls.iter().map(|c| c.id).collect();
        let mut idx_ids: Vec<u64> = indexed.calls.iter().map(|c| c.id).collect();
        lin_ids.sort_unstable();
        idx_ids.sort_unstable();
        assert_eq!(lin_ids, idx_ids, "same call ids must survive both paths");
    }

    #[test]
    fn indexed_filter_matches_linear_on_model_chip() {
        let data = small_indexed_fixture();
        let indices = UsageIndices::from_usage_data(&data);
        let mut filters = UsageFilters::default();
        filters.model.push("claude-opus-4-7".to_string());

        let linear = filter_usage_data_full(
            &data,
            &filters,
            &UsagePeriod::All,
            UsageProviderFilter::All,
        );
        let indexed = filter_usage_data_indexed(
            &data,
            Some(&indices),
            &filters,
            &UsagePeriod::All,
            UsageProviderFilter::All,
        );
        assert_eq!(linear.calls.len(), 1);
        assert_eq!(indexed.calls.len(), 1);
        assert_eq!(indexed.calls[0].id, 2);
    }

    #[test]
    fn indexed_filter_composes_chip_with_provider_and_period() {
        let data = small_indexed_fixture();
        let indices = UsageIndices::from_usage_data(&data);
        let mut filters = UsageFilters::default();
        // alpha-folder seeds the candidate set (2 calls: ids 1 + 3).
        filters.project.push("alpha-folder".to_string());

        // Composed with provider=Claude, only call id=1 survives.
        let indexed = filter_usage_data_indexed(
            &data,
            Some(&indices),
            &filters,
            &UsagePeriod::All,
            UsageProviderFilter::Claude,
        );
        assert_eq!(indexed.calls.len(), 1);
        assert_eq!(indexed.calls[0].id, 1);
    }

    #[test]
    fn indexed_filter_falls_back_to_linear_when_indices_absent() {
        let data = small_indexed_fixture();
        let mut filters = UsageFilters::default();
        filters.project.push("alpha-folder".to_string());

        let linear = filter_usage_data_full(
            &data,
            &filters,
            &UsagePeriod::All,
            UsageProviderFilter::All,
        );
        // Calling with indices=None must produce the same result as
        // the linear pass — the indexed function is `None`-tolerant.
        let no_indices = filter_usage_data_indexed(
            &data,
            None,
            &filters,
            &UsagePeriod::All,
            UsageProviderFilter::All,
        );
        assert_eq!(linear.calls.len(), no_indices.calls.len());
    }

    #[test]
    fn indexed_filter_no_op_when_everything_default() {
        let data = small_indexed_fixture();
        let indices = UsageIndices::from_usage_data(&data);
        let filtered = filter_usage_data_indexed(
            &data,
            Some(&indices),
            &UsageFilters::default(),
            &UsagePeriod::All,
            UsageProviderFilter::All,
        );
        assert_eq!(filtered.calls.len(), 3);
    }

    #[test]
    fn usage_indices_project_lookup_returns_both_raw_and_resolved() {
        // by_project_raw is populated from call.project — verify the
        // indices struct surface a chip can be resolved through.
        let data = small_indexed_fixture();
        let indices = UsageIndices::from_usage_data(&data);
        assert_eq!(indices.project_indices("alpha-folder").len(), 2);
        assert_eq!(indices.project_indices("beta-folder").len(), 1);
        assert!(indices.project_indices("nonexistent").is_empty());
    }
}
