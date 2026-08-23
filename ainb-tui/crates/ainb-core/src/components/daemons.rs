// ABOUTME: Daemons screen component — live runtime health of the four ainb daemons.
//
// Renders fleet daemons, system services, and hook health in one table-driven
// screen. Runtime actions run asynchronously; the screen never performs I/O in
// render. Follows the ainb-tui style guide: rounded borders, gold title,
// cornflower-blue panel, green for healthy.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use ratatui::{
    prelude::*,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Cell, Paragraph, Row, Table},
};

use crate::app::state::DaemonsOverlayState;
use crate::cli::fleet::daemons::{fmt_ago, fmt_duration_ms};
use crate::fleet::daemons::heartbeat::now_ms;
use crate::fleet::daemons::probe::{DaemonKind, DaemonState, DaemonStatus};
use ainb_plugin_notifyd::{HookHealth, Paths};

// Palette shared with the rest of ainb-tui (see components/layout.rs).
const CORNFLOWER_BLUE: Color = Color::Rgb(100, 149, 237);
const GOLD: Color = Color::Rgb(255, 215, 0);
const HEALTHY_GREEN: Color = Color::Rgb(100, 200, 100);
/// Cursor colour, per the TUI style guide. Same RGB as HEALTHY_GREEN but kept
/// separate: one means "this daemon is up", the other means "R acts on this
/// row", and they must be free to diverge.
const SELECTION_GREEN: Color = Color::Rgb(100, 200, 100);
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
    /// Most-recent hook wiring health. Collected beside daemon state, never in
    /// the render path.
    pub hook_health: Option<HookHealth>,
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
    /// Cursor into the rendered rows. `R` restarts THIS row and nothing else.
    ///
    /// Stored as an index rather than a [`DaemonKind`] because the row list is
    /// whatever the collector last published: an index keeps the cursor
    /// meaningful even before the first snapshot arrives, and
    /// [`Self::selected_kind`] is the single place that turns it into a target.
    selected: usize,
}

impl DaemonsState {
    /// The daemon the cursor is on, or `None` when there are no rows.
    ///
    /// THE authority for both the highlight and the restart target. Render and
    /// key handling must both go through it — if one of them ever computed the
    /// row independently, `R` could restart a daemon other than the one the
    /// operator can see highlighted, which is the exact failure this indirection
    /// exists to make impossible.
    #[must_use]
    pub fn selected_kind(&self, rows: &[DaemonStatus]) -> Option<DaemonKind> {
        rows.get(self.selected_index(rows)).map(|d| d.kind)
    }

    /// The cursor clamped to the current row count.
    ///
    /// The collector republishes the row list, so a cursor parked past the end
    /// (rows disappeared between renders) must not silently point at nothing.
    #[must_use]
    pub fn selected_index(&self, rows: &[DaemonStatus]) -> usize {
        self.selected.min(rows.len().saturating_sub(1))
    }

    /// Whether `R` can act on a daemon kind, and why not when it cannot.
    ///
    /// Only notifyd has an in-process restart. The approve broker is served on
    /// notifyd's runtime, so restarting notifyd restarts it too. The bridge,
    /// ATC and the fleet daemon expose no restart entry point, and `R` says so
    /// rather than doing nothing — a key that silently no-ops reads as broken,
    /// and a key that falls back to "some other daemon" would be dangerous.
    #[must_use]
    pub fn restart_support(kind: DaemonKind) -> Result<DaemonKind, String> {
        match kind {
            DaemonKind::Notifyd => Ok(DaemonKind::Notifyd),
            // Same runtime as notifyd: restarting that rebinds this socket.
            DaemonKind::ApproveBroker => Ok(DaemonKind::Notifyd),
            DaemonKind::Bridge | DaemonKind::Atc | DaemonKind::FleetDaemon => Err(format!(
                "{} has no restart from the TUI — use its own CLI verb",
                kind.display_name()
            )),
        }
    }

