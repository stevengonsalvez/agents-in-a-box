// ABOUTME: Persistent storage for repository favorites
// Stores favorites in ~/.agents-in-a-box/favorites.yaml for quick access to
// frequently-used repos. A favorite ALWAYS records a remote indicator (an
// HTTPS/SSH URL or `owner/repo` shorthand) — never a local path. Local-path
// entries from older versions are rewritten to their `origin` remote (or
// dropped) by `migrate_local_to_remote`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// Source type for a favorite repository
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SourceType {
    HttpsUrl,
    SshUrl,
    GithubShorthand,
    LocalPath,
}

impl SourceType {
    /// Get display name for UI
    pub fn display_name(&self) -> &'static str {
        match self {
            SourceType::HttpsUrl => "HTTPS",
            SourceType::SshUrl => "SSH",
            SourceType::GithubShorthand => "GitHub",
            SourceType::LocalPath => "Local",
        }
    }
}

/// Metadata about a favorite repository
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FavoriteMetadata {
    pub host: String,
    pub owner: String,
    pub repo_name: String,
}

/// Usage statistics for a favorite
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FavoriteStats {
    pub created_at: DateTime<Utc>,
    pub last_used: DateTime<Utc>,
    pub use_count: u64,
}

impl Default for FavoriteStats {
    fn default() -> Self {
        let now = Utc::now();
        Self {
            created_at: now,
            last_used: now,
            use_count: 0,
        }
    }
}

/// A single favorite repository entry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Favorite {
    pub alias: String,
    pub source_type: SourceType,
    pub source: String,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub metadata: FavoriteMetadata,
    #[serde(default)]
    pub stats: FavoriteStats,
}

impl Favorite {
    /// Create a new favorite with minimal info
    pub fn new(alias: String, source: String, source_type: SourceType) -> Self {
        Self {
            alias,
            source,
            source_type,
            display_name: None,
            description: None,
            tags: Vec::new(),
            metadata: FavoriteMetadata::default(),
            stats: FavoriteStats::default(),
        }
    }

    /// Get the display name, falling back to alias
    pub fn display(&self) -> &str {
        self.display_name.as_deref().unwrap_or(&self.alias)
    }

    /// Check if favorite matches a search query (case-insensitive)
    pub fn matches_query(&self, query: &str) -> bool {
        let query_lower = query.to_lowercase();
        let search_text = format!(
            "{} {} {} {}",
            self.alias,
            self.source,
            self.display_name.as_deref().unwrap_or(""),
            self.tags.join(" ")
        )
        .to_lowercase();

        search_text.contains(&query_lower)
    }
}

/// Settings for favorites behavior
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FavoritesSettings {
    #[serde(default = "default_auto_promote_threshold")]
    pub auto_promote_threshold: u32,
}

impl Default for FavoritesSettings {
    fn default() -> Self {
        Self {
            auto_promote_threshold: default_auto_promote_threshold(),
        }
    }
}

fn default_auto_promote_threshold() -> u32 {
    5
}

/// Store for repository favorites
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FavoritesStore {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default)]
    pub favorites: Vec<Favorite>,
    #[serde(default)]
    pub settings: FavoritesSettings,
}

fn default_version() -> u32 {
    1
}

impl Default for FavoritesStore {
    fn default() -> Self {
        Self {
            version: 1,
            favorites: Vec::new(),
            settings: FavoritesSettings::default(),
        }
    }
}

impl FavoritesStore {
    /// Get the storage file path (stored in TUI's own config dir, not ~/.claude)
    fn storage_path() -> Option<PathBuf> {
        dirs::home_dir().map(|h| h.join(".agents-in-a-box").join("favorites.yaml"))
    }

    /// Load from disk (returns empty store if file doesn't exist)
    pub fn load() -> Self {
        Self::storage_path()
            .and_then(|path| fs::read_to_string(path).ok())
            .and_then(|content| serde_yaml::from_str(&content).ok())
            .unwrap_or_default()
    }

    /// Save to disk
    pub fn save(&self) -> Result<(), std::io::Error> {
        if let Some(path) = Self::storage_path() {
            // Ensure directory exists
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            let content = serde_yaml::to_string(self)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
            fs::write(path, content)?;
        }
        Ok(())
    }

