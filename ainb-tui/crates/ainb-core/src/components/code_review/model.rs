// ABOUTME: Structured diff model for the Warp-style Code Review surface.
// A ReviewModel is files -> hunks -> rows, with word-level emphasis ranges per row.

use crate::components::git_view::GitFileStatus;

/// A full review of working-directory (or commit) changes: every changed file
/// with its structured hunks, ready to flatten into a scrollable row list.
#[derive(Debug, Clone, Default)]
pub struct ReviewModel {
    /// Changed files, sorted by path.
    pub files: Vec<ReviewFile>,
}

impl ReviewModel {
    /// Total insertions across all files.
    pub fn total_insertions(&self) -> usize {
        self.files.iter().map(|f| f.insertions).sum()
    }

    /// Total deletions across all files.
    pub fn total_deletions(&self) -> usize {
        self.files.iter().map(|f| f.deletions).sum()
    }
}

/// One changed file and its hunks.
#[derive(Debug, Clone)]
pub struct ReviewFile {
    /// Repo-relative path.
    pub path: String,
    /// Git status (Added/Modified/Deleted/Renamed/Untracked).
    pub status: GitFileStatus,
    /// Lines added.
    pub insertions: usize,
    /// Lines removed.
    pub deletions: usize,
    /// Detected syntect language token (e.g. `"rust"`), or `None` if unknown.
    pub language: Option<&'static str>,
    /// Whether this file's diff block is collapsed in the UI.
    pub collapsed: bool,
    /// Binary file — no rows are produced, the UI shows a placeholder.
    pub binary: bool,
    /// Diff hunks in file order.
    pub hunks: Vec<Hunk>,
    /// The new-side file content split into lines, used to reveal context lines
    /// when the user expands a collapsed gap. Empty for binary/deleted files.
    pub new_lines: Vec<String>,
}

/// A contiguous run of changed + surrounding-context lines.
#[derive(Debug, Clone)]
pub struct Hunk {
    /// 1-based first old (pre-image) line number in this hunk; 0 if none.
    pub old_start: usize,
    /// Number of old lines covered (context + removed).
    pub old_len: usize,
    /// 1-based first new (post-image) line number in this hunk; 0 if none.
    pub new_start: usize,
    /// Number of new lines covered (context + added).
    pub new_len: usize,
    /// Hidden context lines between the previous hunk (or file head) and this one.
    pub gap_before: usize,
    /// Hidden context lines after this hunk (only set on the final hunk → file tail).
    pub gap_after: usize,
    /// How many of `gap_before` are currently revealed by the user.
    pub expanded_before: usize,
    /// How many of `gap_after` are currently revealed by the user.
    pub expanded_after: usize,
    /// Rows in display order.
    pub rows: Vec<DiffRow>,
}

/// Whether a row is unchanged context, an addition, or a removal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowKind {
    /// Unchanged line shown for context.
    Context,
    /// Added (post-image only) line.
    Added,
    /// Removed (pre-image only) line.
    Removed,
}

/// A single diff line with its line numbers, text, and word-emphasis ranges.
#[derive(Debug, Clone)]
pub struct DiffRow {
    /// Context / Added / Removed.
    pub kind: RowKind,
    /// 1-based old line number, or `None` for added rows.
    pub old_lineno: Option<usize>,
    /// 1-based new line number, or `None` for removed rows.
    pub new_lineno: Option<usize>,
    /// Full line text with the trailing newline stripped (no diff marker).
    pub raw: String,
    /// Byte ranges within `raw` that changed at the word level (brighter highlight).
    pub emphasis: Vec<(usize, usize)>,
}
