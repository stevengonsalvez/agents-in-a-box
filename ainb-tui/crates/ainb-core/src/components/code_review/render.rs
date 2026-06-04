// ABOUTME: Unified Warp-style Code Review render surface — a left file sidebar plus
// per-file collapsible diff blocks in one continuous scroll, with a line-number
// gutter, solid green/red change bars, muted row tints, and expand-context rows.

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
    /// Index of the sidebar-selected file.
    pub selected_file: usize,
    /// First visible virtual-row index (vertical scroll offset).
    pub scroll: usize,
    /// Index of the hunk the `n`/`N` cursor is on (0-based, across all files).
    pub current_hunk: usize,
    /// Height of the diff body in rows, captured from the last render for clamping.
    pub viewport_height: usize,
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

/// The virtual-row index of each file's header (used for `n`/`N` and file nav).
pub fn file_header_indices(rows: &[VRow]) -> Vec<usize> {
    rows.iter()
        .enumerate()
        .filter_map(|(i, r)| matches!(r, VRow::FileHeader { .. }).then_some(i))
        .collect()
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

/// Toggle the collapse state of the sidebar-selected file.
pub fn toggle_collapse(model: &mut ReviewModel, ui: &CodeReviewUi) {
    if let Some(file) = model.files.get_mut(ui.selected_file) {
        file.collapsed = !file.collapsed;
    }
}

/// Move the sidebar selection to the next/previous file and scroll its header
/// to the top of the body.
pub fn select_file(model: &ReviewModel, ui: &mut CodeReviewUi, forward: bool) {
    if model.files.is_empty() {
        return;
    }
    let last = model.files.len() - 1;
    ui.selected_file = if forward {
        (ui.selected_file + 1).min(last)
    } else {
        ui.selected_file.saturating_sub(1)
    };
    let rows = flatten(model);
    let headers = file_header_indices(&rows);
    if let Some(&idx) = headers.get(ui.selected_file) {
        ui.scroll = idx;
    }
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
    let reveal = hidden.min(step);
    if reveal == 0 {
        return;
    }
    let Some(top) = h.rows.iter().filter_map(|r| r.new_lineno).min() else {
        return;
    };
    let delta = line_delta(h.new_start, h.old_start);
    let mut added = Vec::new();
    for n in top.saturating_sub(reveal)..top {
        if n == 0 {
            continue;
        }
        added.push(context_row(&file.new_lines, n, delta));
    }
    added.append(&mut h.rows);
    h.rows = added;
    h.expanded_before += reveal;
}

fn expand_after(file: &mut ReviewFile, hunk_idx: usize, step: usize) {
    let h = &mut file.hunks[hunk_idx];
    let hidden = h.gap_after.saturating_sub(h.expanded_after);
    let reveal = hidden.min(step);
    if reveal == 0 {
        return;
    }
    let Some(bottom) = h.rows.iter().filter_map(|r| r.new_lineno).max() else {
        return;
    };
    let delta = line_delta(h.new_start, h.old_start);
    for n in (bottom + 1)..=(bottom + reveal) {
        h.rows.push(context_row(&file.new_lines, n, delta));
    }
    h.expanded_after += reveal;
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

/// Render the Code Review surface into `area`.
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
        let empty = Paragraph::new(Line::from(Span::styled(
            "  No changes in this worktree.",
            Style::default().fg(MUTED_GRAY),
        )));
        frame.render_widget(empty, inner);
        return;
    }

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(SIDEBAR_WIDTH),
            Constraint::Min(0),
        ])
        .split(inner);

    render_sidebar(frame, cols[0], model, ui);
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
        Span::styled(" j/k", key),
        Span::styled(" scroll ", dim),
        Span::styled("n/N", key),
        Span::styled(" hunk ", dim),
        Span::styled("[ ]", key),
        Span::styled(" file ", dim),
        Span::styled("Space", key),
        Span::styled(" collapse ", dim),
        Span::styled("z", key),
        Span::styled(" expand ", dim),
        Span::styled("Tab", key),
        Span::styled(" tabs ", dim),
        Span::styled("Esc", key),
        Span::styled(" back ", dim),
    ])
}

