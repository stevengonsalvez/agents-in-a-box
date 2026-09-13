// ABOUTME: Daemons screen component — live runtime health of the four ainb daemons.
//
// Renders fleet daemons, system services, and hook health in one table-driven
// screen. Runtime actions run asynchronously; the screen never performs I/O in
// render. Follows the ainb-tui style guide: rounded borders, gold title,
// cornflower-blue panel, green for healthy.

pub use ainb_app::components::daemons::*;

use ratatui::{
    prelude::*,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Cell, Paragraph, Row, Table},
};

use crate::cli::fleet::daemons::{fmt_ago, fmt_duration_ms};
use crate::fleet::daemons::heartbeat::now_ms;
use crate::fleet::daemons::probe::{DaemonKind, DaemonState, DaemonStatus};
use crate::fleet::read::{EvidenceCensus, EvidenceHealth};
use ainb_plugin_notifyd::HookHealth;

/// The table's floor, and the height the Hooks box asks for. `render` draws the
/// Hooks section only when the table chunk can seat both, and the ATC-help
/// budget has to respect the same number or it evicts the panel silently.
///
/// Derived, not hand-synced. These were two literals kept in step by a comment,
/// and they drifted the moment the box grew a row for the evidence census: the
/// gate moved to 15 while the budget still said 14, so at some heights the
/// panel vanished with no notice — exactly what
/// `the_hooks_panel_never_disappears_without_saying_so` exists to catch.
const TABLE_MIN: u16 = 7;
const HOOKS_BOX: u16 = 8;
const HOOKS_NEEDS: u16 = TABLE_MIN + HOOKS_BOX;

// Palette shared with the rest of ainb-tui (see components/layout.rs).
const CORNFLOWER_BLUE: Color = Color::Rgb(100, 149, 237);
const GOLD: Color = Color::Rgb(255, 215, 0);
const HEALTHY_GREEN: Color = Color::Rgb(100, 200, 100);
/// Cursor colour, per the TUI style guide. Same RGB as HEALTHY_GREEN but kept
/// separate: one means "this daemon is up", the other means "Enter acts on this
/// row", and they must be free to diverge.
const SELECTION_GREEN: Color = Color::Rgb(100, 200, 100);
const STOPPED_RED: Color = Color::Rgb(220, 100, 100);
const SOFT_WHITE: Color = Color::Rgb(220, 220, 230);
const MUTED_GRAY: Color = Color::Rgb(120, 120, 140);
const PANEL_BG: Color = Color::Rgb(30, 30, 40);
const SUBDUED_BORDER: Color = Color::Rgb(60, 60, 80);

/// Render the Daemons screen into `area`. Reads ONLY the cached background
/// snapshot — no disk I/O, no socket connects on the UI thread (H-D2).
pub fn render(frame: &mut Frame, area: Rect, state: &DaemonsState) {
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
            // The footer carries the live key hints, which change with whatever
            // overlay is open. Repeating a fixed set here just gives the title
            // a second, staler copy — `R restart selected` outlived the key.
        ]))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(CORNFLOWER_BLUE))
        .style(Style::default().bg(PANEL_BG));
    let inner = outer.inner(area);
    frame.render_widget(outer, area);

    // Carve a one-line help footer off the bottom, plus — when the cursor is on
    // the ATC row — the supervisor-mode help above it. The help is inline rather
    // than an overlay on purpose: an operator about to switch the thing that
    // drives their whole fleet should be reading what each mode does WHILE the
    // row's real state is still on screen, not instead of it.
    // WRAP FIRST, then budget. `mode_help`'s longest line is ~95 chars, so on an
    // 80-column terminal an unwrapped Paragraph clipped it to
    // "never resolves an ambiguo" — the stated limit of the mode cut off exactly
    // where it matters, on the screen whose whole purpose is to inform a switch.
    // The height must come from the WRAPPED count or the extra lines overflow
    // the chunk they were budgeted into.
    let atc_help = wrap_help(&atc_help_lines(&snapshot, state.selected), inner.width);
    let wanted = u16::try_from(atc_help.len()).unwrap_or(0);
    // The eviction order is: help first, then the Hooks box, then never the
    // table. Budgeting only against the table let the help silently delete the
    // hook-health panel on a 19-23 row terminal — it vanished when the cursor
    // landed on the ATC row and came back when it left, with nothing to say so.
    // HOOKS_SECTION_ROWS + HOOKS_MIN_TABLE is what `render` below requires to
    // draw both.
    // The eviction order is: help first, then never the Hooks box, then never
    // the table. Budgeting only against the table let the help silently delete
    // the hook-health panel on a mid-size terminal — it vanished when the cursor
    // landed on the ATC row and came back when it left, with nothing to say so.
    //
    const FOOTER: u16 = 1;
    // The first attempt at this budget fixed the silent eviction by making the
    // help almost never appear: it required `inner.height >= wanted + 15`, so on
    // a standard 80x24 terminal (inner 78x22, wanted 12 after wrapping) the
    // screen whose stated purpose is to inform a mode switch showed nothing at
    // all. Hiding the thing is not a fix for hiding the wrong thing.
    //
    // What was actually wrong in round one was that the hooks panel vanished
    // SILENTLY. So the help renders whenever the table stays usable, and when it
    // costs the operator the hooks panel it SAYS so, on a line it pays for out
    // of its own budget.
    let without_help = inner.height.saturating_sub(FOOTER);
    let hooks_fit_without_help = without_help >= HOOKS_NEEDS;
    let displaces_hooks = wanted > 0
        && hooks_fit_without_help
        && inner.height.saturating_sub(wanted + FOOTER) < HOOKS_NEEDS;
    let atc_help = if displaces_hooks {
        let mut lines = atc_help;
        // Wrapped like every other line. Pushing it AFTER `wrap_help` left it
        // the one line in the block that could be clipped mid-sentence, and it
        // is the line explaining why a panel is missing.
        lines.extend(wrap_help(
            &["(hook health hidden at this height — move off this row to see it)".to_string()],
            inner.width,
        ));
        lines
    } else {
        atc_help
    };
    let wanted = u16::try_from(atc_help.len()).unwrap_or(0);
    let with_help = inner.height.saturating_sub(wanted + FOOTER);
    let help_height = if wanted > 0 && with_help >= TABLE_MIN {
        wanted
    } else {
        // No help, or showing it would squeeze the table itself — and the table
        // is the screen. The footer still names the CLI verb that prints the
        // same text.
        0
    };
    debug_assert!(
        help_height == 0 || inner.height.saturating_sub(help_height + FOOTER) >= TABLE_MIN,
        "the help must never squeeze the table below a usable size"
    );
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),
            Constraint::Length(help_height),
            Constraint::Length(1),
        ])
        .split(inner);

    // One table, plus the Hooks box. The old System services panel is gone: it
    // listed the MCP pool, the Hangar daemon and the Headroom proxy as ad-hoc
    // lines because they had no DaemonKind, and it sat on "collecting…" when
    // its separate async fetch wedged. Those three are real rows now, so the
    // panel was a second place to look that showed strictly less.
    if chunks[0].height >= HOOKS_NEEDS {
        let sections = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(TABLE_MIN), Constraint::Length(HOOKS_BOX)])
            .split(chunks[0]);
        render_table(frame, sections[0], &snapshot, state);
        render_hook_section(
            frame,
            sections[1],
            snapshot.hook_health.as_ref(),
            snapshot.evidence_census,
            state.live_hooks_status(),
            snapshot.collected_at_ms > 0,
        );
    } else {
        render_table(frame, chunks[0], &snapshot, state);
    }
    if help_height > 0 {
        render_atc_help(frame, chunks[1], &atc_help);
    }
    render_footer(frame, chunks[2], state);
    // Overlays paint last so they float above the table.
    if state.error_open.is_some() {
        render_error_view(frame, inner, state);
    } else if state.menu.is_some() {
        render_action_menu(frame, inner, state);
    }
}

