// ABOUTME: Repository source detection and URL parsing for remote and local repos

use std::path::PathBuf;
use thiserror::Error;

/// Represents the source of a git repository - either remote (URL) or local (path)
#[derive(Debug, Clone, PartialEq)]
pub enum RepoSource {
    /// HTTPS URL (https://github.com/user/repo)
    HttpsUrl(String),
    /// SSH URL (git@github.com:user/repo.git)
    SshUrl(String),
    /// Local filesystem path
    LocalPath(PathBuf),
    /// GitHub shorthand (user/repo) - expands to HTTPS
    GithubShorthand { owner: String, repo: String },
}

/// Parsed repository components for cache path generation
#[derive(Debug, Clone)]
pub struct ParsedRepo {
    pub source: RepoSource,
    pub host: String,
    pub owner: String,
    pub repo_name: String,
}

#[derive(Error, Debug)]
pub enum RepoSourceError {
    #[error("Invalid URL format: {0}")]
    InvalidUrl(String),
    #[error("Path does not exist: {0}")]
    PathNotFound(String),
    #[error("Path is not a git repository: {0}")]
    NotGitRepo(String),
    #[error("Unable to parse repository: {0}")]
    ParseError(String),
}

impl RepoSource {
    /// Classify user input into appropriate RepoSource variant
    pub fn from_input(input: &str) -> Result<Self, RepoSourceError> {
        let input = input.trim();

        if input.is_empty() {
            return Err(RepoSourceError::ParseError("Empty input".to_string()));
        }

        // HTTPS URL
        if input.starts_with("https://") || input.starts_with("http://") {
            return Ok(RepoSource::HttpsUrl(normalize_url(input)));
        }

        // SSH URL (git@host:path or ssh://git@host/path)
        if input.starts_with("git@") || input.starts_with("ssh://") {
            return Ok(RepoSource::SshUrl(input.to_string()));
        }

        // Local path (absolute or home-relative)
        if input.starts_with('/') || input.starts_with('~') {
            let expanded = expand_tilde(input);
            return Ok(RepoSource::LocalPath(expanded));
        }

        // GitHub shorthand: owner/repo (no spaces, exactly one slash, no protocol)
        if !input.contains(' ')
            && input.matches('/').count() == 1
            && !input.contains(':')
            && !input.contains('.')
        {
            let parts: Vec<&str> = input.split('/').collect();
            if parts.len() == 2 && !parts[0].is_empty() && !parts[1].is_empty() {
                return Ok(RepoSource::GithubShorthand {
                    owner: parts[0].to_string(),
                    repo: parts[1].trim_end_matches(".git").to_string(),
                });
            }
        }

        // Check if it looks like a URL with a domain (e.g., gitlab.com/user/repo)
        if input.contains('/') && input.contains('.') && !input.starts_with('.') {
            // Likely a URL without protocol - add https://
            return Ok(RepoSource::HttpsUrl(format!("https://{}", input)));
        }

        // Fallback: treat as local path
        Ok(RepoSource::LocalPath(PathBuf::from(input)))
    }

    /// Convert to canonical git URL for cloning
    pub fn to_clone_url(&self) -> String {
        match self {
            RepoSource::HttpsUrl(url) => ensure_git_suffix(url),
            RepoSource::SshUrl(url) => url.clone(),
            RepoSource::LocalPath(path) => path.display().to_string(),
            RepoSource::GithubShorthand { owner, repo } => {
                format!("https://github.com/{}/{}.git", owner, repo)
            }
        }
    }

    /// Check if this is a remote source (requires cloning)
    pub fn is_remote(&self) -> bool {
        matches!(
            self,
            RepoSource::HttpsUrl(_) | RepoSource::SshUrl(_) | RepoSource::GithubShorthand { .. }
        )
    }

    /// Get a display-friendly name for the repo
    pub fn display_name(&self) -> String {
        match self {
            RepoSource::HttpsUrl(url) => {
                // Extract owner/repo from URL
                if let Ok(parsed) = self.parse_components() {
                    format!("{}/{}", parsed.owner, parsed.repo_name)
                } else {
                    url.clone()
                }
            }
            RepoSource::SshUrl(url) => {
                if let Ok(parsed) = self.parse_components() {
                    format!("{}/{}", parsed.owner, parsed.repo_name)
                } else {
                    url.clone()
                }
            }
            RepoSource::LocalPath(path) => path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("unknown")
                .to_string(),
            RepoSource::GithubShorthand { owner, repo } => {
                format!("{}/{}", owner, repo)
            }
        }
    }

