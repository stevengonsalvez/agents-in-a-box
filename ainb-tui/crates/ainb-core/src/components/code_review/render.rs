// ABOUTME: Unified Warp-style Code Review render surface — a left file sidebar
// plus per-file collapsible diff blocks in one continuous scroll, with a
// line-number gutter, solid green/red change bars, muted row tints, and
// expand-context rows.

use std::cell::Cell;
use std::collections::{BTreeMap, HashSet};

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};

use super::highlight::{self, BAR_ADD, BAR_DEL, GUTTER_FG};
use super::model::{DiffRow, ReviewFile, ReviewModel, RowKind};

// Chrome palette (mirrors the ainb TUI style guide used across git_view.rs).
const CORNFLOWER_BLUE: Color = Color::Rgb(100, 149, 237);
const GOLD: Color = Color::Rgb(255, 215, 0);
const SELECTION_GREEN: Color = Color::Rgb(100, 200, 100);
const DARK_BG: Color = Color::Rgb(25, 25, 35);
const PANEL_BG: Color = Color::Rgb(30, 30, 40);
const LIST_HIGHLIGHT_BG: Color = Color::Rgb(40, 40, 60);
const SOFT_WHITE: Color = Color::Rgb(220, 220, 230);
const MUTED_GRAY: Color = Color::Rgb(120, 120, 140);
const ADD_FG: Color = Color::Rgb(80, 250, 123);
const DEL_FG: Color = Color::Rgb(255, 85, 85);

const SIDEBAR_WIDTH: u16 = 26;

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
    sidebar_window: Cell<usize>,
    /// Screen rect of the sidebar list region from the last render, used to map
    /// a mouse click back to a tree-row index.
    sidebar_rect: Cell<Rect>,
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

// ───────────────────────── sidebar file tree ─────────────────────────

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

// ───────────────────────────── rendering ─────────────────────────────

/// Render the Code Review surface into `area`. Records sidebar/body geometry on
/// `ui` (via interior mutability) for mouse hit-testing.
pub fn render(frame: &mut Frame, area: Rect, model: &ReviewModel, ui: &CodeReviewUi) {
    let total_hunks = hunk_anchors(&flatten(model)).len();
    let cur_hunk = if total_hunks == 0 {
        0
    } else {
        (ui.current_hunk + 1).min(total_hunks)
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(CORNFLOWER_BLUE))
        .style(Style::default().bg(DARK_BG))
        .title(title_line(model, cur_hunk, total_hunks))
        .title_bottom(help_line());
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if model.files.is_empty() {
        ui.sidebar_rect.set(Rect::default());
        let empty = Paragraph::new(Line::from(Span::styled(
            "  No changes in this worktree.",
            Style::default().fg(MUTED_GRAY),
        )));
        frame.render_widget(empty, inner);
        return;
    }

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(SIDEBAR_WIDTH), Constraint::Min(0)])
        .split(inner);

    // Sidebar inner = column minus its RIGHT border; line 0 is the header.
    let sidebar_inner = Rect::new(
        cols[0].x,
        cols[0].y,
        cols[0].width.saturating_sub(1),
        cols[0].height,
    );
    let list_area = Rect::new(
        sidebar_inner.x,
        sidebar_inner.y.saturating_add(1),
        sidebar_inner.width,
        sidebar_inner.height.saturating_sub(1),
    );

    let tree = build_sidebar(model, &ui.collapsed_dirs);
    let sel = ui.sidebar_selected.min(tree.len().saturating_sub(1));
    // Keep the selected row inside the visible window.
    let vis = list_area.height as usize;
    let mut window = ui.sidebar_window.get().min(tree.len().saturating_sub(1));
    if sel < window {
        window = sel;
    } else if vis > 0 && sel >= window + vis {
        window = sel + 1 - vis;
    }
    ui.sidebar_window.set(window);
    ui.sidebar_rect.set(list_area);

    render_sidebar(
        frame,
        cols[0],
        sidebar_inner,
        list_area,
        model,
        sel,
        window,
        &tree,
    );
    render_body(frame, cols[1], model, ui);
}

