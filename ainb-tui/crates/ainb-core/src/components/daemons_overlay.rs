// ABOUTME: Read-only overlay showing the status of ainb-managed background
// daemons: the shared MCP pool and the Headroom compression proxy. Opened via
// `d` / SidebarItem::Daemons. No start/stop actions — control is via CLI
// (`ainb mcp stop`, `ainb headroom stop`). Mirrors mcp_overlay.rs structure.

use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
};

use crate::app::state::DaemonsOverlayState;

const CORNFLOWER_BLUE: Color = Color::Rgb(100, 149, 237);
const GOLD: Color = Color::Rgb(255, 215, 0);
const SELECTION_GREEN: Color = Color::Rgb(100, 200, 100);
const DARK_BG: Color = Color::Rgb(25, 25, 35);
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

pub fn render(frame: &mut Frame, area: Rect, state: &DaemonsOverlayState) {
    let popup = centered(70, 55, area);
    frame.render_widget(Clear, popup);

    let block = Block::default()
        .title(Span::styled(
            " ⚙ Daemons — MCP pool · Headroom proxy · notifyd ",
            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(CORNFLOWER_BLUE))
        .style(Style::default().bg(DARK_BG));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(3),    // daemon cards
            Constraint::Length(1), // help bar
        ])
        .split(inner);

    render_cards(frame, chunks[0], state);
    render_help_bar(frame, chunks[1], state);
}

fn status_dot(running: bool) -> Span<'static> {
    if running {
        Span::styled(
            "● up",
            Style::default().fg(SELECTION_GREEN).add_modifier(Modifier::BOLD),
        )
    } else {
        Span::styled("○ down", Style::default().fg(CLAY))
    }
}

fn render_cards(frame: &mut Frame, area: Rect, state: &DaemonsOverlayState) {
    if state.loading {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "Loading…",
                Style::default().fg(MUTED_GRAY).add_modifier(Modifier::ITALIC),
            )))
            .alignment(Alignment::Center),
            area,
        );
        return;
    }

    // Rows: MCP, Headroom, Headroom consumers, then the notifyd section.
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2), // MCP row
            Constraint::Length(1), // spacer
            Constraint::Length(2), // Headroom row
            Constraint::Length(1), // spacer
            Constraint::Length(2), // Headroom consumers
            Constraint::Length(1), // spacer
            Constraint::Min(3),    // notifyd section
        ])
        .split(area);

    // ── MCP pool ──────────────────────────────────────────────────────────────
    let mcp_line = Line::from(vec![
        Span::styled(
            "  MCP pool     ",
            Style::default().fg(SOFT_WHITE).add_modifier(Modifier::BOLD),
        ),
        status_dot(state.mcp_alive),
        Span::styled(
            "    (use `ainb mcp stop` to stop)",
            Style::default().fg(MUTED_GRAY),
        ),
    ]);
    frame.render_widget(Paragraph::new(mcp_line), rows[0]);

    // ── Headroom proxy ────────────────────────────────────────────────────────
    let port = state.headroom.port;
    let pid_str = state
        .headroom
        .pid
        .map(|p| format!("pid {p}"))
        .unwrap_or_else(|| "—".to_string());
    let tokens_str = state
        .headroom
        .tokens_saved
        .map(|t| format!("saved {t} tokens"))
        .unwrap_or_else(|| "—".to_string());
    // "watched" = a live Headroom session exists, so the in-loop watchdog is
    // re-ensuring this proxy if it drops. Surfaced here, not as its own daemon.
    let watched = state.headroom.running && !state.headroom_consumers.is_empty();

    let mut headroom_spans = vec![
        Span::styled(
            format!("  Headroom :{port:<5}"),
            Style::default().fg(SOFT_WHITE).add_modifier(Modifier::BOLD),
        ),
        status_dot(state.headroom.running),
        Span::styled(
            format!("    {pid_str}   {tokens_str}"),
            Style::default().fg(MUTED_GRAY),
        ),
    ];
    if watched {
        headroom_spans.push(Span::styled(
            "  · watched",
            Style::default().fg(SELECTION_GREEN),
        ));
    }
    let headroom_line = Line::from(headroom_spans);
    frame.render_widget(Paragraph::new(headroom_line), rows[2]);

    // ── Headroom consumers ────────────────────────────────────────────────────
    let consumers_text = if state.headroom_consumers.is_empty() {
        "  Sessions using Headroom: no sessions routed".to_string()
    } else {
        format!(
            "  Sessions using Headroom ({}): {}",
            state.headroom_consumers.len(),
            state.headroom_consumers.join(", ")
        )
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            consumers_text,
            Style::default().fg(MUTED_GRAY),
        ))),
        rows[4],
    );

    // ── notifyd processes ───────────────────────────────────────────────────────
    render_notifyd_section(frame, rows[6], state);
}

