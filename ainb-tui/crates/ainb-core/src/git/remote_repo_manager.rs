// ABOUTME: Manages cloning and caching of remote git repositories
//
// CONCURRENCY / DATA SAFETY: the shared clone cache
// (`~/.agents-in-a-box/repos/<host>/<owner>/<repo>`) is written by every
// concurrently running ainb process on the machine, and live worktrees gitlink
// back into it, so deleting one orphans every worktree cut from it. A 2026-09
// incident wiped the whole cache this way: `clone_repo` did an UNLOCKED
// check-then-act, so two processes both saw a cold cache, both cloned into the
// same path, and the loser's "clean up my partial clone" `remove_dir_all` ate
// the winner's finished repo, which uncached it again and re-armed the race.
//
// Four rules keep that from recurring; keep all four when editing this file:
//   1. A clone lands in a private staging dir and is published with an atomic
//      `rename`. The only directory a failing clone may delete is its own.
//      This is the rule that makes concurrent cloning SAFE.
//   2. Check-then-clone runs under a per-repo advisory file lock, re-checking
//      the cache after acquiring it (double-checked locking). This rule only
//      makes it EFFICIENT, so it degrades to redundant work, never to failure,
//      on a filesystem that cannot lock. Anything that DELETES a shared path
//      must hold this lock, because rule 1 does not cover deletes.
//   3. Every deletion of a repo cache directory goes through
//      `CacheGuard::remove_repo_dir`, which refuses to leave the cache root and
//      refuses to delete a repo that still has live worktrees, failing CLOSED
//      when it cannot tell. (Worktree directories are a different object and
//      are removed directly at the `create_worktree_*` sites.)
//   4. Every destructive attempt leaves a receipt outside `~/.agents-in-a-box`,
//      so the evidence survives even a full wipe of that tree.

use anyhow::{Context, Result};
use fs2::FileExt;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use thiserror::Error;
use tracing::{debug, info, warn};

use super::repo_source::{ParsedRepo, RepoSource};

#[derive(Error, Debug, Clone)]
pub enum RemoteRepoError {
    #[error("Clone failed: {0}")]
    CloneFailed(String),
    #[error("Authentication failed - check your git credentials")]
    AuthFailed,
    #[error("Repository not found: {0}")]
    NotFound(String),
    #[error("Network error: {0}")]
    NetworkError(String),
    #[error("Invalid repository: {0}")]
    InvalidRepo(String),
    #[error("IO error: {0}")]
    IoError(String),
    #[error("Worktree already exists for branch '{branch}' at: {path}")]
    WorktreeExists { branch: String, path: String },
}

impl From<std::io::Error> for RemoteRepoError {
    fn from(err: std::io::Error) -> Self {
        RemoteRepoError::IoError(err.to_string())
    }
}

/// Information about a remote branch
#[derive(Debug, Clone)]
pub struct RemoteBranch {
    pub name: String,
    pub commit_hash: String,
    pub is_default: bool,
}

/// Manages remote repository cloning and caching
pub struct RemoteRepoManager {
    cache_dir: PathBuf,
    receipt_log: Option<PathBuf>,
}

impl RemoteRepoManager {
    /// Create a new RemoteRepoManager with default cache directory
    pub fn new() -> Result<Self> {
        let home_dir = dirs::home_dir().context("Failed to get home directory")?;
        let cache_dir = home_dir.join(".agents-in-a-box").join("repos");

        std::fs::create_dir_all(&cache_dir)?;

        Ok(Self {
            cache_dir,
            receipt_log: default_receipt_log(),
        })
    }

    /// Create with a custom cache directory (for testing)
    pub fn with_cache_dir(cache_dir: PathBuf) -> Result<Self> {
        std::fs::create_dir_all(&cache_dir)?;
        Ok(Self {
            cache_dir,
            receipt_log: default_receipt_log(),
        })
    }

    /// Write destructive-operation receipts to `path` instead of the default
    /// machine-global log. Injected rather than read from the environment deep
    /// in the call stack, so a test can point it at its own temp dir without
    /// mutating process environment shared with other threads.
    #[must_use]
    pub fn with_receipt_log(mut self, path: PathBuf) -> Self {
        self.receipt_log = Some(path);
        self
    }

    /// The safety context for destructive operations on this manager's cache.
    fn guard(&self) -> CacheGuard {
        CacheGuard {
            root: self.cache_dir.clone(),
            receipt_log: self.receipt_log.clone(),
        }
    }

    /// Get the cache directory path
    pub fn cache_dir(&self) -> &Path {
        &self.cache_dir
    }

    /// Get the cache path for a parsed repo (standard clone, not bare)
    pub fn get_cache_path(&self, parsed: &ParsedRepo) -> PathBuf {
        self.cache_dir.join(&parsed.host).join(&parsed.owner).join(&parsed.repo_name)
    }

    /// Check if a repo is already cached (standard clone with .git subdirectory)
    pub fn is_cached(&self, parsed: &ParsedRepo) -> bool {
        Self::cache_path_is_populated(&self.get_cache_path(parsed))
    }

    /// The one definition of "this cache path holds a usable clone" — shared
    /// by `is_cached` and `cached_source_path` so the rule can't drift.
    fn cache_path_is_populated(cache_path: &Path) -> bool {
        cache_path.exists() && cache_path.join(".git").exists()
    }

    /// Cache path for a clonable remote source IF it is already cloned.
    /// `None` for a not-yet-cached remote and for sources that never clone
    /// (local paths, SSH sessions, filter text). This is the disk location
    /// whose refs seed the Configure screen's branch-collision guards and
    /// the base-branch popup — keep every caller on this one resolver so the
    /// guards and the popup can never disagree about where refs live.
    pub fn cached_source_path(&self, source: &RepoSource) -> Option<PathBuf> {
        match source {
            RepoSource::HttpsUrl(_)
            | RepoSource::SshUrl(_)
            | RepoSource::GithubShorthand { .. } => {
                let parsed = source.parse_components().ok()?;
                let cache_path = self.get_cache_path(&parsed);
                Self::cache_path_is_populated(&cache_path).then_some(cache_path)
            }
            _ => None,
        }
    }

    /// List remote branches without cloning (uses git ls-remote)
    pub fn list_remote_branches(
        &self,
        source: &RepoSource,
    ) -> Result<Vec<RemoteBranch>, RemoteRepoError> {
        let url = source.to_clone_url();
        info!("Listing remote branches for: {}", url);

        let output = Command::new("git")
            .args(["ls-remote", "--heads", "--refs", &url])
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_ASKPASS", "echo")
            .output()
            .map_err(|e| RemoteRepoError::NetworkError(e.to_string()))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(classify_git_error(&stderr, &url));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut branches: Vec<RemoteBranch> = stdout
            .lines()
            .filter_map(|line| {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 2 {
                    let commit_hash = parts[0].to_string();
                    let ref_name = parts[1];
                    // refs/heads/branch-name -> branch-name
                    let name = ref_name.strip_prefix("refs/heads/")?.to_string();
                    Some(RemoteBranch {
                        name,
                        commit_hash,
                        is_default: false,
                    })
                } else {
                    None
                }
            })
            .collect();

        // Try to determine default branch
        let default_branch = self.get_default_branch_name(source);

        // Mark default branch
        for branch in &mut branches {
            if Some(&branch.name) == default_branch.as_ref() {
                branch.is_default = true;
            }
        }

        // If no default found, mark main or master
        if !branches.iter().any(|b| b.is_default) {
            for branch in &mut branches {
                if branch.name == "main" || branch.name == "master" {
                    branch.is_default = true;
                    break;
                }
            }
        }

        // Sort: default first, then alphabetical
        branches.sort_by(|a, b| match (a.is_default, b.is_default) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => a.name.cmp(&b.name),
        });