fn title_line(model: &ReviewModel, cur_hunk: usize, total_hunks: usize) -> Line<'static> {
    let mut spans = vec![
        Span::styled(" 📋 ", Style::default().fg(GOLD)),
        Span::styled(
            "Code Review ",
            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("{} files ", model.files.len()),
            Style::default().fg(MUTED_GRAY),
        ),
        Span::styled(
            format!("+{}", model.total_insertions()),
            Style::default().fg(ADD_FG),
        ),
        Span::styled(" ", Style::default()),
        Span::styled(
            format!("-{} ", model.total_deletions()),
            Style::default().fg(DEL_FG),
        ),
    ];
    if total_hunks > 0 {
        spans.push(Span::styled(
            format!(" Hunk {cur_hunk}/{total_hunks} "),
            Style::default().fg(MUTED_GRAY),
        ));
    }
    Line::from(spans)
}

fn help_line() -> Line<'static> {
    let key = Style::default().fg(GOLD).add_modifier(Modifier::BOLD);
    let dim = Style::default().fg(MUTED_GRAY);
    Line::from(vec![
        Span::styled(" ↑/↓", key),
        Span::styled(" files ", dim),
        Span::styled("j/k", key),
        Span::styled(" scroll ", dim),
        Span::styled("n/N", key),
        Span::styled(" hunk ", dim),
        Span::styled("Space", key),
        Span::styled(" toggle ", dim),
        Span::styled("z", key),
        Span::styled(" expand ", dim),
        Span::styled("e/E", key),
        Span::styled(" folders ", dim),
        Span::styled("Tab", key),
        Span::styled(" tabs ", dim),
        Span::styled("Esc", key),
        Span::styled(" back ", dim),
    ])
}

#[allow(clippy::too_many_arguments)]
fn render_sidebar(
    frame: &mut Frame,
    block_area: Rect,
    inner: Rect,
    list_area: Rect,
    model: &ReviewModel,
    sel: usize,
    window: usize,
    tree: &[SidebarRow],
) {
    let block = Block::default()
        .borders(Borders::RIGHT)
        .border_style(Style::default().fg(Color::Rgb(60, 60, 80)))
        .style(Style::default().bg(PANEL_BG));
    frame.render_widget(block, block_area);

    // Header: 📁 Changed Files (N)
    let header = Line::from(vec![
        Span::styled(
            " 📁 Changed Files ",
            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("({})", model.files.len()),
            Style::default().fg(CORNFLOWER_BLUE),
        ),
    ]);
    frame.render_widget(
        Paragraph::new(header),
        Rect::new(inner.x, inner.y, inner.width, 1),
    );

    let start = window.min(tree.len());
    let end = (start + list_area.height as usize).min(tree.len());
    let name_w = (list_area.width as usize).saturating_sub(6);
    let lines: Vec<Line> = tree[start..end]
        .iter()
        .enumerate()
        .map(|(i, row)| sidebar_line(model, row, start + i == sel, name_w))
        .collect();
    frame.render_widget(Paragraph::new(lines), list_area);
}

fn indent(depth: usize) -> String {
    if depth == 0 {
        String::new()
    } else {
        format!("{}└ ", "  ".repeat(depth - 1))
    }
}