/// Render every running `notifyd` process, one line each, with its
/// classification (live owner / stale / orphan) and a binary-drift flag.
/// This is the only daemon surface that enumerates *every* process rather
/// than a single pid — orphans are invisible to a pid-file-only view.
fn render_notifyd_section(frame: &mut Frame, area: Rect, state: &DaemonsOverlayState) {
    use ainb_plugin_notifyd::DaemonClass;

    let orphan_count = state.notifyd.iter().filter(|d| d.class != DaemonClass::LiveOwner).count();

    let mut lines: Vec<Line> = Vec::new();

    // Header: "notifyd processes (N)  · M need attention".
    let mut header = vec![Span::styled(
        format!("  notifyd processes ({})", state.notifyd.len()),
        Style::default().fg(SOFT_WHITE).add_modifier(Modifier::BOLD),
    )];
    if orphan_count > 0 {
        header.push(Span::styled(
            format!("   · {orphan_count} to clean up (kill <pid>)"),
            Style::default().fg(CLAY),
        ));
    }
    lines.push(Line::from(header));

    if state.notifyd.is_empty() {
        lines.push(Line::from(Span::styled(
            "    none running — a hook event will lazy-spawn one",
            Style::default().fg(MUTED_GRAY).add_modifier(Modifier::ITALIC),
        )));
    } else {
        for d in &state.notifyd {
            let (glyph, color) = match d.class {
                DaemonClass::LiveOwner => ("●", SELECTION_GREEN),
                DaemonClass::StaleOwner | DaemonClass::Orphan => ("○", CLAY),
            };
            let mut spans = vec![
                Span::styled(format!("    {glyph} "), Style::default().fg(color)),
                Span::styled(
                    format!("{:<11}", d.class.label()),
                    Style::default().fg(color).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("pid {:<7}", d.proc.pid),
                    Style::default().fg(SOFT_WHITE),
                ),
                Span::styled(
                    format!("up {:<10}", d.proc.etime),
                    Style::default().fg(MUTED_GRAY),
                ),
                Span::styled(d.proc.bin.clone(), Style::default().fg(MUTED_GRAY)),
            ];
            if d.binary_drift {
                spans.push(Span::styled(
                    "  ⚠ old binary",
                    Style::default().fg(CLAY).add_modifier(Modifier::BOLD),
                ));
            }
            lines.push(Line::from(spans));
        }
    }

    frame.render_widget(Paragraph::new(lines), area);
}

