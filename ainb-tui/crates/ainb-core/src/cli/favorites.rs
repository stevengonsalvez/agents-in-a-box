// ABOUTME: CLI favorites command for managing the repository favorites store
//
// Subcommands:
//   list:    Show all favorites sorted by usage count (descending)
//   add:     Add a favorite (auto-detects source type)
//   remove:  Remove a favorite by alias
//   use:     Record usage of a favorite (updates last_used + use_count)

use anyhow::{Context, Result, anyhow};
use clap::Subcommand;
use serde::Serialize;
use std::path::Path;

use super::OutputFormat;
use crate::config::favorites_store::{Favorite, FavoritesStore, SourceType};

#[derive(Subcommand)]
pub enum FavoritesCommands {
    /// List all favorites sorted by usage (most used first)
    List,
    /// Add a new favorite repository
    Add {
        /// Repository source: owner/repo, https URL, ssh URL, or local path
        source: String,
        /// Friendly alias for this favorite (used to look it up later)
        #[arg(long)]
        alias: String,
        /// Optional human-readable description
        #[arg(long)]
        description: Option<String>,
        /// Comma-separated list of tags
        #[arg(long)]
        tags: Option<String>,
    },
    /// Remove a favorite by alias
    Remove {
        /// Alias of the favorite to remove
        alias: String,
    },
    /// Record usage of a favorite (bumps use_count and last_used)
    Use {
        /// Alias of the favorite to record usage for
        alias: String,
    },
}

/// JSON output shape for `favorites list`
#[derive(Debug, Serialize)]
struct FavoriteJson<'a> {
    alias: &'a str,
    source: &'a str,
    source_type: &'static str,
    description: Option<&'a str>,
    tags: &'a [String],
    use_count: u64,
    last_used: String,
    created_at: String,
}

impl<'a> From<&'a Favorite> for FavoriteJson<'a> {
    fn from(f: &'a Favorite) -> Self {
        Self {
            alias: &f.alias,
            source: &f.source,
            source_type: f.source_type.display_name(),
            description: f.description.as_deref(),
            tags: &f.tags,
            use_count: f.stats.use_count,
            last_used: f.stats.last_used.to_rfc3339(),
            created_at: f.stats.created_at.to_rfc3339(),
        }
    }
}

/// Execute a favorites subcommand
#[allow(clippy::unused_async)]
pub async fn execute(command: FavoritesCommands, format: OutputFormat) -> Result<()> {
    match command {
        FavoritesCommands::List => cmd_list(format),
        FavoritesCommands::Add {
            source,
            alias,
            description,
            tags,
        } => cmd_add(&source, &alias, description.as_deref(), tags.as_deref()),
        FavoritesCommands::Remove { alias } => cmd_remove(&alias),
        FavoritesCommands::Use { alias } => cmd_use(&alias),
    }
}

/// List all favorites sorted by usage count (descending)
fn cmd_list(format: OutputFormat) -> Result<()> {
    let store = FavoritesStore::load();
    let sorted = store.sorted_by_usage();

    match format {
        OutputFormat::Json => {
            let entries: Vec<FavoriteJson<'_>> =
                sorted.iter().map(|f| FavoriteJson::from(*f)).collect();
            let json = serde_json::to_string_pretty(&entries)
                .context("Failed to serialize favorites as JSON")?;
            println!("{json}");
        }
        OutputFormat::Text | OutputFormat::Csv | OutputFormat::Markdown => {
            if sorted.is_empty() {
                println!(
                    "No favorites yet. Add one with 'ainb favorites add <source> --alias <name>'."
                );
                return Ok(());
            }

            println!("Favorites (sorted by usage):");
            println!("{}", "\u{2501}".repeat(72));

            for fav in sorted {
                let tags = if fav.tags.is_empty() {
                    String::new()
                } else {
                    format!(" [{}]", fav.tags.join(", "))
                };

                let desc =
                    fav.description.as_deref().map(|d| format!(" — {d}")).unwrap_or_default();

                println!(
                    "  {alias} ({kind})  uses={count}{tags}",
                    alias = fav.alias,
                    kind = fav.source_type.display_name(),
                    count = fav.stats.use_count,
                    tags = tags,
                );
                println!("      source: {}{desc}", fav.source);
            }

            println!();
            println!("Total: {} favorite(s)", store.len());
        }
    }

    Ok(())
}

/// Add a new favorite, auto-detecting source type from the input string
fn cmd_add(source: &str, alias: &str, description: Option<&str>, tags: Option<&str>) -> Result<()> {
    if alias.trim().is_empty() {
        return Err(anyhow!("Alias cannot be empty"));
    }
    if source.trim().is_empty() {
        return Err(anyhow!("Source cannot be empty"));
    }

    let source_type = detect_source_type(source);

    let mut store = FavoritesStore::load();

    let mut fav = Favorite::new(alias.to_string(), source.to_string(), source_type.clone());
    fav.description = description.map(str::to_string);
    fav.tags = parse_tags(tags);

    store.add(fav).map_err(|e| anyhow!("Failed to add favorite '{alias}': {e}"))?;

    store.save().context("Failed to save favorites store")?;

    println!(
        "Added favorite '{alias}' ({kind}) → {source}",
        kind = source_type.display_name(),
    );

    Ok(())
}

