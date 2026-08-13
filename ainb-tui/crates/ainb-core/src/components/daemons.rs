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
use crate::fleet::daemons::probe::{DaemonState, DaemonStatus};
use ainb_plugin_notifyd::{HookHealth, Paths};

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
    // its five rows fit beside the seven-row Fleet and Hooks tables.
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
        render_hook_section(frame, sections[2], snapshot.hook_health.as_ref());
    } else if chunks[0].height >= 18 {
        let sections = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(8), Constraint::Length(7)])
            .split(chunks[0]);
        render_table(frame, sections[0], &snapshot);
        render_hook_section(frame, sections[1], snapshot.hook_health.as_ref());
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
            Span::styled(" MCP · ", Style::default().fg(MUTED_GRAY)),
            Span::styled("P", Style::default().fg(GOLD).add_modifier(Modifier::BOLD)),
            Span::styled(" Headroom · ", Style::default().fg(MUTED_GRAY)),
            Span::styled("R", Style::default().fg(GOLD).add_modifier(Modifier::BOLD)),
            Span::styled(" notifyd · ", Style::default().fg(MUTED_GRAY)),
            Span::styled("S", Style::default().fg(GOLD).add_modifier(Modifier::BOLD)),
            Span::styled(" Hangar", Style::default().fg(MUTED_GRAY)),
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
            vec![
                Row::new(vec![
                    Cell::from("MCP pool"),
                    Cell::from(status(runtime.mcp_alive)),
                    Cell::from(action_detail(
                        runtime.mcp_start_status.as_ref(),
                        "shared tool servers",
                    )),
                    Cell::from("M start"),
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
                        &runtime.hangar_reason,
                    )),
                    Cell::from("S start"),
                ]),
                Row::new(vec![
                    Cell::from("notifyd"),
                    Cell::from(status(
                        runtime.notifyd.iter().any(|daemon| daemon.class.is_healthy()),
                    )),
                    Cell::from(action_detail(
                        runtime.notifyd_restart_status.as_ref(),
                        &format!("{} process(es)", runtime.notifyd.len()),
                    )),
                    Cell::from("R force restart"),
                ]),
                Row::new([
                    "approval broker",
                    status(runtime.approve_running),
                    runtime.approve_reason.as_str(),
                    "repaired by R",
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

fn render_hook_section(frame: &mut Frame, area: Rect, health: Option<&HookHealth>) {
    let block = Block::default()
        .title(Line::from(vec![
            Span::styled(" ◇ ", Style::default().fg(CORNFLOWER_BLUE)),
            Span::styled(
                "Hooks",
                Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
            ),
            Span::styled("  ainb-hooks", Style::default().fg(MUTED_GRAY)),
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
            let issue = health.issues.first().map_or_else(
                || "✓ wiring healthy".to_string(),
                |issue| {
                    format!(
                        "! {}: {} — {}",
                        issue.component, issue.message, issue.repair
                    )
                },
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
                        "script {}  ·  ainb binary {}",
                        if health.script_ready { "✓" } else { "✗" },
                        if health.hook_binary_ready {
                            "✓"
                        } else {
                            "✗"
                        },
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
                // Amber, not green: the process is up but one half of its job is
                // provably not happening (bridge outbound push).
                DaemonState::Degraded => ("◐ degraded", Style::default().fg(GOLD)),
                DaemonState::Stopped => ("○ stopped", Style::default().fg(STOPPED_RED)),
                DaemonState::Unknown => ("? unknown", Style::default().fg(MUTED_GRAY)),
            };
            let pid = d.pid.map_or_else(|| "-".to_string(), |p| p.to_string());
            let uptime = d.uptime_ms.map_or_else(|| "-".to_string(), fmt_duration_ms);
            let last_activity =
                d.last_activity_at.map_or_else(|| "-".to_string(), |ts| fmt_ago(now, ts));
            let health = match (&d.channel, d.connected, d.state) {
                (Some(ch), true, DaemonState::Running | DaemonState::Degraded) => {
                    format!("{ch} - {}", d.reason)
                }
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
    use crate::headroom::ProxyStatus;
    use ainb_plugin_notifyd::{HookAgentHealth, HookHealth, HookHealthIssue};
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
                repair: "ainb fleet runtime install".to_string(),
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

    fn system_runtime() -> DaemonsOverlayState {
        DaemonsOverlayState {
            mcp_alive: true,
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
            notifyd_restart_rx: None,
            notifyd_restart_status: None,
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
            out.contains("ainb fleet runtime install"),
            "repair missing: {out}"
        );
        assert!(out.contains("claude ✓"), "agent wiring missing: {out}");
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
