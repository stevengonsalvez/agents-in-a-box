// ABOUTME: Daemons screen component — live runtime health of the four ainb daemons.
//
// Renders a read-only table from `fleet::daemons::collect` (the SAME aggregator
// behind `ainb fleet daemons`), refreshing on the render tick so a daemon that
// starts/stops/crashes flips live. No controls in v1 — health only, matching the
// rest of the app's read-only screens (Inbox/Stats). Follows the ainb-tui style
// guide: rounded borders, gold title, cornflower-blue panel, green for healthy.

use std::sync::{Arc, Mutex};
use std::time::Duration;

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

/// How often the BACKGROUND collector re-polls the aggregator. The collect only
/// reads a handful of small files, but it also performs a (bounded) socket
/// connect and `sysinfo` process lookups — work that must NEVER run on the UI
/// render thread (H-D2). A few seconds keeps the screen live while staying cheap.
const COLLECT_INTERVAL: Duration = Duration::from_secs(2);

/// The immutable snapshot the background collector publishes and `render` reads.
/// Cheap to clone the `Arc`; the `Mutex` is held only for the microseconds it
/// takes to swap or clone the row vector — never across I/O.
#[derive(Debug, Default)]
pub struct Snapshot {
    /// Most-recently-collected daemon rows.
    pub rows: Vec<DaemonStatus>,
    /// The clock the cached rows' relative-time columns are measured against.
    pub collected_at_ms: i64,
}

/// All state owned by the Daemons screen. Stored at app-level so the cached
/// snapshot survives cross-screen navigation. Cheap to default.
///
/// H-D2: `render` performs ZERO disk I/O and ZERO socket connects. A dedicated
/// background thread runs [`crate::fleet::daemons::collect`] every
/// [`COLLECT_INTERVAL`] and publishes the result into the shared [`Snapshot`];
/// `render` only ever clones the latest published snapshot under a microsecond
/// lock. A mid-crash daemon, a stale socket on a slow FS, or a saturated accept
/// backlog can stall the background thread but can NEVER freeze the UI.
#[derive(Debug, Default)]
pub struct DaemonsState {
    /// The snapshot the background collector publishes into. `None` until the
    /// first render lazily spawns the collector.
    shared: Option<Arc<Mutex<Snapshot>>>,
}

impl DaemonsState {
    /// Lazily spawn the background collector on first use and return the shared
    /// snapshot handle. Idempotent: subsequent calls reuse the running thread.
    ///
    /// The collector seeds an immediate first collect (so the screen is populated
    /// within one interval of opening), then re-collects every [`COLLECT_INTERVAL`]
    /// for the lifetime of the process. It is intentionally a detached daemon
    /// thread — the snapshot is the only shared state and it is best-effort, so
    /// there is nothing to join on teardown.
    fn shared(&mut self) -> Arc<Mutex<Snapshot>> {
        if let Some(shared) = &self.shared {
            return Arc::clone(shared);
        }
        let shared = Arc::new(Mutex::new(Snapshot::default()));
        spawn_collector(Arc::clone(&shared));
        self.shared = Some(Arc::clone(&shared));
        shared
    }

    /// Arm the background collector without rendering — called on navigation INTO
    /// the Daemons screen so collection starts (and the first snapshot lands)
    /// before the first frame, keeping the screen feeling live on entry.
    /// Idempotent: re-entering the screen reuses the already-running collector.
    pub fn arm(&mut self) {
        let _ = self.shared();
    }

    /// Read the latest published snapshot. Off the render path this is a pure
    /// memory read under a microsecond lock.
    fn snapshot(&mut self) -> Snapshot {
        let shared = self.shared();
        let guard = shared.lock().unwrap_or_else(|p| p.into_inner());
        Snapshot {
            rows: guard.rows.clone(),
            collected_at_ms: guard.collected_at_ms,
        }
    }
}

/// Run one collect and publish it into `shared`. Shared by the background thread
/// and the test seam so the publish/merge logic is exercised without a thread.
fn collect_into(shared: &Mutex<Snapshot>) {
    match crate::fleet::daemons::collect() {
        Ok(rows) => {
            let mut guard = shared.lock().unwrap_or_else(|p| p.into_inner());
            guard.rows = rows;
            guard.collected_at_ms = now_ms();
        }
        // Best-effort: an error leaves the prior snapshot in place (and logs)
        // rather than blanking the view.
        Err(e) => tracing::warn!(error = %e, "daemons screen: collect failed"),
    }
}

