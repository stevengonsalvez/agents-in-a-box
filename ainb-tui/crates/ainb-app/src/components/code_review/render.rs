// ABOUTME: Renderer-agnostic half of the `code_review::render` component: its
// state types and the logic that does not draw. The renderer lives in
// `ainb-core::components::code_review::render`, which re-exports this module.

use super::model::{DiffRow, ReviewFile, ReviewModel, RowKind};
use std::cell::Cell;
use std::collections::{BTreeMap, HashSet};

/// Transient UI state for the review surface (selection + scroll).
#[derive(Debug, Clone, Default)]
pub struct CodeReviewUi {
    /// Index of the sidebar-selected file (mirrors the highlighted tree file).
    pub selected_file: usize,
    /// Selected row in the flattened sidebar tree (files and folders).
    pub sidebar_selected: usize,
    /// Directory paths the user has collapsed in the sidebar tree.
    pub collapsed_dirs: HashSet<String>,
    /// First visible virtual-row index (diff body vertical scroll offset).
    pub scroll: usize,
    /// Index of the hunk the `n`/`N` cursor is on (0-based, across all files).
    pub current_hunk: usize,
    /// First visible sidebar tree row, recomputed during render (interior
    /// mutability so `render` can take `&self`).
    pub sidebar_window: Cell<usize>,
    /// Screen rect of the sidebar list region from the last render, used to map
    /// a mouse click back to a tree-row index.
    pub sidebar_rect: Cell<crate::geometry::Area>,
}

/// A row in the flattened, scrollable view of the model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VRow {
    /// A file's header line (path + counts + collapse chevron).
    FileHeader {
        /// Index into `ReviewModel::files`.
        file: usize,
    },
    /// Expand-context affordance above a hunk.
    ExpandBefore {
        /// File index.
        file: usize,
        /// Hunk index within the file.
        hunk: usize,
        /// Hidden context lines still collapsed.
        hidden: usize,
    },
    /// A code line within a hunk.
    Code {
        /// File index.
        file: usize,
        /// Hunk index within the file.
        hunk: usize,
        /// Row index within the hunk.
        row: usize,
    },
    /// Expand-context affordance below the final hunk.
    ExpandAfter {
        /// File index.
        file: usize,
        /// Hunk index within the file.
        hunk: usize,
        /// Hidden context lines still collapsed.
        hidden: usize,
    },
}

impl VRow {
    /// The file this row belongs to.
    pub const fn file(self) -> usize {
        match self {
            Self::FileHeader { file }
            | Self::ExpandBefore { file, .. }
            | Self::Code { file, .. }
            | Self::ExpandAfter { file, .. } => file,
        }
    }
}

/// Flatten the model into a scrollable virtual-row list. Collapsed and binary
/// files contribute only their header row.
pub fn flatten(model: &ReviewModel) -> Vec<VRow> {
    let mut rows = Vec::new();
    for (fi, file) in model.files.iter().enumerate() {
        rows.push(VRow::FileHeader { file: fi });
        if file.collapsed || file.binary {
            continue;
        }
        for (hi, hunk) in file.hunks.iter().enumerate() {
            let hidden_before = hunk.gap_before.saturating_sub(hunk.expanded_before);
            if hidden_before > 0 {
                rows.push(VRow::ExpandBefore {
                    file: fi,
                    hunk: hi,
                    hidden: hidden_before,
                });
            }
            for ri in 0..hunk.rows.len() {
                rows.push(VRow::Code {
                    file: fi,
                    hunk: hi,
                    row: ri,
                });
            }
            let hidden_after = hunk.gap_after.saturating_sub(hunk.expanded_after);
            if hidden_after > 0 {
                rows.push(VRow::ExpandAfter {
                    file: fi,
                    hunk: hi,
                    hidden: hidden_after,
                });
            }
        }
    }
    rows
}

/// Virtual-row index of the first code row of each hunk, in document order,
/// paired with the file the hunk belongs to. Drives `n`/`N` hunk navigation.
pub fn hunk_anchors(rows: &[VRow]) -> Vec<usize> {
    let mut anchors = Vec::new();
    let mut seen: Option<(usize, usize)> = None;
    for (i, r) in rows.iter().enumerate() {
        if let VRow::Code { file, hunk, .. } = r {
            if seen != Some((*file, *hunk)) {
                anchors.push(i);
                seen = Some((*file, *hunk));
            }
        }
    }
    anchors
}

