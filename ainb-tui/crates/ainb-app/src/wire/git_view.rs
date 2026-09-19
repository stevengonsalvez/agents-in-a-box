// ABOUTME: The bounded projection of the git view, which is what a frame
// carries. The section holds a whole diff twice, as `diff_content` and again as
// the review rows, and a section body past `MAX_FRAME_BYTES` is withheld WHOLE
// (`frame.rs`), which would take the file tree and the commit box with it. So
// the frame carries a window of each, and says how much it cut.
//
//   state (whole diff, the TUI paints it) ──▶ scrub ──▶ cut ──▶ frame
//                                                        │
//                                              diff_lines_cut, rows_cut
//
// Scrub BEFORE cut, never after: a key block spans rows (`redact::scrub_lines`
// carries an `in_key` flag across lines), so cutting first could frame the tail
// of a credential whose opening line was dropped.
//
// The projection is owned rather than a borrowed serializer, so the same types
// carry the TypeScript bindings: a field the window reads cannot drift from the
// field the frame writes.

use crate::components::code_review::model::{DiffRow, ReviewFile};
use crate::components::git_view::{ChangedFile, FileTreeItem, GitTab, GitViewState, MarkdownLine};
use crate::fleet::bridge::redact::scrub_lines;

/// The most raw diff lines a frame carries.
pub const MAX_DIFF_LINES: usize = 2_000;

/// The most review rows a frame carries for one file, across its hunks.
pub const MAX_ROWS_PER_FILE: usize = 400;

/// The most review rows a frame carries across every file. The per-file cap
/// alone does not bound the section: a thousand small files are as heavy as one
/// enormous one, and the frame has one ceiling for both.
pub const MAX_ROWS_TOTAL: usize = 4_000;

/// The most characters any one line or row carries, so a minified file cannot
/// pass the ceiling in a handful of rows.
pub const MAX_LINE_CHARS: usize = 2_000;

/// The most rendered markdown lines a frame carries.
pub const MAX_MARKDOWN_LINES: usize = 2_000;

/// The git view as a frame carries it: every field the state holds, with the
/// three long ones windowed and each window's loss counted.
#[derive(serde::Serialize, Debug, Clone)]
#[cfg_attr(feature = "typescript-bindings", derive(specta::Type))]
pub struct GitViewFrame {
    pub active_tab: GitTab,
    pub changed_files: Vec<ChangedFile>,
    pub selected_file_index: usize,
    /// Scrubbed, then cut to [`MAX_DIFF_LINES`].
    pub diff_content: Vec<String>,
    /// Diff lines the frame did not carry, so a short diff and a cut one are
    /// not read as the same thing.
    pub diff_lines_cut: usize,
    pub diff_scroll_offset: usize,
    pub worktree_path: std::path::PathBuf,
    pub is_dirty: bool,
    pub can_push: bool,
    /// The draft crosses as its length, as it did before the bound.
    pub commit_message_len: Option<u32>,
    pub commit_message_cursor: usize,
    pub expanded_folders: std::collections::HashSet<String>,
    pub file_tree_items: Vec<FileTreeItem>,
    pub selected_tree_index: usize,
    /// Already scrubbed line by line at its own field, cut to
    /// [`MAX_MARKDOWN_LINES`].
    pub markdown_content: Vec<MarkdownLine>,
    pub markdown_lines_cut: usize,
    pub markdown_scroll_offset: usize,
    pub commits: Vec<crate::git::operations::CommitInfo>,
    pub selected_commit_index: usize,
    pub review: ReviewFrame,
    pub review_ui: crate::components::code_review::render::CodeReviewUi,
}

/// The review model as a frame carries it.
#[derive(serde::Serialize, Debug, Clone)]
#[cfg_attr(feature = "typescript-bindings", derive(specta::Type))]
pub struct ReviewFrame {
    pub files: Vec<ReviewFileFrame>,
}

/// One changed file: its own fields, its hunks windowed on one row budget, and
/// the rows that budget cost it.
#[derive(serde::Serialize, Debug, Clone)]
#[cfg_attr(feature = "typescript-bindings", derive(specta::Type))]
pub struct ReviewFileFrame {
    pub path: String,
    pub status: crate::components::git_view::GitFileStatus,
    pub insertions: usize,
    pub deletions: usize,
    pub language: Option<String>,
    pub collapsed: bool,
    pub binary: bool,
    pub hunks: Vec<HunkFrame>,
    /// Rows this file lost to [`MAX_ROWS_PER_FILE`].
    pub rows_cut: usize,
}

