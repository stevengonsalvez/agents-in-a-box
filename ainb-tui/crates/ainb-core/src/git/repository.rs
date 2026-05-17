// ABOUTME: Git repository operations and management utilities

#![allow(dead_code)]

use anyhow::{Context, Result};
use git2::{Repository, Status, StatusOptions};
use std::path::Path;
use thiserror::Error;
use tracing::debug;

use crate::models::GitChanges;

#[derive(Error, Debug)]
pub enum GitError {
    #[error("Git repository error: {0}")]
    Git(#[from] git2::Error),
    #[error("Repository not found at path: {0}")]
    NotFound(String),
    #[error("Invalid repository state: {0}")]
    InvalidState(String),
}

pub struct RepositoryManager {
    repo: Repository,
}

impl RepositoryManager {
    pub fn open(path: &Path) -> Result<Self, GitError> {
        let repo = Repository::open(path)
            .with_context(|| format!("Failed to open repository at {}", path.display()))
            .map_err(|e| GitError::NotFound(e.to_string()))?;

        Ok(Self { repo })
    }

    pub fn get_status(&self) -> Result<GitChanges, GitError> {
        let mut opts = StatusOptions::new();
        opts.include_untracked(true);
        opts.include_ignored(false);

        let statuses = self.repo.statuses(Some(&mut opts))?;

        let mut changes = GitChanges::default();

        for entry in statuses.iter() {
            let status = entry.status();

            if status.contains(Status::WT_NEW) || status.contains(Status::INDEX_NEW) {
                changes.added += 1;
            }

            if status.contains(Status::WT_MODIFIED) || status.contains(Status::INDEX_MODIFIED) {
                changes.modified += 1;
            }

            if status.contains(Status::WT_DELETED) || status.contains(Status::INDEX_DELETED) {
                changes.deleted += 1;
            }

            // Handle renamed files (count as modified)
            if status.contains(Status::WT_RENAMED) || status.contains(Status::INDEX_RENAMED) {
                changes.modified += 1;
            }
        }

        debug!(
            "Repository status: +{} ~{} -{}",
            changes.added, changes.modified, changes.deleted
        );
        Ok(changes)
    }

    pub fn get_current_branch(&self) -> Result<String, GitError> {
        let head = self.repo.head()?;

        if let Some(branch_name) = head.shorthand() {
            Ok(branch_name.to_string())
        } else {
            Ok("HEAD".to_string()) // Detached HEAD state
        }
    }

    pub fn get_remote_url(&self) -> Result<Option<String>, GitError> {
        match self.repo.find_remote("origin") {
            Ok(remote) => Ok(remote.url().map(|s| s.to_string())),
            Err(_) => {
                // No origin remote found
                Ok(None)
            }
        }
    }

    pub fn is_clean(&self) -> Result<bool, GitError> {
        let changes = self.get_status()?;
        Ok(changes.total() == 0)
    }

    pub fn get_last_commit_message(&self) -> Result<Option<String>, GitError> {
        match self.repo.head() {
            Ok(head) => match head.peel_to_commit() {
                Ok(commit) => {
                    if let Some(message) = commit.message() {
                        Ok(Some(message.to_string()))
                    } else {
                        Ok(None)
                    }
                }
                Err(_) => Ok(None),
            },
            Err(_) => Ok(None), // No commits yet
        }
    }

    pub fn get_commit_count(&self) -> Result<usize, GitError> {
        match self.repo.head() {
            Ok(_head) => {
                let mut revwalk = self.repo.revwalk()?;
                revwalk.push_head()?;
                Ok(revwalk.count())
            }
            Err(_) => Ok(0), // No commits yet
        }
    }

    pub fn has_uncommitted_changes(&self) -> Result<bool, GitError> {
        let changes = self.get_status()?;
        Ok(changes.total() > 0)
    }

    pub fn get_stash_count(&mut self) -> Result<usize, GitError> {
        let mut count = 0;

        self.repo.stash_foreach(|_index, _message, _oid| {
            count += 1;
            true // Continue iteration
        })?;

        Ok(count)
    }

    pub fn validate_repository_health(&self) -> Result<Vec<String>, GitError> {
        let mut issues = Vec::new();

        // Check if repository is bare
        if self.repo.is_bare() {
            issues.push("Repository is bare".to_string());
        }

        // Check if we can access HEAD
        if self.repo.head().is_err() {
            issues.push("Cannot access repository HEAD".to_string());
        }

        // Check for unresolved merge conflicts
        if self.repo.state() != git2::RepositoryState::Clean {
            issues.push(format!("Repository is in {:?} state", self.repo.state()));
        }

        // Check for missing worktree
        if let Some(workdir) = self.repo.workdir() {
            if !workdir.exists() {
                issues.push("Working directory does not exist".to_string());
            }
        } else {
            issues.push("Repository has no working directory".to_string());
        }

        Ok(issues)
    }

