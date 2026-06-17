// ABOUTME: Daemons screen component — live runtime health of the four ainb daemons.
//
// Renders a read-only table from `fleet::daemons::collect` (the SAME aggregator
// behind `ainb fleet daemons`), refreshing on the render tick so a daemon that
// starts/stops/crashes flips live. No controls in v1 — health only, matching the
// rest of the app's read-only screens (Inbox/Stats). Follows the ainb-tui style
// guide: rounded borders, gold title, cornflower-blue panel, green for healthy.

use ratatui::{
    prelude::*,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Cell, Paragraph, Row, Table},
};

use crate::cli::fleet::daemons::{fmt_ago, fmt_duration_ms};
use crate::fleet::daemons::heartbeat::now_ms;
use crate::fleet::daemons::probe::{DaemonState, DaemonStatus};

// Palette shared with the rest of ainb-tui (see components/layout.rs).
const CORNFLOWER_BLUE: Color = Color::Rgb(100, 149, 237);
const GOLD: Color = Color::Rgb(255, 215, 0);
const HEALTHY_GREEN: Color = Color::Rgb(100, 200, 100);
const STOPPED_RED: Color = Color::Rgb(220, 100, 100);
const SOFT_WHITE: Color = Color::Rgb(220, 220, 230);
const MUTED_GRAY: Color = Color::Rgb(120, 120, 140);
const PANEL_BG: Color = Color::Rgb(30, 30, 40);
const SUBDUED_BORDER: Color = Color::Rgb(60, 60, 80);

/// Re-poll the aggregator at most every N renders to keep CPU low. The
/// aggregator only reads a handful of small files, so even 1 is cheap, but a
/// short throttle avoids re-reading on every redraw of an unchanged screen.
const POLL_TICKS: u64 = 4;

/// All state owned by the Daemons screen. Stored at app-level so the cached
/// snapshot survives cross-screen navigation. Cheap to default.
#[derive(Debug, Default)]
pub struct DaemonsState {
    /// Most-recently-collected daemon rows.
    pub rows: Vec<DaemonStatus>,
    /// The clock the cached rows' relative-time columns are measured against.
    pub collected_at_ms: i64,
    /// Render-tick counter; re-collects every `POLL_TICKS` renders.
    pub tick: u64,
}

impl DaemonsState {
    /// Re-collect the daemon health snapshot from disk. Best-effort: an error
    /// leaves the prior snapshot in place (and logs) rather than blanking the
    /// view.
    pub fn refresh(&mut self) {
        match crate::fleet::daemons::collect() {
            Ok(rows) => {
                self.rows = rows;
                self.collected_at_ms = now_ms();
            }
            Err(e) => {
                tracing::warn!(error = %e, "daemons screen: collect failed");
            }
        }
    }
}

/// Render the Daemons screen into `area`, polling the aggregator on the tick.
pub fn render(frame: &mut Frame, area: Rect, state: &mut DaemonsState) {
    state.tick = state.tick.wrapping_add(1);
    if state.tick == 1 || state.tick % POLL_TICKS == 0 {
        state.refresh();
    }

    let outer = Block::default()
        .title(Line::from(vec![
            Span::styled(" ⚙ ", Style::default().fg(CORNFLOWER_BLUE)),
            Span::styled(
                "Daemons",
                Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "  runtime health",
                Style::default().fg(MUTED_GRAY).add_modifier(Modifier::ITALIC),
            ),
        ]))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(CORNFLOWER_BLUE))
        .style(Style::default().bg(PANEL_BG));
    let inner = outer.inner(area);
    frame.render_widget(outer, area);

    // Carve a one-line help footer off the bottom.
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(inner);

    render_table(frame, chunks[0], state);
    render_footer(frame, chunks[1]);
}

fn render_table(frame: &mut Frame, area: Rect, state: &DaemonsState) {
    let now = if state.collected_at_ms > 0 {
        state.collected_at_ms
    } else {
        now_ms()
    };

    let header = Row::new(vec![
        Cell::from("DAEMON"),
        Cell::from("STATE"),
        Cell::from("PID"),
        Cell::from("UPTIME"),
        Cell::from("LAST ACTIVITY"),
        Cell::from("ERR"),
        Cell::from("HEALTH"),
    ])
    .style(Style::default().fg(MUTED_GRAY).add_modifier(Modifier::BOLD));

    let rows: Vec<Row> = state
        .rows
        .iter()
        .map(|d| {
            let (glyph, glyph_style) = match d.state {
                DaemonState::Running => ("● running", Style::default().fg(HEALTHY_GREEN)),
                DaemonState::Stopped => ("○ stopped", Style::default().fg(STOPPED_RED)),
                DaemonState::Unknown => ("? unknown", Style::default().fg(MUTED_GRAY)),
            };
            let pid = d.pid.map_or_else(|| "-".to_string(), |p| p.to_string());
            let uptime = d.uptime_ms.map_or_else(|| "-".to_string(), fmt_duration_ms);
            let last_activity =
                d.last_activity_at.map_or_else(|| "-".to_string(), |ts| fmt_ago(now, ts));
            let health = match (&d.channel, d.connected, d.state) {
                (Some(ch), true, DaemonState::Running) => format!("{ch} — {}", d.reason),
                _ => d.reason.clone(),
            };
            Row::new(vec![
                Cell::from(d.kind.display_name())
                    .style(Style::default().fg(SOFT_WHITE).add_modifier(Modifier::BOLD)),
                Cell::from(glyph).style(glyph_style),
                Cell::from(pid),
                Cell::from(uptime),
                Cell::from(last_activity),
                Cell::from(d.error_count.to_string()).style(if d.error_count > 0 {
                    Style::default().fg(STOPPED_RED)
                } else {
                    Style::default().fg(MUTED_GRAY)
                }),
                Cell::from(health).style(Style::default().fg(SOFT_WHITE)),
            ])
        })
        .collect();

    let widths = [
        Constraint::Length(14),
        Constraint::Length(10),
        Constraint::Length(8),
        Constraint::Length(8),
        Constraint::Length(14),
        Constraint::Length(4),
        Constraint::Min(20),
    ];

    let table = Table::new(rows, widths)
        .header(header)
        .column_spacing(1)
        .style(Style::default().bg(PANEL_BG));
    frame.render_widget(table, area);
}

