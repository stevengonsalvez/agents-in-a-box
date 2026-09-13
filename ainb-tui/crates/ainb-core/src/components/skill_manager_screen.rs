// ABOUTME: Skills/units manager TUI screen — spec §10.1 layout.
//
// V1 ships the layout shell + render path. Live data binding to
// the actual manifest / lockfile / usage cache happens via
// `SkillsScreenData` populated by the runtime (TODO follow-up: wire
// from ainb-cli helpers + ainb-usage). The tripwire test in
// `tests/test_skills_screen.rs` only needs the render path to be
// deterministic and to surface the spec markers (Sources / Units /
// Detail / help bar) so the cutover gate (P8) is satisfied.

pub use ainb_app::components::skill_manager_screen::*;

use std::path::Path;

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Cell, Clear, Paragraph, Row, Table, TableState},
};

use ainb_skill_core::drift::DriftStatus;

// Style guide constants — match the rest of ainb-tui's components
// (cornflower borders, gold titles, soft white text, muted gray for
// helper text). These mirror crates/ainb-tui/src/components/home_screen_v2.rs.
const CORNFLOWER_BLUE: Color = Color::Rgb(100, 149, 237);
const GOLD: Color = Color::Rgb(255, 215, 0);
const SOFT_WHITE: Color = Color::Rgb(220, 220, 230);
const MUTED_GRAY: Color = Color::Rgb(120, 120, 140);
const SELECTION_GREEN: Color = Color::Rgb(100, 200, 100);
const LIST_HIGHLIGHT_BG: Color = Color::Rgb(40, 40, 60);
// Soft red for inline validation (e.g. a `/path` in the add-source box).
const ERROR_RED: Color = Color::Rgb(230, 110, 110);

/// True when `normalized` parses to a unit URI (carries a `/path`) —
/// which `ainb source add` rejects (`ainb-cli` source.rs). Drives the
/// inline "drop the /path" hint shown before the user hits Enter.
fn source_input_has_path(normalized: &str) -> bool {
    ainb_skill_core::Uri::parse(normalized).ok().and_then(|u| u.path).is_some()
}

/// Render the spec §10.1 skills screen into `area`.
pub fn render(frame: &mut Frame, area: Rect, data: &SkillsScreenData) {
    // Vertical split: top row = sources|units, middle = detail, bottom = help.
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(8),
            Constraint::Length(8),
            Constraint::Length(1),
        ])
        .split(area);

    // Top row horizontal split: resizable `sources_width` cols for the
    // Sources panel, rest for Units. Width is normalized against the
    // actual draw width so a stale/oversized persisted value can never
    // starve the Units table.
    let sources_w = clamp_sources_width(data.sources_width, outer[0].width);
    let top = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(sources_w), Constraint::Min(40)])
        .split(outer[0]);

    render_sources_panel(frame, top[0], data);
    render_units_table(frame, top[1], data);
    render_detail_pane(frame, outer[1], data);
    render_help_bar(frame, outer[2]);

    // Discovery banner overlay (spec §User Flow 1) — drawn LAST
    // so it sits on top of the underlying panels. Hidden state
    // is a no-op.
    if let Some(counts) = data.banner.counts() {
        let detailed = matches!(data.banner, DiscoveryBannerState::Details(_));
        render_discovery_banner(frame, area, counts, detailed);
    }

    // Own-skill Library overlay (`[l]`) — drawn above the panels +
    // banner so it's the active modal when open. Sits below the input
    // prompt (which is never open at the same time).
    if let Some(library) = &data.library {
        render_library_view(frame, area, library);
    }

    // Catalog browse overlay (`[b]`, bead ai-a20) — drawn above the
    // panels + banner. Never open at the same time as the Library
    // overlay or the input prompt.
    if let Some(browse) = &data.browse {
        render_browse_view(frame, area, browse);
    }

    // Source-preview picker — drawn above the panels + banner (active
    // modal after an add-source fetch or `[p]` on a source row).
    if let Some(preview) = &data.preview {
        render_source_preview(frame, area, preview);
    }

    // Source-removal confirm — drawn above everything (active modal).
    if let Some(confirm) = &data.source_remove_confirm {
        render_source_remove_confirm(frame, area, confirm);
    }

    // Sync assess-then-apply — the plan rendered as a git-style diff.
    if let Some(sc) = &data.sync_confirm {
        render_sync_confirm(frame, area, sc);
    }

    // Background fetch in flight — small centered banner so the user
    // sees progress instead of a frozen screen.
    if let Some(uri) = &data.preview_loading {
        let rect = centered_rect(area, (uri.len() as u16 + 20).clamp(30, area.width), 3);
        frame.render_widget(Clear, rect);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(GOLD));
        let inner = block.inner(rect);
        frame.render_widget(block, rect);
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("⏳ fetching ", Style::default().fg(SOFT_WHITE)),
                Span::styled(uri.clone(), Style::default().fg(CORNFLOWER_BLUE)),
                Span::styled(" …", Style::default().fg(MUTED_GRAY)),
            ]))
            .alignment(ratatui::layout::Alignment::Center),
            inner,
        );
    }

    // Input prompt overlay (add-source / search) — drawn on top of
    // everything, including the banner, since it's the active modal.
    if let Some(input) = &data.input {
        render_input_prompt(frame, area, input);
    }
}

/// Render the own-skill Library overlay (`[l]`, bead ai-lgk). A
/// centered panel titled "Own-Skill Library" listing the
/// `library.yaml`-registered owned units, with a deploy-status column.
/// `[Enter]` expands the selected row into a "Library Detail" band
/// beneath the list (name + kind + path + created + deploy).
fn render_library_view(frame: &mut Frame, area: Rect, library: &LibraryViewState) {
    let width = area.width.saturating_sub(8).clamp(40, 100);
    // Body = header + one line per row (+ detail band when expanded),
    // bounded by the available height.
    let detail_lines: u16 = if library.show_detail { 7 } else { 0 };
    let list_lines = (library.rows.len() as u16).max(1);
    let height = (list_lines + detail_lines + 4).min(area.height.saturating_sub(2)).max(8);
    let rect = centered_rect(area, width, height);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(GOLD))
        .title(Span::styled(
            " Own-Skill Library ",
            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
        ));

    let mut lines: Vec<Line> = Vec::new();
    if library.rows.is_empty() {
        lines.push(Line::from(Span::styled(
            "  No owned skills yet.",
            Style::default().fg(MUTED_GRAY).add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "  Run `ainb skill library new <name>` to author one.",
            Style::default().fg(SOFT_WHITE),
        )));
    } else {
        // Column header.
        lines.push(Line::from(vec![Span::styled(
            format!(" {:<24} {:<8} {:<28} {}", "name", "kind", "path", "deploy"),
            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
        )]));
        for (i, row) in library.rows.iter().enumerate() {
            let selected = i == library.selected;
            let marker = if selected { "▶ " } else { "  " };
            let style = if selected {
                Style::default()
                    .bg(LIST_HIGHLIGHT_BG)
                    .fg(SELECTION_GREEN)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(SOFT_WHITE)
            };
            lines.push(Line::from(vec![Span::styled(
                format!(
                    "{marker}{:<24} {:<8} {:<28} {}",
                    row.name, row.kind, row.path, row.deploy
                ),
                style,
            )]));
        }
    }

    // Expanded detail band on `[Enter]`.
    if library.show_detail {
        if let Some(row) = library.selected_row() {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                " ── Library Detail ──",
                Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
            )));
            lines.push(line_kv("Name", &row.name));
            lines.push(line_kv("Kind", &row.kind));
            lines.push(line_kv("Path", &row.path));
            lines.push(line_kv("Created", &row.created));
            lines.push(line_kv("Deploy", &row.deploy));
        }
    }

    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::raw(" "),
        key_span("↑↓"),
        Span::styled(" move  ", Style::default().fg(MUTED_GRAY)),
        key_span("Enter"),
        Span::styled(" detail  ", Style::default().fg(MUTED_GRAY)),
        key_span("Esc"),
        Span::styled(" close", Style::default().fg(MUTED_GRAY)),
    ]));

    frame.render_widget(Clear, rect);
    let para = Paragraph::new(lines).block(block);
    frame.render_widget(para, rect);
}

