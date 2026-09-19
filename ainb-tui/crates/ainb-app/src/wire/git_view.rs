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
    /// The worktree's directory name, scrubbed, or `worktree` when that name
    /// would be the operator's username (a worktree at home) or empty (`/`,
    /// a path ending in `..`). Never the absolute path: the seam denies paths
    /// on the wire for remote surfaces, and nothing that draws this view
    /// reads more than the name (the #1097 rule for the web rows, #1212
    /// here).
    pub worktree_name: String,
    pub is_dirty: bool,
    pub can_push: bool,
    /// The draft crosses as its length, as it did before the bound.
    pub commit_message_len: Option<u32>,
    pub commit_message_cursor: usize,
    /// Sorted, because a set has no order and a frame has to be the same bytes
    /// twice for the same state.
    pub expanded_folders: Vec<String>,
    /// Expanded folders the frame did not carry, so a tree that draws fewer
    /// open folders than the person opened says why.
    pub expanded_folders_cut: usize,
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
    pub review_ui: ReviewUiFrame,
}

/// The review model as a frame carries it.
#[derive(serde::Serialize, Debug, Clone)]
#[cfg_attr(feature = "typescript-bindings", derive(specta::Type))]
pub struct ReviewFrame {
    pub files: Vec<ReviewFileFrame>,
    /// Changed files the frame did not carry. A file costs bytes before any of
    /// its rows do, so twenty thousand empty ones pass the ceiling on their
    /// own.
    pub files_cut: usize,
}

/// What a surface has selected and scrolled to, brought inside the window the
/// frame kept.
#[derive(serde::Serialize, Debug, Clone)]
#[cfg_attr(feature = "typescript-bindings", derive(specta::Type))]
pub struct ReviewUiFrame {
    pub selected_file: usize,
    pub sidebar_selected: usize,
    /// Sorted, for the reason [`GitViewFrame::expanded_folders`] is.
    pub collapsed_dirs: Vec<String>,
    /// Collapsed directories the frame did not carry.
    pub collapsed_dirs_cut: usize,
    /// The first row to draw, in the FRAME's rows rather than the reducer's:
    /// the frame carries a cut of the model, so the same number would
    /// otherwise name different content on each side.
    pub scroll: usize,
    /// The row the reducer is on was not sent, so `scroll` is the nearest one
    /// that was.
    pub scroll_cut: bool,
    pub current_hunk: usize,
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
    /// Hunks this file lost. A hunk costs bytes with no rows in it at all, and
    /// a file rewritten line by line has one per line.
    pub hunks_cut: usize,
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
    project_within(state, MAX_TEXT_BYTES, MAX_LIST_BYTES)
}