    /// Extract host/owner/repo for cache path generation
    pub fn parse_components(&self) -> Result<ParsedRepo, RepoSourceError> {
        match self {
            RepoSource::HttpsUrl(url) => parse_https_url(url, self.clone()),
            RepoSource::SshUrl(url) => parse_ssh_url(url, self.clone()),
            RepoSource::GithubShorthand { owner, repo } => Ok(ParsedRepo {
                source: self.clone(),
                host: "github.com".to_string(),
                owner: owner.clone(),
                repo_name: repo.clone(),
            }),
            RepoSource::LocalPath(path) => {
                let repo_name = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("unknown")
                    .to_string();
                Ok(ParsedRepo {
                    source: self.clone(),
                    host: "local".to_string(),
                    owner: String::new(),
                    repo_name,
                })
            }
        }
    }
}

/// Expand ~ to home directory
fn expand_tilde(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix('~') {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest.trim_start_matches('/'));
        }
    }
    PathBuf::from(path)
}

/// Normalize URL (remove trailing slashes, etc.)
fn normalize_url(url: &str) -> String {
    url.trim_end_matches('/').to_string()
}

/// Ensure URL ends with .git for cloning
fn ensure_git_suffix(url: &str) -> String {
    if url.ends_with(".git") {
        url.to_string()
    } else {
        format!("{}.git", url)
    }
}

/// Parse HTTPS URL into components
fn parse_https_url(url: &str, source: RepoSource) -> Result<ParsedRepo, RepoSourceError> {
    // https://github.com/owner/repo.git or https://github.com/owner/repo
    let url_clean = url
        .trim_end_matches(".git")
        .trim_end_matches('/');

    // Remove protocol
    let without_protocol = url_clean
        .strip_prefix("https://")
        .or_else(|| url_clean.strip_prefix("http://"))
        .unwrap_or(url_clean);

    let parts: Vec<&str> = without_protocol.split('/').collect();

    if parts.len() >= 3 {
        let host = parts[0].to_string();
        let owner = parts[1].to_string();
        let repo_name = parts[2].to_string();

        Ok(ParsedRepo {
            source,
            host,
            owner,
            repo_name,
        })
    } else {
        Err(RepoSourceError::InvalidUrl(format!(
            "Cannot parse URL: {}",
            url
        )))
    }
}

