//! File-walk helper shared across source adapters.
//!
//! Each adapter's `resolve_unit` populates [`ResolvedUnit::file_list`]
//! with paths *relative* to the fetched source root so the tool
//! adapter can map them straight onto its install root.

use std::path::{Path, PathBuf};

/// Walk every file under `source_root.join(rel_path)`. If `rel_path`
/// resolves to a file, returns just that one entry; if it resolves
/// to a directory, returns every file under it (recursively) sorted
/// deterministically.
pub fn collect_files(source_root: &Path, rel_path: &str) -> Vec<PathBuf> {
    let abs = source_root.join(rel_path);
    let mut out: Vec<PathBuf> = Vec::new();
    if abs.is_file() {
        out.push(PathBuf::from(rel_path));
    } else if abs.is_dir() {
        for entry in walkdir::WalkDir::new(&abs).sort_by_file_name() {
            let Ok(entry) = entry else { continue };
            if entry.file_type().is_file() {
                if let Ok(rel) = entry.path().strip_prefix(source_root) {
                    out.push(rel.to_path_buf());
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_single_file_for_file_path() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.md"), "x").unwrap();
        let files = collect_files(dir.path(), "a.md");
        assert_eq!(files, vec![PathBuf::from("a.md")]);
    }

    #[test]
    fn walks_directory_recursively() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("skills/foo/sub")).unwrap();
        std::fs::write(dir.path().join("skills/foo/SKILL.md"), "x").unwrap();
        std::fs::write(dir.path().join("skills/foo/sub/asset.md"), "y").unwrap();
        let files = collect_files(dir.path(), "skills/foo");
        assert_eq!(
            files,
            vec![
                PathBuf::from("skills/foo/SKILL.md"),
                PathBuf::from("skills/foo/sub/asset.md"),
            ]
        );
    }

    #[test]
    fn missing_path_yields_empty_list() {
        let dir = tempfile::tempdir().unwrap();
        assert!(collect_files(dir.path(), "nope").is_empty());
    }
}