/// [`project`] on budgets of your own, so a test can prove what the budget does
/// on a state small enough to build in milliseconds.
#[must_use]
#[allow(clippy::too_many_lines)]
pub fn project_within(
    state: &GitViewState,
    text_budget: usize,
    list_budget: usize,
) -> GitViewFrame {
    let mut bytes = text_budget;
    let mut rows = MAX_ROWS_TOTAL;

    // The file the person has open goes first, so it never frames zero rows
    // because a diff somewhere else spent the budget.
    // The files a frame may carry: the first MAX_LIST_ITEMS, and the open one
    // wherever it sits. A repository with more changed files than that would
    // otherwise frame an open file it never sent, and name the last file it did
    // send as the one in front of the person.
    let held_files = state.review.files.len();
    let selected = state.review_ui.selected_file;
    let mut carried: Vec<usize> = (0..MAX_LIST_ITEMS.min(held_files)).collect();
    if selected >= carried.len() && selected < held_files {
        carried.push(selected);
    }
    let mut framed: Vec<Option<ReviewFileFrame>> = vec![None; carried.len()];
    if let Some(place) = carried.iter().position(|index| *index == selected) {
        framed[place] = project_file(&state.review.files[selected], &mut rows, &mut bytes);
    }

    // Scrubbed over the lines that could be framed, not over the whole diff: a
    // key block carries forward, so the lines past the cap cannot change what
    // the lines before them frame as, and a 100 MB diff is not scrubbed to
    // throw away.
    let (diff_head, diff_over_count) = head_of(&state.diff_content, MAX_DIFF_LINES);
    let raw_diff: Vec<&str> = diff_head.iter().map(String::as_str).collect();
    let (diff_content, diff_over_budget) = spend(&raw_diff, &mut bytes);
    let diff_lines_cut = diff_over_count + diff_over_budget;

    for (place, index) in carried.iter().enumerate() {
        if framed[place].is_none() {
            framed[place] = project_file(&state.review.files[*index], &mut rows, &mut bytes);
        }
    }

    let (markdown_head, markdown_over_count) = head_of(&state.markdown_content, MAX_MARKDOWN_LINES);
    let markdown: Vec<&str> = markdown_head.iter().map(|line| line.content.as_str()).collect();
    let (markdown_text, markdown_over_budget) = spend(&markdown, &mut bytes);
    let markdown_lines_cut = markdown_over_count + markdown_over_budget;
    let markdown_content: Vec<MarkdownLine> = markdown_text
        .into_iter()
        .zip(&state.markdown_content)
        .map(|(content, line)| MarkdownLine {
            content,
            style: line.style.clone(),
        })
        .collect();

    let mut list_bytes = list_budget;
    let (changed_files, files_cut) = take_within(&state.changed_files, &mut list_bytes);
    let (file_tree_items, tree_items_cut) = take_within(&state.file_tree_items, &mut list_bytes);
    let (commits, commits_cut) = take_within(&state.commits, &mut list_bytes);
    let (folders, folders_cut) = take_within(&sorted(&state.expanded_folders), &mut list_bytes);
    let (collapsed_dirs, collapsed_dirs_cut) =
        take_within(&sorted(&state.review_ui.collapsed_dirs), &mut list_bytes);

    // The file the state points at moves when a file before it is dropped, so
    // the frame points at where it ended up rather than where it was.
    let selected_place = carried.iter().position(|index| *index == selected).unwrap_or(0);
    let selected_framed = framed[..selected_place.min(framed.len())].iter().flatten().count();
    let files: Vec<ReviewFileFrame> = framed.into_iter().flatten().collect();
    // The reducer's offsets index ITS virtual rows, over the whole model; the
    // frame carries a cut of that model, so the same number names different
    // content on each side of the wire. The frame therefore carries the
    // offsets in ITS OWN row space, and says when the row the terminal is on
    // was not sent.
    let place = Place::of(&state.review.files, &carried, &files);
    let (scroll, scroll_cut) = place.row(state.review_ui.scroll);
    let (current_hunk, _) = place.hunk(state.review_ui.current_hunk);

    let review = ReviewFrame {
        files_cut: held_files - files.len(),
        files,
    };

    GitViewFrame {
        active_tab: state.active_tab.clone(),
        selected_file_index: within(state.selected_file_index, changed_files.len()),
        files_cut,
        changed_files,
        diff_scroll_offset: within(state.diff_scroll_offset, diff_content.len()),
        diff_content,
        diff_lines_cut,
        worktree_name: worktree_name(&state.worktree_path),
        is_dirty: state.is_dirty,
        can_push: state.can_push,
        commit_message_len: state
            .commit_message_input
            .as_ref()
            .map(|text| u32::try_from(text.chars().count()).unwrap_or(u32::MAX)),
        commit_message_cursor: state.commit_message_cursor,
        expanded_folders: folders,
        expanded_folders_cut: folders_cut,
        selected_tree_index: within(state.selected_tree_index, file_tree_items.len()),
        tree_items_cut,
        file_tree_items,
        markdown_scroll_offset: within(state.markdown_scroll_offset, markdown_content.len()),
        markdown_content,
        markdown_lines_cut,
        selected_commit_index: within(state.selected_commit_index, commits.len()),
        commits_cut,
        commits,
        review_ui: ReviewUiFrame {
            selected_file: within(selected_framed, review.files.len()),
            // Crosses as the state holds it. The sidebar's rows are
            // `build_sidebar`'s (`components/code_review/render.rs:353`): a row
            // per directory whether or not it is collapsed, plus a row per file
            // a collapsed directory is not hiding. Files plus collapsed
            // directories is not that number, and clamping to it moved a valid
            // selection down in any repository with subdirectories. A surface
            // that draws the tree knows its own row count; the frame does not
            // guess at it.
            sidebar_selected: state.review_ui.sidebar_selected,
            collapsed_dirs,
            collapsed_dirs_cut,
            scroll,
            scroll_cut,
            current_hunk,
        },
        review,
    }
}

