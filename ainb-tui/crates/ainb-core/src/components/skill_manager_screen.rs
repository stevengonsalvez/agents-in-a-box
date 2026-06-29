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
    class_a, class_c,
    provenance::{ProvenanceSources, parse_external_dependencies, parse_installed_plugins},
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

/// Which of the two top panels owns keyboard focus.
///
/// Drives both the render path (focused panel gets the bright/gold
/// border + active cursor) and the key-dispatch path (Up/Down/j/k move
/// the focused panel's cursor; `Tab` toggles between them). Defaults to
/// [`FocusedSkillPane::Units`] so existing behaviour — arrows drive the
/// Units table — is unchanged when nothing has touched focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FocusedSkillPane {
    /// Left "Sources" panel. Up/Down move the source cursor; Enter (or a
    /// click) applies that source as the Units filter.
    Sources,
    /// Right "Units" table — the default. Up/Down move the unit cursor.
    #[default]
    Units,
}

/// Default width (terminal columns) of the left Sources panel. Matches
/// the pre-resize hard-coded `Constraint::Length(32)`.
pub const DEFAULT_SOURCES_WIDTH: u16 = 32;
/// Minimum draggable width of the Sources panel (keeps the glyph +
/// short name legible).
pub const MIN_SOURCES_WIDTH: u16 = 18;
/// Columns reserved for the Units table when the Sources panel is at
/// its maximum width — the panel can never grow past
/// `term_w - SOURCES_UNITS_RESERVE`.
pub const SOURCES_UNITS_RESERVE: u16 = 40;

/// Clamp a requested Sources-panel width to `[MIN_SOURCES_WIDTH,
/// term_w - SOURCES_UNITS_RESERVE]`. Mirrors
/// [`crate::app::state::SessionsPaneState::clamp_width`] but for the
/// skill-manager layout. Degrades gracefully on tiny terminals.
pub fn clamp_sources_width(width: u16, term_w: u16) -> u16 {
    if term_w <= MIN_SOURCES_WIDTH {
        return term_w.max(1);
    }
    let max = term_w.saturating_sub(SOURCES_UNITS_RESERVE);
    if max < MIN_SOURCES_WIDTH {
        return term_w.saturating_sub(1).max(1);
    }
    width.clamp(MIN_SOURCES_WIDTH, max)
}