/// How many lines a single expand action reveals.
const EXPAND_STEP: usize = 10;

/// Move the sidebar selection to the next/previous file and scroll its header
/// to the top of the body.
pub fn select_file(model: &ReviewModel, ui: &mut CodeReviewUi, forward: bool) {
    let tree = build_sidebar(model, &ui.collapsed_dirs);
    let file_rows: Vec<usize> = tree
        .iter()
        .enumerate()
        .filter_map(|(i, r)| matches!(r, SidebarRow::File { .. }).then_some(i))
        .collect();
    if file_rows.is_empty() {
        return;
    }
    let next = match file_rows.iter().position(|&i| i == ui.sidebar_selected) {
        Some(p) if forward => (p + 1).min(file_rows.len() - 1),
        Some(p) => p.saturating_sub(1),
        None => 0,
    };
    ui.sidebar_selected = file_rows[next];
    sync_body_to_sidebar(model, ui, &tree);
}

/// Jump to the next/previous hunk, scrolling it to the top and following the
/// sidebar selection. Updates `current_hunk`.
pub fn jump_hunk(model: &ReviewModel, ui: &mut CodeReviewUi, forward: bool) {
    let rows = flatten(model);
    let anchors = hunk_anchors(&rows);
    if anchors.is_empty() {
        return;
    }
    let last = anchors.len() - 1;
    ui.current_hunk = if forward {
        (ui.current_hunk + 1).min(last)
    } else {
        ui.current_hunk.saturating_sub(1)
    };
    let idx = anchors[ui.current_hunk];
    ui.scroll = idx;
    ui.selected_file = rows[idx].file();
    let tree = build_sidebar(model, &ui.collapsed_dirs);
    select_sidebar_file(ui, &tree, ui.selected_file);
}

/// Expand the nearest collapsed context gap at or below the current scroll.
pub fn expand_context(model: &mut ReviewModel, ui: &CodeReviewUi) {
    let rows = flatten(model);
    let target = rows
        .iter()
        .enumerate()
        .skip(ui.scroll)
        .find_map(|(_, r)| expand_target(r))
        .or_else(|| rows.iter().find_map(expand_target));
    if let Some((file, hunk, before)) = target {
        if before {
            expand_before(&mut model.files[file], hunk, EXPAND_STEP);
        } else {
            expand_after(&mut model.files[file], hunk, EXPAND_STEP);
        }
    }
}

const fn expand_target(row: &VRow) -> Option<(usize, usize, bool)> {
    match *row {
        VRow::ExpandBefore { file, hunk, .. } => Some((file, hunk, true)),
        VRow::ExpandAfter { file, hunk, .. } => Some((file, hunk, false)),
        _ => None,
    }
}

fn expand_before(file: &mut ReviewFile, hunk_idx: usize, step: usize) {
    let h = &mut file.hunks[hunk_idx];
    let hidden = h.gap_before.saturating_sub(h.expanded_before);
    let Some(top) = h.rows.iter().filter_map(|r| r.new_lineno).min() else {
        return;
    };
    // `gap_before` counts old lines; never reveal past the new file's head.
    let reveal = hidden.min(step).min(top.saturating_sub(1));
    if reveal == 0 {
        return;
    }
    let delta = line_delta(h.new_start, h.old_start);
    let mut added = Vec::new();
    for n in (top - reveal)..top {
        added.push(context_row(&file.new_lines, n, delta));
    }
    added.append(&mut h.rows);
    h.rows = added;
    h.expanded_before += reveal;
    if top - reveal <= 1 {
        h.expanded_before = h.gap_before; // reached the file head
    }
}