/// Remove a favorite by alias
fn cmd_remove(alias: &str) -> Result<()> {
    let mut store = FavoritesStore::load();

    let removed = store
        .remove(alias)
        .ok_or_else(|| anyhow!("No favorite found with alias '{alias}'"))?;

    store.save().context("Failed to save favorites store")?;

    println!("Removed favorite '{}' ({})", removed.alias, removed.source);
    Ok(())
}

/// Record usage of a favorite
fn cmd_use(alias: &str) -> Result<()> {
    let mut store = FavoritesStore::load();

    if !store.has_alias(alias) {
        return Err(anyhow!("No favorite found with alias '{alias}'"));
    }

    store.record_use(alias);
    store.save().context("Failed to save favorites store")?;

    let count = store.get(alias).map(|f| f.stats.use_count).unwrap_or(0);
    println!("Recorded use of '{alias}' (use_count = {count})");

    Ok(())
}

// --- helpers ---

/// Detect the source type from a raw input string.
///
/// Order of checks:
/// 1. HTTPS / HTTP URL → `HttpsUrl`
/// 2. SSH URL (git@... or scp-like with `::`) → `SshUrl`
/// 3. Existing local filesystem path → `LocalPath`
/// 4. `owner/repo` shorthand (single slash, no whitespace, no protocol) → `GithubShorthand`
/// 5. Fallback: treat as `LocalPath` (the user gave us something path-like)
pub(crate) fn detect_source_type(input: &str) -> SourceType {
    let trimmed = input.trim();

    if trimmed.starts_with("https://") || trimmed.starts_with("http://") {
        return SourceType::HttpsUrl;
    }

    if trimmed.starts_with("git@") || trimmed.contains("::") {
        return SourceType::SshUrl;
    }

    // If it points at an existing path on disk, prefer LocalPath.
    if Path::new(trimmed).exists() {
        return SourceType::LocalPath;
    }

    if is_github_shorthand(trimmed) {
        return SourceType::GithubShorthand;
    }

    // Fallback: assume it was meant as a path the user will create.
    SourceType::LocalPath
}

/// True if input looks like `owner/repo` GitHub shorthand: exactly one `/`,
/// non-empty segments, no whitespace, no scheme markers.
fn is_github_shorthand(input: &str) -> bool {
    if input.contains(char::is_whitespace) {
        return false;
    }
    if input.contains("://")
        || input.starts_with('/')
        || input.starts_with('.')
        || input.starts_with('~')
    {
        return false;
    }

    let parts: Vec<&str> = input.split('/').collect();
    if parts.len() != 2 {
        return false;
    }
    !parts[0].is_empty() && !parts[1].is_empty()
}

