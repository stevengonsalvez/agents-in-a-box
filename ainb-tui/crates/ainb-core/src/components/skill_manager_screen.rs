// ABOUTME: Skills/units manager TUI screen — spec §10.1 layout.
//
// V1 ships the layout shell + render path. Live data binding to
// the actual manifest / lockfile / usage cache happens via
// `SkillsScreenData` populated by the runtime (TODO follow-up: wire
// from ainb-cli helpers + ainb-usage). The tripwire test in
// `tests/test_skills_screen.rs` only needs the render path to be
// deterministic and to surface the spec markers (Sources / Units /
// Detail / help bar) so the cutover gate (P8) is satisfied.

use std::path::{Path, PathBuf};

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Cell, Clear, Paragraph, Row, Table, TableState},
};

use std::collections::BTreeMap;

use ainb_cli::discovery::{
    class_a,
    class_c,
    reconcile::{self, WalkerOutput},
};
use ainb_skill_core::drift::DriftStatus;
use ainb_skill_core::lockfile::{DeployedRef, Lockfile};
use ainb_skill_core::paths::{lockfile_path_in, manifest_path_in};
use ainb_skill_core::{Manifest, UnitEntry};

// Style guide constants — match the rest of ainb-tui's components
// (cornflower borders, gold titles, soft white text, muted gray for
// helper text). These mirror crates/ainb-tui/src/components/home_screen_v2.rs.
const CORNFLOWER_BLUE: Color = Color::Rgb(100, 149, 237);
const GOLD: Color = Color::Rgb(255, 215, 0);
const SOFT_WHITE: Color = Color::Rgb(220, 220, 230);
const MUTED_GRAY: Color = Color::Rgb(120, 120, 140);
const SELECTION_GREEN: Color = Color::Rgb(100, 200, 100);
const LIST_HIGHLIGHT_BG: Color = Color::Rgb(40, 40, 60);

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
    /// The unit's declared URI as recorded in the manifest (`<source>@<ref>/<path>`).
    /// Used as the lookup key into [`SkillsScreenData::drift_cache`] so the
    /// rendered status column can find the right glyph. Reconstructed from
    /// the underlying `UnitEntry.uri` when the row is built; matches the
    /// `LockedUnit.declared_uri` recorded in the lockfile.
    pub declared_uri: String,
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
    /// First-open discovery banner (spec §User Flow 1 + §P5).
    /// `Hidden` is the normal steady state; transitions to
    /// `Visible` (or `Details`) on screen-enter when the manifest
    /// is empty AND walkers find candidates.
    pub banner: DiscoveryBannerState,
    /// Cached walker output captured at the moment the banner was
    /// shown. Reused by the `[Enter]` import path so the user
    /// doesn't see a different count than the banner advertised
    /// (e.g. if a file lands between paint and keypress).
    pub walker_cache: Option<WalkerOutput>,
    /// Per-unit drift status, keyed by `UnitRow.declared_uri`. Populated
    /// by the background drift poll (bead v12.E.4) on `GoToSkillManager`;
    /// rows whose URI is missing from the cache render a "…" placeholder
    /// in the status column until the poll lands. See [`drift_status_glyph`].
    pub drift_cache: BTreeMap<String, DriftStatus>,
}

/// Discovery banner state machine.
///
/// Transitions:
/// - `Hidden` → `Visible` on screen-enter when
///   `manifest.units.is_empty()` AND walker output has candidates.
/// - `Visible` ↔ `Details` via `[d]` keybind (just toggles which
///   variant is rendered; same underlying counts).
/// - `Visible | Details` → `Hidden` on `[Enter]` (after import) or
///   on `[s]` (skip, persisted via marker file).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum DiscoveryBannerState {
    #[default]
    Hidden,
    Visible(DiscoveryBannerCounts),
    Details(DiscoveryBannerCounts),
}

impl DiscoveryBannerState {
    pub fn is_active(&self) -> bool {
        !matches!(self, DiscoveryBannerState::Hidden)
    }

    pub fn counts(&self) -> Option<&DiscoveryBannerCounts> {
        match self {
            DiscoveryBannerState::Visible(c) | DiscoveryBannerState::Details(c) => Some(c),
            DiscoveryBannerState::Hidden => None,
        }
    }
}

