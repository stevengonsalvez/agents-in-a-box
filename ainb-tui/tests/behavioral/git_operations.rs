// ABOUTME: Behavioral tests for git repository operations using real temporary repos
// Tests verify git status detection, branch operations, and change tracking

use anyhow::Result;
use std::fs;
use std::process::Command;

use ainb::git::repository::RepositoryManager;
use ainb::models::GitChanges;

use super::fixtures::TestRepo;

/// Fresh repository should be detected as clean (no uncommitted changes)
#[test]
fn test_repository_detects_clean_state() -> Result<()> {
    // Arrange: Create a fresh repo with initial commit
    let repo = TestRepo::new()?;

    // Act: Open and check status
    let manager = RepositoryManager::open(repo.path())?;
    let is_clean = manager.is_clean()?;
    let changes = manager.get_status()?;

    // Assert: Should be clean with zero changes
    assert!(is_clean, "Fresh repository should be clean");
    assert_eq!(changes.total(), 0, "No changes expected in fresh repo");
    assert_eq!(changes.added, 0);
    assert_eq!(changes.modified, 0);
    assert_eq!(changes.deleted, 0);

    Ok(())
}

/// Modified file should be detected in repository status
#[test]
fn test_repository_detects_modified_files() -> Result<()> {
    // Arrange: Create repo and modify existing file
    let repo = TestRepo::new()?;
    let readme_path = repo.path().join("README.md");

    // Act: Modify the README that was created in initial commit
    fs::write(&readme_path, "# Modified Content\nThis has been changed.")?;

    let manager = RepositoryManager::open(repo.path())?;
    let changes = manager.get_status()?;

    // Assert: Should detect one modified file
    assert!(
        changes.modified >= 1,
        "Expected at least 1 modified file, got {}",
        changes.modified
    );
    assert!(
        !manager.is_clean()?,
        "Repository should not be clean after modification"
    );

    Ok(())
}

/// Untracked (new) files should be counted as added
#[test]
fn test_repository_detects_new_files() -> Result<()> {
    // Arrange: Create repo
    let repo = TestRepo::new()?;

    // Act: Add new untracked files
    fs::write(repo.path().join("new_file.txt"), "New content")?;
    fs::write(repo.path().join("another_new.rs"), "fn main() {}")?;

    let manager = RepositoryManager::open(repo.path())?;
    let changes = manager.get_status()?;

    // Assert: Should detect new files as added
    assert!(
        changes.added >= 2,
        "Expected at least 2 added files, got {}",
        changes.added
    );
    assert!(
        changes.total() >= 2,
        "Total changes should be at least 2"
    );

    Ok(())
}

/// Deleted files should be counted in repository status
#[test]
fn test_repository_detects_deleted_files() -> Result<()> {
    // Arrange: Create repo with multiple committed files
    let repo = TestRepo::new()?;
    repo.add_commit("to_delete.txt", "This will be deleted", "Add file to delete")?;
    repo.add_commit("also_delete.txt", "Also going away", "Add another file")?;

    // Verify files exist before deletion
    assert!(repo.path().join("to_delete.txt").exists());
    assert!(repo.path().join("also_delete.txt").exists());

    // Act: Delete the committed files
    fs::remove_file(repo.path().join("to_delete.txt"))?;
    fs::remove_file(repo.path().join("also_delete.txt"))?;

    let manager = RepositoryManager::open(repo.path())?;
    let changes = manager.get_status()?;

    // Assert: Should detect deleted files
    assert!(
        changes.deleted >= 2,
        "Expected at least 2 deleted files, got {}",
        changes.deleted
    );

    Ok(())
}

/// Should correctly detect current branch name
#[test]
fn test_repository_current_branch_detection() -> Result<()> {
    // Arrange: Create repo (starts on master or main)
    let repo = TestRepo::new()?;

    // Act: Get branch from RepositoryManager
    let manager = RepositoryManager::open(repo.path())?;
    let branch = manager.get_current_branch()?;

    // Assert: Should match default branch (master or main)
    let expected_branch = repo.current_branch()?;
    assert_eq!(
        branch, expected_branch,
        "Branch from RepositoryManager should match git branch"
    );
    assert!(
        branch == "master" || branch == "main",
        "Default branch should be 'master' or 'main', got '{}'",
        branch
    );

    // Test with custom branch
    repo.create_branch("feature/test-branch")?;
    let manager = RepositoryManager::open(repo.path())?;
    let new_branch = manager.get_current_branch()?;

    assert_eq!(
        new_branch, "feature/test-branch",
        "Should detect switched branch"
    );

    Ok(())
}

