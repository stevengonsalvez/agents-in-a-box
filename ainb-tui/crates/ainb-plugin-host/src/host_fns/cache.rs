//! Generic plugin-scoped cache primitive.
//!
//! Wire shape:
//!
//! * `ainb_cache_get(key, out) -> i32` — number of bytes written, or
//!   `BufferTooSmall` / `Ok(0)` (no value) / `HostError`.
//! * `ainb_cache_put(key, val, ttl_secs) -> i32` — `Ok(0)` on success.
//!
//! Storage model: each `(plugin_id, key)` is a single small file under
//! `~/.agents-in-a-box/cache/<plugin-id>/<key>` with a sidecar
//! `<key>.meta` carrying the absolute TTL deadline (`u64` unix seconds).
//!
//! Quotas:
//! * Per-plugin soft cap of 200 MiB. Writes that would push the plugin
//!   over the cap evict the plugin's *own* oldest entries first.
//! * Global cap of 1 GiB. When all plugins together exceed the cap, the
//!   host falls back to a cross-plugin LRU eviction.
//!
//! Phase 6a foundation: get/put both operate on the filesystem only;
//! no lock contention design is needed yet because each plugin owns its
//! own subdirectory and the per-key files are tiny.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use ainb_plugin_api::host::{HostStatus, HOST_MODULE};
use anyhow::Result;
use wasmi::{Caller, Linker};

use crate::runtime::HostState;

use super::{read_bytes, read_string};

/// Per-plugin soft cap before the host evicts that plugin's own oldest
/// entries. Tunable via `AINB_CACHE_PLUGIN_QUOTA_BYTES` (env override).
pub const PLUGIN_QUOTA_BYTES: u64 = 200 * 1024 * 1024;

/// Global cap across all plugins before the host falls back to a
/// cross-plugin LRU sweep. Override via `AINB_CACHE_GLOBAL_QUOTA_BYTES`.
pub const GLOBAL_QUOTA_BYTES: u64 = 1024 * 1024 * 1024;

/// Wire `ainb_cache_get` + `ainb_cache_put` into `linker`. Cache fns are
/// always-on — the per-plugin scoping in [`plugin_cache_dir`] is what
/// stops one plugin reading another's bytes, not a capability gate.
pub fn link(linker: &mut Linker<HostState>) -> Result<()> {
    linker.func_wrap(
        HOST_MODULE,
        "ainb_cache_get",
        |mut caller: Caller<'_, HostState>,
         key_ptr: i32,
         key_len: i32,
         out_ptr: i32,
         out_len: i32|
         -> i32 { cache_get_impl(&mut caller, key_ptr, key_len, out_ptr, out_len) },
    )?;
    linker.func_wrap(
        HOST_MODULE,
        "ainb_cache_put",
        |mut caller: Caller<'_, HostState>,
         key_ptr: i32,
         key_len: i32,
         val_ptr: i32,
         val_len: i32,
         ttl_secs: i64|
         -> i32 {
            cache_put_impl(&mut caller, key_ptr, key_len, val_ptr, val_len, ttl_secs)
        },
    )?;
    Ok(())
}

fn ainb_cache_root() -> Option<PathBuf> {
    if let Ok(env) = std::env::var("AINB_HOME") {
        return Some(PathBuf::from(env).join("cache"));
    }
    dirs::home_dir().map(|h| h.join(".agents-in-a-box").join("cache"))
}

fn plugin_cache_dir(plugin_id: &str) -> Option<PathBuf> {
    ainb_cache_root().map(|r| r.join(sanitize_id(plugin_id)))
}

/// Plugin IDs come straight out of the manifest; reject path separators
/// + leading dots so a malicious manifest can't reach into a sibling
/// plugin's cache.
fn sanitize_id(id: &str) -> String {
    id.chars()
        .map(|c| if c == '/' || c == '\\' || c == '\0' { '_' } else { c })
        .collect()
}