        debug!("Found {} branches", branches.len());
        Ok(branches)
    }

    /// Try to get the default branch name from remote
    fn get_default_branch_name(&self, source: &RepoSource) -> Option<String> {
        let url = source.to_clone_url();

        let output = Command::new("git")
            .args(["ls-remote", "--symref", &url, "HEAD"])
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_ASKPASS", "echo")
            .output()
            .ok()?;

        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            // Parse: ref: refs/heads/main\tHEAD
            for line in stdout.lines() {
                if line.starts_with("ref:") && line.contains("HEAD") {
                    if let Some(branch) = line
                        .split_whitespace()
                        .next()
                        .and_then(|s| s.strip_prefix("ref:"))
                        .and_then(|s| s.strip_prefix("refs/heads/"))
                    {
                        return Some(branch.to_string());
                    }
                }
            }
        }

        None
    }

    /// Clone a remote repository as a standard clone (not bare)
    ///
    /// Standard clone has .git subdirectory and working copy, making it
    /// compatible with the same worktree handling as local repositories.
    ///
    /// Concurrency-safe: the check-then-clone sequence runs under a per-repo
    /// advisory file lock and re-checks the cache once the lock is held, so N
    /// processes launching sessions against the same cold repo produce ONE
    /// clone and N-1 reuses. The clone itself is staged and published by
    /// `rename` (see [`clone_into_cache`]), which is what makes the operation
    /// SAFE; the lock only makes it EFFICIENT. A filesystem that cannot provide
    /// the lock (NFS, CIFS, a bind mount without `flock`) therefore degrades to
    /// redundant clones, never to a failed launch and never to a lost repo.
    pub fn clone_repo(
        &self,
        source: &RepoSource,
        parsed: &ParsedRepo,
    ) -> Result<PathBuf, RemoteRepoError> {
        let url = source.to_clone_url();
        let cache_path = self.get_cache_path(parsed);

        if self.is_cached(parsed) {
            info!("Repository already cached at: {}", cache_path.display());
            // Fetch updates. Deliberately OUTSIDE any clone lock: it is a
            // network round-trip, and holding the lock across it would stall
            // every other launch of the same repo behind this one.
            if let Err(e) = self.fetch_updates(&cache_path) {
                warn!("Failed to fetch updates: {}", e);
                // Continue with cached version
            }
            return Ok(cache_path);
        }

        // Create parent directories (also the lock file's home)
        if let Some(parent) = cache_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let lock = CloneLock::acquire(&cache_path);
        self.clone_under_lock(&url, &cache_path, lock.is_some())
    }

    /// The clone body, run with the per-repo clone lock already held (or, when
    /// `locked` is false, knowingly without one).
    ///
    /// Separated from [`Self::clone_repo`] so a caller that must hold the lock
    /// across a wider critical section (delete-then-reclone in
    /// [`Self::initialize_empty_remote`]) can reuse it without the lock being
    /// taken twice, which would deadlock: two `open`s of the same lock file in
    /// one process yield two file descriptions that exclude each other.
    fn clone_under_lock(
        &self,
        url: &str,
        cache_path: &Path,
        locked: bool,
    ) -> Result<PathBuf, RemoteRepoError> {
        // Double-checked: another process may have published the clone while we
        // waited on the lock. Reuse it, and do NOT fetch: a clone that landed
        // moments ago is fresher than anything a fetch could add, and the fetch
        // would run inside the lock every racer is queued behind.
        if Self::cache_path_is_populated(cache_path) {
            info!(
                "Repository cloned by a concurrent process at: {}",
                cache_path.display()
            );
            return Ok(cache_path.to_path_buf());
        }

        clone_into_cache(&self.guard(), url, cache_path, locked)
    }

    /// Fetch updates for a cached repo
    pub fn fetch_updates(&self, cache_path: &Path) -> Result<(), RemoteRepoError> {
        info!("Fetching updates for: {}", cache_path.display());

        let output = Command::new("git")
            .args(["fetch", "--all", "--prune"])
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_ASKPASS", "echo")
            .current_dir(cache_path)
            .output()
            .map_err(|e| RemoteRepoError::NetworkError(e.to_string()))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            warn!("Fetch failed: {}", stderr);
            // Non-fatal - we can continue with cached version
        }

        Ok(())
    }

    /// Create a worktree for a NEW branch cut from the remote's **default**
    /// branch (`origin/HEAD`), after a fresh fetch.
    ///
    /// This is the base-branch policy for sessions launched from a star (a
    /// remote source): the agent branch (`agents/...`) is always based on the
    /// freshly-fetched default ref — never a stale local branch and never the
    /// cache's currently-checked-out HEAD. Handles `main`/`master`/`develop`
    /// defaults transparently and retries with a filter bypass when transcrypt
    /// (or any smudge/clean filter) aborts the checkout.
    pub fn create_worktree_off_remote_default(
        &self,
        cache_path: &Path,
        worktree_path: &Path,
        new_branch: &str,
        source: &RepoSource,
    ) -> Result<PathBuf, RemoteRepoError> {
        // Resolve the default lazily inside the shared impl so the fetch
        // happens first (origin/HEAD may move).
        self.create_worktree_off_remote_branch(cache_path, worktree_path, new_branch, None, source)
    }

    /// Create a worktree for a NEW branch cut from an arbitrary remote branch
    /// (`origin/<base_branch>`), after a fresh fetch. `base_branch = None`
    /// resolves the remote's default (the star-launch policy). This is the
    /// base-branch-picker path (2026-06): the picked entry's short name comes
    /// straight through as `base_branch`.
    pub fn create_worktree_off_remote_branch(
        &self,
        cache_path: &Path,
        worktree_path: &Path,
        new_branch: &str,
        base_branch: Option<&str>,
        source: &RepoSource,
    ) -> Result<PathBuf, RemoteRepoError> {
        if let Some(parent) = worktree_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Refresh remote refs so origin/<base> reflects upstream. Defensive:
        // clone_repo already fetches on cache reuse, but a directly-supplied
        // cache may be stale. Non-fatal — we can still branch off whatever
        // origin/<base> currently points at.
        let _ = Command::new("git")
            .args(["fetch", "origin", "--prune"])
            .current_dir(cache_path)
            .output();

        let base = match base_branch {
            Some(b) => b.to_string(),
            None => self.resolve_default_branch(cache_path, source)?,
        };
        let start_point = format!("origin/{base}");
        info!(
            "Creating worktree at {} for new branch '{}' off {}",
            worktree_path.display(),
            new_branch,
            start_point
        );

        // Guard against an existing local branch of the same name. Agent branch
        // names are freshly derived, so this should not happen; fail loudly
        // rather than silently reusing a stale local branch.
        let branch_exists = Command::new("git")
            .args(["rev-parse", "--verify", "--quiet", new_branch])
            .current_dir(cache_path)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if branch_exists {
            return Err(RemoteRepoError::InvalidRepo(format!(
                "branch '{}' already exists in cache {}",
                new_branch,
                cache_path.display()
            )));
        }

        self.worktree_add_new_branch(cache_path, worktree_path, new_branch, &start_point)?;
        info!(
            "Successfully created worktree at: {}",
            worktree_path.display()
        );
        Ok(worktree_path.to_path_buf())
    }

    /// Resolve the remote's default branch name (no `origin/` prefix) for a
    /// cached clone. Order: the cache's `origin/HEAD` symbolic-ref (set by
    /// `git clone`), then the remote's advertised HEAD (`ls-remote --symref`),
    /// then a probe of `origin/main` / `origin/master`.
    fn resolve_default_branch(
        &self,
        cache_path: &Path,
        source: &RepoSource,
    ) -> Result<String, RemoteRepoError> {
        // 1. Cache-local origin/HEAD symbolic-ref (offline, reflects the clone).
        if let Ok(out) = Command::new("git")
            .args(["symbolic-ref", "--short", "refs/remotes/origin/HEAD"])
            .current_dir(cache_path)
            .output()
        {
            if out.status.success() {
                let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if let Some(branch) = s.strip_prefix("origin/") {
                    if !branch.is_empty() {
                        return Ok(branch.to_string());
                    }
                }
            }
        }

        // 2. Remote-advertised HEAD (authoritative, but needs connectivity).
        if let Some(branch) = self.get_default_branch_name(source) {
            if !branch.is_empty() {
                return Ok(branch);
            }
        }

        // 3. Probe the common defaults in the cache.
        for cand in ["main", "master"] {
            let ok = Command::new("git")
                .args([
                    "rev-parse",
                    "--verify",
                    "--quiet",
                    &format!("origin/{cand}"),
                ])
                .current_dir(cache_path)
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false);
            if ok {
                return Ok(cand.to_string());
            }
        }

        Err(RemoteRepoError::InvalidRepo(format!(
            "could not resolve default branch (origin/HEAD) in cache {}",
            cache_path.display()
        )))
    }

    /// `git worktree add -b <new_branch> <path> <start_point>` in `cache_path`,
    /// retrying with `--no-checkout` + filter bypass when a smudge/clean filter
    /// (e.g. transcrypt) aborts the checkout.
    fn worktree_add_new_branch(
        &self,
        cache_path: &Path,
        worktree_path: &Path,
        new_branch: &str,
        start_point: &str,
    ) -> Result<(), RemoteRepoError> {
        let wt = worktree_path.to_string_lossy();
        let output = Command::new("git")
            .args([
                "worktree",
                "add",
                "-b",
                new_branch,
                wt.as_ref(),
                start_point,
            ])
            .current_dir(cache_path)
            .output()?;
        if output.status.success() {
            return Ok(());
        }

        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        if !(stderr.contains("smudge filter") || stderr.contains("clean filter")) {
            return Err(RemoteRepoError::CloneFailed(format!(
                "Failed to create worktree: {stderr}"
            )));
        }

        warn!(
            "Worktree creation failed due to filter issue, retrying with --no-checkout: {}",
            stderr
        );
        if worktree_path.exists() {
            let _ = std::fs::remove_dir_all(worktree_path);
        }
        let _ = Command::new("git").args(["worktree", "prune"]).current_dir(cache_path).output();

        // Use `-B` (force create/reset) on retry: the first `-b` attempt may have
        // already created `new_branch` before failing at the checkout stage, so a
        // second `-b` would abort with "already exists".
        let retry = Command::new("git")
            .args([
                "worktree",
                "add",
                "--no-checkout",
                "-B",
                new_branch,
                wt.as_ref(),
                start_point,
            ])
            .current_dir(cache_path)
            .output()?;
        if !retry.status.success() {
            return Err(RemoteRepoError::CloneFailed(format!(
                "Failed to create worktree (even with --no-checkout): {}",
                String::from_utf8_lossy(&retry.stderr)
            )));
        }

        // Checkout files with filter bypass (transcrypt uses the 'crypt' filter).
        let checkout = Command::new("git")
            .args([
                "-c",
                "filter.crypt.smudge=cat",
                "-c",
                "filter.crypt.clean=cat",
                "checkout",
                "--force",
            ])
            .current_dir(worktree_path)
            .output()?;
        if !checkout.status.success() {
            warn!(
                "Checkout with filter bypass had issues: {}",
                String::from_utf8_lossy(&checkout.stderr)
            );
            // Non-fatal — the worktree exists, files may just not be checked out.
        }
        Ok(())
    }

    /// Checkout an existing remote branch into a worktree
    ///
    /// Unlike `create_worktree_off_remote_default`, which cuts a NEW branch from
    /// the remote default, this creates a local tracking branch for an existing
    /// remote branch.
    /// Uses -B flag to handle the case where the branch is already checked
    /// out in the cache (standard clone has default branch checked out).
    /// Returns `Ok(None)` if worktree was created at the provided path,
    /// or `Ok(Some((path, branch)))` if a new suffixed branch was created due to collision.
    pub fn checkout_existing_branch_worktree(
        &self,
        cache_path: &Path,
        worktree_path: &Path,
        remote_branch: &str,
    ) -> Result<Option<(PathBuf, String)>, RemoteRepoError> {
        info!(
            "Checking out existing branch '{}' to worktree at {}",
            remote_branch,
            worktree_path.display()
        );

        // Create parent directory for worktree
        if let Some(parent) = worktree_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Use -B to force create/reset the branch even if it's checked out elsewhere
        // This handles the case where the branch (e.g., main) is already checked out
        // in the cache directory (standard clone has default branch checked out)
        let remote_ref = format!("origin/{}", remote_branch);
        let output = Command::new("git")
            .args([
                "worktree",
                "add",
                "-B",
                remote_branch,
                worktree_path.to_string_lossy().as_ref(),
                &remote_ref,
            ])
            .current_dir(cache_path)
            .output()?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);

            // Check if failure is due to smudge/clean filter (e.g., transcrypt)
            if stderr.contains("smudge filter") || stderr.contains("clean filter") {
                warn!(
                    "Worktree creation failed due to filter issue, retrying with --no-checkout: {}",
                    stderr
                );

                // Clean up any partial worktree that might have been created
                if worktree_path.exists() {
                    let _ = std::fs::remove_dir_all(worktree_path);
                }

                // Also need to prune the worktree reference if it was partially created
                let _ = Command::new("git")
                    .args(["worktree", "prune"])
                    .current_dir(cache_path)
                    .output();

                // Retry with --no-checkout to skip the problematic filter
                let retry_output = Command::new("git")
                    .args([
                        "worktree",
                        "add",
                        "--no-checkout",
                        "-B",
                        remote_branch,
                        worktree_path.to_string_lossy().as_ref(),
                        &remote_ref,
                    ])
                    .current_dir(cache_path)
                    .output()?;

                if !retry_output.status.success() {
                    let retry_stderr = String::from_utf8_lossy(&retry_output.stderr);
                    return Err(RemoteRepoError::CloneFailed(format!(
                        "Failed to create worktree (even with --no-checkout): {}",
                        retry_stderr
                    )));
                }

                // Checkout files with filter bypass (transcrypt uses 'crypt' filter)
                let checkout_output = Command::new("git")
                    .args([
                        "-c",
                        "filter.crypt.smudge=cat",
                        "-c",
                        "filter.crypt.clean=cat",
                        "checkout",
                        "--force",
                    ])
                    .current_dir(worktree_path)
                    .output()?;

                if !checkout_output.status.success() {
                    let checkout_stderr = String::from_utf8_lossy(&checkout_output.stderr);
                    warn!(
                        "Checkout with filter bypass had issues: {}",
                        checkout_stderr
                    );
                    // Continue anyway - the worktree exists, files just might not be checked out
                }

                info!(
                    "Created worktree with filter bypass at: {}",
                    worktree_path.display()
                );
            } else if stderr.contains("is already used by worktree at") {
                // Branch already has a worktree - create a new branch with suffix
                info!(
                    "Branch '{}' already has a worktree, creating suffixed branch",
                    remote_branch
                );

                // Generate suffix and new branch/path names
                let suffix: String = uuid::Uuid::new_v4().to_string().chars().take(8).collect();
                let suffixed_branch = format!("{}-{}", remote_branch, suffix);

                // Generate suffixed worktree path
                let worktree_dir = worktree_path
                    .file_name()
                    .map(|n| format!("{}-{}", n.to_string_lossy(), suffix))
                    .unwrap_or_else(|| format!("worktree-{}", suffix));
                let suffixed_worktree_path = worktree_path
                    .parent()
                    .map(|p| p.join(&worktree_dir))
                    .unwrap_or_else(|| PathBuf::from(&worktree_dir));

                // Create worktree with the new suffixed branch
                let retry_output = Command::new("git")
                    .args([
                        "worktree",
                        "add",
                        "-b",
                        &suffixed_branch,
                        suffixed_worktree_path.to_string_lossy().as_ref(),
                        &remote_ref,
                    ])
                    .current_dir(cache_path)
                    .output()?;

                if !retry_output.status.success() {
                    let retry_stderr = String::from_utf8_lossy(&retry_output.stderr);

                    // Check if failure is due to smudge/clean filter (e.g., transcrypt)
                    if retry_stderr.contains("smudge filter")
                        || retry_stderr.contains("clean filter")
                    {
                        warn!(
                            "Suffixed worktree creation failed due to filter issue, retrying with --no-checkout: {}",
                            retry_stderr
                        );

                        // Clean up any partial worktree
                        if suffixed_worktree_path.exists() {
                            let _ = std::fs::remove_dir_all(&suffixed_worktree_path);
                        }
                        let _ = Command::new("git")
                            .args(["worktree", "prune"])
                            .current_dir(cache_path)
                            .output();

                        // Retry with --no-checkout
                        let no_checkout_output = Command::new("git")
                            .args([
                                "worktree",
                                "add",
                                "--no-checkout",
                                "-b",
                                &suffixed_branch,
                                suffixed_worktree_path.to_string_lossy().as_ref(),
                                &remote_ref,
                            ])
                            .current_dir(cache_path)
                            .output()?;

                        if !no_checkout_output.status.success() {
                            let no_checkout_stderr =
                                String::from_utf8_lossy(&no_checkout_output.stderr);

                            // Check if the suffixed branch already exists
                            if no_checkout_stderr.contains("already exists") {
                                info!(
                                    "Suffixed branch '{}' already exists, looking for existing worktree",
                                    suffixed_branch
                                );

                                // Find worktree for this branch
                                if let Some(result) =
                                    find_worktree_for_branch(cache_path, &suffixed_branch)?
                                {
                                    return Ok(Some(result));
                                }

                                // Branch exists but no worktree found - it's orphaned
                                // Clean it up and retry with a new suffix
                                info!(
                                    "Branch '{}' is orphaned (no worktree found), cleaning up and retrying",
                                    suffixed_branch
                                );

                                if delete_orphaned_branch(cache_path, &suffixed_branch)? {
                                    // Generate a new suffix and retry
                                    let new_suffix: String =
                                        uuid::Uuid::new_v4().to_string().chars().take(8).collect();
                                    let new_suffixed_branch =
                                        format!("{}-{}", remote_branch, new_suffix);
                                    let new_worktree_dir = worktree_path
                                        .file_name()
                                        .map(|n| format!("{}-{}", n.to_string_lossy(), new_suffix))
                                        .unwrap_or_else(|| format!("worktree-{}", new_suffix));
                                    let new_suffixed_worktree_path = worktree_path
                                        .parent()
                                        .map(|p| p.join(&new_worktree_dir))
                                        .unwrap_or_else(|| PathBuf::from(&new_worktree_dir));

                                    info!(
                                        "Retrying with new branch '{}' at {}",
                                        new_suffixed_branch,
                                        new_suffixed_worktree_path.display()
                                    );

                                    let retry2_output = Command::new("git")
                                        .args([
                                            "worktree",
                                            "add",
                                            "--no-checkout",
                                            "-b",
                                            &new_suffixed_branch,
                                            new_suffixed_worktree_path.to_string_lossy().as_ref(),
                                            &remote_ref,
                                        ])
                                        .current_dir(cache_path)
                                        .output()?;

                                    if retry2_output.status.success() {
                                        // Checkout with filter bypass
                                        let _ = Command::new("git")
                                            .args([
                                                "-c",
                                                "filter.crypt.smudge=cat",
                                                "-c",
                                                "filter.crypt.clean=cat",
                                                "checkout",
                                                "--force",
                                            ])
                                            .current_dir(&new_suffixed_worktree_path)
                                            .output();

                                        // Set up tracking
                                        let _ = Command::new("git")
                                            .args([
                                                "branch",
                                                "--set-upstream-to",
                                                &remote_ref,
                                                &new_suffixed_branch,
                                            ])
                                            .current_dir(&new_suffixed_worktree_path)
                                            .output();

                                        info!(
                                            "Created worktree with new suffixed branch '{}' at {}",
                                            new_suffixed_branch,
                                            new_suffixed_worktree_path.display()
                                        );
                                        return Ok(Some((
                                            new_suffixed_worktree_path,
                                            new_suffixed_branch,
                                        )));
                                    }
                                }

                                // If cleanup and retry failed, return error
                                return Err(RemoteRepoError::CloneFailed(format!(
                                    "Branch '{}' exists but couldn't find its worktree, and cleanup failed. \
                                     Try manually: git branch -D {}",
                                    suffixed_branch, suffixed_branch
                                )));
                            }

                            return Err(RemoteRepoError::CloneFailed(format!(
                                "Failed to create suffixed worktree (even with --no-checkout): {}",
                                no_checkout_stderr
                            )));
                        }

                        // Checkout files with filter bypass
                        let checkout_output = Command::new("git")
                            .args([
                                "-c",
                                "filter.crypt.smudge=cat",
                                "-c",
                                "filter.crypt.clean=cat",
                                "checkout",
                                "--force",
                            ])
                            .current_dir(&suffixed_worktree_path)
                            .output()?;

                        if !checkout_output.status.success() {
                            let checkout_stderr = String::from_utf8_lossy(&checkout_output.stderr);
                            warn!(
                                "Checkout with filter bypass had issues: {}",
                                checkout_stderr
                            );
                        }

                        info!(
                            "Created suffixed worktree with filter bypass at: {}",
                            suffixed_worktree_path.display()
                        );
                    } else if retry_stderr.contains("already exists") {
                        // Suffixed branch already exists from a previous session
                        // Find and return the existing worktree for that branch
                        info!(
                            "Suffixed branch '{}' already exists, looking for existing worktree",
                            suffixed_branch
                        );

                        if let Some(result) =
                            find_worktree_for_branch(cache_path, &suffixed_branch)?
                        {
                            return Ok(Some(result));
                        }

                        // Branch exists but no worktree found - it's orphaned
                        // Clean it up and retry with a new suffix
                        info!(
                            "Branch '{}' is orphaned (no worktree found), cleaning up and retrying",
                            suffixed_branch
                        );

                        if delete_orphaned_branch(cache_path, &suffixed_branch)? {
                            // Generate a new suffix and retry
                            let new_suffix: String =
                                uuid::Uuid::new_v4().to_string().chars().take(8).collect();
                            let new_suffixed_branch = format!("{}-{}", remote_branch, new_suffix);
                            let new_worktree_dir = worktree_path
                                .file_name()
                                .map(|n| format!("{}-{}", n.to_string_lossy(), new_suffix))
                                .unwrap_or_else(|| format!("worktree-{}", new_suffix));
                            let new_suffixed_worktree_path = worktree_path
                                .parent()
                                .map(|p| p.join(&new_worktree_dir))
                                .unwrap_or_else(|| PathBuf::from(&new_worktree_dir));

                            info!(
                                "Retrying with new branch '{}' at {}",
                                new_suffixed_branch,
                                new_suffixed_worktree_path.display()
                            );

                            let retry2_output = Command::new("git")
                                .args([
                                    "worktree",
                                    "add",
                                    "-b",
                                    &new_suffixed_branch,
                                    new_suffixed_worktree_path.to_string_lossy().as_ref(),
                                    &remote_ref,
                                ])
                                .current_dir(cache_path)
                                .output()?;

                            if retry2_output.status.success() {
                                // Set up tracking
                                let _ = Command::new("git")
                                    .args([
                                        "branch",
                                        "--set-upstream-to",
                                        &remote_ref,
                                        &new_suffixed_branch,
                                    ])
                                    .current_dir(&new_suffixed_worktree_path)
                                    .output();

                                info!(
                                    "Created worktree with new suffixed branch '{}' at {}",
                                    new_suffixed_branch,
                                    new_suffixed_worktree_path.display()
                                );
                                return Ok(Some((new_suffixed_worktree_path, new_suffixed_branch)));
                            }
                        }

                        // If cleanup and retry failed, return error
                        return Err(RemoteRepoError::CloneFailed(format!(
                            "Branch '{}' exists but couldn't find its worktree, and cleanup failed. \
                             Try manually: git branch -D {}",
                            suffixed_branch, suffixed_branch
                        )));
                    } else {
                        return Err(RemoteRepoError::CloneFailed(format!(
                            "Failed to create worktree with suffixed branch: {}",
                            retry_stderr
                        )));
                    }
                }

                // Set up tracking for the suffixed branch
                let _ = Command::new("git")
                    .args(["branch", "--set-upstream-to", &remote_ref, &suffixed_branch])
                    .current_dir(&suffixed_worktree_path)
                    .output();

                info!(
                    "Created worktree with suffixed branch '{}' at {}",
                    suffixed_branch,
                    suffixed_worktree_path.display()
                );
                return Ok(Some((suffixed_worktree_path, suffixed_branch)));
            } else {
                return Err(RemoteRepoError::CloneFailed(format!(
                    "Failed to create worktree for existing branch: {}",
                    stderr
                )));
            }
        }

        // Set up tracking for the branch in the worktree
        let _ = Command::new("git")
            .args(["branch", "--set-upstream-to", &remote_ref, remote_branch])
            .current_dir(worktree_path)
            .output();

        info!(
            "Successfully checked out existing branch '{}' to worktree",
            remote_branch
        );
        Ok(None)
    }

    /// Get list of cached repositories for recent repos feature
    pub fn list_cached_repos(&self) -> Result<Vec<ParsedRepo>> {
        let mut repos = Vec::new();

        if !self.cache_dir.exists() {
            return Ok(repos);
        }

        // Walk the cache directory structure: host/owner/repo
        // Standard clones have .git subdirectory
        if let Ok(hosts) = std::fs::read_dir(&self.cache_dir) {
            for host_entry in hosts.flatten() {
                if !host_entry.path().is_dir() {
                    continue;
                }
                let host = host_entry.file_name().to_string_lossy().to_string();

                if let Ok(owners) = std::fs::read_dir(host_entry.path()) {
                    for owner_entry in owners.flatten() {
                        if !owner_entry.path().is_dir() {
                            continue;
                        }
                        let owner = owner_entry.file_name().to_string_lossy().to_string();

                        if let Ok(repo_dirs) = std::fs::read_dir(owner_entry.path()) {
                            for repo_entry in repo_dirs.flatten() {
                                let repo_path = repo_entry.path();

                                // Check for standard clone (.git subdirectory)
                                if repo_path.join(".git").exists() {
                                    let repo_name =
                                        repo_entry.file_name().to_string_lossy().to_string();
                                    let url = format!("https://{}/{}/{}", host, owner, repo_name);
                                    repos.push(ParsedRepo {
                                        source: RepoSource::HttpsUrl(url),
                                        host: host.clone(),
                                        owner: owner.clone(),
                                        repo_name,
                                    });
                                }
                            }
                        }
                    }
                }
            }
        }

        // Sort by repo name for consistent ordering
        repos.sort_by(|a, b| {
            let a_key = format!("{}/{}", a.owner, a.repo_name);
            let b_key = format!("{}/{}", b.owner, b.repo_name);
            a_key.cmp(&b_key)
        });

        Ok(repos)
    }

    /// Initialize an EMPTY remote repository in place: clone it (an empty
    /// clone succeeds), commit a README, and push. Returns the branch name
    /// the initial commit landed on. Lets the Configure screen unblock a
    /// fresh GitHub repo without the user ever leaving ainb.
    pub fn initialize_empty_remote(
        &self,
        source: &RepoSource,
        parsed: &ParsedRepo,
    ) -> Result<String, RemoteRepoError> {
        // Re-verify emptiness at action time — the EmptyRemote verdict that
        // offered [i] may be stale (a collaborator pushed meanwhile). Pushing
        // a README onto a repo that now has history is never what's wanted.
        let branches = self.list_remote_branches(source)?;
        if !branches.is_empty() {
            return Err(RemoteRepoError::InvalidRepo(
                "repository is no longer empty — press Esc and re-open it".to_string(),
            ));
        }

        let mut cache_path = self.clone_repo(source, parsed)?;
        // A warm cache can carry commits the remote no longer has (repo
        // deleted and recreated empty under the same name). Pushing that
        // would silently upload the dead repo's entire history. The remote
        // is verifiably empty, so wipe and re-clone fresh instead.
        if local_head_exists(&cache_path) {
            // The clone lock is held across BOTH the delete and the re-clone.
            // Deleting outside it would be the same unlocked destructive act
            // this module exists to prevent: a peer could publish into the gap
            // and have its fresh clone removed a moment later.
            let lock = CloneLock::acquire(&cache_path);
            self.guard()
                .remove_repo_dir(&cache_path, "initialize_empty_remote: stale history")?;
            cache_path =
                self.clone_under_lock(&source.to_clone_url(), &cache_path, lock.is_some())?;
        }
        push_initial_commit(&cache_path, &parsed.repo_name)
    }

    /// Remove a cached repository.
    ///
    /// Errors instead of deleting when live worktrees still link into the
    /// clone. Currently has no callers, but it is public API on a public type,
    /// so it is kept and routed through the guard: wiring it up later cannot
    /// reintroduce an unguarded wipe.
    pub fn remove_cached_repo(&self, parsed: &ParsedRepo) -> Result<(), RemoteRepoError> {
        let cache_path = self.get_cache_path(parsed);
        let _lock = CloneLock::acquire(&cache_path);
        self.guard().remove_repo_dir(&cache_path, "remove_cached_repo")
    }
}

