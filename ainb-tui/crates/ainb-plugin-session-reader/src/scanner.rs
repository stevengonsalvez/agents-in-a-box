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
use serde::{Deserialize, Serialize};

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
                done: false,
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
            done: false,
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

/// Per-scan instrumentation. Counted in the per-file read path so
/// tests (and refresh logs) can assert what a scan actually did —
/// "0 reparses on a no-change refresh" is a counted fact, not an
/// inference from wall-clock time.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ScanCounters {
    /// Files visited by the cached walks (each costs one `stat`).
    pub files_statted: u32,
    /// Files read from disk and parsed (cache miss or no cache).
    pub parsed: u32,
    /// Files served from the per-file parse cache (deserialize only).
    pub cache_hits: u32,
    /// Files older than the watermark that were skipped entirely —
    /// no read, no cache lookup (their contribution rides the stable
    /// aggregate).
    pub stable_skipped: u32,
    /// `true` when the persisted stable aggregate was valid and reused
    /// (no rebuild pass ran).
    pub stable_reused: bool,
}

/// What the per-file read path does with files older than the
/// watermark.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StablePolicy {
    /// Fast path: record `(path, mtime, size)` and skip the file —
    /// its calls are already in the persisted stable aggregate.
    Skip,
    /// Rebuild pass: read the file (cache-served when warm) and route
    /// its calls into [`ScanCtx::stable_calls`].
    Collect,
}

/// Mutable per-scan context threaded through the cached provider
/// walks in place of the bare cache handle. Carries the watermark
/// partition policy, the routed stable output, and the counters.
pub(crate) struct ScanCtx<'a> {
    /// Per-file parse cache (None = cache-less, every file parses).
    pub(crate) cache: &'a mut Option<crate::cache::UsageCache>,
    /// Files with `mtime < watermark` are stable. `None` disables the
    /// partition entirely — every file is "recent" (the legacy full
    /// scan, byte-for-byte).
    pub(crate) watermark_nanos: Option<u64>,
    pub(crate) stable_policy: StablePolicy,
    /// `(path, mtime_nanos, size)` of every stable file seen.
    pub(crate) stable_present: Vec<(String, u64, u64)>,
    /// `(path, mtime_nanos, size)` of every *recent* (newer than the
    /// watermark) cached-provider file seen. Only collected when a
    /// watermark is set — the full-scan path doesn't pay for it. Feeds
    /// the unchanged-snapshot short-circuit in [`scan_incremental`].
    pub(crate) recent_present: Vec<(String, u64, u64)>,
    /// Stable files' calls — populated only under
    /// [`StablePolicy::Collect`].
    pub(crate) stable_calls: Vec<ProviderCall>,
    /// Files whose `stat` failed this scan. Any non-zero count poisons
    /// the unchanged-snapshot short-circuit: such a file can still
    /// parse, but has no fingerprint for the memo to compare.
    pub(crate) stat_failures: u32,
    pub(crate) counters: ScanCounters,
}

impl<'a> ScanCtx<'a> {
    /// Full-scan context: no watermark, no partition — the legacy
    /// behavior every existing entry point keeps.
    pub(crate) fn full(cache: &'a mut Option<crate::cache::UsageCache>) -> Self {
        Self {
            cache,
            watermark_nanos: None,
            stable_policy: StablePolicy::Skip,
            stable_present: Vec::new(),
            recent_present: Vec::new(),
            stable_calls: Vec::new(),
            stat_failures: 0,
            counters: ScanCounters::default(),
        }
    }

    /// Watermark-partitioned context for the incremental path.
    pub(crate) fn incremental(
        cache: &'a mut Option<crate::cache::UsageCache>,
        watermark_nanos: u64,
        stable_policy: StablePolicy,
    ) -> Self {
        Self {
            cache,
            watermark_nanos: Some(watermark_nanos),
            stable_policy,
            stable_present: Vec::new(),
            recent_present: Vec::new(),
            stable_calls: Vec::new(),
            stat_failures: 0,
            counters: ScanCounters::default(),
        }
    }
}

/// What the previous refresh saw on the recent (newer-than-watermark)
/// side — the unchanged-snapshot short-circuit's comparison key.
///
/// A refresh whose stable fingerprint set, recent fingerprint set, and
/// uncached-provider output all equal the previous refresh's must
/// produce a byte-identical snapshot (the cached-provider calls are a
/// pure function of `(path, mtime, size)` via the parse cache, and
/// fold/emit are deterministic) — so the aggregation and publish can
/// be skipped outright.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct RecentMemo {
    /// Sorted `(path, mtime_nanos, size)` of every recent
    /// cached-provider file.
    pub(crate) recent_present: Vec<(String, u64, u64)>,
    /// Gemini / Copilot / Cursor output, in walk order. These parsers
    /// are uncached so fingerprints don't exist for them; whole-output
    /// equality is the (cheap — usually empty) correctness guard.
    pub(crate) uncached_calls: Vec<ProviderCall>,
}