/// Per-category counts shown in the discovery banner. Mirrors the
/// ASCII mockup in spec §User Flow 1.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiscoveryBannerCounts {
    pub marketplace_plugins: usize,
    pub orphan_units_total: usize,
    /// `(tool, count)` pairs in deterministic input order. Tools
    /// with zero orphans are omitted so the banner stays compact.
    pub orphan_units_per_tool: Vec<(String, usize)>,
    pub conflicts: usize,
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

    // Discovery banner overlay (spec §User Flow 1) — drawn LAST
    // so it sits on top of the underlying panels. Hidden state
    // is a no-op.
    if let Some(counts) = data.banner.counts() {
        let detailed = matches!(data.banner, DiscoveryBannerState::Details(_));
        render_discovery_banner(frame, area, counts, detailed);
    }
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
            let uri = Span::styled(format!("({})", source.uri), Style::default().fg(MUTED_GRAY));
            lines.push(Line::from(vec![
                Span::styled(glyph, glyph_style),
                name,
                uri,
            ]));
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

    let rows: Vec<Row> = data
        .units
        .iter()
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

    let mut table_state = TableState::default();
    let last = data.units.len().saturating_sub(1);
    table_state.select(Some(data.selected.min(last)));
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
/// Marker file under `$AINB_HOME` whose presence means the user
/// pressed `[s] skip` on the discovery banner. Cleared by a forced
/// re-scan (e.g. `ainb migrate --discover --force`, future P5+).
const SKIP_MARKER_FILE: &str = ".discovery-skipped";

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
            body_lines.push(banner_row_indent(
                &format!("~/.{tool}/skills/"),
                *n,
            ));
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
        Span::styled(
            format!(" {label:<36}"),
            Style::default().fg(SOFT_WHITE),
        ),
        Span::styled(
            format!("{n:>3} "),
            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
        ),
    ])
}

fn banner_row_indent(label: &str, n: usize) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("   {label:<34}"),
            Style::default().fg(MUTED_GRAY),
        ),
        Span::styled(
            format!("{n:>3} "),
            Style::default().fg(SOFT_WHITE),
        ),
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
    Rect { x, y, width: w, height: h }
}

// ---------------------------------------------------------------------
// Trigger + import (spec §User Flow 1 step 3-5)
// ---------------------------------------------------------------------

/// Run the class-A + class-C walkers against the configured tool
/// homes. Thin wrapper around the walker modules so production
/// callers can capture a [`WalkerOutput`] in one place; tests
/// build synthetic outputs by hand.
///
/// `claude_home` is normally `$HOME/.claude`. Class-C uses
/// `ainb_adapters_tool::install_root_for` internally, which honours
/// `AINB_TOOL_HOME_<TOOL>` and `AINB_USE_REAL_HOMES` env vars.
pub fn run_discovery_walkers(claude_home: &Path) -> WalkerOutput {
    WalkerOutput {
        class_a: class_a::walk(claude_home),
        class_c: class_c::walk_orphans(),
    }
}

