// ABOUTME: Fuzzy file finder component for @ symbol trigger in boss mode prompts

#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};
use tracing::debug;

#[derive(Debug, Clone)]
pub struct FileMatch {
    pub path: PathBuf,
    pub relative_path: String,
    pub score: usize,
}

#[derive(Debug, Clone)]
pub struct FuzzyFileFinderState {
    pub is_active: bool,
    pub query: String,
    pub matches: Vec<FileMatch>,
    pub selected_index: usize,
    pub at_symbol_position: usize, // Position of @ in the prompt
    pub workspace_root: Option<PathBuf>,
}

impl FuzzyFileFinderState {
    pub fn new() -> Self {
        Self {
            is_active: false,
            query: String::new(),
            matches: Vec::new(),
            selected_index: 0,
            at_symbol_position: 0,
            workspace_root: None,
        }
    }

    pub fn activate(&mut self, at_position: usize, workspace_root: Option<PathBuf>) {
        self.is_active = true;
        self.at_symbol_position = at_position;
        self.workspace_root = workspace_root;
        self.query.clear();
        self.matches.clear();
        self.selected_index = 0;

        // Initial scan with empty query shows all files
        self.update_matches();
    }

    pub fn deactivate(&mut self) {
        self.is_active = false;
        self.query.clear();
        self.matches.clear();
        self.selected_index = 0;
    }

    pub fn add_char_to_query(&mut self, ch: char) {
        if self.is_active {
            self.query.push(ch);
            self.update_matches();
            self.selected_index = 0; // Reset selection when query changes
        }
    }

    pub fn backspace_query(&mut self) {
        if self.is_active && !self.query.is_empty() {
            self.query.pop();
            self.update_matches();
            self.selected_index = 0; // Reset selection when query changes
        }
    }

    pub fn move_selection_up(&mut self) {
        if self.is_active && !self.matches.is_empty() {
            self.selected_index = if self.selected_index > 0 {
                self.selected_index - 1
            } else {
                self.matches.len() - 1
            };
        }
    }

    pub fn move_selection_down(&mut self) {
        if self.is_active && !self.matches.is_empty() {
            self.selected_index = if self.selected_index < self.matches.len() - 1 {
                self.selected_index + 1
            } else {
                0
            };
        }
    }

    pub fn get_selected_file(&self) -> Option<&FileMatch> {
        if self.is_active {
            self.matches.get(self.selected_index)
        } else {
            None
        }
    }

    fn update_matches(&mut self) {
        if let Some(ref root) = self.workspace_root {
            self.matches = find_files_fuzzy(root, &self.query, 50); // Limit to 50 matches
        }
    }
}

impl Default for FuzzyFileFinderState {
    fn default() -> Self {
        Self::new()
    }
}

pub fn find_files_fuzzy(root: &Path, query: &str, limit: usize) -> Vec<FileMatch> {
    let mut matches = Vec::new();

    if !root.exists() || !root.is_dir() {
        return matches;
    }

    debug!("Searching for files in {:?} with query: '{}'", root, query);

    // Collect all files first
    let mut all_files = Vec::new();
    collect_files_recursive(root, root, &mut all_files, 0, 5); // Max depth 5

    // Filter and score files
    for file_info in all_files {
        let score = if query.is_empty() {
            1 // Show all files when no query
        } else {
            calculate_fuzzy_score(&file_info.relative_path, query)
        };

        if score > 0 {
            matches.push(FileMatch {
                path: file_info.path,
                relative_path: file_info.relative_path,
                score,
            });
        }

        // Note: Don't break early - we need to score all files to find the best matches
    }

    // Sort by score (higher is better) and then by path length (shorter is better)
    matches.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| a.relative_path.len().cmp(&b.relative_path.len()))
            .then_with(|| a.relative_path.cmp(&b.relative_path))
    });

    matches.truncate(limit);
    matches
}

#[derive(Debug)]
struct FileInfo {
    path: PathBuf,
    relative_path: String,
}

fn collect_files_recursive(
    current_dir: &Path,
    root: &Path,
    files: &mut Vec<FileInfo>,
    depth: usize,
    max_depth: usize,
) {
    if depth > max_depth {
        return;
    }

    let Ok(entries) = fs::read_dir(current_dir) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();

        if path.is_file() {
            if let Ok(relative_path) = path.strip_prefix(root) {
                let relative_str = relative_path.to_string_lossy().to_string();

                // Skip certain file types
                if should_include_file(&relative_str) {
                    files.push(FileInfo {
                        path: path.clone(),
                        relative_path: relative_str,
                    });
                }
            }
        } else if path.is_dir() {
            // Skip certain directories
            if let Some(dir_name) = path.file_name().and_then(|n| n.to_str()) {
                if should_include_directory(dir_name) {
                    collect_files_recursive(&path, root, files, depth + 1, max_depth);
                }
            }
        }
    }
}

fn should_include_file(relative_path: &str) -> bool {
    // Skip hidden files and certain extensions
    let path = Path::new(relative_path);

    // Skip if any path component starts with .
    for component in path.components() {
        if let Some(name) = component.as_os_str().to_str() {
            if name.starts_with('.') && name != "." && name != ".." {
                return false;
            }
        }
    }

    // Skip binary and temporary files
    if let Some(extension) = path.extension().and_then(|e| e.to_str()) {
        match extension.to_lowercase().as_str() {
            "exe" | "dll" | "so" | "dylib" | "bin" | "obj" | "o" | "a" | "lib" => return false,
            "tmp" | "temp" | "bak" | "swp" | "swo" | "log" => return false,
            _ => {}
        }
    }

    true
}

