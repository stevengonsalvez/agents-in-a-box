//! Per-provider scan orchestrator + UsageData aggregator.
//!
//! Walks the four providers in turn, aggregates the resulting calls
//! into a [`UsageData`] snapshot. Per-provider failures are best-effort:
//! a parse error or unreadable file degrades that provider's
//! contribution to whatever was successfully read but lets the others
//! through.
//!
//! ## Aggregator notes
//!
//! - Daily / weekly bucketing is UTC. The display layer can convert.
//! - Project key is the call's `project` field as-is — no upstream-repo
//!   resolution; two worktrees of the same upstream stay separate.
//! - `activities` and `mcp_servers` are intentionally empty here.
//!   session-reader publishes the raw call set (with `tools` +
//!   `bash_commands` per call) and leaves activity classification and
//!   mcp-server attribution to the consumer. Each subscriber owns
//!   that taxonomy because the right buckets are consumer-specific
//!   (burndown uses 12 buckets; the wire schema only carries 6). See
//!   `ainb-plugin-burndown::data::usage::rebuild_activity_and_mcp_columns`
//!   for the reference implementation.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration as StdDuration, Instant};

use ainb_plugin_types_sessions::{
    BranchUsage, ModelUsage, NamedUsage, ProjectUsage, Provider, ProviderCall, ScanProgressEvent,
    SessionUsage, TokenBucket, UsageData,
};
use chrono::{Datelike, Duration, NaiveDate};

/// Minimum gap between progress emits. Caps emission to 10 events/s and
/// satisfies the "every 100 ms or every 10 files, whichever first"
/// trigger from plan §Phase 6 — under the cap the file-count trigger
/// is naturally subsumed by the time trigger.
const PROGRESS_MIN_INTERVAL: StdDuration = StdDuration::from_millis(100);

/// Rate-limited progress sink threaded through the parsers.
///
/// The scanner constructs a reporter that wraps a closure (typically a
/// `tokio::sync::mpsc::Sender` adapter from the plugin layer). Each
/// per-file parse calls [`Self::note_file`]; the reporter throttles to
/// at most one emit per [`PROGRESS_MIN_INTERVAL`], so the publish side
/// of the plugin never fires more than 10 `sessions.scan_progress`
/// publishes per second even on a fully-warm cache that visits
/// hundreds of files per millisecond.
///
/// `noop()` is the default for tests and the un-instrumented `scan`
/// entry point — no callback runs, no rate-limit state is touched.
pub struct ProgressReporter {
    last_emit: Option<Instant>,
    scanned: u32,
    total: u32,
    callback: Box<dyn FnMut(ScanProgressEvent) + Send>,
}

impl ProgressReporter {
    /// Build a reporter whose `callback` is invoked once per emit
    /// (after rate-limit gating). Caller owns whatever channel/host
    /// adapter the closure dispatches to.
    pub fn new(callback: impl FnMut(ScanProgressEvent) + Send + 'static) -> Self {
        Self {
            last_emit: None,
            scanned: 0,
            total: 0,
            callback: Box::new(callback),
        }
    }

    /// No-op reporter — drops every event. Use from tests and from the
    /// legacy `scan` / `parse_dir_cached` paths that don't want
    /// realtime UX feedback.
    #[must_use]
    pub fn noop() -> Self {
        Self::new(|_| {})
    }

    /// Hint the total file count once it's known (e.g. after a cheap
    /// pre-walk). Optional — when `total = 0` the burndown UI omits
    /// the `/M` suffix and renders `"Scanning sessions… N files"`.
    pub fn set_total(&mut self, total: u32) {
        self.total = total;
    }

    /// Record one scanned file. Always increments the counter; emits
    /// only when [`PROGRESS_MIN_INTERVAL`] has elapsed since the last
    /// emit (or on the very first file).
    pub fn note_file(&mut self, current_project: &str) {
        self.scanned = self.scanned.saturating_add(1);
        let now = Instant::now();
        let should_emit = match self.last_emit {
            None => true,
            Some(last) => now.duration_since(last) >= PROGRESS_MIN_INTERVAL,
        };
        if should_emit {
            (self.callback)(ScanProgressEvent {
                scanned: self.scanned,
                total: self.total,
                current_project: current_project.to_string(),
            });
            self.last_emit = Some(now);
        }
    }