/// Render the catalog browse overlay (`[b]`, bead ai-a20). A centered
/// modal with a query input line on top and the ranked result list
/// below. In Query mode the input is the active focus (type → Enter
/// searches); in Results mode the list is focused (arrows → Enter
/// installs the selected hit).
/// Source-preview picker: left = unit list with checkboxes, right =
/// frontmatter insight for the cursor row, bottom = target-tool
/// checkboxes + key hints.
fn render_source_preview(frame: &mut Frame, area: Rect, view: &SourcePreviewViewState) {
    let width = area.width.saturating_sub(4).clamp(60, 130);
    let height = area.height.saturating_sub(2).clamp(14, 40);
    let rect = centered_rect(area, width, height);
    frame.render_widget(Clear, rect);

    let p = &view.preview;
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(GOLD))
        .title(Span::styled(
            format!(
                " Import from {} — {} unit(s), {} selected ",
                p.stored_uri,
                p.units.len(),
                view.checked_count()
            ),
            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(4),    // unit list | detail
            Constraint::Length(1), // tool checkboxes
            Constraint::Length(1), // key hints
            Constraint::Length(1), // CLI-equivalent tip
        ])
        .split(inner);

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
        .split(rows[0]);

    // ── Left: scrolling unit list with checkboxes.
    let visible = cols[0].height as usize;
    let top = view.cursor.saturating_sub(visible.saturating_sub(1));
    let mut list_lines: Vec<Line> = Vec::new();
    for (i, unit) in p.units.iter().enumerate().skip(top).take(visible) {
        let on_cursor = i == view.cursor;
        let checked = view.checked.get(i).copied().unwrap_or(false);
        let box_glyph = if checked { "[x] " } else { "[ ] " };
        let marker = if on_cursor { "\u{25b6} " } else { "  " };
        let name_style = if on_cursor {
            Style::default().fg(SELECTION_GREEN).add_modifier(Modifier::BOLD)
        } else if checked {
            Style::default().fg(SOFT_WHITE)
        } else {
            Style::default().fg(MUTED_GRAY)
        };
        let mut spans = vec![
            Span::styled(marker, Style::default().fg(SELECTION_GREEN)),
            Span::styled(
                box_glyph,
                Style::default().fg(if checked { SELECTION_GREEN } else { MUTED_GRAY }),
            ),
            Span::styled(
                format!("{:<8}", unit.kind),
                Style::default().fg(CORNFLOWER_BLUE),
            ),
            Span::styled(unit.name.clone(), name_style),
        ];
        if view.installed.get(i).copied().unwrap_or(false) {
            spans.push(Span::styled(
                "  \u{2713} installed",
                Style::default().fg(SELECTION_GREEN).add_modifier(Modifier::DIM),
            ));
        }
        list_lines.push(Line::from(spans));
    }
    frame.render_widget(Paragraph::new(list_lines), cols[0]);

    // ── Right: frontmatter insight for the cursor row.
    let mut detail: Vec<Line> = Vec::new();
    if let Some(unit) = p.units.get(view.cursor) {
        detail.push(Line::from(vec![
            Span::styled("name  ", Style::default().fg(MUTED_GRAY)),
            Span::styled(
                unit.name.clone(),
                Style::default().fg(SOFT_WHITE).add_modifier(Modifier::BOLD),
            ),
        ]));
        detail.push(Line::from(vec![
            Span::styled("kind  ", Style::default().fg(MUTED_GRAY)),
            Span::styled(unit.kind.clone(), Style::default().fg(CORNFLOWER_BLUE)),
        ]));
        detail.push(Line::from(vec![
            Span::styled("path  ", Style::default().fg(MUTED_GRAY)),
            Span::styled(unit.path.clone(), Style::default().fg(MUTED_GRAY)),
        ]));
        detail.push(Line::from(""));
        detail.push(Line::from(Span::styled(
            unit.description.clone().unwrap_or_else(|| "(no description)".to_string()),
            Style::default().fg(SOFT_WHITE),
        )));
        if !unit.tags.is_empty() {
            detail.push(Line::from(""));
            detail.push(Line::from(Span::styled(
                format!("tags: {}", unit.tags.join(", ")),
                Style::default().fg(MUTED_GRAY),
            )));
        }
        if !unit.requires.is_empty() {
            detail.push(Line::from(Span::styled(
                format!("requires: {}", unit.requires.join(", ")),
                Style::default().fg(MUTED_GRAY),
            )));
        }
    }
    frame.render_widget(
        Paragraph::new(detail).wrap(ratatui::widgets::Wrap { trim: true }).block(
            Block::default()
                .borders(Borders::LEFT)
                .border_style(Style::default().fg(MUTED_GRAY)),
        ),
        cols[1],
    );

    // ── Tool checkboxes.
    let mut tool_spans: Vec<Span> = vec![Span::styled(
        " Install to  ",
        Style::default().fg(SOFT_WHITE).add_modifier(Modifier::BOLD),
    )];
    for (i, (tool, on)) in PREVIEW_TOOLS.iter().zip(&view.tools).enumerate() {
        tool_spans.push(Span::styled(
            format!("{} ", i + 1),
            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
        ));
        tool_spans.push(Span::styled(
            format!("[{}] {tool}   ", if *on { "x" } else { " " }),
            Style::default().fg(if *on { SELECTION_GREEN } else { MUTED_GRAY }),
        ));
    }
    tool_spans.push(Span::styled(
        "4 ",
        Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
    ));
    tool_spans.push(Span::styled("all", Style::default().fg(MUTED_GRAY)));
    frame.render_widget(Paragraph::new(Line::from(tool_spans)), rows[1]);

    // ── Key hints.
    let hints = Line::from(vec![
        Span::styled(" \u{2191}\u{2193}", Style::default().fg(GOLD)),
        Span::styled(" move  ", Style::default().fg(MUTED_GRAY)),
        Span::styled("Space", Style::default().fg(GOLD)),
        Span::styled(" toggle  ", Style::default().fg(MUTED_GRAY)),
        Span::styled("a", Style::default().fg(GOLD)),
        Span::styled(" all  ", Style::default().fg(MUTED_GRAY)),
        Span::styled("n", Style::default().fg(GOLD)),
        Span::styled(" none  ", Style::default().fg(MUTED_GRAY)),
        Span::styled("Enter", Style::default().fg(GOLD)),
        Span::styled(" import  ", Style::default().fg(MUTED_GRAY)),
        Span::styled("Esc", Style::default().fg(GOLD)),
        Span::styled(" cancel", Style::default().fg(MUTED_GRAY)),
    ]);
    frame.render_widget(Paragraph::new(hints), rows[2]);

    // ── CLI-equivalent tip: the same import as a shell command, so the
    // TUI stays discoverable from (and teaches) the CLI surface.
    let targets = view.targets_csv().unwrap_or_else(|| "claude".to_string());
    let cli_tip = Line::from(vec![
        Span::styled(
            " CLI  ",
            Style::default().fg(MUTED_GRAY).add_modifier(Modifier::DIM),
        ),
        Span::styled(
            format!(
                "ainb skill install {}@{}/<unit> --targets {targets}",
                p.stored_uri, p.r#ref
            ),
            Style::default().fg(MUTED_GRAY).add_modifier(Modifier::DIM),
        ),
    ]);
    frame.render_widget(Paragraph::new(cli_tip), rows[3]);
}

