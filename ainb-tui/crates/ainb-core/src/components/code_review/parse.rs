// ABOUTME: Build a structured ReviewModel from git changes using git2 +
// similar. git2 enumerates changed files and supplies old (HEAD) / new
// (workdir) content; similar re-diffs each file to produce hunks with
// word-level emphasis.

use std::path::Path;

use anyhow::Result;
use git2::{Repository, Status, StatusOptions};
use similar::{ChangeTag, TextDiff};

use super::model::{DiffRow, Hunk, ReviewFile, ReviewModel, RowKind};
use crate::components::git_view::GitFileStatus;
use crate::widgets::syntax_highlighter::detect_language;

/// Context lines kept around each changed region inside a hunk.
const CONTEXT_RADIUS: usize = 3;
/// Bytes scanned when sniffing for a NUL (binary marker).
const BINARY_SNIFF: usize = 8192;

/// Build a [`ReviewModel`] of the working directory's open changes.
///
/// The diff is HEAD-vs-workdir (the file as committed vs the file on disk), so
/// it shows the net of staged + unstaged edits, plus untracked and deleted
/// files. A change that is staged then reverted in the working tree therefore
/// shows nothing.
pub fn build_review_model(worktree: &Path) -> Result<ReviewModel> {
    let repo = Repository::open(worktree)?;

    let mut opts = StatusOptions::new();
    opts.include_untracked(true)
        .recurse_untracked_dirs(true)
        .include_ignored(false)
        .exclude_submodules(true);
    let statuses = repo.statuses(Some(&mut opts))?;

    let mut files = Vec::new();
    for entry in statuses.iter() {
        let Some(path) = entry.path() else { continue };
        let path = path.to_string();
        let Some(status) = map_status(entry.status()) else {
            continue;
        };
        // Skip directory entries (untracked dirs that didn't recurse to files).
        if worktree.join(&path).is_dir() {
            continue;
        }

        let (old, new, binary) =
            file_contents(&repo, worktree, &path, status == GitFileStatus::Deleted);
        let language = detect_language(Some(&path), None);
        let mut file = ReviewFile {
            path,
            status,
            insertions: 0,
            deletions: 0,
            language,
            collapsed: false,
            binary,
            hunks: Vec::new(),
            new_lines: Vec::new(),
        };
        if !binary {
            let (hunks, insertions, deletions) = diff_to_hunks(&old, &new);
            file.hunks = hunks;
            file.insertions = insertions;
            file.deletions = deletions;
            file.new_lines = new.lines().map(String::from).collect();
        }
        files.push(file);
    }

    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(ReviewModel { files })
}

/// Map a git2 status bitset to our coarse [`GitFileStatus`], or `None` to skip.
fn map_status(s: Status) -> Option<GitFileStatus> {
    if s.is_ignored() {
        return None;
    }
    if s.is_wt_deleted() || s.is_index_deleted() {
        return Some(GitFileStatus::Deleted);
    }
    if s.is_wt_renamed() || s.is_index_renamed() {
        return Some(GitFileStatus::Renamed);
    }
    if s.is_wt_new() {
        return Some(GitFileStatus::Untracked);
    }
    if s.is_index_new() {
        return Some(GitFileStatus::Added);
    }
    if s.is_wt_modified()
        || s.is_index_modified()
        || s.is_wt_typechange()
        || s.is_index_typechange()
    {
        return Some(GitFileStatus::Modified);
    }
    None
}

/// Fetch `(old, new, is_binary)` content for a path. Old comes from the HEAD
/// blob (empty for added/untracked); new from the workdir file (empty for
/// deleted).
fn file_contents(
    repo: &Repository,
    worktree: &Path,
    path: &str,
    is_deleted: bool,
) -> (String, String, bool) {
    let old_bytes = head_blob_bytes(repo, path);
    let new_bytes = if is_deleted {
        None
    } else {
        std::fs::read(worktree.join(path)).ok()
    };
    let binary = is_binary(old_bytes.as_deref()) || is_binary(new_bytes.as_deref());
    let old = old_bytes.map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default();
    let new = new_bytes.map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default();
    (old, new, binary)
}

