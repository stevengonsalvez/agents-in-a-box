// ABOUTME: Shared git operations for commit and push functionality - ensures DRY compliance

use anyhow::Result;
use std::path::Path;
use std::process::Command;
use tracing::{debug, error};

/// Core git commit and push operation that can be used by both git view and quick commit
pub fn commit_and_push_changes(worktree_path: &Path, commit_message: &str) -> Result<String> {
    debug!(
        "Committing and pushing changes for worktree: {:?}",
        worktree_path
    );

    if commit_message.trim().is_empty() {
        return Err(anyhow::anyhow!("Commit message cannot be empty"));
    }

    // Try CLI git first as it's more reliable
    debug!("=== Using CLI git for commit and push ===");
    match commit_and_push_cli(worktree_path, commit_message) {
        Ok(result) => {
            debug!("✓ CLI git succeeded!");
            return Ok(result);
        }
        Err(e) => {
            debug!("✗ CLI git failed: {}, falling back to git2", e);
        }
    }

    // Fallback to git2 implementation
    debug!("=== Falling back to git2 implementation ===");
    commit_and_push_git2(worktree_path, commit_message)
}

fn commit_and_push_cli(worktree_path: &Path, commit_message: &str) -> Result<String> {
    debug!("Using CLI git for commit and push");

    // Store original directory
    let original_dir = std::env::current_dir()?;

    let result = (|| -> Result<String> {
        // Change to worktree directory
        std::env::set_current_dir(worktree_path)?;

        // Add all changes
        debug!("Adding all changes...");
        let add_output = Command::new("git")
            .args(&["add", "."])
            .output()?;

        if !add_output.status.success() {
            let stderr = String::from_utf8_lossy(&add_output.stderr);
            return Err(anyhow::anyhow!("git add failed: {}", stderr));
        }

        // Commit with --no-gpg-sign to avoid hanging on GPG passphrase
        debug!("Committing with message: {}", commit_message);
        let commit_output = Command::new("git")
            .args(&["commit", "--no-gpg-sign", "-m", commit_message])
            .output()?;

        if !commit_output.status.success() {
            let stderr = String::from_utf8_lossy(&commit_output.stderr);
            // Check if it's "nothing to commit" which isn't really an error
            if stderr.contains("nothing to commit") || stderr.contains("no changes added") {
                return Err(anyhow::anyhow!("Nothing to commit - no changes staged"));
            }
            return Err(anyhow::anyhow!("git commit failed: {}", stderr));
        }

        // Push changes - let git use its configured credential helper
        // Don't set GIT_TERMINAL_PROMPT=0 as it breaks credential helpers
        debug!("Pushing changes...");
        let push_output = Command::new("git")
            .args(&["push"])
            .output()?;

        if !push_output.status.success() {
            let stderr = String::from_utf8_lossy(&push_output.stderr);
            let stdout = String::from_utf8_lossy(&push_output.stdout);
            error!("git push failed - stderr: {}", stderr);
            error!("git push failed - stdout: {}", stdout);

            // Provide user-friendly error messages
            if stderr.contains("Authentication failed") || stderr.contains("could not read Username") {
                return Err(anyhow::anyhow!(
                    "Push failed: Authentication required. Please configure git credentials:\n\
                     • For HTTPS: Run 'git config --global credential.helper osxkeychain' (macOS) or 'git config --global credential.helper cache' (Linux)\n\
                     • For SSH: Ensure your SSH key is added to ssh-agent"
                ));
            }
            return Err(anyhow::anyhow!("git push failed: {}", stderr));
        }

        debug!("CLI git push succeeded");
        Ok(format!("Committed and pushed: {}", commit_message))
    })();

    // Always restore original directory
    std::env::set_current_dir(original_dir)?;

    result
}