/// Confirm dialog for `[r]` on a source: remove skills + source, remove
/// skills but keep the source (back to preview), or cancel.
fn render_source_remove_confirm(frame: &mut Frame, area: Rect, c: &SourceRemoveConfirm) {
    let width = (c.source_uri.len() as u16 + 20).clamp(52, area.width.saturating_sub(4));
    let rect = centered_rect(area, width, 11);
    frame.render_widget(Clear, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(ERROR_RED))
        .title(Span::styled(
            " Remove source ",
            Style::default().fg(ERROR_RED).add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);

    // One label per SourceRemoveChoice, in ALL order — the cursor and the
    // handler both index this same list, so they can't drift.
    let rows: Vec<String> = SourceRemoveChoice::ALL
        .iter()
        .map(|choice| match choice {
            SourceRemoveChoice::RemoveSkillsAndSource => format!(
                "Remove skills + source  ({} unit(s), drops the dependency)",
                c.unit_count
            ),
            SourceRemoveChoice::RemoveSkillsKeepSource => {
                "Remove skills, keep source  (back to preview — re-import via [p])".to_string()
            }
            SourceRemoveChoice::Cancel => "Cancel".to_string(),
        })
        .collect();
    let mut lines: Vec<Line> = vec![
        Line::from(Span::styled(
            format!("{}  ({})", c.source_name, c.source_uri),
            Style::default().fg(SOFT_WHITE).add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
    ];
    for (i, label) in rows.iter().enumerate() {
        let on = i == c.cursor;
        let marker = if on { "\u{25b6} " } else { "  " };
        let style = if on {
            Style::default().fg(SELECTION_GREEN).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(SOFT_WHITE)
        };
        lines.push(Line::from(vec![
            Span::styled(marker, Style::default().fg(SELECTION_GREEN)),
            Span::styled(label.clone(), style),
        ]));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled("\u{2191}\u{2193}", Style::default().fg(GOLD)),
        Span::styled(" move \u{b7} ", Style::default().fg(MUTED_GRAY)),
        Span::styled("Enter", Style::default().fg(GOLD)),
        Span::styled(" confirm \u{b7} ", Style::default().fg(MUTED_GRAY)),
        Span::styled("Esc", Style::default().fg(GOLD)),
        Span::styled(" cancel", Style::default().fg(MUTED_GRAY)),
    ]));
    frame.render_widget(
        Paragraph::new(lines).wrap(ratatui::widgets::Wrap { trim: true }),
        inner,
    );
}

/// Assess-then-apply sync popup. Renders the dry-run plan as a
/// git-style coloured diff (`+` additions green, `-` removals red, `#`
/// section headers gold), scrollable, with apply/cancel hints.
fn render_sync_confirm(frame: &mut Frame, area: Rect, sc: &SyncConfirmState) {
    let width = area.width.saturating_sub(6).clamp(60, 120);
    let height = area.height.saturating_sub(4).clamp(12, 40);
    let rect = centered_rect(area, width, height);
    frame.render_widget(Clear, rect);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(GOLD))
        .title(Span::styled(
            format!(" Sync {} — review plan ", sc.label),
            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(1)])
        .split(inner);

    // Colour each plan line like a unified diff.
    let body_h = rows[0].height as usize;
    let start = sc.scroll.min(sc.plan.len().saturating_sub(1));
    let end = (start + body_h).min(sc.plan.len());
    let lines: Vec<Line> = sc.plan[start..end]
        .iter()
        .map(|l| {
            let t = l.trim_start();
            let style = if t.starts_with('+') {
                Style::default().fg(SELECTION_GREEN)
            } else if t.starts_with('-') || t.starts_with('~') {
                Style::default().fg(ERROR_RED)
            } else if t.starts_with('#') {
                Style::default().fg(GOLD).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(SOFT_WHITE)
            };
            Line::from(Span::styled(l.clone(), style))
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), rows[0]);

    let hints = Line::from(vec![
        Span::styled(
            format!(" [{}/{}] ", end, sc.plan.len().max(1)),
            Style::default().fg(MUTED_GRAY),
        ),
        Span::styled("\u{2191}\u{2193}", Style::default().fg(GOLD)),
        Span::styled(" scroll \u{b7} ", Style::default().fg(MUTED_GRAY)),
        Span::styled("Enter", Style::default().fg(GOLD)),
        Span::styled(" apply \u{b7} ", Style::default().fg(MUTED_GRAY)),
        Span::styled("Esc", Style::default().fg(GOLD)),
        Span::styled(" cancel", Style::default().fg(MUTED_GRAY)),
    ]);
    frame.render_widget(Paragraph::new(hints), rows[1]);
}

fn render_browse_view(frame: &mut Frame, area: Rect, browse: &BrowseViewState) {
    let width = area.width.saturating_sub(6).clamp(50, 110);
    let list_lines = (browse.results.len() as u16).max(1);
    let height = (list_lines + 8).min(area.height.saturating_sub(2)).max(10);
    let rect = centered_rect(area, width, height);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(GOLD))
        .title(Span::styled(
            format!(" Browse Catalog ({}) ", browse.catalog.label()),
            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
        ));

    let mut lines: Vec<Line> = Vec::new();

    // ── Query input line. A caret marks Query mode.
    let caret = if browse.mode == BrowseMode::Query {
        "_"
    } else {
        ""
    };
    lines.push(Line::from(vec![
        Span::styled(
            " Query: ",
            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("{}{caret}", browse.query),
            Style::default().fg(SOFT_WHITE),
        ),
    ]));

    // ── Status / hint line.
    if let Some(status) = &browse.status {
        lines.push(Line::from(Span::styled(
            format!("  {status}"),
            Style::default().fg(MUTED_GRAY).add_modifier(Modifier::BOLD),
        )));
    }
    lines.push(Line::from(""));

    // ── Results list.
    if browse.results.is_empty() {
        let hint = if browse.mode == BrowseMode::Query {
            if browse.catalog.lists_on_blank() {
                "  Press Enter to list all curated skills, or type to filter."
            } else {
                "  Type a query and press Enter to search."
            }
        } else {
            "  No results."
        };
        lines.push(Line::from(Span::styled(
            hint,
            Style::default().fg(MUTED_GRAY),
        )));
    } else {
        lines.push(Line::from(vec![Span::styled(
            format!(
                " {:<24} {:<7} {:>6}  {:<26} {}",
                "name", "kind", "stars", "repo", "install / command"
            ),
            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
        )]));
        for (i, row) in browse.results.iter().enumerate() {
            let selected = browse.mode == BrowseMode::Results && i == browse.selected;
            let marker = if selected { "▶ " } else { "  " };
            let style = if selected {
                Style::default()
                    .bg(LIST_HIGHLIGHT_BG)
                    .fg(SELECTION_GREEN)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(SOFT_WHITE)
            };
            lines.push(Line::from(vec![Span::styled(
                format!(
                    "{marker}{:<24} {:<7} {:>6}  {:<26} {}",
                    row.name,
                    row.kind.badge(),
                    row.stars,
                    row.repo,
                    row.install_uri
                ),
                style,
            )]));
        }
    }

    // ── Help footer — phase-aware.
    lines.push(Line::from(""));
    let footer = if browse.mode == BrowseMode::Query {
        vec![
            Span::raw(" "),
            key_span("Enter"),
            Span::styled(" search  ", Style::default().fg(MUTED_GRAY)),
            key_span("Tab"),
            Span::styled(" switch catalog  ", Style::default().fg(MUTED_GRAY)),
            key_span("Esc"),
            Span::styled(" close", Style::default().fg(MUTED_GRAY)),
        ]
    } else {
        vec![
            Span::raw(" "),
            key_span("↑↓"),
            Span::styled(" select  ", Style::default().fg(MUTED_GRAY)),
            key_span("Enter"),
            Span::styled(" install  ", Style::default().fg(MUTED_GRAY)),
            key_span("/"),
            Span::styled(" new search  ", Style::default().fg(MUTED_GRAY)),
            key_span("Tab"),
            Span::styled(" switch catalog  ", Style::default().fg(MUTED_GRAY)),
            key_span("Esc"),
            Span::styled(" close", Style::default().fg(MUTED_GRAY)),
        ]
    };
    lines.push(Line::from(footer));

    frame.render_widget(Clear, rect);
    let para = Paragraph::new(lines).block(block);
    frame.render_widget(para, rect);
}

/// Render the active text-input prompt as a centered single-line
/// overlay with a blinking-style caret. Used for both `[i] add source`
/// and `[/] search`.
fn render_input_prompt(frame: &mut Frame, area: Rect, input: &InputState) {
    match input.kind {
        InputKind::AddSource => render_add_source_prompt(frame, area, input),
        InputKind::Search => render_search_prompt(frame, area, input),
    }
}

/// Compact single-line filter prompt (the original overlay).
fn render_search_prompt(frame: &mut Frame, area: Rect, input: &InputState) {
    let width = BANNER_WIDTH.min(area.width.saturating_sub(4)).max(20);
    let rect = centered_rect(area, width, 5);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(GOLD))
        .title(Span::styled(
            input.title(),
            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
        ));

    let lines = vec![
        Line::from(""),
        Line::from(vec![
            Span::styled(" ▌ ", Style::default().fg(CORNFLOWER_BLUE)),
            Span::styled(
                format!("{}█", input.buffer),
                Style::default().fg(SOFT_WHITE),
            ),
        ]),
        Line::from(Span::styled(
            "   type to filter   [Enter] apply  [Esc] cancel",
            Style::default().fg(MUTED_GRAY),
        )),
    ];

    frame.render_widget(Clear, rect);
    frame.render_widget(Paragraph::new(lines).block(block), rect);
}

/// Backend legend rows for the add-source overlay: `(typed form, what it
/// is)`. Mirrors the fetchers actually wired in `ainb-cli` (gh/gist/git
/// → GitFetcher, https → file download, local → on-disk). `npm:` is
/// omitted because it bails ("not supported in v1").
const ADD_SOURCE_LEGEND: &[(&str, &str)] = &[
    ("gh:owner/repo", "GitHub           ← default"),
    ("gist:<id>", "GitHub gist"),
    ("git:https://host/o/r", "GitLab/Bitbucket/self-host"),
    ("https://host/f.tgz", "single file"),
    ("local:/abs/path", "on-disk folder"),
];