/// Spawn the detached background collector: one immediate collect, then a collect
/// every [`COLLECT_INTERVAL`] forever. Keeps ALL disk I/O / socket connects off
/// the UI render thread (H-D2).
fn spawn_collector(shared: Arc<Mutex<Snapshot>>) {
    std::thread::Builder::new()
        .name("ainb-daemons-collect".into())
        .spawn(move || {
            loop {
                collect_into(&shared);
                std::thread::sleep(COLLECT_INTERVAL);
            }
        })
        // A failure to spawn the collector must not crash the app: the screen
        // then simply shows an empty (never stale-wrong) table.
        .map_err(|e| tracing::warn!(error = %e, "daemons screen: collector thread spawn failed"))
        .ok();
}

/// Render the Daemons screen into `area`. Reads ONLY the cached background
/// snapshot — no disk I/O, no socket connects on the UI thread (H-D2).
pub fn render(frame: &mut Frame, area: Rect, state: &mut DaemonsState) {
    let snapshot = state.snapshot();

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

    render_table(frame, chunks[0], &snapshot);
    render_footer(frame, chunks[1]);
}

fn render_table(frame: &mut Frame, area: Rect, snapshot: &Snapshot) {
    let now = if snapshot.collected_at_ms > 0 {
        snapshot.collected_at_ms
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

    let rows: Vec<Row> = snapshot
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

    /// A `DaemonsState` whose background collector is pre-empted: the shared
    /// snapshot is seeded with `rows` and the `shared` handle is installed, so
    /// `render`/`snapshot` read the seed and never spawn the real collector.
    /// This is the H-D2 test seam — render is decoupled from any live collect.
    fn seeded_state(rows: Vec<DaemonStatus>) -> DaemonsState {
        let shared = Arc::new(Mutex::new(Snapshot {
            rows,
            collected_at_ms: now_ms(),
        }));
        DaemonsState {
            shared: Some(shared),
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
        // Seed the shared snapshot so render reads a deterministic cache and never
        // touches the host's real ~/.agents-in-a-box state (H-D2: render does no
        // collect of its own).
        let mut state = seeded_state(vec![
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
        ]);
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
    fn render_reads_only_the_cached_snapshot_no_io() {
        // H-D2: with a pre-seeded snapshot, render must reflect exactly the seed —
        // proving it reads the cache and performs no collect of its own.
        let mut state = seeded_state(vec![status(
            DaemonKind::Bridge,
            DaemonState::Running,
            true,
            Some("Telegram (@seam)"),
        )]);
        let out = render_to_string(&mut state, 120, 8);
        assert!(
            out.contains("Telegram (@seam)"),
            "seeded row missing: {out}"
        );
        // The host's real daemons (notifyd/ATC/fleet) are NOT in the seed, so
        // their display names must be absent — render didn't collect them.
        assert!(
            !out.contains("ATC"),
            "render must not collect beyond the seed"
        );
    }

    #[test]
    fn collect_into_publishes_into_the_shared_snapshot() {
        // The background collector's publish step populates the snapshot from a
        // real collect() (always 4 daemons) without any render involved.
        let shared = Mutex::new(Snapshot::default());
        collect_into(&shared);
        let guard = shared.lock().unwrap();
        assert_eq!(guard.rows.len(), 4, "collect publishes all four daemons");
        assert!(
            guard.collected_at_ms > 0,
            "publish stamps the collect clock"
        );
    }

    #[test]
    fn render_does_not_panic_on_default_state() {
        // A fresh state lazily spawns the background collector; the first render
        // sees an empty snapshot (the collector hasn't published yet) and must
        // render an empty table without panicking.
        let mut state = DaemonsState::default();
        let _ = render_to_string(&mut state, 100, 10);
        // The collector handle is now installed (spawned lazily on first render).
        assert!(
            state.shared.is_some(),
            "first render must arm the collector"
        );
    }
}
