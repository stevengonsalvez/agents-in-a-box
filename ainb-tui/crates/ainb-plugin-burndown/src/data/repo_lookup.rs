// ABOUTME: Resolve a working directory to its upstream repo identifier
// (e.g. "owner/repo") by reading .git/config without shelling out.
//
// Used by usage aggregation to collapse worktrees of the same repo into
// a single ProjectUsage row keyed by the upstream remote.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

/// Resolve a working directory to its upstream repo identifier
/// (e.g. `"owner/repo"`).
///
/// Returns `None` for non-git paths or repos without an `origin`
/// remote. Failures are silent — callers fall back to the local folder
/// name. Results are memoised in `cache` keyed by the input `cwd`
/// string.
///
/// Implementation notes:
/// - Walks up from `cwd` looking for `.git` (file or directory).
/// - When `.git` is a file (worktree pointer of the form
///   `gitdir: <path>`), follows the pointer to the main repo's git
///   directory. Worktree gitdirs typically nest under
///   `<main>/.git/worktrees/<name>/`; we strip down to the repo's
///   top-level git dir before reading `config` so all worktrees of a
///   repo collapse to the same remote.
/// - Parses `.git/config` as INI: looks for the `[remote "origin"]`
///   section and reads its `url = ...` entry.
/// - Extracts `owner/repo` from common URL shapes (HTTPS, SSH,
///   `ssh://`, `git://`, scp-style `user@host:path`). The full
///   path-after-host is preserved so nested GitLab groups
///   (`group/sub/repo`) are kept intact. Trailing `.git` is stripped.
pub fn resolve_repo(cwd: &str, cache: &mut HashMap<String, Option<String>>) -> Option<String> {
    if let Some(hit) = cache.get(cwd) {
        return hit.clone();
    }
    let resolved = resolve_repo_uncached(Path::new(cwd));
    cache.insert(cwd.to_string(), resolved.clone());
    resolved
}

fn resolve_repo_uncached(start: &Path) -> Option<String> {
    let git_dir = find_git_dir(start)?;
    let config_path = git_dir.join("config");
    let config_text = fs::read_to_string(&config_path).ok()?;
    let url = parse_origin_url(&config_text)?;
    extract_owner_repo(&url)
}

/// Walk up from `start` to find a `.git` entry. If `.git` is a file
/// (worktree pointer), resolve it to the main repo's top-level git
/// directory. Returns the directory containing the `config` file we
/// should read.
fn find_git_dir(start: &Path) -> Option<PathBuf> {
    let mut cur = if start.is_absolute() {
        start.to_path_buf()
    } else {
        // Best-effort canonicalize; if cwd is missing, fall back to
        // the path as given so we can still walk parents.
        fs::canonicalize(start).unwrap_or_else(|_| start.to_path_buf())
    };

    loop {
        let candidate = cur.join(".git");
        if let Ok(meta) = fs::metadata(&candidate) {
            if meta.is_dir() {
                return Some(candidate);
            }
            if meta.is_file() {
                if let Some(gitdir) = read_gitdir_pointer(&candidate) {
                    return Some(normalize_worktree_gitdir(&gitdir));
                }
                return None;
            }
        }
        if !cur.pop() {
            return None;
        }
    }
}

/// Parse a `.git` pointer file (`gitdir: <path>`).
fn read_gitdir_pointer(path: &Path) -> Option<PathBuf> {
    let content = fs::read_to_string(path).ok()?;
    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("gitdir:") {
            let target = rest.trim();
            if target.is_empty() {
                return None;
            }
            let target_path = PathBuf::from(target);
            // Pointer paths can be relative to the directory holding
            // the `.git` file.
            if target_path.is_absolute() {
                return Some(target_path);
            }
            let parent = path.parent()?;
            return Some(parent.join(target_path));
        }
    }
    None
}

/// Worktree gitdirs typically look like
/// `<main>/.git/worktrees/<name>`. The shared `config` lives at
/// `<main>/.git/config`. Walk up until we find a directory that
/// contains a `config` file (the canonical git dir layout).
fn normalize_worktree_gitdir(gitdir: &Path) -> PathBuf {
    let mut cur = gitdir.to_path_buf();
    // The top-level git dir is always called `.git` and contains
    // `config`. Step up while we're inside `worktrees/...`.
    loop {
        if cur.join("config").is_file() && cur.file_name().and_then(|s| s.to_str()) == Some(".git")
        {
            return cur;
        }
        if !cur.pop() {
            return gitdir.to_path_buf();
        }
    }
}