impl Default for RemoteRepoManager {
    fn default() -> Self {
        Self::new().expect("Failed to create RemoteRepoManager")
    }
}

/// Find an existing worktree for a given branch name
///
/// Parses `git worktree list --porcelain` output to find a worktree
/// that is checked out on the specified branch.
fn find_worktree_for_branch(
    cache_path: &Path,
    branch_name: &str,
) -> Result<Option<(PathBuf, String)>, RemoteRepoError> {
    let worktree_list_output = Command::new("git")
        .args(["worktree", "list", "--porcelain"])
        .current_dir(cache_path)
        .output()?;

    if !worktree_list_output.status.success() {
        return Ok(None);
    }

    let output = String::from_utf8_lossy(&worktree_list_output.stdout);

    // Parse porcelain output to find worktree with matching branch
    // Format: worktree /path/to/worktree\nbranch refs/heads/branch-name\n\n
    let mut current_worktree: Option<PathBuf> = None;

    for line in output.lines() {
        if let Some(path_str) = line.strip_prefix("worktree ") {
            current_worktree = Some(PathBuf::from(path_str));
        } else if let Some(branch) = line.strip_prefix("branch refs/heads/") {
            if branch == branch_name {
                if let Some(ref path) = current_worktree {
                    info!(
                        "Found existing worktree for branch '{}' at: {}",
                        branch_name,
                        path.display()
                    );
                    return Ok(Some((path.clone(), branch_name.to_string())));
                }
            }
        }
    }

    Ok(None)
}