    pub fn get_repository_path(&self) -> &Path {
        self.repo.path()
    }

    pub fn get_workdir_path(&self) -> Option<&Path> {
        self.repo.workdir()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn create_test_repo_with_content(path: &Path) -> Result<Repository> {
        let repo = Repository::init(path)?;

        // Create a test file
        let test_file = path.join("test.txt");
        fs::write(&test_file, "test content")?;

        // Add and commit the file
        let mut index = repo.index()?;
        index.add_path(Path::new("test.txt"))?;
        index.write()?;

        let signature = git2::Signature::now("Test User", "test@example.com")?;
        let tree_id = index.write_tree()?;
        let tree = repo.find_tree(tree_id)?;

        repo.commit(
            Some("HEAD"),
            &signature,
            &signature,
            "Initial commit",
            &tree,
            &[],
        )?;

        // Drop tree before returning repo
        drop(tree);
        Ok(repo)
    }

    #[test]
    fn test_repository_manager_open() {
        let temp_dir = TempDir::new().unwrap();
        create_test_repo_with_content(temp_dir.path()).unwrap();

        let manager = RepositoryManager::open(temp_dir.path());
        assert!(manager.is_ok());
    }

    #[test]
    fn test_repository_manager_open_invalid_path() {
        let temp_dir = TempDir::new().unwrap();

        let manager = RepositoryManager::open(temp_dir.path());
        assert!(manager.is_err());
    }

    #[test]
    fn test_get_status_clean_repo() {
        let temp_dir = TempDir::new().unwrap();
        create_test_repo_with_content(temp_dir.path()).unwrap();

        let manager = RepositoryManager::open(temp_dir.path()).unwrap();
        let status = manager.get_status().unwrap();

        assert_eq!(status.total(), 0);
    }

    #[test]
    fn test_get_status_with_changes() {
        let temp_dir = TempDir::new().unwrap();
        create_test_repo_with_content(temp_dir.path()).unwrap();

        // Create a new file
        let new_file = temp_dir.path().join("new.txt");
        fs::write(&new_file, "new content").unwrap();

        // Modify existing file
        let test_file = temp_dir.path().join("test.txt");
        fs::write(&test_file, "modified content").unwrap();

        let manager = RepositoryManager::open(temp_dir.path()).unwrap();
        let status = manager.get_status().unwrap();

        assert!(status.total() > 0);
        assert!(status.added > 0); // new.txt
        assert!(status.modified > 0); // test.txt
    }

    #[test]
    fn test_get_current_branch() {
        let temp_dir = TempDir::new().unwrap();
        create_test_repo_with_content(temp_dir.path()).unwrap();

        let manager = RepositoryManager::open(temp_dir.path()).unwrap();
        let branch = manager.get_current_branch().unwrap();

        assert!(!branch.is_empty());
        // Default branch is usually "master" or "main"
        assert!(branch == "master" || branch == "main");
    }

    #[test]
    fn test_is_clean() {
        let temp_dir = TempDir::new().unwrap();
        create_test_repo_with_content(temp_dir.path()).unwrap();

        let manager = RepositoryManager::open(temp_dir.path()).unwrap();
        assert!(manager.is_clean().unwrap());

        // Add a new file
        let new_file = temp_dir.path().join("dirty.txt");
        fs::write(&new_file, "content").unwrap();

        assert!(!manager.is_clean().unwrap());
    }

    #[test]
    fn test_get_commit_count() {
        let temp_dir = TempDir::new().unwrap();
        create_test_repo_with_content(temp_dir.path()).unwrap();

        let manager = RepositoryManager::open(temp_dir.path()).unwrap();
        let count = manager.get_commit_count().unwrap();

        assert_eq!(count, 1); // We created one commit
    }

    #[test]
    fn test_validate_repository_health() {
        let temp_dir = TempDir::new().unwrap();
        create_test_repo_with_content(temp_dir.path()).unwrap();

        let manager = RepositoryManager::open(temp_dir.path()).unwrap();
        let issues = manager.validate_repository_health().unwrap();

        assert!(issues.is_empty()); // Healthy repository should have no issues
    }
}
