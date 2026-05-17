// ABOUTME: Persistent storage for repository favorites
// Stores favorites in ~/.claude/favorites.yaml for quick access to frequently-used repos

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

/// Source type for a favorite repository
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
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
}

#[cfg(test)]
mod tests {
    use super::*;

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
