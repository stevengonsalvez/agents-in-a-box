// ABOUTME: Behavioral tests for git worktree integration verifying worktree creation and removal
// with real git repositories in temporary directories.

use anyhow::Result;
use std::path::Path;
use tempfile::TempDir;
use uuid::Uuid;

use ainb::git::worktree_manager::{WorktreeError, WorktreeManager};

use crate::fixtures::TestRepo;

/// Helper to create a WorktreeManager with a temporary base directory
fn create_test_manager() -> Result<(WorktreeManager, TempDir)> {
    let worktree_base = TempDir::new()?;
    let manager = WorktreeManager::with_base_dir(worktree_base.path().to_path_buf())?;
    Ok((manager, worktree_base))
}

/// Verify symlink exists and points to expected target
fn verify_symlink(symlink_path: &Path, expected_target: &Path) -> bool {
    if !symlink_path.is_symlink() {
        return false;
    }
    match std::fs::read_link(symlink_path) {
        Ok(target) => target == expected_target,
        Err(_) => false,
    }
}

#[test]
fn test_worktree_creation_creates_branch_and_symlink() -> Result<()> {
    // GIVEN: A test git repository and worktree manager
    let repo = TestRepo::new()?;
    let (manager, _worktree_base) = create_test_manager()?;
    let session_id = Uuid::new_v4();
    let branch_name = "feature/test-branch";

    // WHEN: Creating a worktree
    let worktree_info = manager.create_worktree(
        session_id,
        repo.path(),
        branch_name,
        None, // Use default base branch
    )?;

    // THEN: Worktree directory exists
    assert!(
        worktree_info.path.exists(),
        "Worktree path should exist: {}",
        worktree_info.path.display()
    );

    // THEN: Worktree is a valid git repository
    assert!(
        worktree_info.path.join(".git").exists(),
        "Worktree should have .git file"
    );

    // THEN: Session symlink exists and points to worktree
    assert!(
        worktree_info.session_path.exists(),
        "Session symlink should exist: {}",
        worktree_info.session_path.display()
    );
    assert!(
        verify_symlink(&worktree_info.session_path, &worktree_info.path),
        "Session symlink should point to worktree path"
    );

    // THEN: Branch name matches
    assert_eq!(worktree_info.branch_name, branch_name);

    // THEN: Source repository path matches
    assert_eq!(worktree_info.source_repository, repo.path());

    // THEN: Commit hash is populated
    assert!(
        worktree_info.commit_hash.is_some(),
        "Commit hash should be populated"
    );

    Ok(())
}

#[test]
fn test_worktree_removal_cleans_up_symlink_and_directory() -> Result<()> {
    // GIVEN: An existing worktree
    let repo = TestRepo::new()?;
    let (manager, _worktree_base) = create_test_manager()?;
    let session_id = Uuid::new_v4();
    let branch_name = "feature/to-be-removed";

    let worktree_info = manager.create_worktree(session_id, repo.path(), branch_name, None)?;

    let worktree_path = worktree_info.path.clone();
    let session_path = worktree_info.session_path.clone();

    // Verify worktree was created
    assert!(worktree_path.exists(), "Worktree should exist before removal");
    assert!(
        session_path.exists(),
        "Session symlink should exist before removal"
    );

    // WHEN: Removing the worktree
    manager.remove_worktree(session_id)?;

    // THEN: Worktree directory is removed
    assert!(
        !worktree_path.exists(),
        "Worktree directory should be removed: {}",
        worktree_path.display()
    );

    // THEN: Session symlink is removed
    assert!(
        !session_path.exists(),
        "Session symlink should be removed: {}",
        session_path.display()
    );

    Ok(())
}

#[test]
fn test_worktree_creation_fails_for_invalid_branch_names() -> Result<()> {
    // GIVEN: A test repository and worktree manager
    let repo = TestRepo::new()?;
    let (manager, _worktree_base) = create_test_manager()?;

    // List of invalid branch names with reasons
    let invalid_names = [
        ("", "empty string"),
        ("branch with space", "contains space"),
        ("branch~tilde", "contains tilde"),
        ("branch^caret", "contains caret"),
        ("branch:colon", "contains colon"),
        ("branch?question", "contains question mark"),
        ("branch*asterisk", "contains asterisk"),
        ("branch[bracket", "contains bracket"),
        ("branch\\backslash", "contains backslash"),
        ("-starts-with-dash", "starts with dash"),
        ("ends-with-slash/", "ends with slash"),
        ("contains//double-slash", "contains double slash"),
    ];

    for (invalid_name, reason) in invalid_names {
        let session_id = Uuid::new_v4();

        // WHEN: Attempting to create worktree with invalid branch name
        let result = manager.create_worktree(session_id, repo.path(), invalid_name, None);

        // THEN: Creation fails with InvalidBranchName error
        assert!(
            result.is_err(),
            "Should reject branch name '{}' ({})",
            invalid_name,
            reason
        );

        if let Err(WorktreeError::InvalidBranchName(msg)) = result {
            assert!(
                !msg.is_empty(),
                "Error message should not be empty for '{}'",
                invalid_name
            );
        } else {
            panic!(
                "Expected InvalidBranchName error for '{}' ({}), got: {:?}",
                invalid_name, reason, result
            );
        }
    }

    Ok(())
}