/// Flip `data.banner` to `Visible` when:
/// - the manifest under `ainb_home` is empty AND
/// - the user has not already pressed `[s]` in a prior open
///   (skip-marker absent under `ainb_home`) AND
/// - the combined walker output is non-empty.
///
/// Idempotent: re-entering an already-`Visible` banner is a no-op
/// (per spec edge case "Banner re-appears next open until dismissed
/// via [s]" — but with the *same* counts the user saw the first
/// time, not a freshly-walked snapshot).
///
/// Pure with respect to discovery: the only fs interaction is
/// reading `<ainb_home>/manifest.yaml` and checking for the skip
/// marker. Walkers run upstream of this fn so tests can inject
/// synthetic walker output without env-var surgery.
pub fn maybe_show_discovery_banner(
    data: &mut SkillsScreenData,
    ainb_home: &Path,
    walker: WalkerOutput,
) {
    if data.banner.is_active() {
        tracing::info!("discovery banner: skip — banner already active");
        return;
    }
    let manifest_path = ainb_home.join("manifest.yaml");
    let manifest = Manifest::load_from(&manifest_path).unwrap_or_default();
    if !manifest.units.is_empty() {
        tracing::info!(
            units = manifest.units.len(),
            "discovery banner: skip — manifest non-empty"
        );
        return;
    }
    let skip_path = ainb_home.join(SKIP_MARKER_FILE);
    if skip_path.exists() {
        tracing::info!(?skip_path, "discovery banner: skip — skip marker present");
        return;
    }
    let counts = compute_counts(&walker);
    let total = counts.marketplace_plugins + counts.orphan_units_total;
    tracing::info!(
        ainb_home = %ainb_home.display(),
        class_a = walker.class_a.len(),
        class_c = walker.class_c.len(),
        marketplace_plugins = counts.marketplace_plugins,
        orphan_units_total = counts.orphan_units_total,
        total,
        "discovery banner: walker output"
    );
    if total == 0 {
        tracing::info!("discovery banner: skip — walker output empty");
        return;
    }
    tracing::info!(total, "discovery banner: showing Visible");
    data.banner = DiscoveryBannerState::Visible(counts);
    data.walker_cache = Some(walker);
}

/// Apply `[Enter] import all` — calls the reconciler on the cached
/// walker output, merges the patch into the on-disk manifest, and
/// refreshes the screen view-model so the Units / Sources panels
/// show the just-imported entries.
///
/// Best-effort: returns `Err` only on filesystem failures during
/// the manifest write. On success the banner is dismissed (no
/// skip-marker — the import itself is the "yes" answer).
pub fn apply_discovery_import(
    data: &mut SkillsScreenData,
    ainb_home: &Path,
) -> std::io::Result<()> {
    let Some(walker) = data.walker_cache.take() else {
        // No cached walker → nothing to import. Caller already
        // checked banner state; this is a defensive no-op.
        data.banner = DiscoveryBannerState::Hidden;
        return Ok(());
    };
    let patch = reconcile::reconcile(&walker);

    let manifest_path = ainb_home.join("manifest.yaml");
    let mut manifest = Manifest::load_from(&manifest_path).unwrap_or_default();
    for src in patch.new_sources {
        // De-dup by name; new entry wins to keep the patch
        // idempotent across repeat imports.
        if let Some(existing) = manifest.source_mut(&src.name) {
            *existing = src;
        } else {
            // Source name uniqueness was just checked, so
            // `add_source` cannot fail here.
            let _ = manifest.add_source(src);
        }
    }
    for unit in &patch.new_units {
        if !manifest.units.iter().any(|u| u.uri == unit.uri) {
            manifest.units.push(unit.clone());
        }
    }
    if let Err(e) = manifest.save_to(&manifest_path) {
        // Re-stash the walker output so the user can retry.
        data.walker_cache = Some(walker);
        return Err(std::io::Error::other(format!(
            "manifest save failed: {e}"
        )));
    }

    refresh_view_model_from_manifest(data, &manifest);
    data.banner = DiscoveryBannerState::Hidden;
    Ok(())
}

/// Apply `[s] skip` — writes a marker file under `ainb_home` so
/// subsequent SkillManager opens do not re-show the banner, and
/// flips the in-memory state to `Hidden`.
pub fn apply_discovery_skip(
    data: &mut SkillsScreenData,
    ainb_home: &Path,
) -> std::io::Result<()> {
    std::fs::create_dir_all(ainb_home)?;
    std::fs::write(ainb_home.join(SKIP_MARKER_FILE), b"")?;
    data.banner = DiscoveryBannerState::Hidden;
    data.walker_cache = None;
    Ok(())
}