/// The per-row action menu — the one place every daemon offers the same verbs.
fn render_action_menu(frame: &mut Frame, area: Rect, state: &DaemonsState) {
    let Some(menu) = state.menu.as_ref() else {
        return;
    };
    let entries = menu.entries(state.has_error_for(menu.kind));
    // Wide enough for the longest label ("switch to full mode"), which the old
    // 30-column popup clipped.
    let width = 34_u16.min(area.width.saturating_sub(2));
    let height = u16::try_from(entries.len()).unwrap_or(3) + 3;
    let popup = centered(area, width, height.min(area.height));
    frame.render_widget(ratatui::widgets::Clear, popup);

    let block = Block::default()
        .title(Line::from(vec![
            Span::styled(" ", Style::default()),
            Span::styled(
                menu.kind.display_name(),
                Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" ", Style::default()),
        ]))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(CORNFLOWER_BLUE))
        .style(Style::default().bg(PANEL_BG));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let mut lines: Vec<Line> = Vec::with_capacity(entries.len() + 1);
    for (i, entry) in entries.iter().enumerate() {
        let label = match entry {
            // `label`, not `id`: a bare verb id cannot say which mode a
            // switch switches to.
            MenuEntry::Act(a) => a.label(),
            MenuEntry::OpenMissionControl => "open mission control",
            MenuEntry::ViewError => "view last error",
        };
        let selected = i == menu.cursor;
        lines.push(Line::from(vec![
            Span::styled(
                if selected { "▶ " } else { "  " },
                Style::default().fg(HEALTHY_GREEN),
            ),
            Span::styled(
                label,
                if selected {
                    Style::default().fg(SOFT_WHITE).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(MUTED_GRAY)
                },
            ),
        ]));
    }
    lines.push(Line::from(vec![
        Span::styled("Enter", Style::default().fg(CORNFLOWER_BLUE)),
        Span::styled(" run · ", Style::default().fg(MUTED_GRAY)),
        Span::styled("Esc", Style::default().fg(CORNFLOWER_BLUE)),
        Span::styled(" close", Style::default().fg(MUTED_GRAY)),
    ]));
    frame.render_widget(
        Paragraph::new(lines).style(Style::default().bg(PANEL_BG)),
        inner,
    );
}

/// The full text of the selected row's last failed action.
fn render_error_view(frame: &mut Frame, area: Rect, state: &DaemonsState) {
    let Some(kind) = state.error_open else {
        return;
    };
    let Some(outcome) = state.outcomes.get(kind.id()) else {
        return;
    };
    let width = area.width.saturating_sub(6).min(76);
    let height = area.height.saturating_sub(4).min(18);
    let popup = centered(area, width, height);
    frame.render_widget(ratatui::widgets::Clear, popup);

    let block = Block::default()
        .title(Line::from(vec![
            Span::styled(" ", Style::default()),
            Span::styled(
                kind.display_name(),
                Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(" · {} failed ", outcome.action.id()),
                Style::default().fg(STOPPED_RED),
            ),
        ]))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(STOPPED_RED))
        .style(Style::default().bg(PANEL_BG));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let mut lines: Vec<Line> = outcome
        .detail
        .lines()
        .map(|l| Line::from(Span::styled(l.to_string(), Style::default().fg(SOFT_WHITE))))
        .collect();
    lines.push(Line::from(Span::styled(String::new(), Style::default())));
    lines.push(Line::from(vec![
        Span::styled("Esc", Style::default().fg(CORNFLOWER_BLUE)),
        Span::styled(" close", Style::default().fg(MUTED_GRAY)),
    ]));
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(ratatui::widgets::Wrap { trim: false })
            .style(Style::default().bg(PANEL_BG)),
        inner,
    );
}

/// A `width` × `height` rect centred in `area`, clamped to fit.
fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    }
}

fn render_hook_section(
    frame: &mut Frame,
    area: Rect,
    health: Option<&HookHealth>,
    evidence: Option<EvidenceCensus>,
    status: Option<&str>,
    collected: bool,
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
            Span::styled(" install / repair", Style::default().fg(MUTED_GRAY)),
            Span::styled(
                "  B",
                Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" pin running", Style::default().fg(MUTED_GRAY)),
        ]))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(SUBDUED_BORDER))
        .style(Style::default().bg(PANEL_BG));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let lines = match health {
        // Once a collect HAS run, "collecting…" is a lie — the collector looked
        // and found nothing readable. Saying so is what lets the operator act
        // on it instead of waiting for a placeholder that never resolves.
        None if collected => vec![Line::from(Span::styled(
            "hook health unavailable — run `ainb doctor --fix-hooks`",
            Style::default().fg(STOPPED_RED),
        ))],
        None => vec![Line::from(Span::styled(
            "reading hook health…",
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
            let issue = status.map_or_else(
                || {
                    health.issues.first().map_or_else(
                        || "✓ wiring healthy".to_string(),
                        |issue| {
                            format!("! {}: {}: {}", issue.component, issue.message, issue.repair)
                        },
                    )
                },
                |status| format!("I {status}"),
            );
            // The pointer and the binary you are looking at are separate facts,
            // and the failure being diagnosed here is precisely them being
            // different. One combined line hid that; two lines cannot.
            let pointer = health
                .hook_binary
                .as_ref()
                .map_or_else(|| "(no pointer)".to_string(), |p| p.display().to_string());
            let running = health
                .running_binary
                .as_ref()
                .map_or_else(|| "(unknown)".to_string(), |p| p.display().to_string());
            // Decided by the probe with `canonical_eq`: current_exe resolves
            // symlinks and the pointer does not, so comparing the paths here
            // would gold-flag every healthy Homebrew install.
            let pointer_matches_running = health.hook_binary_is_running_binary;
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
                        "script {}  ·  hooks {} ({}) → {pointer}",
                        if health.script_ready { "✓" } else { "✗" },
                        if health.hook_binary_ready {
                            "✓"
                        } else {
                            "✗"
                        },
                        health.hook_binary_mode.map(|mode| mode.label()).unwrap_or("unknown"),
                    ),
                    Style::default().fg(if health.script_ready && health.hook_binary_ready {
                        HEALTHY_GREEN
                    } else {
                        STOPPED_RED
                    }),
                )),
                Line::from(Span::styled(
                    format!("running → {running}"),
                    Style::default().fg(if pointer_matches_running {
                        MUTED_GRAY
                    } else {
                        GOLD
                    }),
                )),
                evidence_line(evidence),
                Line::from(Span::styled(agent_line, Style::default().fg(SOFT_WHITE))),
                // notifyd and the approve broker had a line here when they had
                // no row of their own. They are first-class rows in the table
                // above now, so this only restated it, and the line it costs is
                // what made the whole panel vanish on a short terminal.
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

fn evidence_line(evidence: Option<EvidenceCensus>) -> Line<'static> {
    let Some(evidence) = evidence else {
        return Line::from(Span::styled(
            "Claude hook evidence unavailable",
            Style::default().fg(STOPPED_RED),
        ));
    };
    let style = match evidence.health {
        EvidenceHealth::Healthy => Style::default().fg(HEALTHY_GREEN),
        EvidenceHealth::Silent => Style::default().fg(GOLD),
        EvidenceHealth::Unavailable => Style::default().fg(STOPPED_RED),
    };
    Line::from(Span::styled(
        format!(
            "Claude hook {}  ·  {} active probe(s), {} fresh state",
            evidence.health.label(),
            evidence.active_probes,
            evidence.fresh_hook_states
        ),
        style,
    ))
}

