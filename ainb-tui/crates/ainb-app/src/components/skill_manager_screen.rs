// ABOUTME: Renderer-agnostic half of the `skill_manager_screen` component: its
// state types and the logic that does not draw. The renderer lives in
// `ainb-core::components::skill_manager_screen`, which re-exports this module.

use ainb_cli::discovery::{
    class_a, class_c,
    provenance::{ProvenanceSources, parse_external_dependencies, parse_installed_plugins},
    reconcile::{self, WalkerOutput},
};
use ainb_skill_core::drift::DriftStatus;
use ainb_skill_core::lockfile::{DeployedRef, Lockfile};
use ainb_skill_core::paths::{lockfile_path_in, manifest_path_in};
use ainb_skill_core::{Manifest, UnitEntry};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// One source row in the left panel.
#[derive(Debug, Clone)]
pub struct SourceRow {
    pub name: String,
    pub uri: String,
    /// Declared ref (branch/tag) from the manifest — `[p]` re-preview
    /// must fetch this ref, not default to `main`.
    pub r#ref: String,
    pub enabled: bool,
    /// True when this source is marked as (one of) the user's own
    /// libraries in `library.yaml` — drives the `★lib` badge and lets
    /// `[L]` toggle + `[s]` two-way-sync it back to its remote.
    pub is_library: bool,
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
    /// Source-preview picker: `Some` after an add-source fetch (or `[p]`
    /// on an existing source row). Multi-select units + target tools;
    /// nothing is persisted until Enter confirms the import.
    pub preview: Option<SourcePreviewViewState>,
    /// `Some(uri)` while a preview fetch (git clone) runs in the
    /// background — renders a "fetching…" banner and blocks a second
    /// concurrent fetch. Cleared when the fetch completes either way.
    pub preview_loading: Option<String>,
    /// Source-removal confirm dialog: `Some` after `[r]` on a source row.
    /// Offers "remove skills + source", "remove skills, keep source", and
    /// cancel — nothing is removed until a choice is confirmed.
    pub source_remove_confirm: Option<SourceRemoveConfirm>,
    /// Sync assess-then-apply dialog: `Some` after `[s]` computes a
    /// dry-run plan. Renders the planned mutations as a git-style diff;
    /// `Enter` applies, `Esc` cancels. Nothing is written until applied.
    pub sync_confirm: Option<SyncConfirmState>,
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
            preview: None,
            preview_loading: None,
            source_remove_confirm: None,
            sync_confirm: None,
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

/// Target tools offered as checkboxes in the source-preview picker, in
/// key order (`1`/`2`/`3` toggle; `4` = all). Other adapters stay
/// CLI-only (`ainb skill install --targets`).
pub const PREVIEW_TOOLS: [&str; 3] = ["claude", "codex", "copilot"];

/// Source-preview picker state: the fetched-but-not-persisted source, a
/// checkbox per discovered unit, and the target-tool checkboxes.
#[derive(Debug, Clone)]
pub struct SourcePreviewViewState {
    pub preview: ainb_cli::source::SourcePreview,
    /// One checkbox per `preview.units` entry. Pre-checked for units
    /// already installed (see [`installed`]) so the picker opens showing
    /// current state; the user toggles the rest to install more.
    pub checked: Vec<bool>,
    /// One flag per `preview.units` entry: is this unit already installed
    /// (its full URI is declared in the manifest)? Drives the "installed"
    /// badge and the pre-check above.
    pub installed: Vec<bool>,
    pub cursor: usize,
    /// claude / codex / copilot (see [`PREVIEW_TOOLS`]). Claude on by
    /// default — the primary tool this manager fronts.
    pub tools: [bool; 3],
}

impl SourcePreviewViewState {
    /// Build the picker. `installed_uris` is the set of full unit URIs
    /// (`<source>@<ref>/<path>`) already declared in the manifest, used
    /// to pre-check + badge units the user already has.
    pub fn new(
        preview: ainb_cli::source::SourcePreview,
        installed_uris: &std::collections::HashSet<String>,
    ) -> Self {
        let installed: Vec<bool> = preview
            .units
            .iter()
            .map(|u| {
                let full = format!("{}@{}/{}", preview.stored_uri, preview.r#ref, u.path);
                installed_uris.contains(&full)
            })
            .collect();
        Self {
            checked: installed.clone(),
            installed,
            cursor: 0,
            tools: [true, false, false],
            preview,
        }
    }

    pub fn move_cursor(&mut self, delta: isize) {
        let len = self.preview.units.len();
        if len == 0 {
            return;
        }
        let max = (len - 1) as isize;
        self.cursor = (self.cursor as isize + delta).clamp(0, max) as usize;
    }

    pub fn toggle_current(&mut self) {
        if let Some(c) = self.checked.get_mut(self.cursor) {
            *c = !*c;
        }
    }

    pub fn set_all(&mut self, on: bool) {
        self.checked.iter_mut().for_each(|c| *c = on);
    }

    /// Toggle tool checkbox `i` (0..3). `3` = turn all three on.
    pub fn toggle_tool(&mut self, i: usize) {
        if i == 3 {
            self.tools = [true, true, true];
        } else if let Some(t) = self.tools.get_mut(i) {
            *t = !*t;
        }
    }

    pub fn checked_count(&self) -> usize {
        self.checked.iter().filter(|c| **c).count()
    }

    /// Unit paths (relative to the source root) currently checked.
    pub fn checked_paths(&self) -> Vec<String> {
        self.preview
            .units
            .iter()
            .zip(&self.checked)
            .filter(|(_, c)| **c)
            .map(|(u, _)| u.path.clone())
            .collect()
    }

    /// Comma-separated targets for `import_selected`; `None` when no
    /// tool is checked.
    pub fn targets_csv(&self) -> Option<String> {
        let picked: Vec<&str> = PREVIEW_TOOLS
            .iter()
            .zip(&self.tools)
            .filter(|(_, on)| **on)
            .map(|(t, _)| *t)
            .collect();
        if picked.is_empty() {
            None
        } else {
            Some(picked.join(","))
        }
    }
}

/// The choice a `[r]` source-remove dialog is currently on. Ordered — the
/// render draws the options in `ALL` order and the cursor indexes into it,
/// so the picker labels and the handler both go through this one type
/// rather than agreeing on bare 0/1/2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceRemoveChoice {
    /// Uninstall every unit AND drop the source (loses the dependency).
    RemoveSkillsAndSource,
    /// Uninstall units but keep the source registered (back to preview).
    RemoveSkillsKeepSource,
    Cancel,
}

impl SourceRemoveChoice {
    /// Draw / index order.
    pub const ALL: [SourceRemoveChoice; 3] = [
        Self::RemoveSkillsAndSource,
        Self::RemoveSkillsKeepSource,
        Self::Cancel,
    ];

