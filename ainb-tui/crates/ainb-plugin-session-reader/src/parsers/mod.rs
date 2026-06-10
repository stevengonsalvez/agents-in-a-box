//! Per-provider JSONL parsers.
//!
//! Each provider module exposes a `parse_dir(root: &Path) -> Vec<ProviderCall>`
//! entry point. The implementations are best-effort: a single broken
//! file logs a warning and gets skipped, but the rest of the directory
//! keeps parsing. A directory that doesn't exist or is unreadable
//! returns an empty vec without error — that's how Gemini / Copilot
//! stubs degrade cleanly when the user doesn't use those providers.

pub mod claude;
pub mod codex;
pub mod copilot;
pub mod cost;
pub mod cursor;
pub mod gemini;

pub(crate) use cost::estimate_cost_usd;

/// Common helper: parse an RFC 3339 timestamp into UTC.
pub(crate) fn parse_timestamp(timestamp: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(timestamp)
        .map(|dt| dt.with_timezone(&chrono::Utc))
        .ok()
}

/// `(mtime_nanos, size_bytes)` for `path`. Returns `None` when stat
/// fails (file vanished, permission denied) — caller must fall back
/// to a normal read.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn stat_for_cache(path: &std::path::Path) -> Option<(u64, u64)> {
    use std::time::UNIX_EPOCH;
    let meta = std::fs::metadata(path).ok()?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX))?;
    Some((mtime, meta.len()))
}

/// Cache- and watermark-aware read-and-parse for one provider session
/// file, routed through the scan context.
///
/// 1. `stat(path)` → `(mtime, size)`.
/// 2. When the context has a watermark and `mtime` predates it, the
///    file is *stable*: its fingerprint is recorded and — under
///    `StablePolicy::Skip` — the file is neither read nor looked up
///    (its calls already ride the persisted stable aggregate). Under
///    `Collect` it takes the cached-read path and its calls route to
///    `ctx.stable_calls` instead of the return value.
/// 3. Recent files (or any file in a full scan): if the cache hits on
///    `(path, mtime, size)`, return the cached calls with no
///    filesystem read; otherwise read, `parse`, and persist.
///
/// Any cache I/O / SQL failure is logged at `warn` and the call falls
/// through to a fresh parse — the cache is never allowed to break the
/// scan.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn read_file_cached<F>(
    path: &std::path::Path,
    ctx: &mut crate::scanner::ScanCtx<'_>,
    parse: F,
) -> Vec<ainb_plugin_types_sessions::ProviderCall>
where
    F: FnOnce(&str) -> Vec<ainb_plugin_types_sessions::ProviderCall>,
{
    use crate::scanner::StablePolicy;

    let path_str = path.to_string_lossy().into_owned();
    let stat = stat_for_cache(path);
    ctx.counters.files_statted = ctx.counters.files_statted.saturating_add(1);

    if let (Some(watermark), Some((mtime, size))) = (ctx.watermark_nanos, stat) {
        if mtime < watermark {
            ctx.stable_present.push((path_str.clone(), mtime, size));
            match ctx.stable_policy {
                StablePolicy::Skip => {
                    ctx.counters.stable_skipped = ctx.counters.stable_skipped.saturating_add(1);
                    return Vec::new();
                }
                StablePolicy::Collect => {
                    let calls = read_one_cached(path, &path_str, stat, ctx, parse);
                    ctx.stable_calls.extend(calls);
                    return Vec::new();
                }
            }
        }
    }

    read_one_cached(path, &path_str, stat, ctx, parse)
}

/// The cached read-parse-insert core shared by the recent and
/// stable-collect routes.
#[cfg(not(target_arch = "wasm32"))]
fn read_one_cached<F>(
    path: &std::path::Path,
    path_str: &str,
    stat: Option<(u64, u64)>,
    ctx: &mut crate::scanner::ScanCtx<'_>,
    parse: F,
) -> Vec<ainb_plugin_types_sessions::ProviderCall>
where
    F: FnOnce(&str) -> Vec<ainb_plugin_types_sessions::ProviderCall>,
{
    if let (Some(cache_ref), Some((mtime, size))) = (ctx.cache.as_ref(), stat) {
        match cache_ref.lookup(path_str, mtime, size) {
            Ok(Some(hit)) => {
                ctx.counters.cache_hits = ctx.counters.cache_hits.saturating_add(1);
                return hit;
            }
            Ok(None) => {}
            Err(err) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %err,
                    "session-reader cache: lookup failed; re-parsing"
                );
            }
        }
    }

    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(err) => {
            // Identical degrade-to-empty contract as the un-cached path.
            tracing::warn!(
                path = %path.display(),
                error = %err,
                "session-reader: skip unreadable file"
            );
            return Vec::new();
        }
    };

    let calls = parse(&content);
    ctx.counters.parsed = ctx.counters.parsed.saturating_add(1);
    // Pace the expensive op: a sub-millisecond breather per parsed
    // file keeps a cold/hard scan from pegging a core flat-out while
    // costing a warm refresh (0–2 parses) nothing. The crate forbids
    // unsafe code, so macOS thread-QoS FFI isn't available — this
    // duty-cycle yield is the portable softening.
    std::thread::sleep(std::time::Duration::from_micros(500));

    if let (Some(cache_ref), Some((mtime, size))) = (ctx.cache.as_mut(), stat) {
        let fingerprint = crate::cache::fingerprint(content.as_bytes());
        if let Err(err) = cache_ref.insert(path_str, mtime, size, fingerprint, &calls) {
            tracing::warn!(
                path = %path.display(),
                error = %err,
                "session-reader cache: insert failed"
            );
        }
    }

    calls
}