fn sidebar_line(
    model: &ReviewModel,
    row: &SidebarRow,
    selected: bool,
    name_w: usize,
) -> Line<'static> {
    let marker = if selected { "▶" } else { " " };
    let row_style = if selected {
        Style::default().bg(LIST_HIGHLIGHT_BG)
    } else {
        Style::default()
    };
    match row {
        SidebarRow::Dir {
            name,
            depth,
            count,
            collapsed,
            ..
        } => {
            let chevron = if *collapsed { "▶" } else { "▼" };
            Line::from(vec![
                Span::styled(format!("{marker} "), Style::default().fg(SELECTION_GREEN)),
                Span::raw(indent(*depth)),
                Span::styled(
                    format!("{chevron} 📁 "),
                    Style::default().fg(CORNFLOWER_BLUE),
                ),
                Span::styled(
                    truncate_end(name, name_w.saturating_sub(2 * depth)),
                    Style::default().fg(CORNFLOWER_BLUE).add_modifier(Modifier::BOLD),
                ),
                Span::styled(format!(" ({count})"), Style::default().fg(MUTED_GRAY)),
            ])
            .style(row_style)
        }
        SidebarRow::File { file, name, depth } => {
            let f = &model.files[*file];
            Line::from(vec![
                Span::styled(format!("{marker} "), Style::default().fg(SELECTION_GREEN)),
                Span::raw(indent(*depth)),
                Span::styled(
                    format!("[{}] ", f.status.symbol()),
                    Style::default().fg(f.status.color()),
                ),
                Span::styled(
                    truncate_end(name, name_w.saturating_sub(2 * depth)),
                    Style::default().fg(SOFT_WHITE),
                ),
            ])
            .style(row_style)
        }
    }
}

fn render_body(frame: &mut Frame, area: Rect, model: &ReviewModel, ui: &CodeReviewUi) {
    let rows = flatten(model);
    let height = area.height as usize;
    let start = ui.scroll.min(rows.len().saturating_sub(1));
    let end = (start + height).min(rows.len());

    let mut lines = Vec::with_capacity(end - start);
    for vrow in &rows[start..end] {
        lines.push(build_line(model, *vrow, area.width as usize));
    }
    frame.render_widget(Paragraph::new(lines), area);
}

fn build_line(model: &ReviewModel, vrow: VRow, width: usize) -> Line<'static> {
    match vrow {
        VRow::FileHeader { file } => {
            let f = &model.files[file];
            let chevron = if f.collapsed { "› " } else { "∨ " };
            Line::from(vec![
                Span::styled(chevron, Style::default().fg(GOLD)),
                Span::styled(
                    f.path.clone(),
                    Style::default().fg(SOFT_WHITE).add_modifier(Modifier::BOLD),
                ),
                Span::styled(format!("   +{}", f.insertions), Style::default().fg(ADD_FG)),
                Span::styled(" • ", Style::default().fg(MUTED_GRAY)),
                Span::styled(format!("-{}", f.deletions), Style::default().fg(DEL_FG)),
                Span::styled(
                    format!("   [{}]", status_label(&f.status)),
                    Style::default().fg(MUTED_GRAY),
                ),
            ])
            .style(Style::default().bg(PANEL_BG))
        }
        VRow::ExpandBefore { hidden, .. } | VRow::ExpandAfter { hidden, .. } => {
            expand_line(hidden, width)
        }
        VRow::Code { file, hunk, row } => {
            let drow = &model.files[file].hunks[hunk].rows[row];
            let lang = model.files[file].language;
            let lineno = drow.new_lineno.or(drow.old_lineno);
            let gutter = lineno.map_or_else(|| "     ".to_string(), |n| format!("{n:>4} "));
            let (bar, bar_color) = match drow.kind {
                RowKind::Added => ("▌", BAR_ADD),
                RowKind::Removed => ("▌", BAR_DEL),
                RowKind::Context => (" ", MUTED_GRAY),
            };
            let mut spans = vec![
                Span::styled(gutter, Style::default().fg(GUTTER_FG)),
                Span::styled(bar, Style::default().fg(bar_color)),
                Span::styled(" ", Style::default()),
            ];
            spans.extend(highlight::highlight_row(
                &drow.raw,
                lang,
                &drow.emphasis,
                drow.kind,
            ));
            Line::from(spans)
        }
    }
}

fn expand_line(hidden: usize, width: usize) -> Line<'static> {
    let label = format!(" ↕ expand {hidden} lines ");
    let dashes = width.saturating_sub(label.chars().count() + 2) / 2;
    let dash: String = "┄".repeat(dashes);
    Line::from(vec![
        Span::styled(format!(" {dash}"), Style::default().fg(MUTED_GRAY)),
        Span::styled(
            label,
            Style::default().fg(MUTED_GRAY).add_modifier(Modifier::DIM),
        ),
        Span::styled(dash, Style::default().fg(MUTED_GRAY)),
    ])
}

