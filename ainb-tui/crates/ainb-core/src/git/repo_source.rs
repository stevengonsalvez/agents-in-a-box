// ABOUTME: Repository source detection and URL parsing for remote and local repos

use std::path::{Path, PathBuf};

use lazy_static::lazy_static;
use regex::Regex;
use thiserror::Error;

/// Represents the source of a git repository - either remote (URL) or local (path)
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepoSource {
    /// HTTPS URL (https://github.com/user/repo)
    HttpsUrl(String),
    /// SSH URL for clone (git@github.com:user/repo.git)
    SshUrl(String),
    /// `ssh://user@host[:port]` with no repo segment — opens an interactive SSH
    /// session, NOT a clone. New-session screen 1 (smart-parse v2) introduces
    /// this variant to distinguish from `SshUrl`.
    SshSession(String),
    /// Local filesystem path
    LocalPath(PathBuf),
    /// GitHub shorthand (user/repo) - expands to HTTPS
    GithubShorthand { owner: String, repo: String },
    /// Unparseable input — pass through to the fuzzy filter on the picker list.
    /// New-session screen 1 (smart-parse v2) sink variant; never produced by
    /// the legacy `from_input` parser.
    Filter(String),
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
    /// Classify user input into appropriate `RepoSource` variant.
    ///
    /// **Status:** legacy (finding #14). New code should use [`parse_with`]
    /// (smart-parse v2) which returns an infallible typed variant —
    /// including the `Filter` sink for unparseable strings — instead of a
    /// `Result`. `from_input` survives only for callers that operate on
    /// already-validated git remote URLs (e.g., the output of
    /// `get_remote_url()`) where the fallible contract is meaningful and
    /// the `SshSession`/`Filter` variants are unreachable.
    ///
    /// Behavioural differences vs `parse_with`:
    /// - `from_input` accepts uppercase `owner/repo`; `parse_with` does not
    ///   (it rejects `Cargo/Cargo.toml`-style false positives).
    /// - `from_input` does NOT consult the filesystem; `parse_with` does.
    /// - `from_input` returns `Err(ParseError)` on empty input;
    ///   `parse_with` returns `Filter("")`.
    #[deprecated(
        since = "1.2.0",
        note = "Use `parse_with(input, &RealFs)` for new code; this remains for already-validated git remote URLs."
    )]
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

    /// Convert to canonical git URL for cloning.
    ///
    /// `SshSession` and `Filter` are non-clonable; their string form is
    /// returned as-is so callers that mis-route still get a sane debug value.
    pub fn to_clone_url(&self) -> String {
        match self {
            RepoSource::HttpsUrl(url) => ensure_git_suffix(url),
            RepoSource::SshUrl(url) => url.clone(),
            RepoSource::LocalPath(path) => path.display().to_string(),
            RepoSource::GithubShorthand { owner, repo } => {
                format!("https://github.com/{}/{}.git", owner, repo)
            }
            // Non-clonable variants — string passthrough for safe debug.
            RepoSource::SshSession(s) | RepoSource::Filter(s) => s.clone(),
        }
    }

    /// Check if this is a remote source (requires cloning).
    ///
    /// `SshSession` is interactive, NOT a clone target — excluded here.
    /// `Filter` is unparseable input — also excluded.
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
            RepoSource::LocalPath(path) => {
                path.file_name().and_then(|n| n.to_str()).unwrap_or("unknown").to_string()
            }
            RepoSource::GithubShorthand { owner, repo } => {
                format!("{}/{}", owner, repo)
            }
            // Non-clonable variants: passthrough so the picker can render them.
            RepoSource::SshSession(s) | RepoSource::Filter(s) => s.clone(),
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
                let repo_name =
                    path.file_name().and_then(|n| n.to_str()).unwrap_or("unknown").to_string();
                Ok(ParsedRepo {
                    source: self.clone(),
                    host: "local".to_string(),
                    owner: String::new(),
                    repo_name,
                })
            }
            // SshSession is an interactive session, not a clone — no components
            // to extract. Filter is unparseable text and likewise has no
            // owner/repo to expose.
            RepoSource::SshSession(s) | RepoSource::Filter(s) => Err(RepoSourceError::ParseError(
                format!("not a clonable repo: {s}"),
            )),
        }
    }
}

