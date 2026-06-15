// ABOUTME: Read-only live overlay for the shared MCP pool. Rendered on top of
// the current screen (like the recovery/help overlays) when the user opens it
// from the main menu or with `p`. Shows what's served and which sessions
// share each backend process. Data is fetched lazily off-thread by
// `AppState::check_mcp_overlay`; this component only paints the snapshot.

use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Cell, Clear, Paragraph, Row, Table},
};

use crate::app::state::McpOverlayState;

const CORNFLOWER_BLUE: Color = Color::Rgb(100, 149, 237);
const GOLD: Color = Color::Rgb(255, 215, 0);
const SELECTION_GREEN: Color = Color::Rgb(100, 200, 100);
const DARK_BG: Color = Color::Rgb(25, 25, 35);
const LIST_HIGHLIGHT_BG: Color = Color::Rgb(40, 40, 60);
const SOFT_WHITE: Color = Color::Rgb(220, 220, 230);
const MUTED_GRAY: Color = Color::Rgb(120, 120, 140);
const CLAY: Color = Color::Rgb(217, 119, 87);

/// Centered popup rect sized as a percentage of the area.
fn centered(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let v = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(v[1])[1]
}

pub fn render(frame: &mut Frame, area: Rect, state: &McpOverlayState) {
    let popup = centered(82, 70, area);
    frame.render_widget(Clear, popup);

    let shared_total: usize = state.servers.iter().filter(|s| s.clients > 1).count();
    let title = format!(
        " 🧬 MCP Pool — {} server(s) · {} shared ",
        state.servers.len(),
        shared_total
    );
    let block = Block::default()
        .title(Span::styled(
            title,
            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(CORNFLOWER_BLUE))
        .style(Style::default().bg(DARK_BG));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    // Reserve a single line for the last action's result (import summary)
    // only when there is one — otherwise the table gets the space back.
    let action_h: u16 = if state.last_action.is_some() { 1 } else { 0 };
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(3),
            Constraint::Length(action_h),
            Constraint::Length(1),
        ])
        .split(inner);

    // Body: empty states vs the table.
    if !state.pool_enabled {
        render_notice(
            frame,
            chunks[0],
            "MCP pool is disabled",
            "Set `[mcp_pool] enabled = true` in config to share MCP processes across sessions.",
        );
    } else if !state.daemon_running {
        render_notice(
            frame,
            chunks[0],
            "Pool daemon not running",
            "It starts automatically with your next `ainb run` session — or run `ainb mcp daemon`.",
        );
    } else if state.servers.is_empty() {
        let msg = if state.loading {
            "Loading…"
        } else {
            "No servers pooled yet"
        };
        render_notice(
            frame,
            chunks[0],
            msg,
            "Press `i` to import servers from .mcp.json + Claude user scope — or start a session with MCP servers and they'll appear here.",
        );
    } else {
        render_table(frame, chunks[0], state);
    }

    if let Some(msg) = state.last_action.as_deref() {
        render_action_line(frame, chunks[1], msg);
    }
    render_help_bar(frame, chunks[2], state);
}

/// One-line result of the last in-overlay action (e.g. import). Green for a
/// normal outcome, clay for a failure.
fn render_action_line(frame: &mut Frame, area: Rect, msg: &str) {
    let color = if msg.contains("failed") {
        CLAY
    } else {
        SELECTION_GREEN
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!("▸ {msg}"),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        )))
        .alignment(Alignment::Center),
        area,
    );
}

fn render_notice(frame: &mut Frame, area: Rect, headline: &str, hint: &str) {
    let lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            headline,
            Style::default().fg(SOFT_WHITE).add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled(hint, Style::default().fg(MUTED_GRAY))),
    ];
    let p = Paragraph::new(lines).alignment(Alignment::Center);
    frame.render_widget(p, area);
}

