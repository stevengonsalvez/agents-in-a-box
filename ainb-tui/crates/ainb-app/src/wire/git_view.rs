// ABOUTME: The bounded projection of the git view, which is what a frame
// carries. The section holds a whole diff twice, as `diff_content` and again as
// the review rows, and a section body past `MAX_FRAME_BYTES` is withheld WHOLE
// (`frame.rs`), which would take the file tree and the commit box with it. So
// the frame carries a window of each, and says how much it cut.
//
//   state (whole diff, the TUI paints it) ──▶ scrub ──▶ cut ──▶ spend ──▶ frame
//                                                                 │
//                                        diff_lines_cut, rows_cut, files_cut
//
// Two bounds, because counts alone do not hold: 2,000 diff lines and 4,000
// review rows of 2,000 characters is 11 MiB before escaping, and a control
// character costs six bytes encoded. So the counts stop any one source crowding
// out the others, and one BYTE budget ([`MAX_TEXT_BYTES`]) is what actually
// keeps the section inside the ceiling whatever the content is. The budget
// degrades the section, counting what it dropped; it never withholds it.
//
// It is spent in the order a person reads: the review file they are looking at,
// then the diff, then the rest of the review, then markdown, then the lists. So
// the open file never frames zero rows because a diff elsewhere ate the budget.
//
// Scrub BEFORE cut, never after: a key block spans rows (`redact::scrub_lines`
// carries an `in_key` flag across lines), so cutting first could frame the tail
// of a credential whose opening line was dropped. For the same reason the scrub
// runs over a whole file's rows and over the whole markdown document, not over
// one hunk or one line at a time.
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

/// The most review rows a frame carries across every file.
///
/// The per-file cap alone does not bound the section: a thousand small files
/// are as heavy as one enormous one, and the frame has one ceiling for both.
pub const MAX_ROWS_TOTAL: usize = 4_000;

/// The most characters any one line or row carries, so a minified file cannot
/// pass the ceiling in a handful of rows.
pub const MAX_LINE_CHARS: usize = 2_000;

/// The most rendered markdown lines a frame carries.
pub const MAX_MARKDOWN_LINES: usize = 2_000;

/// The most entries a frame carries in any one of the section's lists: the
/// changed files, the file tree, the expanded folders, the commits.
pub const MAX_LIST_ITEMS: usize = 1_000;

/// The encoded bytes of TEXT one frame of this section may spend, half the
/// frame ceiling, leaving the other half for the structure around it.
///
/// Encoded, not raw: escaping is where the bytes actually go, since a control
/// character is one byte in the state and six on the wire.
pub const MAX_TEXT_BYTES: usize = crate::wire::frame::MAX_FRAME_BYTES / 2;

/// The encoded bytes the section's lists may spend, held back from the text
/// budget rather than taken out of it.
///
/// Their own reserve because they are what a person navigates by: a diff that
/// spent every byte would otherwise frame a file tree of nothing, which is the
/// withheld section again by another route.
pub const MAX_LIST_BYTES: usize = crate::wire::frame::MAX_FRAME_BYTES / 8;