/// Where the model's virtual rows and hunks ended up in the frame's.
///
/// The reducer counts rows the way `flatten`
/// (`components/code_review/render.rs:112`) does: a row per file heading, a
/// row per hidden gap, a row per code line, and nothing past the heading for a
/// collapsed or binary file. The frame carries a cut of that, so an offset
/// means one thing on each side until it is translated here, once, where both
/// shapes are in hand.
struct Place<'a> {
    /// Per file, its first row in the model and in the frame, how many rows
    /// and hunks each side carries, and both sides themselves, because a row
    /// inside a file is placed hunk by hunk.
    files: Vec<PlacedFile<'a>>,
}

struct PlacedFile<'a> {
    model: &'a ReviewFile,
    frame: Option<&'a ReviewFileFrame>,
    model_row: usize,
    frame_row: usize,
    model_rows: usize,
    frame_rows: usize,
    model_hunk: usize,
    frame_hunk: usize,
    model_hunks: usize,
    frame_hunks: usize,
}

impl<'a> Place<'a> {
    /// Walk the model's files beside the frame's, in the order the frame kept
    /// them: `carried` says which model file each framed slot came from.
    fn of(model: &'a [ReviewFile], carried: &[usize], framed: &'a [ReviewFileFrame]) -> Self {
        let mut files = Vec::with_capacity(model.len());
        let mut model_row = 0;
        let mut frame_row = 0;
        let mut model_hunk = 0;
        let mut frame_hunk = 0;
        // A framed file keeps its place in `framed` in model order, so the two
        // walks advance together and a file the budget dropped simply has no
        // frame rows of its own.
        let mut next_framed = 0;
        for (index, file) in model.iter().enumerate() {
            let frame = carried
                .iter()
                .position(|carried| *carried == index)
                .and_then(|_| framed.get(next_framed))
                .filter(|frame| frame.path == file.path);
            let model_rows = model_file_rows(file);
            let frame_rows = frame.map_or(0, frame_file_rows);
            let model_hunks = if file.collapsed || file.binary {
                0
            } else {
                file.hunks.len()
            };
            // Zeroed for a collapsed or binary file exactly as the model side
            // is: `flatten` gives such a file its heading and nothing else, so
            // counting its hunks here would shift every later file's hunk base
            // and put the cursor on another file's line.
            let frame_hunks = frame.map_or(0, |frame| {
                if frame.collapsed || frame.binary {
                    0
                } else {
                    frame.hunks.len()
                }
            });
            files.push(PlacedFile {
                model: file,
                frame,
                model_row,
                frame_row,
                model_rows,
                frame_rows,
                model_hunk,
                frame_hunk,
                model_hunks,
                frame_hunks,
            });
            model_row += model_rows;
            model_hunk += model_hunks;
            if frame.is_some() {
                frame_row += frame_rows;
                frame_hunk += frame_hunks;
                next_framed += 1;
            }
        }
        Self { files }
    }

    /// The framed row nearest the model's `row`, and whether that exact row is
    /// missing from the frame.
    fn row(&self, row: usize) -> (usize, bool) {
        let Some(file) = self.files.iter().find(|file| row < file.model_row + file.model_rows)
        else {
            // Past the last row the model has. A review with no rows at all is
            // not a cut one: there was nothing to leave out.
            let last = self.files.last().map_or(0, |file| file.frame_row + file.frame_rows);
            return (last.saturating_sub(1), last > 0);
        };
        let local = row - file.model_row;
        let Some(frame) = file.frame else {
            // The file itself was not framed: the top of whatever follows it.
            return (file.frame_row, true);
        };
        let (inside, cut) = row_in_file(file.model, frame, local);
        (file.frame_row + inside, cut)
    }