/// Delete an orphaned branch (branch exists but worktree was removed)
///
/// This cleans up branches that were left behind when their worktree
/// directories were manually deleted.
fn delete_orphaned_branch(cache_path: &Path, branch_name: &str) -> Result<bool, RemoteRepoError> {
    info!(
        "Attempting to delete orphaned branch '{}' from cache",
        branch_name
    );

    // First prune any stale worktree references
    let _ = Command::new("git").args(["worktree", "prune"]).current_dir(cache_path).output();

    // Delete the branch
    let output = Command::new("git")
        .args(["branch", "-D", branch_name])
        .current_dir(cache_path)
        .output()?;

    if output.status.success() {
        info!("Successfully deleted orphaned branch '{}'", branch_name);
        Ok(true)
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        warn!(
            "Failed to delete orphaned branch '{}': {}",
            branch_name, stderr
        );
        Ok(false)
    }
}

/// Does the clone at `path` have any commit on HEAD?
fn local_head_exists(path: &Path) -> bool {
    Command::new("git")
        .args(["rev-parse", "--verify", "--quiet", "HEAD"])
        .current_dir(path)
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Create the initial commit (a README) in a clone of an EMPTY remote and
/// push it to `origin`. Returns the branch name pushed (the clone's HEAD
/// symref — git/GitHub advertise the default branch name even for empty
/// repos, so this respects a `master`/custom default).
///
/// Split from `initialize_empty_remote` so it's testable against a local
/// bare remote without network. Idempotent-ish: if the cache already holds
/// a local commit (a previous init attempt that failed at push), it skips
/// the README/commit step and just pushes.
fn push_initial_commit(cache_path: &Path, repo_name: &str) -> Result<String, RemoteRepoError> {
    let branch = {
        let out = Command::new("git")
            .args(["symbolic-ref", "--short", "HEAD"])
            .current_dir(cache_path)
            .output()?;
        let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if out.status.success() && !name.is_empty() {
            name
        } else {
            "main".to_string()
        }
    };

    // A previous attempt may have committed but failed at push — don't stack
    // a second README commit on top.
    let has_commit = local_head_exists(cache_path);

    if !has_commit {
        let readme = cache_path.join("README.md");
        if !readme.exists() {
            std::fs::write(&readme, format!("# {repo_name}\n"))?;
        }
        let add = Command::new("git")
            .args(["add", "README.md"])
            .current_dir(cache_path)
            .output()?;
        if !add.status.success() {
            return Err(RemoteRepoError::InvalidRepo(
                String::from_utf8_lossy(&add.stderr).trim().to_string(),
            ));
        }
        // Uses the user's own git identity — a missing identity surfaces
        // git's exact "tell me who you are" error. Signing is forced off:
        // a gpg pin-entry prompt would hang this non-interactive path.
        let commit = Command::new("git")
            .args([
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-m",
                "Initial commit",
            ])
            .env("GIT_TERMINAL_PROMPT", "0")
            .current_dir(cache_path)
            .output()?;
        if !commit.status.success() {
            return Err(RemoteRepoError::InvalidRepo(
                String::from_utf8_lossy(&commit.stderr).trim().to_string(),
            ));
        }
        info!("Created initial README commit in {}", cache_path.display());
    }

    let push = Command::new("git")
        .args(["push", "-u", "origin", &branch])
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ASKPASS", "echo")
        .current_dir(cache_path)
        .output()?;
    if !push.status.success() {
        let stderr = String::from_utf8_lossy(&push.stderr);
        // Second arg names the repository in NotFound messages — the branch
        // here would render "Repository not found: main".
        return Err(classify_git_error(&stderr, repo_name));
    }

    info!("Pushed initial commit to origin/{branch}");
    Ok(branch)
}

/// A dot-prefixed sibling of `cache_path` carrying `suffix`.
///
/// Siblings (same parent, hence same filesystem) so a `rename` between one and
/// `cache_path` is atomic, and dot-prefixed so `list_cached_repos` skips them.
/// Built by string append rather than `Path::with_extension` so repos named
/// `foo.rs` and `foo.py` cannot collide on a shared `foo.lock`. One helper for
/// both the lock and the staging path so the two can never desynchronise.
fn sibling_dotted(cache_path: &Path, suffix: &str) -> Result<PathBuf, RemoteRepoError> {
    let parent = cache_path.parent().ok_or_else(|| {
        RemoteRepoError::IoError(format!(
            "cache path has no parent directory: {}",
            cache_path.display()
        ))
    })?;
    let name = cache_path
        .file_name()
        .ok_or_else(|| {
            RemoteRepoError::IoError(format!(
                "cache path has no file name: {}",
                cache_path.display()
            ))
        })?
        .to_string_lossy();
    Ok(parent.join(format!(".{name}.{suffix}")))
}

/// Distinguishes staging dirs made by two clones inside one process; the pid
/// separates processes.
static STAGING_NONCE: AtomicU64 = AtomicU64::new(0);

/// The filename fragment marking a staging directory, shared by the path
/// builder and the sweeper so a rename of one cannot orphan the other.
const STAGING_TAG: &str = "clone-tmp";

/// The private staging directory a clone of `cache_path` is built in.
fn staging_path(cache_path: &Path) -> Result<PathBuf, RemoteRepoError> {
    let nonce = STAGING_NONCE.fetch_add(1, Ordering::Relaxed);
    sibling_dotted(
        cache_path,
        &format!("{STAGING_TAG}-{}-{nonce}", std::process::id()),
    )
}

/// The advisory lock file serialising clones of the repo at `cache_path`.
///
/// Lives BESIDE the repo directory, never inside it: the clone is published by
/// renaming a new directory over `cache_path`, so a lock file held under that
/// path would be swapped out from under its holder mid-critical-section.
fn clone_lock_path(cache_path: &Path) -> Result<PathBuf, RemoteRepoError> {
    sibling_dotted(cache_path, "clone-lock")
}

/// Remove staging directories for this repo left behind by runs that died
/// mid-clone (Ctrl-C, OOM, a cancelled task). Without this they accumulate
/// forever, one full partial checkout per abandoned attempt.
///
/// ONLY sound while the clone lock is held: the lock is what guarantees no peer
/// has a staging directory for this repo in flight, so every match is a corpse.
/// Called without it, this would delete a live peer's partial clone.
fn sweep_stale_staging(guard: &CacheGuard, cache_path: &Path, keep: &Path) {
    let (Some(parent), Some(name)) = (cache_path.parent(), cache_path.file_name()) else {
        return;
    };
    let prefix = format!(".{}.{STAGING_TAG}-", name.to_string_lossy());
    let Ok(entries) = std::fs::read_dir(parent) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path == keep || !entry.file_name().to_string_lossy().starts_with(&prefix) {
            continue;
        }
        info!("Sweeping abandoned staging dir {}", path.display());
        if let Err(e) = guard.remove_repo_dir(&path, "sweep abandoned staging dir") {
            warn!("Failed to sweep staging dir {}: {}", path.display(), e);
        }
    }
}