/// Read the HEAD tree's blob bytes for `path`, if it exists there.
fn head_blob_bytes(repo: &Repository, path: &str) -> Option<Vec<u8>> {
    let tree = repo.head().ok()?.peel_to_tree().ok()?;
    let entry = tree.get_path(Path::new(path)).ok()?;
    let blob = entry.to_object(repo).ok()?.peel_to_blob().ok()?;
    Some(blob.content().to_vec())
}

/// Sniff for a NUL byte in the first [`BINARY_SNIFF`] bytes.
fn is_binary(bytes: Option<&[u8]>) -> bool {
    bytes.is_some_and(|b| b.iter().take(BINARY_SNIFF).any(|&c| c == 0))
}

/// Number of text lines (a trailing newline does not add an empty final line).
fn count_lines(s: &str) -> usize {
    if s.is_empty() { 0 } else { s.lines().count() }
}

/// Re-diff `old` vs `new` into hunks (with word-level emphasis) and return the
/// hunks plus total insertions/deletions.
fn diff_to_hunks(old: &str, new: &str) -> (Vec<Hunk>, usize, usize) {
    let diff = TextDiff::from_lines(old, new);
    let old_total = count_lines(old);

    let mut hunks = Vec::new();
    let mut total_ins = 0usize;
    let mut total_del = 0usize;
    let mut prev_old_end = 0usize;

    for ops in diff.grouped_ops(CONTEXT_RADIUS) {
        let mut rows: Vec<DiffRow> = Vec::new();
        for op in &ops {
            for change in diff.iter_inline_changes(op) {
                let kind = match change.tag() {
                    ChangeTag::Equal => RowKind::Context,
                    ChangeTag::Insert => RowKind::Added,
                    ChangeTag::Delete => RowKind::Removed,
                };
                let old_lineno = change.old_index().map(|i| i + 1);
                let new_lineno = change.new_index().map(|i| i + 1);

                let mut raw = String::new();
                let mut emphasis: Vec<(usize, usize)> = Vec::new();
                for (emph, val) in change.iter_strings_lossy() {
                    let start = raw.len();
                    raw.push_str(&val);
                    if emph {
                        emphasis.push((start, raw.len()));
                    }
                }
                // Strip the trailing line ending and clamp emphasis ranges.
                let keep = raw.trim_end_matches(['\n', '\r']).len();
                raw.truncate(keep);
                emphasis.retain_mut(|(s, e)| {
                    if *s >= keep {
                        return false;
                    }
                    *e = (*e).min(keep);
                    *e > *s
                });

                match kind {
                    RowKind::Added => total_ins += 1,
                    RowKind::Removed => total_del += 1,
                    RowKind::Context => {}
                }
                rows.push(DiffRow {
                    kind,
                    old_lineno,
                    new_lineno,
                    raw,
                    emphasis,
                });
            }
        }

        let old_first = rows.iter().find_map(|r| r.old_lineno).unwrap_or(0);
        let new_first = rows.iter().find_map(|r| r.new_lineno).unwrap_or(0);
        let old_last = rows.iter().filter_map(|r| r.old_lineno).next_back();
        let gap_before = old_first.saturating_sub(prev_old_end + 1);
        if let Some(last) = old_last {
            prev_old_end = last;
        }

        hunks.push(Hunk {
            old_start: old_first,
            new_start: new_first,
            gap_before,
            gap_after: 0,
            expanded_before: 0,
            expanded_after: 0,
            rows,
        });
    }

    if let Some(last) = hunks.last_mut() {
        last.gap_after = old_total.saturating_sub(prev_old_end);
    }

    (hunks, total_ins, total_del)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modified_line_yields_added_and_removed_rows() {
        let old = "fn a() {}\nlet x = 1;\nfn b() {}\n";
        let new = "fn a() {}\nlet x = 2;\nfn b() {}\n";
        let (hunks, ins, del) = diff_to_hunks(old, new);
        assert_eq!(hunks.len(), 1);
        assert_eq!(ins, 1);
        assert_eq!(del, 1);
        let kinds: Vec<_> = hunks[0].rows.iter().map(|r| r.kind).collect();
        assert!(kinds.contains(&RowKind::Removed));
        assert!(kinds.contains(&RowKind::Added));
        // The removed row is old line 2, the added row is new line 2.
        let removed = hunks[0].rows.iter().find(|r| r.kind == RowKind::Removed).unwrap();
        assert_eq!(removed.old_lineno, Some(2));
        assert_eq!(removed.new_lineno, None);
        let added = hunks[0].rows.iter().find(|r| r.kind == RowKind::Added).unwrap();
        assert_eq!(added.new_lineno, Some(2));
    }

    #[test]
    fn word_emphasis_covers_only_changed_substring() {
        let old = "use a::{X};\n";
        let new = "use a::{Y, X};\n";
        let (hunks, _, _) = diff_to_hunks(old, new);
        let added = hunks.iter().flat_map(|h| &h.rows).find(|r| r.kind == RowKind::Added).unwrap();
        assert!(!added.emphasis.is_empty(), "expected word emphasis");
        // The emphasized span must be a strict subset of the line, not the whole line.
        let total: usize = added.emphasis.iter().map(|(s, e)| e - s).sum();
        assert!(
            total < added.raw.len(),
            "emphasis {total} should be smaller than line {} ({:?})",
            added.raw.len(),
            added.raw
        );
        // And the emphasized text should include the inserted token `Y`.
        let emphasized: String = added.emphasis.iter().map(|&(s, e)| &added.raw[s..e]).collect();
        assert!(emphasized.contains('Y'), "emphasized was {emphasized:?}");
    }

    #[test]
    fn untracked_file_is_all_added() {
        let (hunks, ins, del) = diff_to_hunks("", "alpha\nbeta\n");
        assert_eq!(del, 0);
        assert_eq!(ins, 2);
        for row in hunks.iter().flat_map(|h| &h.rows) {
            assert_eq!(row.kind, RowKind::Added);
            assert_eq!(row.old_lineno, None);
        }
    }

    #[test]
    fn deleted_file_is_all_removed() {
        let (hunks, ins, del) = diff_to_hunks("gamma\ndelta\n", "");
        assert_eq!(ins, 0);
        assert_eq!(del, 2);
        for row in hunks.iter().flat_map(|h| &h.rows) {
            assert_eq!(row.kind, RowKind::Removed);
            assert_eq!(row.new_lineno, None);
        }
    }

    #[test]
    fn mid_file_edit_reports_context_gaps() {
        // 100-line file, change line 50 only.
        let mut old = String::new();
        let mut new = String::new();
        for i in 1..=100 {
            old.push_str(&format!("line {i}\n"));
            if i == 50 {
                new.push_str("line 50 CHANGED\n");
            } else {
                new.push_str(&format!("line {i}\n"));
            }
        }
        let (hunks, _, _) = diff_to_hunks(&old, &new);
        assert_eq!(hunks.len(), 1);
        // ~46 lines hidden before (50 - 3 context - 1) and ~47 after.
        assert!(hunks[0].gap_before > 0, "expected hidden lines before hunk");
        assert!(hunks[0].gap_after > 0, "expected hidden lines after hunk");
    }

    #[test]
    fn is_binary_detects_nul() {
        assert!(is_binary(Some(&[b'a', 0, b'b'])));
        assert!(!is_binary(Some(b"plain text")));
        assert!(!is_binary(None));
    }
}