/// Aggregate view-model the screen renders.
///
/// Hand-populated for tests; the production runtime will assemble
/// this from `ainb_skill_core::Manifest` + `ainb_skill_core::Lockfile` +
/// `ainb_usage::UsageCache`.
///
/// NOTE: `Default` is implemented by hand (NOT derived) so the
/// resize/focus fields get sane non-zero defaults — `sources_width`
/// must default to [`DEFAULT_SOURCES_WIDTH`] (32), not 0, and
/// `focused_pane` to [`FocusedSkillPane::Units`]. A derived `Default`
/// would zero `sources_width` and collapse the Sources panel.
#[derive(Debug, Clone)]
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
    /// Active text-input prompt (add-source URI or search filter).
    /// `None` in the steady state. When `Some`, the SkillManager key
    /// handler routes every keystroke into the buffer until Enter /
    /// Esc. Rendered as a centered overlay (see [`render_input_prompt`]).
    pub input: Option<InputState>,
    /// Applied search filter (lower-cased substring). When `Some`,
    /// the Units table only renders rows whose name / source / kind
    /// contains it. Cleared by submitting an empty search or `[/]`
    /// then Esc.
    pub search: Option<String>,
    /// Own-skill Library view (`[l]`). `None` in the steady state;
    /// `Some` while the Library overlay is open. Sourced from
    /// `library.yaml`, not the manifest units (bead ai-lgk).
    pub library: Option<LibraryViewState>,
    /// Catalog browse modal (`[b]`). `None` in the steady state; `Some`
    /// while the browse overlay is open. Holds the query buffer + the
    /// ephemeral search results (NO SQLite — discarded on close). Sourced
    /// from a `CatalogBackend`, not the manifest (bead ai-a20).
    pub browse: Option<BrowseViewState>,
    /// Width (terminal columns) of the left Sources panel. Resizable by
    /// dragging the Sources/Units divider or via `[`/`]`. Persisted to
    /// `ui_preferences.skill_manager_sources_width` on resize-finish and
    /// re-applied on screen-open. Defaults to [`DEFAULT_SOURCES_WIDTH`].
    pub sources_width: u16,
    /// Which top panel owns keyboard focus (`Tab` toggles). The focused
    /// panel renders a bright/gold border + active cursor; the other is
    /// muted. Defaults to [`FocusedSkillPane::Units`].
    pub focused_pane: FocusedSkillPane,
    /// Cursor row in the Sources panel (index into [`Self::sources`]).
    /// Independent of [`Self::selected`] (the Units cursor). Bounded by
    /// the source nav helpers; ignored when `sources` is empty.
    pub source_selected: usize,
    /// Active source filter: when `Some(uri)`, the Units list only shows
    /// rows whose `source` matches that source's URI (ANDed with the
    /// text `search`). Set by selecting a Source; cleared by `Esc` or
    /// the "All sources" affordance. Keyed on `SourceRow.uri` because
    /// that's what `UnitRow.source` is built from.
    pub source_filter: Option<String>,
    /// True while a Sources/Units divider drag is in flight (between
    /// `MouseClick` on the edge and `MouseDragEnd`). Drives the bright
    /// edge highlight and gates `drag_resize`.
    pub resize_active: bool,
    /// `Some(uri)` after the first `[r]` on a unit — arms a one-shot
    /// confirm so a single keypress can't uninstall. A second `[r]` on
    /// the *same* unit confirms; moving the cursor (which changes the
    /// selected URI) re-arms for the new row, so a stray `r` never
    /// removes the wrong unit. Cleared on any successful action via
    /// [`Self::reload_from_disk`].
    pub pending_remove_confirm: Option<String>,
}

impl Default for SkillsScreenData {
    fn default() -> Self {
        Self {
            sources: Vec::new(),
            units: Vec::new(),
            selected: 0,
            detail: None,
            banner: DiscoveryBannerState::default(),
            walker_cache: None,
            drift_cache: BTreeMap::new(),
            input: None,
            search: None,
            library: None,
            browse: None,
            sources_width: DEFAULT_SOURCES_WIDTH,
            focused_pane: FocusedSkillPane::default(),
            source_selected: 0,
            source_filter: None,
            resize_active: false,
            pending_remove_confirm: None,
        }
    }
}

/// One catalog hit row in the browse modal, projected from a
/// [`ainb_skill_core::CatalogHit`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowseRow {
    pub name: String,
    pub repo: String,
    pub stars: u64,
    /// For a `skill` kind: the unit URI fed to the install flow. For
    /// npx/plugin/mcp kinds: the shell command that installs it.
    pub install_uri: String,
    pub description: String,
    /// How this entry installs — drives the shelf badge and the install
    /// routing (unit flow vs run-the-command).
    pub kind: ainb_skill_core::catalog::CatalogEntryKind,
}

/// Which phase the browse modal is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BrowseMode {
    /// Typing the query — keystrokes go into the buffer; Enter searches.
    #[default]
    Query,
    /// Browsing the result list — arrows select; Enter installs.
    Results,
}

/// Which catalog the `[b]` modal is browsing. `Tab` toggles between them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CatalogKind {
    /// The toolkit's curated shelf (owned skills + vetted external),
    /// fetched from the pinned GitHub release index — offline-capable and
    /// the default, since it needs no API key.
    #[default]
    Curated,
    /// The public skills.sh catalog (needs network + an API key).
    SkillsSh,
}

