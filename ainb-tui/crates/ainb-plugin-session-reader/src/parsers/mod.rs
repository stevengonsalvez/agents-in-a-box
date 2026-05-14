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

/// Cache-aware read-and-parse for one provider session file.
///
/// 1. `stat(path)` → `(mtime, size)`.
/// 2. If a cache is supplied and `lookup(path, mtime, size)` hits, return
///    the cached `Vec<ProviderCall>` with no filesystem read.
/// 3. Otherwise read the file, call `parse(&content)`, hash the bytes
///    with FNV-1a 64, and persist `(mtime, size, fingerprint, parsed)`
///    via `cache.insert`.
///
/// Any cache I/O / SQL failure is logged at `warn` and the call falls
/// through to a fresh parse — the cache is never allowed to break the
/// scan.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn read_file_cached<F>(
    path: &std::path::Path,
    cache: &mut Option<crate::cache::UsageCache>,
    parse: F,
) -> Vec<ainb_plugin_types_sessions::ProviderCall>
where
    F: FnOnce(&str) -> Vec<ainb_plugin_types_sessions::ProviderCall>,
{
    let path_str = path.to_string_lossy().into_owned();
    let stat = stat_for_cache(path);

    if let (Some(cache_ref), Some((mtime, size))) = (cache.as_ref(), stat) {
        match cache_ref.lookup(&path_str, mtime, size) {
            Ok(Some(hit)) => return hit,
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

    if let (Some(cache_ref), Some((mtime, size))) = (cache.as_mut(), stat) {
        let fingerprint = crate::cache::fingerprint(content.as_bytes());
        if let Err(err) = cache_ref.insert(&path_str, mtime, size, fingerprint, &calls) {
            tracing::warn!(
                path = %path.display(),
                error = %err,
                "session-reader cache: insert failed"
            );
        }
    }

    calls
}
