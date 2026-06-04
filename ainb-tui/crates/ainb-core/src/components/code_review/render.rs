// ABOUTME: Unified Warp-style Code Review render surface — a left file sidebar plus
// per-file collapsible diff blocks in one continuous scroll, with a line-number
// gutter, solid green/red change bars, muted row tints, and expand-context rows.

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};

use super::highlight::{self, BAR_ADD, BAR_DEL, GUTTER_FG};
use super::model::{ReviewModel, RowKind};
use crate::components::git_view::GitViewState;

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

/// Render the Code Review surface into `area`.
pub fn render(frame: &mut Frame, area: Rect, git_state: &GitViewState) {
    let model = &git_state.review;
    let ui = &git_state.review_ui;

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(CORNFLOWER_BLUE))
        .style(Style::default().bg(DARK_BG))
        .title(title_line(model))
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
    render_body(frame, cols[1], git_state);
}

fn title_line(model: &ReviewModel) -> Line<'static> {
    Line::from(vec![
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
    ])
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

fn render_body(frame: &mut Frame, area: Rect, git_state: &GitViewState) {
    let model = &git_state.review;
    let ui = &git_state.review_ui;
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
    use crate::components::code_review::model::{DiffRow, Hunk, ReviewFile};
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
}