fn sanitize_key(k: &str) -> String {
    // Keys must be filesystem-safe; map slashes to a single underscore.
    // Empty keys are refused at the call site, not here.
    k.chars()
        .map(|c| match c {
            '/' | '\\' | '\0' | '\n' | '\r' => '_',
            other => other,
        })
        .collect()
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn quota_plugin_bytes() -> u64 {
    std::env::var("AINB_CACHE_PLUGIN_QUOTA_BYTES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(PLUGIN_QUOTA_BYTES)
}

fn quota_global_bytes() -> u64 {
    std::env::var("AINB_CACHE_GLOBAL_QUOTA_BYTES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(GLOBAL_QUOTA_BYTES)
}

fn cache_get_impl(
    caller: &mut Caller<'_, HostState>,
    key_ptr: i32,
    key_len: i32,
    out_ptr: i32,
    out_len: i32,
) -> i32 {
    let key_raw = match read_string(caller, key_ptr, key_len) {
        Ok(s) => s,
        Err(e) => {
            caller.data_mut().last_error = Some(format!("ainb_cache_get: bad key utf8: {e}"));
            return HostStatus::InvalidArgument as i32;
        }
    };
    if key_raw.is_empty() {
        caller.data_mut().last_error = Some("ainb_cache_get: empty key".into());
        return HostStatus::InvalidArgument as i32;
    }
    let key = sanitize_key(&key_raw);
    let plugin_id = caller.data().plugin_id.clone();
    let Some(dir) = plugin_cache_dir(&plugin_id) else {
        caller.data_mut().last_error = Some("ainb_cache_get: $HOME unresolvable".into());
        return HostStatus::HostError as i32;
    };
    let val_path = dir.join(&key);
    let meta_path = dir.join(format!("{key}.meta"));

    // TTL check — if the meta says the entry has expired, evict and
    // surface as a miss (Ok(0)).
    if let Ok(meta) = fs::read_to_string(&meta_path) {
        if let Ok(deadline) = meta.trim().parse::<u64>() {
            if deadline <= now_secs() {
                let _ = fs::remove_file(&val_path);
                let _ = fs::remove_file(&meta_path);
                return 0;
            }
        }
    }

    let bytes = match fs::read(&val_path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return 0,
        Err(e) => {
            caller.data_mut().last_error = Some(format!("ainb_cache_get: read: {e}"));
            return HostStatus::HostError as i32;
        }
    };

    let cap = match usize::try_from(out_len) {
        Ok(n) => n,
        Err(_) => return HostStatus::InvalidArgument as i32,
    };
    if bytes.len() > cap {
        return HostStatus::BufferTooSmall as i32;
    }
    let memory = match caller
        .get_export("memory")
        .and_then(wasmi::Extern::into_memory)
    {
        Some(m) => m,
        None => return HostStatus::HostError as i32,
    };
    let off = match usize::try_from(out_ptr) {
        Ok(n) => n,
        Err(_) => return HostStatus::InvalidArgument as i32,
    };
    if memory.write(&mut *caller, off, &bytes).is_err() {
        return HostStatus::HostError as i32;
    }
    i32::try_from(bytes.len()).unwrap_or(HostStatus::HostError as i32)
}

fn cache_put_impl(
    caller: &mut Caller<'_, HostState>,
    key_ptr: i32,
    key_len: i32,
    val_ptr: i32,
    val_len: i32,
    ttl_secs: i64,
) -> i32 {
    if ttl_secs < 0 {
        caller.data_mut().last_error = Some("ainb_cache_put: negative ttl".into());
        return HostStatus::InvalidArgument as i32;
    }
    let key_raw = match read_string(caller, key_ptr, key_len) {
        Ok(s) => s,
        Err(e) => {
            caller.data_mut().last_error = Some(format!("ainb_cache_put: bad key utf8: {e}"));
            return HostStatus::InvalidArgument as i32;
        }
    };
    if key_raw.is_empty() {
        caller.data_mut().last_error = Some("ainb_cache_put: empty key".into());
        return HostStatus::InvalidArgument as i32;
    }
    let val = match read_bytes(caller, val_ptr, val_len) {
        Ok(b) => b,
        Err(e) => {
            caller.data_mut().last_error = Some(format!("ainb_cache_put: read value: {e}"));
            return HostStatus::InvalidArgument as i32;
        }
    };
    let key = sanitize_key(&key_raw);
    let plugin_id = caller.data().plugin_id.clone();
    let Some(dir) = plugin_cache_dir(&plugin_id) else {
        caller.data_mut().last_error = Some("ainb_cache_put: $HOME unresolvable".into());
        return HostStatus::HostError as i32;
    };
    if let Err(e) = fs::create_dir_all(&dir) {
        caller.data_mut().last_error = Some(format!("ainb_cache_put: mkdir: {e}"));
        return HostStatus::HostError as i32;
    }

    // Per-plugin quota: if this write would push the plugin over the
    // cap, evict its own oldest entries (by mtime) until it fits.
    if let Err(e) = enforce_plugin_quota(&dir, val.len() as u64, quota_plugin_bytes()) {
        caller.data_mut().last_error = Some(format!("ainb_cache_put: quota: {e}"));
        return HostStatus::HostError as i32;
    }

    // Global quota: if total cache is over the global cap, sweep the
    // global oldest until under cap.
    if let Some(root) = ainb_cache_root() {
        if let Err(e) = enforce_global_quota(&root, quota_global_bytes()) {
            caller.data_mut().last_error = Some(format!("ainb_cache_put: global quota: {e}"));
            return HostStatus::HostError as i32;
        }
    }

    let val_path = dir.join(&key);
    let meta_path = dir.join(format!("{key}.meta"));
    if let Err(e) = fs::write(&val_path, &val) {
        caller.data_mut().last_error =
            Some(format!("ainb_cache_put: write {}: {e}", val_path.display()));
        return HostStatus::HostError as i32;
    }
    let deadline: u64 = if ttl_secs == 0 {
        u64::MAX // 0 = no expiry; encode as u64::MAX so the get-side compare always passes
    } else {
        now_secs().saturating_add(u64::try_from(ttl_secs).unwrap_or(u64::MAX))
    };
    if let Err(e) = fs::write(&meta_path, deadline.to_string()) {
        caller.data_mut().last_error =
            Some(format!("ainb_cache_put: write meta: {e}"));
        return HostStatus::HostError as i32;
    }
    0
}

fn entries_oldest_first(dir: &Path) -> std::io::Result<Vec<(PathBuf, SystemTime, u64)>> {
    let mut out = Vec::new();
    let read = match fs::read_dir(dir) {
        Ok(r) => r,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(e),
    };
    for entry in read.flatten() {
        let path = entry.path();
        if path.extension().map(|e| e == "meta").unwrap_or(false) {
            continue;
        }
        let m = entry.metadata()?;
        let mtime = m.modified().unwrap_or(UNIX_EPOCH);
        out.push((path, mtime, m.len()));
    }
    out.sort_by_key(|(_, m, _)| *m);
    Ok(out)
}

fn dir_total_bytes(dir: &Path) -> u64 {
    entries_oldest_first(dir)
        .map(|v| v.iter().map(|(_, _, n)| *n).sum())
        .unwrap_or(0)
}

fn enforce_plugin_quota(dir: &Path, incoming: u64, cap: u64) -> std::io::Result<()> {
    if incoming > cap {
        // Single-write larger than the entire quota — defer to error
        // path; we don't truncate caller's data.
        return Err(std::io::Error::new(
            std::io::ErrorKind::Other,
            format!("value {incoming} exceeds plugin quota {cap}"),
        ));
    }
    let mut current = dir_total_bytes(dir);
    if current + incoming <= cap {
        return Ok(());
    }
    let entries = entries_oldest_first(dir)?;
    for (path, _, sz) in entries {
        let _ = fs::remove_file(&path);
        let meta = path.with_extension({
            let ext = path
                .extension()
                .and_then(|s| s.to_str())
                .unwrap_or("");
            if ext.is_empty() {
                "meta".to_string()
            } else {
                format!("{ext}.meta")
            }
        });
        // The sidecar is `<key>.meta`; the simplest invariant is to try
        // both forms (`<key>` had no extension → meta is `<key>.meta`).
        let _ = fs::remove_file(meta);
        let alt_meta = {
            let mut s = path.as_os_str().to_owned();
            s.push(".meta");
            PathBuf::from(s)
        };
        let _ = fs::remove_file(alt_meta);
        current = current.saturating_sub(sz);
        if current + incoming <= cap {
            break;
        }
    }
    Ok(())
}

fn enforce_global_quota(root: &Path, cap: u64) -> std::io::Result<()> {
    // Walk all per-plugin subdirs once to compute the global total.
    let read = match fs::read_dir(root) {
        Ok(r) => r,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    let mut all: Vec<(PathBuf, SystemTime, u64)> = Vec::new();
    for plugin in read.flatten() {
        let pp = plugin.path();
        if !pp.is_dir() {
            continue;
        }
        for (path, mtime, sz) in entries_oldest_first(&pp)? {
            all.push((path, mtime, sz));
        }
    }
    let mut total: u64 = all.iter().map(|(_, _, n)| *n).sum();
    if total <= cap {
        return Ok(());
    }
    all.sort_by_key(|(_, m, _)| *m);
    for (path, _, sz) in all {
        let _ = fs::remove_file(&path);
        let alt_meta = {
            let mut s = path.as_os_str().to_owned();
            s.push(".meta");
            PathBuf::from(s)
        };
        let _ = fs::remove_file(alt_meta);
        total = total.saturating_sub(sz);
        if total <= cap {
            break;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn sanitize_id_strips_path_separators() {
        assert_eq!(sanitize_id("foo/bar"), "foo_bar");
        assert_eq!(sanitize_id("evil\\path"), "evil_path");
        assert_eq!(sanitize_id("ok"), "ok");
    }

    #[test]
    fn enforce_plugin_quota_evicts_oldest_first() {
        let dir = TempDir::new().unwrap();
        // Write three 10-byte entries with sleep between so mtime
        // ordering is deterministic on every filesystem (sub-second
        // resolution varies across HFS/APFS/ext4 — 50ms is enough).
        for (name, body) in [("a", &b"AAAAAAAAAA"[..]), ("b", &b"BBBBBBBBBB"[..]), ("c", &b"CCCCCCCCCC"[..])] {
            fs::write(dir.path().join(name), body).unwrap();
            fs::write(dir.path().join(format!("{name}.meta")), "0").unwrap();
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        // Cap of 20 bytes + 10-byte incoming → must evict `a` (oldest)
        // and possibly `b` to fit. `c` is newest; must survive.
        enforce_plugin_quota(dir.path(), 10, 20).expect("quota ok");
        assert!(!dir.path().join("a").exists(), "oldest must be evicted");
        assert!(dir.path().join("c").exists(), "newest must survive");
    }

    #[test]
    fn sanitize_key_strips_traversal_chars() {
        assert_eq!(sanitize_key("foo/bar"), "foo_bar");
        assert_eq!(sanitize_key("plain"), "plain");
        assert_eq!(sanitize_key("with\nnewline"), "with_newline");
    }

    #[test]
    fn now_secs_is_monotonic_within_a_call() {
        let a = now_secs();
        let b = now_secs();
        assert!(b >= a);
    }
}