fn render_sidebar(frame: &mut Frame, area: Rect, model: &ReviewModel, ui: &CodeReviewUi) {
    let block = Block::default()
        .borders(Borders::RIGHT)
        .border_style(Style::default().fg(Color::Rgb(60, 60, 80)))
        .style(Style::default().bg(PANEL_BG));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let name_w = inner.width.saturating_sub(10) as usize;
    let mut lines = Vec::new();
    for (fi, file) in model.files.iter().enumerate() {
        let selected = fi == ui.selected_file;
        let marker = if selected { "▶ " } else { "  " };
        let marker_style = if selected {
            Style::default().fg(SELECTION_GREEN)
        } else {
            Style::default().fg(MUTED_GRAY)
        };
        let path = truncate_end(&file.path, name_w);
        let path_style = if selected {
            Style::default().fg(SOFT_WHITE).bg(LIST_HIGHLIGHT_BG)
        } else {
            Style::default().fg(SOFT_WHITE)
        };
        lines.push(Line::from(vec![
            Span::styled(marker, marker_style),
            Span::styled(file.status.symbol().to_string(), Style::default().fg(file.status.color())),
            Span::styled(format!(" {path}"), path_style),
        ]));
        lines.push(Line::from(vec![
            Span::styled("    +", Style::default().fg(MUTED_GRAY)),
            Span::styled(file.insertions.to_string(), Style::default().fg(ADD_FG)),
            Span::styled(" -", Style::default().fg(MUTED_GRAY)),
            Span::styled(file.deletions.to_string(), Style::default().fg(DEL_FG)),
        ]));
    }
    frame.render_widget(Paragraph::new(lines), inner);
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
        Span::styled(label, Style::default().fg(MUTED_GRAY).add_modifier(Modifier::DIM)),
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
            old_len: rows,
            new_start: 1,
            new_len: rows,
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
        let headers = rows
            .iter()
            .filter(|r| matches!(r, VRow::FileHeader { .. }))
            .count();
        assert_eq!(headers, 2);
        let code = rows
            .iter()
            .filter(|r| matches!(r, VRow::Code { .. }))
            .count();
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
            old_len: 2,
            new_start: 10,
            new_len: 2,
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
        term.draw(|fr| render(fr, fr.size(), &model, &ui)).unwrap();
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
    fn toggle_collapse_flips_selected_file_and_hides_code() {
        let mut model = ReviewModel {
            files: vec![
                file("a.rs", false, vec![hunk(0, 0, 2)]),
                file("b.rs", false, vec![hunk(0, 0, 2)]),
            ],
        };
        let ui = CodeReviewUi {
            selected_file: 1,
            ..Default::default()
        };
        toggle_collapse(&mut model, &ui);
        assert!(model.files[1].collapsed);
        assert!(!model.files[0].collapsed);
        let rows = flatten(&model);
        assert!(!rows
            .iter()
            .any(|r| matches!(r, VRow::Code { file: 1, .. })));
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
        let headers = file_header_indices(&flatten(&model));
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
            old_len: 2,
            new_start: 50,
            new_len: 3,
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
        assert!(after > before, "expand must reveal rows ({before} -> {after})");
        assert_eq!(model.files[0].hunks[0].expanded_before, 10);
        // Revealed rows come from new_lines (line 40..49 precede the hunk top at 50).
        assert_eq!(model.files[0].hunks[0].rows[0].new_lineno, Some(40));
        assert_eq!(model.files[0].hunks[0].rows[0].raw, "line 40");
    }

    #[test]
    fn renders_large_diff_within_budget() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        use std::time::Instant;

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
                old_len: 200,
                new_start: 1,
                new_len: 200,
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

        // 20 frames over a 2000-row diff. Viewport-only rendering keeps this fast.
        let start = Instant::now();
        for _ in 0..20 {
            term.draw(|fr| render(fr, fr.size(), &model, &ui)).unwrap();
        }
        let elapsed = start.elapsed();
        assert!(
            elapsed.as_millis() < 1500,
            "20 frames of a 2000-row diff took {elapsed:?} (budget 1500ms)"
        );
    }
}