impl CatalogKind {
    /// Short label shown in the modal title.
    pub fn label(self) -> &'static str {
        match self {
            CatalogKind::Curated => "ainb curated",
            CatalogKind::SkillsSh => "skills.sh",
        }
    }

    /// The other catalog — what `Tab` switches to.
    pub fn toggled(self) -> Self {
        match self {
            CatalogKind::Curated => CatalogKind::SkillsSh,
            CatalogKind::SkillsSh => CatalogKind::Curated,
        }
    }

    /// Whether a blank query should list the whole shelf (curated) rather
    /// than prompt for input (skills.sh).
    pub fn lists_on_blank(self) -> bool {
        matches!(self, CatalogKind::Curated)
    }
}

/// State of the `[b]` catalog browse overlay. Rendered on top of the
/// Sources/Units/Detail panels. Results are ephemeral.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BrowseViewState {
    pub mode: BrowseMode,
    /// Which catalog is being browsed (`Tab` toggles). Defaults to the
    /// curated shelf.
    pub catalog: CatalogKind,
    /// The query being typed (Query mode) or the query that produced the
    /// current results (Results mode).
    pub query: String,
    pub results: Vec<BrowseRow>,
    pub selected: usize,
    /// Optional status line (e.g. an error or "no results") shown beneath
    /// the input. `None` in the happy path.
    pub status: Option<String>,
    /// True after the first Enter on a command-kind (npx/plugin/mcp) row —
    /// the entry installs by RUNNING a shell command, so we require a second
    /// Enter to confirm. Reset by any navigation / new search.
    pub pending_command_confirm: bool,
}

impl BrowseViewState {
    /// A fresh modal in Query mode with an empty buffer.
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace the result list (e.g. after a search) and switch to
    /// Results mode, resetting the cursor to the top. Records a status
    /// hint when the result set is empty.
    pub fn set_results(&mut self, rows: Vec<BrowseRow>) {
        self.status = if rows.is_empty() {
            Some(format!("no results for `{}`", self.query.trim()))
        } else {
            None
        };
        self.results = rows;
        self.selected = 0;
        self.mode = BrowseMode::Results;
        self.pending_command_confirm = false;
    }

    /// Record an error status and stay/return to Query mode so the user
    /// can edit + retry.
    pub fn set_error(&mut self, message: impl Into<String>) {
        self.status = Some(message.into());
        self.results.clear();
        self.mode = BrowseMode::Query;
        self.pending_command_confirm = false;
    }

    pub fn select_prev(&mut self) {
        self.pending_command_confirm = false;
        if self.results.is_empty() {
            return;
        }
        self.selected = if self.selected == 0 {
            self.results.len() - 1
        } else {
            self.selected - 1
        };
    }

    pub fn select_next(&mut self) {
        self.pending_command_confirm = false;
        if self.results.is_empty() {
            return;
        }
        self.selected = (self.selected + 1) % self.results.len();
    }

    /// The currently-selected result row, if any.
    pub fn selected_row(&self) -> Option<&BrowseRow> {
        self.results.get(self.selected)
    }

    /// Arm the command-kind install confirm: surface the exact shell command
    /// and that a second Enter runs it. ainb only ever runs commands from the
    /// vetted curated index, and never without this explicit confirm.
    pub fn set_status_confirm(&mut self, cmd: &str) {
        self.status = Some(format!(
            "⚠ runs a shell command — Enter again to run: {cmd}"
        ));
    }
}

/// One owned-skill row in the Library view, projected from a
/// [`ainb_skill_core::OwnedUnit`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryRow {
    pub name: String,
    pub kind: String,
    /// Tool-home-relative path (`.claude/skills/<name>`).
    pub path: String,
    pub created: String,
    /// Deploy status — `promoted` once a `promoted_uri` is set, else
    /// `local`. Mirrors the column the CLI `list` prints.
    pub deploy: String,
}

/// State of the `[l]` own-skill Library overlay. Rendered on top of the
/// Sources/Units/Detail panels; reuses the same table chrome as the
/// Units panel but sourced from `library.yaml`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LibraryViewState {
    pub rows: Vec<LibraryRow>,
    pub selected: usize,
    /// When `true`, `[Enter]` has expanded the selected row into a
    /// "Library Detail" panel beneath the list.
    pub show_detail: bool,
}

