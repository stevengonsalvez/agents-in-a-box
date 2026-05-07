// ABOUTME: Manages cloning and caching of remote git repositories

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;
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
}

impl RemoteRepoManager {
    /// Create a new RemoteRepoManager with default cache directory
    pub fn new() -> Result<Self> {
        let home_dir = dirs::home_dir().context("Failed to get home directory")?;
        let cache_dir = home_dir.join(".agents-in-a-box").join("repos");

        std::fs::create_dir_all(&cache_dir)?;

        Ok(Self { cache_dir })
    }

    /// Create with a custom cache directory (for testing)
    pub fn with_cache_dir(cache_dir: PathBuf) -> Result<Self> {
        std::fs::create_dir_all(&cache_dir)?;
        Ok(Self { cache_dir })
    }

    /// Get the cache directory path
    pub fn cache_dir(&self) -> &Path {
        &self.cache_dir
    }

    /// Get the cache path for a parsed repo (standard clone, not bare)
    pub fn get_cache_path(&self, parsed: &ParsedRepo) -> PathBuf {
        self.cache_dir
            .join(&parsed.host)
            .join(&parsed.owner)
            .join(&parsed.repo_name)
    }

    /// Check if a repo is already cached (standard clone with .git subdirectory)
    pub fn is_cached(&self, parsed: &ParsedRepo) -> bool {
        let cache_path = self.get_cache_path(parsed);
        cache_path.exists() && cache_path.join(".git").exists()
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
    pub fn clone_repo(
        &self,
        source: &RepoSource,
        parsed: &ParsedRepo,
    ) -> Result<PathBuf, RemoteRepoError> {
        let url = source.to_clone_url();
        let cache_path = self.get_cache_path(parsed);

        if self.is_cached(parsed) {
            info!("Repository already cached at: {}", cache_path.display());
            // Fetch updates
            if let Err(e) = self.fetch_updates(&cache_path) {
                warn!("Failed to fetch updates: {}", e);
                // Continue with cached version
            }
            return Ok(cache_path);
        }

        info!("Cloning {} to {}", url, cache_path.display());

        // Create parent directories
        if let Some(parent) = cache_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Standard clone (not --bare) for compatibility with worktree discovery
        let output = Command::new("git")
            .args(["clone", &url])
            .arg(&cache_path)
            .output()
            .map_err(|e| RemoteRepoError::CloneFailed(e.to_string()))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(classify_git_error(&stderr, &url));
        }

        info!("Successfully cloned to: {}", cache_path.display());
        Ok(cache_path)
    }

    /// Fetch updates for a cached repo
    pub fn fetch_updates(&self, cache_path: &Path) -> Result<(), RemoteRepoError> {
        info!("Fetching updates for: {}", cache_path.display());

        let output = Command::new("git")
            .args(["fetch", "--all", "--prune"])
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

    /// Create a worktree from a cached standard clone
    ///
    /// For standard clones, refs are in refs/remotes/origin/{branch},
    /// so we use origin/{base_branch} to create the new branch.
    pub fn create_worktree_from_cache(
        &self,
        cache_path: &Path,
        worktree_path: &Path,
        branch_name: &str,
        base_branch: &str,
    ) -> Result<(), RemoteRepoError> {
        info!(
            "Creating worktree at {} for branch {} (base: {})",
            worktree_path.display(),
            branch_name,
            base_branch
        );

        // Create parent directory for worktree
        if let Some(parent) = worktree_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Check if the new branch already exists locally
        let branch_exists = Command::new("git")
            .args(["rev-parse", "--verify", branch_name])
            .current_dir(cache_path)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);

        if !branch_exists {
            // Standard clone has refs in refs/remotes/origin/{branch}
            // Use origin/{base_branch} shorthand
            let base_ref = format!("origin/{}", base_branch);

            // Verify the remote branch exists
            let ref_check = Command::new("git")
                .args(["rev-parse", "--verify", &base_ref])
                .current_dir(cache_path)
                .output()?;

            if !ref_check.status.success() {
                // Get list of available remote branches for better error message
                let branches_output = Command::new("git")
                    .args(["branch", "-r"])
                    .current_dir(cache_path)
                    .output()
                    .ok();
                let available = branches_output
                    .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
                    .unwrap_or_default();
                let branch_list: Vec<&str> = available.lines()
                    .map(|s| s.trim())
                    .filter(|s| !s.is_empty() && !s.contains("->"))
                    .collect();

                return Err(RemoteRepoError::InvalidRepo(format!(
                    "Base branch 'origin/{}' not found. Available branches: {}",
                    base_branch,
                    if branch_list.is_empty() { "(none)".to_string() } else { branch_list.join(", ") }
                )));
            }

            // Create new local branch from the remote tracking branch
            let output = Command::new("git")
                .args(["branch", branch_name, &base_ref])
                .current_dir(cache_path)
                .output()?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                // Branch might already exist, which is okay
                if !stderr.contains("already exists") {
                    return Err(RemoteRepoError::InvalidRepo(format!(
                        "Failed to create branch '{}': {}",
                        branch_name, stderr
                    )));
                }
            }
        }

