// ABOUTME: Shared test fixtures and utilities for behavioral tests
//
// Provides:
// - TestRepo: Temporary git repository for testing
// - tmux_available(): Check if tmux is installed
// - require_tmux!(): Skip test if tmux unavailable

use anyhow::Result;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;

/// Creates a temporary git repository with initial commit
pub struct TestRepo {
    pub dir: TempDir,
    pub path: PathBuf,
}

impl TestRepo {
    /// Create a new temporary git repository with initial commit
    pub fn new() -> Result<Self> {
        let dir = TempDir::new()?;
        let path = dir.path().to_path_buf();

        // Initialize git repo
        let output = Command::new("git")
            .args(["init"])
            .current_dir(&path)
            .output()?;
        if !output.status.success() {
            anyhow::bail!("git init failed: {}", String::from_utf8_lossy(&output.stderr));
        }

        // Configure git user for commits
        let output = Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(&path)
            .output()?;
        if !output.status.success() {
            anyhow::bail!("git config email failed: {}", String::from_utf8_lossy(&output.stderr));
        }

        let output = Command::new("git")
            .args(["config", "user.name", "Test User"])
            .current_dir(&path)
            .output()?;
        if !output.status.success() {
            anyhow::bail!("git config name failed: {}", String::from_utf8_lossy(&output.stderr));
        }

        // Create initial file and commit
        std::fs::write(path.join("README.md"), "# Test Repo\n")?;

        let output = Command::new("git")
            .args(["add", "."])
            .current_dir(&path)
            .output()?;
        if !output.status.success() {
            anyhow::bail!("git add failed: {}", String::from_utf8_lossy(&output.stderr));
        }

        let output = Command::new("git")
            .args(["commit", "-m", "Initial commit"])
            .current_dir(&path)
            .output()?;
        if !output.status.success() {
            anyhow::bail!("git commit failed: {}", String::from_utf8_lossy(&output.stderr));
        }

        Ok(Self { dir, path })
    }

    /// Get the path to the repository
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Add a file and commit it
    pub fn add_commit(&self, filename: &str, content: &str, message: &str) -> Result<()> {
        std::fs::write(self.path.join(filename), content)?;

        let output = Command::new("git")
            .args(["add", filename])
            .current_dir(&self.path)
            .output()?;
        if !output.status.success() {
            anyhow::bail!("git add failed: {}", String::from_utf8_lossy(&output.stderr));
        }

        let output = Command::new("git")
            .args(["commit", "-m", message])
            .current_dir(&self.path)
            .output()?;
        if !output.status.success() {
            anyhow::bail!("git commit failed: {}", String::from_utf8_lossy(&output.stderr));
        }

        Ok(())
    }

    /// Get current branch name
    pub fn current_branch(&self) -> Result<String> {
        let output = Command::new("git")
            .args(["branch", "--show-current"])
            .current_dir(&self.path)
            .output()?;

        if !output.status.success() {
            anyhow::bail!("git branch failed: {}", String::from_utf8_lossy(&output.stderr));
        }

        Ok(String::from_utf8(output.stdout)?.trim().to_string())
    }

    /// Create and checkout a new branch
    pub fn create_branch(&self, branch_name: &str) -> Result<()> {
        let output = Command::new("git")
            .args(["checkout", "-b", branch_name])
            .current_dir(&self.path)
            .output()?;
        if !output.status.success() {
            anyhow::bail!("git checkout -b failed: {}", String::from_utf8_lossy(&output.stderr));
        }
        Ok(())
    }

    /// Checkout an existing branch
    pub fn checkout(&self, branch_name: &str) -> Result<()> {
        let output = Command::new("git")
            .args(["checkout", branch_name])
            .current_dir(&self.path)
            .output()?;
        if !output.status.success() {
            anyhow::bail!("git checkout failed: {}", String::from_utf8_lossy(&output.stderr));
        }
        Ok(())
    }
}

/// Check if tmux is available on the system
pub fn tmux_available() -> bool {
    Command::new("tmux")
        .args(["-V"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Macro to skip test if tmux is not available
#[macro_export]
macro_rules! require_tmux {
    () => {
        if !super::fixtures::tmux_available() {
            eprintln!("Skipping test: tmux not available");
            return Ok(());
        }
    };
}

/// Helper to clean up a tmux session by name
pub fn cleanup_tmux_session(name: &str) {
    let _ = Command::new("tmux")
        .args(["kill-session", "-t", name])
        .output();
}

/// Check if a tmux session exists
pub fn tmux_session_exists(name: &str) -> bool {
    Command::new("tmux")
        .args(["has-session", "-t", name])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Capture tmux pane content
pub fn capture_tmux_pane(session_name: &str) -> Result<String> {
    let output = Command::new("tmux")
        .args(["capture-pane", "-t", session_name, "-p"])
        .output()?;

    Ok(String::from_utf8(output.stdout)?)
}

/// Send keys to a tmux session
pub fn send_tmux_keys(session_name: &str, keys: &str) -> Result<()> {
    Command::new("tmux")
        .args(["send-keys", "-t", session_name, keys])
        .output()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_repo_creation() -> Result<()> {
        let repo = TestRepo::new()?;
        assert!(repo.path().exists());
        assert!(repo.path().join(".git").exists());
        assert!(repo.path().join("README.md").exists());
        Ok(())
    }

    #[test]
    fn test_repo_add_commit() -> Result<()> {
        let repo = TestRepo::new()?;
        repo.add_commit("test.txt", "hello", "Add test file")?;
        assert!(repo.path().join("test.txt").exists());
        Ok(())
    }
}