/// Apply `[s]` on the Units panel — flip the `shadowed_by`
/// relationship for the currently-selected unit when it is part of a
/// conflict pair (spec §User Flow 3 + hdt.8 / P7).
///
/// Behaviour:
/// - If the selected unit currently has `shadowed_by = Some(X)`, find
///   the unit with `uri == X` and swap: selected becomes active, the
///   peer becomes shadowed-by-selected.
/// - If the selected unit has `shadowed_by = None` but some other unit
///   has `shadowed_by = Some(selected.uri)`, swap the other way.
/// - Otherwise the unit is not part of a conflict pair → no-op
///   (silent; the keystroke just does nothing).
///
/// Persists immediately to `<ainb_home>/manifest.yaml` and refreshes
/// the view-model so subsequent renders show the flipped state. Pure
/// w.r.t. disk except for that one manifest write.
pub fn apply_conflict_flip(
    data: &mut SkillsScreenData,
    ainb_home: &Path,
) -> std::io::Result<()> {
    let manifest_path = ainb_home.join("manifest.yaml");
    let mut manifest = match Manifest::load_from(&manifest_path) {
        Ok(m) => m,
        // No manifest on disk → nothing to flip. Silent no-op so the
        // keystroke can't crash the screen.
        Err(_) => return Ok(()),
    };
    if manifest.units.is_empty() {
        return Ok(());
    }
    let sel = data.selected;
    if sel >= manifest.units.len() {
        return Ok(());
    }

    let sel_uri_str = manifest.units[sel].uri.clone();
    let other_idx: Option<usize> = if manifest.units[sel].shadowed_by.is_some() {
        // Case A: selected is shadowed → peer is the unit pointed at
        // by selected.shadowed_by.
        let target = manifest.units[sel]
            .shadowed_by
            .as_ref()
            .map(|u| u.to_string());
        target.and_then(|t| manifest.units.iter().position(|u| u.uri == t))
    } else {
        // Case B: selected is active → peer is whichever unit has
        // shadowed_by pointing back at selected.uri.
        manifest.units.iter().enumerate().find_map(|(i, u)| {
            u.shadowed_by
                .as_ref()
                .filter(|x| x.to_string() == sel_uri_str)
                .map(|_| i)
        })
    };

    let Some(other) = other_idx else {
        // No conflict pair — silent no-op (negative case).
        return Ok(());
    };
    if other == sel {
        // Defensive: should never happen (a unit cannot shadow
        // itself) but guard against a malformed manifest.
        return Ok(());
    }

    // Swap which side carries `shadowed_by`. Exactly one side is
    // expected to have it set before the flip; afterwards the other
    // side does.
    let sel_was_shadowed = manifest.units[sel].shadowed_by.is_some();
    let other_uri_str = manifest.units[other].uri.clone();
    if sel_was_shadowed {
        // sel was shadowed → sel becomes active, other becomes shadowed.
        manifest.units[sel].shadowed_by = None;
        manifest.units[other].shadowed_by = ainb_skill_core::Uri::parse(&sel_uri_str).ok();
    } else {
        // sel was active → sel becomes shadowed, other becomes active.
        manifest.units[sel].shadowed_by = ainb_skill_core::Uri::parse(&other_uri_str).ok();
        manifest.units[other].shadowed_by = None;
    }

    manifest
        .save_to(&manifest_path)
        .map_err(|e| std::io::Error::other(format!("manifest save failed: {e}")))?;

    refresh_view_model_from_manifest(data, &manifest);
    Ok(())
}

/// Apply `[d] details` — toggles between the compact `Visible`
/// and expanded `Details` rendering. No-op when banner is `Hidden`.
pub fn toggle_discovery_details(data: &mut SkillsScreenData) {
    data.banner = match std::mem::take(&mut data.banner) {
        DiscoveryBannerState::Visible(c) => DiscoveryBannerState::Details(c),
        DiscoveryBannerState::Details(c) => DiscoveryBannerState::Visible(c),
        DiscoveryBannerState::Hidden => DiscoveryBannerState::Hidden,
    };
}

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

fn compute_counts(walker: &WalkerOutput) -> DiscoveryBannerCounts {
    let marketplace_plugins = walker.class_a.len();
    let orphan_units_total = walker.class_c.len();
    let conflicts = count_conflicts(walker);

    // Deterministic per-tool breakdown in first-seen order. Tools
    // with zero orphans aren't included so the banner stays terse.
    let mut per_tool: Vec<(String, usize)> = Vec::new();
    for orphan in &walker.class_c {
        if let Some(entry) = per_tool.iter_mut().find(|(t, _)| t == &orphan.tool) {
            entry.1 += 1;
        } else {
            per_tool.push((orphan.tool.clone(), 1));
        }
    }
    DiscoveryBannerCounts {
        marketplace_plugins,
        orphan_units_total,
        orphan_units_per_tool: per_tool,
        conflicts,
    }
}