        // Create the worktree - try normal first, fallback to --no-checkout if filters fail
        let output = Command::new("git")
            .args([
                "worktree",
                "add",
                worktree_path.to_string_lossy().as_ref(),
                branch_name,
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

                // Retry with --no-checkout to skip the problematic filter
                let retry_output = Command::new("git")
                    .args([
                        "worktree",
                        "add",
                        "--no-checkout",
                        worktree_path.to_string_lossy().as_ref(),
                        branch_name,
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
                        "-c", "filter.crypt.smudge=cat",
                        "-c", "filter.crypt.clean=cat",
                        "checkout",
                        "--force",
                    ])
                    .current_dir(worktree_path)
                    .output()?;

                if !checkout_output.status.success() {
                    let checkout_stderr = String::from_utf8_lossy(&checkout_output.stderr);
                    warn!("Checkout with filter bypass had issues: {}", checkout_stderr);
                    // Continue anyway - the worktree exists, files just might not be checked out
                }

                info!(
                    "Created worktree with filter bypass at: {}",
                    worktree_path.display()
                );
            } else {
                return Err(RemoteRepoError::CloneFailed(format!(
                    "Failed to create worktree: {}",
                    stderr
                )));
            }
        }

        info!(
            "Successfully created worktree at: {}",
            worktree_path.display()
        );
        Ok(())
    }

    /// Checkout an existing remote branch into a worktree
    ///
    /// Unlike create_worktree_from_cache which creates a new local branch,
    /// this creates a local tracking branch for an existing remote branch.
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
                        "-c", "filter.crypt.smudge=cat",
                        "-c", "filter.crypt.clean=cat",
                        "checkout",
                        "--force",
                    ])
                    .current_dir(worktree_path)
                    .output()?;

                if !checkout_output.status.success() {
                    let checkout_stderr = String::from_utf8_lossy(&checkout_output.stderr);
                    warn!("Checkout with filter bypass had issues: {}", checkout_stderr);
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
                let worktree_dir = worktree_path.file_name()
                    .map(|n| format!("{}-{}", n.to_string_lossy(), suffix))
                    .unwrap_or_else(|| format!("worktree-{}", suffix));
                let suffixed_worktree_path = worktree_path.parent()
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
                    if retry_stderr.contains("smudge filter") || retry_stderr.contains("clean filter") {
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
                            let no_checkout_stderr = String::from_utf8_lossy(&no_checkout_output.stderr);

                            // Check if the suffixed branch already exists
                            if no_checkout_stderr.contains("already exists") {
                                info!(
                                    "Suffixed branch '{}' already exists, looking for existing worktree",
                                    suffixed_branch
                                );

                                // Find worktree for this branch
                                if let Some(result) = find_worktree_for_branch(cache_path, &suffixed_branch)? {
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
                                    let new_suffix: String = uuid::Uuid::new_v4().to_string().chars().take(8).collect();
                                    let new_suffixed_branch = format!("{}-{}", remote_branch, new_suffix);
                                    let new_worktree_dir = worktree_path.file_name()
                                        .map(|n| format!("{}-{}", n.to_string_lossy(), new_suffix))
                                        .unwrap_or_else(|| format!("worktree-{}", new_suffix));
                                    let new_suffixed_worktree_path = worktree_path.parent()
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
                                            .args(["-c", "filter.crypt.smudge=cat", "-c", "filter.crypt.clean=cat", "checkout", "--force"])
                                            .current_dir(&new_suffixed_worktree_path)
                                            .output();

                                        // Set up tracking
                                        let _ = Command::new("git")
                                            .args(["branch", "--set-upstream-to", &remote_ref, &new_suffixed_branch])
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
                            }

                            return Err(RemoteRepoError::CloneFailed(format!(
                                "Failed to create suffixed worktree (even with --no-checkout): {}",
                                no_checkout_stderr
                            )));
                        }

                        // Checkout files with filter bypass
                        let checkout_output = Command::new("git")
                            .args([
                                "-c", "filter.crypt.smudge=cat",
                                "-c", "filter.crypt.clean=cat",
                                "checkout",
                                "--force",
                            ])
                            .current_dir(&suffixed_worktree_path)
                            .output()?;

                        if !checkout_output.status.success() {
                            let checkout_stderr = String::from_utf8_lossy(&checkout_output.stderr);
                            warn!("Checkout with filter bypass had issues: {}", checkout_stderr);
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

                        if let Some(result) = find_worktree_for_branch(cache_path, &suffixed_branch)? {
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
                            let new_suffix: String = uuid::Uuid::new_v4().to_string().chars().take(8).collect();
                            let new_suffixed_branch = format!("{}-{}", remote_branch, new_suffix);
                            let new_worktree_dir = worktree_path.file_name()
                                .map(|n| format!("{}-{}", n.to_string_lossy(), new_suffix))
                                .unwrap_or_else(|| format!("worktree-{}", new_suffix));
                            let new_suffixed_worktree_path = worktree_path.parent()
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
                                    .args(["branch", "--set-upstream-to", &remote_ref, &new_suffixed_branch])
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
                                    let repo_name = repo_entry.file_name().to_string_lossy().to_string();
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

    /// Remove a cached repository
    pub fn remove_cached_repo(&self, parsed: &ParsedRepo) -> Result<(), RemoteRepoError> {
        let cache_path = self.get_cache_path(parsed);

        if cache_path.exists() {
            std::fs::remove_dir_all(&cache_path)?;
            info!("Removed cached repo: {}", cache_path.display());
        }

        Ok(())
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
    let _ = Command::new("git")
        .args(["worktree", "prune"])
        .current_dir(cache_path)
        .output();

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
    fn test_list_cached_repos_empty() {
        let temp_dir = TempDir::new().unwrap();
        let manager = RemoteRepoManager::with_cache_dir(temp_dir.path().to_path_buf()).unwrap();

        let repos = manager.list_cached_repos().unwrap();
        assert!(repos.is_empty());
    }

    #[test]
    fn test_error_classification_auth() {
        let err = classify_git_error("fatal: Authentication failed for 'https://github.com/private/repo'", "url");
        assert!(matches!(err, RemoteRepoError::AuthFailed));
    }

    #[test]
    fn test_error_classification_not_found() {
        let err = classify_git_error("fatal: repository 'https://github.com/user/nonexistent' not found", "url");
        assert!(matches!(err, RemoteRepoError::NotFound(_)));
    }

    #[test]
    fn test_error_classification_network() {
        let err = classify_git_error("fatal: Could not resolve host: github.com", "url");
        assert!(matches!(err, RemoteRepoError::NetworkError(_)));
    }
}