/// Full add-source overlay: input + live `resolves → gh:…` preview +
/// the backend legend. The preview applies [`normalize_source_input`]
/// so the user sees exactly what bare `owner/repo` becomes, and turns
/// red with a hint when the result carries a `/path` (rejected by
/// `ainb source add`).
fn render_add_source_prompt(frame: &mut Frame, area: Rect, input: &InputState) {
    let width = 58.min(area.width.saturating_sub(4)).max(24);
    let rect = centered_rect(area, width, 14);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(GOLD))
        .title(Span::styled(
            input.title(),
            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
        ));

    // Input line.
    let mut lines = vec![Line::from(vec![
        Span::styled(" ▌ ", Style::default().fg(CORNFLOWER_BLUE)),
        Span::styled(
            format!("{}█", input.buffer),
            Style::default().fg(SOFT_WHITE),
        ),
    ])];

    // Live preview / validation line.
    if input.buffer.trim().is_empty() {
        lines.push(Line::from(Span::styled(
            "   type a repo, e.g. owner/repo",
            Style::default().fg(MUTED_GRAY),
        )));
    } else {
        let normalized = normalize_source_input(&input.buffer);
        if source_input_has_path(&normalized) {
            lines.push(Line::from(Span::styled(
                "   ✗ drop the /path — that's `skill install`",
                Style::default().fg(ERROR_RED),
            )));
        } else {
            lines.push(Line::from(vec![
                Span::styled("   resolves → ", Style::default().fg(MUTED_GRAY)),
                Span::styled(normalized, Style::default().fg(SELECTION_GREEN)),
            ]));
        }
    }

    lines.push(Line::from(""));
    for (form, desc) in ADD_SOURCE_LEGEND {
        lines.push(Line::from(vec![
            Span::styled(format!("  {form:<22}"), Style::default().fg(GOLD)),
            Span::styled((*desc).to_string(), Style::default().fg(MUTED_GRAY)),
        ]));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "  +@ref (branch/tag/sha, default main)  ·  no /path",
        Style::default().fg(MUTED_GRAY),
    )));
    lines.push(Line::from(vec![
        Span::styled("  [Enter] ", Style::default().fg(GOLD)),
        Span::styled("add    ", Style::default().fg(MUTED_GRAY)),
        Span::styled("[Esc] ", Style::default().fg(GOLD)),
        Span::styled("cancel", Style::default().fg(MUTED_GRAY)),
    ]));

    frame.render_widget(Clear, rect);
    frame.render_widget(Paragraph::new(lines).block(block), rect);
}

fn render_sources_panel(frame: &mut Frame, area: Rect, data: &SkillsScreenData) {
    let focused = data.focused_pane == FocusedSkillPane::Sources;
    // Focused panel = bright/gold border; unfocused = muted cornflower.
    // The edge brightens further while a resize drag is in flight so the
    // divider is obvious mid-drag.
    let border_color = if focused || data.resize_active {
        GOLD
    } else {
        CORNFLOWER_BLUE
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border_color))
        .title(Span::styled(
            " Sources ",
            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
        ));

    let mut lines: Vec<Line> = Vec::new();
    if data.sources.is_empty() {
        lines.push(Line::from(Span::styled(
            "(no sources configured)",
            Style::default().fg(MUTED_GRAY),
        )));
    } else {
        // "All sources" affordance — selecting it (Esc / click) clears
        // the source filter. Highlighted when no source filter is active.
        // `▶` is the keyboard cursor (only on a focused, selected source);
        // `●` marks the active filter location. They never collide because
        // "All sources" isn't keyboard-selectable (cleared via Esc/click).
        let all_active = data.source_filter.is_none();
        let all_marker = if all_active { "● " } else { "  " };
        let all_style = if all_active {
            Style::default().fg(SELECTION_GREEN).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(MUTED_GRAY)
        };
        lines.push(Line::from(Span::styled(
            format!("{all_marker}All sources"),
            all_style,
        )));

        for (i, source) in data.sources.iter().enumerate() {
            let selected = focused && i == data.source_selected;
            // A source is "filtered-on" when its uri is the active
            // filter — shown even when the panel is unfocused so the
            // user can see which source the Units list is pinned to.
            let active_filter = data.source_filter.as_deref() == Some(source.uri.as_str());
            let marker = if selected {
                "▶ "
            } else if active_filter {
                "● "
            } else {
                "  "
            };
            let glyph = if source.enabled { "✓" } else { "✗" };
            let glyph_style = if source.enabled {
                Style::default().fg(GOLD)
            } else {
                Style::default().fg(MUTED_GRAY)
            };
            let (name_color, name_mods) = if selected {
                (SELECTION_GREEN, Modifier::BOLD)
            } else if active_filter {
                (SELECTION_GREEN, Modifier::empty())
            } else {
                (SOFT_WHITE, Modifier::empty())
            };
            let name = Span::styled(
                format!(" {:<12} ", source.name),
                Style::default().fg(name_color).add_modifier(name_mods),
            );
            let uri = Span::styled(format!("({})", source.uri), Style::default().fg(MUTED_GRAY));
            let mut spans = vec![
                Span::styled(
                    marker,
                    Style::default().fg(SELECTION_GREEN).add_modifier(Modifier::BOLD),
                ),
                Span::styled(glyph, glyph_style),
                name,
                uri,
            ];
            if source.is_library {
                spans.push(Span::styled(
                    "  ★lib",
                    Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
                ));
            }
            lines.push(Line::from(spans));
        }
    }
    let para = Paragraph::new(lines).block(block);
    frame.render_widget(para, area);
}

fn render_units_table(frame: &mut Frame, area: Rect, data: &SkillsScreenData) {
    let focused = data.focused_pane == FocusedSkillPane::Units;
    let border_color = if focused { GOLD } else { CORNFLOWER_BLUE };
    // Title reflects the active source filter so the user always knows
    // which source the list is pinned to. The source's short `name` is
    // friendlier than its `uri`, so look it up; fall back to the uri.
    let title = match data.source_filter.as_deref() {
        Some(uri) => {
            let label = data
                .sources
                .iter()
                .find(|s| s.uri == uri)
                .map(|s| s.name.as_str())
                .unwrap_or(uri);
            format!(" Units (filtered: {label}) ")
        }
        None => " Units ".to_string(),
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border_color))
        .title(Span::styled(
            title,
            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
        ));

    // Empty-state hint: when the manifest has no units, render a
    // muted paragraph inside the panel pointing at the actions the
    // user can take next. Skip the table render entirely — empty
    // headers were the source of "looks broken" feedback.
    if data.units.is_empty() {
        let lines = vec![
            Line::from(Span::styled(
                "  No units installed yet",
                Style::default().fg(MUTED_GRAY).add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "  Press [i] to add a source (e.g. gh:owner/repo)",
                Style::default().fg(SOFT_WHITE),
            )),
            Line::from(Span::styled(
                "  Or press [m] again to refresh discovery",
                Style::default().fg(SOFT_WHITE),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "  [q] to return home",
                Style::default().fg(MUTED_GRAY),
            )),
        ];
        let para = Paragraph::new(lines).block(block);
        frame.render_widget(para, area);
        return;
    }

    let header = Row::new(vec![
        Cell::from("#"),
        Cell::from("name"),
        Cell::from("kind"),
        Cell::from("source"),
        Cell::from("ref"),
        Cell::from("targets"),
        Cell::from("status"),
    ])
    .style(Style::default().fg(GOLD).add_modifier(Modifier::BOLD));

    // Apply the active search filter: only rows whose index is in
    // `visible_indices()` are shown. The cursor highlight is mapped
    // from the absolute `selected` index to its position within the
    // visible slice further down.
    let visible = data.visible_indices();
    let rows: Vec<Row> = visible
        .iter()
        .filter_map(|&i| data.units.get(i))
        .map(|u| {
            let targets = u.targets.join(" ");
            let (glyph, glyph_color) =
                drift_status_glyph(data.drift_cache.get(&u.declared_uri).copied());
            Row::new(vec![
                Cell::from(u.idx.to_string()),
                Cell::from(u.name.clone()),
                Cell::from(u.kind.clone()),
                Cell::from(u.source.clone()),
                Cell::from(u.git_ref.clone()),
                Cell::from(targets),
                Cell::from(Span::styled(
                    glyph.to_string(),
                    Style::default().fg(glyph_color).add_modifier(Modifier::BOLD),
                )),
            ])
        })
        .collect();

    // Constraint mix: small numeric col fixed, name/source/targets
    // share remaining width via Percentage so narrow terminals don't
    // crop the targets list. Previously all Length-based which
    // overflowed when total > area.width. The status column is fixed
    // at 6 cols (room for the glyph + a 2-char count when we expand
    // later).
    let widths = [
        Constraint::Length(3),
        Constraint::Percentage(22),
        Constraint::Length(8),
        Constraint::Percentage(22),
        Constraint::Length(10),
        Constraint::Percentage(40),
        Constraint::Length(6),
    ];
    let table = Table::new(rows, widths)
        .header(header)
        .block(block)
        .highlight_style(
            Style::default()
                .bg(LIST_HIGHLIGHT_BG)
                .fg(SELECTION_GREEN)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");

    // Map the absolute `selected` index to its position within the
    // visible (filtered) slice so the highlight tracks the cursor.
    let mut table_state = TableState::default();
    let highlight_pos = visible.iter().position(|&i| i == data.selected).unwrap_or(0);
    table_state.select(Some(highlight_pos));
    frame.render_stateful_widget(table, area, &mut table_state);
}