    /// Move the cursor, saturating at both ends.
    pub fn move_selection(&mut self, delta: isize, row_count: usize) {
        if row_count == 0 {
            self.selected = 0;
            return;
        }
        let at = self.selected.min(row_count - 1);
        self.selected = at.saturating_add_signed(delta).min(row_count - 1);
    }
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
    pub fn snapshot(&mut self) -> Snapshot {
        let shared = self.shared();
        let guard = shared.lock().unwrap_or_else(|p| p.into_inner());
        Snapshot {
            rows: guard.rows.clone(),
            collected_at_ms: guard.collected_at_ms,
            hook_health: guard.hook_health.clone(),
        }
    }
}

/// Run one collect and publish it into `shared`. Shared by the background thread
/// and the test seam so the publish/merge logic is exercised without a thread.
fn collect_into(shared: &Mutex<Snapshot>) {
    // Hook health only opens local files and attempts local Unix sockets. It
    // still belongs here, not in render: hooks may live on a slow volume and a
    // stale socket can block briefly while connecting.
    let hook_health = Paths::from_home().ok().map(|paths| ainb_plugin_notifyd::hook_health(&paths));
    match crate::fleet::daemons::collect() {
        Ok(rows) => {
            let mut guard = shared.lock().unwrap_or_else(|p| p.into_inner());
            guard.rows = rows;
            guard.collected_at_ms = now_ms();
            guard.hook_health = hook_health;
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
pub fn render(
    frame: &mut Frame,
    area: Rect,
    state: &mut DaemonsState,
    runtime: Option<&DaemonsOverlayState>,
) {
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
            Span::styled(
                "  \u{2191}\u{2193}",
                Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" select \u{b7} ", Style::default().fg(MUTED_GRAY)),
            Span::styled("R", Style::default().fg(GOLD).add_modifier(Modifier::BOLD)),
            Span::styled(" restart selected", Style::default().fg(MUTED_GRAY)),
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

    // A normal 24-row terminal gets all three tables: fleet daemons, system
    // services, and hooks. The system table omits a redundant column header so
    // its five rows fit beside the Fleet table and compact Hooks section.
    if chunks[0].height >= 21 {
        let sections = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(7),
                Constraint::Length(7),
                Constraint::Length(7),
            ])
            .split(chunks[0]);
        render_table(frame, sections[0], &snapshot);
        render_system_services(frame, sections[1], runtime);
        render_hook_section(frame, sections[2], snapshot.hook_health.as_ref(), runtime);
    } else if chunks[0].height >= 18 {
        let sections = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(8), Constraint::Length(7)])
            .split(chunks[0]);
        render_table(frame, sections[0], &snapshot);
        render_hook_section(frame, sections[1], snapshot.hook_health.as_ref(), runtime);
    } else {
        render_table(frame, chunks[0], &snapshot);
    }
    render_footer(frame, chunks[1]);
}

