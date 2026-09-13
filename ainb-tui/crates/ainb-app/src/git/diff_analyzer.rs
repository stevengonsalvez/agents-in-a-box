// ABOUTME: Git diff analysis for detailed change statistics and file-level insights

#![allow(dead_code)]

use anyhow::Result;
use git2::{Diff, DiffOptions, Repository};
use std::path::Path;
use tracing::debug;

use crate::models::GitChanges;

#[derive(Debug, Clone)]
pub struct CustomDiffStats {
    pub files_changed: usize,
    pub insertions: usize,
    pub deletions: usize,
    pub files: Vec<FileDiff>,
}

#[derive(Debug, Clone)]
pub struct FileDiff {
    pub path: String,
    pub status: FileStatus,
    pub insertions: usize,
    pub deletions: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileStatus {
    Added,
    Modified,
    Deleted,
    Renamed,
    Copied,
    Untracked,
}

pub struct DiffAnalyzer {
    repo: Repository,
}

impl DiffAnalyzer {
    pub fn new(repo_path: &Path) -> Result<Self> {
        let repo = Repository::open(repo_path)?;
        Ok(Self { repo })
    }

    pub fn analyze_working_directory(&self) -> Result<CustomDiffStats> {
        let mut opts = DiffOptions::new();
        opts.include_untracked(true);
        opts.include_ignored(false);

        let diff = self.repo.diff_index_to_workdir(None, Some(&mut opts))?;
        self.analyze_diff(&diff)
    }

    pub fn analyze_staged_changes(&self) -> Result<CustomDiffStats> {
        let head = self.repo.head()?;
        let head_tree = head.peel_to_tree()?;

        let diff = self.repo.diff_tree_to_index(Some(&head_tree), None, None)?;
        self.analyze_diff(&diff)
    }

    pub fn analyze_branch_diff(
        &self,
        base_branch: &str,
        target_branch: &str,
    ) -> Result<CustomDiffStats> {
        let base_commit = self.repo.revparse_single(base_branch)?.peel_to_commit()?;
        let target_commit = self.repo.revparse_single(target_branch)?.peel_to_commit()?;

        let base_tree = base_commit.tree()?;
        let target_tree = target_commit.tree()?;

        let diff = self.repo.diff_tree_to_tree(Some(&base_tree), Some(&target_tree), None)?;
        self.analyze_diff(&diff)
    }

    pub fn get_simple_changes(&self) -> Result<GitChanges> {
        let working_diff = self.analyze_working_directory()?;
        let staged_diff = self.analyze_staged_changes()?;

        let mut changes = GitChanges::default();

        // Combine working directory and staged changes
        for file in &working_diff.files {
            match file.status {
                FileStatus::Added | FileStatus::Untracked => changes.added += 1,
                FileStatus::Modified => changes.modified += 1,
                FileStatus::Deleted => changes.deleted += 1,
                FileStatus::Renamed | FileStatus::Copied => changes.modified += 1,
            }
        }

        for file in &staged_diff.files {
            match file.status {
                FileStatus::Added => {
                    // Don't double-count if already counted in working directory
                    if !working_diff.files.iter().any(|f| f.path == file.path) {
                        changes.added += 1;
                    }
                }
                FileStatus::Modified => {
                    if !working_diff.files.iter().any(|f| f.path == file.path) {
                        changes.modified += 1;
                    }
                }
                FileStatus::Deleted => {
                    if !working_diff.files.iter().any(|f| f.path == file.path) {
                        changes.deleted += 1;
                    }
                }
                FileStatus::Renamed | FileStatus::Copied => {
                    if !working_diff.files.iter().any(|f| f.path == file.path) {
                        changes.modified += 1;
                    }
                }
                FileStatus::Untracked => {} // Only in working directory
            }
        }

        debug!(
            "Simple changes: +{} ~{} -{}",
            changes.added, changes.modified, changes.deleted
        );
        Ok(changes)
    }

    fn analyze_diff(&self, diff: &Diff) -> Result<CustomDiffStats> {
        let git_stats = diff.stats()?;
        let mut files = Vec::new();

        diff.foreach(
            &mut |delta, _progress| {
                let file_path = delta
                    .new_file()
                    .path()
                    .or_else(|| delta.old_file().path())
                    .and_then(|p| p.to_str())
                    .unwrap_or("unknown")
                    .to_string();

                let status = match delta.status() {
                    git2::Delta::Added => FileStatus::Added,
                    git2::Delta::Deleted => FileStatus::Deleted,
                    git2::Delta::Modified => FileStatus::Modified,
                    git2::Delta::Renamed => FileStatus::Renamed,
                    git2::Delta::Copied => FileStatus::Copied,
                    git2::Delta::Untracked => FileStatus::Untracked,
                    _ => FileStatus::Modified,
                };

                files.push(FileDiff {
                    path: file_path,
                    status,
                    insertions: 0, // Will be filled in during hunk processing
                    deletions: 0,
                });

                true // Continue iteration
            },
            None,
            None,
            None,
        )?;

        // Get detailed line statistics
        diff.foreach(
            &mut |_delta, _progress| true,
            None,
            Some(&mut |_delta, _hunk| true),
            Some(&mut |delta, _hunk, line| {
                if let Some(file_index) = files.iter_mut().find(|f| {
                    delta
                        .new_file()
                        .path()
                        .or_else(|| delta.old_file().path())
                        .and_then(|p| p.to_str())
                        .map(|p| p == f.path)
                        .unwrap_or(false)
                }) {
                    match line.origin() {
                        '+' => file_index.insertions += 1,
                        '-' => file_index.deletions += 1,
                        _ => {}
                    }
                }
                true
            }),
        )?;

        Ok(CustomDiffStats {
            files_changed: git_stats.files_changed(),
            insertions: git_stats.insertions(),
            deletions: git_stats.deletions(),
            files,
        })
    }