const fn status_label(status: &crate::components::git_view::GitFileStatus) -> &'static str {
    use crate::components::git_view::GitFileStatus as S;
    match status {
        S::Added => "Added",
        S::Modified => "Modified",
        S::Deleted => "Deleted",
        S::Renamed => "Renamed",
        S::Untracked => "Untracked",
    }
}

/// Truncate `s` to at most `max` columns, prefixing an ellipsis when cut (keeps
/// the tail of the path, which is the most informative part).
fn truncate_end(s: &str, max: usize) -> String {
    let count = s.chars().count();
    if count <= max || max == 0 {
        return s.to_string();
    }
    let tail: String = s.chars().skip(count - max + 1).collect();
    format!("…{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::code_review::model::Hunk;
    use crate::components::git_view::GitFileStatus;

    fn file(path: &str, collapsed: bool, hunks: Vec<Hunk>) -> ReviewFile {
        ReviewFile {
            path: path.to_string(),
            status: GitFileStatus::Modified,
            insertions: 1,
            deletions: 0,
            language: None,
            collapsed,
            binary: false,
            hunks,
            new_lines: Vec::new(),
        }
    }

    fn hunk(gap_before: usize, gap_after: usize, rows: usize) -> Hunk {
        Hunk {
            old_start: 1,
            new_start: 1,
            gap_before,
            gap_after,
            expanded_before: 0,
            expanded_after: 0,
            rows: (0..rows)
                .map(|i| DiffRow {
                    kind: RowKind::Context,
                    old_lineno: Some(i + 1),
                    new_lineno: Some(i + 1),
                    raw: format!("line {i}"),
                    emphasis: vec![],
                })
                .collect(),
        }
    }

    #[test]
    fn collapsed_file_contributes_only_header() {
        let model = ReviewModel {
            files: vec![
                file("a.rs", false, vec![hunk(0, 0, 2)]),
                file("b.rs", true, vec![hunk(0, 0, 5)]),
            ],
        };
        let rows = flatten(&model);
        // Two headers, code rows only for the expanded file (2).
        let headers = rows.iter().filter(|r| matches!(r, VRow::FileHeader { .. })).count();
        assert_eq!(headers, 2);
        let code = rows.iter().filter(|r| matches!(r, VRow::Code { .. })).count();
        assert_eq!(code, 2, "collapsed file must not contribute code rows");
    }

    #[test]
    fn gaps_produce_expand_rows() {
        let model = ReviewModel {
            files: vec![file("a.rs", false, vec![hunk(40, 18, 3)])],
        };
        let rows = flatten(&model);
        assert!(rows.iter().any(|r| matches!(r, VRow::ExpandBefore { .. })));
        assert!(rows.iter().any(|r| matches!(r, VRow::ExpandAfter { .. })));
    }

    #[test]
    fn fully_expanded_gap_yields_no_expand_row() {
        let mut h = hunk(5, 0, 2);
        h.expanded_before = 5;
        let model = ReviewModel {
            files: vec![file("a.rs", false, vec![h])],
        };
        let rows = flatten(&model);
        assert!(!rows.iter().any(|r| matches!(r, VRow::ExpandBefore { .. })));
    }

    #[test]
    fn surface_renders_title_gutter_bar_and_tints() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let drows = vec![
            DiffRow {
                kind: RowKind::Removed,
                old_lineno: Some(10),
                new_lineno: None,
                raw: "let x = 1;".into(),
                emphasis: vec![(8, 9)],
            },
            DiffRow {
                kind: RowKind::Added,
                old_lineno: None,
                new_lineno: Some(10),
                raw: "let x = 2;".into(),
                emphasis: vec![(8, 9)],
            },
            DiffRow {
                kind: RowKind::Context,
                old_lineno: Some(11),
                new_lineno: Some(11),
                raw: "ok".into(),
                emphasis: vec![],
            },
        ];
        let h = Hunk {
            old_start: 10,
            new_start: 10,
            gap_before: 5,
            gap_after: 0,
            expanded_before: 0,
            expanded_after: 0,
            rows: drows,
        };
        let mut f = file("src/demo.rs", false, vec![h]);
        f.language = Some("rust");
        f.deletions = 1;
        let model = ReviewModel { files: vec![f] };
        let ui = CodeReviewUi::default();

        let mut term = Terminal::new(TestBackend::new(100, 24)).unwrap();
        term.draw(|fr| render(fr, fr.area(), &model, &ui)).unwrap();
        let buf = term.backend().buffer();

        let mut text = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                text.push_str(buf.get(x, y).symbol());
            }
            text.push('\n');
        }
        assert!(text.contains("Code Review"), "missing title");
        assert!(text.contains("src/demo.rs"), "missing file path");
        assert!(text.contains('▌'), "missing change bar");
        assert!(text.contains("expand"), "missing expand-context row");
        assert!(text.contains("10"), "missing gutter line number");

        let mut has_add_bg = false;
        let mut has_del_bg = false;
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                let bg = buf.get(x, y).bg;
                if bg == highlight::ADD_TINT || bg == highlight::ADD_TINT_BRIGHT {
                    has_add_bg = true;
                }
                if bg == highlight::DEL_TINT || bg == highlight::DEL_TINT_BRIGHT {
                    has_del_bg = true;
                }
            }
        }
        assert!(has_add_bg, "no added-row green tint rendered");
        assert!(has_del_bg, "no removed-row red tint rendered");
    }

    #[test]
    fn sidebar_activate_collapses_file_and_hides_code() {
        let mut model = ReviewModel {
            files: vec![
                file("a.rs", false, vec![hunk(0, 0, 2)]),
                file("b.rs", false, vec![hunk(0, 0, 2)]),
            ],
        };
        let mut ui = CodeReviewUi::default();
        // Point the sidebar at b.rs's row, then activate it (the production toggle
        // path).
        let tree = build_sidebar(&model, &ui.collapsed_dirs);
        ui.sidebar_selected = tree
            .iter()
            .position(|r| matches!(r, SidebarRow::File { file: 1, .. }))
            .expect("b.rs file row");
        sidebar_activate(&mut model, &mut ui);
        assert!(model.files[1].collapsed);
        assert!(!model.files[0].collapsed);
        assert_eq!(ui.selected_file, 1);
        let rows = flatten(&model);
        assert!(!rows.iter().any(|r| matches!(r, VRow::Code { file: 1, .. })));
    }

    #[test]
    fn jump_hunk_advances_scrolls_and_clamps() {
        let model = ReviewModel {
            files: vec![
                file("a.rs", false, vec![hunk(0, 0, 2)]),
                file("b.rs", false, vec![hunk(0, 0, 2)]),
            ],
        };
        let mut ui = CodeReviewUi::default();
        let anchors = hunk_anchors(&flatten(&model));
        assert_eq!(anchors.len(), 2);

        jump_hunk(&model, &mut ui, true);
        assert_eq!(ui.current_hunk, 1);
        assert_eq!(ui.scroll, anchors[1]);
        assert_eq!(ui.selected_file, 1);
        // Clamps at the last hunk.
        jump_hunk(&model, &mut ui, true);
        assert_eq!(ui.current_hunk, 1);
        // Goes back.
        jump_hunk(&model, &mut ui, false);
        assert_eq!(ui.current_hunk, 0);
        assert_eq!(ui.scroll, anchors[0]);
    }

    #[test]
    fn select_file_moves_and_scrolls_to_header() {
        let model = ReviewModel {
            files: vec![
                file("a.rs", false, vec![hunk(0, 0, 2)]),
                file("b.rs", false, vec![hunk(0, 0, 2)]),
            ],
        };
        let mut ui = CodeReviewUi::default();
        let rows = flatten(&model);
        let headers: Vec<usize> = rows
            .iter()
            .enumerate()
            .filter_map(|(i, r)| matches!(r, VRow::FileHeader { .. }).then_some(i))
            .collect();
        select_file(&model, &mut ui, true);
        assert_eq!(ui.selected_file, 1);
        assert_eq!(ui.scroll, headers[1]);
        select_file(&model, &mut ui, false);
        assert_eq!(ui.selected_file, 0);
        assert_eq!(ui.scroll, headers[0]);
    }

    #[test]
    fn expand_context_reveals_lines_and_drops_hidden() {
        let mut f = file("a.rs", false, vec![]);
        f.new_lines = (1..=100).map(|i| format!("line {i}")).collect();
        f.hunks = vec![Hunk {
            old_start: 50,
            new_start: 50,
            gap_before: 49,
            gap_after: 0,
            expanded_before: 0,
            expanded_after: 0,
            rows: vec![
                DiffRow {
                    kind: RowKind::Context,
                    old_lineno: Some(50),
                    new_lineno: Some(50),
                    raw: "line 50".into(),
                    emphasis: vec![],
                },
                DiffRow {
                    kind: RowKind::Added,
                    old_lineno: None,
                    new_lineno: Some(51),
                    raw: "line 51 new".into(),
                    emphasis: vec![],
                },
            ],
        }];
        let mut model = ReviewModel { files: vec![f] };
        let ui = CodeReviewUi::default();

        let before = model.files[0].hunks[0].rows.len();
        expand_context(&mut model, &ui);
        let after = model.files[0].hunks[0].rows.len();
        assert!(
            after > before,
            "expand must reveal rows ({before} -> {after})"
        );
        assert_eq!(model.files[0].hunks[0].expanded_before, 10);
        // Revealed rows come from new_lines (line 40..49 precede the hunk top at 50).
        assert_eq!(model.files[0].hunks[0].rows[0].new_lineno, Some(40));
        assert_eq!(model.files[0].hunks[0].rows[0].raw, "line 40");
    }

    #[test]
    fn renders_large_diff_within_budget() {
        use std::time::Instant;

        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let mut files = Vec::new();
        for fi in 0..10 {
            let rows: Vec<DiffRow> = (0..200)
                .map(|i| DiffRow {
                    kind: if i % 5 == 0 {
                        RowKind::Added
                    } else {
                        RowKind::Context
                    },
                    old_lineno: Some(i + 1),
                    new_lineno: Some(i + 1),
                    raw: format!("let value_{i} = compute({i});"),
                    emphasis: if i % 5 == 0 { vec![(4, 9)] } else { vec![] },
                })
                .collect();
            let h = Hunk {
                old_start: 1,
                new_start: 1,
                gap_before: 0,
                gap_after: 0,
                expanded_before: 0,
                expanded_after: 0,
                rows,
            };
            let mut f = file(&format!("src/file_{fi}.rs"), false, vec![h]);
            f.language = Some("rust");
            files.push(f);
        }
        let model = ReviewModel { files };
        let ui = CodeReviewUi::default();
        let mut term = Terminal::new(TestBackend::new(120, 40)).unwrap();

        // 20 frames over a 2000-row diff. Viewport-only rendering keeps this fast
        // (~0.25s locally). This guards against an O(total-rows) regression that
        // renders the whole diff per frame instead of just the viewport, which
        // would be ~50x slower (tens of seconds). The budget is deliberately
        // generous because shared macOS CI runners are noisy and slow under load
        // (observed ~1.7s), so a tighter bound flakes without catching real
        // regressions any better.
        let start = Instant::now();
        for _ in 0..20 {
            term.draw(|fr| render(fr, fr.area(), &model, &ui)).unwrap();
        }
        let elapsed = start.elapsed();
        assert!(
            elapsed.as_millis() < 4000,
            "20 frames of a 2000-row diff took {elapsed:?} (budget 4000ms)"
        );
    }

    #[test]
    fn sidebar_tree_nests_files_under_folders() {
        let model = ReviewModel {
            files: vec![
                file("README.md", false, vec![]),
                file("src/a.rs", false, vec![]),
                file("src/b.rs", false, vec![]),
            ],
        };
        let tree = build_sidebar(&model, &HashSet::new());
        assert!(
            matches!(&tree[0], SidebarRow::Dir { name, count, .. } if name == "src" && *count == 2),
            "first row should be the src folder with 2 files"
        );
        let files = tree.iter().filter(|r| matches!(r, SidebarRow::File { .. })).count();
        assert_eq!(files, 3);
        // Collapsing src hides its files (only README.md remains visible).
        let collapsed: HashSet<String> = ["src".to_string()].into_iter().collect();
        let tree2 = build_sidebar(&model, &collapsed);
        let files2 = tree2.iter().filter(|r| matches!(r, SidebarRow::File { .. })).count();
        assert_eq!(files2, 1);
    }

    #[test]
    fn sidebar_nav_lands_on_file_and_scrolls_body() {
        let model = ReviewModel {
            files: vec![file("src/a.rs", false, vec![hunk(0, 0, 2)])],
        };
        let mut ui = CodeReviewUi::default();
        // Row 0 is the src folder; ↓ lands on the nested file and selects it.
        sidebar_nav(&model, &mut ui, true);
        assert_eq!(ui.sidebar_selected, 1);
        assert_eq!(ui.selected_file, 0);
    }

    #[test]
    fn sidebar_activate_toggles_folder() {
        let mut model = ReviewModel {
            files: vec![file("src/a.rs", false, vec![])],
        };
        let mut ui = CodeReviewUi::default(); // row 0 = src folder
        sidebar_activate(&mut model, &mut ui);
        assert!(ui.collapsed_dirs.contains("src"));
        sidebar_activate(&mut model, &mut ui);
        assert!(!ui.collapsed_dirs.contains("src"));
    }

    #[test]
    fn sidebar_row_at_maps_clicks_after_render() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        // Before any render the sidebar rect is empty → every click misses.
        let ui = CodeReviewUi::default();
        assert_eq!(sidebar_row_at(&ui, 5, 5), None);

        let model = ReviewModel {
            files: vec![file("src/a.rs", false, vec![hunk(0, 0, 2)])],
        };
        let mut term = Terminal::new(TestBackend::new(100, 24)).unwrap();
        term.draw(|f| render(f, f.area(), &model, &ui)).unwrap();

        // render() recorded the sidebar list rect; clicks inside map to tree rows.
        let r = ui.sidebar_rect.get();
        assert!(r.width > 0 && r.height > 0);
        assert_eq!(sidebar_row_at(&ui, r.x, r.y), Some(0)); // src folder
        assert_eq!(sidebar_row_at(&ui, r.x + 1, r.y + 1), Some(1)); // a.rs
        assert_eq!(sidebar_row_at(&ui, r.x + r.width + 3, r.y), None); // outside
    }

    #[test]
    fn expand_after_caps_at_new_file_tail() {
        // gap_after claims 50 hidden (old lines) but the new side has only 8 lines.
        let mut f = file("a.rs", false, vec![]);
        f.new_lines = (1..=8).map(|i| format!("L{i}")).collect();
        f.hunks = vec![Hunk {
            old_start: 1,
            new_start: 1,
            gap_before: 0,
            gap_after: 50,
            expanded_before: 0,
            expanded_after: 0,
            rows: (1..=3)
                .map(|i| DiffRow {
                    kind: RowKind::Context,
                    old_lineno: Some(i),
                    new_lineno: Some(i),
                    raw: format!("L{i}"),
                    emphasis: vec![],
                })
                .collect(),
        }];
        let mut model = ReviewModel { files: vec![f] };
        let ui = CodeReviewUi::default();
        expand_context(&mut model, &ui);
        let h = &model.files[0].hunks[0];
        // Only lines 4..=8 (the 5 available) are revealed — never past the tail.
        assert_eq!(h.rows.iter().filter_map(|r| r.new_lineno).max(), Some(8));
        assert!(
            h.rows.iter().all(|r| !r.raw.is_empty()),
            "no blank context rows"
        );
        // The affordance closes once the new content is exhausted.
        assert_eq!(h.expanded_after, h.gap_after);
    }
}