    /// Force-emit the current counters regardless of the rate-limit
    /// window. Used at end-of-scan so the burndown sees a final
    /// `scanned == N` tick if the previous tick fell inside the
    /// throttle window.
    pub fn flush(&mut self, current_project: &str) {
        if self.scanned == 0 {
            return;
        }
        (self.callback)(ScanProgressEvent {
            scanned: self.scanned,
            total: self.total,
            current_project: current_project.to_string(),
        });
        self.last_emit = Some(Instant::now());
    }
}

/// Source roots for the five providers. `None` skips that provider
/// entirely; `Some(path)` is walked even if the directory doesn't exist
/// (parsers degrade to empty in that case).
#[derive(Debug, Clone, Default)]
pub struct ProviderRoots {
    /// `~/.claude/projects/` — outer dir is per-project subdirs.
    pub claude_projects: Option<PathBuf>,
    /// `~/.codex/sessions/` — `<YYYY>/<MM>/<DD>/rollout-*.jsonl`.
    pub codex_sessions: Option<PathBuf>,
    /// Gemini Code Assist sessions root.
    pub gemini_sessions: Option<PathBuf>,
    /// `~/.config/github-copilot/sessions/`.
    pub copilot_sessions: Option<PathBuf>,
    /// Cursor IDE chat sessions. macOS:
    /// `~/Library/Application Support/Cursor/User/workspaceStorage`;
    /// linux: `~/.config/Cursor/User/workspaceStorage`.
    pub cursor_sessions: Option<PathBuf>,
}

impl ProviderRoots {
    /// Construct with the canonical default paths under the current
    /// user's home directory. Falls back silently when `$HOME` is not
    /// set — every provider becomes `None`.
    #[must_use]
    pub fn defaults() -> Self {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        match home {
            Some(home) => Self {
                claude_projects: Some(home.join(".claude/projects")),
                codex_sessions: Some(home.join(".codex/sessions")),
                gemini_sessions: Some(home.join(".gemini/sessions")),
                copilot_sessions: Some(home.join(".config/github-copilot/sessions")),
                cursor_sessions: Some(cursor_default_root(&home)),
            },
            None => Self::default(),
        }
    }
}

/// Pick the right Cursor workspace-storage root for the host OS.
/// macOS uses `~/Library/Application Support/Cursor/...`; other Unixes
/// follow XDG and put it under `~/.config/Cursor/...`. Windows isn't
/// targeted by the v1 release matrix.
fn cursor_default_root(home: &Path) -> PathBuf {
    if cfg!(target_os = "macos") {
        home.join("Library/Application Support/Cursor/User/workspaceStorage")
    } else {
        home.join(".config/Cursor/User/workspaceStorage")
    }
}

/// Run every provider parser, aggregate, return a snapshot.
///
/// Cache-less convenience wrapper around [`scan_with_cache`] for
/// callers that don't want persistence (tests, the wasm32 build).
pub fn scan(roots: &ProviderRoots) -> UsageData {
    #[cfg(not(target_arch = "wasm32"))]
    {
        scan_with_cache(roots, &mut None)
    }
    #[cfg(target_arch = "wasm32")]
    {
        let mut all_calls = Vec::new();
        if let Some(root) = &roots.claude_projects {
            all_calls.extend(crate::parsers::claude::parse_dir(root));
        }
        if let Some(root) = &roots.codex_sessions {
            all_calls.extend(crate::parsers::codex::parse_dir(root));
        }
        if let Some(root) = &roots.gemini_sessions {
            all_calls.extend(crate::parsers::gemini::parse_dir(root));
        }
        if let Some(root) = &roots.copilot_sessions {
            all_calls.extend(crate::parsers::copilot::parse_dir(root));
        }
        if let Some(root) = &roots.cursor_sessions {
            all_calls.extend(crate::parsers::cursor::parse_dir(root));
        }
        aggregate(all_calls)
    }
}

/// Cache-aware scan. Pass `Some(cache)` to short-circuit per-file parses
/// when `(mtime, size)` matches the previous run; `None` is equivalent
/// to the legacy [`scan`] call site.
///
/// Gemini and Copilot parsers are stubs that don't read files; they're
/// invoked without the cache.
#[cfg(not(target_arch = "wasm32"))]
pub fn scan_with_cache(
    roots: &ProviderRoots,
    cache: &mut Option<crate::cache::UsageCache>,
) -> UsageData {
    let mut reporter = ProgressReporter::noop();
    scan_with_cache_and_progress(roots, cache, &mut reporter)
}