/// The git view as a frame carries it: every field the state holds, with the
/// long ones windowed and each window's loss counted.
#[derive(serde::Serialize, Debug, Clone)]
#[cfg_attr(feature = "typescript-bindings", derive(specta::Type))]
pub struct GitViewFrame {
    pub active_tab: GitTab,
    pub changed_files: Vec<ChangedFile>,
    /// Changed paths the frame did not carry, so a shortened tree says so.
    pub files_cut: usize,
    pub selected_file_index: usize,
    /// Scrubbed, then cut to [`MAX_DIFF_LINES`] and to what the budget allows.
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
    /// Sorted, because a set has no order and a frame has to be the same bytes
    /// twice for the same state.
    pub expanded_folders: Vec<String>,
    pub file_tree_items: Vec<FileTreeItem>,
    /// Tree rows the frame did not carry.
    pub tree_items_cut: usize,
    pub selected_tree_index: usize,
    /// Scrubbed as one document, then cut to [`MAX_MARKDOWN_LINES`] and to
    /// [`MAX_LINE_CHARS`] a line.
    pub markdown_content: Vec<MarkdownLine>,
    pub markdown_lines_cut: usize,
    pub markdown_scroll_offset: usize,
    pub commits: Vec<crate::git::operations::CommitInfo>,
    /// Commits the frame did not carry.
    pub commits_cut: usize,
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
    /// Rows this file lost, to [`MAX_ROWS_PER_FILE`], to [`MAX_ROWS_TOTAL`], or
    /// to the byte budget.
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
#[allow(clippy::too_many_lines)]
pub fn project(state: &GitViewState) -> GitViewFrame {
    let mut bytes = MAX_TEXT_BYTES;
    let mut rows = MAX_ROWS_TOTAL;

    // The file the person has open goes first, so it never frames zero rows
    // because a diff somewhere else spent the budget.
    let files = &state.review.files;
    let mut framed: Vec<Option<ReviewFileFrame>> = vec![None; files.len()];
    let selected = state.review_ui.selected_file;
    if let Some(file) = files.get(selected) {
        framed[selected] = Some(project_file(file, &mut rows, &mut bytes));
    }

    // Scrubbed over the lines that could be framed, not over the whole diff: a
    // key block carries forward, so the lines past the count cap cannot change
    // what the lines before them frame as, and a 100 MB diff is not scrubbed to
    // throw away.
    let (diff_head, over_count) = head_of(&state.diff_content, MAX_DIFF_LINES);
    let (diff_content, diff_over_budget) = spend(&scrub_lines(diff_head), &mut bytes);
    let diff_lines_cut = over_count + diff_over_budget;

    for (index, file) in files.iter().enumerate() {
        if framed[index].is_none() {
            framed[index] = Some(project_file(file, &mut rows, &mut bytes));
        }
    }

    // Scrubbed as one document rather than a line at a time: a credential's
    // body is lines below its opening, and `scrub_lines` is what carries that
    // across them.
    let (markdown_head, markdown_over_count) = head_of(&state.markdown_content, MAX_MARKDOWN_LINES);
    let markdown: Vec<&str> = markdown_head.iter().map(|line| line.content.as_str()).collect();
    let (markdown_text, markdown_over_budget) = spend(&scrub_lines(&markdown), &mut bytes);
    let markdown_lines_cut = markdown_over_count + markdown_over_budget;
    let markdown_content: Vec<MarkdownLine> = markdown_text
        .into_iter()
        .zip(&state.markdown_content)
        .map(|(content, line)| MarkdownLine {
            content,
            style: line.style.clone(),
        })
        .collect();

    let mut list_bytes = MAX_LIST_BYTES;
    let (changed_files, files_cut) = take_within(&state.changed_files, &mut list_bytes);
    let (file_tree_items, tree_items_cut) = take_within(&state.file_tree_items, &mut list_bytes);
    let (commits, commits_cut) = take_within(&state.commits, &mut list_bytes);
    // Sorted before the cut so which folders survive is the same every frame: a
    // set has no order of its own to take from.
    let mut folders: Vec<String> = state.expanded_folders.iter().cloned().collect();
    folders.sort();
    let (folders, _) = take_within(&folders, &mut list_bytes);

    let review = ReviewFrame {
        files: framed.into_iter().flatten().collect(),
    };
    let hunk_count: usize = review.files.iter().map(|file| file.hunks.len()).sum();
    let row_count: usize = review
        .files
        .iter()
        .flat_map(|file| &file.hunks)
        .map(|hunk| hunk.rows.len())
        .sum();

    GitViewFrame {
        active_tab: state.active_tab.clone(),
        selected_file_index: within(state.selected_file_index, changed_files.len()),
        files_cut,
        changed_files,
        diff_scroll_offset: within(state.diff_scroll_offset, diff_content.len()),
        diff_content,
        diff_lines_cut,
        worktree_path: state.worktree_path.clone(),
        is_dirty: state.is_dirty,
        can_push: state.can_push,
        commit_message_len: state
            .commit_message_input
            .as_ref()
            .map(|text| u32::try_from(text.chars().count()).unwrap_or(u32::MAX)),
        commit_message_cursor: state.commit_message_cursor,
        expanded_folders: folders,
        selected_tree_index: within(state.selected_tree_index, file_tree_items.len()),
        tree_items_cut,
        file_tree_items,
        markdown_scroll_offset: within(state.markdown_scroll_offset, markdown_content.len()),
        markdown_content,
        markdown_lines_cut,
        selected_commit_index: within(state.selected_commit_index, commits.len()),
        commits_cut,
        commits,
        review_ui: crate::components::code_review::render::CodeReviewUi {
            selected_file: within(state.review_ui.selected_file, review.files.len()),
            sidebar_selected: state.review_ui.sidebar_selected,
            collapsed_dirs: state.review_ui.collapsed_dirs.clone(),
            scroll: within(state.review_ui.scroll, row_count + hunk_count),
            current_hunk: within(state.review_ui.current_hunk, hunk_count),
        },
        review,
    }
}

/// `index`, brought inside a list of `len` the frame cut, so a surface does not
/// scroll to a row the frame no longer carries.
fn within(index: usize, len: usize) -> usize {
    index.min(len.saturating_sub(1))
}

/// One file's projection: scrub every row the file has as one text, then keep
/// what the row caps and the byte budget allow, in display order, so the hunks
/// a person reads first keep theirs.
fn project_file(file: &ReviewFile, total: &mut usize, bytes: &mut usize) -> ReviewFileFrame {
    let held: Vec<DiffRow> = file.hunks.iter().flat_map(|hunk| hunk.rows.iter().cloned()).collect();
    let keep = MAX_ROWS_PER_FILE.min(*total);
    let framed = scrub_and_cut(&held, keep, MAX_LINE_CHARS, bytes);
    *total -= framed.len();
    let rows_cut = held.len() - framed.len();

    let mut framed = framed.into_iter();
    let hunks = file
        .hunks
        .iter()
        .map(|hunk| HunkFrame {
            old_start: hunk.old_start,
            new_start: hunk.new_start,
            gap_before: hunk.gap_before,
            gap_after: hunk.gap_after,
            expanded_before: hunk.expanded_before,
            expanded_after: hunk.expanded_after,
            rows: framed.by_ref().take(hunk.rows.len()).collect(),
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
        rows_cut,
    }
}

/// `rows`, scrubbed as one text, each row cut to `chars`, and no more of them
/// than `keep` or than `budget` encoded bytes allow.
///
/// The one place that rule lives: this projection and the state's own row
/// serializer (`code_review::model`) both come through here, so the scrub, the
/// cut, and what they cost the emphasis ranges cannot drift apart.
///
/// Emphasis ranges are byte offsets into the row's original text, so a row
/// loses them whenever its text changed at all, by the scrub or by the cut.
pub(crate) fn scrub_and_cut(
    rows: &[DiffRow],
    keep: usize,
    chars: usize,
    budget: &mut usize,
) -> Vec<DiffRow> {
    let raws: Vec<&str> = rows.iter().map(|row| row.raw.as_str()).collect();
    let scrubbed = scrub_lines(&raws);
    let mut framed = Vec::new();
    for (row, scrubbed) in rows.iter().zip(scrubbed).take(keep) {
        let (raw, was_cut) = crate::fleet::conversation::cut(&scrubbed, chars);
        let emphasis = if was_cut || raw != row.raw {
            Vec::new()
        } else {
            row.emphasis.clone()
        };
        let row = DiffRow {
            raw,
            emphasis,
            ..row.clone()
        };
        if !afford(&row, budget) {
            break;
        }
        framed.push(row);
    }
    framed
}

/// The first `keep` of `items`, and how many that left behind.
fn head_of<T>(items: &[T], keep: usize) -> (&[T], usize) {
    let kept = keep.min(items.len());
    (&items[..kept], items.len() - kept)
}

/// `lines`, each cut to [`MAX_LINE_CHARS`], no more of them than `budget`
/// allows, and how many of them that left behind.
///
/// The lines arrive scrubbed: the scrub runs over the text before any of it is
/// cut.
fn spend(lines: &[String], budget: &mut usize) -> (Vec<String>, usize) {
    let mut framed = Vec::new();
    for line in lines {
        let (text, _) = crate::fleet::conversation::cut(line, MAX_LINE_CHARS);
        if !afford(&text, budget) {
            break;
        }
        framed.push(text);
    }
    let cut = lines.len() - framed.len();
    (framed, cut)
}

/// The first entries of `items` that fit [`MAX_LIST_ITEMS`] and `budget`, and
/// how many were left behind.
///
/// The section's lists are unbounded in the state: one entry per changed path,
/// per tree row, per commit. A repository with a hundred thousand changed paths
/// passes the ceiling through the tree alone, and a shortened tree that says so
/// beats no section at all.
fn take_within<T>(items: &[T], budget: &mut usize) -> (Vec<T>, usize)
where
    T: serde::Serialize + Clone,
{
    let mut framed = Vec::new();
    for item in items.iter().take(MAX_LIST_ITEMS) {
        if !afford(item, budget) {
            break;
        }
        framed.push(item.clone());
    }
    let cut = items.len() - framed.len();
    (framed, cut)
}

/// Whether `value` fits what is left of `budget`, and spends it if it does.
///
/// The cost is what the value encodes to, escaping included, because escaping
/// is where the bytes go: a control character is one byte in the state and six
/// on the wire. A value that cannot be encoded costs the whole budget, so a
/// frame never grows on a field it could not measure.
fn afford<T: serde::Serialize + ?Sized>(value: &T, budget: &mut usize) -> bool {
    let cost = serde_json::to_string(value).map_or(usize::MAX, |encoded| encoded.len());
    if cost > *budget {
        return false;
    }
    *budget -= cost;
    true
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