/// GitChanges total() and format() should work correctly
#[test]
fn test_git_changes_total_and_format() -> Result<()> {
    // Test default (no changes)
    let empty = GitChanges::default();
    assert_eq!(empty.total(), 0);
    assert_eq!(empty.format(), "No changes");

    // Test with values
    let changes = GitChanges {
        added: 3,
        modified: 5,
        deleted: 2,
    };
    assert_eq!(changes.total(), 10, "Total should be 3+5+2=10");
    assert_eq!(changes.format(), "+3 ~5 -2", "Format should show +added ~modified -deleted");

    // Test with only one type of change
    let only_added = GitChanges {
        added: 1,
        modified: 0,
        deleted: 0,
    };
    assert_eq!(only_added.total(), 1);
    assert_eq!(only_added.format(), "+1 ~0 -0");

    // Verify real repo produces valid GitChanges
    let repo = TestRepo::new()?;
    fs::write(repo.path().join("test.txt"), "test")?;

    let manager = RepositoryManager::open(repo.path())?;
    let real_changes = manager.get_status()?;

    assert!(
        real_changes.total() > 0,
        "Real changes should have positive total"
    );
    assert!(
        real_changes.format().contains('+'),
        "Format should contain + indicator"
    );

    Ok(())
}

/// Status should change after staging files with git add
#[test]
fn test_repository_status_after_staging() -> Result<()> {
    // Arrange: Create repo with new file
    let repo = TestRepo::new()?;
    fs::write(repo.path().join("staged_file.txt"), "Will be staged")?;

    // Verify initial state shows as added (untracked)
    let manager = RepositoryManager::open(repo.path())?;
    let before_stage = manager.get_status()?;
    assert!(before_stage.added >= 1, "Should detect untracked file as added");

    // Act: Stage the file
    Command::new("git")
        .args(["add", "staged_file.txt"])
        .current_dir(repo.path())
        .output()?;

    // Assert: File should still show in status (now as staged/index change)
    // Note: get_status() counts both working tree and index changes
    let after_stage = manager.get_status()?;
    assert!(
        after_stage.total() >= 1,
        "Staged file should still appear in status"
    );

    Ok(())
}

/// Repository should be clean after committing all changes
#[test]
fn test_repository_is_clean_after_commit() -> Result<()> {
    // Arrange: Create repo and add uncommitted changes
    let repo = TestRepo::new()?;
    fs::write(repo.path().join("new_file.txt"), "Content to commit")?;
    fs::write(repo.path().join("README.md"), "Modified README")?;

    // Verify dirty state
    let manager = RepositoryManager::open(repo.path())?;
    let dirty_changes = manager.get_status()?;
    assert!(dirty_changes.total() > 0, "Should have uncommitted changes");
    assert!(!manager.is_clean()?, "Should not be clean");

    // Act: Stage and commit all changes
    Command::new("git")
        .args(["add", "."])
        .current_dir(repo.path())
        .output()?;

    Command::new("git")
        .args(["commit", "-m", "Commit all changes"])
        .current_dir(repo.path())
        .output()?;

    // Assert: Should now be clean
    let after_commit = manager.get_status()?;
    assert_eq!(
        after_commit.total(),
        0,
        "Should have no changes after commit"
    );
    assert!(manager.is_clean()?, "Should be clean after committing");
    assert_eq!(after_commit.format(), "No changes");

    Ok(())
}

#[cfg(test)]
mod additional_coverage {
    use super::*;

    /// Verify RepositoryManager correctly handles has_uncommitted_changes
    #[test]
    fn test_has_uncommitted_changes() -> Result<()> {
        let repo = TestRepo::new()?;
        let manager = RepositoryManager::open(repo.path())?;

        // Clean repo should have no uncommitted changes
        assert!(!manager.has_uncommitted_changes()?);

        // Add untracked file
        fs::write(repo.path().join("dirty.txt"), "dirty")?;
        assert!(manager.has_uncommitted_changes()?);

        Ok(())
    }

    /// Verify commit count tracking
    #[test]
    fn test_commit_count() -> Result<()> {
        let repo = TestRepo::new()?;
        let manager = RepositoryManager::open(repo.path())?;

        // Initial commit exists
        let initial_count = manager.get_commit_count()?;
        assert_eq!(initial_count, 1, "Should have 1 initial commit");

        // Add more commits
        repo.add_commit("file1.txt", "content1", "Second commit")?;
        repo.add_commit("file2.txt", "content2", "Third commit")?;

        let new_count = manager.get_commit_count()?;
        assert_eq!(new_count, 3, "Should have 3 commits total");

        Ok(())
    }
}