/// Cache + progress-aware scan. Drives [`ProgressReporter::note_file`]
/// from each per-file parse so the plugin's async publish loop can
/// fan progress out to the host without blocking the scan thread.
#[cfg(not(target_arch = "wasm32"))]
pub fn scan_with_cache_and_progress(
    roots: &ProviderRoots,
    cache: &mut Option<crate::cache::UsageCache>,
    reporter: &mut ProgressReporter,
) -> UsageData {
    let mut all_calls = Vec::new();
    if let Some(root) = &roots.claude_projects {
        all_calls.extend(crate::parsers::claude::parse_dir_cached_with_progress(
            root, cache, reporter,
        ));
    }
    if let Some(root) = &roots.codex_sessions {
        all_calls.extend(crate::parsers::codex::parse_dir_cached_with_progress(
            root, cache, reporter,
        ));
    }
    if let Some(root) = &roots.gemini_sessions {
        all_calls.extend(crate::parsers::gemini::parse_dir(root));
    }
    if let Some(root) = &roots.copilot_sessions {
        all_calls.extend(crate::parsers::copilot::parse_dir(root));
    }
    if let Some(root) = &roots.cursor_sessions {
        all_calls.extend(crate::parsers::cursor::parse_dir(root));
    }
    aggregate(all_calls)
}