#[test]
fn test_worktree_creation_accepts_valid_branch_names() -> Result<()> {
    // GIVEN: A test repository and worktree manager
    let repo = TestRepo::new()?;
    let (manager, _worktree_base) = create_test_manager()?;

    // List of valid branch names
    let valid_names = [
        "simple-branch",
        "feature/nested/path",
        "UPPERCASE",
        "MixedCase",
        "with-numbers-123",
        "with_underscores",
        "a", // Single character
        "feature/ABC-123-add-feature",
    ];

    for valid_name in valid_names {
        let session_id = Uuid::new_v4();

        // WHEN: Creating worktree with valid branch name
        let result = manager.create_worktree(session_id, repo.path(), valid_name, None);

        // THEN: Creation succeeds
        assert!(
            result.is_ok(),
            "Should accept branch name '{}', got error: {:?}",
            valid_name,
            result.err()
        );

        let worktree_info = result.unwrap();
        assert_eq!(worktree_info.branch_name, valid_name);

        // Cleanup for next iteration
        manager.remove_worktree(session_id)?;
    }

    Ok(())
}

#[test]
fn test_worktree_with_custom_base_branch() -> Result<()> {
    // GIVEN: A repository with a non-default branch
    let repo = TestRepo::new()?;
    repo.create_branch("develop")?;
    repo.add_commit("develop.txt", "develop content", "Add develop file")?;

    // Switch back to main/master for the test
    repo.checkout(&repo.current_branch()?)?;
    // Determine the default branch (main or master)
    let main_branch = repo.current_branch()?;
    if main_branch != "develop" {
        repo.checkout(&main_branch)?;
    }

    let (manager, _worktree_base) = create_test_manager()?;
    let session_id = Uuid::new_v4();
    let branch_name = "feature/from-develop";

    // WHEN: Creating worktree with custom base branch
    let worktree_info = manager.create_worktree(
        session_id,
        repo.path(),
        branch_name,
        Some("develop"), // Custom base branch
    )?;

    // THEN: Worktree is created successfully
    assert!(worktree_info.path.exists());
    assert_eq!(worktree_info.branch_name, branch_name);

    // THEN: Worktree should contain the file from develop branch
    assert!(
        worktree_info.path.join("develop.txt").exists(),
        "Worktree should contain files from develop branch"
    );

    Ok(())
}

#[test]
fn test_worktree_duplicate_creation_fails() -> Result<()> {
    // GIVEN: An existing worktree
    let repo = TestRepo::new()?;
    let (manager, _worktree_base) = create_test_manager()?;
    let session_id = Uuid::new_v4();
    let branch_name = "feature/duplicate-test";

    // First creation succeeds
    let first_result = manager.create_worktree(session_id, repo.path(), branch_name, None);
    assert!(
        first_result.is_ok(),
        "First worktree creation should succeed"
    );

    // WHEN: Attempting to create worktree with same session ID
    let second_result = manager.create_worktree(session_id, repo.path(), branch_name, None);

    // THEN: Second creation fails with AlreadyExists error
    assert!(
        second_result.is_err(),
        "Duplicate worktree creation should fail"
    );

    if let Err(WorktreeError::AlreadyExists(path)) = second_result {
        assert!(
            !path.is_empty(),
            "AlreadyExists error should include path"
        );
    } else {
        panic!(
            "Expected AlreadyExists error, got: {:?}",
            second_result
        );
    }

    Ok(())
}

#[test]
fn test_worktree_list_returns_all_worktrees() -> Result<()> {
    // GIVEN: Multiple worktrees
    let repo = TestRepo::new()?;
    let (manager, _worktree_base) = create_test_manager()?;

    let session_ids: Vec<Uuid> = (0..3).map(|_| Uuid::new_v4()).collect();
    let branch_names = ["feature/first", "feature/second", "feature/third"];

    // Create multiple worktrees
    for (session_id, branch_name) in session_ids.iter().zip(branch_names.iter()) {
        manager.create_worktree(*session_id, repo.path(), branch_name, None)?;
    }

    // WHEN: Listing all worktrees
    let worktrees = manager.list_all_worktrees()?;

    // THEN: All created worktrees are returned
    assert_eq!(
        worktrees.len(),
        session_ids.len(),
        "Should return all created worktrees"
    );

    // THEN: Each created session is in the list
    for session_id in &session_ids {
        let found = worktrees.iter().any(|(id, _)| id == session_id);
        assert!(
            found,
            "Session {} should be in the worktree list",
            session_id
        );
    }

    // THEN: Each worktree has correct branch name
    for (_id, info) in &worktrees {
        assert!(
            branch_names.contains(&info.branch_name.as_str()),
            "Branch name '{}' should be one of the created branches",
            info.branch_name
        );
    }

    Ok(())
}