/// Trait abstracting filesystem existence checks so `parse_with` stays pure
/// and unit-testable.
pub trait FileExists {
    /// Return true iff `p` refers to an existing filesystem entry.
    fn exists(&self, p: &Path) -> bool;
}

/// Real-filesystem implementation backing production callers.
pub struct RealFs;

impl FileExists for RealFs {
    fn exists(&self, p: &Path) -> bool {
        p.exists()
    }
}

lazy_static! {
    /// `owner/repo` shorthand — lowercase only to avoid catching `Cargo/Cargo.toml`.
    static ref SHORTHAND_RE: Regex =
        Regex::new(r"^([a-z0-9-]+)/([a-z0-9._-]+)$").expect("shorthand regex compiles");
    /// `^https?://...`
    static ref HTTP_RE: Regex = Regex::new(r"^https?://").expect("http regex compiles");
    /// `git@host:owner/repo` (optional `.git`)
    static ref GIT_SSH_RE: Regex =
        Regex::new(r"^git@[^:]+:.+/.+(\.git)?$").expect("git ssh regex compiles");
    /// `ssh://host[/]` — host-only, NO repo segment. The trailing `/?$` rejects
    /// `ssh://host/path/to/repo.git` (which is a clone URL, not a session).
    static ref SSH_SESSION_RE: Regex =
        Regex::new(r"^ssh://[^/]+/?$").expect("ssh session regex compiles");
}

/// Smart-parse v2: classify raw user input into a typed `RepoSource`.
///
/// Pure function; the only side effect is the existence check delegated to
/// `fs`. Order matches the new-session screen 1 spec table:
/// 1. Absolute or `~`-prefixed path that exists → `LocalPath`
/// 2. `owner/repo` shorthand: local subdir under cwd wins if it exists,
///    otherwise `GithubShorthand`
/// 3. `^https?://...` → `HttpsUrl`
/// 4. `^git@host:owner/repo(.git)?$` → `SshUrl` (clone-able)
/// 5. `^ssh://host[/]$` (no repo segment) → `SshSession` (interactive, no clone)
/// 6. Everything else → `Filter` (sink for fuzzy-filter pass-through)
///
/// Unlike the legacy [`RepoSource::from_input`], this NEVER returns `Result`
/// — `Filter` is the catch-all. Callers cannot mis-handle the parsed value
/// because every input maps to a typed variant.
#[must_use]
pub fn parse_with(input: &str, fs: &dyn FileExists) -> RepoSource {
    // 1. Local path: absolute OR tilde-expanded, existing on disk.
    if input.starts_with('/') || input.starts_with('~') {
        let expanded = expand_tilde(input);
        if fs.exists(&expanded) {
            // Best-effort canonicalize for real filesystems; fall back to the
            // expanded path when canonicalize fails (e.g. test stubs whose
            // paths don't exist on the real disk).
            let canonical = expanded.canonicalize().unwrap_or(expanded);
            return RepoSource::LocalPath(canonical);
        }
    }

    // 2. `owner/repo` shorthand. Local-path-under-cwd takes precedence per
    //    spec rule (Cargo subdir wins over a plausible GitHub repo of the
    //    same name).
    if let Some(caps) = SHORTHAND_RE.captures(input) {
        let owner = caps.get(1).map_or("", |m| m.as_str()).to_string();
        let repo = caps.get(2).map_or("", |m| m.as_str()).to_string();
        if let Ok(cwd) = std::env::current_dir() {
            let candidate = cwd.join(input);
            if fs.exists(&candidate) {
                let canonical = candidate.canonicalize().unwrap_or(candidate);
                return RepoSource::LocalPath(canonical);
            }
        }
        return RepoSource::GithubShorthand { owner, repo };
    }

    // 3. `http(s)://...`
    if HTTP_RE.is_match(input) {
        return RepoSource::HttpsUrl(input.to_string());
    }

    // 4. `git@host:owner/repo(.git)?` — must come BEFORE the ssh:// check
    //    because `git@host:` paths don't start with `ssh://`.
    if GIT_SSH_RE.is_match(input) {
        return RepoSource::SshUrl(input.to_string());
    }

    // 5. `ssh://host[/]` — host-only, interactive session (NOT a clone).
    if SSH_SESSION_RE.is_match(input) {
        return RepoSource::SshSession(input.to_string());
    }

    // 6. Fall-through: filter text for the fuzzy picker.
    RepoSource::Filter(input.to_string())
}