fn commit_and_push_git2(worktree_path: &Path, commit_message: &str) -> Result<String> {
    use git2::{Repository, Signature, CredentialType};

    let repo = Repository::open(worktree_path)?;

    // Stage all changes
    let mut index = repo.index()?;

    // Add all files in the working directory
    index.add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)?;
    index.write()?;

    // Try to get user config for commit signature
    let config = repo.config().ok();
    let user_name = config
        .as_ref()
        .and_then(|c| c.get_string("user.name").ok())
        .unwrap_or_else(|| "Claude Box".to_string());
    let user_email = config
        .as_ref()
        .and_then(|c| c.get_string("user.email").ok())
        .unwrap_or_else(|| "claude-box@local".to_string());

    // Create commit
    let signature = Signature::now(&user_name, &user_email)?;
    let tree_id = index.write_tree()?;
    let tree = repo.find_tree(tree_id)?;

    // Get parent commit
    let parent_commit = match repo.head() {
        Ok(head) => {
            let oid = head.target().unwrap();
            Some(repo.find_commit(oid)?)
        }
        Err(_) => None, // Initial commit
    };

    let parents: Vec<&git2::Commit> = match &parent_commit {
        Some(commit) => vec![commit],
        None => vec![],
    };

    let commit_id = repo.commit(
        Some("HEAD"),
        &signature,
        &signature,
        commit_message,
        &tree,
        &parents,
    )?;

    debug!("Created commit: {}", commit_id);

    // Push to remote
    let mut remote = repo.find_remote("origin")?;
    let head_ref = repo.head()?;
    let branch = head_ref.shorthand().unwrap_or("main");
    let refspec = format!("refs/heads/{}:refs/heads/{}", branch, branch);

    // Set up credentials callback that handles both SSH and HTTPS
    let mut callbacks = git2::RemoteCallbacks::new();
    callbacks.credentials(|url, username_from_url, allowed_types| {
        debug!("Credential callback: url={}, username={:?}, allowed={:?}",
               url, username_from_url, allowed_types);

        // Try SSH key from agent first (for git@github.com URLs)
        if allowed_types.contains(CredentialType::SSH_KEY) {
            debug!("Trying SSH key from agent");
            if let Ok(cred) = git2::Cred::ssh_key_from_agent(username_from_url.unwrap_or("git")) {
                return Ok(cred);
            }
        }

        // Try default credentials (for HTTPS with credential helper)
        if allowed_types.contains(CredentialType::USER_PASS_PLAINTEXT) {
            debug!("Trying default credentials for HTTPS");
            // Try to use git credential helper via command line
            if let Some(creds) = get_git_credentials(url) {
                return git2::Cred::userpass_plaintext(&creds.0, &creds.1);
            }
        }

        // Try default (for systems with credential managers)
        if allowed_types.contains(CredentialType::DEFAULT) {
            debug!("Trying default credentials");
            return git2::Cred::default();
        }

        Err(git2::Error::from_str("No suitable credentials found"))
    });

    let mut push_options = git2::PushOptions::new();
    push_options.remote_callbacks(callbacks);

    match remote.push(&[&refspec], Some(&mut push_options)) {
        Ok(_) => {
            debug!("git2 push succeeded");
            Ok(format!("Committed and pushed: {}", commit_message))
        }
        Err(e) => {
            let user_friendly_msg = match e.code() {
                git2::ErrorCode::Auth => {
                    "Authentication failed. Please configure git credentials:\n\
                     • For HTTPS: git config --global credential.helper osxkeychain (macOS)\n\
                     • For SSH: Ensure SSH key is added to ssh-agent"
                }
                _ => "Push failed. Please check your remote repository configuration.",
            };

            error!("git2 push failed: {}", e);
            Err(anyhow::anyhow!("Push failed: {}", user_friendly_msg))
        }
    }
}