/// Parse a tiny subset of INI: locate `[remote "origin"]` and return
/// its `url = ...` value. We don't pull a full INI crate because git
/// config has its own quirks (subsections, includes) and we only need
/// one field.
fn parse_origin_url(config: &str) -> Option<String> {
    let mut in_origin = false;
    for raw_line in config.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            // Section header. Match `[remote "origin"]` with flexible
            // whitespace; reject anything else.
            in_origin = is_remote_origin_header(line);
            continue;
        }
        if !in_origin {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            if key.trim().eq_ignore_ascii_case("url") {
                let v = value.trim().trim_matches('"');
                if !v.is_empty() {
                    return Some(v.to_string());
                }
            }
        }
    }
    None
}

fn is_remote_origin_header(line: &str) -> bool {
    // Strip the [ ] and normalize whitespace.
    let inner = &line[1..line.len() - 1].trim();
    // Expected shape: `remote "origin"` (subsection name in quotes).
    let Some(rest) = inner.strip_prefix("remote") else {
        return false;
    };
    let rest = rest.trim_start();
    rest == "\"origin\""
}

/// Extract `owner/repo` (or full path-after-host) from a git URL.
/// Returns `None` for unrecognised shapes.
fn extract_owner_repo(url: &str) -> Option<String> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return None;
    }

    // Order matters: check explicit schemes first, then fall back to
    // scp-style `user@host:path`.
    let path = if let Some(rest) = strip_scheme(trimmed) {
        // After scheme, rest is `[user@]host[:port]/path`. Skip the
        // host segment.
        let after_host = rest.split_once('/')?.1;
        after_host.to_string()
    } else if let Some(after_colon) = scp_style_path(trimmed) {
        after_colon.to_string()
    } else {
        return None;
    };

    let path = path.trim_start_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let path = path.trim_end_matches('/');

    // Require at least `owner/repo` (one slash). A bare `repo` with no
    // namespace can't be attributed to an upstream id.
    if !path.contains('/') {
        return None;
    }
    if path.is_empty() {
        return None;
    }
    Some(path.to_string())
}

/// Recognise `scheme://` prefixes and return the remainder. Returns
/// `None` for inputs without an explicit scheme.
fn strip_scheme(url: &str) -> Option<&str> {
    for scheme in ["https://", "http://", "ssh://", "git://", "git+ssh://"] {
        if let Some(rest) = url.strip_prefix(scheme) {
            return Some(rest);
        }
    }
    None
}