    pub fn get_file_changes_summary(&self) -> Result<Vec<String>> {
        let diff_stats = self.analyze_working_directory()?;
        let mut summary = Vec::new();

        for file in &diff_stats.files {
            let status_symbol = match file.status {
                FileStatus::Added | FileStatus::Untracked => "A",
                FileStatus::Modified => "M",
                FileStatus::Deleted => "D",
                FileStatus::Renamed => "R",
                FileStatus::Copied => "C",
            };

            if file.insertions > 0 || file.deletions > 0 {
                summary.push(format!(
                    "{} {} (+{} -{})",
                    status_symbol, file.path, file.insertions, file.deletions
                ));
            } else {
                summary.push(format!("{} {}", status_symbol, file.path));
            }
        }

        Ok(summary)
    }

    pub fn get_repository_path(&self) -> &Path {
        self.repo.path()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn create_test_repo_with_changes(path: &Path) -> Result<Repository> {
        let repo = Repository::init(path)?;

        // Create initial file and commit
        let initial_file = path.join("initial.txt");
        fs::write(&initial_file, "initial content\nline 2\nline 3")?;

        let mut index = repo.index()?;
        index.add_path(Path::new("initial.txt"))?;
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

        // Drop tree before making changes
        drop(tree);

        // Create some changes
        fs::write(&initial_file, "modified content\nline 2\nline 3\nnew line")?;

        let new_file = path.join("new.txt");
        fs::write(&new_file, "new file content")?;

        Ok(repo)
    }

    #[test]
    fn test_diff_analyzer_creation() {
        let temp_dir = TempDir::new().unwrap();
        create_test_repo_with_changes(temp_dir.path()).unwrap();

        let analyzer = DiffAnalyzer::new(temp_dir.path());
        assert!(analyzer.is_ok());
    }

    #[test]
    fn test_analyze_working_directory() {
        let temp_dir = TempDir::new().unwrap();
        create_test_repo_with_changes(temp_dir.path()).unwrap();

        let analyzer = DiffAnalyzer::new(temp_dir.path()).unwrap();
        let diff_stats = analyzer.analyze_working_directory().unwrap();

        assert!(diff_stats.files_changed > 0);
        assert!(!diff_stats.files.is_empty());

        // Should have both modified and new files
        let has_modified = diff_stats.files.iter().any(|f| f.status == FileStatus::Modified);
        let has_new = diff_stats
            .files
            .iter()
            .any(|f| f.status == FileStatus::Added || f.status == FileStatus::Untracked);

        assert!(has_modified || has_new);
    }

    #[test]
    fn test_get_simple_changes() {
        let temp_dir = TempDir::new().unwrap();
        create_test_repo_with_changes(temp_dir.path()).unwrap();

        let analyzer = DiffAnalyzer::new(temp_dir.path()).unwrap();
        let changes = analyzer.get_simple_changes().unwrap();

        assert!(changes.total() > 0);
    }

    #[test]
    fn test_get_file_changes_summary() {
        let temp_dir = TempDir::new().unwrap();
        create_test_repo_with_changes(temp_dir.path()).unwrap();

        let analyzer = DiffAnalyzer::new(temp_dir.path()).unwrap();
        let summary = analyzer.get_file_changes_summary().unwrap();

        assert!(!summary.is_empty());

        // Check that summary contains file paths and status indicators
        for line in &summary {
            assert!(line.contains("initial.txt") || line.contains("new.txt"));
            assert!(line.starts_with('A') || line.starts_with('M') || line.starts_with('D'));
        }
    }

    #[test]
    fn test_analyze_clean_repository() {
        let temp_dir = TempDir::new().unwrap();
        let repo = Repository::init(temp_dir.path()).unwrap();

        // Create initial commit
        let test_file = temp_dir.path().join("test.txt");
        fs::write(&test_file, "test content").unwrap();

        let mut index = repo.index().unwrap();
        index.add_path(Path::new("test.txt")).unwrap();
        index.write().unwrap();

        let signature = git2::Signature::now("Test User", "test@example.com").unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();

        repo.commit(
            Some("HEAD"),
            &signature,
            &signature,
            "Initial commit",
            &tree,
            &[],
        )
        .unwrap();

        let analyzer = DiffAnalyzer::new(temp_dir.path()).unwrap();
        let changes = analyzer.get_simple_changes().unwrap();

        assert_eq!(changes.total(), 0);
    }
}
