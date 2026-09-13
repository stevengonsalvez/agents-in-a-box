// ABOUTME: Unified Warp-style Code Review render surface — a left file sidebar
// plus per-file collapsible diff blocks in one continuous scroll, with a
// line-number gutter, solid green/red change bars, muted row tints, and
// expand-context rows.

pub use ainb_app::components::code_review::render::*;

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};

use super::highlight::{self, BAR_ADD, BAR_DEL, GUTTER_FG};
use super::model::{ReviewModel, RowKind};

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

// ───────────────────────── sidebar file tree ─────────────────────────

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
        ui.sidebar_rect.set(crate::geometry::Area::default());
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
    ui.sidebar_rect.set(crate::geometry::Area::new(
        list_area.x,
        list_area.y,
        list_area.width,
        list_area.height,
    ));

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
                    Style::default().fg(super::super::git_view::status_color(&f.status)),
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
    use super::super::model::{DiffRow, ReviewFile};
    use super::*;
    use crate::components::code_review::model::Hunk;
    use crate::components::git_view::GitFileStatus;
    use std::collections::HashSet;

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