/// Match scp-style `[user@]host:path` (e.g. `git@github.com:owner/repo.git`).
/// Returns the path portion or `None` if the input doesn't look like
/// scp form.
fn scp_style_path(url: &str) -> Option<&str> {
    // scp-style has no `//` and the first `:` separates host from path.
    if url.contains("://") {
        return None;
    }
    let (host, path) = url.split_once(':')?;
    if host.is_empty() || path.is_empty() {
        return None;
    }
    // Reject Windows-style drive letters like `C:\foo`.
    if host.len() == 1 && host.chars().next().is_some_and(|c| c.is_ascii_alphabetic()) {
        return None;
    }
    Some(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn parses_ssh_scp_style_url() {
        assert_eq!(
            extract_owner_repo("git@github.com:owner/repo.git"),
            Some("owner/repo".to_string())
        );
    }

    #[test]
    fn parses_https_url_with_dot_git() {
        assert_eq!(
            extract_owner_repo("https://github.com/owner/repo.git"),
            Some("owner/repo".to_string())
        );
    }

    #[test]
    fn parses_https_url_without_dot_git() {
        assert_eq!(
            extract_owner_repo("https://github.com/owner/repo"),
            Some("owner/repo".to_string())
        );
    }

    #[test]
    fn parses_gitlab_nested_group() {
        assert_eq!(
            extract_owner_repo("https://gitlab.com/group/sub/repo"),
            Some("group/sub/repo".to_string())
        );
        assert_eq!(
            extract_owner_repo("git@gitlab.com:group/sub/repo.git"),
            Some("group/sub/repo".to_string())
        );
    }

    #[test]
    fn parses_ssh_scheme_url() {
        assert_eq!(
            extract_owner_repo("ssh://git@github.com/owner/repo.git"),
            Some("owner/repo".to_string())
        );
        assert_eq!(
            extract_owner_repo("ssh://git@host.example.com:2222/team/proj.git"),
            Some("team/proj".to_string())
        );
    }

    #[test]
    fn rejects_bare_repo_name() {
        // Need at least owner/repo — a bare name has no upstream id.
        assert_eq!(extract_owner_repo("git@github.com:repo.git"), None);
    }

    #[test]
    fn rejects_unknown_scheme() {
        assert_eq!(extract_owner_repo(""), None);
        // Local file path is not a remote URL we can attribute.
        assert_eq!(extract_owner_repo("/tmp/some/path"), None);
    }

    #[test]
    fn parses_origin_section_only() {
        let cfg = r#"
[core]
    repositoryformatversion = 0
[remote "upstream"]
    url = https://github.com/other/upstream.git
[remote "origin"]
    url = git@github.com:owner/repo.git
[branch "main"]
    remote = origin
"#;
        assert_eq!(
            parse_origin_url(cfg),
            Some("git@github.com:owner/repo.git".to_string())
        );
    }

    #[test]
    fn parse_origin_returns_none_when_no_origin() {
        let cfg = r#"
[core]
    repositoryformatversion = 0
[remote "upstream"]
    url = https://github.com/other/upstream.git
"#;
        assert_eq!(parse_origin_url(cfg), None);
    }

    #[test]
    fn resolve_returns_none_for_non_git_path() {
        let dir = tempdir().unwrap();
        let mut cache = HashMap::new();
        let cwd = dir.path().to_string_lossy().into_owned();
        assert_eq!(resolve_repo(&cwd, &mut cache), None);
        // Cache hit (negative) — second call should not re-resolve.
        assert!(cache.contains_key(&cwd));
        assert_eq!(resolve_repo(&cwd, &mut cache), None);
    }

    #[test]
    fn resolve_returns_none_when_no_origin_remote() {
        let dir = tempdir().unwrap();
        let git_dir = dir.path().join(".git");
        fs::create_dir(&git_dir).unwrap();
        fs::write(
            git_dir.join("config"),
            "[core]\n\trepositoryformatversion = 0\n",
        )
        .unwrap();

        let mut cache = HashMap::new();
        let cwd = dir.path().to_string_lossy().into_owned();
        assert_eq!(resolve_repo(&cwd, &mut cache), None);
    }

    #[test]
    fn resolve_finds_origin_in_repo_root() {
        let dir = tempdir().unwrap();
        let git_dir = dir.path().join(".git");
        fs::create_dir(&git_dir).unwrap();
        fs::write(
            git_dir.join("config"),
            "[remote \"origin\"]\n\turl = git@github.com:acme/widget.git\n",
        )
        .unwrap();

        let mut cache = HashMap::new();
        let cwd = dir.path().to_string_lossy().into_owned();
        assert_eq!(
            resolve_repo(&cwd, &mut cache),
            Some("acme/widget".to_string())
        );
    }

    #[test]
    fn resolve_walks_up_from_subdirectory() {
        let dir = tempdir().unwrap();
        let git_dir = dir.path().join(".git");
        fs::create_dir(&git_dir).unwrap();
        fs::write(
            git_dir.join("config"),
            "[remote \"origin\"]\n\turl = https://github.com/acme/widget\n",
        )
        .unwrap();
        let nested = dir.path().join("a").join("b");
        fs::create_dir_all(&nested).unwrap();

        let mut cache = HashMap::new();
        let cwd = nested.to_string_lossy().into_owned();
        assert_eq!(
            resolve_repo(&cwd, &mut cache),
            Some("acme/widget".to_string())
        );
    }

    #[test]
    fn resolve_follows_worktree_pointer_to_main_repo() {
        // Layout:
        //   <root>/main/.git/        (real git dir)
        //   <root>/main/.git/config  (with origin)
        //   <root>/main/.git/worktrees/wt1/   (worktree gitdir)
        //   <root>/wt1/.git          (file: "gitdir: <root>/main/.git/worktrees/wt1")
        let dir = tempdir().unwrap();
        let main = dir.path().join("main");
        let main_git = main.join(".git");
        let wt_gitdir = main_git.join("worktrees").join("wt1");
        fs::create_dir_all(&wt_gitdir).unwrap();
        fs::write(
            main_git.join("config"),
            "[remote \"origin\"]\n\turl = git@github.com:acme/widget.git\n",
        )
        .unwrap();

        let wt = dir.path().join("wt1");
        fs::create_dir_all(&wt).unwrap();
        fs::write(
            wt.join(".git"),
            format!("gitdir: {}\n", wt_gitdir.display()),
        )
        .unwrap();

        let mut cache = HashMap::new();
        let cwd = wt.to_string_lossy().into_owned();
        assert_eq!(
            resolve_repo(&cwd, &mut cache),
            Some("acme/widget".to_string())
        );
    }

    #[test]
    fn cache_hit_short_circuits_resolution() {
        let mut cache = HashMap::new();
        cache.insert(
            "/no/such/path".to_string(),
            Some("cached/value".to_string()),
        );
        // No filesystem access — value comes straight from cache.
        assert_eq!(
            resolve_repo("/no/such/path", &mut cache),
            Some("cached/value".to_string())
        );
    }

    #[test]
    fn rejects_scp_style_with_drive_letter() {
        // Ensure Windows paths don't get mistaken for scp URLs.
        assert_eq!(extract_owner_repo("C:\\users\\dev\\repo"), None);
    }
}