impl LibraryViewState {
    /// Build the view-model from the on-disk `library.yaml` under
    /// `ainb_home`. Missing / malformed file yields an empty view (the
    /// overlay renders its empty-state hint).
    pub fn load_from_disk(ainb_home: &Path) -> Self {
        use ainb_skill_core::library::{Library, library_path_in};
        let lib = Library::load_from(&library_path_in(ainb_home)).unwrap_or_default();
        let rows = lib
            .owned
            .iter()
            .map(|u| LibraryRow {
                name: u.name.clone(),
                kind: u.kind.to_string(),
                path: u.path.clone(),
                created: u.created.clone(),
                deploy: if u.promoted_uri.is_some() {
                    "promoted".to_string()
                } else {
                    "local".to_string()
                },
            })
            .collect();
        Self {
            rows,
            selected: 0,
            show_detail: false,
        }
    }

    /// Move the selection cursor, wrapping at the ends. No-op when the
    /// list is empty. Clears `show_detail` so the detail panel always
    /// reflects the freshly-selected row on the next `[Enter]`.
    pub fn select_prev(&mut self) {
        if self.rows.is_empty() {
            return;
        }
        self.selected = if self.selected == 0 {
            self.rows.len() - 1
        } else {
            self.selected - 1
        };
        self.show_detail = false;
    }

    pub fn select_next(&mut self) {
        if self.rows.is_empty() {
            return;
        }
        self.selected = (self.selected + 1) % self.rows.len();
        self.show_detail = false;
    }

    /// The currently-selected row, if any.
    pub fn selected_row(&self) -> Option<&LibraryRow> {
        self.rows.get(self.selected)
    }
}

/// Which kind of text the active input prompt is collecting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputKind {
    /// `gh:owner/repo` source URI for `ainb source add`.
    AddSource,
    /// Substring to filter the Units table.
    Search,
}

/// State of the active text-input prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputState {
    pub kind: InputKind,
    pub buffer: String,
}