fn render_help_bar(frame: &mut Frame, area: Rect, state: &DaemonsOverlayState) {
    let refreshed = match state.last_refreshed.map(|t| t.elapsed().as_secs()) {
        Some(0) => "just now".to_string(),
        Some(n) => format!("{n}s ago"),
        None => "—".to_string(),
    };
    let spans = vec![
        Span::styled("r", Style::default().fg(GOLD)),
        Span::styled(" refresh  ", Style::default().fg(MUTED_GRAY)),
        Span::styled("esc", Style::default().fg(GOLD)),
        Span::styled(" close", Style::default().fg(MUTED_GRAY)),
        Span::styled(
            format!("    · refreshed {refreshed}"),
            Style::default().fg(MUTED_GRAY).add_modifier(Modifier::ITALIC),
        ),
    ];
    frame.render_widget(
        Paragraph::new(Line::from(spans)).alignment(Alignment::Center),
        area,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::headroom::ProxyStatus;
    use ratatui::{Terminal, backend::TestBackend};

    fn make_state(
        running: bool,
        port: u16,
        pid: Option<u32>,
        tokens: Option<u64>,
    ) -> DaemonsOverlayState {
        DaemonsOverlayState {
            mcp_alive: true,
            headroom: ProxyStatus {
                running,
                port,
                pid,
                tokens_saved: tokens,
            },
            headroom_consumers: vec!["agents/worktree-abc".to_string()],
            notifyd: Vec::new(),
            loading: false,
            last_refreshed: Some(std::time::Instant::now()),
            fetch_rx: None,
        }
    }

    fn buffer_text(term: &Terminal<TestBackend>) -> String {
        let buf = term.backend().buffer().clone();
        (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf.get(x, y).symbol().chars().next().unwrap_or(' '))
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn daemons_overlay_renders_headroom_port() {
        let state = make_state(true, 8787, Some(12345), Some(9876));
        let backend = TestBackend::new(120, 30);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| {
            let area = f.size();
            render(f, area, &state);
        })
        .unwrap();
        let text = buffer_text(&term);
        assert!(!text.is_empty(), "buffer should not be empty");
        assert!(text.contains("Headroom"), "missing 'Headroom' in:\n{text}");
        assert!(text.contains("8787"), "missing port 8787 in:\n{text}");
    }

    #[test]
    fn daemons_overlay_renders_mcp_status() {
        let state = make_state(false, 8787, None, None);
        let backend = TestBackend::new(120, 30);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| {
            let area = f.size();
            render(f, area, &state);
        })
        .unwrap();
        let text = buffer_text(&term);
        assert!(text.contains("MCP"), "missing 'MCP' in:\n{text}");
    }

    #[test]
    fn daemons_overlay_renders_loading_state() {
        let state = DaemonsOverlayState {
            mcp_alive: false,
            headroom: ProxyStatus {
                running: false,
                port: 8787,
                pid: None,
                tokens_saved: None,
            },
            headroom_consumers: Vec::new(),
            notifyd: Vec::new(),
            loading: true,
            last_refreshed: None,
            fetch_rx: None,
        };
        let backend = TestBackend::new(120, 30);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| {
            let area = f.size();
            render(f, area, &state);
        })
        .unwrap();
        let text = buffer_text(&term);
        assert!(text.contains("Loading"), "missing 'Loading' in:\n{text}");
    }

    #[test]
    fn daemons_overlay_flags_notifyd_orphans() {
        use ainb_plugin_notifyd::{ClassifiedDaemon, DaemonClass, NotifydProc};
        let mut state = make_state(true, 8787, Some(1), Some(0));
        state.notifyd = vec![
            ClassifiedDaemon {
                proc: NotifydProc {
                    pid: 10244,
                    bin: "/opt/homebrew/Cellar/ainb/1.9.4/libexec/ainb".to_string(),
                    cmd: "ainb notifyd run".to_string(),
                    etime: "01:23".to_string(),
                },
                class: DaemonClass::LiveOwner,
                binary_drift: false,
            },
            ClassifiedDaemon {
                proc: NotifydProc {
                    pid: 41530,
                    bin: "/opt/homebrew/Cellar/ainb/1.7.4/libexec/ainb".to_string(),
                    cmd: "ainb notifyd".to_string(),
                    etime: "06-04:04:24".to_string(),
                },
                class: DaemonClass::Orphan,
                binary_drift: true,
            },
        ];
        let backend = TestBackend::new(120, 30);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| {
            let area = f.size();
            render(f, area, &state);
        })
        .unwrap();
        let text = buffer_text(&term);
        assert!(
            text.contains("notifyd processes (2)"),
            "missing header in:\n{text}"
        );
        assert!(
            text.contains("LIVE owner"),
            "missing live label in:\n{text}"
        );
        assert!(text.contains("ORPHAN"), "missing orphan label in:\n{text}");
        assert!(text.contains("10244"), "missing live pid in:\n{text}");
        assert!(text.contains("41530"), "missing orphan pid in:\n{text}");
        assert!(
            text.contains("to clean up"),
            "missing cleanup hint in:\n{text}"
        );
    }

    #[test]
    fn daemons_overlay_renders_consumer_list() {
        let state = make_state(true, 8787, Some(1), Some(0));
        let backend = TestBackend::new(120, 30);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| {
            let area = f.size();
            render(f, area, &state);
        })
        .unwrap();
        let text = buffer_text(&term);
        assert!(
            text.contains("worktree-abc"),
            "missing consumer name in:\n{text}"
        );
        // A running proxy with a live consumer is watched by the in-loop watchdog.
        assert!(
            text.contains("watched"),
            "missing 'watched' marker in:\n{text}"
        );
    }
}