/// Clone `url` into a private staging directory, then publish it into
/// `cache_path` with an atomic `rename`.
///
/// The staging directory is a sibling of `cache_path` so the rename stays on
/// one filesystem (and therefore atomic). The only directory this function ever
/// removes is that staging directory, which it created, so a failed clone
/// cannot delete a populated shared repo no matter who else is running. Losing
/// the publish race to another process is a success, not a failure: their clone
/// is adopted and ours discarded.
fn clone_into_cache(
    guard: &CacheGuard,
    url: &str,
    cache_path: &Path,
    locked: bool,
) -> Result<PathBuf, RemoteRepoError> {
    let staging = staging_path(cache_path)?;
    if locked {
        sweep_stale_staging(guard, cache_path, &staging);
    }
    // A same-named leftover can only come from a crashed earlier run whose pid
    // we now reuse; it is ours by construction, but route it through the guard
    // anyway so nothing in this file deletes unchecked.
    guard.remove_repo_dir(&staging, "clone_into_cache: stale staging dir")?;

    info!(
        "Cloning {} to {} (staging at {})",
        url,
        cache_path.display(),
        staging.display()
    );

    // Standard clone (not --bare) for compatibility with worktree discovery
    let output = Command::new("git")
        .args(["clone", url])
        .arg(&staging)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ASKPASS", "echo")
        .output()
        .map_err(|e| RemoteRepoError::CloneFailed(e.to_string()))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // A failed `git clone` (auth rejected mid-transfer, network drop)
        // leaves a partial checkout. It is in OUR staging dir, so clearing it
        // can never touch the shared cache, and the cache was never written at
        // all, so a retry starts clean either way. A cleanup that fails is
        // logged, never raised: the clone error is the one the user needs.
        if let Err(e) = guard.remove_repo_dir(&staging, "clone_into_cache: failed clone") {
            warn!(
                "Failed to remove staged partial clone at {}: {}",
                staging.display(),
                e
            );
        }
        return Err(classify_git_error(&stderr, url));
    }

    publish_staged_clone(guard, &staging, cache_path, locked)?;

    info!("Successfully cloned to: {}", cache_path.display());
    Ok(cache_path.to_path_buf())
}