    /// Whether choosing this keeps the source records (units still removed).
    pub fn keeps_source(self) -> bool {
        matches!(self, Self::RemoveSkillsKeepSource)
    }
}

/// Confirm dialog shown by `[r]` on a source row.
#[derive(Debug, Clone)]
pub struct SourceRemoveConfirm {
    pub source_name: String,
    pub source_uri: String,
    /// Installed units belonging to this source (for the count shown).
    pub unit_count: usize,
    /// Index into [`SourceRemoveChoice::ALL`].
    pub cursor: usize,
}

impl SourceRemoveConfirm {
    pub fn move_cursor(&mut self, delta: isize) {
        let max = (SourceRemoveChoice::ALL.len() - 1) as isize;
        self.cursor = (self.cursor as isize + delta).clamp(0, max) as usize;
    }

    /// The currently-highlighted choice.
    pub fn choice(&self) -> SourceRemoveChoice {
        SourceRemoveChoice::ALL[self.cursor.min(SourceRemoveChoice::ALL.len() - 1)]
    }
}

/// Assess-then-apply dialog for `[s]` sync. Holds the dry-run plan text
/// (rendered as a git-style diff) and the scope that produced it so the
/// apply step re-runs the identical scope with `--yes`.
#[derive(Debug, Clone, Default)]
pub struct SyncConfirmState {
    /// What the sync is scoped to — a source name or a unit URI. Passed
    /// back verbatim as `SyncArgs.source_or_unit` on apply.
    pub target: String,
    /// Human label for the dialog title (e.g. `unit foo` / `source bar`).
    pub label: String,
    /// The dry-run plan, one line per emitted output row.
    pub plan: Vec<String>,
    /// Vertical scroll offset into [`Self::plan`].
    pub scroll: usize,
}

impl SyncConfirmState {
    pub fn scroll_by(&mut self, delta: isize) {
        let max = self.plan.len().saturating_sub(1) as isize;
        self.scroll = (self.scroll as isize + delta).clamp(0, max.max(0)) as usize;
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
            InputKind::AddSource => " Add source ",
            InputKind::Search => " Search units ",
        }
    }
}