fn render_table(frame: &mut Frame, area: Rect, snapshot: &Snapshot, state: &DaemonsState) {
    let now = if snapshot.collected_at_ms > 0 {
        snapshot.collected_at_ms
    } else {
        now_ms()
    };

    let header = Row::new(vec![
        // The cursor gets its own column. Prefixing it into the DAEMON cell ate
        // two characters of every name, so long ones truncated.
        Cell::from(""),
        Cell::from("DAEMON"),
        Cell::from("TYPE"),
        Cell::from("STATE"),
        Cell::from("PID"),
        Cell::from("UPTIME"),
        Cell::from("VERSION"),
        Cell::from("LAST ACTIVITY"),
        Cell::from("ERR"),
        Cell::from("HEALTH"),
    ])
    .style(Style::default().fg(MUTED_GRAY).add_modifier(Modifier::BOLD));

    let rows: Vec<Row> = snapshot
        .rows
        .iter()
        .enumerate()
        .map(|(i, d)| {
            let selected = i == state.selected;
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
            // A running or finished action owns the STATE cell until the next
            // collect: it is the most recent truth about this row, and a
            // failure has to be attached to the daemon it happened to.
            let (glyph, glyph_style) = match (
                state.inflight.contains_key(d.kind.id()),
                state.outcomes.get(d.kind.id()),
            ) {
                (true, _) => ("⟳ working", Style::default().fg(GOLD)),
                (false, Some(o)) if !o.ok => (
                    "✗ failed",
                    Style::default().fg(STOPPED_RED).add_modifier(Modifier::BOLD),
                ),
                _ => (glyph, glyph_style),
            };
            // A failed action replaces HEALTH with its own summary plus the way
            // to read the rest. A stale probe reason under a red badge reads as
            // if nothing happened.
            let health = match state.outcomes.get(d.kind.id()) {
                Some(o) if !o.ok => format!("{}  ·  Enter → error", o.summary),
                Some(o) => o.summary.clone(),
                None => health,
            };
            Row::new(vec![
                Cell::from(if selected { "▶" } else { "" })
                    .style(Style::default().fg(SELECTION_GREEN)),
                Cell::from(d.kind.display_name()).style(if selected {
                    Style::default().fg(HEALTHY_GREEN).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(SOFT_WHITE).add_modifier(Modifier::BOLD)
                }),
                Cell::from(d.kind.runtime_type()).style(Style::default().fg(MUTED_GRAY)),
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

    // The cursor lives in its OWN gutter column rather than being prefixed onto
    // the name: "approve broker" is exactly the 14 columns DAEMON allows, so a
    // 2-char marker inside that cell truncated the daemon's name.
    let widths = [
        Constraint::Length(1),
        Constraint::Length(14),
        Constraint::Length(7),
        Constraint::Length(9),
        Constraint::Length(7),
        Constraint::Length(7),
        Constraint::Length(13),
        Constraint::Length(13),
        Constraint::Length(3),
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

/// The supervisor-mode help for the ATC row, or empty when the cursor is
/// elsewhere / the mode is unknown.
///
/// The lines come from [`crate::fleet::atc::mode_help`] — the same text
/// `ainb fleet atc mode` prints — so the screen and the CLI can never describe
/// the modes differently.
fn atc_help_lines(snapshot: &Snapshot, selected: usize) -> Vec<String> {
    if snapshot.rows.get(selected).map(|r| r.kind) != Some(DaemonKind::Atc) {
        return Vec::new();
    }
    let Some(atc) = snapshot.atc.as_ref() else {
        return Vec::new();
    };
    atc.help.clone()
}

/// Wrap help lines to `width`, preserving each line's leading indent.
///
/// Done here rather than with `Paragraph::wrap` because the caller has to budget
/// the block's height, and only the WRAPPED count is the real height.
///
/// The indent is load-bearing: `mode_help` indents its "limits:" lines so they
/// read as belonging to the mode above them. An earlier version seeded the first
/// output line from the first WORD, which silently dropped that indent and
/// detached every limits line from its mode.
///
/// ponytail: a single word longer than `width` is emitted over-long rather than
/// hard-split. Nothing in `mode_help` comes close, and an over-long line is
/// clipped horizontally without changing the line COUNT, so the height budget
/// stays correct. Hard-split if that ever stops being true.
fn wrap_help(lines: &[String], width: u16) -> Vec<String> {
    let width = usize::from(width).max(20);
    let mut out = Vec::new();
    for line in lines {
        if line.chars().count() <= width {
            out.push(line.clone());
            continue;
        }
        let lead: String = line.chars().take_while(|c| c.is_whitespace()).collect();
        // Continuations sit two columns inside the original indent, so a wrapped
        // line is visibly a continuation and not a new bullet.
        let hang = format!("{lead}  ");
        let mut current = lead.clone();
        let mut has_word = false;
        for word in line.split_whitespace() {
            let prospective = if has_word {
                current.chars().count() + 1 + word.chars().count()
            } else {
                current.chars().count() + word.chars().count()
            };
            if prospective > width && has_word {
                out.push(std::mem::take(&mut current));
                current.push_str(&hang);
                has_word = false;
            }
            if has_word {
                current.push(' ');
            }
            current.push_str(word);
            has_word = true;
        }
        if has_word {
            out.push(current);
        }
    }
    out
}

/// Paint the mode help. The first line (the current owner) is emphasised: it is
/// the fact the rest of the block is context for.
fn render_atc_help(frame: &mut Frame, area: Rect, lines: &[String]) {
    let painted: Vec<Line> = lines
        .iter()
        .enumerate()
        .map(|(i, text)| {
            let style = if i == 0 {
                Style::default().fg(GOLD).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(MUTED_GRAY)
            };
            Line::from(Span::styled(text.clone(), style))
        })
        .collect();
    frame.render_widget(
        Paragraph::new(painted).style(Style::default().bg(PANEL_BG)),
        area,
    );
}

fn render_footer(frame: &mut Frame, area: Rect, state: &DaemonsState) {
    // Hints name the keys that work RIGHT NOW: an overlay owns Enter and Esc,
    // so advertising the table's keys underneath it would be a lie.
    let spans = if state.error_open.is_some() {
        vec![
            Span::styled("Esc", Style::default().fg(CORNFLOWER_BLUE)),
            Span::styled(" close error", Style::default().fg(MUTED_GRAY)),
        ]
    } else if state.menu.is_some() {
        vec![
            Span::styled("↑/↓", Style::default().fg(CORNFLOWER_BLUE)),
            Span::styled(" choose  ", Style::default().fg(MUTED_GRAY)),
            Span::styled("Enter", Style::default().fg(CORNFLOWER_BLUE)),
            Span::styled(" run  ", Style::default().fg(MUTED_GRAY)),
            Span::styled("Esc", Style::default().fg(CORNFLOWER_BLUE)),
            Span::styled(" close", Style::default().fg(MUTED_GRAY)),
        ]
    } else {
        vec![
            Span::styled("↑/↓", Style::default().fg(CORNFLOWER_BLUE)),
            Span::styled(" select  ", Style::default().fg(MUTED_GRAY)),
            Span::styled("Enter", Style::default().fg(CORNFLOWER_BLUE)),
            // Not "start / restart / stop": the entries a row offers depend on
            // its state, so naming three of them in a fixed list goes stale the
            // moment a row offers a fourth — which the mode switch is.
            Span::styled(" actions  ", Style::default().fg(MUTED_GRAY)),
            Span::styled("r", Style::default().fg(CORNFLOWER_BLUE)),
            Span::styled(" refresh", Style::default().fg(MUTED_GRAY)),
            Span::styled("  │  ", Style::default().fg(SUBDUED_BORDER)),
            Span::styled("q/Esc", Style::default().fg(CORNFLOWER_BLUE)),
            Span::styled(" back", Style::default().fg(MUTED_GRAY)),
        ]
    };
    let footer = Paragraph::new(Line::from(spans)).style(Style::default().bg(PANEL_BG));
    frame.render_widget(footer, area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::daemon::Action;
    use crate::fleet::daemons::probe::DaemonKind;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

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
            scheduler_orphan: None,
            // A real ATC row names its instance whenever one is provisioned,
            // and the menu reads that rather than the reason text. A fixture
            // that left it None would be an UNPROVISIONED row.
            atc_instance: (kind == DaemonKind::Atc)
                .then(|| channel.map_or_else(|| "main".to_string(), ToString::to_string)),
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

    /// The collector used to run "for the lifetime of the process": one visit
    /// to this screen armed a thread that re-probed every daemon socket every
    /// two seconds until the TUI exited. A real session logged ~25 connects a
    /// minute to the hangar daemon for three hours off a single navigation.
    #[test]
    fn a_screen_nobody_is_watching_stops_polling() {
        let armed = 10_000;
        assert!(
            !collector_should_park(armed, armed + COLLECT_IDLE_STOP_MS),
            "a screen still on view must keep collecting"
        );
        assert!(
            collector_should_park(armed, armed + COLLECT_IDLE_STOP_MS + 1),
            "past the idle window the collector must park"
        );
        assert!(
            collector_should_park(armed, armed + 3 * 60 * 60 * 1_000),
            "three hours after the last look, nothing should still be probing"
        );
    }

    /// Parking is only useful if coming back works, and only SAFE if coming
    /// back twice does not leave two collectors racing the same snapshot.
    #[test]
    fn a_parked_collector_is_revived_exactly_once() {
        let shared = Mutex::new(Snapshot {
            collector_parked: true,
            ..Snapshot::default()
        });
        assert!(
            DaemonsState::touch(&shared, 1_000),
            "re-entering the screen must revive a parked collector"
        );
        assert!(
            !DaemonsState::touch(&shared, 2_000),
            "a live collector must not be spawned a second time"
        );
        let guard = shared.lock().unwrap();
        assert_eq!(
            guard.last_touch_ms, 2_000,
            "every touch refreshes the clock"
        );
        assert!(!guard.collector_parked);
    }

    /// A `DaemonsState` whose background collector is pre-empted: the shared
    /// snapshot is seeded with `rows` and the `shared` handle is installed, so
    /// `render`/`snapshot` read the seed and never spawn the real collector.
    /// This is the H-D2 test seam — render is decoupled from any live collect.
    fn seeded_state(rows: Vec<DaemonStatus>) -> DaemonsState {
        let shared = Arc::new(Mutex::new(Snapshot {
            atc: None,
            rows,
            collected_at_ms: now_ms(),
            hook_health: None,
            evidence_census: None,
            evidence_collected_at_ms: 0,
            last_touch_ms: now_ms(),
            collector_parked: false,
        }));
        DaemonsState {
            shared: Some(shared),
            ..DaemonsState::default()
        }
    }

    fn atc_view(provider: &str) -> AtcModeView {
        AtcModeView {
            name: "tower".to_string(),
            provider: provider.to_string(),
            help: crate::fleet::atc::mode_help(provider),
        }
    }

    /// A seeded state whose ATC supervisor mode is known — the shape the mode
    /// toggle and the inline help both read.
    fn seeded_state_with_atc(rows: Vec<DaemonStatus>) -> DaemonsState {
        let shared = Arc::new(Mutex::new(Snapshot {
            atc: Some(atc_view("claude")),
            rows,
            collected_at_ms: now_ms(),
            hook_health: None,
            evidence_census: None,
            evidence_collected_at_ms: 0,
            last_touch_ms: now_ms(),
            collector_parked: false,
        }));
        DaemonsState {
            shared: Some(shared),
            ..DaemonsState::default()
        }
    }

    /// A seeded state carrying BOTH the ATC mode and hook health, for the
    /// layout-budget tests.
    fn seeded_state_with_atc_and_hooks(rows: Vec<DaemonStatus>) -> DaemonsState {
        let shared = Arc::new(Mutex::new(Snapshot {
            atc: Some(atc_view("claude")),
            rows,
            collected_at_ms: now_ms(),
            hook_health: Some(hook_health()),
            evidence_census: None,
            evidence_collected_at_ms: 0,
            last_touch_ms: now_ms(),
            collector_parked: false,
        }));
        DaemonsState {
            shared: Some(shared),
            ..DaemonsState::default()
        }
    }

    fn atc_row() -> DaemonStatus {
        status(DaemonKind::Atc, DaemonState::Running, true, Some("tower"))
    }

    // ── Supervisor mode toggle ──────────────────────────────────────────────

    #[test]
    fn the_open_menu_does_not_reshuffle_when_the_collector_republishes() {
        // The entry list is captured at open. If it re-read the mode per frame,
        // a collect landing mid-keystroke would move the cursor's meaning — you
        // press "switch to lite" and get "stop".
        let mut state = seeded_state_with_atc(vec![atc_row()]);
        state.open_menu();
        let before = state.menu.as_ref().unwrap().entries(false);

        // The collector publishes a switched mode underneath the open menu.
        {
            let shared = state.shared();
            let mut guard = shared.lock().unwrap();
            guard.atc = Some(atc_view("claude"));
        }
        let after = state.menu.as_ref().unwrap().entries(false);
        assert_eq!(before, after, "the open menu must not reshuffle");
    }

    // ── Inline ATC help ─────────────────────────────────────────────────────

    /// The help answers the question an operator opens this row with: what does
    /// this instance do, what does it cost, and do I need one at all.
    #[test]
    fn selecting_the_atc_row_says_what_the_instance_does_and_what_it_costs() {
        let mut state = seeded_state_with_atc(vec![atc_row()]);
        let text = render_to_string(&mut state, 120, 30);
        assert!(text.contains("brain: claude"), "the provider: {text}");
        assert!(
            text.contains("coordinates the fleet"),
            "what it does: {text}"
        );
        assert!(text.contains("spends tokens"), "what it costs: {text}");
        // The reason an operator might want NO instance: transient-error
        // auto-continue is the daemon's now and needs neither one nor an LLM.
        assert!(
            text.contains("retry sweep"),
            "what no longer needs an instance: {text}"
        );
    }

    #[test]
    fn the_help_comes_from_the_same_text_the_cli_prints() {
        // One source for both surfaces: a screen and a CLI that describe the
        // modes differently is how an operator switches the wrong way.
        let snapshot = Snapshot {
            atc: Some(atc_view("codex")),
            rows: vec![atc_row()],
            collected_at_ms: now_ms(),
            hook_health: None,
            evidence_census: None,
            evidence_collected_at_ms: 0,
            last_touch_ms: now_ms(),
            collector_parked: false,
        };
        assert_eq!(
            atc_help_lines(&snapshot, 0),
            crate::fleet::atc::mode_help("codex")
        );
    }

    #[test]
    fn the_help_is_absent_on_every_other_row() {
        let snapshot = Snapshot {
            atc: Some(atc_view("claude")),
            rows: vec![
                status(DaemonKind::Bridge, DaemonState::Running, true, None),
                atc_row(),
            ],
            collected_at_ms: now_ms(),
            hook_health: None,
            evidence_census: None,
            evidence_collected_at_ms: 0,
            last_touch_ms: now_ms(),
            collector_parked: false,
        };
        assert!(atc_help_lines(&snapshot, 0).is_empty(), "bridge row");
        assert!(!atc_help_lines(&snapshot, 1).is_empty(), "atc row");
    }

    /// The ATC-only verbs stay ATC-only. `provision` and `remove-orphan` are
    /// meaningless on a daemon that has no instance to provision, and offering
    /// one there is a menu entry that could only ever fail.
    #[test]
    fn the_atc_only_verbs_are_offered_on_no_other_daemon() {
        let bridge = status(DaemonKind::Bridge, DaemonState::Running, true, None);
        let mut state = seeded_state_with_atc(vec![atc_row(), bridge]);

        // A PROVISIONED row offers neither: `provision` refuses when one already
        // exists and `remove-orphan` when there is no orphan. That is `offers`
        // doing its job, and it is why the property below is about the bridge.
        state.open_menu();
        let on_atc = state.menu.as_ref().unwrap().entries(false);
        assert!(
            on_atc.contains(&MenuEntry::Act(Action::Start)),
            "the ATC row must still offer its lifecycle verbs: {on_atc:?}"
        );

        state.close_all_overlays();
        state.move_selection(1);
        state.open_menu();
        let on_bridge = state.menu.as_ref().unwrap().entries(false);
        for verb in [Action::Provision, Action::RemoveOrphan] {
            assert!(
                !on_bridge.contains(&MenuEntry::Act(verb)),
                "the bridge row must not offer {verb:?}: {on_bridge:?}"
            );
        }
    }

    #[test]
    fn the_help_wraps_instead_of_clipping_the_limits_it_exists_to_state() {
        // On 80 columns the longest help line was cut at "…ambiguo", losing the
        // limit on the screen whose purpose is to inform a decision.
        let lines = crate::fleet::atc::mode_help("claude");
        let wrapped = wrap_help(&lines, 78);
        assert!(
            wrapped.iter().all(|l| l.chars().count() <= 78),
            "a wrapped line still overflows: {wrapped:?}"
        );
        // Wrapping inserts a continuation indent, so compare on collapsed
        // whitespace — the phrase surviving across a line break is the point.
        let joined = wrapped.join(" ").split_whitespace().collect::<Vec<_>>().join(" ");
        for phrase in [
            "coordinates the fleet",
            "spends tokens",
            // The line an operator deciding whether they need an instance at
            // all has to see: transient-error auto-continue no longer needs one.
            "retry sweep, with no instance and no LLM",
        ] {
            assert!(
                joined.contains(phrase),
                "wrapping lost {phrase:?}: {joined}"
            );
        }
        assert!(
            wrapped.len() >= lines.len(),
            "wrapping cannot shrink the block"
        );
    }

    #[test]
    fn the_help_renders_on_a_standard_eighty_by_twentyfour_terminal() {
        // The regression an over-cautious budget introduced: suppressing the
        // help whenever it would cost the hooks panel meant it needed a 29-row
        // terminal, so the screen whose purpose is to inform a decision showed
        // nothing at the commonest size. Hiding the thing is not a fix for
        // hiding the wrong thing.
        let mut state = seeded_state_with_atc_and_hooks(vec![atc_row()]);
        let text = render_to_string(&mut state, 80, 24);
        assert!(
            text.contains("spends tokens"),
            "the ATC help must render at 80x24: {text}"
        );
    }

    #[test]
    fn the_help_never_squeezes_the_table_itself() {
        // The one thing that outranks both help and hooks: the rows ARE the
        // screen.
        for height in 8..40_u16 {
            let mut state = seeded_state_with_atc_and_hooks(vec![atc_row()]);
            let text = render_to_string(&mut state, 100, height);
            assert!(
                text.contains("ATC"),
                "height {height}: the daemon row was squeezed out"
            );
        }
    }

    #[test]
    fn the_hooks_panel_never_disappears_without_saying_so() {
        // A sweep rather than one band: the budget is a size calculation, so the
        // property is the invariant, not any single terminal size.
        for height in 8..40_u16 {
            let mut bare = seeded_state_with_atc_and_hooks(vec![atc_row()]);
            // Selection defaults to the ATC row (index 0) in both, so the only
            // difference is whether the help is eligible at this height.
            let with_atc_selected = render_to_string(&mut bare, 100, height);

            let mut other = seeded_state_with_atc_and_hooks(vec![
                atc_row(),
                status(DaemonKind::Bridge, DaemonState::Running, true, None),
            ]);
            other.move_selection(1); // off the ATC row: no help
            let without_help = render_to_string(&mut other, 100, height);

            // The panel may yield to the help. What it may never do is vanish
            // in silence, which was the actual complaint.
            if without_help.contains("Hooks") && !with_atc_selected.contains("Hooks") {
                assert!(
                    with_atc_selected.contains("hook health hidden"),
                    "height {height}: the hooks panel vanished with nothing to say so"
                );
            }
        }
    }

    #[test]
    fn a_short_terminal_drops_the_help_rather_than_the_table() {
        // The rows are the point of the screen; the help is context. On a
        // terminal too short for both, the help goes.
        let mut state = seeded_state_with_atc(vec![atc_row()]);
        let text = render_to_string(&mut state, 100, 10);
        assert!(text.contains("ATC"), "the row must survive: {text}");
        assert!(
            !text.contains("never answers an ASK"),
            "the help must not squeeze the table: {text}"
        );
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
            running_binary: Some(PathBuf::from("/usr/local/bin/ainb")),
            hook_binary_is_running_binary: true,
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
            atc: None,
            rows,
            collected_at_ms: now_ms(),
            hook_health: Some(hook_health),
            evidence_census: Some(EvidenceCensus::from_counts(true, 1, 1)),
            evidence_collected_at_ms: 0,
            last_touch_ms: now_ms(),
            collector_parked: false,
        }));
        DaemonsState {
            shared: Some(shared),
            ..DaemonsState::default()
        }
    }

    /// Render the screen against an in-memory TestBackend and return the buffer
    /// as a single string for substring assertions.
    /// One host frame: the pre-draw tick, then the paint. Split apart in the
    /// seal, so a helper that only painted would test half a frame.
    fn render_to_string(state: &mut DaemonsState, w: u16, h: u16) -> String {
        let backend = TestBackend::new(w, h);
        let mut terminal = Terminal::new(backend).unwrap();
        state.tick();
        terminal.draw(|f| render(f, f.area(), state)).unwrap();
        let buf = terminal.backend().buffer().clone();
        buf.content().iter().map(|c| c.symbol()).collect::<String>()
    }

    /// Render and return the buffer as LINES, so a test can ask which row the
    /// cursor is drawn on rather than only whether a glyph exists somewhere.
    fn render_to_lines(state: &mut DaemonsState, w: u16, h: u16) -> Vec<String> {
        let backend = TestBackend::new(w, h);
        let mut terminal = Terminal::new(backend).unwrap();
        state.tick();
        terminal.draw(|f| render(f, f.area(), state)).unwrap();
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
    /// `selected_kind` hands the action menu. If render and dispatch ever
    /// compute the row separately and disagree — the highlighted daemon
    /// differing from the one acted on — this fails.
    #[test]
    fn the_highlighted_row_is_the_row_the_menu_acts_on() {
        let rows = vec![
            status(DaemonKind::Bridge, DaemonState::Stopped, false, None),
            status(
                DaemonKind::Notifyd,
                DaemonState::Running,
                true,
                Some("unix socket"),
            ),
            status(DaemonKind::ApproveBroker, DaemonState::Running, true, None),
            status(DaemonKind::Atc, DaemonState::Running, true, None),
        ];
        let mut state = seeded_state(rows.clone());

        for want in 0..rows.len() {
            // Drive the cursor the way the key handler does, from wherever it is.
            state.selected = 0;
            state.move_selection(want as isize);

            let target = state
                .selected_status()
                .expect("a populated table always has a selected row")
                .kind;

            let lines = render_to_lines(&mut state, 160, 30);
            let marked: Vec<&String> = lines.iter().filter(|l| l.contains('\u{25b6}')).collect();
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
        let out = render_to_string(&mut state, 120, 12);
        assert!(out.contains("Daemons"), "title missing: {out}");
        assert!(out.contains("DAEMON"), "header missing");
        assert!(out.contains("TYPE"), "header missing");
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
    fn shows_process_and_derived_rows_without_conflating_a_missing_pid() {
        let mut atc = status(DaemonKind::Atc, DaemonState::Running, true, Some("tower"));
        atc.pid = None;
        let mut hangar = status(DaemonKind::HangarDaemon, DaemonState::Stopped, false, None);
        hangar.pid = None;
        let mut state = seeded_state(vec![atc, hangar]);

        let lines = render_to_lines(&mut state, 160, 12);
        let atc_line = lines.iter().find(|line| line.contains("ATC")).expect("ATC row");
        let hangar_line =
            lines.iter().find(|line| line.contains("hangar daemon")).expect("hangar row");

        assert!(
            atc_line.contains("derived"),
            "ATC must say derived: {atc_line}"
        );
        assert!(
            hangar_line.contains("process"),
            "hangar must stay a process: {hangar_line}"
        );
        assert!(
            hangar_line.contains("-"),
            "missing process PID stays explicit: {hangar_line}"
        );
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
        let out = render_to_string(&mut state, 120, 24);
        // The System services panel is gone on purpose: everything it listed is
        // a real table row now, so a second panel would show strictly less.
        assert!(
            !out.contains("System services"),
            "the System services panel must not come back: {out}"
        );
        assert!(
            !out.contains("collecting…"),
            "nothing on this screen may sit on a collecting placeholder: {out}"
        );
        assert!(out.contains("Hooks"), "hook section missing: {out}");
        assert!(
            out.contains("I install / repair"),
            "hook install/repair action missing: {out}"
        );
        assert!(out.contains("release"), "hook mode missing: {out}");
        assert!(
            out.contains("/usr/local/bin/ainb"),
            "hook target missing: {out}"
        );
        assert!(
            out.contains("0.4.4 → 0.4.5"),
            "version state missing: {out}"
        );
        assert!(
            out.contains("ainb doctor --fix-hooks"),
            "repair missing: {out}"
        );
        assert!(out.contains("claude ✓"), "agent wiring missing: {out}");
        assert!(
            out.contains("Claude hook Healthy"),
            "hook evidence health missing: {out}"
        );
    }

    #[test]
    fn renders_silent_and_unavailable_hook_evidence_as_warnings() {
        let mut state = seeded_state_with_hook(Vec::new(), hook_health());
        let shared = Arc::clone(state.shared.as_ref().expect("seeded state"));
        shared.lock().unwrap().evidence_census = Some(EvidenceCensus::from_counts(true, 2, 0));
        let silent = render_to_string(&mut state, 120, 24);
        assert!(
            silent.contains("Claude hook Silent"),
            "silent warning missing: {silent}"
        );

        shared.lock().unwrap().evidence_census = Some(EvidenceCensus::from_counts(false, 2, 2));
        let unavailable = render_to_string(&mut state, 120, 24);
        assert!(
            unavailable.contains("Claude hook Unavailable"),
            "unavailable warning missing: {unavailable}"
        );
    }

    /// Bug 3: a stopped daemon had no way back up. Every row is now selectable
    /// and Enter offers the same three verbs — the point of the whole screen.
    #[test]
    fn enter_opens_an_action_menu_offering_start_restart_and_stop() {
        let mut state = seeded_state(vec![
            status(DaemonKind::Atc, DaemonState::Stopped, false, None),
            status(
                DaemonKind::McpPool,
                DaemonState::Running,
                true,
                Some("sock"),
            ),
        ]);
        state.open_menu();
        let out = render_to_string(&mut state, 120, 24);
        assert!(out.contains("start"), "menu must offer start: {out}");
        assert!(out.contains("restart"), "menu must offer restart: {out}");
        assert!(out.contains("stop"), "menu must offer stop: {out}");
        assert!(
            out.contains("ATC"),
            "the menu must name the row it acts on: {out}"
        );
    }

    /// The menu acts on the SELECTED row, not always the first one.
    #[test]
    fn the_menu_follows_the_selection() {
        let mut state = seeded_state(vec![
            status(DaemonKind::Atc, DaemonState::Stopped, false, None),
            status(
                DaemonKind::McpPool,
                DaemonState::Running,
                true,
                Some("sock"),
            ),
        ]);
        state.move_selection(1);
        state.open_menu();
        assert_eq!(
            state.menu.as_ref().map(|m| m.kind),
            Some(DaemonKind::McpPool)
        );
    }

    /// Selection saturates rather than wrapping, so holding a key parks at the
    /// edge instead of cycling past the row you were aiming for.
    #[test]
    fn selection_saturates_at_both_ends() {
        let mut state = seeded_state(vec![
            status(DaemonKind::Atc, DaemonState::Stopped, false, None),
            status(
                DaemonKind::McpPool,
                DaemonState::Running,
                true,
                Some("sock"),
            ),
        ]);
        state.move_selection(-5);
        assert_eq!(state.selected, 0);
        state.move_selection(50);
        assert_eq!(state.selected, 1);
    }

    /// The reported complaint: an ATC with no instance offered start, restart
    /// and stop, all three of which bail before they read the verb, and the
    /// only real fix was a CLI command quoted in the error.
    #[test]
    fn an_unprovisioned_atc_offers_provisioning_instead_of_dead_verbs() {
        let mut row = status(DaemonKind::Atc, DaemonState::Stopped, false, None);
        // No instance: what the probe reports when nothing is provisioned.
        row.atc_instance = None;
        row.reason = format!(
            "{}, but a heartbeat timer for 'main' is installed and failing every interval",
            crate::fleet::daemons::probe::ATC_UNPROVISIONED
        );
        row.scheduler_orphan = Some("main".to_string());
        let mut state = seeded_state(vec![row]);

        state.open_menu();
        let menu = state.menu.as_ref().expect("menu opened");
        let entries = menu.entries(false);

        assert!(
            entries.contains(&MenuEntry::Act(Action::Provision)),
            "provisioning must be offered: {entries:?}"
        );
        assert!(
            entries.contains(&MenuEntry::Act(Action::RemoveOrphan)),
            "the orphan timer must be removable: {entries:?}"
        );
        // Mission control is a tmux session that provisioning creates, so with
        // nothing provisioned there is nothing to attach and the entry would
        // only fail.
        assert!(
            !entries.contains(&MenuEntry::OpenMissionControl),
            "there is no session to open yet: {entries:?}"
        );
        for dead in [Action::Start, Action::Restart, Action::Stop] {
            assert!(
                !entries.contains(&MenuEntry::Act(dead)),
                "{} would bail on an unprovisioned ATC: {entries:?}",
                dead.id()
            );
        }
    }

    /// The mirror image: a provisioned ATC must not be offered a `provision`
    /// that would reset its meta, nor a teardown of a timer that is doing its
    /// job.
    #[test]
    fn a_provisioned_atc_keeps_the_lifecycle_verbs_and_hides_provisioning() {
        let mut state = seeded_state(vec![status(
            DaemonKind::Atc,
            DaemonState::Running,
            true,
            Some("main"),
        )]);

        state.open_menu();
        let entries = state.menu.as_ref().expect("menu opened").entries(false);

        assert!(entries.contains(&MenuEntry::Act(Action::Restart)));
        assert!(entries.contains(&MenuEntry::OpenMissionControl));
        assert!(!entries.contains(&MenuEntry::Act(Action::Provision)));
        assert!(!entries.contains(&MenuEntry::Act(Action::RemoveOrphan)));
    }

    /// A failure belongs to the daemon it happened to. The row shows a badge
    /// plus the way to read the rest, and the full text is one Enter away.
    #[test]
    fn a_failed_action_badges_its_own_row_and_its_detail_is_readable() {
        let mut state = seeded_state(vec![status(
            DaemonKind::Atc,
            DaemonState::Stopped,
            false,
            None,
        )]);
        state.outcomes.insert(
            DaemonKind::Atc.id(),
            ActionOutcome {
                action: Action::Start,
                ok: false,
                summary: "start failed".to_string(),
                detail: "cmd: ainb daemon atc start\nexit: exit status: 1\n\nstderr:\nsocket already bound by pid 4412".to_string(),
            },
        );
        let out = render_to_string(&mut state, 120, 24);
        assert!(
            out.contains("✗ failed"),
            "row must badge the failure: {out}"
        );
        assert!(
            out.contains("Enter → error"),
            "the row must say how to read the error: {out}"
        );

        state.open_menu();
        // `view last error` is always last, and it exists only because this row
        // HAS an error. Saturating to the end rather than counting entries: the
        // count moves whenever a row gains an action, and this test is about
        // the error view, not the menu's length.
        state.move_menu(isize::MAX);
        state.confirm_menu();
        let out = render_to_string(&mut state, 120, 24);
        assert!(
            out.contains("socket already bound by pid 4412"),
            "the error view must show the real stderr: {out}"
        );
        assert!(
            out.contains("ainb daemon atc start"),
            "the error view must show the command that failed: {out}"
        );
    }

    /// A row with no failure does not offer to show one.
    #[test]
    fn a_clean_row_has_no_view_error_entry() {
        let mut state = seeded_state(vec![status(
            DaemonKind::McpPool,
            DaemonState::Running,
            true,
            Some("sock"),
        )]);
        state.open_menu();
        let out = render_to_string(&mut state, 120, 24);
        assert!(
            !out.contains("view last error"),
            "nothing failed here, so there is nothing to view: {out}"
        );
    }

    /// Running an action from under the open error view must close BOTH
    /// overlays. Clearing only the menu left `error_open` set with nothing able
    /// to render it: the screen painted normally, but `has_overlay` stayed true
    /// so every key except Esc was swallowed and the table was unusable.
    #[test]
    fn acting_from_under_the_error_view_closes_both_overlays() {
        let mut state = seeded_state(vec![status(
            DaemonKind::Atc,
            DaemonState::Stopped,
            false,
            None,
        )]);
        state.outcomes.insert(
            DaemonKind::Atc.id(),
            ActionOutcome {
                action: Action::Start,
                ok: false,
                summary: "start failed".to_string(),
                detail: "boom".to_string(),
            },
        );
        state.open_menu();
        state.error_open = Some(DaemonKind::Atc);
        // Move the cursor back onto a verb and run it.
        state.move_menu(1);
        state.confirm_menu();
        assert!(state.error_open.is_none(), "the error view must close");
        assert!(state.menu.is_none(), "the menu must close");
        assert!(
            !state.has_overlay(),
            "no overlay may remain armed, or the screen swallows every key"
        );
    }

    /// `q` leaves the screen outright, so it must not leave an overlay armed:
    /// the state is app-level and would still be there on re-entry, bound to a
    /// row the selection no longer sits on.
    #[test]
    fn close_all_overlays_clears_both_layers() {
        let mut state = seeded_state(vec![status(
            DaemonKind::Atc,
            DaemonState::Stopped,
            false,
            None,
        )]);
        state.open_menu();
        state.error_open = Some(DaemonKind::Atc);
        state.close_all_overlays();
        assert!(!state.has_overlay());
    }

    /// An action that never returns must not pin its row: `inflight` doubles as
    /// the one-outstanding guard, so a stuck entry would silently swallow every
    /// later action on that daemon for the rest of the process.
    #[test]
    fn an_action_that_never_returns_gives_up_instead_of_pinning_its_row() {
        let mut state = seeded_state(vec![status(
            DaemonKind::McpPool,
            DaemonState::Running,
            true,
            Some("sock"),
        )]);
        // A sender that never sends, started long enough ago to be past the
        // give-up point.
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<ActionOutcome>();
        let started = std::time::Instant::now() - (ACTION_TIMEOUT + Duration::from_secs(1));
        state.inflight.insert(DaemonKind::McpPool.id(), (rx, started));

        state.poll_actions();

        assert!(
            !state.inflight.contains_key(DaemonKind::McpPool.id()),
            "the guard must release so the row is actionable again"
        );
        let outcome = state
            .outcomes
            .get(DaemonKind::McpPool.id())
            .expect("giving up must leave a visible outcome");
        assert!(!outcome.ok);
        assert!(
            outcome.detail.contains("did not finish"),
            "the row must say what happened, got {:?}",
            outcome.detail
        );
        drop(tx);
    }

    /// Esc unwinds the innermost overlay first. Popping straight out from under
    /// an open error view would throw away what the user just opened.
    #[test]
    fn esc_closes_the_error_view_before_the_menu() {
        let mut state = seeded_state(vec![status(
            DaemonKind::Atc,
            DaemonState::Stopped,
            false,
            None,
        )]);
        state.open_menu();
        state.error_open = Some(DaemonKind::Atc);
        state.close_overlay();
        assert!(state.error_open.is_none(), "the error view closes first");
        assert!(state.menu.is_some(), "the menu is still open underneath");
        state.close_overlay();
        assert!(state.menu.is_none(), "then the menu closes");
        assert!(!state.has_overlay(), "and the screen is free to pop");
    }

    /// The reported failure was a pointer aimed at a deleted worktree while a
    /// perfectly good ainb was running. The panel has to show BOTH paths, or
    /// the only way to see the mismatch is to go and read the pointer file.
    #[test]
    fn hook_panel_shows_the_pointer_and_the_running_binary() {
        let mut health = hook_health();
        health.hook_binary = Some(PathBuf::from("/gone/worktree/target/debug/ainb"));
        health.hook_binary_ready = false;
        health.hook_binary_mode = Some(HookBinaryMode::Dev);
        health.running_binary = Some(PathBuf::from("/home/u/.local/bin/ainb"));
        let mut state = seeded_state_with_hook(Vec::new(), health);

        let out = render_to_string(&mut state, 160, 30);

        assert!(
            out.contains("/gone/worktree/target/debug/ainb"),
            "the hook pointer must be visible: {out}"
        );
        assert!(
            out.contains("/home/u/.local/bin/ainb"),
            "the running binary must be visible beside it: {out}"
        );
        assert!(
            out.contains("B pin running"),
            "the pin-running action must be advertised: {out}"
        );
    }

    /// A finished action's line must not outlive the health beside it. The
    /// state is app-level, so a status that never expires would replace the
    /// live issue line for the rest of the process, hiding every fault found
    /// after the last repair.
    #[test]
    fn a_finished_hook_status_expires_and_gives_the_issue_line_back() {
        let mut state = DaemonsState::default();
        let now = std::time::Instant::now();

        state.hooks_status = Some((
            "hooks installed for claude".to_string(),
            now + STATUS_LINGER,
        ));
        assert_eq!(
            state.live_hooks_status(),
            Some("hooks installed for claude"),
            "a fresh result must be readable, not gone on the next collect"
        );

        state.hooks_status = Some(("hooks installed for claude".to_string(), now));
        assert_eq!(
            state.live_hooks_status(),
            None,
            "once expired the live issue line must come back"
        );
    }

    #[test]
    fn renders_hook_repair_progress_from_its_own_state() {
        let mut state = seeded_state_with_hook(Vec::new(), hook_health());
        state.hooks_status = Some((
            "hooks repaired for claude, codex".to_string(),
            std::time::Instant::now() + STATUS_LINGER,
        ));
        let out = render_to_string(&mut state, 120, 24);
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
        // At most every controllable daemon, and never more. Not equality: the
        // fleet-daemon row is conditional now (it appears only while one is
        // actually running, since ATC is the supervisor), so on the machine
        // running this test it is normally absent.
        assert!(
            !guard.rows.is_empty() && guard.rows.len() <= crate::cli::daemon::CONTROLLABLE.len(),
            "collect published {} rows, expected 1..={}",
            guard.rows.len(),
            crate::cli::daemon::CONTROLLABLE.len()
        );
        assert!(
            guard.collected_at_ms > 0,
            "publish stamps the collect clock"
        );
    }

    #[test]
    fn render_does_not_panic_on_default_state() {
        // A fresh state sees an empty snapshot (the collector hasn't published
        // yet) and must render an empty table without panicking.
        let mut state = DaemonsState::default();
        let _ = render_to_string(&mut state, 100, 10);
        // The collector handle is installed by the tick, which is what the frame
        // helper runs before painting.
        assert!(state.shared.is_some(), "the tick must arm the collector");
    }

    /// The seal: painting is a read. A frame that only paints must not spawn a
    /// background thread, or the draw path is still doing work behind the
    /// reducer's back.
    #[test]
    fn painting_alone_never_arms_the_collector() {
        let state = DaemonsState::default();
        let backend = TestBackend::new(100, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| render(f, f.area(), &state)).unwrap();

        assert!(
            state.shared.is_none(),
            "render must not spawn the collector; only `tick` may"
        );
    }
}