/// Parse a `ssh://[user@]host[:port][/...]` URL into an `SshTarget`.
///
/// Canonical parser for the smart-parse v2 `SshSession` shape. Accepts a
/// trailing path/slash which is ignored — only the connection coords matter
/// for interactive sessions. Returns `None` if the URL doesn't carry a host
/// segment.
///
/// Examples:
/// - `ssh://prod-1.internal`        → host=prod-1.internal, port=22, user=None
/// - `ssh://deploy@prod-1`          → host=prod-1, port=22, user=Some("deploy")
/// - `ssh://deploy@prod-1:2222`     → host=prod-1, port=2222, user=Some("deploy")
/// - `ssh://prod-1/some/path`       → host=prod-1, port=22, user=None (path dropped)
#[must_use]
pub fn parse_ssh_session_url(url: &str) -> Option<crate::models::SshTarget> {
    let rest = url.strip_prefix("ssh://")?;
    // Trim trailing path — only the authority part contributes to the target.
    let authority = match rest.find('/') {
        Some(i) => &rest[..i],
        None => rest,
    };
    if authority.is_empty() {
        return None;
    }
    // user@host[:port]
    let (user, host_port) = match authority.find('@') {
        Some(i) => (Some(authority[..i].to_string()), &authority[i + 1..]),
        None => (None, authority),
    };
    if host_port.is_empty() {
        return None;
    }
    let (host, port) = match host_port.rfind(':') {
        Some(i) => {
            let port_str = &host_port[i + 1..];
            match port_str.parse::<u16>() {
                Ok(p) => (host_port[..i].to_string(), p),
                Err(_) => (host_port.to_string(), 22),
            }
        }
        None => (host_port.to_string(), 22),
    };
    if host.is_empty() {
        return None;
    }
    let mut target = crate::models::SshTarget::new(host).with_port(port);
    if let Some(u) = user {
        target = target.with_user(u);
    }
    Some(target)
}

/// Resolve the HEAD branch name of a local git repo. Best-effort — returns
/// `None` for non-local sources or when git2 cannot open the path.
#[must_use]
pub fn head_branch(path: &Path) -> Option<String> {
    let repo = git2::Repository::open(path).ok()?;
    let head = repo.head().ok()?;
    head.shorthand().map(str::to_string)
}