/// Parse a comma-separated tag string into a vector of trimmed, non-empty tags.
fn parse_tags(raw: Option<&str>) -> Vec<String> {
    raw.map(|s| {
        s.split(',')
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(str::to_string)
            .collect()
    })
    .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- detect_source_type tests ---

    #[test]
    fn test_detect_https_url() {
        assert_eq!(
            detect_source_type("https://github.com/anthropics/claude-code"),
            SourceType::HttpsUrl
        );
        assert_eq!(
            detect_source_type("http://example.com/repo.git"),
            SourceType::HttpsUrl
        );
    }

    #[test]
    fn test_detect_ssh_url_git_at() {
        assert_eq!(
            detect_source_type("git@github.com:anthropics/claude-code.git"),
            SourceType::SshUrl
        );
    }

    #[test]
    fn test_detect_ssh_url_double_colon() {
        // scp-like / rsync-style remote with `::`
        assert_eq!(
            detect_source_type("rsync.example.com::module/path"),
            SourceType::SshUrl
        );
    }

    #[test]
    fn test_detect_github_shorthand() {
        assert_eq!(
            detect_source_type("anthropics/claude-code"),
            SourceType::GithubShorthand
        );
        assert_eq!(
            detect_source_type("rust-lang/rust"),
            SourceType::GithubShorthand
        );
    }

    #[test]
    fn test_detect_local_path_existing() {
        // /tmp exists on every Unix system used in CI.
        let st = detect_source_type("/tmp");
        assert_eq!(st, SourceType::LocalPath);
    }

    #[test]
    fn test_detect_fallback_to_local_path() {
        // No slash, not a URL, doesn't exist → fallback LocalPath.
        assert_eq!(
            detect_source_type("not-a-real-thing-xyz"),
            SourceType::LocalPath
        );
    }

    #[test]
    fn test_detect_three_segments_not_shorthand() {
        // Three slashes is not GitHub shorthand. Falls through to local path.
        assert_eq!(detect_source_type("a/b/c"), SourceType::LocalPath);
    }

    #[test]
    fn test_is_github_shorthand_rejects_whitespace() {
        assert!(!is_github_shorthand("owner /repo"));
        assert!(!is_github_shorthand("owner/ repo"));
    }

    #[test]
    fn test_is_github_shorthand_rejects_empty_segments() {
        assert!(!is_github_shorthand("/repo"));
        assert!(!is_github_shorthand("owner/"));
        assert!(!is_github_shorthand("/"));
    }

    #[test]
    fn test_is_github_shorthand_rejects_path_like() {
        assert!(!is_github_shorthand("./local/repo"));
        assert!(!is_github_shorthand("~/repo"));
    }

    // --- parse_tags tests ---

    #[test]
    fn test_parse_tags_none() {
        assert!(parse_tags(None).is_empty());
    }

    #[test]
    fn test_parse_tags_empty_string() {
        assert!(parse_tags(Some("")).is_empty());
    }

    #[test]
    fn test_parse_tags_single() {
        assert_eq!(parse_tags(Some("frontend")), vec!["frontend".to_string()]);
    }

    #[test]
    fn test_parse_tags_multiple_with_whitespace() {
        assert_eq!(
            parse_tags(Some("frontend, react ,  ui")),
            vec![
                "frontend".to_string(),
                "react".to_string(),
                "ui".to_string()
            ]
        );
    }

    #[test]
    fn test_parse_tags_filters_empty() {
        assert_eq!(
            parse_tags(Some("a,,b,  ,c")),
            vec!["a".to_string(), "b".to_string(), "c".to_string()]
        );
    }

    // --- store-level round-trip tests (in-memory) ---

    #[test]
    fn test_add_and_remove_round_trip_in_store() {
        let mut store = FavoritesStore::default();

        let fav = Favorite::new(
            "claude".to_string(),
            "anthropics/claude-code".to_string(),
            detect_source_type("anthropics/claude-code"),
        );
        store.add(fav).unwrap();

        assert!(store.has_alias("claude"));
        assert_eq!(store.len(), 1);
        assert_eq!(
            store.get("claude").unwrap().source_type,
            SourceType::GithubShorthand
        );

        let removed = store.remove("claude");
        assert!(removed.is_some());
        assert_eq!(store.len(), 0);
        assert!(!store.has_alias("claude"));
    }

    #[test]
    fn test_sorted_by_usage_descending() {
        let mut store = FavoritesStore::default();

        store
            .add(Favorite::new(
                "low".to_string(),
                "owner/low".to_string(),
                SourceType::GithubShorthand,
            ))
            .unwrap();
        store
            .add(Favorite::new(
                "mid".to_string(),
                "owner/mid".to_string(),
                SourceType::GithubShorthand,
            ))
            .unwrap();
        store
            .add(Favorite::new(
                "high".to_string(),
                "owner/high".to_string(),
                SourceType::GithubShorthand,
            ))
            .unwrap();

        store.record_use("low");
        store.record_use("mid");
        store.record_use("mid");
        store.record_use("high");
        store.record_use("high");
        store.record_use("high");

        let sorted = store.sorted_by_usage();
        assert_eq!(sorted[0].alias, "high");
        assert_eq!(sorted[1].alias, "mid");
        assert_eq!(sorted[2].alias, "low");
    }

    #[test]
    fn test_record_use_increments_count() {
        let mut store = FavoritesStore::default();

        store
            .add(Favorite::new(
                "test".to_string(),
                "owner/repo".to_string(),
                SourceType::GithubShorthand,
            ))
            .unwrap();

        let initial_last_used = store.get("test").unwrap().stats.last_used;

        // Sleep to ensure last_used timestamp moves forward
        std::thread::sleep(std::time::Duration::from_millis(2));
        store.record_use("test");

        let fav = store.get("test").unwrap();
        assert_eq!(fav.stats.use_count, 1);
        assert!(fav.stats.last_used >= initial_last_used);
    }

    #[test]
    fn test_favorite_json_serialization() {
        let mut fav = Favorite::new(
            "react".to_string(),
            "facebook/react".to_string(),
            SourceType::GithubShorthand,
        );
        fav.description = Some("UI library".to_string());
        fav.tags = vec!["frontend".to_string(), "ui".to_string()];

        let json_view = FavoriteJson::from(&fav);
        let s = serde_json::to_string(&json_view).unwrap();

        assert!(s.contains("\"alias\":\"react\""));
        assert!(s.contains("\"source\":\"facebook/react\""));
        assert!(s.contains("\"source_type\":\"GitHub\""));
        assert!(s.contains("\"description\":\"UI library\""));
        assert!(s.contains("\"frontend\""));
        assert!(s.contains("\"use_count\":0"));
    }

    #[test]
    fn test_add_duplicate_alias_fails() {
        let mut store = FavoritesStore::default();

        store
            .add(Favorite::new(
                "dup".to_string(),
                "owner/a".to_string(),
                SourceType::GithubShorthand,
            ))
            .unwrap();

        let result = store.add(Favorite::new(
            "dup".to_string(),
            "owner/b".to_string(),
            SourceType::GithubShorthand,
        ));
        assert!(result.is_err());
    }
}