fn should_include_directory(dir_name: &str) -> bool {
    // Skip common directories that shouldn't be included
    match dir_name {
        ".git" | ".svn" | ".hg" => false,
        "node_modules" | "target" | "dist" | "build" | "out" => false,
        ".vscode" | ".idea" | ".vs" => false,
        "__pycache__" | ".pytest_cache" => false,
        "coverage" | ".coverage" | ".nyc_output" => false,
        _ => !dir_name.starts_with('.'), // Skip most hidden directories
    }
}

fn calculate_fuzzy_score(path: &str, query: &str) -> usize {
    if query.is_empty() {
        return 1;
    }

    let path_lower = path.to_lowercase();
    let query_lower = query.to_lowercase();

    // Exact match gets highest score
    if path_lower.contains(&query_lower) {
        return 1000 + (100 - path.len().min(100));
    }

    // Fuzzy matching
    let mut score = 0;
    let path_chars: Vec<char> = path_lower.chars().collect();
    let query_chars: Vec<char> = query_lower.chars().collect();

    let mut path_idx = 0;
    let mut consecutive_matches = 0;

    for query_char in query_chars {
        let mut found = false;

        // Look for the character in the remaining path
        while path_idx < path_chars.len() {
            if path_chars[path_idx] == query_char {
                found = true;
                score += 10 + consecutive_matches * 5; // Bonus for consecutive matches
                consecutive_matches += 1;
                path_idx += 1;
                break;
            }
            consecutive_matches = 0;
            path_idx += 1;
        }

        if !found {
            return 0; // All characters must be found
        }
    }

    // Bonus for shorter paths
    score += (100 - path.len().min(100)) / 10;

    score
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_fuzzy_score_exact_match() {
        let score = calculate_fuzzy_score("src/main.rs", "main");
        assert!(score > 0);
        assert!(score >= 1000); // Exact match bonus
    }

    #[test]
    fn test_fuzzy_score_partial_match() {
        // Use a query that matches characters but not as a substring
        let score = calculate_fuzzy_score("src/components/mod.rs", "scm");
        assert!(score > 0);
        assert!(score < 1000); // Less than exact match
    }

    #[test]
    fn test_fuzzy_score_no_match() {
        let score = calculate_fuzzy_score("src/main.rs", "xyz");
        assert_eq!(score, 0);
    }

    #[test]
    fn test_fuzzy_score_empty_query() {
        let score = calculate_fuzzy_score("src/main.rs", "");
        assert_eq!(score, 1);
    }

    #[test]
    fn test_should_include_file() {
        assert!(should_include_file("src/main.rs"));
        assert!(should_include_file("README.md"));
        assert!(!should_include_file(".hidden/file.txt"));
        assert!(!should_include_file("build/file.exe"));
        assert!(!should_include_file("temp.tmp"));
    }

    #[test]
    fn test_should_include_directory() {
        assert!(should_include_directory("src"));
        assert!(should_include_directory("components"));
        assert!(!should_include_directory(".git"));
        assert!(!should_include_directory("node_modules"));
        assert!(!should_include_directory("target"));
    }

    #[test]
    fn test_file_finder_state_activation() {
        let mut state = FuzzyFileFinderState::new();
        assert!(!state.is_active);

        state.activate(5, Some(PathBuf::from("/test")));
        assert!(state.is_active);
        assert_eq!(state.at_symbol_position, 5);
        assert_eq!(state.workspace_root, Some(PathBuf::from("/test")));
    }

    #[test]
    fn test_file_finder_navigation() {
        let mut state = FuzzyFileFinderState::new();
        state.matches = vec![
            FileMatch {
                path: PathBuf::from("a"),
                relative_path: "a".to_string(),
                score: 10,
            },
            FileMatch {
                path: PathBuf::from("b"),
                relative_path: "b".to_string(),
                score: 20,
            },
            FileMatch {
                path: PathBuf::from("c"),
                relative_path: "c".to_string(),
                score: 30,
            },
        ];
        state.is_active = true;

        assert_eq!(state.selected_index, 0);

        state.move_selection_down();
        assert_eq!(state.selected_index, 1);

        state.move_selection_down();
        assert_eq!(state.selected_index, 2);

        state.move_selection_down(); // Should wrap to 0
        assert_eq!(state.selected_index, 0);

        state.move_selection_up(); // Should wrap to 2
        assert_eq!(state.selected_index, 2);
    }

    #[test]
    fn test_find_files_with_temp_directory() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();

        // Create some test files
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/main.rs"), "fn main() {}").unwrap();
        fs::write(root.join("src/lib.rs"), "// lib").unwrap();
        fs::write(root.join("README.md"), "# Test").unwrap();

        let matches = find_files_fuzzy(root, "main", 10);
        assert!(!matches.is_empty());

        let main_match = matches.iter().find(|m| m.relative_path.contains("main.rs"));
        assert!(main_match.is_some());
    }
}