    /// Path of the one-time pre-migration backup (`favorites.yaml.pre-remote-migration.bak`).
    fn migration_backup_path() -> Option<PathBuf> {
        Self::storage_path().map(|p| {
            let mut s = p.into_os_string();
            s.push(".pre-remote-migration.bak");
            PathBuf::from(s)
        })
    }

    /// Write a one-time backup of the current store before the destructive
    /// local→remote migration overwrites `favorites.yaml`. No-op if a backup
    /// already exists (idempotent across restarts), so the original is never
    /// clobbered by a second migration pass.
    pub fn write_migration_backup(&self) -> Result<(), std::io::Error> {
        if let Some(path) = Self::migration_backup_path() {
            if path.exists() {
                return Ok(());
            }
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            let content = serde_yaml::to_string(self)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
            fs::write(path, content)?;
        }
        Ok(())
    }

    /// Get a favorite by alias
    pub fn get(&self, alias: &str) -> Option<&Favorite> {
        self.favorites.iter().find(|f| f.alias == alias)
    }

    /// Get a mutable favorite by alias
    pub fn get_mut(&mut self, alias: &str) -> Option<&mut Favorite> {
        self.favorites.iter_mut().find(|f| f.alias == alias)
    }

    /// Check if an alias exists
    pub fn has_alias(&self, alias: &str) -> bool {
        self.favorites.iter().any(|f| f.alias == alias)
    }

    /// Add a favorite (returns error if alias already exists)
    pub fn add(&mut self, favorite: Favorite) -> Result<(), &'static str> {
        if self.has_alias(&favorite.alias) {
            return Err("Alias already exists");
        }
        self.favorites.push(favorite);
        Ok(())
    }

    /// Add or replace a favorite
    pub fn set(&mut self, favorite: Favorite) {
        if let Some(existing) = self.get_mut(&favorite.alias) {
            *existing = favorite;
        } else {
            self.favorites.push(favorite);
        }
    }

    /// Remove a favorite by alias
    pub fn remove(&mut self, alias: &str) -> Option<Favorite> {
        if let Some(pos) = self.favorites.iter().position(|f| f.alias == alias) {
            Some(self.favorites.remove(pos))
        } else {
            None
        }
    }

    /// Record usage of a favorite (updates stats)
    pub fn record_use(&mut self, alias: &str) {
        if let Some(favorite) = self.get_mut(alias) {
            favorite.stats.last_used = Utc::now();
            favorite.stats.use_count += 1;
        }
    }

    /// Search favorites by query
    pub fn search(&self, query: &str) -> Vec<&Favorite> {
        self.favorites.iter().filter(|f| f.matches_query(query)).collect()
    }

    /// Get all favorites sorted by use count (most used first)
    pub fn sorted_by_usage(&self) -> Vec<&Favorite> {
        let mut sorted: Vec<_> = self.favorites.iter().collect();
        sorted.sort_by(|a, b| b.stats.use_count.cmp(&a.stats.use_count));
        sorted
    }

    /// Get total number of favorites
    pub fn len(&self) -> usize {
        self.favorites.len()
    }

    /// Check if store is empty
    pub fn is_empty(&self) -> bool {
        self.favorites.is_empty()
    }

    /// Get total uses across all favorites
    pub fn total_uses(&self) -> u64 {
        self.favorites.iter().map(|f| f.stats.use_count).sum()
    }

    /// Rewrite any legacy `LocalPath` favorites to their `origin` remote
    /// indicator, dropping the ones whose path has no resolvable remote.
    ///
    /// Idempotent: a store that already contains only remote entries returns an
    /// empty [`MigrationReport`] and is left untouched. The caller is
    /// responsible for persisting (`save`) when the returned report is
    /// non-empty.
    pub fn migrate_local_to_remote(&mut self) -> MigrationReport {
        let mut report = MigrationReport::default();
        let mut keep: Vec<Favorite> = Vec::with_capacity(self.favorites.len());

        for fav in std::mem::take(&mut self.favorites) {
            if fav.source_type != SourceType::LocalPath {
                keep.push(fav);
                continue;
            }
            match favorite_from_local_repo(fav.alias.clone(), Path::new(&fav.source)) {
                Ok(remote) => {
                    // Preserve user-facing metadata + stats; only the source
                    // pointer changes.
                    let migrated = Favorite {
                        alias: fav.alias.clone(),
                        source_type: remote.source_type,
                        source: remote.source.clone(),
                        display_name: fav.display_name,
                        description: fav.description,
                        tags: fav.tags,
                        metadata: fav.metadata,
                        stats: fav.stats,
                    };
                    report.migrated.push((fav.alias, remote.source));
                    keep.push(migrated);
                }
                Err(_) => {
                    report.dropped.push(fav.alias);
                }
            }
        }

        self.favorites = keep;
        report
    }
}

