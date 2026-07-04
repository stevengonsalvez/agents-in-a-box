// ABOUTME: The card-create repo roster reader (spec F3) — the "@ autocomplete
// source" New Session already uses, read from BELOW ainb-core so the hangar
// daemon can serve it without a crate cycle.
//
// WHY HERE (not ainb-core)
// The New-Session repo picker reads two host-side files: the FavoritesStore
// (`~/.agents-in-a-box/favorites.yaml`) and the RepositoryCache scan cache
// (`~/.agents-in-a-box/cache/repositories.json`). Those types live in ainb-core,
// which the hangar daemon can NOT depend on (ainb-core already depends on the
// daemon). So this module re-reads the SAME two files with its own minimal serde
// mirrors — the daemon's `hangar/repo_list` RPC calls it, and the plugin fuzzy-
// filters the returned roster on the `@`-query plugin-side.
//
// RULES (spec F3 + reference_new_session_test_traps)
// - Favorites FIRST (★ pinned), scanned repos SECOND.
// - Recency: favorites carry `stats.last_used`; the roster sorts favorites
//   most-recent-first (alias tiebreak), so the picker's recency tiebreak is
//   already applied.
// - Scan cache is read AS-IS. This reader NEVER triggers a scan (the daemon must
//   not block a card-create on a cold filesystem walk); a cold/absent cache
//   simply yields no scanned repos.
// - A favorite migrated to a remote indicator has no local path — that is
//   expected (the trap: migrate-to-remote drops no-remote stars). The scanned
//   entry for the same repo (which DOES carry a local path) is deduped against
//   the favorite so the picker shows one row, keeping the ★.

use std::path::Path;

use chrono::{DateTime, Utc};
use serde::Deserialize;

/// One pickable repository in the card-create `@` roster.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoEntry {
    /// The display name: a favorite's `alias`, or a scanned repo's `name`.
    pub name: String,
    /// The local checkout path, when known. Scanned repos always carry one;
    /// a favorite migrated to a remote indicator carries `None` (no local path).
    pub path: Option<String>,
    /// The remote indicator (`owner/repo` shorthand or a URL), when this entry
    /// came from a favorite. `None` for a scan-only entry.
    pub remote: Option<String>,
    /// Whether this entry is a ★ favorite (pinned first in the roster).
    pub is_favorite: bool,
    /// Recency (epoch ms) from a favorite's `stats.last_used`, when present.
    /// `None` for scan-only entries and favorites with no recorded use.
    pub last_used_ms: Option<i64>,
}

/// Read the card-create repo roster from `ainb_dir` (the `.agents-in-a-box`
/// directory), favorites-first + scanned-second with recency applied.
///
/// Reads `ainb_dir/favorites.yaml` and `ainb_dir/cache/repositories.json` AS-IS
/// (never scanning). A missing / unparseable file contributes no entries rather
/// than erroring — a cold cache or a first-run install yields whatever the other
/// source has (possibly an empty roster, which the picker renders as just the
/// `📁 scratch` first entry the plugin prepends).
#[must_use]
pub fn read_roster(ainb_dir: &Path) -> Vec<RepoEntry> {
    let mut favorites = read_favorites(&ainb_dir.join("favorites.yaml"));
    // Favorites most-recent-first; a favorite with no recorded use sorts after
    // those that have one, alias-ascending as the stable tiebreak.
    favorites.sort_by(|a, b| {
        b.last_used_ms
            .cmp(&a.last_used_ms)
            .then_with(|| a.name.to_ascii_lowercase().cmp(&b.name.to_ascii_lowercase()))
    });

    // Dedup keys: a favorite claims its alias AND the trailing segment of its
    // remote (`owner/repo` → `repo`), both lowercased, so the scanned entry for
    // the same repo folds into the ★ favorite row instead of duplicating.
    let mut claimed: std::collections::HashSet<String> = std::collections::HashSet::new();
    for fav in &favorites {
        claimed.insert(fav.name.to_ascii_lowercase());
        if let Some(remote) = &fav.remote {
            if let Some(tail) = remote.rsplit('/').next() {
                claimed.insert(trim_git_suffix(tail).to_ascii_lowercase());
            }
        }
    }

    let mut roster = favorites;
    for scanned in read_scanned(&ainb_dir.join("cache").join("repositories.json")) {
        if claimed.contains(&scanned.name.to_ascii_lowercase()) {
            continue;
        }
        roster.push(scanned);
    }
    roster
}