/// If `cache_path` now holds a real clone then a peer won the publish race:
/// adopt theirs and discard ours. Reports whether that happened.
fn adopt_if_published(guard: &CacheGuard, staging: &Path, cache_path: &Path) -> bool {
    if !RemoteRepoManager::cache_path_is_populated(cache_path) {
        return false;
    }
    info!(
        "Concurrent clone already published {}, reusing it and discarding {}",
        cache_path.display(),
        staging.display()
    );
    if let Err(e) = guard.remove_repo_dir(staging, "publish_staged_clone: discard loser") {
        warn!(
            "Failed to discard redundant staged clone at {}: {}",
            staging.display(),
            e
        );
    }
    true
}

/// Move a finished staging clone into place at `cache_path`.
///
/// `rename` over an existing non-empty directory fails rather than clobbering
/// it, which is precisely the guarantee wanted here: whoever renames first
/// wins, and the loser adopts the winner's clone instead of replacing a repo
/// that live worktrees may already point at.
fn publish_staged_clone(
    guard: &CacheGuard,
    staging: &Path,
    cache_path: &Path,
    locked: bool,
) -> Result<(), RemoteRepoError> {
    if std::fs::rename(staging, cache_path).is_ok() {
        return Ok(());
    }
    if adopt_if_published(guard, staging, cache_path) {
        return Ok(());
    }

    // The destination exists but holds no usable clone: a partial left by an
    // aborted run from before this staging scheme, or a stray directory.
    if !locked {
        // With no lock there is nothing stopping a peer publishing a real clone
        // into this path between the check above and a delete below, and that
        // clone would not be ours to remove. Refusing costs one failed launch;
        // guessing wrong costs a repo and every worktree cut from it.
        if let Err(e) = guard.remove_repo_dir(staging, "publish_staged_clone: unlocked bail-out") {
            warn!(
                "Failed to discard staged clone at {}: {}",
                staging.display(),
                e
            );
        }
        return Err(RemoteRepoError::IoError(format!(
            "cannot publish clone to {}: the destination holds unusable content and no clone lock is available to clear it safely",
            cache_path.display()
        )));
    }

    guard.remove_repo_dir(
        cache_path,
        "publish_staged_clone: clear unusable destination",
    )?;

    std::fs::rename(staging, cache_path).map_err(|e| {
        let _ = guard.remove_repo_dir(staging, "publish_staged_clone: unpublishable clone");
        RemoteRepoError::IoError(format!(
            "failed to publish clone {} -> {}: {e}",
            staging.display(),
            cache_path.display()
        ))
    })
}

/// Holds the exclusive advisory clone lock for one repo, releasing it on drop
/// (including on the `?` early-returns through the clone path).
struct CloneLock {
    file: std::fs::File,
}