/// Why deriving a remote favorite from a local repo failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeriveFavoriteError {
    /// The path is not a git repository (or could not be opened).
    NotAGitRepo,
    /// The repository has no `origin` remote.
    NoRemote,
    /// The remote URL could not be classified as a clonable remote source.
    Unparseable(String),
}

impl std::fmt::Display for DeriveFavoriteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotAGitRepo => write!(f, "not a git repository"),
            Self::NoRemote => write!(f, "no git remote (origin) found"),
            Self::Unparseable(url) => write!(f, "unrecognised remote URL: {url}"),
        }
    }
}

/// Summary of a `migrate_local_to_remote` pass.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MigrationReport {
    /// `(alias, new_source)` for each local-path favorite rewritten to remote.
    pub migrated: Vec<(String, String)>,
    /// Aliases of local-path favorites dropped for having no resolvable remote.
    pub dropped: Vec<String>,
}

impl MigrationReport {
    /// True when nothing was migrated or dropped (store left untouched).
    pub fn is_empty(&self) -> bool {
        self.migrated.is_empty() && self.dropped.is_empty()
    }
}

/// Derive a remote [`Favorite`] from a local repository path by reading its
/// `origin` remote. Returns an error (and stores nothing) when the path is not
/// a git repo, has no `origin`, or whose remote URL can't be classified.
///
/// This is the single source of truth for the "a star is always a remote
/// indicator" rule — both star entry points and the startup migration call it.
pub fn favorite_from_local_repo(
    alias: String,
    local_path: &Path,
) -> Result<Favorite, DeriveFavoriteError> {
    let repo = crate::git::RepositoryManager::open(local_path)
        .map_err(|_| DeriveFavoriteError::NotAGitRepo)?;
    let url = match repo.get_remote_url() {
        Ok(Some(url)) if !url.trim().is_empty() => url,
        _ => return Err(DeriveFavoriteError::NoRemote),
    };
    let (source, source_type) =
        classify_remote_url(&url).ok_or_else(|| DeriveFavoriteError::Unparseable(url.clone()))?;
    Ok(Favorite::new(alias, source, source_type))
}