impl InputState {
    pub fn new(kind: InputKind) -> Self {
        Self {
            kind,
            buffer: String::new(),
        }
    }
    /// Prompt label shown in the overlay border.
    pub fn title(&self) -> &'static str {
        match self.kind {
            InputKind::AddSource => " Add source — gh:owner/repo ",
            InputKind::Search => " Search units ",
        }
    }
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
    let width = BANNER_WIDTH.min(area.width.saturating_sub(4)).max(20);
    let height = 5;
    let rect = centered_rect(area, width, height);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(GOLD))
        .title(Span::styled(
            input.title(),
            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
        ));

    let hint = match input.kind {
        InputKind::AddSource => "e.g. gh:owner/repo   [Enter] add  [Esc] cancel",
        InputKind::Search => "type to filter   [Enter] apply  [Esc] cancel",
    };
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
            format!("   {hint}"),
            Style::default().fg(MUTED_GRAY),
        )),
    ];

    frame.render_widget(Clear, rect);
    let para = Paragraph::new(lines).block(block);
    frame.render_widget(para, rect);
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
            lines.push(Line::from(vec![
                Span::styled(
                    marker,
                    Style::default().fg(SELECTION_GREEN).add_modifier(Modifier::BOLD),
                ),
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

/// Load the three parsed source manifests the provenance matcher
/// needs, so the `[Enter] import all` path can attribute each orphan
/// to its real source (external clones resolve to `gh:`, not
/// `local:`). Best-effort: a missing / malformed file degrades to an
/// empty view, which makes the import fall back to the legacy
/// (byte-identical) reconcile.
///
/// Lookups (all rooted at `$HOME`, matching how the discovery
/// walkers resolve `claude_home` in `app::events`):
/// - `installed_plugins.json` at `$HOME/.claude/plugins/`.
/// - `external-dependencies.yaml` at `$HOME` (the bootstrap writes it
///   there; the sandbox fixture seeds it at the sandbox root).
/// - already-adopted units from `<ainb_home>/manifest.yaml`.
fn load_provenance_sources(ainb_home: &Path) -> ProvenanceSources {
    let home = std::env::var_os("HOME").map(PathBuf::from);

    let installed_plugins_json = home
        .as_ref()
        .map(|h| h.join(".claude").join("plugins").join("installed_plugins.json"))
        .and_then(|p| std::fs::read_to_string(p).ok())
        .unwrap_or_default();

    let ext_yaml = home
        .as_ref()
        .map(|h| h.join("external-dependencies.yaml"))
        .filter(|p| p.is_file())
        .and_then(|p| std::fs::read_to_string(p).ok())
        .unwrap_or_default();

    let manifest = Manifest::load_from(&manifest_path_in(ainb_home)).unwrap_or_default();

    ProvenanceSources {
        installed_plugins: parse_installed_plugins(&installed_plugins_json),
        external_deps: parse_external_dependencies(&ext_yaml),
        manifest_units: manifest.units,
        toolkit_bundled: Vec::new(),
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

/// Force the discovery banner to show whenever the walkers find
/// candidates — used by the explicit `[m] refresh discovery`
/// keybind. Unlike [`maybe_show_discovery_banner`], this **ignores**
/// the skip-marker: the user pressed the refresh key on purpose, so
/// a prior `[s] skip` should not suppress it. It also clears the
/// on-disk skip-marker so the banner keeps appearing on subsequent
/// screen-opens until the user imports or skips again.
///
/// Still respects a non-empty manifest (nothing to discover when
/// units already exist) and an already-active banner (idempotent).
pub fn force_show_discovery_banner(
    data: &mut SkillsScreenData,
    ainb_home: &Path,
    walker: WalkerOutput,
) {
    if data.banner.is_active() {
        return;
    }
    // Clear any prior skip-marker — the explicit refresh overrides it.
    let skip_path = ainb_home.join(SKIP_MARKER_FILE);
    let _ = std::fs::remove_file(&skip_path);

    let counts = compute_counts(&walker);
    let total = counts.marketplace_plugins + counts.orphan_units_total;
    tracing::info!(
        total,
        class_a = walker.class_a.len(),
        class_c = walker.class_c.len(),
        "discovery banner: forced refresh"
    );
    if total == 0 {
        return;
    }
    data.banner = DiscoveryBannerState::Visible(counts);
    data.walker_cache = Some(walker);
}

/// Apply `[Enter] import all`.
///
/// Calls the provenance-aware reconciler on the cached walker output,
/// merges the patch into the on-disk manifest, and refreshes the
/// screen view-model so the Units / Sources panels show the
/// just-imported entries.
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
    // Provenance-aware reconcile: an orphan that name-matches an
    // `agent-skills[]` entry in `external-dependencies.yaml` is
    // imported as its `gh:` upstream, not a bare `local:` orphan, so
    // the Units source column reflects its real source. With no
    // provenance data this is byte-identical to the legacy reconcile
    // (v1.2 round-trip unaffected).
    let sources = load_provenance_sources(ainb_home);
    let patch = reconcile::reconcile_with_sources(&walker, &sources);

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
        return Err(std::io::Error::other(format!("manifest save failed: {e}")));
    }

    refresh_view_model_from_manifest(data, &manifest);
    data.banner = DiscoveryBannerState::Hidden;
    Ok(())
}

/// Apply `[s] skip` — writes a marker file under `ainb_home` so
/// subsequent SkillManager opens do not re-show the banner, and
/// flips the in-memory state to `Hidden`.
pub fn apply_discovery_skip(data: &mut SkillsScreenData, ainb_home: &Path) -> std::io::Result<()> {
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
pub fn apply_conflict_flip(data: &mut SkillsScreenData, ainb_home: &Path) -> std::io::Result<()> {
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
        let target = manifest.units[sel].shadowed_by.as_ref().map(|u| u.to_string());
        target.and_then(|t| manifest.units.iter().position(|u| u.uri == t))
    } else {
        // Case B: selected is active → peer is whichever unit has
        // shadowed_by pointing back at selected.uri.
        manifest.units.iter().enumerate().find_map(|(i, u)| {
            u.shadowed_by.as_ref().filter(|x| x.to_string() == sel_uri_str).map(|_| i)
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
        // Any disk-changing action invalidates a pending remove confirm.
        self.pending_remove_confirm = None;
    }

    /// Indices into [`Self::units`] that match BOTH the active
    /// [`Self::search`] text filter AND the active [`Self::source_filter`]
    /// (every index when neither is set). Render, selection movement,
    /// and the action keybinds all resolve through this so the visible
    /// rows and the cursor stay consistent.
    ///
    /// * Source filter: `unit.source == <selected source's uri>`. This
    ///   is the canonical filter key — `UnitRow.source` is built from
    ///   the same `gh:org/repo`-style locator that `SourceRow.uri`
    ///   holds (see `unit_row_from_entry` / `refresh_view_model_from_manifest`).
    /// * Text filter: case-insensitive substring on name / source / kind.
    ///
    /// The two filters are ANDed: a source filter narrows to one
    /// source, then the search box narrows further within it.
    pub fn visible_indices(&self) -> Vec<usize> {
        let search = self.search.as_deref();
        let source = self.source_filter.as_deref();
        self.units
            .iter()
            .enumerate()
            .filter(|(_, u)| match source {
                Some(src) => unit_owning_source_uri(u) == src,
                None => true,
            })
            .filter(|(_, u)| match search {
                Some(q) => {
                    u.name.to_lowercase().contains(q)
                        || u.source.to_lowercase().contains(q)
                        || u.kind.to_lowercase().contains(q)
                }
                None => true,
            })
            .map(|(i, _)| i)
            .collect()
    }

    /// Toggle keyboard focus between the Sources and Units panels
    /// (`Tab` / `Shift-Tab`). Symmetric, so Shift-Tab routes here too.
    pub fn toggle_focus(&mut self) {
        self.focused_pane = match self.focused_pane {
            FocusedSkillPane::Sources => FocusedSkillPane::Units,
            FocusedSkillPane::Units => FocusedSkillPane::Sources,
        };
    }

    /// Move the Sources-panel cursor by one row in `dir`, wrapping at
    /// the ends. No-op when there are no sources. Does NOT apply the
    /// filter — the caller applies it on Enter / click so arrow-keying
    /// through sources stays cheap and reversible.
    pub fn move_source_selection(&mut self, dir: SelectionMove) {
        if self.sources.is_empty() {
            self.source_selected = 0;
            return;
        }
        let last = self.sources.len() - 1;
        let cur = self.source_selected.min(last);
        self.source_selected = match dir {
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
    }

    /// Apply the currently-selected Source as the Units filter and move
    /// keyboard focus to the Units panel (the user just narrowed the
    /// list and will want to navigate it). Resets the Units cursor to
    /// the first visible row so the selection never lands on a
    /// now-hidden unit. No-op when there are no sources.
    pub fn apply_selected_source_filter(&mut self) {
        let Some(src) = self.sources.get(self.source_selected) else {
            return;
        };
        self.source_filter = Some(src.uri.clone());
        self.focused_pane = FocusedSkillPane::Units;
        // Snap the Units cursor onto the first row that survives the new
        // filter so the highlight + detail pane stay consistent.
        self.selected = self.visible_indices().first().copied().unwrap_or(0);
    }

    /// Clear the active Source filter (Esc / "All sources"). Returns
    /// `true` when a filter was actually cleared, so the key handler can
    /// decide whether Esc was consumed (clear filter) or should fall
    /// through to its existing behaviour (return Home).
    pub fn clear_source_filter(&mut self) -> bool {
        if self.source_filter.take().is_some() {
            // Keep the Units cursor valid against the now-wider list.
            self.selected = self.visible_indices().first().copied().unwrap_or(0);
            true
        } else {
            false
        }
    }

    /// Grow the Sources panel by `delta` columns, clamped to the legal
    /// range for `term_w`. Used by the `]` keybind.
    pub fn grow_sources(&mut self, delta: u16, term_w: u16) {
        self.sources_width = clamp_sources_width(self.sources_width.saturating_add(delta), term_w);
    }

    /// Shrink the Sources panel by `delta` columns, clamped. Used by `[`.
    pub fn shrink_sources(&mut self, delta: u16, term_w: u16) {
        self.sources_width = clamp_sources_width(self.sources_width.saturating_sub(delta), term_w);
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
/// pane so the right-hand view stays in sync. Steps through the
/// *visible* (search-filtered) rows so the cursor never lands on a
/// hidden row. Wraps at the ends. No-op when nothing is visible.
pub fn move_selection(data: &mut SkillsScreenData, home: &Path, dir: SelectionMove) {
    let visible = data.visible_indices();
    if visible.is_empty() {
        return;
    }
    let vlast = visible.len() - 1;
    // Current cursor position within the visible slice (default to the
    // first visible row when the absolute `selected` is filtered out).
    let cur_pos = visible.iter().position(|&i| i == data.selected).unwrap_or(0);
    let new_pos = match dir {
        SelectionMove::Prev => {
            if cur_pos == 0 {
                vlast
            } else {
                cur_pos - 1
            }
        }
        SelectionMove::Next => {
            if cur_pos == vlast {
                0
            } else {
                cur_pos + 1
            }
        }
        SelectionMove::First => 0,
        SelectionMove::Last => vlast,
    };
    data.selected = visible[new_pos];
    recompute_detail(data, home);
}

/// Rebuild the Detail pane for the current `data.selected` against the
/// on-disk lockfile. Shared by [`move_selection`] and the mouse / source
/// filter handlers so the detail pane stays in sync after any change to
/// the Units cursor, without each call site re-loading the lockfile by
/// hand.
pub fn recompute_detail(data: &mut SkillsScreenData, home: &Path) {
    let lockfile = Lockfile::load_from(&lockfile_path_in(home)).unwrap_or_default();
    data.detail = compute_detail_for_selected(data, &lockfile);
}

/// Build the detail-pane content for the currently-selected unit by
/// joining the manifest's URI with the lockfile's deployment record.
/// Returns `None` when the units list is empty (the render path
/// shows its "(select a unit to see details)" placeholder).
fn compute_detail_for_selected(data: &SkillsScreenData, lockfile: &Lockfile) -> Option<UnitDetail> {
    if data.units.is_empty() {
        return None;
    }
    let idx = data.selected.min(data.units.len().saturating_sub(1));
    let row = data.units.get(idx)?;
    let manifest_uri = manifest_uri_for_row(row);
    let locked = lockfile.units.iter().find(|u| u.declared_uri == manifest_uri);
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

/// The URI of the source a unit belongs to, matching the convention of
/// [`SourceRow::uri`].
///
/// Most units encode their source as `<type>:<locator>` (e.g. a local
/// skill's source is `local:~/.claude/skills`), which is exactly what
/// [`UnitRow::source`] holds. **Marketplace units are the exception**: a
/// unit URI is `marketplace:<plugin>@<marketplace>/<path>`, so its
/// `<type>:<locator>` is `marketplace:<plugin>` — but the *source* is the
/// whole marketplace, `marketplace:<marketplace>` (the marketplace name
/// lives in the `@ref`, captured as [`UnitRow::git_ref`]). Without this
/// the Sources filter never matches a marketplace plugin's skills.
fn unit_owning_source_uri(u: &UnitRow) -> String {
    if u.source.starts_with("marketplace:") && !u.git_ref.is_empty() {
        return format!("marketplace:{}", u.git_ref);
    }
    u.source.clone()
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
            enabled: true,
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