/// Strip a trailing `.git` from a remote's last segment so `repo.git` dedups
/// against a scanned `repo`.
fn trim_git_suffix(s: &str) -> &str {
    s.strip_suffix(".git").unwrap_or(s)
}

// ---------------------------------------------------------------------------
// FavoritesStore mirror (only the fields the roster needs).
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct FavoritesFile {
    #[serde(default)]
    favorites: Vec<FavoriteRow>,
}

#[derive(Debug, Deserialize)]
struct FavoriteRow {
    alias: String,
    #[serde(default)]
    source: String,
    #[serde(default)]
    stats: FavoriteStats,
}

#[derive(Debug, Default, Deserialize)]
struct FavoriteStats {
    #[serde(default)]
    last_used: Option<DateTime<Utc>>,
}

/// Read + parse the favorites file into roster entries. A missing / malformed
/// file yields an empty vec (never an error — the roster degrades gracefully).
fn read_favorites(path: &Path) -> Vec<RepoEntry> {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(file) = serde_yaml::from_str::<FavoritesFile>(&raw) else {
        return Vec::new();
    };
    file.favorites
        .into_iter()
        .filter(|f| !f.alias.trim().is_empty())
        .map(|f| {
            let remote = (!f.source.trim().is_empty()).then(|| f.source.clone());
            RepoEntry {
                name: f.alias,
                path: None,
                remote,
                is_favorite: true,
                last_used_ms: f.stats.last_used.map(|t| t.timestamp_millis()),
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// RepositoryCache mirror (only `repositories[].{path,name}`).
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct RepositoryCacheFile {
    #[serde(default)]
    repositories: Vec<CachedRepo>,
}

#[derive(Debug, Deserialize)]
struct CachedRepo {
    path: String,
    name: String,
}

/// Read + parse the scan cache into roster entries, IN CACHE ORDER, without
/// scanning. A missing / malformed / cold cache yields an empty vec.
fn read_scanned(path: &Path) -> Vec<RepoEntry> {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(file) = serde_json::from_str::<RepositoryCacheFile>(&raw) else {
        return Vec::new();
    };
    file.repositories
        .into_iter()
        .filter(|r| !r.name.trim().is_empty())
        .map(|r| RepoEntry {
            name: r.name,
            path: Some(r.path),
            remote: None,
            is_favorite: false,
            last_used_ms: None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    /// Write `favorites.yaml` + the scan cache into a fresh `.agents-in-a-box`
    /// dir and return it.
    fn seed_dir(favorites_yaml: Option<&str>, cache_json: Option<&str>) -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join(".agents-in-a-box");
        fs::create_dir_all(dir.join("cache")).unwrap();
        if let Some(y) = favorites_yaml {
            fs::write(dir.join("favorites.yaml"), y).unwrap();
        }
        if let Some(j) = cache_json {
            fs::write(dir.join("cache").join("repositories.json"), j).unwrap();
        }
        (tmp, dir)
    }

    /// A completely cold install (no files) yields an empty roster — never an
    /// error, never a triggered scan.
    #[test]
    fn cold_install_is_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let roster = read_roster(&tmp.path().join(".agents-in-a-box"));
        assert!(roster.is_empty());
    }

    /// Favorites come first, sorted most-recent-first, and carry the ★ flag.
    #[test]
    fn favorites_first_by_recency() {
        let yaml = r"
version: 1
favorites:
  - alias: older
    source_type: github_shorthand
    source: acme/older
    stats:
      created_at: 2026-01-01T00:00:00Z
      last_used: 2026-01-01T00:00:00Z
      use_count: 2
  - alias: newer
    source_type: github_shorthand
    source: acme/newer
    stats:
      created_at: 2026-06-01T00:00:00Z
      last_used: 2026-06-01T00:00:00Z
      use_count: 9
settings: {}
";
        let (_tmp, dir) = seed_dir(Some(yaml), None);
        let roster = read_roster(&dir);
        assert_eq!(roster.len(), 2);
        assert_eq!(roster[0].name, "newer", "most-recent favorite pinned first");
        assert_eq!(roster[1].name, "older");
        assert!(roster.iter().all(|e| e.is_favorite));
        assert_eq!(roster[0].remote.as_deref(), Some("acme/newer"));
        assert!(roster[0].last_used_ms.unwrap() > roster[1].last_used_ms.unwrap());
    }

    /// Scanned repos follow favorites, in cache order, carrying a local path.
    #[test]
    fn scanned_follow_favorites_in_cache_order() {
        let yaml = r"
favorites:
  - alias: star
    source_type: github_shorthand
    source: acme/star
    stats:
      last_used: 2026-06-01T00:00:00Z
";
        let json = r#"{
  "version": 1,
  "last_scan": "2026-06-01T00:00:00Z",
  "scan_paths": ["/repos"],
  "scan_paths_mtime": {},
  "repositories": [
    {"path": "/repos/beta", "name": "beta"},
    {"path": "/repos/alpha", "name": "alpha"}
  ]
}"#;
        let (_tmp, dir) = seed_dir(Some(yaml), Some(json));
        let roster = read_roster(&dir);
        assert_eq!(roster.len(), 3);
        assert_eq!(roster[0].name, "star");
        assert!(roster[0].is_favorite);
        // Cache order preserved (beta then alpha — NOT alphabetised).
        assert_eq!(roster[1].name, "beta");
        assert_eq!(roster[1].path.as_deref(), Some("/repos/beta"));
        assert!(!roster[1].is_favorite);
        assert_eq!(roster[2].name, "alpha");
    }

    /// A scanned repo that matches a favorite (by remote tail or `.git` suffix)
    /// is deduped away — the picker shows one ★ row, not two.
    #[test]
    fn scanned_dedups_against_favorite() {
        let yaml = r"
favorites:
  - alias: claude-code
    source_type: github_shorthand
    source: anthropics/claude-code
    stats:
      last_used: 2026-06-01T00:00:00Z
";
        // The scanned repo's name is the remote tail with a `.git` — still dedups.
        let json = r#"{
  "repositories": [
    {"path": "/repos/claude-code", "name": "claude-code"},
    {"path": "/repos/other", "name": "other"}
  ]
}"#;
        let (_tmp, dir) = seed_dir(Some(yaml), Some(json));
        let roster = read_roster(&dir);
        assert_eq!(roster.len(), 2, "claude-code folds into the favorite");
        assert_eq!(roster[0].name, "claude-code");
        assert!(roster[0].is_favorite);
        assert_eq!(roster[1].name, "other");
    }

    /// A malformed cache file is ignored (no scan, no error) — favorites still
    /// surface. This is the cold/corrupt-cache degradation path.
    #[test]
    fn malformed_cache_is_ignored() {
        let yaml = r"
favorites:
  - alias: keep
    source: acme/keep
    stats: {}
";
        let (_tmp, dir) = seed_dir(Some(yaml), Some("{ this is not json"));
        let roster = read_roster(&dir);
        assert_eq!(roster.len(), 1);
        assert_eq!(roster[0].name, "keep");
        assert_eq!(roster[0].last_used_ms, None, "no last_used → None recency");
    }
}