    /// The framed hunk nearest the model's `hunk`, and whether that hunk is
    /// missing from the frame.
    fn hunk(&self, hunk: usize) -> (usize, bool) {
        let Some(file) = self.files.iter().find(|file| hunk < file.model_hunk + file.model_hunks)
        else {
            let last = self.files.last().map_or(0, |file| file.frame_hunk + file.frame_hunks);
            return (last.saturating_sub(1), last > 0);
        };
        let local = hunk - file.model_hunk;
        if file.frame_hunks == 0 {
            return (file.frame_hunk, true);
        }
        if local < file.frame_hunks {
            (file.frame_hunk + local, false)
        } else {
            (file.frame_hunk + file.frame_hunks - 1, true)
        }
    }
}

/// How many virtual rows `file` has in the model, as `flatten` counts them.
fn model_file_rows(file: &ReviewFile) -> usize {
    if file.collapsed || file.binary {
        return 1;
    }
    1 + file
        .hunks
        .iter()
        .map(|hunk| {
            hunk_rows(
                hunk.gap_before,
                hunk.expanded_before,
                hunk.rows.len(),
                hunk.gap_after,
                hunk.expanded_after,
            )
        })
        .sum::<usize>()
}

/// The same count over a framed file, which is what a surface draws.
fn frame_file_rows(file: &ReviewFileFrame) -> usize {
    if file.collapsed || file.binary {
        return 1;
    }
    1 + file
        .hunks
        .iter()
        .map(|hunk| {
            hunk_rows(
                hunk.gap_before,
                hunk.expanded_before,
                hunk.rows.len(),
                hunk.gap_after,
                hunk.expanded_after,
            )
        })
        .sum::<usize>()
}

/// Where `local`, a row inside one file's own rows, sits in the framed file,
/// and whether that exact row is missing from it.
///
/// Walked hunk by hunk rather than by totals, because a hunk truncated part
/// way still frames the gap below it: counting rows alone, the first row past
/// the cut would land on that gap row and be reported as present, a row off
/// and reading as though nothing had been left out.
fn row_in_file(model: &ReviewFile, frame: &ReviewFileFrame, local: usize) -> (usize, bool) {
    // The heading, which a framed file always has.
    if local == 0 {
        return (0, false);
    }
    let mut at_model = 1;
    let mut at_frame = 1;
    for (index, hunk) in model.hunks.iter().enumerate() {
        let framed = frame.hunks.get(index);
        let model_before = hunk.gap_before > hunk.expanded_before;
        let frame_before = framed.is_some_and(|hunk| hunk.gap_before > hunk.expanded_before);
        if model_before {
            if local == at_model {
                return if frame_before {
                    (at_frame, false)
                } else {
                    (at_frame.saturating_sub(1), true)
                };
            }
            at_model += 1;
        }
        if frame_before {
            at_frame += 1;
        }

        let model_rows = hunk.rows.len();
        let frame_rows = framed.map_or(0, |hunk| hunk.rows.len());
        if local < at_model + model_rows {
            let row = local - at_model;
            return if row < frame_rows {
                (at_frame + row, false)
            } else {
                // Past what this hunk kept: the last row of it that was sent.
                (at_frame + frame_rows.saturating_sub(1), true)
            };
        }
        at_model += model_rows;
        at_frame += frame_rows;

        let model_after = hunk.gap_after > hunk.expanded_after;
        let frame_after = framed.is_some_and(|hunk| hunk.gap_after > hunk.expanded_after);
        if model_after {
            if local == at_model {
                // The gap below a hunk the frame truncated is drawn, but it is
                // not the row the reducer is on: that row was cut.
                return if frame_after && frame_rows == model_rows {
                    (at_frame, false)
                } else {
                    (at_frame.saturating_sub(1), true)
                };
            }
            at_model += 1;
        }
        if frame_after {
            at_frame += 1;
        }
    }
    (at_frame.saturating_sub(1), true)
}

/// The virtual rows one hunk contributes, as `flatten` counts them: an expand
/// affordance for each gap still hidden, and a row per code line.
///
/// The gaps do NOT depend on the rows. `flatten`
/// (`components/code_review/render.rs:121-145`) pushes `ExpandBefore` before
/// it walks the rows and `ExpandAfter` after, each on its own `hidden > 0`,
/// so a hunk with no rows left still shows the two affordances around where
/// they were.
const fn hunk_rows(
    gap_before: usize,
    expanded_before: usize,
    rows: usize,
    gap_after: usize,
    expanded_after: usize,
) -> usize {
    (gap_before > expanded_before) as usize + rows + (gap_after > expanded_after) as usize
}