/// Try to get credentials from git's credential helper
fn get_git_credentials(url: &str) -> Option<(String, String)> {
    use std::io::Write;
    use url::Url;

    // Parse the URL using the url crate for robustness
    let parsed = Url::parse(url).ok()?;
    let protocol = parsed.scheme();
    let host = parsed.host_str()?;

    // Only handle http/https URLs
    if protocol != "http" && protocol != "https" {
        return None;
    }

    // Ask git credential helper for credentials
    let mut child = match Command::new("git")
        .args(&["credential", "fill"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return None,
    };

    // Write credential request to stdin
    if let Some(mut stdin) = child.stdin.take() {
        let input = format!("protocol={}\nhost={}\n\n", protocol, host);
        if stdin.write_all(input.as_bytes()).is_err() {
            return None;
        }
    }

    // Read response
    let output = match child.wait_with_output() {
        Ok(o) => o,
        Err(_) => return None,
    };

    if !output.status.success() {
        return None;
    }

    // Parse response
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut username = None;
    let mut password = None;

    for line in stdout.lines() {
        if let Some(user) = line.strip_prefix("username=") {
            username = Some(user.to_string());
        } else if let Some(pass) = line.strip_prefix("password=") {
            password = Some(pass.to_string());
        }
    }

    match (username, password) {
        (Some(u), Some(p)) => Some((u, p)),
        _ => None,
    }
}

/// Information about a single commit
#[derive(Debug, Clone)]
pub struct CommitInfo {
    pub hash_short: String,
    pub author: String,
    pub date: String,
    pub message: String,
}

/// Get commits on the current branch that aren't on main/master
/// Returns commits in reverse chronological order (newest first)
pub fn get_branch_commits(worktree_path: &Path, limit: usize) -> Result<Vec<CommitInfo>> {
    use git2::Repository;

    let repo = Repository::open(worktree_path)?;

    // Get current branch HEAD
    let head = repo.head()?;
    let head_oid = head.target().ok_or_else(|| anyhow::anyhow!("No HEAD target"))?;

    // Try to find merge-base with main/master to show only branch-specific commits
    let merge_base = find_merge_base_with_main(&repo, head_oid);

    // Walk commits from HEAD
    let mut revwalk = repo.revwalk()?;
    revwalk.push(head_oid)?;
    revwalk.set_sorting(git2::Sort::TIME)?;

    let mut commits = Vec::new();
    for oid_result in revwalk.take(limit) {
        let oid = oid_result?;

        // Stop at merge-base (only show branch-specific commits)
        if let Some(base) = merge_base {
            if oid == base {
                break;
            }
        }

        let commit = repo.find_commit(oid)?;
        commits.push(CommitInfo {
            hash_short: format!("{:.7}", oid),
            author: commit.author().name().unwrap_or("Unknown").to_string(),
            date: format_relative_time(commit.time().seconds()),
            message: commit.summary().unwrap_or("").to_string(),
        });
    }

    Ok(commits)
}

/// Find merge-base with main or master branch
fn find_merge_base_with_main(repo: &git2::Repository, head_oid: git2::Oid) -> Option<git2::Oid> {
    // Try main first, then master
    let main_branch = repo
        .find_branch("main", git2::BranchType::Local)
        .or_else(|_| repo.find_branch("master", git2::BranchType::Local))
        .ok()?;

    let main_oid = main_branch.get().target()?;

    // If we're on main/master, no merge-base needed
    if head_oid == main_oid {
        return None;
    }

    repo.merge_base(head_oid, main_oid).ok()
}

/// Get the diff for a specific commit
pub fn get_commit_diff(worktree_path: &Path, commit_hash: &str) -> Result<Vec<String>> {
    // Use git show to get the commit diff - more reliable than git2 for this
    let output = Command::new("git")
        .current_dir(worktree_path)
        .args(&["show", "--stat", "--patch", commit_hash])
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow::anyhow!("Failed to get commit diff: {}", stderr));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(stdout.lines().map(|s| s.to_string()).collect())
}

/// Format timestamp as relative time (e.g., "2 hours ago")
fn format_relative_time(timestamp: i64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let diff = now - timestamp;

    match diff {
        d if d < 0 => "in the future".to_string(),
        d if d < 60 => "just now".to_string(),
        d if d < 3600 => format!("{}m ago", d / 60),
        d if d < 86400 => format!("{}h ago", d / 3600),
        d if d < 604800 => format!("{}d ago", d / 86400),
        d => format!("{}w ago", d / 604800),
    }
}