/// Result of an incremental scan: the snapshot, what the scan did,
/// and the (reused or rebuilt) stable aggregate the caller should
/// persist when `stable_rebuilt` is set.
pub(crate) struct ScanOutcome {
    /// The emitted snapshot — `None` when the unchanged-snapshot
    /// short-circuit proved this refresh byte-identical to the
    /// previous one (the caller keeps its published snapshot and skips
    /// the publish).
    pub(crate) data: Option<UsageData>,
    pub(crate) counters: ScanCounters,
    pub(crate) stable: StableAggregate,
    pub(crate) stable_rebuilt: bool,
    /// What this refresh saw on the recent side — feed back as `prev`
    /// on the next refresh to arm the short-circuit.
    pub(crate) memo: RecentMemo,
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
///
/// Pre-walks the Claude and Codex provider dirs to count `.jsonl`
/// files before the actual parse loop, then calls
/// [`ProgressReporter::set_total`] so the burndown UI can render a
/// real `N/M` progress bar (instead of the open-ended `N files`
/// fallback). The pre-walk is cheap — directory enumeration only, no
/// file reads — typically under 50 ms even for 5000+ Claude session
/// JSONLs. Gemini, Copilot, and Cursor parsers aren't progress-aware
/// (they don't emit `note_file`) so they're excluded from the total
/// to keep the bar honest; their file counts are usually small enough
/// that the under-count is invisible.
#[cfg(not(target_arch = "wasm32"))]
pub fn scan_with_cache_and_progress(
    roots: &ProviderRoots,
    cache: &mut Option<crate::cache::UsageCache>,
    reporter: &mut ProgressReporter,
) -> UsageData {
    // Pre-walk: count the files the progress-aware parsers will visit
    // so the UI can render an `N/M` ratio. Each branch returns 0 if the
    // root is None or unreadable — same semantics as the parse path.
    let claude_files = roots.claude_projects.as_deref().map_or(0, count_jsonl_in_two_level_tree);
    let codex_files = roots.codex_sessions.as_deref().map_or(0, count_jsonl_recursive);
    let total = claude_files.saturating_add(codex_files);
    if total > 0 {
        // Saturate at u32::MAX — unlikely in practice (would require
        // ~4 billion .jsonl files) but keeps the cast explicit.
        reporter.set_total(u32::try_from(total).unwrap_or(u32::MAX));
    }

    let mut ctx = ScanCtx::full(cache);
    let all_calls = walk_providers(roots, &mut ctx, reporter);
    aggregate(all_calls)
}

/// Walk every provider through `ctx`. Claude and Codex go through the
/// cached, watermark-aware per-file path; Gemini / Copilot / Cursor
/// parsers are uncached and always contribute to the returned (recent)
/// calls — identically in the full and incremental paths, so the
/// partition stays a valid split of the same total.
#[cfg(not(target_arch = "wasm32"))]
fn walk_providers(
    roots: &ProviderRoots,
    ctx: &mut ScanCtx<'_>,
    reporter: &mut ProgressReporter,
) -> Vec<ProviderCall> {
    let mut calls = walk_cached_providers(roots, ctx, reporter);
    calls.extend(parse_uncached_providers(roots));
    calls
}

/// The cache-aware half of [`walk_providers`]: Claude and Codex,
/// through the watermark-partitioned per-file path.
#[cfg(not(target_arch = "wasm32"))]
fn walk_cached_providers(
    roots: &ProviderRoots,
    ctx: &mut ScanCtx<'_>,
    reporter: &mut ProgressReporter,
) -> Vec<ProviderCall> {
    let mut calls = Vec::new();
    if let Some(root) = &roots.claude_projects {
        calls.extend(crate::parsers::claude::parse_dir_cached_with_progress(
            root, ctx, reporter,
        ));
    }
    if let Some(root) = &roots.codex_sessions {
        calls.extend(crate::parsers::codex::parse_dir_cached_with_progress(
            root, ctx, reporter,
        ));
    }
    calls
}

/// The uncached half of [`walk_providers`]: Gemini / Copilot / Cursor
/// parse from scratch on every scan (no per-file cache, no watermark
/// partition). Split out so [`scan_incremental`] can compare their
/// output across refreshes for the unchanged-snapshot short-circuit.
#[cfg(not(target_arch = "wasm32"))]
fn parse_uncached_providers(roots: &ProviderRoots) -> Vec<ProviderCall> {
    let mut calls = Vec::new();
    if let Some(root) = &roots.gemini_sessions {
        calls.extend(crate::parsers::gemini::parse_dir(root));
    }
    if let Some(root) = &roots.copilot_sessions {
        calls.extend(crate::parsers::copilot::parse_dir(root));
    }
    if let Some(root) = &roots.cursor_sessions {
        calls.extend(crate::parsers::cursor::parse_dir(root));
    }
    calls
}

/// Incremental scan: re-aggregate only files newer than the watermark
/// and fold the persisted stable rollup in via [`AggState::absorb`].
///
/// Pass 1 walks with [`StablePolicy::Skip`]: recent files take the
/// normal cached-read path; stable files cost one `stat` each and are
/// recorded, not read. If the recorded stable set exactly matches
/// `stored.folded`, the stored state is reused (`stable_reused`).
///
/// Any mismatch — a file aged past the watermark, was deleted, or
/// changed `(mtime, size)` — triggers one rebuild pass with
/// [`StablePolicy::Collect`]: stable files are read (cache-served when
/// warm, so typically deserialize-only) and folded into a fresh stable
/// state, which the caller persists. This set-equality contract is
/// what makes appended/edited old files impossible to double-count:
/// an appended file's mtime moves it to the recent side AND breaks
/// equality, so its stale contribution is rebuilt out.
///
/// The published snapshot is `emit(stable ⊕ fold(recent))`, sharing
/// [`emit`]/[`fold`] with [`aggregate`] — the property tests pin the
/// result byte-identical to a one-shot full scan of the same tree.
///
/// **Unchanged-snapshot short-circuit (issue #255).** When `prev` is
/// the memo of the previous refresh and (a) the stable fingerprint set
/// matches `stored.folded`, (b) the recent fingerprint set matches
/// `prev.recent_present`, and (c) the uncached providers' output
/// matches `prev.uncached_calls`, the snapshot is provably
/// byte-identical to the previous one — fold/clone/absorb/emit are
/// all skipped and `data` comes back `None` so the caller can skip
/// the (multi-hundred-MB) republish too.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn scan_incremental(
    roots: &ProviderRoots,
    cache: &mut Option<crate::cache::UsageCache>,
    stored: Option<StableAggregate>,
    watermark_nanos: u64,
    prev: Option<&RecentMemo>,
    reporter: &mut ProgressReporter,
) -> ScanOutcome {
    // Pass 1: skip stable files, parse-or-cache recent ones.
    let mut ctx = ScanCtx::incremental(cache, watermark_nanos, StablePolicy::Skip);
    let recent_calls = walk_cached_providers(roots, &mut ctx, reporter);
    let uncached_calls = parse_uncached_providers(roots);
    let mut counters = ctx.counters;
    let mut stable_present = std::mem::take(&mut ctx.stable_present);
    stable_present.sort_unstable();
    let mut recent_present = std::mem::take(&mut ctx.recent_present);
    recent_present.sort_unstable();

    let stable_matches = stored.as_ref().is_some_and(|st| st.folded == stable_present);

    // Short-circuit: nothing moved since the previous refresh — the
    // snapshot is byte-identical, skip aggregation and tell the caller
    // to skip the publish. Any stat failure disarms it: a file without
    // a fingerprint can still contribute calls the memo can't see.
    if let (true, 0, Some(prev)) = (stable_matches, ctx.stat_failures, prev) {
        if prev.recent_present == recent_present && prev.uncached_calls == uncached_calls {
            counters.stable_reused = true;
            return ScanOutcome {
                data: None,
                counters,
                stable: stored.expect("matched above"),
                stable_rebuilt: false,
                memo: RecentMemo {
                    recent_present,
                    uncached_calls,
                },
            };
        }
    }

    let (stable, stable_rebuilt, recent_calls, recent_present) = if stable_matches {
        counters.stable_reused = true;
        (
            stored.expect("matched above"),
            false,
            recent_calls,
            recent_present,
        )
    } else {
        // Rebuild pass: read stable files too (cache-served when
        // warm) and fold a fresh rollup. Progress already ticked
        // in pass 1, so this pass reports to a noop sink. The
        // uncached providers are NOT re-parsed — pass 1's output is
        // reused (they have no stable/recent partition).
        let mut rebuild_ctx = ScanCtx::incremental(cache, watermark_nanos, StablePolicy::Collect);
        let mut noop = ProgressReporter::noop();
        let recent2 = walk_cached_providers(roots, &mut rebuild_ctx, &mut noop);
        counters.files_statted += rebuild_ctx.counters.files_statted;
        counters.parsed += rebuild_ctx.counters.parsed;
        counters.cache_hits += rebuild_ctx.counters.cache_hits;
        let mut folded = std::mem::take(&mut rebuild_ctx.stable_present);
        folded.sort_unstable();
        let mut recent_present2 = std::mem::take(&mut rebuild_ctx.recent_present);
        recent_present2.sort_unstable();
        let state = fold(std::mem::take(&mut rebuild_ctx.stable_calls));
        (
            StableAggregate {
                watermark_nanos,
                folded,
                state,
            },
            true,
            recent2,
            recent_present2,
        )
    };

    let mut merged = stable.state.clone();
    let mut all_recent = recent_calls;
    all_recent.extend(uncached_calls.iter().cloned());
    merged.absorb(fold(all_recent));
    let data = emit(merged);

    ScanOutcome {
        data: Some(data),
        counters,
        stable,
        stable_rebuilt,
        memo: RecentMemo {
            recent_present,
            uncached_calls,
        },
    }
}

/// Count `.jsonl` files in the Claude layout: `<root>/<project>/<session>.jsonl`.
/// Two-level walk (project dir → session files). Matches the iteration
/// shape of `parsers::claude::parse_dir_cached_with_progress` so the
/// running counter and the pre-walk total stay in sync.
#[cfg(not(target_arch = "wasm32"))]
fn count_jsonl_in_two_level_tree(root: &Path) -> usize {
    let mut count = 0usize;
    let Ok(entries) = std::fs::read_dir(root) else {
        return 0;
    };
    for project_entry in entries.flatten() {
        let p = project_entry.path();
        if !p.is_dir() {
            continue;
        }
        let Ok(session_entries) = std::fs::read_dir(&p) else {
            continue;
        };
        for session_entry in session_entries.flatten() {
            if session_entry.path().extension().and_then(|s| s.to_str()) == Some("jsonl") {
                count = count.saturating_add(1);
            }
        }
    }
    count
}

/// Count `.jsonl` files recursively under `root`. Used for the Codex
/// layout (`<root>/<YYYY>/<MM>/<DD>/rollout-*.jsonl`) where depth
/// varies. Plain depth-first walk; symlinks aren't followed.
#[cfg(not(target_arch = "wasm32"))]
fn count_jsonl_recursive(root: &Path) -> usize {
    let mut count = 0usize;
    let mut stack: Vec<PathBuf> = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let p = entry.path();
            let Ok(ft) = entry.file_type() else { continue };
            if ft.is_dir() {
                stack.push(p);
            } else if ft.is_file() && p.extension().and_then(|s| s.to_str()) == Some("jsonl") {
                count = count.saturating_add(1);
            }
        }
    }
    count
}