/// Conflict count = number of marketplace-shipped units that share
/// a name with a class-C orphan in the `claude` tool home. Mirrors
/// the reconciler's A-vs-C-in-claude rule so the banner number
/// matches the import outcome 1:1.
fn count_conflicts(walker: &WalkerOutput) -> usize {
    use std::collections::HashSet;
    let claude_names: HashSet<&str> = walker
        .class_c
        .iter()
        .filter(|o| o.tool == "claude")
        .map(|o| o.name.as_str())
        .collect();
    if claude_names.is_empty() {
        return 0;
    }
    walker
        .class_a
        .iter()
        .flat_map(|plugin| plugin.units.iter())
        .filter(|u| claude_names.contains(u.name.as_str()))
        .count()
}

impl SkillsScreenData {
    /// Load the screen's view-model from the on-disk manifest +
    /// lockfile under `home`. Best-effort: missing manifest / lockfile
    /// yields empty rows (the screen renders placeholders), and
    /// malformed YAML is treated as "not present" rather than a hard
    /// error so a corrupt lockfile never blocks the user from opening
    /// the screen.
    ///
    /// Banner state (`banner` / `walker_cache`) is left untouched so a
    /// caller can sequence `load_from_disk` -> `maybe_show_discovery_banner`
    /// without clobbering banner counts the user just saw.
    ///
    /// Spec §Implementation Phases P8 + §Components row
    /// `tui::discovery_banner` / `skills_screen_data`.
    pub fn load_from_disk(home: &Path) -> Self {
        let manifest = Manifest::load_from(&manifest_path_in(home)).unwrap_or_default();
        let lockfile = Lockfile::load_from(&lockfile_path_in(home)).unwrap_or_default();
        let mut data = SkillsScreenData::default();
        refresh_view_model_from_manifest(&mut data, &manifest);
        data.detail = compute_detail_for_selected(&data, &lockfile);
        data
    }

    /// Reload manifest + lockfile and refresh the rendered rows in
    /// place. Banner state is left untouched. Used by callers that
    /// already own a `SkillsScreenData` and want to pick up
    /// out-of-band manifest mutations (e.g. after `ainb migrate
    /// --discover` has rewritten disk).
    pub fn reload_from_disk(&mut self, home: &Path) {
        let manifest = Manifest::load_from(&manifest_path_in(home)).unwrap_or_default();
        let lockfile = Lockfile::load_from(&lockfile_path_in(home)).unwrap_or_default();
        refresh_view_model_from_manifest(self, &manifest);
        self.detail = compute_detail_for_selected(self, &lockfile);
    }
}

/// Direction of selection-cursor movement on the Units panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionMove {
    Prev,
    Next,
    First,
    Last,
}

/// Move the Units-panel selection cursor and recompute the Detail
/// pane so the right-hand view stays in sync. Wraps at list ends.
/// No-op when units list is empty.
pub fn move_selection(data: &mut SkillsScreenData, home: &Path, dir: SelectionMove) {
    if data.units.is_empty() {
        return;
    }
    let last = data.units.len() - 1;
    let cur = data.selected.min(last);
    data.selected = match dir {
        SelectionMove::Prev => {
            if cur == 0 {
                last
            } else {
                cur - 1
            }
        }
        SelectionMove::Next => {
            if cur == last {
                0
            } else {
                cur + 1
            }
        }
        SelectionMove::First => 0,
        SelectionMove::Last => last,
    };
    let lockfile = Lockfile::load_from(&lockfile_path_in(home)).unwrap_or_default();
    data.detail = compute_detail_for_selected(data, &lockfile);
}