/// Normalize a raw add-source input into a source URI.
///
/// Bare `owner/repo` (optionally `@ref`) with no scheme is GitHub
/// shorthand — prepend `gh:`. Anything already carrying a `type:`
/// scheme (`git:`, `local:`, `https:`, `gist:`, …) or not shaped like
/// `owner/repo` (local paths like `/Users/x` or `./foo`, bare single
/// names) passes through untouched so [`ainb_skill_core::Uri::parse`]
/// can validate it. Pure: no IO. Shared by the render preview and the
/// submit handler so both agree on what the user's input resolves to.
pub fn normalize_source_input(raw: &str) -> String {
    let t = raw.trim();
    // Already has a `type:` scheme, or a leading `/`/`.` (a local path
    // like `/Users/x` or `./foo`) → leave it for Uri::parse.
    if t.contains(':') || t.starts_with('/') || t.starts_with('.') {
        return t.to_string();
    }
    // GitHub shorthand is exactly `owner/repo` (or `owner/repo@ref`),
    // each segment limited to repo-name chars — so a local path
    // (`a/b/c`, bare names) is never mistaken for a repo.
    let bare = t.split('@').next().unwrap_or(t);
    let segs: Vec<&str> = bare.split('/').collect();
    let looks_like_repo = segs.len() == 2
        && segs.iter().all(|s| {
            !s.is_empty() && s.chars().all(|c| c.is_alphanumeric() || matches!(c, '.' | '_' | '-'))
        });
    if looks_like_repo {
        format!("gh:{t}")
    } else {
        t.to_string()
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

/// Marker file under `$AINB_HOME` whose presence means the user
/// pressed `[s] skip` on the discovery banner. Cleared programmatically
/// by `clear_discovery_skip_marker` on a forced re-scan.
pub const SKIP_MARKER_FILE: &str = ".discovery-skipped";

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

    refresh_view_model_from_manifest(data, &manifest, ainb_home);
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

    refresh_view_model_from_manifest(data, &manifest, ainb_home);
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

pub fn compute_counts(walker: &WalkerOutput) -> DiscoveryBannerCounts {
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
        refresh_view_model_from_manifest(&mut data, &manifest, home);
        data.detail = compute_detail_for_selected(&data, &lockfile);
        data
    }

    /// Reload manifest + lockfile and refresh the rendered rows in
    /// place. Banner state is left untouched. Used by callers that
    /// already own a `SkillsScreenData` and want to pick up
    /// out-of-band manifest mutations (e.g. after `ainb skill install`
    /// or the discovery import has rewritten disk).
    pub fn reload_from_disk(&mut self, home: &Path) {
        let manifest = Manifest::load_from(&manifest_path_in(home)).unwrap_or_default();
        let lockfile = Lockfile::load_from(&lockfile_path_in(home)).unwrap_or_default();
        refresh_view_model_from_manifest(self, &manifest, home);
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

    /// The `units` index the user actually SEES highlighted under the
    /// current filter. Render highlights `visible[position(selected) | 0]`,
    /// so keystroke actions (`[s]` sync, `[y]` copy, `[o]` open) must map
    /// `selected` through the same logic — a stale absolute `selected` that
    /// drifted out of the filtered set would act on an off-screen unit
    /// (the bug `[r]` remove already guards against). Returns `None` when
    /// nothing is visible. Callers should also assign the result back to
    /// `selected` so the detail pane + subsequent keys agree.
    pub fn highlighted_unit_index(&self) -> Option<usize> {
        let visible = self.visible_indices();
        if visible.is_empty() {
            return None;
        }
        let pos = visible.iter().position(|&i| i == self.selected).unwrap_or(0);
        Some(visible[pos])
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
fn refresh_view_model_from_manifest(data: &mut SkillsScreenData, manifest: &Manifest, home: &Path) {
    // Which sources the user has marked as their own library
    // (`library.yaml`). Missing / malformed file → nothing marked.
    let lib = ainb_skill_core::library::Library::load_from(
        &ainb_skill_core::library::library_path_in(home),
    )
    .unwrap_or_default();
    data.sources = manifest
        .sources
        .iter()
        .map(|s| SourceRow {
            name: s.name.clone(),
            uri: s.uri.clone(),
            r#ref: s.r#ref.clone(),
            enabled: s.enabled,
            is_library: lib.is_library_source(&s.name),
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
