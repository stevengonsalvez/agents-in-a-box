// ABOUTME: Skills/units manager TUI screen — spec §10.1 layout.
//
// V1 ships the layout shell + render path. Live data binding to
// the actual manifest / lockfile / usage cache happens via
// `SkillsScreenData` populated by the runtime (TODO follow-up: wire
// from ainb-cli helpers + ainb-usage). The tripwire test in
// `tests/test_skills_screen.rs` only needs the render path to be
// deterministic and to surface the spec markers (Sources / Units /
// Detail / help bar) so the cutover gate (P8) is satisfied.

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Cell, Paragraph, Row, Table},
};

// Style guide constants — match the rest of ainb-tui's components
// (cornflower borders, gold titles, soft white text, muted gray for
// helper text). These mirror crates/ainb-tui/src/components/home_screen_v2.rs.
const CORNFLOWER_BLUE: Color = Color::Rgb(100, 149, 237);
const GOLD: Color = Color::Rgb(255, 215, 0);
const SOFT_WHITE: Color = Color::Rgb(220, 220, 230);
const MUTED_GRAY: Color = Color::Rgb(120, 120, 140);

/// One source row in the left panel.
#[derive(Debug, Clone)]
pub struct SourceRow {
    pub name: String,
    pub uri: String,
    pub enabled: bool,
}

/// One unit row in the right table.
#[derive(Debug, Clone)]
pub struct UnitRow {
    pub idx: usize,
    pub name: String,
    pub kind: String,
    pub source: String,
    pub git_ref: String,
    pub targets: Vec<String>,
}

/// Detail pane content for the currently-focused unit.
#[derive(Debug, Clone, Default)]
pub struct UnitDetail {
    pub uri: String,
    pub deployed: Vec<String>,
    pub last_used: Option<String>,
    pub invocations: Option<u64>,
    pub requires: Vec<String>,
    pub upstream_status: String,
}

/// Aggregate view-model the screen renders.
///
/// Hand-populated for tests; the production runtime will assemble
/// this from `ainb_skill_core::Manifest` + `ainb_skill_core::Lockfile` +
/// `ainb_usage::UsageCache`.
#[derive(Debug, Clone, Default)]
pub struct SkillsScreenData {
    pub sources: Vec<SourceRow>,
    pub units: Vec<UnitRow>,
    pub selected: usize,
    pub detail: Option<UnitDetail>,
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

    // Top row horizontal split: 32 cols for sources, rest for units.
    let top = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(32), Constraint::Min(40)])
        .split(outer[0]);

    render_sources_panel(frame, top[0], data);
    render_units_table(frame, top[1], data);
    render_detail_pane(frame, outer[1], data);
    render_help_bar(frame, outer[2]);
}

fn render_sources_panel(frame: &mut Frame, area: Rect, data: &SkillsScreenData) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(CORNFLOWER_BLUE))
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
        for source in &data.sources {
            let glyph = if source.enabled { "✓" } else { "✗" };
            let glyph_style = if source.enabled {
                Style::default().fg(GOLD)
            } else {
                Style::default().fg(MUTED_GRAY)
            };
            let name = Span::styled(
                format!(" {:<12} ", source.name),
                Style::default().fg(SOFT_WHITE),
            );
            let uri = Span::styled(
                format!("({})", source.uri),
                Style::default().fg(MUTED_GRAY),
            );
            lines.push(Line::from(vec![Span::styled(glyph, glyph_style), name, uri]));
        }
    }
    let para = Paragraph::new(lines).block(block);
    frame.render_widget(para, area);
}

fn render_units_table(frame: &mut Frame, area: Rect, data: &SkillsScreenData) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(CORNFLOWER_BLUE))
        .title(Span::styled(
            " Units ",
            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
        ));

    let header = Row::new(vec![
        Cell::from("#"),
        Cell::from("name"),
        Cell::from("kind"),
        Cell::from("source"),
        Cell::from("ref"),
        Cell::from("targets"),
    ])
    .style(Style::default().fg(GOLD).add_modifier(Modifier::BOLD));

    let rows: Vec<Row> = data
        .units
        .iter()
        .map(|u| {
            let targets = u.targets.join(" ");
            Row::new(vec![
                Cell::from(u.idx.to_string()),
                Cell::from(u.name.clone()),
                Cell::from(u.kind.clone()),
                Cell::from(u.source.clone()),
                Cell::from(u.git_ref.clone()),
                Cell::from(targets),
            ])
        })
        .collect();

    let widths = [
        Constraint::Length(3),
        Constraint::Length(18),
        Constraint::Length(7),
        Constraint::Length(14),
        Constraint::Length(9),
        Constraint::Min(8),
    ];
    let table = Table::new(rows, widths).header(header).block(block);
    frame.render_widget(table, area);
}

fn render_detail_pane(frame: &mut Frame, area: Rect, data: &SkillsScreenData) {
    let title = match data.detail.as_ref() {
        Some(_) if !data.units.is_empty() => {
            let sel = data
                .units
                .get(data.selected.min(data.units.len().saturating_sub(1)));
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
        if let Some(last) = d.last_used.as_deref() {
            let inv = d.invocations.unwrap_or(0);
            lines.push(line_kv("Last used", &format!("{last} ({inv} invocations)")));
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
        key_span("/"),
        Span::styled("search  ", Style::default().fg(MUTED_GRAY)),
        key_span("?"),
        Span::styled("help    ", Style::default().fg(MUTED_GRAY)),
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