/// Classify an `origin` URL into a `(source_string, SourceType)` pair, or
/// `None` if it isn't a shareable remote.
///
/// GitHub hosts collapse to `owner/repo` shorthand for nicer display; every
/// other host keeps its full URL. SSH (`git@host:` / `ssh://`) maps to
/// `SshUrl`; `http(s)://` maps to `HttpsUrl`. Anything else — a bare local
/// path, or a non-shareable scheme such as `file://` — returns `None` so the
/// caller refuses the favorite.
fn classify_remote_url(url: &str) -> Option<(String, SourceType)> {
    use crate::git::RepoSource;

    let is_ssh = url.starts_with("git@") || url.starts_with("ssh://");
    let is_https = url.starts_with("http://") || url.starts_with("https://");
    let source = if is_ssh {
        RepoSource::SshUrl(url.to_string())
    } else if is_https {
        RepoSource::HttpsUrl(url.to_string())
    } else {
        // Bare local path, `file://`, or any other non-shareable scheme.
        return None;
    };

    // Prefer GitHub shorthand when the host resolves to github.com.
    if let Ok(parsed) = source.parse_components() {
        if parsed.host == "github.com" && !parsed.owner.is_empty() {
            return Some((
                format!("{}/{}", parsed.owner, parsed.repo_name),
                SourceType::GithubShorthand,
            ));
        }
    }

    match source {
        RepoSource::HttpsUrl(u) => Some((u, SourceType::HttpsUrl)),
        RepoSource::SshUrl(u) => Some((u, SourceType::SshUrl)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Create a git repo at `dir` with `origin` pointing at `url`.
    fn init_repo_with_remote(dir: &Path, url: &str) {
        let repo = git2::Repository::init(dir).expect("git init");
        repo.remote("origin", url).expect("add origin");
    }

    #[test]
    fn favorite_from_local_repo_github_https_becomes_shorthand() {
        let tmp = tempfile::tempdir().unwrap();
        init_repo_with_remote(tmp.path(), "https://github.com/anthropics/claude-code.git");
        let fav = favorite_from_local_repo("cc".into(), tmp.path()).unwrap();
        assert_eq!(fav.source_type, SourceType::GithubShorthand);
        assert_eq!(fav.source, "anthropics/claude-code");
    }

    #[test]
    fn favorite_from_local_repo_github_ssh_becomes_shorthand() {
        let tmp = tempfile::tempdir().unwrap();
        init_repo_with_remote(tmp.path(), "git@github.com:anthropics/claude-code.git");
        let fav = favorite_from_local_repo("cc".into(), tmp.path()).unwrap();
        assert_eq!(fav.source_type, SourceType::GithubShorthand);
        assert_eq!(fav.source, "anthropics/claude-code");
    }

    #[test]
    fn favorite_from_local_repo_non_github_keeps_url() {
        let tmp = tempfile::tempdir().unwrap();
        init_repo_with_remote(tmp.path(), "https://gitlab.com/group/proj.git");
        let fav = favorite_from_local_repo("p".into(), tmp.path()).unwrap();
        assert_eq!(fav.source_type, SourceType::HttpsUrl);
        assert_eq!(fav.source, "https://gitlab.com/group/proj.git");
    }

    #[test]
    fn favorite_from_local_repo_file_url_refused() {
        // A `file://` origin is machine-local, not a shareable remote → refused.
        let tmp = tempfile::tempdir().unwrap();
        init_repo_with_remote(tmp.path(), "file:///srv/git/proj.git");
        assert!(matches!(
            favorite_from_local_repo("p".into(), tmp.path()),
            Err(DeriveFavoriteError::Unparseable(_))
        ));
    }

    #[test]
    fn favorite_from_local_repo_no_origin_errors() {
        let tmp = tempfile::tempdir().unwrap();
        git2::Repository::init(tmp.path()).unwrap(); // repo, but no origin
        assert!(matches!(
            favorite_from_local_repo("x".into(), tmp.path()),
            Err(DeriveFavoriteError::NoRemote)
        ));
    }

    #[test]
    fn favorite_from_local_repo_not_a_repo_errors() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(matches!(
            favorite_from_local_repo("x".into(), tmp.path()),
            Err(DeriveFavoriteError::NotAGitRepo)
        ));
    }

    #[test]
    fn migrate_noop_when_all_remote() {
        let mut store = FavoritesStore::default();
        store
            .add(Favorite::new(
                "cc".into(),
                "anthropics/claude-code".into(),
                SourceType::GithubShorthand,
            ))
            .unwrap();
        let report = store.migrate_local_to_remote();
        assert!(report.is_empty());
        assert_eq!(store.len(), 1);
        assert_eq!(
            store.get("cc").unwrap().source_type,
            SourceType::GithubShorthand
        );
    }

    #[test]
    fn migrate_rewrites_localpath_with_origin() {
        let tmp = tempfile::tempdir().unwrap();
        init_repo_with_remote(tmp.path(), "https://github.com/anthropics/claude-code.git");

        let mut store = FavoritesStore::default();
        let mut fav = Favorite::new(
            "cc".into(),
            tmp.path().display().to_string(),
            SourceType::LocalPath,
        );
        fav.tags = vec!["keep-me".into()];
        fav.stats.use_count = 7;
        store.add(fav).unwrap();

        let report = store.migrate_local_to_remote();
        assert_eq!(report.dropped.len(), 0);
        assert_eq!(report.migrated.len(), 1);

        let migrated = store.get("cc").unwrap();
        assert_eq!(migrated.source_type, SourceType::GithubShorthand);
        assert_eq!(migrated.source, "anthropics/claude-code");
        // Metadata + stats preserved across the rewrite.
        assert_eq!(migrated.tags, vec!["keep-me".to_string()]);
        assert_eq!(migrated.stats.use_count, 7);
    }

    #[test]
    fn migrate_drops_localpath_without_remote() {
        let tmp = tempfile::tempdir().unwrap(); // not a git repo
        let mut store = FavoritesStore::default();
        store
            .add(Favorite::new(
                "orphan".into(),
                tmp.path().display().to_string(),
                SourceType::LocalPath,
            ))
            .unwrap();
        let report = store.migrate_local_to_remote();
        assert_eq!(report.dropped, vec!["orphan".to_string()]);
        assert!(store.is_empty());
    }

    #[test]
    fn test_favorite_creation() {
        let fav = Favorite::new(
            "claude".to_string(),
            "anthropics/claude-code".to_string(),
            SourceType::GithubShorthand,
        );
        assert_eq!(fav.alias, "claude");
        assert_eq!(fav.source, "anthropics/claude-code");
        assert_eq!(fav.stats.use_count, 0);
    }

    #[test]
    fn test_store_add_and_get() {
        let mut store = FavoritesStore::default();

        let fav = Favorite::new(
            "test".to_string(),
            "owner/repo".to_string(),
            SourceType::GithubShorthand,
        );

        assert!(store.add(fav).is_ok());
        assert!(store.has_alias("test"));
        assert!(store.get("test").is_some());

        // Should fail to add duplicate
        let dup = Favorite::new(
            "test".to_string(),
            "other/repo".to_string(),
            SourceType::GithubShorthand,
        );
        assert!(store.add(dup).is_err());
    }

    #[test]
    fn test_store_remove() {
        let mut store = FavoritesStore::default();

        let fav = Favorite::new(
            "test".to_string(),
            "owner/repo".to_string(),
            SourceType::GithubShorthand,
        );

        store.add(fav).unwrap();
        assert!(store.has_alias("test"));

        let removed = store.remove("test");
        assert!(removed.is_some());
        assert!(!store.has_alias("test"));
    }

    #[test]
    fn test_record_use() {
        let mut store = FavoritesStore::default();

        let fav = Favorite::new(
            "test".to_string(),
            "owner/repo".to_string(),
            SourceType::GithubShorthand,
        );

        store.add(fav).unwrap();
        assert_eq!(store.get("test").unwrap().stats.use_count, 0);

        store.record_use("test");
        assert_eq!(store.get("test").unwrap().stats.use_count, 1);

        store.record_use("test");
        assert_eq!(store.get("test").unwrap().stats.use_count, 2);
    }

    #[test]
    fn test_search() {
        let mut store = FavoritesStore::default();

        let mut fav1 = Favorite::new(
            "react".to_string(),
            "facebook/react".to_string(),
            SourceType::GithubShorthand,
        );
        fav1.tags = vec!["frontend".to_string()];

        let fav2 = Favorite::new(
            "vue".to_string(),
            "vuejs/vue".to_string(),
            SourceType::GithubShorthand,
        );

        store.add(fav1).unwrap();
        store.add(fav2).unwrap();

        // Search by alias
        let results = store.search("react");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].alias, "react");

        // Search by tag
        let results = store.search("frontend");
        assert_eq!(results.len(), 1);

        // Search by source
        let results = store.search("facebook");
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn test_sorted_by_usage() {
        let mut store = FavoritesStore::default();

        let fav1 = Favorite::new(
            "low".to_string(),
            "owner/low".to_string(),
            SourceType::GithubShorthand,
        );
        let fav2 = Favorite::new(
            "high".to_string(),
            "owner/high".to_string(),
            SourceType::GithubShorthand,
        );

        store.add(fav1).unwrap();
        store.add(fav2).unwrap();

        // Use "high" more
        store.record_use("high");
        store.record_use("high");
        store.record_use("high");
        store.record_use("low");

        let sorted = store.sorted_by_usage();
        assert_eq!(sorted[0].alias, "high");
        assert_eq!(sorted[1].alias, "low");
    }

    #[test]
    fn test_serialization() {
        let mut store = FavoritesStore::default();

        let mut fav = Favorite::new(
            "test".to_string(),
            "owner/repo".to_string(),
            SourceType::GithubShorthand,
        );
        fav.display_name = Some("Test Repo".to_string());
        fav.tags = vec!["tag1".to_string(), "tag2".to_string()];

        store.add(fav).unwrap();

        let yaml = serde_yaml::to_string(&store).unwrap();
        assert!(yaml.contains("alias: test"));
        assert!(yaml.contains("github_shorthand"));

        let loaded: FavoritesStore = serde_yaml::from_str(&yaml).unwrap();
        assert!(loaded.has_alias("test"));
        assert_eq!(
            loaded.get("test").unwrap().display_name,
            Some("Test Repo".to_string())
        );
    }
}