fn render_detail_pane(frame: &mut Frame, area: Rect, data: &SkillsScreenData) {
    let title = match data.detail.as_ref() {
        Some(_) if !data.units.is_empty() => {
            let sel = data.units.get(data.selected.min(data.units.len().saturating_sub(1)));
            match sel {
                Some(u) => format!(" Detail (#{} {}) ", u.idx, u.name),
                None => " Detail ".to_string(),
            }
        }
        _ => " Detail ".to_string(),
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(CORNFLOWER_BLUE))
        .title(Span::styled(
            title,
            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
        ));

    let mut lines: Vec<Line> = Vec::new();
    if let Some(d) = data.detail.as_ref() {
        lines.push(line_kv("URI", &d.uri));
        if !d.deployed.is_empty() {
            lines.push(line_kv("Deployed", &d.deployed.join(", ")));
        }
        if let Some(inv) = d.invocations {
            let when = d
                .last_used
                .as_deref()
                .map(format_time_ago)
                .unwrap_or_else(|| "never".to_string());
            lines.push(line_kv(
                "Usage",
                &format!("{inv} invocations · last used {when}"),
            ));
        }
        if !d.requires.is_empty() {
            lines.push(line_kv("Requires", &d.requires.join(", ")));
        }
        if !d.upstream_status.is_empty() {
            lines.push(line_kv("Upstream", &d.upstream_status));
        }
    } else {
        lines.push(Line::from(Span::styled(
            "(select a unit to see details)",
            Style::default().fg(MUTED_GRAY),
        )));
    }
    let para = Paragraph::new(lines).block(block);
    frame.render_widget(para, area);
}

fn render_help_bar(frame: &mut Frame, area: Rect) {
    let spans = vec![
        key_span("Tab"),
        Span::styled(" focus  ", Style::default().fg(MUTED_GRAY)),
        key_span("[ ]"),
        Span::styled(" resize  ", Style::default().fg(MUTED_GRAY)),
        key_span("i"),
        Span::styled("nstall  ", Style::default().fg(MUTED_GRAY)),
        key_span("u"),
        Span::styled("pdate  ", Style::default().fg(MUTED_GRAY)),
        key_span("c"),
        Span::styled("heck  ", Style::default().fg(MUTED_GRAY)),
        key_span("r"),
        Span::styled("emove  ", Style::default().fg(MUTED_GRAY)),
        key_span("s"),
        Span::styled("ync  ", Style::default().fg(MUTED_GRAY)),
        key_span("o"),
        Span::styled("pen  ", Style::default().fg(MUTED_GRAY)),
        key_span("y"),
        Span::styled(" copy→lib  ", Style::default().fg(MUTED_GRAY)),
        key_span("L"),
        Span::styled(" mark-lib  ", Style::default().fg(MUTED_GRAY)),
        key_span("b"),
        Span::styled("rowse  ", Style::default().fg(MUTED_GRAY)),
        key_span("l"),
        Span::styled("ibrary  ", Style::default().fg(MUTED_GRAY)),
        key_span("/"),
        Span::styled("search  ", Style::default().fg(MUTED_GRAY)),
        key_span("Esc"),
        Span::styled(" clear  ", Style::default().fg(MUTED_GRAY)),
        key_span("q"),
        Span::styled(" quit", Style::default().fg(MUTED_GRAY)),
    ];
    let p = Paragraph::new(Line::from(spans));
    frame.render_widget(p, area);
}

fn line_kv(label: &str, value: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("{label}: "),
            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
        ),
        Span::styled(value.to_string(), Style::default().fg(SOFT_WHITE)),
    ])
}

fn key_span(key: &str) -> Span<'static> {
    Span::styled(
        format!("[{key}]"),
        Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
    )
}

/// Render an RFC 3339 timestamp as a human-friendly "Xs/m/h/d ago"
/// string, anchored to the current wall clock. Unparseable input
/// falls back to the raw value so the Detail pane stays informative
/// even when the lockfile carries a non-standard timestamp.
fn format_time_ago(rfc3339: &str) -> String {
    let Ok(stamp) = chrono::DateTime::parse_from_rfc3339(rfc3339) else {
        return rfc3339.to_string();
    };
    let now = chrono::Utc::now().with_timezone(stamp.offset());
    let delta = now.signed_duration_since(stamp);
    let secs = delta.num_seconds();
    if secs < 0 {
        // Stamp is in the future — fall back to the raw timestamp
        // rather than printing a nonsensical negative duration.
        return rfc3339.to_string();
    }
    if secs < 60 {
        format!("{secs}s ago")
    } else if secs < 60 * 60 {
        format!("{}m ago", secs / 60)
    } else if secs < 60 * 60 * 24 {
        format!("{}h ago", secs / 3600)
    } else {
        format!("{}d ago", secs / 86_400)
    }
}

// ---------------------------------------------------------------------
// Discovery banner (spec §User Flow 1 + §P5)
// ---------------------------------------------------------------------

/// Width of the banner overlay (matches the ASCII mockup in spec
/// §User Flow 1). Banner is centered horizontally inside `area`.
const BANNER_WIDTH: u16 = 44;
/// Title rendered in the banner border. Stable string so tripwires
/// can assert against it.
pub const BANNER_TITLE: &str = "Detected existing units — import them?";

/// Render the discovery banner overlay on top of the main screen.
/// The caller decides whether to invoke this by checking
/// [`SkillsScreenData::banner`].
fn render_discovery_banner(
    frame: &mut Frame,
    area: Rect,
    counts: &DiscoveryBannerCounts,
    detailed: bool,
) {
    // Calculate body height: 1 line per displayed row + blank
    // separators + help bar. Cap at the available area height.
    let mut body_lines: Vec<Line<'static>> = Vec::new();
    body_lines.push(banner_row(
        "Marketplace plugins:",
        counts.marketplace_plugins,
    ));
    body_lines.push(banner_row("Orphan units:", counts.orphan_units_total));
    if detailed {
        for (tool, n) in &counts.orphan_units_per_tool {
            body_lines.push(banner_row_indent(&format!("~/.{tool}/skills/"), *n));
        }
    }
    body_lines.push(Line::from(""));
    body_lines.push(banner_row(
        "Conflicts (orphan wins by default):",
        counts.conflicts,
    ));
    body_lines.push(Line::from(""));
    body_lines.push(help_line());

    // Body height = number of lines, plus 2 for the rounded border.
    let height = (body_lines.len() as u16).saturating_add(2);
    let banner_area = centered_rect(area, BANNER_WIDTH, height);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(GOLD))
        .title(Span::styled(
            format!(" {BANNER_TITLE} "),
            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
        ));

    // Clear under the banner so the underlying panels don't bleed
    // through (Ratatui overlay convention).
    frame.render_widget(Clear, banner_area);
    let para = Paragraph::new(body_lines).block(block);
    frame.render_widget(para, banner_area);
}

fn banner_row(label: &str, n: usize) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!(" {label:<36}"), Style::default().fg(SOFT_WHITE)),
        Span::styled(
            format!("{n:>3} "),
            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
        ),
    ])
}

fn banner_row_indent(label: &str, n: usize) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("   {label:<34}"), Style::default().fg(MUTED_GRAY)),
        Span::styled(format!("{n:>3} "), Style::default().fg(SOFT_WHITE)),
    ])
}

fn help_line() -> Line<'static> {
    Line::from(vec![
        Span::raw(" "),
        key_span("Enter"),
        Span::styled(" import all  ", Style::default().fg(MUTED_GRAY)),
        key_span("d"),
        Span::styled(" details  ", Style::default().fg(MUTED_GRAY)),
        key_span("s"),
        Span::styled(" skip", Style::default().fg(MUTED_GRAY)),
    ])
}

fn centered_rect(area: Rect, width: u16, height: u16) -> Rect {
    let w = width.min(area.width);
    let h = height.min(area.height);
    let x = area.x + area.width.saturating_sub(w) / 2;
    let y = area.y + area.height.saturating_sub(h) / 2;
    Rect {
        x,
        y,
        width: w,
        height: h,
    }
}

// ---------------------------------------------------------------------
// Trigger + import (spec §User Flow 1 step 3-5)
// ---------------------------------------------------------------------

