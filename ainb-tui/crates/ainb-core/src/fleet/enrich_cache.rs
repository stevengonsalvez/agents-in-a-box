// ABOUTME: Content-addressed enrich cache — blake3(ctx) → drafted suggestion.
//
// Rust READS this cache (attaching fresh suggestions to `ainb fleet needs`
// cards) and the enrich PRODUCER (the workflow batched agent, or the calling
// session for small fleets) WRITES it via `ainb fleet enrich-cache put`. The
// key is a blake3 hash of the exact serialized card context, emitted by the
// Rust reader as `enrich_key` so reader and producer never disagree. An entry
// therefore self-invalidates the instant the session advances. Growth is
// bounded by an LRU cap; writes are atomic (temp file + rename).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Maximum number of cached suggestions kept on disk. Churned sessions would
/// otherwise grow the file without bound; the oldest-written entries are
/// evicted first.
const CACHE_CAP: usize = 200;

/// Override the cache file location (used by tests).
const CACHE_ENV: &str = "AINB_FLEET_ENRICH_CACHE";

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Entry {
    suggestion: String,
    #[serde(default)]
    last_used_ms: i64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Cache {
    #[serde(default)]
    entries: HashMap<String, Entry>,
}

/// Stable content key for a card's context. The same `ctx` string always
/// produces the same key; any change produces a different one.
#[must_use]
pub fn ctx_key(ctx: &str) -> String {
    blake3::hash(ctx.as_bytes()).to_hex().to_string()
}

fn cache_path() -> Option<PathBuf> {
    if let Ok(p) = std::env::var(CACHE_ENV) {
        return Some(PathBuf::from(p));
    }
    let mut home = dirs::home_dir()?;
    home.push(".agents-in-a-box");
    home.push("fleet");
    home.push("enrich-cache.json");
    Some(home)
}

fn load(path: &Path) -> Cache {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Look up a fresh cached suggestion by its context key. Read-only — never
/// mutates the cache, so attaching cached suggestions during a `needs` read
/// stays a pure read.
#[must_use]
pub fn lookup(key: &str) -> Option<String> {
    lookup_at(&cache_path()?, key)
}

/// Insert or refresh an entry, evict down to the LRU cap, and write atomically.
/// No-op (Ok) when there is no resolvable home directory.
pub fn put(key: &str, suggestion: &str) -> anyhow::Result<()> {
    let Some(path) = cache_path() else {
        return Ok(());
    };
    put_at(&path, key, suggestion)
}

fn lookup_at(path: &Path, key: &str) -> Option<String> {
    load(path).entries.get(key).map(|e| e.suggestion.clone())
}

fn put_at(path: &Path, key: &str, suggestion: &str) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut cache = load(path);
    cache.entries.insert(
        key.to_string(),
        Entry {
            suggestion: suggestion.to_string(),
            last_used_ms: now_ms(),
        },
    );
    evict_to_cap(&mut cache, CACHE_CAP);
    atomic_write(path, &cache)
}

/// Drop oldest-written entries until at most `cap` remain.
fn evict_to_cap(cache: &mut Cache, cap: usize) {
    if cache.entries.len() <= cap {
        return;
    }
    let mut by_age: Vec<(String, i64)> =
        cache.entries.iter().map(|(k, e)| (k.clone(), e.last_used_ms)).collect();
    by_age.sort_by_key(|(k, ts)| (*ts, k.clone()));
    let drop_n = cache.entries.len() - cap;
    for (k, _) in by_age.into_iter().take(drop_n) {
        cache.entries.remove(&k);
    }
}

fn atomic_write(path: &Path, cache: &Cache) -> anyhow::Result<()> {
    let json = serde_json::to_string(cache)?;
    // Per-process temp name so concurrent writers don't clobber each other's
    // temp file; the rename itself is atomic on the same filesystem.
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    fn temp_cache() -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        std::env::temp_dir().join(format!("ainb-enrich-cache-{}-{n}.json", std::process::id()))
    }

    #[test]
    fn ctx_key_is_stable_and_sensitive() {
        assert_eq!(ctx_key("hello"), ctx_key("hello"));
        assert_ne!(ctx_key("hello"), ctx_key("hello "));
    }

    #[test]
    fn put_then_lookup_roundtrips() {
        let path = temp_cache();
        let key = ctx_key("{\"kind\":\"ASK\"}");
        assert!(lookup_at(&path, &key).is_none());
        put_at(&path, &key, "pick option 1").unwrap();
        assert_eq!(lookup_at(&path, &key).as_deref(), Some("pick option 1"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn advancing_ctx_misses_cache() {
        let path = temp_cache();
        put_at(&path, &ctx_key("v1"), "old").unwrap();
        // A changed context hashes to a different key → miss.
        assert!(lookup_at(&path, &ctx_key("v2")).is_none());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn evicts_oldest_beyond_cap() {
        let mut cache = Cache::default();
        for i in 0..(CACHE_CAP + 5) {
            cache.entries.insert(
                format!("k{i}"),
                Entry {
                    suggestion: format!("s{i}"),
                    last_used_ms: i as i64,
                },
            );
        }
        evict_to_cap(&mut cache, CACHE_CAP);
        assert_eq!(cache.entries.len(), CACHE_CAP);
        // The five oldest (k0..k4) are gone; the newest survive.
        assert!(!cache.entries.contains_key("k0"));
        assert!(!cache.entries.contains_key("k4"));
        assert!(cache.entries.contains_key("k5"));
    }
}