/// True when an HTTPS clone URL points at `github.com` (case-insensitive host
/// match). Used to gate the `gh auth status` pre-check: that probe is
/// GitHub-specific, so GitLab / Bitbucket / self-hosted HTTPS remotes must NOT
/// be routed through it (they'd hit an irrelevant GitHub auth screen). Returns
/// `false` for unparseable URLs and any non-GitHub host.
pub fn is_github_host(https_url: &str) -> bool {
    url::Url::parse(https_url)
        .ok()
        .and_then(|parsed| parsed.host_str().map(|h| h.eq_ignore_ascii_case("github.com")))
        .unwrap_or(false)
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
    let url_clean = url.trim_end_matches(".git").trim_end_matches('/');

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
#[allow(deprecated)] // existing legacy `from_input` tests pre-date finding #14
mod tests {
    use super::*;

    #[test]
    fn test_https_url_detection() {
        let source = RepoSource::from_input("https://github.com/user/repo").unwrap();
        assert!(matches!(source, RepoSource::HttpsUrl(_)));
        assert!(source.is_remote());
    }

    #[test]
    fn test_is_github_host_gates_the_auth_precheck() {
        // GitHub HTTPS — the only HTTPS host that should trigger `gh auth`.
        assert!(is_github_host("https://github.com/user/repo"));
        assert!(is_github_host("https://github.com/user/repo.git"));
        // Case-insensitive host match.
        assert!(is_github_host("https://GitHub.com/user/repo"));
        // Non-GitHub HTTPS remotes must NOT be routed through `gh auth status`.
        assert!(!is_github_host("https://gitlab.com/user/repo"));
        assert!(!is_github_host("https://bitbucket.org/user/repo"));
        assert!(!is_github_host("https://git.example.com/user/repo"));
        // A look-alike host that merely contains "github.com" is not GitHub.
        assert!(!is_github_host("https://github.com.evil.example/user/repo"));
        // Garbage / non-URL input fails closed.
        assert!(!is_github_host("not a url"));
        assert!(!is_github_host(""));
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

    // ------------------------------------------------------------------------
    // `parse_with` (smart-parse v2) — new-session screen 1 redesign Phase 3.
    // These tests pin the spec table in `plans/new-session-redesign-spec.md`.
    // The legacy `from_input` tests above remain authoritative for the legacy
    // parser; `parse_with` is a different contract (no Result, Filter sink,
    // injected FileExists for purity).
    // ------------------------------------------------------------------------

    use std::collections::HashSet;

    /// In-memory `FileExists` stub for unit tests.
    struct StubExists(HashSet<PathBuf>);

    impl FileExists for StubExists {
        fn exists(&self, p: &Path) -> bool {
            self.0.contains(p)
        }
    }

    fn stub<I: IntoIterator<Item = PathBuf>>(paths: I) -> StubExists {
        StubExists(paths.into_iter().collect())
    }

    #[test]
    fn parses_absolute_local_path_if_exists_via_parse_with() {
        let s = stub([PathBuf::from("/home/foo/repo")]);
        // The stub path doesn't exist on the real disk, so canonicalize falls
        // back to the expanded form — we get the input path back unchanged.
        assert_eq!(
            parse_with("/home/foo/repo", &s),
            RepoSource::LocalPath("/home/foo/repo".into())
        );
    }

    #[test]
    fn parses_tilde_local_path_if_expands_to_existing() {
        let home = dirs::home_dir().expect("home_dir resolves in test env");
        let s = stub([home.join("repo")]);
        let parsed = parse_with("~/repo", &s);
        assert!(matches!(parsed, RepoSource::LocalPath(_)), "got {parsed:?}");
        if let RepoSource::LocalPath(p) = parsed {
            assert_eq!(p, home.join("repo"));
        }
    }

    #[test]
    fn parses_owner_repo_as_github_shorthand_via_parse_with() {
        let s = stub([]);
        assert_eq!(
            parse_with("foo/bar", &s),
            RepoSource::GithubShorthand {
                owner: "foo".into(),
                repo: "bar".into()
            }
        );
    }

    #[test]
    fn local_path_takes_precedence_over_github_shorthand() {
        // Spec rule: if a local subdir matches `foo/bar`, use it instead of
        // treating the input as a GitHub clone candidate.
        let cwd = std::env::current_dir().expect("cwd resolves in test env");
        let candidate = cwd.join("foo/bar");
        let s = stub([candidate.clone()]);
        let parsed = parse_with("foo/bar", &s);
        // Real cwd exists, so canonicalize succeeds — compare against the
        // canonicalized form when possible, else fall back.
        let expected = candidate.canonicalize().unwrap_or(candidate);
        assert!(
            matches!(parsed, RepoSource::LocalPath(ref p) if p == &expected),
            "got {parsed:?}, expected LocalPath({expected:?})"
        );
    }

    #[test]
    fn parses_https_url_as_clone_via_parse_with() {
        let s = stub([]);
        assert!(matches!(
            parse_with("https://github.com/foo/bar.git", &s),
            RepoSource::HttpsUrl(_)
        ));
    }

    #[test]
    fn parses_git_ssh_url_as_clone_via_parse_with() {
        let s = stub([]);
        assert!(matches!(
            parse_with("git@github.com:foo/bar.git", &s),
            RepoSource::SshUrl(_)
        ));
    }

    #[test]
    fn parses_ssh_protocol_no_repo_as_session_not_clone() {
        // Spec: `ssh://user@host` (no repo path) opens an interactive session,
        // it does NOT clone. This is the key behavioural difference between
        // `SshUrl` (clone) and `SshSession` (interactive).
        let s = stub([]);
        let parsed = parse_with("ssh://deploy@prod-1.internal", &s);
        assert!(
            matches!(parsed, RepoSource::SshSession(_)),
            "got {parsed:?} — expected SshSession (host-only, no repo segment)"
        );
        // Negative assertion: must NOT be classified as a clone-able SshUrl.
        assert!(!matches!(parsed, RepoSource::SshUrl(_)));
    }

    #[test]
    fn unparseable_input_falls_through_to_filter() {
        let s = stub([]);
        assert_eq!(
            parse_with("just some text", &s),
            RepoSource::Filter("just some text".into())
        );
    }

    // ------------------------------------------------------------------------
    // `parse_ssh_session_url` — canonical SSH-session parser (finding #8).
    // ------------------------------------------------------------------------

    #[test]
    fn ssh_session_parses_host_only() {
        let t = parse_ssh_session_url("ssh://prod-1.internal").expect("host only");
        assert_eq!(t.host, "prod-1.internal");
        assert_eq!(t.port, 22);
        assert!(t.user.is_none());
    }

    #[test]
    fn ssh_session_parses_user_and_host() {
        let t = parse_ssh_session_url("ssh://deploy@prod-1").expect("user+host");
        assert_eq!(t.host, "prod-1");
        assert_eq!(t.port, 22);
        assert_eq!(t.user.as_deref(), Some("deploy"));
    }

    #[test]
    fn ssh_session_parses_user_host_port() {
        let t = parse_ssh_session_url("ssh://deploy@prod-1:2222").expect("user+host+port");
        assert_eq!(t.host, "prod-1");
        assert_eq!(t.port, 2222);
        assert_eq!(t.user.as_deref(), Some("deploy"));
    }

    #[test]
    fn ssh_session_default_port_when_omitted() {
        let t = parse_ssh_session_url("ssh://user@host").expect("default port");
        assert_eq!(t.port, 22);
    }

    #[test]
    fn ssh_session_drops_trailing_path() {
        let t = parse_ssh_session_url("ssh://prod-1/some/path").expect("trailing path");
        assert_eq!(t.host, "prod-1");
        assert_eq!(t.port, 22);
        assert!(t.user.is_none());
    }

    #[test]
    fn ssh_session_with_path_and_user() {
        let t = parse_ssh_session_url("ssh://alice@host:5022/srv/x").expect("path+user");
        assert_eq!(t.host, "host");
        assert_eq!(t.port, 5022);
        assert_eq!(t.user.as_deref(), Some("alice"));
    }

    #[test]
    fn ssh_session_rejects_malformed_input() {
        assert!(parse_ssh_session_url("prod-1").is_none());
        assert!(parse_ssh_session_url("ssh://").is_none());
        assert!(parse_ssh_session_url("ssh://user@").is_none());
    }

    #[test]
    fn shorthand_rejects_uppercase_to_avoid_false_positive() {
        // `Cargo/Cargo.toml` is far more likely to be a relative file path
        // the user typed than a GitHub repo — lowercase-only shorthand keeps
        // it out of `GithubShorthand`. (The legacy `from_input` does NOT
        // enforce this; `parse_with` is the stricter v2 contract.)
        let s = stub([]);
        assert_eq!(
            parse_with("Cargo/Cargo.toml", &s),
            RepoSource::Filter("Cargo/Cargo.toml".into())
        );
    }
}
