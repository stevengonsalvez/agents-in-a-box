// ABOUTME: Structured diff model for the Warp-style Code Review surface.
// A ReviewModel is files -> hunks -> rows, with word-level emphasis ranges per
// row.

use crate::components::git_view::GitFileStatus;

/// A full review of working-directory (or commit) changes: every changed file
/// with its structured hunks, ready to flatten into a scrollable row list.
#[derive(serde::Serialize, Debug, Clone, Default)]
#[cfg_attr(feature = "typescript-bindings", derive(specta::Type))]
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
#[derive(serde::Serialize, Debug, Clone)]
#[cfg_attr(feature = "typescript-bindings", derive(specta::Type))]
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
    #[serde(skip)]
    pub new_lines: Vec<String>,
}

/// A contiguous run of changed + surrounding-context lines.
#[derive(serde::Serialize, Debug, Clone)]
#[cfg_attr(feature = "typescript-bindings", derive(specta::Type))]
pub struct Hunk {
    /// 1-based first old (pre-image) line number in this hunk; 0 if none.
    pub old_start: usize,
    /// 1-based first new (post-image) line number in this hunk; 0 if none.
    pub new_start: usize,
    /// Hidden context lines between the previous hunk (or file head) and this
    /// one.
    pub gap_before: usize,
    /// Hidden context lines after this hunk (only set on the final hunk → file
    /// tail).
    pub gap_after: usize,
    /// How many of `gap_before` are currently revealed by the user.
    pub expanded_before: usize,
    /// How many of `gap_after` are currently revealed by the user.
    pub expanded_after: usize,
    /// Rows in display order. A frame scrubs them as one text (a key block
    /// spans rows) and drops the word-emphasis ranges of any row the scrub
    /// changed, since those byte offsets point into the original text.
    #[serde(serialize_with = "scrub_rows")]
    #[cfg_attr(feature = "typescript-bindings", specta(type = Vec<DiffRow>))]
    pub rows: Vec<DiffRow>,
}

fn scrub_rows<S: serde::Serializer>(rows: &[DiffRow], serializer: S) -> Result<S::Ok, S::Error> {
    // The state's own serializer keeps the row-level guard: no cut, no budget,
    // only the scrub. The frame's projection spends a budget through the same
    // helper, so the scrub and what it costs the emphasis ranges cannot drift
    // between the two.
    let mut unbounded = usize::MAX;
    serializer.collect_seq(crate::wire::git_view::scrub_and_cut(
        rows,
        rows.len(),
        usize::MAX,
        &mut unbounded,
    ))
}

/// Whether a row is unchanged context, an addition, or a removal.
#[derive(serde::Serialize, Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "typescript-bindings", derive(specta::Type))]
pub enum RowKind {
    /// Unchanged line shown for context.
    Context,
    /// Added (post-image only) line.
    Added,
    /// Removed (pre-image only) line.
    Removed,
}

/// A single diff line with its line numbers, text, and word-emphasis ranges.
#[derive(serde::Serialize, Debug, Clone)]
#[cfg_attr(feature = "typescript-bindings", derive(specta::Type))]
pub struct DiffRow {
    /// Context / Added / Removed.
    pub kind: RowKind,
    /// 1-based old line number, or `None` for added rows.
    pub old_lineno: Option<usize>,
    /// 1-based new line number, or `None` for removed rows.
    pub new_lineno: Option<usize>,
    /// Full line text with the trailing newline stripped (no diff marker).
    #[serde(serialize_with = "crate::wire::fields::scrub_str")]
    #[cfg_attr(feature = "typescript-bindings", specta(type = String))]
    pub raw: String,
    /// Byte ranges within `raw` that changed at the word level (brighter
    /// highlight).
    pub emphasis: Vec<(usize, usize)>,
}