/// Reset the discovery banner state. Used by tests + by a future
/// `--force` flag that clears the skip marker AND re-runs the
/// banner trigger.
pub fn clear_discovery_skip_marker(ainb_home: &Path) -> std::io::Result<()> {
    let p = ainb_home.join(SKIP_MARKER_FILE);
    match std::fs::remove_file(&p) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Map a [`DriftStatus`] to the glyph + colour rendered in the Units
/// panel's `status` column.
///
/// `None` (drift not yet computed) renders an ellipsis in muted gray —
/// the cache is filled asynchronously on screen-enter (bead v12.E.4)
/// and rows pop into colour as results arrive.
fn drift_status_glyph(status: Option<DriftStatus>) -> (char, Color) {
    match status {
        None => ('…', MUTED_GRAY),
        Some(DriftStatus::InSync) => ('✓', SELECTION_GREEN),
        // Yellow-ish warning — picked from the same palette family the
        // doctor screen uses for "needs attention" rows.
        Some(DriftStatus::Outdated { .. }) => ('⚠', Color::Rgb(230, 200, 90)),
        Some(DriftStatus::Ahead { .. }) => ('▲', Color::Rgb(120, 200, 240)),
        Some(DriftStatus::Diverged { .. }) => ('⟷', Color::Rgb(230, 110, 110)),
    }
}

// ---------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use ainb_cli::discovery::class_a::{
        DiscoveredMarketplaceUnit, DiscoveredUnit, DiscoveredUnitKind,
    };
    use ainb_cli::discovery::reconcile::WalkerOutput;
    use ainb_skill_core::{Manifest, UnitEntry};

    fn mk_preview(unit_names: &[&str]) -> SourcePreviewViewState {
        SourcePreviewViewState::new(
            ainb_cli::source::SourcePreview {
                name: "test-src".to_string(),
                stored_uri: "gh:o/r".to_string(),
                r#ref: "main".to_string(),
                kind: "raw".to_string(),
                fetched_path: PathBuf::from("/tmp/x"),
                resolved_sha: "abc".to_string(),
                fetched_at: "now".to_string(),
                units: unit_names.iter().map(|n| ainb_adapters_source_unit(n)).collect(),
                already_added: false,
            },
            &std::collections::HashSet::new(),
        )
    }

    fn ainb_adapters_source_unit(name: &str) -> ainb_cli::source::UnitDescriptor {
        ainb_cli::source::UnitDescriptor {
            name: name.to_string(),
            kind: "skill".to_string(),
            description: Some(format!("{name} desc")),
            path: format!("skills/{name}"),
            tags: Vec::new(),
            requires: Vec::new(),
        }
    }

    /// Picker defaults: opt-in selection (nothing checked), claude the
    /// only target; a/n + Space + tool keys drive the state; targets_csv
    /// reflects the checkboxes and goes None when all are off.
    #[test]
    fn preview_picker_selection_and_targets() {
        let mut v = mk_preview(&["one", "two", "three"]);
        assert_eq!(v.checked_count(), 0, "opt-in default");
        assert_eq!(v.targets_csv().as_deref(), Some("claude"));

        v.toggle_current(); // check `one`
        v.move_cursor(1);
        v.toggle_current(); // check `two`
        assert_eq!(v.checked_paths(), vec!["skills/one", "skills/two"]);

        v.set_all(true);
        assert_eq!(v.checked_count(), 3);
        v.set_all(false);
        assert_eq!(v.checked_count(), 0);

        v.toggle_tool(1); // + codex
        assert_eq!(v.targets_csv().as_deref(), Some("claude,codex"));
        v.toggle_tool(0); // - claude
        v.toggle_tool(1); // - codex
        assert_eq!(v.targets_csv(), None, "no tools selected");
        v.toggle_tool(3); // all on
        assert_eq!(v.targets_csv().as_deref(), Some("claude,codex,copilot"));

        // Cursor clamps at both ends.
        v.move_cursor(-10);
        assert_eq!(v.cursor, 0);
        v.move_cursor(10);
        assert_eq!(v.cursor, 2);
    }
    use ainb_cli::discovery::class_c::DiscoveredOrphanUnit;
    use ainb_skill_core::UnitKind;

    use std::path::PathBuf;

    fn mk_orphan(tool: &str, name: &str) -> DiscoveredOrphanUnit {
        DiscoveredOrphanUnit {
            tool: tool.to_string(),
            kind: UnitKind::Skill,
            name: name.to_string(),
            path: PathBuf::from(format!("/fixture/{tool}/skills/{name}")),
            frontmatter_valid: false,
        }
    }

    fn mk_plugin(
        plugin: &str,
        marketplace: &str,
        unit_names: &[&str],
    ) -> DiscoveredMarketplaceUnit {
        DiscoveredMarketplaceUnit {
            plugin: plugin.to_string(),
            marketplace: marketplace.to_string(),
            version: "v1".to_string(),
            units: unit_names
                .iter()
                .map(|n| DiscoveredUnit {
                    kind: DiscoveredUnitKind::Skill,
                    name: (*n).to_string(),
                    path: PathBuf::from(format!(
                        "/fixture/cache/{marketplace}/{plugin}/v1/skills/{n}"
                    )),
                })
                .collect(),
        }
    }

    // ---- normalize_source_input ----------------------------------

    #[test]
    fn normalize_bare_repo_gets_gh_prefix() {
        assert_eq!(
            normalize_source_input("stevengonsalvez/ainb-toolkit"),
            "gh:stevengonsalvez/ainb-toolkit"
        );
    }

    #[test]
    fn normalize_bare_repo_with_ref() {
        assert_eq!(
            normalize_source_input("stevengonsalvez/ainb-toolkit@v1.2"),
            "gh:stevengonsalvez/ainb-toolkit@v1.2"
        );
    }

    #[test]
    fn normalize_trims_whitespace() {
        assert_eq!(normalize_source_input("  owner/repo  "), "gh:owner/repo");
    }

    #[test]
    fn normalize_existing_scheme_passes_through() {
        for s in [
            "gh:owner/repo",
            "git:https://gitlab.com/x/y",
            "gist:abc123",
            "local:/Users/me/skills",
            "https://example.com/x.tgz",
        ] {
            assert_eq!(normalize_source_input(s), s);
        }
    }

    #[test]
    fn normalize_local_paths_are_not_repos() {
        // No scheme but not `owner/repo` shaped → left alone for Uri::parse.
        for s in ["/Users/me/skills", "./foo", "a/b/c", "singlename"] {
            assert_eq!(normalize_source_input(s), s);
        }
    }

    #[test]
    fn source_input_path_detection() {
        assert!(!source_input_has_path("gh:owner/repo"));
        assert!(!source_input_has_path("gh:owner/repo@main"));
        assert!(source_input_has_path("gh:owner/repo@main/skills/commit"));
    }

    /// Flatten a rendered TestBackend buffer into one string for substring
    /// assertions (good enough — cell-level layout isn't what we're testing).
    fn render_add_source_to_string(buffer: &str) -> String {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut data = SkillsScreenData::default();
        data.input = Some(InputState {
            kind: InputKind::AddSource,
            buffer: buffer.to_string(),
        });
        terminal.draw(|f| render(f, f.size(), &data)).expect("render did not panic");
        terminal.backend().buffer().content().iter().map(|c| c.symbol()).collect()
    }

    #[test]
    fn add_source_overlay_renders_legend_and_live_preview() {
        // Empty buffer → placeholder + the legend rows.
        let empty = render_add_source_to_string("");
        assert!(empty.contains("type a repo"), "placeholder missing");
        assert!(empty.contains("GitHub"), "legend missing");
        assert!(empty.contains("GitLab"), "git: row missing");

        // Bare repo → live `resolves → gh:` preview.
        let repo = render_add_source_to_string("owner/repo");
        assert!(repo.contains("resolves"), "preview label missing");
        assert!(repo.contains("gh:owner/repo"), "normalized preview missing");

        // Unit URI (has /path) → the validation hint, not a resolve preview.
        let unit = render_add_source_to_string("gh:owner/repo@main/skills/commit");
        assert!(unit.contains("drop the /path"), "path validation missing");
    }

    // ---- compute_counts ------------------------------------------

    #[test]
    fn counts_empty() {
        let c = compute_counts(&WalkerOutput::default());
        assert_eq!(c.marketplace_plugins, 0);
        assert_eq!(c.orphan_units_total, 0);
        assert!(c.orphan_units_per_tool.is_empty());
        assert_eq!(c.conflicts, 0);
    }

    #[test]
    fn counts_per_tool_aggregates() {
        let w = WalkerOutput {
            class_c: vec![
                mk_orphan("claude", "a"),
                mk_orphan("claude", "b"),
                mk_orphan("codex", "c"),
            ],
            ..Default::default()
        };
        let c = compute_counts(&w);
        assert_eq!(c.orphan_units_total, 3);
        assert_eq!(
            c.orphan_units_per_tool,
            vec![("claude".to_string(), 2), ("codex".to_string(), 1)]
        );
    }

    #[test]
    fn counts_conflicts_only_in_claude_tool_home() {
        let w = WalkerOutput {
            class_a: vec![mk_plugin("reflect", "official", &["commit"])],
            class_c: vec![
                mk_orphan("claude", "commit"), // collides → conflict
                mk_orphan("codex", "commit"),  // different tool → not a conflict
            ],
        };
        let c = compute_counts(&w);
        assert_eq!(c.conflicts, 1);
    }

    #[test]
    fn counts_no_conflicts_when_names_differ() {
        let w = WalkerOutput {
            class_a: vec![mk_plugin("reflect", "official", &["commit"])],
            class_c: vec![mk_orphan("claude", "summarize")],
        };
        let c = compute_counts(&w);
        assert_eq!(c.conflicts, 0);
    }

    // ---- maybe_show_discovery_banner -----------------------------

    fn isolated_ainb_home() -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let ainb_home = tmp.path().join("ainb-home");
        std::fs::create_dir_all(&ainb_home).unwrap();
        (tmp, ainb_home)
    }

    fn synth_walker_with_one_candidate() -> WalkerOutput {
        WalkerOutput {
            class_c: vec![mk_orphan("claude", "x")],
            ..Default::default()
        }
    }

    #[test]
    fn trigger_visible_when_all_conditions_met() {
        let (_tmp, ainb_home) = isolated_ainb_home();
        let mut data = SkillsScreenData::default();
        maybe_show_discovery_banner(&mut data, &ainb_home, synth_walker_with_one_candidate());
        assert!(matches!(data.banner, DiscoveryBannerState::Visible(_)));
        assert!(data.walker_cache.is_some());
    }

    #[test]
    fn trigger_hidden_when_manifest_has_units() {
        let (_tmp, ainb_home) = isolated_ainb_home();
        let manifest = Manifest {
            sources: vec![],
            units: vec![UnitEntry {
                uri: "local:~/x@head/y".to_string(),
                targets: None,
                shadowed_by: None,
            }],
            ..Default::default()
        };
        manifest.save_to(&ainb_home.join("manifest.yaml")).unwrap();

        let mut data = SkillsScreenData::default();
        maybe_show_discovery_banner(&mut data, &ainb_home, synth_walker_with_one_candidate());
        assert!(matches!(data.banner, DiscoveryBannerState::Hidden));
    }

    #[test]
    fn trigger_hidden_when_skip_marker_present() {
        let (_tmp, ainb_home) = isolated_ainb_home();
        std::fs::write(ainb_home.join(SKIP_MARKER_FILE), b"").unwrap();
        let mut data = SkillsScreenData::default();
        maybe_show_discovery_banner(&mut data, &ainb_home, synth_walker_with_one_candidate());
        assert!(matches!(data.banner, DiscoveryBannerState::Hidden));
    }

    #[test]
    fn trigger_hidden_when_walker_has_no_candidates() {
        let (_tmp, ainb_home) = isolated_ainb_home();
        let mut data = SkillsScreenData::default();
        maybe_show_discovery_banner(&mut data, &ainb_home, WalkerOutput::default());
        assert!(matches!(data.banner, DiscoveryBannerState::Hidden));
    }

    #[test]
    fn trigger_idempotent_when_already_visible() {
        let (_tmp, ainb_home) = isolated_ainb_home();
        let mut data = SkillsScreenData::default();
        let prior = DiscoveryBannerCounts {
            marketplace_plugins: 42,
            orphan_units_total: 0,
            orphan_units_per_tool: vec![],
            conflicts: 0,
        };
        data.banner = DiscoveryBannerState::Visible(prior.clone());
        maybe_show_discovery_banner(&mut data, &ainb_home, synth_walker_with_one_candidate());
        // Untouched because banner was already active.
        assert_eq!(data.banner, DiscoveryBannerState::Visible(prior));
    }

    // ---- toggle_discovery_details -------------------------------

    #[test]
    fn toggle_details_flips_visible_to_details_and_back() {
        let counts = DiscoveryBannerCounts::default();
        let mut data = SkillsScreenData::default();
        data.banner = DiscoveryBannerState::Visible(counts.clone());
        toggle_discovery_details(&mut data);
        assert!(matches!(data.banner, DiscoveryBannerState::Details(_)));
        toggle_discovery_details(&mut data);
        assert!(matches!(data.banner, DiscoveryBannerState::Visible(_)));
    }

    #[test]
    fn toggle_details_noop_when_hidden() {
        let mut data = SkillsScreenData::default();
        toggle_discovery_details(&mut data);
        assert!(matches!(data.banner, DiscoveryBannerState::Hidden));
    }

    // ---- apply_discovery_skip -----------------------------------

    #[test]
    fn skip_writes_marker_and_hides() {
        let (_tmp, ainb_home) = isolated_ainb_home();
        let mut data = SkillsScreenData::default();
        data.banner = DiscoveryBannerState::Visible(DiscoveryBannerCounts::default());
        apply_discovery_skip(&mut data, &ainb_home).unwrap();
        assert!(matches!(data.banner, DiscoveryBannerState::Hidden));
        assert!(ainb_home.join(SKIP_MARKER_FILE).exists());
    }

    #[test]
    fn skip_then_clear_marker_allows_retrigger() {
        let (_tmp, ainb_home) = isolated_ainb_home();
        let mut data = SkillsScreenData::default();
        apply_discovery_skip(&mut data, &ainb_home).unwrap();
        assert!(ainb_home.join(SKIP_MARKER_FILE).exists());
        clear_discovery_skip_marker(&ainb_home).unwrap();
        assert!(!ainb_home.join(SKIP_MARKER_FILE).exists());
    }

    // ---- apply_discovery_import ---------------------------------

    #[test]
    fn import_writes_manifest_and_populates_view_model() {
        let (_tmp, ainb_home) = isolated_ainb_home();
        let walker = WalkerOutput {
            class_c: vec![mk_orphan("claude", "commit")],
            ..Default::default()
        };
        let mut data = SkillsScreenData::default();
        data.banner = DiscoveryBannerState::Visible(compute_counts(&walker));
        data.walker_cache = Some(walker);

        apply_discovery_import(&mut data, &ainb_home).unwrap();

        // Banner dismissed.
        assert!(matches!(data.banner, DiscoveryBannerState::Hidden));
        assert!(data.walker_cache.is_none());

        // Manifest written.
        let manifest = Manifest::load_from(&ainb_home.join("manifest.yaml")).unwrap();
        assert_eq!(manifest.units.len(), 1);
        assert_eq!(manifest.units[0].uri, "local:~/.claude/skills@head/commit");
        assert_eq!(manifest.sources.len(), 1);

        // View-model refreshed.
        assert_eq!(data.units.len(), 1);
        assert_eq!(data.sources.len(), 1);
    }

    #[test]
    fn import_no_op_when_no_walker_cache() {
        let (_tmp, ainb_home) = isolated_ainb_home();
        let mut data = SkillsScreenData::default();
        data.banner = DiscoveryBannerState::Visible(DiscoveryBannerCounts::default());
        apply_discovery_import(&mut data, &ainb_home).unwrap();
        assert!(matches!(data.banner, DiscoveryBannerState::Hidden));
        // No manifest was written.
        assert!(!ainb_home.join("manifest.yaml").exists());
    }

    // ---- render smoke (banner draws without panicking) ----------

    #[test]
    fn render_with_visible_banner_does_not_panic() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut data = SkillsScreenData::default();
        data.banner = DiscoveryBannerState::Visible(DiscoveryBannerCounts {
            marketplace_plugins: 3,
            orphan_units_total: 9,
            orphan_units_per_tool: vec![("claude".to_string(), 6), ("codex".to_string(), 3)],
            conflicts: 0,
        });
        terminal.draw(|f| render(f, f.size(), &data)).expect("render did not panic");
    }

    #[test]
    fn render_banner_contains_title_and_help_markers() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut data = SkillsScreenData::default();
        data.banner = DiscoveryBannerState::Visible(DiscoveryBannerCounts {
            marketplace_plugins: 3,
            orphan_units_total: 9,
            orphan_units_per_tool: vec![],
            conflicts: 1,
        });
        terminal.draw(|f| render(f, f.size(), &data)).unwrap();

        let buf = terminal.backend().buffer().clone();
        let mut joined = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                joined.push_str(buf.get(x, y).symbol());
            }
            joined.push('\n');
        }
        // Substring-AND, not OR — per project_ainb_tui_width_aware_panels
        // / tmux-ui-tripwire hard rules. Each marker proves a distinct
        // banner element painted.
        assert!(joined.contains(BANNER_TITLE), "title missing: {joined}");
        assert!(
            joined.contains("Marketplace plugins:"),
            "mp row missing: {joined}"
        );
        assert!(
            joined.contains("Orphan units:"),
            "orphan row missing: {joined}"
        );
        assert!(
            joined.contains("Conflicts"),
            "conflicts row missing: {joined}"
        );
        assert!(joined.contains("[Enter]"), "help: Enter missing: {joined}");
        assert!(joined.contains("[d]"), "help: d missing: {joined}");
        assert!(joined.contains("[s]"), "help: s missing: {joined}");
    }

    // -----------------------------------------------------------------
    // Resizable / focusable / filterable Sources panel (this change)
    // -----------------------------------------------------------------

    fn src(name: &str, uri: &str) -> SourceRow {
        SourceRow {
            name: name.to_string(),
            uri: uri.to_string(),
            r#ref: "main".to_string(),
            enabled: true,
            is_library: false,
        }
    }

    fn unit_in(idx: usize, name: &str, source: &str) -> UnitRow {
        UnitRow {
            idx,
            name: name.to_string(),
            kind: "skill".to_string(),
            source: source.to_string(),
            git_ref: "main".to_string(),
            targets: vec!["claude".to_string()],
            declared_uri: format!("{source}@main/{name}"),
        }
    }

    /// Two sources, three units (2 from acme, 1 from beta). The fixture
    /// the filter / nav tests below share.
    fn two_source_fixture() -> SkillsScreenData {
        SkillsScreenData {
            sources: vec![src("acme", "gh:org/acme"), src("beta", "gh:org/beta")],
            units: vec![
                unit_in(1, "alpha", "gh:org/acme"),
                unit_in(2, "bravo", "gh:org/beta"),
                unit_in(3, "charlie", "gh:org/acme"),
            ],
            ..Default::default()
        }
    }

    #[test]
    fn default_has_sane_resize_and_focus_state() {
        // TRAP guard: a derived Default would zero `sources_width`.
        let data = SkillsScreenData::default();
        assert_eq!(data.sources_width, DEFAULT_SOURCES_WIDTH);
        assert_eq!(data.focused_pane, FocusedSkillPane::Units);
        assert!(data.source_filter.is_none());
        assert!(!data.resize_active);
    }

    #[test]
    fn visible_indices_filters_by_source() {
        let mut data = two_source_fixture();
        // No filter → all three units visible.
        assert_eq!(data.visible_indices(), vec![0, 1, 2]);

        // Filter to acme → only the two acme units (idx 0 + 2).
        data.source_filter = Some("gh:org/acme".to_string());
        assert_eq!(data.visible_indices(), vec![0, 2]);

        // Filter to beta → only bravo (idx 1).
        data.source_filter = Some("gh:org/beta".to_string());
        assert_eq!(data.visible_indices(), vec![1]);
    }

    #[test]
    fn visible_indices_filters_marketplace_units_by_marketplace() {
        // Regression: selecting a marketplace source showed no skills because
        // a marketplace unit `marketplace:<plugin>@<marketplace>/...` has a
        // `<type>:<locator>` of `marketplace:<plugin>`, but its source is the
        // whole marketplace, `marketplace:<marketplace>`.
        let mk = |idx, name: &str, plugin: &str, marketplace: &str| UnitRow {
            idx,
            name: name.to_string(),
            kind: "skill".to_string(),
            source: format!("marketplace:{plugin}"),
            git_ref: marketplace.to_string(),
            targets: vec!["claude".to_string()],
            declared_uri: format!("marketplace:{plugin}@{marketplace}/skills/{name}"),
        };
        let mut data = SkillsScreenData {
            sources: vec![
                src(
                    "marketplace-beads-marketplace",
                    "marketplace:beads-marketplace",
                ),
                src("local-claude-skills", "local:~/.claude/skills"),
            ],
            units: vec![
                mk(1, "beads", "beads", "beads-marketplace"),
                mk(2, "task-agent", "beads", "beads-marketplace"),
                unit_in(3, "commit", "local:~/.claude/skills"),
            ],
            ..Default::default()
        };
        // The marketplace source now surfaces the plugin's two skills.
        data.source_filter = Some("marketplace:beads-marketplace".to_string());
        assert_eq!(data.visible_indices(), vec![0, 1]);
        // Local sources still match on `<type>:<locator>`.
        data.source_filter = Some("local:~/.claude/skills".to_string());
        assert_eq!(data.visible_indices(), vec![2]);
    }

    #[test]
    fn visible_indices_ands_source_and_search_filters() {
        let mut data = two_source_fixture();
        data.source_filter = Some("gh:org/acme".to_string());
        // Search "charlie" within the acme-only slice → just charlie.
        data.search = Some("charlie".to_string());
        assert_eq!(data.visible_indices(), vec![2]);
        // A search that matches a beta-only unit yields nothing because
        // the source filter excludes it first.
        data.search = Some("bravo".to_string());
        assert!(data.visible_indices().is_empty());
    }

    #[test]
    fn toggle_focus_flips_between_panes() {
        let mut data = SkillsScreenData::default();
        assert_eq!(data.focused_pane, FocusedSkillPane::Units);
        data.toggle_focus();
        assert_eq!(data.focused_pane, FocusedSkillPane::Sources);
        data.toggle_focus();
        assert_eq!(data.focused_pane, FocusedSkillPane::Units);
    }

    #[test]
    fn source_nav_wraps_and_apply_filters_units_then_focuses_units() {
        let mut data = two_source_fixture();
        data.focused_pane = FocusedSkillPane::Sources;
        // Start at 0 (acme); Prev wraps to last (beta).
        data.move_source_selection(SelectionMove::Prev);
        assert_eq!(data.source_selected, 1);
        // Next wraps back to 0.
        data.move_source_selection(SelectionMove::Next);
        assert_eq!(data.source_selected, 0);

        // Apply acme → filter pinned, focus jumps to Units, cursor snaps
        // onto the first visible (acme) row.
        data.apply_selected_source_filter();
        assert_eq!(data.source_filter.as_deref(), Some("gh:org/acme"));
        assert_eq!(data.focused_pane, FocusedSkillPane::Units);
        assert_eq!(data.selected, 0);
        assert_eq!(data.visible_indices(), vec![0, 2]);
    }

    #[test]
    fn clear_source_filter_returns_true_only_when_set() {
        let mut data = two_source_fixture();
        // Nothing to clear → false (so Esc falls through to "go home").
        assert!(!data.clear_source_filter());

        data.source_filter = Some("gh:org/beta".to_string());
        data.selected = 1;
        // Clearing returns true and re-snaps the cursor to the first row
        // of the now-wider list.
        assert!(data.clear_source_filter());
        assert!(data.source_filter.is_none());
        assert_eq!(data.selected, 0);
    }

    #[test]
    fn resize_helpers_clamp_within_bounds() {
        let term_w = 120u16;
        let mut data = SkillsScreenData::default();
        assert_eq!(data.sources_width, 32);

        // Shrinking past the floor clamps at MIN_SOURCES_WIDTH.
        for _ in 0..50 {
            data.shrink_sources(2, term_w);
        }
        assert_eq!(data.sources_width, MIN_SOURCES_WIDTH);

        // Growing past the ceiling clamps at term_w - reserve.
        for _ in 0..200 {
            data.grow_sources(2, term_w);
        }
        assert_eq!(data.sources_width, term_w - SOURCES_UNITS_RESERVE);
    }

    #[test]
    fn render_focused_units_shows_filtered_title() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let mut data = two_source_fixture();
        data.source_filter = Some("gh:org/acme".to_string());
        data.focused_pane = FocusedSkillPane::Units;

        let backend = TestBackend::new(120, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| render(f, f.size(), &data)).unwrap();
        let buf = terminal.backend().buffer().clone();
        let mut joined = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                joined.push_str(buf.get(x, y).symbol());
            }
            joined.push('\n');
        }
        // Filtered title surfaces the source's short name.
        assert!(
            joined.contains("Units (filtered: acme)"),
            "filtered title missing: {joined}"
        );
        // "All sources" affordance is always present in the Sources panel.
        assert!(
            joined.contains("All sources"),
            "All sources affordance missing: {joined}"
        );
        // Both acme units visible; the beta unit is filtered out.
        assert!(
            joined.contains("alpha"),
            "alpha (acme) should show: {joined}"
        );
        assert!(
            joined.contains("charlie"),
            "charlie (acme) should show: {joined}"
        );
        assert!(
            !joined.contains("bravo"),
            "bravo (beta) should be filtered out: {joined}"
        );
    }

    #[test]
    fn render_sources_focus_paints_gold_border_on_sources() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let mut data = two_source_fixture();
        data.focused_pane = FocusedSkillPane::Sources;

        let backend = TestBackend::new(120, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| render(f, f.size(), &data)).unwrap();
        let buf = terminal.backend().buffer().clone();

        // The Sources panel occupies columns [0, sources_width). Its top
        // border cells should carry the GOLD focus colour. Probe a cell
        // on the left vertical border (mid-height) — clear of the gold
        // " Sources " title that sits on the top border row.
        let sources_border = buf.get(0, 5);
        assert_eq!(
            sources_border.style().fg,
            Some(GOLD),
            "focused Sources panel should have a GOLD border"
        );
        // The Units panel's left vertical border sits at the divider
        // column (== sources_width); probe it clear of the title row.
        let units_border = buf.get(DEFAULT_SOURCES_WIDTH, 5);
        assert_eq!(
            units_border.style().fg,
            Some(CORNFLOWER_BLUE),
            "unfocused Units panel should keep its CORNFLOWER border"
        );
    }
}