/// One hunk, its rows already scrubbed and within the file's budget.
#[derive(serde::Serialize, Debug, Clone)]
#[cfg_attr(feature = "typescript-bindings", derive(specta::Type))]
pub struct HunkFrame {
    pub old_start: usize,
    pub new_start: usize,
    pub gap_before: usize,
    pub gap_after: usize,
    pub expanded_before: usize,
    pub expanded_after: usize,
    pub rows: Vec<DiffRow>,
}

/// Project `state` into the bounded frame.
#[must_use]
pub fn project(state: &GitViewState) -> GitViewFrame {
    let diff = scrub_lines(&state.diff_content);
    let diff_lines_cut = diff.len().saturating_sub(MAX_DIFF_LINES);
    // One budget across every file, spent in display order, so a diff of many
    // small files is bounded exactly as a diff of one huge file is.
    let mut budget = MAX_ROWS_TOTAL;
    let markdown_lines_cut = state.markdown_content.len().saturating_sub(MAX_MARKDOWN_LINES);

    GitViewFrame {
        active_tab: state.active_tab.clone(),
        changed_files: state.changed_files.clone(),
        selected_file_index: state.selected_file_index,
        diff_content: diff.into_iter().take(MAX_DIFF_LINES).map(|line| cut_line(&line)).collect(),
        diff_lines_cut,
        diff_scroll_offset: state.diff_scroll_offset,
        worktree_path: state.worktree_path.clone(),
        is_dirty: state.is_dirty,
        can_push: state.can_push,
        commit_message_len: state
            .commit_message_input
            .as_ref()
            .map(|text| u32::try_from(text.chars().count()).unwrap_or(u32::MAX)),
        commit_message_cursor: state.commit_message_cursor,
        expanded_folders: state.expanded_folders.clone(),
        file_tree_items: state.file_tree_items.clone(),
        selected_tree_index: state.selected_tree_index,
        markdown_content: state.markdown_content.iter().take(MAX_MARKDOWN_LINES).cloned().collect(),
        markdown_lines_cut,
        markdown_scroll_offset: state.markdown_scroll_offset,
        commits: state.commits.clone(),
        selected_commit_index: state.selected_commit_index,
        review: ReviewFrame {
            files: state.review.files.iter().map(|file| project_file(file, &mut budget)).collect(),
        },
        review_ui: state.review_ui.clone(),
    }
}

/// One file's projection: scrub every row it has, then spend the budget across
/// its hunks in display order, so the hunks a person reads first keep theirs.
fn project_file(file: &ReviewFile, total: &mut usize) -> ReviewFileFrame {
    let held: usize = file.hunks.iter().map(|hunk| hunk.rows.len()).sum();
    let mut budget = MAX_ROWS_PER_FILE.min(*total);
    let allowed = budget;
    let hunks = file
        .hunks
        .iter()
        .map(|hunk| {
            let raws: Vec<&str> = hunk.rows.iter().map(|row| row.raw.as_str()).collect();
            let scrubbed = scrub_lines(&raws);
            let keep = budget.min(hunk.rows.len());
            budget -= keep;
            *total -= keep;
            HunkFrame {
                old_start: hunk.old_start,
                new_start: hunk.new_start,
                gap_before: hunk.gap_before,
                gap_after: hunk.gap_after,
                expanded_before: hunk.expanded_before,
                expanded_after: hunk.expanded_after,
                rows: hunk
                    .rows
                    .iter()
                    .zip(scrubbed)
                    .take(keep)
                    .map(|(row, raw)| {
                        // A row the scrub changed loses its emphasis ranges:
                        // those byte offsets point into the original text.
                        let emphasis = if raw == row.raw {
                            row.emphasis.clone()
                        } else {
                            Vec::new()
                        };
                        DiffRow {
                            raw: cut_line(&raw),
                            emphasis,
                            ..row.clone()
                        }
                    })
                    .collect(),
            }
        })
        .collect();

    ReviewFileFrame {
        path: file.path.clone(),
        status: file.status.clone(),
        insertions: file.insertions,
        deletions: file.deletions,
        language: file.language.map(str::to_string),
        collapsed: file.collapsed,
        binary: file.binary,
        hunks,
        rows_cut: held.saturating_sub(allowed),
    }
}

/// `line`, cut to [`MAX_LINE_CHARS`] with the cut marked, so a truncated line
/// is not read as the whole line.
fn cut_line(line: &str) -> String {
    crate::fleet::conversation::cut(line, MAX_LINE_CHARS).0
}

/// Serialize `state` as the bounded projection; the section field's own
/// serializer.
pub fn bounded<S: serde::Serializer>(
    state: &Option<GitViewState>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    use serde::Serialize;
    state.as_ref().map(project).serialize(serializer)
}