fn render_system_services(frame: &mut Frame, area: Rect, runtime: Option<&DaemonsOverlayState>) {
    let block = Block::default()
        .title(Line::from(vec![
            Span::styled(" ◇ ", Style::default().fg(CORNFLOWER_BLUE)),
            Span::styled(
                "System services",
                Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "  r",
                Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" refresh · ", Style::default().fg(MUTED_GRAY)),
            Span::styled("M", Style::default().fg(GOLD).add_modifier(Modifier::BOLD)),
            Span::styled(" restart MCP · ", Style::default().fg(MUTED_GRAY)),
            Span::styled("P", Style::default().fg(GOLD).add_modifier(Modifier::BOLD)),
            Span::styled(" Headroom · ", Style::default().fg(MUTED_GRAY)),
            Span::styled("S", Style::default().fg(GOLD).add_modifier(Modifier::BOLD)),
            Span::styled(" start / upgrade Hangar", Style::default().fg(MUTED_GRAY)),
        ]))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(SUBDUED_BORDER))
        .style(Style::default().bg(PANEL_BG));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let rows = match runtime {
        None => vec![Row::new(["collecting…", "", "", ""])],
        Some(runtime) if runtime.loading => vec![Row::new(["collecting…", "", "", ""])],
        Some(runtime) => {
            let status = |up| if up { "● up" } else { "○ down" };
            let headroom_detail = format!(
                ":{}  {}",
                runtime.headroom.port,
                runtime
                    .headroom
                    .pid
                    .map(|pid| format!("pid {pid}"))
                    .unwrap_or_else(|| "not running".to_string())
            );
            let action_detail = |status: Option<&String>, default: &str| {
                status.cloned().unwrap_or_else(|| default.to_string())
            };
            let version = |running: Option<&str>, old: bool| match running {
                Some(version) if old => format!("{version} → {}", env!("CARGO_PKG_VERSION")),
                Some(version) if version == env!("CARGO_PKG_VERSION") => format!("{version} ✓"),
                Some(version) => format!("{version} newer"),
                None => "version unknown".to_string(),
            };
            let notify_version = notifyd_version_label(&runtime.notifyd);
            vec![
                Row::new(vec![
                    Cell::from("MCP pool"),
                    Cell::from(status(runtime.mcp_alive)),
                    Cell::from(action_detail(
                        runtime.mcp_start_status.as_ref(),
                        &version(
                            runtime.mcp_runtime.version.as_deref(),
                            runtime.mcp_runtime.old,
                        ),
                    )),
                    Cell::from("M restart current"),
                ]),
                Row::new(vec![
                    Cell::from("Headroom"),
                    Cell::from(status(runtime.headroom.running)),
                    Cell::from(action_detail(
                        runtime.headroom_start_status.as_ref(),
                        &headroom_detail,
                    )),
                    Cell::from("P start"),
                ]),
                Row::new(vec![
                    Cell::from("Hangar"),
                    Cell::from(status(runtime.hangar_running)),
                    Cell::from(action_detail(
                        runtime.hangar_start_status.as_ref(),
                        &version(
                            runtime.hangar_runtime.version.as_deref(),
                            runtime.hangar_runtime.old,
                        ),
                    )),
                    Cell::from("S start / upgrade"),
                ]),
                Row::new(vec![
                    Cell::from("notifyd"),
                    Cell::from(status(
                        runtime.notifyd.iter().any(|daemon| daemon.class.is_healthy()),
                    )),
                    Cell::from(action_detail(
                        runtime.restart_status.as_ref(),
                        &notify_version,
                    )),
                    Cell::from("R restart current"),
                ]),
                Row::new(vec![
                    Cell::from("approval broker"),
                    Cell::from(status(runtime.approve_running)),
                    Cell::from(format!("{notify_version} · {}", runtime.approve_reason)),
                    Cell::from("repaired by R"),
                ]),
            ]
        }
    };
    let widths = [
        Constraint::Length(18),
        Constraint::Length(12),
        Constraint::Min(22),
        Constraint::Length(24),
    ];
    frame.render_widget(
        Table::new(rows, widths).style(Style::default().fg(SOFT_WHITE).bg(PANEL_BG)),
        inner,
    );
}

fn notifyd_version_label(daemons: &[ainb_plugin_notifyd::ClassifiedDaemon]) -> String {
    let Some(owner) = daemons.iter().find(|daemon| daemon.class.is_healthy()) else {
        return if daemons.is_empty() {
            "not running".to_string()
        } else {
            "owner unknown".to_string()
        };
    };
    let version = owner
        .proc
        .bin
        .split_once("/Cellar/ainb/")
        .and_then(|(_, rest)| rest.split('/').next())
        .filter(|version| !version.is_empty());
    match version {
        Some(version)
            if crate::fleet::daemons::probe::release_version_is_older(
                version,
                env!("CARGO_PKG_VERSION"),
            ) =>
        {
            format!("{version} → {}", env!("CARGO_PKG_VERSION"))
        }
        Some(version) if version == env!("CARGO_PKG_VERSION") => format!("{version} ✓"),
        Some(version) => format!("{version} newer"),
        None if owner.binary_drift => "different build".to_string(),
        None => format!("{} ✓", env!("CARGO_PKG_VERSION")),
    }
}