fn expand_after(file: &mut ReviewFile, hunk_idx: usize, step: usize) {
    let h = &mut file.hunks[hunk_idx];
    let hidden = h.gap_after.saturating_sub(h.expanded_after);
    let Some(bottom) = h.rows.iter().filter_map(|r| r.new_lineno).max() else {
        return;
    };
    // `gap_after` counts old lines; never reveal past the new file's tail.
    let avail = file.new_lines.len().saturating_sub(bottom);
    let reveal = hidden.min(step).min(avail);
    if reveal == 0 {
        return;
    }
    let delta = line_delta(h.new_start, h.old_start);
    for n in (bottom + 1)..=(bottom + reveal) {
        h.rows.push(context_row(&file.new_lines, n, delta));
    }
    h.expanded_after += reveal;
    if bottom + reveal >= file.new_lines.len() {
        h.expanded_after = h.gap_after; // reached the file tail
    }
}

/// Signed `new - old` line-number offset across an unchanged region.
fn line_delta(new_start: usize, old_start: usize) -> isize {
    isize::try_from(new_start).unwrap_or(0) - isize::try_from(old_start).unwrap_or(0)
}

fn context_row(new_lines: &[String], new_lineno: usize, delta: isize) -> DiffRow {
    let raw = new_lines.get(new_lineno - 1).cloned().unwrap_or_default();
    let old = isize::try_from(new_lineno)
        .ok()
        .map(|n| n - delta)
        .and_then(|o| usize::try_from(o).ok());
    DiffRow {
        kind: RowKind::Context,
        old_lineno: old,
        new_lineno: Some(new_lineno),
        raw,
        emphasis: Vec::new(),
    }
}

/// A row in the flattened sidebar file tree.
#[derive(Debug, Clone)]
pub enum SidebarRow {
    /// A directory node.
    Dir {
        /// Full repo-relative directory path (the collapse key).
        path: String,
        /// Display name (last path component).
        name: String,
        /// Nesting depth.
        depth: usize,
        /// Number of changed files under it.
        count: usize,
        /// Whether the folder is collapsed.
        collapsed: bool,
    },
    /// A changed file (index into `ReviewModel::files`).
    File {
        /// Index into `ReviewModel::files`.
        file: usize,
        /// Display name (filename).
        name: String,
        /// Nesting depth.
        depth: usize,
    },
}

#[derive(Default)]
struct TreeNode {
    dirs: BTreeMap<String, Self>,
    files: Vec<(String, usize)>,
}

impl TreeNode {
    fn insert(&mut self, parts: &[&str], idx: usize) {
        match parts {
            [] => {}
            [name] => self.files.push(((*name).to_string(), idx)),
            [dir, rest @ ..] => self.dirs.entry((*dir).to_string()).or_default().insert(rest, idx),
        }
    }

    fn count(&self) -> usize {
        self.files.len() + self.dirs.values().map(Self::count).sum::<usize>()
    }
}

/// Build the flattened sidebar tree from the model, honoring collapsed dirs.
/// Directories sort before files at each level; folders come first.
#[allow(clippy::implicit_hasher)]
pub fn build_sidebar(model: &ReviewModel, collapsed: &HashSet<String>) -> Vec<SidebarRow> {
    let mut root = TreeNode::default();
    for (i, f) in model.files.iter().enumerate() {
        let parts: Vec<&str> = f.path.split('/').filter(|p| !p.is_empty()).collect();
        root.insert(&parts, i);
    }
    let mut out = Vec::new();
    flatten_tree(&root, "", 0, collapsed, &mut out);
    out
}

fn flatten_tree(
    node: &TreeNode,
    prefix: &str,
    depth: usize,
    collapsed: &HashSet<String>,
    out: &mut Vec<SidebarRow>,
) {
    for (name, child) in &node.dirs {
        let path = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        let is_collapsed = collapsed.contains(&path);
        out.push(SidebarRow::Dir {
            path: path.clone(),
            name: name.clone(),
            depth,
            count: child.count(),
            collapsed: is_collapsed,
        });
        if !is_collapsed {
            flatten_tree(child, &path, depth + 1, collapsed, out);
        }
    }
    for (name, idx) in &node.files {
        out.push(SidebarRow::File {
            file: *idx,
            name: name.clone(),
            depth,
        });
    }
}

