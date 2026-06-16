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

/// Read the durable child→parent map under `home` (missing/corrupt → empty).
#[must_use]
pub fn load_map_in(home: &Path) -> BTreeMap<String, String> {
    std::fs::read_to_string(map_path_in(home))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Record `child_id`'s parent durably under `home`. Idempotent (last write wins).
pub fn record_parent_in(home: &Path, child_id: &str, parent_id: &str) -> Result<()> {
    if child_id.trim().is_empty() || parent_id.trim().is_empty() {
        return Ok(());
    }
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
}