#[test]
fn test_worktree_removal_for_nonexistent_session_fails() -> Result<()> {
    // GIVEN: A worktree manager with no worktrees
    let (_manager, worktree_base) = create_test_manager()?;
    let manager = WorktreeManager::with_base_dir(worktree_base.path().to_path_buf())?;
    let nonexistent_session_id = Uuid::new_v4();

    // WHEN: Attempting to remove a nonexistent worktree
    let result = manager.remove_worktree(nonexistent_session_id);

    // THEN: Removal fails with NotFound error
    assert!(result.is_err(), "Removing nonexistent worktree should fail");

    if let Err(WorktreeError::NotFound(msg)) = result {
        assert!(
            !msg.is_empty(),
            "NotFound error should include descriptive message"
        );
    } else {
        panic!("Expected NotFound error, got: {:?}", result);
    }

    Ok(())
}

#[test]
fn test_worktree_path_generation_is_deterministic() -> Result<()> {
    // GIVEN: A worktree manager and fixed inputs
    let (manager, _worktree_base) = create_test_manager()?;
    let session_id = Uuid::parse_str("12345678-1234-1234-1234-123456789abc")?;
    let branch_name = "feature/test";

    // We need a real repo for full creation
    let test_repo = TestRepo::new()?;

    // WHEN: Creating a worktree (to test path generation)
    let worktree_info = manager.create_worktree(
        session_id,
        test_repo.path(),
        branch_name,
        None,
    );

    // For path format testing, verify the generated path follows expected pattern
    // The path should be in: base_dir/by-name/{repo_name}--{branch_name}--{short_uuid}
    if let Ok(info) = worktree_info {
        let path_str = info.path.to_string_lossy();

        // THEN: Path contains by-name directory
        assert!(
            path_str.contains("by-name"),
            "Path should contain 'by-name': {}",
            path_str
        );

        // THEN: Path contains sanitized branch name (/ becomes -)
        assert!(
            path_str.contains("feature-test"),
            "Path should contain sanitized branch name: {}",
            path_str
        );

        // THEN: Path contains short UUID (first 8 chars)
        let short_uuid = &session_id.to_string()[..8];
        assert!(
            path_str.contains(short_uuid),
            "Path should contain short UUID '{}': {}",
            short_uuid,
            path_str
        );

        // THEN: Session path contains by-session directory
        let session_path_str = info.session_path.to_string_lossy();
        assert!(
            session_path_str.contains("by-session"),
            "Session path should contain 'by-session': {}",
            session_path_str
        );

        // THEN: Session path contains full UUID
        assert!(
            session_path_str.contains(&session_id.to_string()),
            "Session path should contain full UUID: {}",
            session_path_str
        );
    }

    Ok(())
}

#[test]
fn test_worktree_info_contains_correct_metadata() -> Result<()> {
    // GIVEN: A test repository and worktree
    let repo = TestRepo::new()?;
    let (manager, _worktree_base) = create_test_manager()?;
    let session_id = Uuid::new_v4();
    let branch_name = "feature/metadata-test";

    // WHEN: Creating a worktree
    let worktree_info = manager.create_worktree(session_id, repo.path(), branch_name, None)?;

    // THEN: ID matches the provided session ID
    assert_eq!(worktree_info.id, session_id, "ID should match session ID");

    // THEN: Path exists and is a directory
    assert!(
        worktree_info.path.is_dir(),
        "Path should be an existing directory"
    );

    // THEN: Session path exists and is a symlink
    assert!(
        worktree_info.session_path.is_symlink(),
        "Session path should be a symlink"
    );

    // THEN: Branch name matches
    assert_eq!(
        worktree_info.branch_name, branch_name,
        "Branch name should match"
    );

    // THEN: Source repository points to original repo
    assert_eq!(
        worktree_info.source_repository,
        repo.path(),
        "Source repository should match original repo path"
    );

    // THEN: Commit hash is a valid git hash (40 hex chars)
    if let Some(ref hash) = worktree_info.commit_hash {
        assert_eq!(hash.len(), 40, "Commit hash should be 40 characters");
        assert!(
            hash.chars().all(|c| c.is_ascii_hexdigit()),
            "Commit hash should be hexadecimal"
        );
    } else {
        panic!("Commit hash should be present");
    }

    // WHEN: Retrieving worktree info via get_worktree_info
    let retrieved_info = manager.get_worktree_info(session_id)?;

    // THEN: Retrieved info matches created info
    assert_eq!(retrieved_info.id, worktree_info.id);
    assert_eq!(retrieved_info.path, worktree_info.path);
    assert_eq!(retrieved_info.session_path, worktree_info.session_path);
    assert_eq!(retrieved_info.branch_name, worktree_info.branch_name);

    // Compare canonicalized paths to handle macOS /var -> /private/var symlink
    let retrieved_source = retrieved_info.source_repository.canonicalize()
        .unwrap_or_else(|_| retrieved_info.source_repository.clone());
    let original_source = worktree_info.source_repository.canonicalize()
        .unwrap_or_else(|_| worktree_info.source_repository.clone());
    assert_eq!(
        retrieved_source,
        original_source,
        "Source repository should match (canonicalized)"
    );

    assert_eq!(retrieved_info.commit_hash, worktree_info.commit_hash);

    Ok(())
}