/// Build the detail-pane content for the currently-selected unit by
/// joining the manifest's URI with the lockfile's deployment record.
/// Returns `None` when the units list is empty (the render path
/// shows its "(select a unit to see details)" placeholder).
fn compute_detail_for_selected(
    data: &SkillsScreenData,
    lockfile: &Lockfile,
) -> Option<UnitDetail> {
    if data.units.is_empty() {
        return None;
    }
    let idx = data.selected.min(data.units.len().saturating_sub(1));
    let row = data.units.get(idx)?;
    let manifest_uri = manifest_uri_for_row(row);
    let locked = lockfile
        .units
        .iter()
        .find(|u| u.declared_uri == manifest_uri);
    let deployed_paths = locked
        .map(|u| {
            u.deployed
                .values()
                .filter_map(|d| match d {
                    DeployedRef::Deployed { path, .. } => Some(path.clone()),
                    DeployedRef::Skipped { .. } | DeployedRef::PendingUninstall => None,
                })
                .collect()
        })
        .unwrap_or_default();
    let (last_used, invocations) = locked
        .filter(|u| !u.usage.is_empty())
        .map(|u| (u.usage.last_used_at.clone(), Some(u.usage.invocations)))
        .unwrap_or((None, None));
    Some(UnitDetail {
        uri: manifest_uri,
        deployed: deployed_paths,
        last_used,
        invocations,
        // Requires / upstream wiring is follow-up work; MVP keeps
        // these empty so the detail pane simply omits the lines.
        requires: Vec::new(),
        upstream_status: String::new(),
    })
}

/// Reconstruct the manifest URI for a `UnitRow`. `UnitRow` is the
/// render-side projection (split into name/source/kind/ref); the
/// lockfile keys on the full declared URI, so we re-assemble.
fn manifest_uri_for_row(row: &UnitRow) -> String {
    if row.source.is_empty() {
        // Defensive: row came from a URI the parser couldn't split.
        // `name` holds the original URI in that branch.
        return row.name.clone();
    }
    if row.git_ref.is_empty() {
        format!("{}/{}", row.source, row.name)
    } else {
        format!("{}@{}/{}", row.source, row.git_ref, row.name)
    }
}

/// Rebuild the screen's view-model rows from a manifest. Called
/// after `[Enter]` so the user immediately sees imported entries
/// without waiting for a separate refresh trigger. Best-effort;
/// keeps existing `selected` / `detail` state unchanged.
fn refresh_view_model_from_manifest(data: &mut SkillsScreenData, manifest: &Manifest) {
    data.sources = manifest
        .sources
        .iter()
        .map(|s| SourceRow {
            name: s.name.clone(),
            uri: s.uri.clone(),
            enabled: s.enabled,
        })
        .collect();
    data.units = manifest
        .units
        .iter()
        .enumerate()
        .map(|(i, u)| unit_row_from_entry(i + 1, u))
        .collect();
}

fn unit_row_from_entry(idx: usize, u: &UnitEntry) -> UnitRow {
    let parsed = ainb_skill_core::Uri::parse(&u.uri).ok();
    let (name, source, kind, git_ref) = match parsed.as_ref() {
        Some(uri) => (
            uri.path.clone().unwrap_or_else(|| uri.locator.clone()),
            format!("{}:{}", uri.source_type, uri.locator),
            uri.source_type.to_string(),
            uri.ref_.clone().unwrap_or_default(),
        ),
        None => (u.uri.clone(), String::new(), String::new(), String::new()),
    };
    UnitRow {
        idx,
        name,
        kind,
        source,
        git_ref,
        targets: u.targets.clone().unwrap_or_default(),
        declared_uri: u.uri.clone(),
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

    fn mk_plugin(plugin: &str, marketplace: &str, unit_names: &[&str]) -> DiscoveredMarketplaceUnit {
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
                mk_orphan("claude", "commit"),  // collides → conflict
                mk_orphan("codex", "commit"),   // different tool → not a conflict
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
        terminal
            .draw(|f| render(f, f.size(), &data))
            .expect("render did not panic");
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
        assert!(joined.contains("Orphan units:"), "orphan row missing: {joined}");
        assert!(joined.contains("Conflicts"), "conflicts row missing: {joined}");
        assert!(joined.contains("[Enter]"), "help: Enter missing: {joined}");
        assert!(joined.contains("[d]"), "help: d missing: {joined}");
        assert!(joined.contains("[s]"), "help: s missing: {joined}");
    }
}