/// What a frame calls the worktree: its directory name, scrubbed, or
/// [`NEUTRAL_WORKTREE_NAME`] when that name would say something else. At the
/// home directory the name is the operator's username, and at `/` or a path
/// ending in `..` there is no name at all.
fn worktree_name(path: &std::path::Path) -> String {
    let at_home = dirs::home_dir().is_some_and(|home| home == path);
    match path.file_name() {
        Some(name) if !at_home => crate::fleet::bridge::redact::scrub(&name.to_string_lossy()),
        _ => NEUTRAL_WORKTREE_NAME.to_string(),
    }
}

/// The worktree's name on a frame when its directory name is not a project's.
pub const NEUTRAL_WORKTREE_NAME: &str = "worktree";

/// `set`, in an order a frame can repeat: a set has none of its own, so which
/// entries a cut keeps would otherwise change run to run.
fn sorted(set: &std::collections::HashSet<String>) -> Vec<String> {
    let mut entries: Vec<String> = set.iter().cloned().collect();
    entries.sort();
    entries
}

/// `index`, brought inside a list of `len` the frame cut, so a surface does not
/// scroll to a row the frame no longer carries.
fn within(index: usize, len: usize) -> usize {
    index.min(len.saturating_sub(1))
}

/// One file's projection: scrub every row the file has as one text, then keep
/// what the row caps and the byte budget allow, in display order, so the hunks
/// a person reads first keep theirs.
fn project_file(
    file: &ReviewFile,
    total: &mut usize,
    bytes: &mut usize,
) -> Option<ReviewFileFrame> {
    // The file's own fields cost bytes before a single row does, and the path
    // costs what it ENCODES to: a path is arbitrary bytes, and one with a quote
    // or a control character in it is longer on the wire than in the state.
    if !afford(FILE_BYTES + text_bytes(&file.path), bytes) {
        return None;
    }

    let held: usize = file.hunks.iter().map(|hunk| hunk.rows.len()).sum();
    let keep = MAX_ROWS_PER_FILE.min(*total);
    // Only the rows that could be kept are scrubbed: the scrub carries its key
    // block forward, never backward, so the rows past the cap cannot change
    // what the rows before them frame as.
    let candidates: Vec<DiffRow> =
        file.hunks.iter().flat_map(|hunk| &hunk.rows).take(keep).cloned().collect();
    let framed = scrub_and_cut(&candidates, keep, MAX_LINE_CHARS, bytes);

    // A hunk costs bytes with no rows in it, and a file rewritten line by line
    // has one hunk per line, so the hunks are counted and afforded too. The
    // header is afforded BEFORE its rows leave the iterator: a hunk whose
    // header will not fit keeps none of them, and rows taken for a hunk that
    // was never pushed would be counted as framed and reported as kept.
    let mut rows = framed.into_iter().peekable();
    let mut hunks = Vec::new();
    let mut kept = 0;
    for hunk in file.hunks.iter().take(MAX_ROWS_PER_FILE) {
        if rows.peek().is_none() && !hunk.rows.is_empty() {
            break;
        }
        if !afford(HUNK_BYTES, bytes) {
            break;
        }
        let rows: Vec<DiffRow> = rows.by_ref().take(hunk.rows.len()).collect();
        kept += rows.len();
        hunks.push(HunkFrame {
            old_start: hunk.old_start,
            new_start: hunk.new_start,
            gap_before: hunk.gap_before,
            gap_after: hunk.gap_after,
            expanded_before: hunk.expanded_before,
            expanded_after: hunk.expanded_after,
            rows,
        });
    }
    *total -= kept;
    let rows_cut = held - kept;

    Some(ReviewFileFrame {
        path: file.path.clone(),
        status: file.status.clone(),
        insertions: file.insertions,
        deletions: file.deletions,
        language: file.language.map(str::to_string),
        collapsed: file.collapsed,
        binary: file.binary,
        hunks_cut: file.hunks.len() - hunks.len(),
        hunks,
        rows_cut,
    })
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
    let mut framed = Vec::new();
    let mut in_key = false;
    // A chunk at a time, so a budget that runs out stops the scrub as well as
    // the frame: scrubbing text nobody will read is the projection's whole
    // cost.
    for chunk in rows[..keep.min(rows.len())].chunks(SCRUB_CHUNK) {
        let raws: Vec<&str> = chunk.iter().map(|row| row.raw.as_str()).collect();
        let scrubbed = crate::fleet::bridge::redact::scrub_lines_from(&raws, &mut in_key);
        let before = framed.len();
        for (row, scrubbed) in chunk.iter().zip(scrubbed) {
            let (raw, was_cut) = crate::fleet::conversation::cut(&scrubbed, chars);
            if !afford(row_bytes(&raw, row.emphasis.len()), budget) {
                break;
            }
            let emphasis = if was_cut || raw != row.raw {
                Vec::new()
            } else {
                row.emphasis.clone()
            };
            framed.push(DiffRow {
                raw,
                emphasis,
                ..row.clone()
            });
        }
        if framed.len() - before < chunk.len() {
            break;
        }
    }
    framed
}