/// Parse SSH URL into components
fn parse_ssh_url(url: &str, source: RepoSource) -> Result<ParsedRepo, RepoSourceError> {
    // git@github.com:owner/repo.git or ssh://git@github.com/owner/repo.git
    let url_clean = url.trim_end_matches(".git");

    // Handle ssh:// prefix
    let url_normalized = if let Some(rest) = url_clean.strip_prefix("ssh://") {
        rest.to_string()
    } else {
        url_clean.to_string()
    };

    // git@github.com:owner/repo format
    if let Some(at_pos) = url_normalized.find('@') {
        let after_at = &url_normalized[at_pos + 1..];

        // Could be : or / separator depending on format
        let (host, path) = if let Some(colon_pos) = after_at.find(':') {
            (
                after_at[..colon_pos].to_string(),
                after_at[colon_pos + 1..].to_string(),
            )
        } else if let Some(slash_pos) = after_at.find('/') {
            (
                after_at[..slash_pos].to_string(),
                after_at[slash_pos + 1..].to_string(),
            )
        } else {
            return Err(RepoSourceError::InvalidUrl(format!(
                "Cannot parse SSH URL: {}",
                url
            )));
        };

        let path_parts: Vec<&str> = path.split('/').collect();
        if path_parts.len() >= 2 {
            return Ok(ParsedRepo {
                source,
                host,
                owner: path_parts[0].to_string(),
                repo_name: path_parts[1].to_string(),
            });
        }
    }

    Err(RepoSourceError::InvalidUrl(format!(
        "Cannot parse SSH URL: {}",
        url
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_https_url_detection() {
        let source = RepoSource::from_input("https://github.com/user/repo").unwrap();
        assert!(matches!(source, RepoSource::HttpsUrl(_)));
        assert!(source.is_remote());
    }

    #[test]
    fn test_https_url_with_git_suffix() {
        let source = RepoSource::from_input("https://github.com/user/repo.git").unwrap();
        assert!(matches!(source, RepoSource::HttpsUrl(_)));
        assert!(source.is_remote());
    }

    #[test]
    fn test_ssh_url_detection() {
        let source = RepoSource::from_input("git@github.com:user/repo.git").unwrap();
        assert!(matches!(source, RepoSource::SshUrl(_)));
        assert!(source.is_remote());
    }

    #[test]
    fn test_ssh_url_without_git_suffix() {
        let source = RepoSource::from_input("git@github.com:user/repo").unwrap();
        assert!(matches!(source, RepoSource::SshUrl(_)));
        assert!(source.is_remote());
    }

    #[test]
    fn test_local_path_absolute() {
        let source = RepoSource::from_input("/Users/test/repo").unwrap();
        assert!(matches!(source, RepoSource::LocalPath(_)));
        assert!(!source.is_remote());
    }

    #[test]
    fn test_local_path_tilde() {
        let source = RepoSource::from_input("~/projects/repo").unwrap();
        if let RepoSource::LocalPath(path) = source {
            assert!(!path.to_string_lossy().contains('~'));
            assert!(path.to_string_lossy().contains("projects/repo"));
        } else {
            panic!("Expected LocalPath");
        }
    }

    #[test]
    fn test_github_shorthand() {
        let source = RepoSource::from_input("user/repo").unwrap();
        assert!(matches!(source, RepoSource::GithubShorthand { .. }));
        assert!(source.is_remote());
        assert_eq!(source.to_clone_url(), "https://github.com/user/repo.git");
    }

    #[test]
    fn test_github_shorthand_display() {
        let source = RepoSource::from_input("anthropics/claude-code").unwrap();
        assert_eq!(source.display_name(), "anthropics/claude-code");
    }

    #[test]
    fn test_parse_https_components() {
        let source = RepoSource::from_input("https://github.com/rust-lang/rust").unwrap();
        let parsed = source.parse_components().unwrap();
        assert_eq!(parsed.host, "github.com");
        assert_eq!(parsed.owner, "rust-lang");
        assert_eq!(parsed.repo_name, "rust");
    }

    #[test]
    fn test_parse_ssh_components() {
        let source = RepoSource::from_input("git@github.com:anthropics/claude.git").unwrap();
        let parsed = source.parse_components().unwrap();
        assert_eq!(parsed.host, "github.com");
        assert_eq!(parsed.owner, "anthropics");
        assert_eq!(parsed.repo_name, "claude");
    }

    #[test]
    fn test_gitlab_url() {
        let source = RepoSource::from_input("https://gitlab.com/group/project").unwrap();
        let parsed = source.parse_components().unwrap();
        assert_eq!(parsed.host, "gitlab.com");
        assert_eq!(parsed.owner, "group");
        assert_eq!(parsed.repo_name, "project");
    }

    #[test]
    fn test_url_without_protocol() {
        let source = RepoSource::from_input("gitlab.com/user/repo").unwrap();
        assert!(matches!(source, RepoSource::HttpsUrl(_)));
        assert!(source.to_clone_url().starts_with("https://"));
    }

    #[test]
    fn test_empty_input_error() {
        let result = RepoSource::from_input("");
        assert!(result.is_err());
    }

    #[test]
    fn test_whitespace_trimming() {
        let source = RepoSource::from_input("  https://github.com/user/repo  ").unwrap();
        assert!(matches!(source, RepoSource::HttpsUrl(_)));
    }

    #[test]
    fn test_clone_url_adds_git_suffix() {
        let source = RepoSource::from_input("https://github.com/user/repo").unwrap();
        assert!(source.to_clone_url().ends_with(".git"));
    }

    #[test]
    fn test_clone_url_preserves_existing_git_suffix() {
        let source = RepoSource::from_input("https://github.com/user/repo.git").unwrap();
        let url = source.to_clone_url();
        assert!(url.ends_with(".git"));
        assert!(!url.ends_with(".git.git"));
    }
}