impl CloneLock {
    /// Block until this process holds the clone lock for `cache_path`, or
    /// report `None` when no lock can be had.
    ///
    /// A lock is an OPTIMISATION here, not the safety mechanism: staging plus
    /// atomic rename is what makes concurrent clones safe, and the lock only
    /// stops N racers doing N redundant clones. So a filesystem that cannot
    /// provide one (NFS, CIFS, a 9p or drvfs bind mount where `flock` returns
    /// ENOTSUP) degrades to extra work, never to a failed session launch.
    ///
    /// The lock is per open file description, so two threads of one ainb
    /// process exclude each other exactly as two separate processes do.
    fn acquire(cache_path: &Path) -> Option<Self> {
        let path = match clone_lock_path(cache_path) {
            Ok(path) => path,
            Err(e) => {
                warn!("Proceeding without a clone lock: {}", e);
                return None;
            }
        };
        let file = match std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
        {
            Ok(file) => file,
            Err(e) => {
                warn!(
                    "Proceeding without a clone lock ({} is not writable: {})",
                    path.display(),
                    e
                );
                return None;
            }
        };
        if let Err(e) = FileExt::lock_exclusive(&file) {
            warn!(
                "Proceeding without a clone lock (this filesystem cannot lock {}: {})",
                path.display(),
                e
            );
            return None;
        }
        debug!("Holding clone lock {}", path.display());
        Some(Self { file })
    }
}

impl Drop for CloneLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

/// Count the LINKED worktrees in `git worktree list --porcelain` output.
///
/// The first block is the repo's own checkout, never a linked worktree. Blocks
/// git marks `prunable` are registrations whose checkout is already gone, so
/// they are not live and must not keep the guard latched.
fn count_linked_worktrees(porcelain: &str) -> usize {
    porcelain
        .split("\n\n")
        .filter(|block| block.lines().any(|l| l.starts_with("worktree ")))
        .skip(1)
        .filter(|block| !block.lines().any(|l| l == "prunable" || l.starts_with("prunable ")))
        .count()
}