/// Pure aggregation: `Vec<ProviderCall>` → `UsageData`.
#[allow(clippy::too_many_lines, clippy::cast_precision_loss)]
pub fn aggregate(mut calls: Vec<ProviderCall>) -> UsageData {
    if calls.is_empty() {
        return UsageData::default();
    }

    calls.sort_by_key(|c| c.timestamp);

    let mut daily: BTreeMap<NaiveDate, BucketAccumulator> = BTreeMap::new();
    let mut weekly: BTreeMap<NaiveDate, BucketAccumulator> = BTreeMap::new();
    let mut projects: BTreeMap<String, ProjectAccumulator> = BTreeMap::new();
    let mut sessions: BTreeMap<String, SessionAccumulator> = BTreeMap::new();
    let mut models: BTreeMap<String, BucketAccumulator> = BTreeMap::new();
    let mut branches: BTreeMap<String, BucketAccumulator> = BTreeMap::new();
    let mut tools: BTreeMap<String, usize> = BTreeMap::new();
    let mut shell_commands: BTreeMap<String, usize> = BTreeMap::new();
    let mut model_project_counts: BTreeMap<String, BTreeMap<String, usize>> = BTreeMap::new();
    let mut grand_total = TokenBucket::default();

    for call in &calls {
        let bucket = call_bucket(call);
        let day = call.timestamp.date_naive();
        let week = week_start(day);
        let session_key = format!(
            "{}:{}:{}",
            call.provider.as_str(),
            call.project,
            call.session_id
        );

        merge(&mut grand_total, &bucket);

        daily.entry(day).or_default().ingest(&bucket, &call.project, &session_key);
        weekly.entry(week).or_default().ingest(&bucket, &call.project, &session_key);
        models
            .entry(call.model.clone())
            .or_default()
            .ingest(&bucket, &call.project, &session_key);
        if let Some(branch) = call.branch.as_deref().filter(|b| !b.is_empty()) {
            branches.entry(branch.to_string()).or_default().ingest(
                &bucket,
                &call.project,
                &session_key,
            );
        }

        let project = projects.entry(call.project.clone()).or_insert_with(|| ProjectAccumulator {
            path: call.project_path.clone(),
            bucket: TokenBucket::default(),
            sessions: HashSet::new(),
        });
        project.path = call.project_path.clone();
        project.sessions.insert(session_key.clone());
        merge(&mut project.bucket, &bucket);

        let session = sessions.entry(session_key.clone()).or_insert_with(|| SessionAccumulator {
            provider: call.provider,
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
        merge(&mut session.bucket, &bucket);

        for tool in &call.tools {
            *tools.entry(tool.clone()).or_insert(0) += 1;
        }
        for cmd in &call.bash_commands {
            *shell_commands.entry(cmd.clone()).or_insert(0) += 1;
        }
        *model_project_counts
            .entry(call.model.clone())
            .or_default()
            .entry(call.project.clone())
            .or_insert(0) += 1;
    }

    grand_total.call_count = calls.len();
    grand_total.session_count = sessions.len();
    grand_total.project_count = projects.len();

    UsageData {
        daily: daily
            .into_iter()
            .map(|(d, mut a)| {
                a.bucket.session_count = a.sessions.len();
                a.bucket.project_count = a.projects.len();
                (d, a.bucket)
            })
            .collect(),
        weekly: weekly
            .into_iter()
            .map(|(d, mut a)| {
                a.bucket.session_count = a.sessions.len();
                a.bucket.project_count = a.projects.len();
                (d, a.bucket)
            })
            .collect(),
        projects: sort_by_total_desc(
            projects
                .into_iter()
                .map(|(name, mut p)| {
                    p.bucket.session_count = p.sessions.len();
                    p.bucket.project_count = 1;
                    ProjectUsage {
                        name,
                        path: p.path,
                        bucket: p.bucket,
                        repo: None,
                    }
                })
                .collect(),
            |p| p.bucket,
        ),
        grand_total,
        calls: calls.clone(),
        sessions: sort_sessions_by_recency(
            sessions
                .into_iter()
                .map(|(_k, s)| SessionUsage {
                    provider: s.provider,
                    project: s.project,
                    session_id: s.session_id,
                    first_timestamp: s.first_timestamp,
                    last_timestamp: s.last_timestamp,
                    bucket: s.bucket,
                })
                .collect(),
        ),
        models: sort_by_total_desc(
            models
                .into_iter()
                .map(|(model, mut a)| {
                    a.bucket.session_count = a.sessions.len();
                    a.bucket.project_count = a.projects.len();
                    ModelUsage {
                        model,
                        bucket: a.bucket,
                    }
                })
                .collect(),
            |m| m.bucket,
        ),
        activities: Vec::new(),
        tools: map_to_named_usage_sorted(tools),
        mcp_servers: Vec::new(),
        shell_commands: map_to_named_usage_sorted(shell_commands),
        branches: sort_by_total_desc(
            branches
                .into_iter()
                .map(|(branch, mut a)| {
                    a.bucket.session_count = a.sessions.len();
                    a.bucket.project_count = a.projects.len();
                    BranchUsage {
                        branch,
                        bucket: a.bucket,
                    }
                })
                .collect(),
            |b| b.bucket,
        ),
        model_project_counts: model_project_counts
            .into_iter()
            .map(|(model, projects)| {
                let mut rows: Vec<(String, usize)> = projects.into_iter().collect();
                rows.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
                (model, rows)
            })
            .collect(),
    }
}

#[derive(Default)]
struct BucketAccumulator {
    bucket: TokenBucket,
    sessions: HashSet<String>,
    projects: HashSet<String>,
}

impl BucketAccumulator {
    fn ingest(&mut self, bucket: &TokenBucket, project: &str, session_key: &str) {
        merge(&mut self.bucket, bucket);
        self.sessions.insert(session_key.to_string());
        self.projects.insert(project.to_string());
    }
}

struct ProjectAccumulator {
    path: String,
    bucket: TokenBucket,
    sessions: HashSet<String>,
}

struct SessionAccumulator {
    provider: Provider,
    project: String,
    session_id: String,
    first_timestamp: chrono::DateTime<chrono::Utc>,
    last_timestamp: chrono::DateTime<chrono::Utc>,
    bucket: TokenBucket,
}

fn call_bucket(call: &ProviderCall) -> TokenBucket {
    TokenBucket {
        input_tokens: call.input_tokens,
        cache_creation_tokens: call.cache_creation_tokens,
        cache_read_tokens: call.cache_read_tokens,
        output_tokens: call.output_tokens,
        reasoning_tokens: call.reasoning_tokens,
        session_count: 0,
        project_count: 0,
        call_count: 1,
        cost_usd: call.cost_usd,
    }
}

fn merge(into: &mut TokenBucket, from: &TokenBucket) {
    into.input_tokens += from.input_tokens;
    into.cache_creation_tokens += from.cache_creation_tokens;
    into.cache_read_tokens += from.cache_read_tokens;
    into.output_tokens += from.output_tokens;
    into.reasoning_tokens += from.reasoning_tokens;
    into.call_count += from.call_count;
    into.cost_usd = match (into.cost_usd, from.cost_usd) {
        (Some(a), Some(b)) => Some(a + b),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    };
}

fn week_start(date: NaiveDate) -> NaiveDate {
    let days = date.weekday().num_days_from_monday();
    date - Duration::days(i64::from(days))
}

#[allow(clippy::cast_precision_loss)]
fn sort_by_total_desc<T, F>(mut rows: Vec<T>, key: F) -> Vec<T>
where
    F: Fn(&T) -> TokenBucket,
{
    rows.sort_by(|a, b| {
        let av = key(a).cost_usd.unwrap_or(key(a).total() as f64);
        let bv = key(b).cost_usd.unwrap_or(key(b).total() as f64);
        bv.total_cmp(&av)
    });
    rows
}

fn sort_sessions_by_recency(mut rows: Vec<SessionUsage>) -> Vec<SessionUsage> {
    rows.sort_by(|a, b| b.last_timestamp.cmp(&a.last_timestamp));
    rows
}

fn map_to_named_usage_sorted(map: BTreeMap<String, usize>) -> Vec<NamedUsage> {
    let mut rows: Vec<NamedUsage> =
        map.into_iter().map(|(name, calls)| NamedUsage { name, calls }).collect();
    rows.sort_by(|a, b| b.calls.cmp(&a.calls).then(a.name.cmp(&b.name)));
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Utc};
    use std::sync::{Arc, Mutex};

    #[test]
    fn progress_reporter_emits_on_first_file() {
        let captured: Arc<Mutex<Vec<ScanProgressEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&captured);
        let mut reporter = ProgressReporter::new(move |evt| sink.lock().unwrap().push(evt));
        reporter.note_file("alpha");
        let evts = captured.lock().unwrap().clone();
        assert_eq!(evts.len(), 1);
        assert_eq!(evts[0].scanned, 1);
        assert_eq!(evts[0].total, 0);
        assert_eq!(evts[0].current_project, "alpha");
    }

    #[test]
    fn progress_reporter_caps_to_ten_per_second() {
        // 50 back-to-back note_file calls — only the first should emit
        // because the rate-limit gate prevents another emit until
        // PROGRESS_MIN_INTERVAL has elapsed (100 ms).
        let captured: Arc<Mutex<Vec<ScanProgressEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&captured);
        let mut reporter = ProgressReporter::new(move |evt| sink.lock().unwrap().push(evt));
        for _ in 0..50 {
            reporter.note_file("p");
        }
        let evts = captured.lock().unwrap().clone();
        assert_eq!(evts.len(), 1, "rate-limit allows only first emit in <100ms");
        assert_eq!(evts[0].scanned, 1);
    }

    #[test]
    fn progress_reporter_emits_again_after_interval() {
        let captured: Arc<Mutex<Vec<ScanProgressEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&captured);
        let mut reporter = ProgressReporter::new(move |evt| sink.lock().unwrap().push(evt));
        reporter.note_file("a");
        std::thread::sleep(PROGRESS_MIN_INTERVAL + StdDuration::from_millis(20));
        reporter.note_file("b");
        let evts = captured.lock().unwrap().clone();
        assert_eq!(evts.len(), 2);
        assert_eq!(evts[1].scanned, 2);
        assert_eq!(evts[1].current_project, "b");
    }

    #[test]
    fn progress_reporter_noop_drops_events() {
        let mut reporter = ProgressReporter::noop();
        reporter.note_file("a");
        reporter.flush("b");
        // No panic, no observable side effects — by construction
        // there's nothing to assert beyond "this compiles + runs".
    }

    #[test]
    fn progress_reporter_set_total_propagates() {
        let captured: Arc<Mutex<Vec<ScanProgressEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&captured);
        let mut reporter = ProgressReporter::new(move |evt| sink.lock().unwrap().push(evt));
        reporter.set_total(42);
        reporter.note_file("x");
        let evts = captured.lock().unwrap().clone();
        assert_eq!(evts[0].total, 42);
    }

    #[test]
    fn progress_reporter_flush_emits_when_throttled() {
        // A flush should bypass the rate-limit and emit the current
        // counters — used at end-of-scan to guarantee a final tick.
        let captured: Arc<Mutex<Vec<ScanProgressEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&captured);
        let mut reporter = ProgressReporter::new(move |evt| sink.lock().unwrap().push(evt));
        reporter.note_file("a"); // 1st emit
        reporter.note_file("b"); // throttled
        reporter.flush("b"); // bypass throttle
        let evts = captured.lock().unwrap().clone();
        assert_eq!(evts.len(), 2);
        assert_eq!(evts[1].scanned, 2);
    }

    fn call(
        provider: Provider,
        project: &str,
        session: &str,
        ts: i64,
        input: u64,
        output: u64,
        cost: Option<f64>,
    ) -> ProviderCall {
        ProviderCall {
            id: ts as u64,
            provider,
            model: "m".into(),
            session_id: session.into(),
            project: project.into(),
            project_path: format!("/tmp/{project}"),
            timestamp: DateTime::<Utc>::from_timestamp(ts, 0).unwrap(),
            input_tokens: input,
            cache_creation_tokens: 0,
            cache_read_tokens: 0,
            output_tokens: output,
            reasoning_tokens: 0,
            cost_usd: cost,
            tools: vec!["Read".into()],
            bash_commands: vec![],
            user_message: String::new(),
            branch: Some("main".into()),
        }
    }

    #[test]
    fn empty_input_returns_default_usage_data() {
        let data = aggregate(Vec::new());
        assert_eq!(data, UsageData::default());
    }

    #[test]
    fn aggregates_grand_total_correctly() {
        let calls = vec![
            call(
                Provider::Claude,
                "p",
                "s1",
                1_700_000_000,
                10,
                20,
                Some(0.001),
            ),
            call(
                Provider::Claude,
                "p",
                "s1",
                1_700_000_001,
                30,
                40,
                Some(0.002),
            ),
        ];
        let data = aggregate(calls);
        assert_eq!(data.grand_total.input_tokens, 40);
        assert_eq!(data.grand_total.output_tokens, 60);
        assert_eq!(data.grand_total.call_count, 2);
        assert_eq!(data.grand_total.session_count, 1);
        assert_eq!(data.grand_total.project_count, 1);
        assert_eq!(data.grand_total.cost_usd, Some(0.003));
    }

    #[test]
    fn projects_sorted_by_cost_descending() {
        let calls = vec![
            call(Provider::Claude, "small", "s1", 1, 10, 10, Some(0.001)),
            call(Provider::Claude, "big", "s2", 2, 1000, 1000, Some(1.0)),
        ];
        let data = aggregate(calls);
        assert_eq!(data.projects.len(), 2);
        assert_eq!(data.projects[0].name, "big");
        assert_eq!(data.projects[1].name, "small");
    }

    #[test]
    fn sessions_sorted_by_last_timestamp_descending() {
        let calls = vec![
            call(Provider::Claude, "p", "old", 1_700_000_000, 1, 1, None),
            call(Provider::Claude, "p", "new", 1_700_000_500, 1, 1, None),
        ];
        let data = aggregate(calls);
        assert_eq!(data.sessions[0].session_id, "new");
        assert_eq!(data.sessions[1].session_id, "old");
    }

    #[test]
    fn branches_only_track_non_empty_strings() {
        let mut a = call(Provider::Claude, "p", "s1", 1, 1, 1, None);
        a.branch = Some(String::new());
        let calls = vec![a];
        let data = aggregate(calls);
        assert!(data.branches.is_empty());
    }

    #[test]
    fn model_project_counts_are_deterministically_sorted() {
        let calls = vec![
            call(Provider::Claude, "alpha", "s1", 1, 1, 1, None),
            call(Provider::Claude, "beta", "s2", 2, 1, 1, None),
            call(Provider::Claude, "alpha", "s3", 3, 1, 1, None),
        ];
        let data = aggregate(calls);
        assert_eq!(data.model_project_counts.len(), 1);
        let (_, rows) = &data.model_project_counts[0];
        assert_eq!(rows[0], ("alpha".into(), 2));
        assert_eq!(rows[1], ("beta".into(), 1));
    }

    #[test]
    fn weekly_bucket_groups_by_iso_monday() {
        // 1700000000 == 2023-11-14 22:13:20 UTC = Tuesday
        // Week start = 2023-11-13 (Monday).
        let calls = vec![call(Provider::Claude, "p", "s1", 1_700_000_000, 1, 1, None)];
        let data = aggregate(calls);
        assert_eq!(data.weekly.len(), 1);
        assert_eq!(
            data.weekly[0].0,
            NaiveDate::from_ymd_opt(2023, 11, 13).unwrap()
        );
    }

    #[test]
    fn defaults_constructor_returns_some_when_home_is_set() {
        if std::env::var_os("HOME").is_some() {
            let r = ProviderRoots::defaults();
            assert!(r.claude_projects.is_some());
            assert!(r.codex_sessions.is_some());
        }
    }
}