fn render_hook_section(
    frame: &mut Frame,
    area: Rect,
    health: Option<&HookHealth>,
    runtime: Option<&DaemonsOverlayState>,
) {
    let block = Block::default()
        .title(Line::from(vec![
            Span::styled(" ◇ ", Style::default().fg(CORNFLOWER_BLUE)),
            Span::styled(
                "Hooks",
                Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
            ),
            Span::styled("  ainb-hooks", Style::default().fg(MUTED_GRAY)),
            Span::styled(
                "  I",
                Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" repair", Style::default().fg(MUTED_GRAY)),
        ]))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(SUBDUED_BORDER))
        .style(Style::default().bg(PANEL_BG));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let lines = match health {
        None => vec![Line::from(Span::styled(
            "collecting hook health…",
            Style::default().fg(MUTED_GRAY),
        ))],
        Some(health) => {
            let installed = health.installed_version.as_deref().unwrap_or("not installed");
            let version_style = if health.version_current {
                Style::default().fg(HEALTHY_GREEN)
            } else {
                Style::default().fg(GOLD)
            };
            let agent_line = health
                .agents
                .iter()
                .map(|agent| {
                    format!(
                        "{} {}",
                        agent.agent,
                        if agent.wiring_ready { "✓" } else { "✗" }
                    )
                })
                .collect::<Vec<_>>()
                .join("   ");
            let issue =
                runtime.and_then(|runtime| runtime.hooks_repair_status.as_deref()).map_or_else(
                    || {
                        health.issues.first().map_or_else(
                            || "✓ wiring healthy".to_string(),
                            |issue| {
                                format!(
                                    "! {}: {} — {}",
                                    issue.component, issue.message, issue.repair
                                )
                            },
                        )
                    },
                    |status| format!("I {status}"),
                );
            vec![
                Line::from(vec![
                    Span::styled("version ", Style::default().fg(MUTED_GRAY)),
                    Span::styled(
                        format!("{installed} → {}", health.bundled_version),
                        version_style,
                    ),
                ]),
                Line::from(Span::styled(
                    format!(
                        "script {}  ·  ainb binary {} ({}){}",
                        if health.script_ready { "✓" } else { "✗" },
                        if health.hook_binary_ready {
                            "✓"
                        } else {
                            "✗"
                        },
                        health.hook_binary_mode.map(|mode| mode.label()).unwrap_or("unknown"),
                        health
                            .hook_binary
                            .as_ref()
                            .map_or_else(String::new, |target| format!(" · {}", target.display()),),
                    ),
                    Style::default().fg(if health.script_ready && health.hook_binary_ready {
                        HEALTHY_GREEN
                    } else {
                        STOPPED_RED
                    }),
                )),
                Line::from(Span::styled(agent_line, Style::default().fg(SOFT_WHITE))),
                Line::from(Span::styled(
                    format!(
                        "notifyd {}  ·  approval broker {}",
                        if health.notify_socket_live {
                            "running"
                        } else {
                            "idle"
                        },
                        if health.approve_socket_live {
                            "running"
                        } else {
                            "idle"
                        },
                    ),
                    Style::default().fg(MUTED_GRAY),
                )),
                Line::from(Span::styled(
                    issue,
                    Style::default().fg(if health.issues.is_empty() {
                        HEALTHY_GREEN
                    } else {
                        GOLD
                    }),
                )),
            ]
        }
    };
    frame.render_widget(
        Paragraph::new(lines).style(Style::default().bg(PANEL_BG)),
        inner,
    );
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
        Cell::from("VERSION"),
        Cell::from("LAST ACTIVITY"),
        Cell::from("ERR"),
        Cell::from("HEALTH"),
    ])
    .style(Style::default().fg(MUTED_GRAY).add_modifier(Modifier::BOLD));

    // The highlight comes from the SAME call the restart target does, so the
    // marked row and the row `R` acts on cannot drift apart.
    let cursor = state.selected_index(&snapshot.rows);
    let rows: Vec<Row> = snapshot
        .rows
        .iter()
        .enumerate()
        .map(|(index, d)| {
            let (glyph, glyph_style) = match d.state {
                DaemonState::Running => ("● running", Style::default().fg(HEALTHY_GREEN)),
                // Amber, not green: the process is up but one half of its job is
                // provably not happening (bridge outbound push).
                DaemonState::Degraded => ("◐ degraded", Style::default().fg(GOLD)),
                DaemonState::Stopped => ("○ stopped", Style::default().fg(STOPPED_RED)),
                DaemonState::Unknown => ("? unknown", Style::default().fg(MUTED_GRAY)),
            };
            let pid = d.pid.map_or_else(|| "-".to_string(), |p| p.to_string());
            let uptime = d.uptime_ms.map_or_else(|| "-".to_string(), fmt_duration_ms);
            let version = daemon_version_label(d);
            let last_activity =
                d.last_activity_at.map_or_else(|| "-".to_string(), |ts| fmt_ago(now, ts));
            let health = match (&d.channel, d.connected, d.state) {
                (Some(ch), true, DaemonState::Running | DaemonState::Degraded) => {
                    format!("{ch} - {}", d.reason)
                }
                _ => d.reason.clone(),
            };
            let marker = if index == cursor { "\u{25b6} " } else { "  " };
            Row::new(vec![
                Cell::from(format!("{marker}{}", d.kind.display_name())).style(
                    if index == cursor {
                        Style::default()
                            .fg(SELECTION_GREEN)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(SOFT_WHITE).add_modifier(Modifier::BOLD)
                    },
                ),
                Cell::from(glyph).style(glyph_style),
                Cell::from(pid),
                Cell::from(uptime),
                Cell::from(version.0).style(version.1),
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
        Constraint::Length(17),
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

/// Short, operator-facing release verdict. Keep expected version in the cell:
/// a bare red old version forces humans to remember what Ainb they launched.
fn daemon_version_label(daemon: &DaemonStatus) -> (String, Style) {
    match (&daemon.version, daemon.version_current) {
        (Some(version), Some(true)) => (format!("{version} ✓"), Style::default().fg(HEALTHY_GREEN)),
        (Some(version), Some(false))
            if crate::fleet::daemons::probe::release_version_is_older(
                version,
                env!("CARGO_PKG_VERSION"),
            ) =>
        {
            (
                format!("{version} → {}", env!("CARGO_PKG_VERSION")),
                Style::default().fg(GOLD),
            )
        }
        (Some(version), Some(false)) => (
            format!("{version} newer"),
            Style::default().fg(HEALTHY_GREEN),
        ),
        _ => ("unknown".to_string(), Style::default().fg(MUTED_GRAY)),
    }
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
    use crate::headroom::ProxyStatus;
    use ainb_plugin_notifyd::{HookAgentHealth, HookBinaryMode, HookHealth, HookHealthIssue};
    use ratatui::backend::TestBackend;
    use std::path::PathBuf;

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
            version: Some(env!("CARGO_PKG_VERSION").to_string()),
            version_current: Some(true),
            connected,
            channel: channel.map(str::to_string),
            last_activity_at: Some(now_ms() - 5_000),
            error_count: 0,
            last_error: None,
            last_attention_poll_at: None,
            last_attention_error: None,
            inbound_expected: 0,
            inbound_live: 0,
            last_inbound_error: None,
            reason: if connected {
                "running + connected".to_string()
            } else {
                "no heartbeat — not running this session".to_string()
            },
        }
    }

    #[test]
    fn daemon_version_label_names_current_target_for_stale_daemon() {
        let mut daemon = status(DaemonKind::Bridge, DaemonState::Running, true, None);
        daemon.version = Some("0.0.0".to_string());
        daemon.version_current = Some(false);
        assert_eq!(
            daemon_version_label(&daemon).0,
            format!("0.0.0 → {}", env!("CARGO_PKG_VERSION"))
        );
    }

    #[test]
    fn daemon_version_label_does_not_suggest_downgrading_newer_daemon() {
        let mut daemon = status(DaemonKind::Bridge, DaemonState::Running, true, None);
        daemon.version = Some("999.0.0".to_string());
        daemon.version_current = Some(false);
        assert_eq!(daemon_version_label(&daemon).0, "999.0.0 newer");
    }

    /// A `DaemonsState` whose background collector is pre-empted: the shared
    /// snapshot is seeded with `rows` and the `shared` handle is installed, so
    /// `render`/`snapshot` read the seed and never spawn the real collector.
    /// This is the H-D2 test seam — render is decoupled from any live collect.
    fn seeded_state(rows: Vec<DaemonStatus>) -> DaemonsState {
        let shared = Arc::new(Mutex::new(Snapshot {
            rows,
            collected_at_ms: now_ms(),
            hook_health: None,
        }));
        DaemonsState {
            shared: Some(shared),
            selected: 0,
        }
    }

    fn hook_health() -> HookHealth {
        HookHealth {
            bundled_version: "0.4.5".to_string(),
            installed_version: Some("0.4.4".to_string()),
            version_current: false,
            script_path: PathBuf::from("/tmp/notify.sh"),
            script_ready: true,
            hook_binary: Some(PathBuf::from("/usr/local/bin/ainb")),
            hook_binary_mode: Some(HookBinaryMode::Release),
            hook_binary_ready: true,
            agents: vec![
                HookAgentHealth {
                    agent: "claude".to_string(),
                    installed: true,
                    wiring_ready: true,
                    detail: "marketplace install recorded".to_string(),
                },
                HookAgentHealth {
                    agent: "codex".to_string(),
                    installed: true,
                    wiring_ready: true,
                    detail: "hooks.json points at shared hook".to_string(),
                },
                HookAgentHealth {
                    agent: "copilot".to_string(),
                    installed: false,
                    wiring_ready: false,
                    detail: "not installed".to_string(),
                },
            ],
            notify_socket_live: true,
            approve_socket_live: false,
            last_event: None,
            issues: vec![HookHealthIssue {
                component: "version".to_string(),
                message: "installed 0.4.4; ainb bundles 0.4.5".to_string(),
                repair: "ainb doctor --fix-hooks".to_string(),
            }],
        }
    }

    fn seeded_state_with_hook(rows: Vec<DaemonStatus>, hook_health: HookHealth) -> DaemonsState {
        let shared = Arc::new(Mutex::new(Snapshot {
            rows,
            collected_at_ms: now_ms(),
            hook_health: Some(hook_health),
        }));
        DaemonsState {
            shared: Some(shared),
            selected: 0,
        }
    }

    /// Render the screen against an in-memory TestBackend and return the buffer
    /// as a single string for substring assertions.
    fn render_to_string(
        state: &mut DaemonsState,
        runtime: Option<&DaemonsOverlayState>,
        w: u16,
        h: u16,
    ) -> String {
        let backend = TestBackend::new(w, h);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| render(f, f.area(), state, runtime)).unwrap();
        let buf = terminal.backend().buffer().clone();
        buf.content().iter().map(|c| c.symbol()).collect::<String>()
    }

    /// Render and return the buffer as LINES, so a test can ask which row the
    /// cursor is drawn on rather than only whether a glyph exists somewhere.
    fn render_to_lines(
        state: &mut DaemonsState,
        runtime: Option<&DaemonsOverlayState>,
        w: u16,
        h: u16,
    ) -> Vec<String> {
        let backend = TestBackend::new(w, h);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| render(f, f.area(), state, runtime)).unwrap();
        let buf = terminal.backend().buffer().clone();
        (0..h)
            .map(|y| {
                (0..w)
                    .map(|x| buf.cell((x, y)).map_or(" ", |c| c.symbol()).to_string())
                    .collect::<String>()
            })
            .collect()
    }

    /// The row the cursor is DRAWN on is the row `R` restarts — always.
    ///
    /// This is the safety property, so it is asserted against the rendered
    /// buffer rather than against state: it reads back which line carries the
    /// cursor glyph and requires that line to name the same daemon that
    /// `selected_kind` hands the restart. If render and dispatch ever compute
    /// the row separately and disagree — the highlighted daemon differing from
    /// the restarted one — this fails.
    #[test]
    fn the_highlighted_row_is_the_row_restart_targets() {
        let rows = vec![
            status(DaemonKind::Bridge, DaemonState::Stopped, false, None),
            status(DaemonKind::Notifyd, DaemonState::Running, true, Some(1)),
            status(DaemonKind::ApproveBroker, DaemonState::Running, true, None),
            status(DaemonKind::Atc, DaemonState::Running, true, None),
        ];
        let mut state = seeded_state(rows.clone());

        for want in 0..rows.len() {
            // Drive the cursor the way the key handler does, from wherever it is.
            state.selected = 0;
            state.move_selection(want as isize, rows.len());

            let target = state
                .selected_kind(&rows)
                .expect("a populated table always has a selected row");

            let lines = render_to_lines(&mut state, None, 160, 30);
            let marked: Vec<&String> =
                lines.iter().filter(|l| l.contains('\u{25b6}')).collect();
            assert_eq!(
                marked.len(),
                1,
                "exactly one row may carry the cursor, got {marked:?}"
            );
            assert!(
                marked[0].contains(target.display_name()),
                "cursor is drawn on {:?} but restart would target {:?}",
                marked[0].trim(),
                target.display_name()
            );
        }
    }

    /// `R` refuses daemons it cannot restart instead of silently no-opping or
    /// falling back to a different daemon.
    #[test]
    fn restart_support_never_redirects_to_an_unrelated_daemon() {
        assert_eq!(
            DaemonsState::restart_support(DaemonKind::Notifyd),
            Ok(DaemonKind::Notifyd)
        );
        // The broker shares notifyd's runtime, so this redirect is the one
        // legitimate case — and it is the ONLY one.
        assert_eq!(
            DaemonsState::restart_support(DaemonKind::ApproveBroker),
            Ok(DaemonKind::Notifyd)
        );
        for kind in [DaemonKind::Bridge, DaemonKind::Atc, DaemonKind::FleetDaemon] {
            let err = DaemonsState::restart_support(kind)
                .expect_err("{kind:?} has no restart entry point");
            assert!(
                err.contains(kind.display_name()),
                "the refusal must name the daemon the cursor is on, got {err:?}"
            );
        }
    }

    /// The cursor saturates rather than wrapping, and survives an empty table.
    #[test]
    fn cursor_saturates_and_tolerates_an_empty_table() {
        let rows = vec![
            status(DaemonKind::Bridge, DaemonState::Stopped, false, None),
            status(DaemonKind::Notifyd, DaemonState::Running, true, Some(1)),
        ];
        let mut state = seeded_state(rows.clone());

        state.move_selection(-1, rows.len());
        assert_eq!(state.selected_index(&rows), 0, "saturates at the top");

        state.move_selection(50, rows.len());
        assert_eq!(
            state.selected_index(&rows),
            rows.len() - 1,
            "saturates at the bottom"
        );

        // A cursor parked past the end must not point at nothing when the
        // collector republishes a shorter list.
        assert_eq!(state.selected_kind(&[]), None);
        assert_eq!(state.selected_index(&[]), 0);
    }

    fn system_runtime() -> DaemonsOverlayState {
        DaemonsOverlayState {
            selected: crate::app::state::DaemonRow::ORDER[0],
            mcp_alive: true,
            mcp_runtime: crate::mcp_pool::client::DaemonRuntimeStatus::default(),
            headroom: ProxyStatus {
                running: true,
                port: 8787,
                pid: Some(42),
                tokens_saved: Some(9),
            },
            headroom_consumers: Vec::new(),
            notifyd: Vec::new(),
            approve_running: true,
            approve_reason: "serving".to_string(),
            hangar_running: true,
            hangar_reason: "running".to_string(),
            hangar_runtime: crate::cli::hangar::DaemonRuntimeStatus::default(),
            loading: false,
            last_refreshed: None,
            fetch_rx: None,
            restart_rx: None,
            restart_status: None,
            hooks_repair_rx: None,
            hooks_repair_status: None,
            hangar_start_rx: None,
            hangar_start_status: None,
            mcp_start_rx: None,
            mcp_start_status: None,
            headroom_start_rx: None,
            headroom_start_status: None,
        }
    }

    #[test]
    fn renders_title_header_and_all_daemon_rows() {
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
                DaemonKind::ApproveBroker,
                DaemonState::Running,
                true,
                Some("approve socket"),
            ),
            status(
                DaemonKind::Atc,
                DaemonState::Running,
                true,
                Some("primary (every 15m)"),
            ),
            status(DaemonKind::FleetDaemon, DaemonState::Stopped, false, None),
        ]);
        let out = render_to_string(&mut state, None, 120, 12);
        assert!(out.contains("Daemons"), "title missing: {out}");
        assert!(out.contains("DAEMON"), "header missing");
        assert!(out.contains("HEALTH"), "header missing");
        // Every daemon's display name renders as a row.
        assert!(out.contains("phone bridge"), "bridge row missing");
        assert!(out.contains("notifyd"), "notifyd row missing");
        assert!(out.contains("approve broker"), "approve broker row missing");
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
        let out = render_to_string(&mut state, None, 120, 8);
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
    fn renders_hook_version_state_and_repair_command_on_tall_screen() {
        let mut state = seeded_state_with_hook(
            vec![status(
                DaemonKind::Notifyd,
                DaemonState::Running,
                true,
                Some("socket+db"),
            )],
            hook_health(),
        );
        let runtime = system_runtime();
        let out = render_to_string(&mut state, Some(&runtime), 120, 24);
        assert!(
            out.contains("System services"),
            "system section missing: {out}"
        );
        assert!(out.contains("Hooks"), "hook section missing: {out}");
        assert!(
            out.contains("I repair"),
            "hook repair action missing: {out}"
        );
        assert!(out.contains("release"), "hook mode missing: {out}");
        assert!(
            out.contains("/usr/local/bin/ainb"),
            "hook target missing: {out}"
        );
        for service in [
            "MCP pool",
            "Headroom",
            "Hangar",
            "notifyd",
            "approval broker",
        ] {
            assert!(
                out.contains(service),
                "service row missing: {service}; {out}"
            );
        }
        assert!(
            out.contains("0.4.4 → 0.4.5"),
            "version state missing: {out}"
        );
        assert!(
            out.contains("ainb doctor --fix-hooks"),
            "repair missing: {out}"
        );
        assert!(out.contains("claude ✓"), "agent wiring missing: {out}");
    }

    #[test]
    fn renders_hook_repair_progress_from_runtime_state() {
        let mut state = seeded_state_with_hook(Vec::new(), hook_health());
        let mut runtime = system_runtime();
        runtime.hooks_repair_status = Some("hooks repaired for claude, codex".to_string());
        let out = render_to_string(&mut state, Some(&runtime), 120, 24);
        assert!(
            out.contains("I hooks repaired for claude, codex"),
            "hook repair status missing: {out}"
        );
    }

    #[test]
    fn collect_into_publishes_into_the_shared_snapshot() {
        // The background collector's publish step populates the snapshot from a
        // real collect() (every daemon) without any render involved.
        let shared = Mutex::new(Snapshot::default());
        collect_into(&shared);
        let guard = shared.lock().unwrap();
        assert_eq!(guard.rows.len(), 5, "collect publishes every daemon");
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
        let _ = render_to_string(&mut state, None, 100, 10);
        // The collector handle is now installed (spawned lazily on first render).
        assert!(
            state.shared.is_some(),
            "first render must arm the collector"
        );
    }
}