/// How many live worktrees are linked to the clone at `cache_path`.
///
/// Asks git rather than counting `.git/worktrees/` entries, for two reasons the
/// filesystem cannot express. First, a registration OUTLIVES its checkout: `rm
/// -rf` on a worktree leaves the entry on disk until someone prunes, so a
/// directory count latches at 1 forever and jams every later delete. Second,
/// `.git` is not always a directory (a linked worktree or `--separate-git-dir`
/// makes it a file), and a `read_dir` there fails with ENOTDIR, which a count
/// would read as "no worktrees" precisely when the answer matters most.
///
/// Fails CLOSED: any question it cannot answer becomes an error, and the guard
/// refuses the delete. A guard whose purpose is to refuse must never treat an
/// unreadable repo as an empty one.
fn live_worktree_count(cache_path: &Path) -> Result<usize, RemoteRepoError> {
    // Not a git checkout at all: a staging dir, a stray directory. Nothing can
    // be linked to it, and `git worktree list` would fail on it.
    if !cache_path.join(".git").exists() {
        return Ok(0);
    }

    // Drop registrations whose checkout is already gone. git only does this on
    // an explicit prune, and `delete_orphaned_branch` in this file prunes for
    // the same reason before touching branches.
    let _ = Command::new("git").args(["worktree", "prune"]).current_dir(cache_path).output();

    let output = Command::new("git")
        .args(["worktree", "list", "--porcelain"])
        .current_dir(cache_path)
        .output()
        .map_err(|e| {
            RemoteRepoError::IoError(format!(
                "refusing to delete {}: cannot run git to check for live worktrees ({e})",
                cache_path.display()
            ))
        })?;

    if !output.status.success() {
        return Err(RemoteRepoError::IoError(format!(
            "refusing to delete {}: cannot determine whether live worktrees link to it ({})",
            cache_path.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }

    Ok(count_linked_worktrees(&String::from_utf8_lossy(
        &output.stdout,
    )))
}

/// The safety context every deletion of a repo cache directory runs under: the
/// root such deletions may not escape, and where the receipt is written.
#[derive(Clone)]
struct CacheGuard {
    root: PathBuf,
    receipt_log: Option<PathBuf>,
}

impl CacheGuard {
    /// Delete a repo cache directory, refusing if the path escapes the cache
    /// root or if live worktrees still link into it, and recording a receipt
    /// either way.
    ///
    /// Every deletion of a repo cache directory in this file goes through here.
    /// (Worktree directories are removed directly at the `create_worktree_*`
    /// call sites, each followed by `git worktree prune`; those are a different
    /// object with a different invariant and are not covered by this guard.)
    /// A caller that genuinely means to remove a repo with worktrees must
    /// remove the worktrees first; there is deliberately no override flag.
    fn remove_repo_dir(&self, path: &Path, operation: &str) -> Result<(), RemoteRepoError> {
        // Nothing to delete, nothing to record. Keeps the receipt log meaning
        // "a real directory was removed or refused".
        if !path.exists() {
            return Ok(());
        }

        self.assert_contained(path, operation)?;

        let worktrees = live_worktree_count(path)?;
        if worktrees > 0 {
            self.record(
                path,
                operation,
                &format!("REFUSED ({worktrees} live worktree(s))"),
            );
            return Err(RemoteRepoError::IoError(format!(
                "refusing to delete {} during {operation}: {worktrees} live worktree(s) still link to it, remove those worktrees first",
                path.display()
            )));
        }

        // A receipt BEFORE the removal, so a crash or a kill part-way through
        // still leaves evidence of what was being deleted, and another after,
        // so the log never claims a removal that did not actually complete.
        self.record(path, operation, "ATTEMPT remove_dir_all");
        match std::fs::remove_dir_all(path) {
            Ok(()) => {
                self.record(path, operation, "REMOVED");
                info!(
                    "Removed repo cache directory {} ({operation})",
                    path.display()
                );
                Ok(())
            }
            Err(e) => {
                self.record(path, operation, &format!("FAILED ({e})"));
                Err(RemoteRepoError::IoError(format!(
                    "failed to remove {} during {operation}: {e}",
                    path.display()
                )))
            }
        }
    }

    /// Refuse any path that does not resolve to somewhere strictly beneath the
    /// cache root.
    ///
    /// `get_cache_path` joins host, owner and repo straight from regex-parsed
    /// remote URLs, so an owner or repo of `..` would otherwise walk out of
    /// `~/.agents-in-a-box/repos` and hand an arbitrary directory to
    /// `remove_dir_all`. Canonicalising both sides also means a symlink planted
    /// inside the cache cannot redirect a delete outside it.
    fn assert_contained(&self, path: &Path, operation: &str) -> Result<(), RemoteRepoError> {
        let root = std::fs::canonicalize(&self.root).map_err(|e| {
            RemoteRepoError::IoError(format!(
                "refusing to delete {} during {operation}: repo cache root {} is unreadable ({e})",
                path.display(),
                self.root.display()
            ))
        })?;
        let resolved = std::fs::canonicalize(path).map_err(|e| {
            RemoteRepoError::IoError(format!(
                "refusing to delete {} during {operation}: path is unreadable ({e})",
                path.display()
            ))
        })?;

        if resolved == root || !resolved.starts_with(&root) {
            self.record(path, operation, "REFUSED (outside the repo cache)");
            return Err(RemoteRepoError::IoError(format!(
                "refusing to delete {} during {operation}: it resolves to {}, which is not inside the repo cache {}",
                path.display(),
                resolved.display(),
                root.display()
            )));
        }
        Ok(())
    }

    /// Record one destructive operation on a repo cache path.
    ///
    /// Best effort by design (a receipt that cannot be written must never block
    /// or fail the operation it describes) but it is also emitted through
    /// `tracing`, so the evidence exists in two places.
    fn record(&self, path: &Path, operation: &str, outcome: &str) {
        let resolved = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        let pid = std::process::id();
        warn!(
            target: "destructive_op",
            path = %resolved.display(),
            operation,
            outcome,
            pid,
            "repo cache directory removal"
        );

        let Some(log_path) = self.receipt_log.as_ref() else {
            return;
        };
        if let Some(parent) = log_path.parent() {
            if std::fs::create_dir_all(parent).is_err() {
                return;
            }
        }
        // One preformatted `write_all`, never `writeln!`: `File` does not
        // override `write_fmt`, so a multi-fragment format issues one write
        // syscall per fragment and concurrent appends from other ainb processes
        // splice into each other, corrupting the very log used to reconstruct
        // an incident.
        let line = format!(
            "{}\tpid={pid}\t{operation}\t{outcome}\t{}\n",
            chrono::Utc::now().to_rfc3339(),
            resolved.display()
        );
        if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(log_path) {
            let _ = file.write_all(line.as_bytes());
        }
    }
}

/// Where destructive-operation receipts go unless a caller injects a path.
///
/// Deliberately OUTSIDE `~/.agents-in-a-box`: when that whole tree is what got
/// wiped, a log stored inside it is gone with it. Same OS cache-dir resolver as
/// `cli::statusline` (`~/Library/Caches/ainb` on macOS, `~/.cache/ainb` on
/// Linux). `AINB_DESTRUCTIVE_OPS_LOG` redirects it to a rotated ops location;
/// it is read once, when a manager is constructed, never from the destructive
/// path itself.
fn default_receipt_log() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("AINB_DESTRUCTIVE_OPS_LOG") {
        return Some(PathBuf::from(path));
    }
    let dir = dirs::cache_dir().or_else(|| dirs::home_dir().map(|h| h.join(".cache")))?;
    Some(dir.join("ainb").join("destructive-ops.log"))
}

/// Classify git errors into appropriate RemoteRepoError variants
fn classify_git_error(stderr: &str, url: &str) -> RemoteRepoError {
    let stderr_lower = stderr.to_lowercase();

    if stderr_lower.contains("authentication failed")
        || stderr_lower.contains("permission denied")
        || stderr_lower.contains("could not read username")
        || stderr_lower.contains("invalid credentials")
        || stderr_lower.contains("fatal: could not read password")
    {
        RemoteRepoError::AuthFailed
    } else if stderr_lower.contains("not found")
        || stderr_lower.contains("does not exist")
        || stderr_lower.contains("repository not found")
        || stderr_lower.contains("fatal: repository")
    {
        RemoteRepoError::NotFound(url.to_string())
    } else if stderr_lower.contains("could not resolve host")
        || stderr_lower.contains("network")
        || stderr_lower.contains("connection")
        || stderr_lower.contains("timeout")
    {
        RemoteRepoError::NetworkError(stderr.to_string())
    } else {
        RemoteRepoError::CloneFailed(stderr.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_cache_path_generation() {
        let temp_dir = TempDir::new().unwrap();
        let manager = RemoteRepoManager::with_cache_dir(temp_dir.path().to_path_buf()).unwrap();

        let source = RepoSource::from_input("https://github.com/user/repo").unwrap();
        let parsed = source.parse_components().unwrap();

        let cache_path = manager.get_cache_path(&parsed);
        assert!(cache_path.to_string_lossy().contains("github.com"));
        assert!(cache_path.to_string_lossy().contains("user"));
        // Standard clone (not bare), so path ends with repo name, not repo.git
        assert!(cache_path.to_string_lossy().ends_with("repo"));
    }

    #[test]
    fn test_is_cached_false_for_nonexistent() {
        let temp_dir = TempDir::new().unwrap();
        let manager = RemoteRepoManager::with_cache_dir(temp_dir.path().to_path_buf()).unwrap();

        let source = RepoSource::from_input("https://github.com/nonexistent/repo").unwrap();
        let parsed = source.parse_components().unwrap();

        assert!(!manager.is_cached(&parsed));
    }

    #[test]
    fn cached_source_path_resolves_only_cached_remotes() {
        let temp_dir = TempDir::new().unwrap();
        let manager = RemoteRepoManager::with_cache_dir(temp_dir.path().to_path_buf()).unwrap();

        // Fake a cached clone: `is_cached` checks for `<cache>/.git` on disk.
        let cached = temp_dir.path().join("github.com").join("owner").join("repo");
        std::fs::create_dir_all(cached.join(".git")).unwrap();

        // Cached remote → resolves to the clone-cache path. This is the seed
        // for the Configure screen's branch-collision guards: without it a
        // remote pick gets empty guard lists and an existing branch name only
        // fails AFTER Launch, at `git worktree add -b` (the feat/ota repro).
        let hit = RepoSource::GithubShorthand {
            owner: "owner".to_string(),
            repo: "repo".to_string(),
        };
        assert_eq!(manager.cached_source_path(&hit), Some(cached));

        // Same shape, never cloned → None (guards backfill async instead).
        let miss = RepoSource::GithubShorthand {
            owner: "owner".to_string(),
            repo: "never-cloned".to_string(),
        };
        assert_eq!(manager.cached_source_path(&miss), None);

        // Non-clonable sources never resolve, even if a path happens to exist.
        let local = RepoSource::LocalPath(temp_dir.path().to_path_buf());
        assert_eq!(manager.cached_source_path(&local), None);
        let ssh_session = RepoSource::SshSession("ssh://user@host".to_string());
        assert_eq!(manager.cached_source_path(&ssh_session), None);
    }

    #[test]
    fn test_list_cached_repos_empty() {
        let temp_dir = TempDir::new().unwrap();
        let manager = RemoteRepoManager::with_cache_dir(temp_dir.path().to_path_buf()).unwrap();

        let repos = manager.list_cached_repos().unwrap();
        assert!(repos.is_empty());
    }

    fn run_git(args: &[&str], cwd: &Path) {
        let out = Command::new("git").args(args).current_dir(cwd).output().unwrap();
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    #[test]
    fn push_initial_commit_initializes_empty_remote_on_its_default_branch() {
        let tmp = TempDir::new().unwrap();
        let bare = tmp.path().join("bare.git");
        // Non-"main" default proves the branch name comes from the remote's
        // advertised HEAD, not a hardcoded fallback.
        run_git(
            &["init", "--bare", "-b", "trunk", bare.to_str().unwrap()],
            tmp.path(),
        );
        let clone = tmp.path().join("clone");
        run_git(
            &["clone", bare.to_str().unwrap(), clone.to_str().unwrap()],
            tmp.path(),
        );
        run_git(&["config", "user.name", "Test"], &clone);
        run_git(&["config", "user.email", "test@example.com"], &clone);

        let branch = push_initial_commit(&clone, "myrepo").unwrap();
        assert_eq!(branch, "trunk");

        // The bare remote now has the branch with the README commit.
        let out = Command::new("git")
            .args(["ls-remote", "--heads", bare.to_str().unwrap()])
            .output()
            .unwrap();
        assert!(
            String::from_utf8_lossy(&out.stdout).contains("refs/heads/trunk"),
            "initial commit did not land on the bare remote"
        );
        assert!(clone.join("README.md").exists());

        // Re-running (e.g. after a failed push retry) must not stack a second
        // commit — just re-push.
        assert_eq!(push_initial_commit(&clone, "myrepo").unwrap(), "trunk");
        let count = Command::new("git")
            .args(["rev-list", "--count", "HEAD"])
            .current_dir(&clone)
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&count.stdout).trim(), "1");
    }

    /// The first block is the repo's own checkout, not a linked worktree, and a
    /// `prunable` block is a registration whose checkout is already gone. Both
    /// must be excluded or the guard latches shut on a repo with no live
    /// worktrees and blocks every later delete.
    #[test]
    fn count_linked_worktrees_skips_the_main_checkout_and_prunable_entries() {
        let main_only = "worktree /repos/widget\nHEAD abc\nbranch refs/heads/main\n";
        assert_eq!(count_linked_worktrees(main_only), 0);

        let one_live = "worktree /repos/widget\nHEAD abc\nbranch refs/heads/main\n\
                        \n\
                        worktree /wt/feature\nHEAD abc\nbranch refs/heads/agents/feature\n";
        assert_eq!(count_linked_worktrees(one_live), 1);

        // The exact shape git emits after the checkout is deleted by hand.
        let stale = "worktree /repos/widget\nHEAD abc\nbranch refs/heads/main\n\
                     \n\
                     worktree /wt/gone\nHEAD abc\nbranch refs/heads/agents/gone\n\
                     prunable gitdir file points to non-existent location\n";
        assert_eq!(
            count_linked_worktrees(stale),
            0,
            "a prunable registration is not a live worktree"
        );

        let mixed = "worktree /repos/widget\nHEAD abc\nbranch refs/heads/main\n\
                     \n\
                     worktree /wt/gone\nHEAD abc\nprunable gitdir file points to non-existent location\n\
                     \n\
                     worktree /wt/live\nHEAD abc\nbranch refs/heads/agents/live\n";
        assert_eq!(count_linked_worktrees(mixed), 1);
    }

    #[test]
    fn test_error_classification_auth() {
        let err = classify_git_error(
            "fatal: Authentication failed for 'https://github.com/private/repo'",
            "url",
        );
        assert!(matches!(err, RemoteRepoError::AuthFailed));
    }

    #[test]
    fn test_error_classification_not_found() {
        let err = classify_git_error(
            "fatal: repository 'https://github.com/user/nonexistent' not found",
            "url",
        );
        assert!(matches!(err, RemoteRepoError::NotFound(_)));
    }

    #[test]
    fn test_error_classification_network() {
        let err = classify_git_error("fatal: Could not resolve host: github.com", "url");
        assert!(matches!(err, RemoteRepoError::NetworkError(_)));
    }
}