fn render_footer(frame: &mut Frame, area: Rect) {
    let footer = Paragraph::new(Line::from(vec![
        Span::styled("read-only • live", Style::default().fg(MUTED_GRAY)),
        Span::styled("  │  ", Style::default().fg(SUBDUED_BORDER)),
        Span::styled("q/Esc", Style::default().fg(CORNFLOWER_BLUE)),
        Span::styled(" back", Style::default().fg(MUTED_GRAY)),
    ]))
    .style(Style::default().bg(PANEL_BG));
    frame.render_widget(footer, area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet::daemons::probe::DaemonKind;
    use ratatui::backend::TestBackend;

    fn status(
        kind: DaemonKind,
        state: DaemonState,
        connected: bool,
        channel: Option<&str>,
    ) -> DaemonStatus {
        DaemonStatus {
            kind,
            state,
            pid: Some(1234),
            uptime_ms: Some(3_600_000),
            connected,
            channel: channel.map(str::to_string),
            last_activity_at: Some(now_ms() - 5_000),
            error_count: 0,
            last_error: None,
            reason: if connected {
                "running + connected".to_string()
            } else {
                "no heartbeat — not running this session".to_string()
            },
        }
    }

    /// Render the screen against an in-memory TestBackend and return the buffer
    /// as a single string for substring assertions.
    fn render_to_string(state: &mut DaemonsState, w: u16, h: u16) -> String {
        let backend = TestBackend::new(w, h);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| render(f, f.area(), state)).unwrap();
        let buf = terminal.backend().buffer().clone();
        buf.content().iter().map(|c| c.symbol()).collect::<String>()
    }

    #[test]
    fn renders_title_header_and_all_four_rows() {
        // Pre-seed the cache so render doesn't depend on the host's real
        // ~/.agents-in-a-box state (refresh only fires when tick hits the
        // throttle; tick starts at 0 → first render refreshes, so seed AFTER a
        // bump by forcing the cache and a non-trigger tick).
        let mut state = DaemonsState {
            rows: vec![
                status(
                    DaemonKind::Bridge,
                    DaemonState::Running,
                    true,
                    Some("Telegram (@bot)"),
                ),
                status(DaemonKind::Notifyd, DaemonState::Stopped, false, None),
                status(
                    DaemonKind::Atc,
                    DaemonState::Running,
                    true,
                    Some("primary (every 15m)"),
                ),
                status(DaemonKind::FleetDaemon, DaemonState::Stopped, false, None),
            ],
            collected_at_ms: now_ms(),
            tick: 1, // already past the first-render refresh trigger
        };
        let out = render_to_string(&mut state, 120, 12);
        assert!(out.contains("Daemons"), "title missing: {out}");
        assert!(out.contains("DAEMON"), "header missing");
        assert!(out.contains("HEALTH"), "header missing");
        // Every daemon's display name renders as a row.
        assert!(out.contains("phone bridge"), "bridge row missing");
        assert!(out.contains("notifyd"), "notifyd row missing");
        assert!(out.contains("ATC"), "ATC row missing");
        assert!(out.contains("fleet daemon"), "fleet daemon row missing");
        // State glyphs + a connected channel render.
        assert!(out.contains("running"), "running state missing");
        assert!(out.contains("stopped"), "stopped state missing");
        assert!(out.contains("Telegram (@bot)"), "channel missing");
    }

    #[test]
    fn refresh_does_not_panic_on_empty_home() {
        // Even with no cached rows and a real collect(), rendering must not
        // panic — it surfaces whatever the host's daemons look like.
        let mut state = DaemonsState::default();
        let _ = render_to_string(&mut state, 100, 10);
        // After the first render the cache is populated (4 daemons always).
        assert_eq!(state.rows.len(), 4);
    }
}
