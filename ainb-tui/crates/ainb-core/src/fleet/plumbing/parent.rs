// ABOUTME: Child→parent session linkage — the inbox routing key.
//
// A spawned session records its parent so a finishing child can route its
// completion to the right parent's inbox. There are two linkage sources, in
// priority order:
//
//   1. The `AINB_PARENT_SESSION` environment variable, exported into the
//      child's session by `ainb run --parent <id>`. This is the live, in-band
//      signal the Stop hook reads first — it needs no disk lookup.
//   2. A durable map at `~/.agents-in-a-box/parents.json` ({child_id: parent_id}),
//      written by `ainb run --parent`. The fallback when the env var is absent
//      (e.g. the hook fired in a context that lost the env, or the child id the
//      hook reports differs from the spawn-time id).
//
// Keeping both means linkage survives a restart (the durable map) while staying
// zero-lookup on the hot path (the env var).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use fs2::FileExt;

use super::atomic::write_atomic_json;
use super::paths;

/// Environment variable carrying the parent session id into a spawned child.
pub const PARENT_ENV: &str = "AINB_PARENT_SESSION";

/// Path to the durable child→parent map under the default home.
pub fn map_path() -> Result<PathBuf> {
    Ok(paths::ainb_home()?.join("parents.json"))
}

/// Path to the durable child→parent map under an explicit home.
#[must_use]
pub fn map_path_in(home: &Path) -> PathBuf {
    home.join("parents.json")
}

/// Path to the advisory lock guarding the parents.json read-modify-write window.
#[must_use]
pub fn map_lock_path_in(home: &Path) -> PathBuf {
    home.join("parents.json.lock")
}

/// Acquire an exclusive advisory lock guarding the parents.json
/// read-modify-write window. Two children self-registering concurrently each do
/// a load→insert→write_atomic; the atomic rename keeps the file from tearing but
/// does NOT prevent a lost update (child A reads, child B reads, A writes, B
/// writes over A — A's linkage vanishes and A's completion is silently dropped).
/// Holding this lock for the whole RMW serialises the registrations so no update
/// is lost. Mirrors the `Inbox::lock` / `settings::lock_settings` pattern.
fn lock_map(home: &Path) -> Result<std::fs::File> {
    std::fs::create_dir_all(home)
        .with_context(|| format!("creating ainb home {}", home.display()))?;
    let f = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(map_lock_path_in(home))
        .context("opening parents.json lock file")?;
    f.lock_exclusive().context("acquiring parents.json lock")?;
    Ok(f)
}

/// Read the durable child→parent map under `home` (missing/corrupt → empty).
#[must_use]
pub fn load_map_in(home: &Path) -> BTreeMap<String, String> {
    std::fs::read_to_string(map_path_in(home))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Record `child_id`'s parent durably under `home`. Idempotent (last write wins).
///
/// The whole load→insert→write happens under [`lock_map`] so two children
/// self-registering concurrently cannot lost-update each other (which would
/// erase a child's linkage and silently drop its completion). The lock makes the
/// read-modify-write mutually exclusive across processes + threads.
pub fn record_parent_in(home: &Path, child_id: &str, parent_id: &str) -> Result<()> {
    if child_id.trim().is_empty() || parent_id.trim().is_empty() {
        return Ok(());
    }
    let _guard = lock_map(home)?;
    let mut map = load_map_in(home);
    map.insert(child_id.to_string(), parent_id.to_string());
    write_atomic_json(&map_path_in(home), &map).context("writing parents.json")
}

/// Resolve the parent of `child_id` under `home`: the `AINB_PARENT_SESSION`
/// environment value first (live, in-band), else the durable map. Returns `None`
/// when neither source links the child (it is a leaf / unmanaged session).
#[must_use]
pub fn resolve_parent_in(home: &Path, child_id: &str, env_parent: Option<&str>) -> Option<String> {
    if let Some(p) = env_parent {
        let p = p.trim();
        if !p.is_empty() {
            return Some(p.to_string());
        }
    }
    load_map_in(home).get(child_id).filter(|p| !p.trim().is_empty()).cloned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn record_then_resolve_from_map() {
        let home = TempDir::new().unwrap();
        record_parent_in(home.path(), "child", "parent").unwrap();
        assert_eq!(
            resolve_parent_in(home.path(), "child", None),
            Some("parent".to_string())
        );
    }

    #[test]
    fn env_var_takes_priority_over_map() {
        let home = TempDir::new().unwrap();
        record_parent_in(home.path(), "child", "map-parent").unwrap();
        assert_eq!(
            resolve_parent_in(home.path(), "child", Some("env-parent")),
            Some("env-parent".to_string())
        );
    }

    #[test]
    fn blank_env_falls_through_to_map() {
        let home = TempDir::new().unwrap();
        record_parent_in(home.path(), "child", "map-parent").unwrap();
        assert_eq!(
            resolve_parent_in(home.path(), "child", Some("   ")),
            Some("map-parent".to_string())
        );
    }

    #[test]
    fn unlinked_child_resolves_none() {
        let home = TempDir::new().unwrap();
        assert!(resolve_parent_in(home.path(), "leaf", None).is_none());
    }

    #[test]
    fn record_ignores_blank_ids() {
        let home = TempDir::new().unwrap();
        record_parent_in(home.path(), "", "p").unwrap();
        record_parent_in(home.path(), "c", "").unwrap();
        assert!(load_map_in(home.path()).is_empty());
    }

    #[test]
    fn record_is_last_write_wins() {
        let home = TempDir::new().unwrap();
        record_parent_in(home.path(), "child", "p1").unwrap();
        record_parent_in(home.path(), "child", "p2").unwrap();
        assert_eq!(
            resolve_parent_in(home.path(), "child", None),
            Some("p2".to_string())
        );
    }

    /// Regression (C1 / H-A2): the load→insert→write of parents.json runs under
    /// an advisory lock, so two children self-registering DIFFERENT linkages
    /// concurrently both survive. The pre-fix unlocked read-modify-write
    /// lost-updates under contention — one child's linkage would vanish and its
    /// completion would be silently dropped. We register N distinct children from
    /// N threads against one shared home and assert ALL N linkages are present.
    #[test]
    fn concurrent_distinct_registrations_lose_no_update() {
        use std::sync::Arc;
        const N: usize = 64;
        let home = Arc::new(TempDir::new().unwrap());

        let mut handles = Vec::new();
        for i in 0..N {
            let h = home.clone();
            handles.push(std::thread::spawn(move || {
                let child = format!("child-{i}");
                let parent = format!("parent-{i}");
                record_parent_in(h.path(), &child, &parent).unwrap();
            }));
        }
        for j in handles {
            j.join().unwrap();
        }

        let map = load_map_in(home.path());
        assert_eq!(map.len(), N, "lost-update: not all linkages survived");
        for i in 0..N {
            assert_eq!(
                map.get(&format!("child-{i}")).map(String::as_str),
                Some(format!("parent-{i}").as_str()),
                "child-{i} linkage vanished under concurrency",
            );
        }
    }

    #[test]
    fn record_creates_and_uses_the_map_lock() {
        let home = TempDir::new().unwrap();
        let lock = map_lock_path_in(home.path());
        assert!(!lock.exists(), "lock file should not exist before record");
        record_parent_in(home.path(), "c", "p").unwrap();
        assert!(
            lock.exists(),
            "record must create + use the parents.json lock"
        );
    }
}