/// All directory paths in the model (every prefix of every file).
fn all_dirs(model: &ReviewModel) -> HashSet<String> {
    let mut out = HashSet::new();
    for f in &model.files {
        let parts: Vec<&str> = f.path.split('/').filter(|p| !p.is_empty()).collect();
        for d in 1..parts.len() {
            out.insert(parts[..d].join("/"));
        }
    }
    out
}

/// Move the sidebar tree selection up/down; when it lands on a file, scroll the
/// body to that file and update `selected_file`.
pub fn sidebar_nav(model: &ReviewModel, ui: &mut CodeReviewUi, down: bool) {
    let tree = build_sidebar(model, &ui.collapsed_dirs);
    if tree.is_empty() {
        return;
    }
    let last = tree.len() - 1;
    ui.sidebar_selected = if down {
        (ui.sidebar_selected + 1).min(last)
    } else {
        ui.sidebar_selected.saturating_sub(1)
    };
    sync_body_to_sidebar(model, ui, &tree);
}

fn sync_body_to_sidebar(model: &ReviewModel, ui: &mut CodeReviewUi, tree: &[SidebarRow]) {
    if let Some(SidebarRow::File { file, .. }) = tree.get(ui.sidebar_selected) {
        ui.selected_file = *file;
        let body = flatten(model);
        if let Some(pos) =
            body.iter().position(|r| matches!(r, VRow::FileHeader { file: f } if f == file))
        {
            ui.scroll = pos;
        }
    }
}

/// Move the sidebar selection to the tree row of `file` (keeps the highlight in
/// sync when the body is driven by hunk/file jumps).
fn select_sidebar_file(ui: &mut CodeReviewUi, tree: &[SidebarRow], file: usize) {
    if let Some(i) = tree
        .iter()
        .position(|r| matches!(r, SidebarRow::File { file: f, .. } if *f == file))
    {
        ui.sidebar_selected = i;
    }
}

/// Activate the selected sidebar row: toggle a folder, or collapse/expand a
/// file's diff block.
pub fn sidebar_activate(model: &mut ReviewModel, ui: &mut CodeReviewUi) {
    let tree = build_sidebar(model, &ui.collapsed_dirs);
    match tree.get(ui.sidebar_selected) {
        Some(SidebarRow::Dir { path, .. }) => {
            let path = path.clone();
            if !ui.collapsed_dirs.remove(&path) {
                ui.collapsed_dirs.insert(path);
            }
            let n = build_sidebar(model, &ui.collapsed_dirs).len();
            if ui.sidebar_selected >= n {
                ui.sidebar_selected = n.saturating_sub(1);
            }
        }
        Some(SidebarRow::File { file, .. }) => {
            let file = *file;
            ui.selected_file = file;
            if let Some(f) = model.files.get_mut(file) {
                f.collapsed = !f.collapsed;
            }
        }
        None => {}
    }
}

/// Collapse or expand every directory in the tree.
pub fn sidebar_set_all_collapsed(model: &ReviewModel, ui: &mut CodeReviewUi, collapsed: bool) {
    ui.collapsed_dirs = if collapsed {
        all_dirs(model)
    } else {
        HashSet::new()
    };
    let n = build_sidebar(model, &ui.collapsed_dirs).len();
    if ui.sidebar_selected >= n {
        ui.sidebar_selected = n.saturating_sub(1);
    }
}

/// Hit-test a mouse position against the last-rendered sidebar; returns the
/// tree-row index under `(x, y)`, if any.
pub fn sidebar_row_at(ui: &CodeReviewUi, x: u16, y: u16) -> Option<usize> {
    let r = ui.sidebar_rect.get();
    if r.width > 0 && x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height {
        Some(ui.sidebar_window.get() + usize::from(y - r.y))
    } else {
        None
    }
}

/// Select (and act on) a sidebar row, e.g. from a mouse click.
pub fn sidebar_click(model: &mut ReviewModel, ui: &mut CodeReviewUi, row: usize) {
    let tree = build_sidebar(model, &ui.collapsed_dirs);
    if row >= tree.len() {
        return;
    }
    ui.sidebar_selected = row;
    if matches!(tree[row], SidebarRow::Dir { .. }) {
        sidebar_activate(model, ui);
    } else {
        sync_body_to_sidebar(model, ui, &tree);
    }
}