/// Pure aggregation: `Vec<ProviderCall>` → `UsageData`.
///
/// Implemented as [`fold`] (per-call accumulation) followed by [`emit`]
/// (derive the sorted, deterministic snapshot). The incremental refresh
/// path reuses the same two stages — it folds only recent calls, merges
/// onto a persisted stable [`AggState`] via [`AggState::absorb`], and
/// emits — so both paths share the exact code that determines the
/// published bytes.
pub fn aggregate(calls: Vec<ProviderCall>) -> UsageData {
    emit(fold(calls))
}

/// Fold stage: accumulate calls into mergeable per-dimension state.
///
/// Calls sort by `(timestamp, id)` — a total order (`id` is FNV-1a 64
/// of `path:offset`, unique per call) — so the fold result is
/// independent of input order and [`AggState::absorb`] reproduces
/// exactly what one fold over the concatenated input would build.
///
/// Costs accumulate as integer nano-USD ([`usd_to_nanos`]) rather than
/// `f64`: float addition is not associative, so summing in a different
/// order (incremental merge vs one-shot fold) could drift in the last
/// ulp and break the byte-identity contract. Integer addition is
/// exact; [`emit`] materializes the `f64` once at the end.
#[allow(clippy::too_many_lines, clippy::cast_precision_loss)]
pub(crate) fn fold(mut calls: Vec<ProviderCall>) -> AggState {
    calls.sort_by_key(|c| (c.timestamp, c.id));

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
    let mut grand_cost_nanos: Option<i64> = None;

    for call in &calls {
        let bucket = call_bucket(call);
        let cost = call.cost_usd.map(usd_to_nanos);
        let day = call.timestamp.date_naive();
        let week = week_start(day);
        let session_key = format!(
            "{}:{}:{}",
            call.provider.as_str(),
            call.project,
            call.session_id
        );

        merge(&mut grand_total, &bucket);
        add_cost_nanos(&mut grand_cost_nanos, cost);

        daily.entry(day).or_default().ingest(&bucket, cost, &call.project, &session_key);
        weekly
            .entry(week)
            .or_default()
            .ingest(&bucket, cost, &call.project, &session_key);
        models.entry(call.model.clone()).or_default().ingest(
            &bucket,
            cost,
            &call.project,
            &session_key,
        );
        if let Some(branch) = call.branch.as_deref().filter(|b| !b.is_empty()) {
            branches.entry(branch.to_string()).or_default().ingest(
                &bucket,
                cost,
                &call.project,
                &session_key,
            );
        }

        let project = projects.entry(call.project.clone()).or_insert_with(|| ProjectAccumulator {
            path: call.project_path.clone(),
            last_path_key: (call.timestamp, call.id),
            bucket: TokenBucket::default(),
            cost_nanos: None,
            sessions: HashSet::new(),
        });
        project.path = call.project_path.clone();
        project.last_path_key = (call.timestamp, call.id);
        project.sessions.insert(session_key.clone());
        merge(&mut project.bucket, &bucket);
        add_cost_nanos(&mut project.cost_nanos, cost);

        let session = sessions.entry(session_key.clone()).or_insert_with(|| SessionAccumulator {
            provider: call.provider,
            project: call.project.clone(),
            project_path: call.project_path.clone(),
            session_id: call.session_id.clone(),
            first_timestamp: call.timestamp,
            last_timestamp: call.timestamp,
            bucket: TokenBucket::default(),
            cost_nanos: None,
        });
        if call.timestamp < session.first_timestamp {
            session.first_timestamp = call.timestamp;
        }
        if call.timestamp > session.last_timestamp {
            session.last_timestamp = call.timestamp;
        }
        merge(&mut session.bucket, &bucket);
        add_cost_nanos(&mut session.cost_nanos, cost);

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

    AggState {
        calls,
        daily,
        weekly,
        projects,
        sessions,
        models,
        branches,
        tools,
        shell_commands,
        model_project_counts,
        grand_total,
        grand_cost_nanos,
    }
}

/// Emit stage: derive the sorted, deterministic [`UsageData`] from a
/// fold state. Shared verbatim by [`aggregate`] and the incremental
/// merge path, so both produce byte-identical snapshots for the same
/// underlying calls.
#[allow(clippy::too_many_lines, clippy::cast_precision_loss)]
pub(crate) fn emit(state: AggState) -> UsageData {
    let AggState {
        calls,
        daily,
        weekly,
        projects,
        sessions,
        models,
        branches,
        tools,
        shell_commands,
        model_project_counts,
        mut grand_total,
        grand_cost_nanos,
    } = state;

    grand_total.call_count = calls.len();
    grand_total.session_count = sessions.len();
    grand_total.project_count = projects.len();
    grand_total.cost_usd = grand_cost_nanos.map(nanos_to_usd);

    UsageData {
        daily: daily
            .into_iter()
            .map(|(d, mut a)| {
                a.bucket.session_count = a.sessions.len();
                a.bucket.project_count = a.projects.len();
                a.bucket.cost_usd = a.cost_nanos.map(nanos_to_usd);
                (d, a.bucket)
            })
            .collect(),
        weekly: weekly
            .into_iter()
            .map(|(d, mut a)| {
                a.bucket.session_count = a.sessions.len();
                a.bucket.project_count = a.projects.len();
                a.bucket.cost_usd = a.cost_nanos.map(nanos_to_usd);
                (d, a.bucket)
            })
            .collect(),
        projects: sort_by_total_desc(
            projects
                .into_iter()
                .map(|(name, mut p)| {
                    p.bucket.session_count = p.sessions.len();
                    p.bucket.project_count = 1;
                    p.bucket.cost_usd = p.cost_nanos.map(nanos_to_usd);
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
        calls,
        sessions: sort_sessions_by_recency(
            sessions
                .into_iter()
                .map(|(_k, mut s)| {
                    s.bucket.cost_usd = s.cost_nanos.map(nanos_to_usd);
                    SessionUsage {
                        provider: s.provider,
                        project: s.project,
                        project_path: s.project_path,
                        session_id: s.session_id,
                        first_timestamp: s.first_timestamp,
                        last_timestamp: s.last_timestamp,
                        bucket: s.bucket,
                    }
                })
                .collect(),
        ),
        models: sort_by_total_desc(
            models
                .into_iter()
                .map(|(model, mut a)| {
                    a.bucket.session_count = a.sessions.len();
                    a.bucket.project_count = a.projects.len();
                    a.bucket.cost_usd = a.cost_nanos.map(nanos_to_usd);
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
                    a.bucket.cost_usd = a.cost_nanos.map(nanos_to_usd);
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

/// Persisted stable (older-than-watermark) aggregate: the fold state
/// of every file whose mtime predates the watermark, plus the exact
/// fingerprint set it was built from.
///
/// Validity contract: the stored state is reusable on a refresh iff
/// the walk's stable file set — every `(path, mtime, size)` older than
/// the watermark — equals `folded` exactly. Any aged-in, deleted, or
/// touched file breaks equality and forces a rebuild, which is what
/// makes an edited-then-aged or appended file impossible to
/// double-count.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct StableAggregate {
    /// `now - incremental_window_days` (Unix nanos) at build time.
    /// Bookkeeping only — validity is decided by `folded` equality.
    pub(crate) watermark_nanos: u64,
    /// Sorted `(path, mtime_nanos, size)` of every folded file.
    pub(crate) folded: Vec<(String, u64, u64)>,
    /// The fold state of the folded files' calls.
    pub(crate) state: AggState,
}

/// Mergeable fold state — the output of [`fold`], the input of
/// [`emit`], and the unit the incremental path persists as the stable
/// (older-than-watermark) aggregate.
///
/// Every field merges associatively in [`Self::absorb`]: token sums
/// add, distinct-id sets union, session first/last take min/max, and
/// costs add as integer nano-USD. Distinct counts are NOT stored here —
/// [`emit`] derives them from the set sizes, which is what makes the
/// merge exact where naive `session_count` addition would over-count.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct AggState {
    /// All calls, sorted by `(timestamp, id)`.
    pub(crate) calls: Vec<ProviderCall>,
    daily: BTreeMap<NaiveDate, BucketAccumulator>,
    weekly: BTreeMap<NaiveDate, BucketAccumulator>,
    projects: BTreeMap<String, ProjectAccumulator>,
    sessions: BTreeMap<String, SessionAccumulator>,
    models: BTreeMap<String, BucketAccumulator>,
    branches: BTreeMap<String, BucketAccumulator>,
    tools: BTreeMap<String, usize>,
    shell_commands: BTreeMap<String, usize>,
    model_project_counts: BTreeMap<String, BTreeMap<String, usize>>,
    grand_total: TokenBucket,
    grand_cost_nanos: Option<i64>,
}

impl AggState {
    /// Merge `other` into `self` so that
    /// `emit(fold(a) ⊕ fold(b)) == emit(fold(a ++ b))` byte-for-byte.
    ///
    /// On key collisions buckets merge and sets union; for the
    /// project-path last-write the side with the greater
    /// `(timestamp, id)` wins, mirroring fold's iteration order.
    pub(crate) fn absorb(&mut self, other: Self) {
        // Merge two (timestamp, id)-sorted call vecs; `self` first on
        // (impossible-in-practice) equal keys to mirror stable sort.
        let mut merged = Vec::with_capacity(self.calls.len() + other.calls.len());
        let mut a = std::mem::take(&mut self.calls).into_iter().peekable();
        let mut b = other.calls.into_iter().peekable();
        loop {
            match (a.peek(), b.peek()) {
                (Some(x), Some(y)) => {
                    if (x.timestamp, x.id) <= (y.timestamp, y.id) {
                        merged.push(a.next().expect("peeked"));
                    } else {
                        merged.push(b.next().expect("peeked"));
                    }
                }
                (Some(_), None) => merged.push(a.next().expect("peeked")),
                (None, Some(_)) => merged.push(b.next().expect("peeked")),
                (None, None) => break,
            }
        }
        self.calls = merged;

        for (k, v) in other.daily {
            merge_bucket_acc(self.daily.entry(k).or_default(), v);
        }
        for (k, v) in other.weekly {
            merge_bucket_acc(self.weekly.entry(k).or_default(), v);
        }
        for (k, v) in other.models {
            merge_bucket_acc(self.models.entry(k).or_default(), v);
        }
        for (k, v) in other.branches {
            merge_bucket_acc(self.branches.entry(k).or_default(), v);
        }
        for (k, v) in other.projects {
            match self.projects.entry(k) {
                std::collections::btree_map::Entry::Occupied(mut e) => e.get_mut().absorb(v),
                std::collections::btree_map::Entry::Vacant(e) => {
                    e.insert(v);
                }
            }
        }
        for (k, v) in other.sessions {
            match self.sessions.entry(k) {
                std::collections::btree_map::Entry::Occupied(mut e) => e.get_mut().absorb(&v),
                std::collections::btree_map::Entry::Vacant(e) => {
                    e.insert(v);
                }
            }
        }
        for (k, n) in other.tools {
            *self.tools.entry(k).or_insert(0) += n;
        }
        for (k, n) in other.shell_commands {
            *self.shell_commands.entry(k).or_insert(0) += n;
        }
        for (model, inner) in other.model_project_counts {
            let mine = self.model_project_counts.entry(model).or_default();
            for (project, n) in inner {
                *mine.entry(project).or_insert(0) += n;
            }
        }
        merge(&mut self.grand_total, &other.grand_total);
        add_cost_nanos(&mut self.grand_cost_nanos, other.grand_cost_nanos);
    }
}

fn merge_bucket_acc(into: &mut BucketAccumulator, from: BucketAccumulator) {
    merge(&mut into.bucket, &from.bucket);
    add_cost_nanos(&mut into.cost_nanos, from.cost_nanos);
    into.sessions.extend(from.sessions);
    into.projects.extend(from.projects);
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct BucketAccumulator {
    bucket: TokenBucket,
    cost_nanos: Option<i64>,
    sessions: HashSet<String>,
    projects: HashSet<String>,
}

impl BucketAccumulator {
    fn ingest(
        &mut self,
        bucket: &TokenBucket,
        cost: Option<i64>,
        project: &str,
        session_key: &str,
    ) {
        merge(&mut self.bucket, bucket);
        add_cost_nanos(&mut self.cost_nanos, cost);
        self.sessions.insert(session_key.to_string());
        self.projects.insert(project.to_string());
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ProjectAccumulator {
    path: String,
    /// `(timestamp, id)` of the call that last wrote `path` — fold's
    /// last-write-wins replayed exactly during [`AggState::absorb`].
    last_path_key: (chrono::DateTime<chrono::Utc>, u64),
    bucket: TokenBucket,
    cost_nanos: Option<i64>,
    sessions: HashSet<String>,
}

impl ProjectAccumulator {
    fn absorb(&mut self, other: Self) {
        if other.last_path_key >= self.last_path_key {
            self.path = other.path;
            self.last_path_key = other.last_path_key;
        }
        merge(&mut self.bucket, &other.bucket);
        add_cost_nanos(&mut self.cost_nanos, other.cost_nanos);
        self.sessions.extend(other.sessions);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SessionAccumulator {
    provider: Provider,
    project: String,
    project_path: String,
    session_id: String,
    first_timestamp: chrono::DateTime<chrono::Utc>,
    last_timestamp: chrono::DateTime<chrono::Utc>,
    bucket: TokenBucket,
    cost_nanos: Option<i64>,
}

impl SessionAccumulator {
    fn absorb(&mut self, other: &Self) {
        if other.first_timestamp < self.first_timestamp {
            self.first_timestamp = other.first_timestamp;
        }
        if other.last_timestamp > self.last_timestamp {
            self.last_timestamp = other.last_timestamp;
        }
        merge(&mut self.bucket, &other.bucket);
        add_cost_nanos(&mut self.cost_nanos, other.cost_nanos);
    }
}

/// Convert a per-call USD cost to integer nano-USD for exact,
/// associative accumulation. Rounded once per call, so one-shot and
/// incremental paths see identical integer inputs.
fn usd_to_nanos(usd: f64) -> i64 {
    (usd * 1e9).round() as i64
}

/// Materialize one accumulated nano-USD sum back to the published
/// `f64`. Callers map over their `Option` cost.
#[allow(clippy::cast_precision_loss)]
fn nanos_to_usd(nanos: i64) -> f64 {
    nanos as f64 / 1e9
}

/// `Option` cost addition with the same None-coalescing semantics as
/// [`merge`]'s `cost_usd` arm: any `Some` survives, two `Some`s add.
fn add_cost_nanos(into: &mut Option<i64>, from: Option<i64>) {
    *into = match (*into, from) {
        (Some(a), Some(b)) => Some(a.saturating_add(b)),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    };
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
        // Cost rides the accumulators as integer nano-USD (see `fold`);
        // the bucket's f64 is materialized once in `emit`.
        cost_usd: None,
    }
}

fn merge(into: &mut TokenBucket, from: &TokenBucket) {
    // Saturating sums: token counts come straight from on-disk JSONL
    // (corruptible / hostile), and a debug-build overflow panic inside
    // the blocking scan task would cost the plugin its cache handle.
    // Unsigned saturating addition stays associative, so the
    // byte-identity merge contract is unaffected.
    into.input_tokens = into.input_tokens.saturating_add(from.input_tokens);
    into.cache_creation_tokens =
        into.cache_creation_tokens.saturating_add(from.cache_creation_tokens);
    into.cache_read_tokens = into.cache_read_tokens.saturating_add(from.cache_read_tokens);
    into.output_tokens = into.output_tokens.saturating_add(from.output_tokens);
    into.reasoning_tokens = into.reasoning_tokens.saturating_add(from.reasoning_tokens);
    into.call_count = into.call_count.saturating_add(from.call_count);
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

    /// Profiling harness for issue #255 — NOT a correctness test.
    ///
    /// Replays one steady-state incremental refresh phase by phase
    /// against a copy of the real on-disk cache + the real `$HOME`
    /// session data, printing wall-clock per phase. Run manually:
    ///
    /// ```sh
    /// cargo test -p ainb-plugin-session-reader --release \
    ///   profile_real_refresh_phases -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "profiling harness against real $HOME data — run manually"]
    fn profile_real_refresh_phases() {
        use std::time::Instant;

        let Some(db) = crate::cache::default_db_path() else {
            eprintln!("profile: no resolvable cache path; skipping");
            return;
        };
        if !db.exists() {
            eprintln!("profile: no real cache at {}; skipping", db.display());
            return;
        }
        // Copy the live db (plus WAL/SHM so an in-flight write is
        // recoverable) — never contend with a running plugin instance.
        let tmp = tempfile::tempdir().expect("tempdir");
        let copy = tmp.path().join("usage.sqlite");
        std::fs::copy(&db, &copy).expect("copy db");
        for sfx in ["-wal", "-shm"] {
            let mut src = db.as_os_str().to_owned();
            src.push(sfx);
            let src = std::path::PathBuf::from(src);
            if src.exists() {
                let mut dst = copy.as_os_str().to_owned();
                dst.push(sfx);
                std::fs::copy(&src, std::path::PathBuf::from(dst)).expect("copy sidecar");
            }
        }
        let mut cache = Some(crate::cache::UsageCache::open(&copy).expect("open copy"));

        let t = Instant::now();
        let stored = cache.as_ref().unwrap().load_stable().unwrap_or_default();
        let load_stable_t = t.elapsed();
        let Some(stored) = stored else {
            eprintln!("profile: no stable rollup in cache; run a refresh first");
            return;
        };
        eprintln!(
            "load_stable: {load_stable_t:?} (folded={} stable_calls={})",
            stored.folded.len(),
            stored.state.calls.len()
        );

        // Reuse the rollup's own watermark so the stored fingerprint
        // set stays valid (steady-state path, no rebuild pass).
        let watermark = stored.watermark_nanos;
        let roots = ProviderRoots::defaults();

        let t = Instant::now();
        let mut ctx = ScanCtx::incremental(&mut cache, watermark, StablePolicy::Skip);
        let mut reporter = ProgressReporter::noop();
        let recent_calls = walk_providers(&roots, &mut ctx, &mut reporter);
        let walk_t = t.elapsed();
        eprintln!(
            "pass1 walk (stat + recent hydrate): {walk_t:?} \
             (statted={} parsed={} cache_hits={} stable_skipped={} recent_calls={})",
            ctx.counters.files_statted,
            ctx.counters.parsed,
            ctx.counters.cache_hits,
            ctx.counters.stable_skipped,
            recent_calls.len()
        );
        let mut stable_present = std::mem::take(&mut ctx.stable_present);
        stable_present.sort_unstable();
        eprintln!(
            "stable set match: {} (walk={} stored={})",
            stable_present == stored.folded,
            stable_present.len(),
            stored.folded.len()
        );

        let t = Instant::now();
        let folded_recent = fold(recent_calls);
        eprintln!("fold(recent): {:?}", t.elapsed());

        let t = Instant::now();
        // Deliberate clone: this measures exactly the clone the
        // production merge path pays per refresh.
        #[allow(clippy::redundant_clone)]
        let mut merged = stored.state.clone();
        eprintln!("stable.state.clone(): {:?}", t.elapsed());

        let t = Instant::now();
        merged.absorb(folded_recent);
        eprintln!("absorb: {:?}", t.elapsed());

        let t = Instant::now();
        let data = emit(merged);
        eprintln!("emit: {:?} (total_calls={})", t.elapsed(), data.calls.len());

        let t = Instant::now();
        let chunks = crate::plugin::chunk_usage_data(data, 0, false, 2 * 1024 * 1024);
        eprintln!(
            "chunk_usage_data: {:?} ({} chunks)",
            t.elapsed(),
            chunks.len()
        );

        let t = Instant::now();
        let total: usize = chunks
            .iter()
            .map(|c| rmp_serde::to_vec_named(c).map(|b| b.len()).unwrap_or(0))
            .sum();
        eprintln!(
            "final encode all chunks: {:?} ({} bytes)",
            t.elapsed(),
            total
        );
    }

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

    #[test]
    fn count_jsonl_two_level_matches_real_layout() {
        // Build a fake Claude-layout: <root>/<project>/<session>.jsonl
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("proj-a")).unwrap();
        std::fs::create_dir_all(root.join("proj-b")).unwrap();
        std::fs::write(root.join("proj-a/s1.jsonl"), b"{}").unwrap();
        std::fs::write(root.join("proj-a/s2.jsonl"), b"{}").unwrap();
        std::fs::write(root.join("proj-b/s3.jsonl"), b"{}").unwrap();
        // Ignored: non-jsonl file, plus a stray file at the root level.
        std::fs::write(root.join("proj-a/notes.txt"), b"hi").unwrap();
        std::fs::write(root.join("toplevel-stray.jsonl"), b"{}").unwrap();

        assert_eq!(count_jsonl_in_two_level_tree(root), 3);
    }

    #[test]
    fn count_jsonl_recursive_walks_arbitrary_depth() {
        // Codex layout: <root>/<YYYY>/<MM>/<DD>/rollout-*.jsonl
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let dir = root.join("2026/05/19");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("rollout-1.jsonl"), b"{}").unwrap();
        std::fs::write(dir.join("rollout-2.jsonl"), b"{}").unwrap();
        std::fs::write(dir.join("rollout-3.txt"), b"hi").unwrap(); // not jsonl
        // Another day with one rollout.
        let dir2 = root.join("2026/05/18");
        std::fs::create_dir_all(&dir2).unwrap();
        std::fs::write(dir2.join("rollout-1.jsonl"), b"{}").unwrap();

        assert_eq!(count_jsonl_recursive(root), 3);
    }

    #[test]
    fn count_jsonl_returns_zero_for_missing_root() {
        let missing = std::path::PathBuf::from("/nonexistent/path/to/nowhere");
        assert_eq!(count_jsonl_in_two_level_tree(&missing), 0);
        assert_eq!(count_jsonl_recursive(&missing), 0);
    }

    #[test]
    fn scan_pre_walk_sets_total_on_reporter() {
        // End-to-end: build fake Claude + Codex trees, run a scan with
        // a real ProgressReporter, and verify `total` equals the actual
        // file count when the first note_file fires.
        let tmp = tempfile::tempdir().unwrap();
        let claude_root = tmp.path().join("claude/projects");
        let codex_root = tmp.path().join("codex/sessions");
        std::fs::create_dir_all(claude_root.join("proj-a")).unwrap();
        std::fs::write(claude_root.join("proj-a/s1.jsonl"), b"").unwrap();
        std::fs::write(claude_root.join("proj-a/s2.jsonl"), b"").unwrap();
        std::fs::create_dir_all(codex_root.join("2026/05/19")).unwrap();
        std::fs::write(codex_root.join("2026/05/19/rollout.jsonl"), b"").unwrap();

        let roots = ProviderRoots {
            claude_projects: Some(claude_root),
            codex_sessions: Some(codex_root),
            ..ProviderRoots::default()
        };

        let captured: Arc<Mutex<Vec<ScanProgressEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&captured);
        let mut reporter = ProgressReporter::new(move |evt| sink.lock().unwrap().push(evt));
        let mut cache: Option<crate::cache::UsageCache> = None;
        let _data = scan_with_cache_and_progress(&roots, &mut cache, &mut reporter);

        let evts = captured.lock().unwrap().clone();
        // 3 files total (2 Claude + 1 Codex). First emit should carry
        // total=3; later emits inherit the same total via set_total.
        assert!(!evts.is_empty(), "reporter saw at least one event");
        assert_eq!(
            evts[0].total, 3,
            "pre-walk set total to count of progress-aware files"
        );
    }

    // ── merge ≡ aggregate property tests ────────────────────────────
    //
    // The byte-identity contract: folding a partition of the calls and
    // absorbing the parts must emit EXACTLY the bytes a one-shot
    // aggregate over all calls emits. Seeded xorshift PRNG instead of
    // a proptest dependency — reproducible, zero new deps, hundreds of
    // randomized cases per run.

    fn xorshift(state: &mut u64) -> u64 {
        let mut x = *state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        *state = x;
        x
    }

    fn random_call(rng: &mut u64, id: u64) -> ProviderCall {
        let providers = [Provider::Claude, Provider::Codex, Provider::Gemini];
        let projects = ["alpha", "beta", "gamma", "delta"];
        let sessions = ["s1", "s2", "s3"];
        let models = ["m-small", "m-big", "m-think"];
        let tool_pool = ["Read", "Edit", "Bash", "Grep"];
        let cmd_pool = ["ls", "cargo test"];
        let branch_pool = [None, Some("main"), Some("dev"), Some("")];

        // Narrow timestamp range (~4 days) so daily/weekly/session
        // buckets collide across partitions; allow exact-duplicate
        // timestamps to exercise the (timestamp, id) tiebreak.
        let ts = 1_700_000_000 + i64::try_from(xorshift(rng) % 350_000).unwrap();
        let cost = match xorshift(rng) % 3 {
            0 => None,
            // Adversarial float costs: tiny + huge magnitudes in one
            // sum is exactly where f64 association order would leak.
            1 => Some((xorshift(rng) % 1_000_000) as f64 / 1e6),
            _ => Some((xorshift(rng) % 97) as f64 + 0.000_001),
        };
        let n_tools = (xorshift(rng) % 3) as usize;

        ProviderCall {
            id,
            provider: providers[(xorshift(rng) % 3) as usize],
            model: models[(xorshift(rng) % 3) as usize].into(),
            session_id: sessions[(xorshift(rng) % 3) as usize].into(),
            project: projects[(xorshift(rng) % 4) as usize].into(),
            project_path: format!("/tmp/{}", projects[(xorshift(rng) % 4) as usize]),
            timestamp: DateTime::<Utc>::from_timestamp(ts, 0).unwrap(),
            input_tokens: xorshift(rng) % 10_000,
            cache_creation_tokens: xorshift(rng) % 1_000,
            cache_read_tokens: xorshift(rng) % 50_000,
            output_tokens: xorshift(rng) % 5_000,
            reasoning_tokens: xorshift(rng) % 2_000,
            cost_usd: cost,
            tools: tool_pool[..n_tools].iter().map(|s| (*s).into()).collect(),
            bash_commands: cmd_pool[..(xorshift(rng) % 2) as usize]
                .iter()
                .map(|s| (*s).into())
                .collect(),
            user_message: String::new(),
            branch: branch_pool[(xorshift(rng) % 4) as usize].map(Into::into),
        }
    }

    fn encode(data: &UsageData) -> Vec<u8> {
        rmp_serde::to_vec_named(data).expect("encode UsageData")
    }

    #[test]
    fn absorb_of_random_two_way_partition_is_byte_identical_to_aggregate() {
        let mut rng: u64 = 0x5EED_CAFE_F00D_0001;
        for case in 0..300 {
            let n = (xorshift(&mut rng) % 60) as usize;
            let calls: Vec<ProviderCall> =
                (0..n).map(|i| random_call(&mut rng, 1_000 + i as u64)).collect();

            let (mut left, mut right) = (Vec::new(), Vec::new());
            for c in &calls {
                if xorshift(&mut rng) % 2 == 0 {
                    left.push(c.clone());
                } else {
                    right.push(c.clone());
                }
            }

            let oracle = aggregate(calls);
            let mut merged = fold(left);
            merged.absorb(fold(right));
            let incremental = emit(merged);

            assert_eq!(
                encode(&incremental),
                encode(&oracle),
                "case {case}: merged partition must emit identical bytes"
            );
        }
    }

    #[test]
    fn absorb_is_associative_across_three_way_partition() {
        let mut rng: u64 = 0xDEAD_BEEF_0BAD_F00D;
        for case in 0..150 {
            let n = (xorshift(&mut rng) % 45) as usize;
            let calls: Vec<ProviderCall> =
                (0..n).map(|i| random_call(&mut rng, 5_000 + i as u64)).collect();

            let mut parts: [Vec<ProviderCall>; 3] = [Vec::new(), Vec::new(), Vec::new()];
            for c in &calls {
                parts[(xorshift(&mut rng) % 3) as usize].push(c.clone());
            }
            let [a, b, c] = parts;

            let oracle = encode(&aggregate(calls));

            // (A ⊕ B) ⊕ C
            let mut left = fold(a.clone());
            left.absorb(fold(b.clone()));
            left.absorb(fold(c.clone()));
            // A ⊕ (B ⊕ C)
            let mut right_inner = fold(b);
            right_inner.absorb(fold(c));
            let mut right = fold(a);
            right.absorb(right_inner);

            assert_eq!(encode(&emit(left)), oracle, "case {case}: left-assoc");
            assert_eq!(encode(&emit(right)), oracle, "case {case}: right-assoc");
        }
    }

    #[test]
    fn absorb_empty_is_identity() {
        let mut rng: u64 = 0x1234_5678_9ABC_DEF0;
        let calls: Vec<ProviderCall> = (0..25).map(|i| random_call(&mut rng, 9_000 + i)).collect();
        let oracle = encode(&aggregate(calls.clone()));

        let mut left = fold(calls.clone());
        left.absorb(fold(Vec::new()));
        assert_eq!(encode(&emit(left)), oracle, "X ⊕ ∅ == X");

        let mut right = fold(Vec::new());
        right.absorb(fold(calls));
        assert_eq!(encode(&emit(right)), oracle, "∅ ⊕ X == X");

        let mut both = fold(Vec::new());
        both.absorb(fold(Vec::new()));
        assert_eq!(
            encode(&emit(both)),
            encode(&UsageData::default()),
            "∅ ⊕ ∅ == default"
        );
    }

    #[test]
    fn aggregate_empty_still_returns_default_usage_data() {
        assert_eq!(aggregate(Vec::new()), UsageData::default());
    }

    // ── incremental scan integration (L1 counters + L2 oracle) ──────
    //
    // Every scenario asserts the incremental snapshot is byte-identical
    // to a cache-less full scan (`scan`) of the SAME tree state — the
    // legacy path is the oracle — plus the counter facts that prove
    // what the scan actually did.

    use tempfile::TempDir;

    fn claude_line(ts: &str, session: &str, input: u64, output: u64) -> String {
        format!(
            r#"{{"type":"assistant","timestamp":"{ts}","sessionId":"{session}","cwd":"/tmp/proj","gitBranch":"main","message":{{"model":"claude-3-5-sonnet","content":[{{"type":"text","text":"hi"}},{{"type":"tool_use","name":"Read"}}],"usage":{{"input_tokens":{input},"output_tokens":{output},"cache_read_input_tokens":7}}}}}}"#
        )
    }

    struct IncrFixture {
        _tmp: TempDir,
        claude_root: PathBuf,
        roots: ProviderRoots,
        cache_dir: TempDir,
    }

    impl IncrFixture {
        fn new() -> Self {
            let tmp = TempDir::new().expect("tempdir");
            let claude_root = tmp.path().join("claude/projects");
            std::fs::create_dir_all(&claude_root).expect("mkdir");
            let roots = ProviderRoots {
                claude_projects: Some(claude_root.clone()),
                ..ProviderRoots::default()
            };
            let cache_dir = TempDir::new().expect("cache dir");
            Self {
                _tmp: tmp,
                claude_root,
                roots,
                cache_dir,
            }
        }

        fn write_file(&self, project: &str, name: &str, lines: &[String]) {
            let dir = self.claude_root.join(project);
            std::fs::create_dir_all(&dir).expect("mkdir project");
            std::fs::write(dir.join(name), lines.join("\n")).expect("write jsonl");
        }

        fn open_cache(&self) -> Option<crate::cache::UsageCache> {
            Some(
                crate::cache::UsageCache::open(&self.cache_dir.path().join("usage.sqlite"))
                    .expect("open cache"),
            )
        }

        fn oracle_bytes(&self) -> Vec<u8> {
            encode(&scan(&self.roots))
        }
    }

    fn now_nanos() -> u64 {
        u64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos(),
        )
        .unwrap_or(u64::MAX)
    }

    /// Sleep long enough for the filesystem mtime clock to tick past
    /// `now` so a watermark captured between writes cleanly splits them.
    fn tick() {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    #[test]
    fn incremental_cold_rebuild_matches_full_scan_oracle() {
        let fx = IncrFixture::new();
        fx.write_file(
            "proj-a",
            "old.jsonl",
            &[claude_line("2026-05-01T10:00:00Z", "s1", 100, 200)],
        );
        tick();
        let watermark = now_nanos();
        tick();
        fx.write_file(
            "proj-b",
            "new.jsonl",
            &[claude_line("2026-06-01T10:00:00Z", "s2", 10, 20)],
        );

        let mut cache = fx.open_cache();
        let mut reporter = ProgressReporter::noop();
        let out = scan_incremental(&fx.roots, &mut cache, None, watermark, None, &mut reporter);

        assert!(out.stable_rebuilt, "no stored stable => rebuild");
        assert!(!out.counters.stable_reused);
        assert_eq!(out.stable.folded.len(), 1, "old.jsonl folded");
        assert_eq!(
            encode(out.data.as_ref().expect("changed scan publishes")),
            fx.oracle_bytes(),
            "matches full-scan oracle"
        );
    }

    #[test]
    fn incremental_no_change_refresh_parses_zero_and_reuses_stable() {
        let fx = IncrFixture::new();
        fx.write_file(
            "proj-a",
            "old.jsonl",
            &[claude_line("2026-05-01T10:00:00Z", "s1", 100, 200)],
        );
        tick();
        let watermark = now_nanos();
        tick();
        fx.write_file(
            "proj-b",
            "new.jsonl",
            &[claude_line("2026-06-01T10:00:00Z", "s2", 10, 20)],
        );

        let mut cache = fx.open_cache();
        let mut reporter = ProgressReporter::noop();
        let first = scan_incremental(&fx.roots, &mut cache, None, watermark, None, &mut reporter);

        // Second refresh, nothing changed on disk: the CPU-fix gate.
        let second = scan_incremental(
            &fx.roots,
            &mut cache,
            Some(first.stable),
            watermark,
            None,
            &mut reporter,
        );

        assert!(
            second.counters.stable_reused,
            "stable set unchanged => reuse"
        );
        assert!(!second.stable_rebuilt);
        assert_eq!(
            second.counters.parsed, 0,
            "no-change refresh parses ZERO files"
        );
        assert_eq!(
            second.counters.stable_skipped, 1,
            "old file skipped entirely"
        );
        assert_eq!(
            second.counters.cache_hits, 1,
            "recent file served from cache"
        );
        assert_eq!(
            encode(second.data.as_ref().expect("changed scan publishes")),
            fx.oracle_bytes(),
            "matches full-scan oracle"
        );
    }

    // ── unchanged-snapshot short-circuit (issue #255) ───────────────

    #[test]
    fn unchanged_refresh_short_circuits_with_memo() {
        let fx = IncrFixture::new();
        fx.write_file(
            "proj-a",
            "old.jsonl",
            &[claude_line("2026-05-01T10:00:00Z", "s1", 100, 200)],
        );
        tick();
        let watermark = now_nanos();
        tick();
        fx.write_file(
            "proj-b",
            "new.jsonl",
            &[claude_line("2026-06-01T10:00:00Z", "s2", 10, 20)],
        );

        let mut cache = fx.open_cache();
        let mut reporter = ProgressReporter::noop();
        let first = scan_incremental(&fx.roots, &mut cache, None, watermark, None, &mut reporter);
        assert!(first.data.is_some(), "first refresh always publishes");
        assert_eq!(
            first.memo.recent_present.len(),
            1,
            "one recent file fingerprinted"
        );

        let second = scan_incremental(
            &fx.roots,
            &mut cache,
            Some(first.stable),
            watermark,
            Some(&first.memo),
            &mut reporter,
        );
        assert!(
            second.data.is_none(),
            "unchanged refresh skips aggregation entirely"
        );
        assert!(second.counters.stable_reused);
        assert!(!second.stable_rebuilt);
        assert_eq!(second.counters.parsed, 0);
        assert_eq!(second.memo, first.memo, "memo carries forward unchanged");

        // The short-circuit re-arms from its own returned memo.
        let third = scan_incremental(
            &fx.roots,
            &mut cache,
            Some(second.stable),
            watermark,
            Some(&second.memo),
            &mut reporter,
        );
        assert!(third.data.is_none(), "still unchanged on the third refresh");
    }

    #[test]
    fn short_circuit_disarms_when_recent_file_changes() {
        let fx = IncrFixture::new();
        tick();
        let watermark = now_nanos();
        tick();
        fx.write_file(
            "proj-b",
            "new.jsonl",
            &[claude_line("2026-06-01T10:00:00Z", "s2", 10, 20)],
        );

        let mut cache = fx.open_cache();
        let mut reporter = ProgressReporter::noop();
        let first = scan_incremental(&fx.roots, &mut cache, None, watermark, None, &mut reporter);

        tick();
        fx.write_file(
            "proj-b",
            "new.jsonl",
            &[
                claude_line("2026-06-01T10:00:00Z", "s2", 10, 20),
                claude_line("2026-06-01T11:00:00Z", "s2", 1, 2),
            ],
        );

        let second = scan_incremental(
            &fx.roots,
            &mut cache,
            Some(first.stable),
            watermark,
            Some(&first.memo),
            &mut reporter,
        );
        let data = second.data.as_ref().expect("changed file must re-publish");
        assert_eq!(data.grand_total.call_count, 2);
        assert_eq!(encode(data), fx.oracle_bytes(), "matches full-scan oracle");
        assert_ne!(second.memo, first.memo, "memo reflects the new fingerprint");
    }

    #[test]
    fn short_circuit_disarms_when_recent_file_deleted() {
        // Deletion is the trap a naive `parsed == 0` check would miss:
        // nothing parses, the stable set is untouched, but the snapshot
        // must shrink — the recent fingerprint set is what catches it.
        let fx = IncrFixture::new();
        tick();
        let watermark = now_nanos();
        tick();
        fx.write_file(
            "proj-b",
            "keep.jsonl",
            &[claude_line("2026-06-01T10:00:00Z", "s2", 10, 20)],
        );
        fx.write_file(
            "proj-b",
            "gone.jsonl",
            &[claude_line("2026-06-02T10:00:00Z", "s3", 30, 40)],
        );

        let mut cache = fx.open_cache();
        let mut reporter = ProgressReporter::noop();
        let first = scan_incremental(&fx.roots, &mut cache, None, watermark, None, &mut reporter);
        assert_eq!(
            first.data.as_ref().expect("publishes").grand_total.call_count,
            2
        );

        std::fs::remove_file(fx.claude_root.join("proj-b/gone.jsonl")).expect("delete");

        let second = scan_incremental(
            &fx.roots,
            &mut cache,
            Some(first.stable),
            watermark,
            Some(&first.memo),
            &mut reporter,
        );
        assert_eq!(second.counters.parsed, 0, "nothing re-parses on a delete");
        let data = second.data.as_ref().expect("deletion must re-publish");
        assert_eq!(
            data.grand_total.call_count, 1,
            "deleted file's call is gone"
        );
        assert_eq!(encode(data), fx.oracle_bytes(), "matches full-scan oracle");
    }

    #[test]
    fn short_circuit_disarms_when_stable_file_touched() {
        let fx = IncrFixture::new();
        fx.write_file(
            "proj-a",
            "old.jsonl",
            &[claude_line("2026-05-01T10:00:00Z", "s1", 100, 200)],
        );
        tick();
        let watermark = now_nanos();
        tick();
        fx.write_file(
            "proj-b",
            "new.jsonl",
            &[claude_line("2026-06-01T10:00:00Z", "s2", 10, 20)],
        );

        let mut cache = fx.open_cache();
        let mut reporter = ProgressReporter::noop();
        let first = scan_incremental(&fx.roots, &mut cache, None, watermark, None, &mut reporter);

        // Rewrite the stable file: its mtime moves it to the recent
        // side AND breaks the stable fingerprint set.
        tick();
        fx.write_file(
            "proj-a",
            "old.jsonl",
            &[
                claude_line("2026-05-01T10:00:00Z", "s1", 100, 200),
                claude_line("2026-05-01T11:00:00Z", "s1", 5, 6),
            ],
        );

        let second = scan_incremental(
            &fx.roots,
            &mut cache,
            Some(first.stable),
            watermark,
            Some(&first.memo),
            &mut reporter,
        );
        assert!(
            second.stable_rebuilt,
            "touched stable file forces a rebuild"
        );
        let data = second.data.as_ref().expect("stable change must re-publish");
        assert_eq!(data.grand_total.call_count, 3);
        assert_eq!(encode(data), fx.oracle_bytes(), "matches full-scan oracle");
    }

    #[test]
    fn incremental_new_recent_file_parses_only_it() {
        let fx = IncrFixture::new();
        fx.write_file(
            "proj-a",
            "old.jsonl",
            &[claude_line("2026-05-01T10:00:00Z", "s1", 100, 200)],
        );
        tick();
        let watermark = now_nanos();
        tick();
        fx.write_file(
            "proj-b",
            "new.jsonl",
            &[claude_line("2026-06-01T10:00:00Z", "s2", 10, 20)],
        );

        let mut cache = fx.open_cache();
        let mut reporter = ProgressReporter::noop();
        let first = scan_incremental(&fx.roots, &mut cache, None, watermark, None, &mut reporter);

        fx.write_file(
            "proj-b",
            "new2.jsonl",
            &[claude_line("2026-06-02T11:00:00Z", "s3", 5, 5)],
        );
        let second = scan_incremental(
            &fx.roots,
            &mut cache,
            Some(first.stable),
            watermark,
            None,
            &mut reporter,
        );

        assert!(second.counters.stable_reused, "stable side untouched");
        assert_eq!(second.counters.parsed, 1, "only the new file parses");
        assert_eq!(
            encode(second.data.as_ref().expect("changed scan publishes")),
            fx.oracle_bytes(),
            "matches full-scan oracle"
        );
    }

    #[test]
    fn incremental_aged_out_file_rebuilds_without_double_count() {
        let fx = IncrFixture::new();
        fx.write_file(
            "proj-a",
            "a.jsonl",
            &[claude_line("2026-05-01T10:00:00Z", "s1", 100, 200)],
        );
        tick();
        let wm1 = now_nanos();
        tick();
        fx.write_file(
            "proj-b",
            "b.jsonl",
            &[claude_line("2026-06-01T10:00:00Z", "s2", 10, 20)],
        );

        let mut cache = fx.open_cache();
        let mut reporter = ProgressReporter::noop();
        let first = scan_incremental(&fx.roots, &mut cache, None, wm1, None, &mut reporter);
        assert_eq!(first.stable.folded.len(), 1);

        // The watermark advances past b.jsonl — it ages into the
        // stable set, breaking fingerprint equality.
        let wm2 = now_nanos();
        let second = scan_incremental(
            &fx.roots,
            &mut cache,
            Some(first.stable),
            wm2,
            None,
            &mut reporter,
        );

        assert!(second.stable_rebuilt, "aged-in file forces rebuild");
        assert_eq!(second.stable.folded.len(), 2, "both files folded now");
        assert_eq!(
            second.counters.parsed, 0,
            "rebuild is cache-served, no reparse"
        );
        assert_eq!(
            encode(second.data.as_ref().expect("changed scan publishes")),
            fx.oracle_bytes(),
            "matches full-scan oracle"
        );
    }

    #[test]
    fn incremental_deleted_old_file_drops_its_contribution() {
        let fx = IncrFixture::new();
        fx.write_file(
            "proj-a",
            "doomed.jsonl",
            &[claude_line("2026-05-01T10:00:00Z", "s1", 100, 200)],
        );
        fx.write_file(
            "proj-a",
            "keeper.jsonl",
            &[claude_line("2026-05-02T10:00:00Z", "s9", 1, 1)],
        );
        tick();
        let watermark = now_nanos();

        let mut cache = fx.open_cache();
        let mut reporter = ProgressReporter::noop();
        let first = scan_incremental(&fx.roots, &mut cache, None, watermark, None, &mut reporter);
        assert_eq!(first.stable.folded.len(), 2);

        std::fs::remove_file(fx.claude_root.join("proj-a/doomed.jsonl")).expect("rm");
        let second = scan_incremental(
            &fx.roots,
            &mut cache,
            Some(first.stable),
            watermark,
            None,
            &mut reporter,
        );

        assert!(
            second.stable_rebuilt,
            "deletion breaks fingerprint equality"
        );
        assert_eq!(
            second.stable.folded.len(),
            1,
            "only the keeper remains folded"
        );
        assert_eq!(
            encode(second.data.as_ref().expect("changed scan publishes")),
            fx.oracle_bytes(),
            "matches post-delete oracle"
        );
    }

    #[test]
    fn incremental_appended_old_file_moves_to_recent_without_double_count() {
        let fx = IncrFixture::new();
        fx.write_file(
            "proj-a",
            "grow.jsonl",
            &[claude_line("2026-05-01T10:00:00Z", "s1", 100, 200)],
        );
        tick();
        let watermark = now_nanos();

        let mut cache = fx.open_cache();
        let mut reporter = ProgressReporter::noop();
        let first = scan_incremental(&fx.roots, &mut cache, None, watermark, None, &mut reporter);
        assert_eq!(first.stable.folded.len(), 1, "grow.jsonl folded as stable");

        // Append a line: mtime bumps past the watermark, so the file
        // flips to the recent side AND vanishes from the stable set —
        // its old contribution must be rebuilt out, not double-counted.
        let path = fx.claude_root.join("proj-a/grow.jsonl");
        let mut content = std::fs::read_to_string(&path).expect("read");
        content.push('\n');
        content.push_str(&claude_line("2026-05-01T10:05:00Z", "s1", 11, 22));
        std::fs::write(&path, content).expect("append");

        let second = scan_incremental(
            &fx.roots,
            &mut cache,
            Some(first.stable),
            watermark,
            None,
            &mut reporter,
        );

        assert!(second.stable_rebuilt, "stable set lost the appended file");
        assert!(second.stable.folded.is_empty(), "nothing stable remains");
        assert_eq!(
            second.data.as_ref().expect("changed scan publishes").grand_total.call_count,
            2,
            "old + appended call, counted once each"
        );
        assert_eq!(
            encode(second.data.as_ref().expect("changed scan publishes")),
            fx.oracle_bytes(),
            "matches post-append oracle"
        );
    }

    #[test]
    fn incremental_without_stored_stable_on_empty_tree_is_default() {
        let fx = IncrFixture::new();
        let mut cache = fx.open_cache();
        let mut reporter = ProgressReporter::noop();
        let out = scan_incremental(
            &fx.roots,
            &mut cache,
            None,
            now_nanos(),
            None,
            &mut reporter,
        );
        assert_eq!(
            encode(out.data.as_ref().expect("changed scan publishes")),
            encode(&UsageData::default())
        );
        assert!(out.stable.folded.is_empty());
    }

    #[test]
    fn agg_state_roundtrips_through_bincode() {
        // P3 persists AggState as the stable aggregate blob — prove the
        // serde derives round-trip through the same codec the cache uses.
        let mut rng: u64 = 0xFEED_FACE_CAFE_BEEF;
        let calls: Vec<ProviderCall> = (0..30).map(|i| random_call(&mut rng, 7_000 + i)).collect();
        let state = fold(calls);
        let bytes = bincode::serialize(&state).expect("serialize AggState");
        let back: AggState = bincode::deserialize(&bytes).expect("deserialize AggState");
        assert_eq!(
            encode(&emit(back)),
            encode(&emit(state)),
            "round-tripped state emits identical bytes"
        );
    }
}