fn render_table(frame: &mut Frame, area: Rect, state: &McpOverlayState) {
    let header = Row::new(vec![
        Cell::from("Server"),
        Cell::from("State"),
        Cell::from("Shared"),
        Cell::from("Sessions"),
        Cell::from("PID"),
        Cell::from("Spawns"),
        Cell::from("Uptime"),
    ])
    .style(Style::default().fg(MUTED_GRAY).add_modifier(Modifier::BOLD));

    let rows = state.servers.iter().enumerate().map(|(i, s)| {
        let selected = i == state.selected;
        let shared = s.clients > 1;
        let state_color = match s.state.as_str() {
            "running" => SELECTION_GREEN,
            "grace" | "idle" => MUTED_GRAY,
            "failed" | "disabled" => CLAY,
            _ => SOFT_WHITE,
        };
        let shared_cell = if shared {
            Span::styled(
                format!("✓ ×{}", s.clients),
                Style::default().fg(SELECTION_GREEN).add_modifier(Modifier::BOLD),
            )
        } else {
            Span::styled(format!("×{}", s.clients), Style::default().fg(MUTED_GRAY))
        };
        let sessions = if s.sessions.is_empty() {
            "—".to_string()
        } else {
            s.sessions.join(", ")
        };
        let pid = s.child_pid.map(|p| p.to_string()).unwrap_or_else(|| "—".to_string());
        let uptime = s.uptime_secs.map(fmt_uptime).unwrap_or_else(|| "—".to_string());

        let row = Row::new(vec![
            Cell::from(Span::styled(
                s.name.clone(),
                Style::default().fg(SOFT_WHITE).add_modifier(Modifier::BOLD),
            )),
            Cell::from(Span::styled(
                s.state.clone(),
                Style::default().fg(state_color),
            )),
            Cell::from(shared_cell),
            Cell::from(Span::styled(sessions, Style::default().fg(SOFT_WHITE))),
            Cell::from(Span::styled(pid, Style::default().fg(MUTED_GRAY))),
            Cell::from(Span::styled(
                s.spawn_count.to_string(),
                Style::default().fg(MUTED_GRAY),
            )),
            Cell::from(Span::styled(uptime, Style::default().fg(MUTED_GRAY))),
        ]);
        if selected {
            row.style(Style::default().bg(LIST_HIGHLIGHT_BG))
        } else {
            row
        }
    });

    let widths = [
        Constraint::Length(16),
        Constraint::Length(9),
        Constraint::Length(7),
        Constraint::Min(16),
        Constraint::Length(8),
        Constraint::Length(7),
        Constraint::Length(8),
    ];
    let table = Table::new(rows, widths).header(header).column_spacing(1);
    frame.render_widget(table, area);
}

fn fmt_uptime(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m{:02}s", secs / 60, secs % 60)
    } else {
        format!("{}h{:02}m", secs / 3600, (secs % 3600) / 60)
    }
}

fn render_help_bar(frame: &mut Frame, area: Rect, state: &McpOverlayState) {
    let refreshed = match state.last_refreshed.map(|t| t.elapsed().as_secs()) {
        Some(0) => "just now".to_string(),
        Some(n) => format!("{n}s ago"),
        None => "—".to_string(),
    };
    let mut spans = vec![
        Span::styled("↑↓", Style::default().fg(GOLD)),
        Span::styled(" select  ", Style::default().fg(MUTED_GRAY)),
        Span::styled("s", Style::default().fg(GOLD)),
        Span::styled(" stop server  ", Style::default().fg(MUTED_GRAY)),
        Span::styled("X", Style::default().fg(GOLD)),
        Span::styled(" stop pool  ", Style::default().fg(MUTED_GRAY)),
        Span::styled("i", Style::default().fg(GOLD)),
        Span::styled(" import  ", Style::default().fg(MUTED_GRAY)),
        Span::styled("I", Style::default().fg(GOLD)),
        Span::styled(" import·user  ", Style::default().fg(MUTED_GRAY)),
        Span::styled("r", Style::default().fg(GOLD)),
        Span::styled(" refresh  ", Style::default().fg(MUTED_GRAY)),
        Span::styled("esc", Style::default().fg(GOLD)),
        Span::styled(" close", Style::default().fg(MUTED_GRAY)),
    ];
    spans.push(Span::styled(
        format!("    · refreshed {refreshed}"),
        Style::default().fg(MUTED_GRAY).add_modifier(Modifier::ITALIC),
    ));
    frame.render_widget(
        Paragraph::new(Line::from(spans)).alignment(Alignment::Center),
        area,
    );
}