/// The first `keep` of `items`, and how many that left behind.
fn head_of<T>(items: &[T], keep: usize) -> (&[T], usize) {
    let kept = keep.min(items.len());
    (&items[..kept], items.len() - kept)
}

/// `lines`, scrubbed, each cut to [`MAX_LINE_CHARS`], no more of them than
/// `budget` allows, and how many of them that left behind.
///
/// Scrubbed here rather than by the caller, and a chunk at a time, so a budget
/// that runs out stops the scrub too.
fn spend(lines: &[&str], budget: &mut usize) -> (Vec<String>, usize) {
    let mut framed = Vec::new();
    let mut in_key = false;
    for chunk in lines.chunks(SCRUB_CHUNK) {
        let scrubbed = crate::fleet::bridge::redact::scrub_lines_from(chunk, &mut in_key);
        let before = framed.len();
        for line in scrubbed {
            let (text, _) = crate::fleet::conversation::cut(&line, MAX_LINE_CHARS);
            if !afford(text_bytes(&text), budget) {
                break;
            }
            framed.push(text);
        }
        if framed.len() - before < chunk.len() {
            break;
        }
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
        let cost = serde_json::to_string(item).map_or(usize::MAX, |encoded| encoded.len());
        if !afford(cost, budget) {
            break;
        }
        framed.push(item.clone());
    }
    let cut = items.len() - framed.len();
    (framed, cut)
}

/// Whether `cost` fits what is left of `budget`, and spends it if it does.
const fn afford(cost: usize, budget: &mut usize) -> bool {
    if cost > *budget {
        return false;
    }
    *budget -= cost;
    true
}

/// What `text` costs as a JSON string.
///
/// Encoded, not raw, because escaping is where the bytes go: a control
/// character is one byte here and six on the wire. Counted rather than
/// serialized, because the frame serializes every row once already and
/// measuring by serializing would make that twice.
fn text_bytes(text: &str) -> usize {
    2 + text
        .bytes()
        .map(|byte| match byte {
            b'"' | b'\\' | 0x08 | 0x09 | 0x0a | 0x0c | 0x0d => 2,
            0x00..=0x1f => 6,
            _ => 1,
        })
        .sum::<usize>()
}

/// What one framed row costs: its text, its emphasis ranges, and the fields
/// around them.
fn row_bytes(raw: &str, emphasis: usize) -> usize {
    text_bytes(raw) + emphasis * EMPHASIS_BYTES + ROW_BYTES
}

/// The bytes a row, a file and a hunk cost with no text in them at all: their
/// keys, their numbers and the punctuation between. Rounded up from what an
/// empty one encodes to, so the budget is never spent past what it thinks.
const ROW_BYTES: usize = 96;
const EMPHASIS_BYTES: usize = 24;
const FILE_BYTES: usize = 192;
const HUNK_BYTES: usize = 160;

/// How many lines are scrubbed between two looks at the budget. Small enough
/// that a spent budget stops the scrub promptly, large enough that a key block
/// crossing a boundary is the rare case rather than every line.
const SCRUB_CHUNK: usize = 64;

/// Serialize `state` as the bounded projection; the section field's own
/// serializer.
pub fn bounded<S: serde::Serializer>(
    state: &Option<GitViewState>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    use serde::Serialize;
    state.as_ref().map(project).serialize(serializer)
}
