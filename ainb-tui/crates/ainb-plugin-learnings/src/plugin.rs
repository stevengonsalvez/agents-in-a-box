//! ABI v2 [`Plugin`] implementation for the learnings browser.
//!
//! P5 replaces the P3 empty-state shell with the tabbed Browse UI: on
//! `plugin/init` the plugin scans the configured `learnings_dir` for `.md`
//! records (tolerating non-record + corrupt notes per the data layer), loads
//! them into the [`LearningsUi`] view, and renders a tabbed shell with all
//! three tabs live: Browse (list + filter chips + detail), Search (`qmd`
//! query + ranked results), and Graph (typed entity neighbourhood from the
//! `.entities.yaml` relationships + community clusters).
//!
//! Render mirrors burndown: paint locally into a ratatui `Buffer`, then
//! convert to a sparse [`WireBuffer`] cell stream for the host
//! ([`buffer_to_wire`]). The host re-paints each cell at its `(x, y)`.

use async_trait::async_trait;

use ainb_plugin_sdk::{
    Cell, Color, Coord, HandleKeyParams, HandleMouseParams, HostClient, InitContext, MouseButton,
    MouseKind, Plugin, RenderParams, Result, WireBuffer,
};
use ratatui::buffer::Buffer as RBuffer;
use ratatui::layout::Rect as RRect;
use ratatui::style::{Color as RColor, Modifier as RModifier};

use crate::config::LearningsConfig;
use crate::data::{
    QmdCli, QmdSearch, parse_community_reports, resolve_config_path, scan_learnings_dir_report,
};
use crate::ui::{LearningsUi, SearchContext, render as render_ui};

/// Static manifest TOML compiled into the binary. The SDK `Server` uses
/// this on `plugin/init` to echo `name`/`version` back to the host.
const MANIFEST_TOML: &str = include_str!("../manifest.toml");

/// nano_graphrag community-reports filename inside `graph_cache`. The Graph
/// tab's community-cluster view parses this JSON ([`parse_community_reports`]).
const COMMUNITY_REPORTS_FILE: &str = "kv_store_community_reports.json";

/// Title token painted in the panel header. Unique + stable so the
/// tmux-ui-tripwire can assert an exact match (never a substring-OR). The UI
/// shell pads this (` 🧠 Learnings `); this bare token is the substring the
/// P3 tripwire asserts, kept here so a UI-side rename is caught by the
/// scaffold test too.
pub const TITLE_TOKEN: &str = "🧠 Learnings";

/// Fallback viewport if the host sends a degenerate 0×0 — matches the
/// historical 80×24 baseline used across the in-tree plugins.
const FALLBACK_VIEWPORT: (u16, u16) = (80, 24);

/// Learnings plugin state.
///
/// Holds the resolved [`LearningsConfig`] (injected at `plugin/init`) plus the
/// [`LearningsUi`] view populated from the scanned KB. A render generation
/// counter is bumped on every state-mutating key so the host's next render
/// sees fresh state.
///
/// The `qmd` search runner is held behind a [`QmdSearch`] trait object so it's
/// injectable: production uses [`QmdCli`] (the default), and tests pass a fake
/// returning a captured payload via [`Self::with_search_runner`] so the Search
/// tab's ranked-result rendering is deterministic without spawning `qmd`.
pub struct LearningsPlugin {
    /// Resolved config injected at `plugin/init`.
    config: LearningsConfig,
    /// Tabbed UI view (records + per-tab state).
    ui: LearningsUi,
    /// Injected `qmd` runner — `QmdCli` in production, a fake in tests.
    search_runner: Box<dyn QmdSearch + Send + Sync>,
    /// `true` once the Graph tab's community clusters have been lazily loaded
    /// from `graph_cache` (memoized so re-entry doesn't re-read).
    communities_loaded: bool,
    /// Freshness witness, bumped once per state-mutating `handle_key`.
    generation: u64,
}

impl Default for LearningsPlugin {
    fn default() -> Self {
        Self {
            config: LearningsConfig::default(),
            ui: LearningsUi::default(),
            search_runner: Box::new(QmdCli::default()),
            communities_loaded: false,
            generation: 0,
        }
    }
}

impl std::fmt::Debug for LearningsPlugin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The boxed trait object isn't `Debug`; omit it (it carries no state
        // worth printing — just the binary name).
        f.debug_struct("LearningsPlugin")
            .field("config", &self.config)
            .field("ui", &self.ui)
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}

impl LearningsPlugin {
    /// Construct with an injected [`QmdSearch`] runner. Production builds use
    /// [`Self::default`] (the real [`QmdCli`]); tests pass a fake so the Search
    /// tab renders ranked results deterministically without a live `qmd` index.
    #[must_use]
    pub fn with_search_runner(runner: Box<dyn QmdSearch + Send + Sync>) -> Self {
        Self {
            search_runner: runner,
            ..Self::default()
        }
    }

    /// Borrow the resolved config. Used by tests to assert the parse landed.
    #[must_use]
    pub const fn config(&self) -> &LearningsConfig {
        &self.config
    }

    /// Borrow the UI view (for tests).
    #[must_use]
    pub const fn ui(&self) -> &LearningsUi {
        &self.ui
    }

    /// Replace the resolved config from an injected
    /// `PluginInitParams.config` JSON value, then (re)scan the KB into the UI.
    ///
    /// Kept as a `&mut self` method so unit tests can drive it without the SDK
    /// `Server`. A scan error (e.g. the configured dir does not exist) leaves
    /// the UI empty rather than failing init — the Browse list then shows its
    /// empty state, which is the correct user-visible behaviour for a missing
    /// KB.
    pub fn apply_init_config(&mut self, value: &serde_json::Value) {
        self.config = LearningsConfig::from_init_config(value);
        self.rescan();
    }

    /// Scan the configured `learnings_dir` and load the records + corrupt-note
    /// count into the UI. Best-effort: a read error logs and leaves the view
    /// empty.
    ///
    /// The Graph tab's community clusters are NOT loaded here — they're loaded
    /// lazily the first time the user reaches the Graph tab
    /// ([`Self::ensure_communities_loaded`]). Lazy-loading keeps the `graph_cache`
    /// read off the init path so a session that never opens the Graph tab never
    /// touches the (potentially large) nano_graphrag cache.
    fn rescan(&mut self) {
        let dir = resolve_config_path(&self.config.learnings_dir);
        match scan_learnings_dir_report(&dir) {
            Ok(report) => {
                self.ui.set_records(report.records, report.failed_count);
            }
            Err(err) => {
                tracing::warn!(
                    dir = %dir.display(),
                    %err,
                    "learnings scan failed — Browse will show the empty state"
                );
                self.ui.set_records(Vec::new(), 0);
            }
        }
    }

    /// Load the Graph tab's community clusters from
    /// `<graph_cache>/kv_store_community_reports.json` the first time the user
    /// reaches the Graph tab, then memoize (`communities_loaded`) so a later
    /// re-entry doesn't re-read.
    ///
    /// Best-effort: a missing or malformed file logs and leaves the community
    /// view in its empty state (the correct user-visible behaviour for a KB
    /// without a built graph). Lazy by design so the `graph_cache` read only
    /// happens for a user who actually opens the Graph tab.
    fn ensure_communities_loaded(&mut self) {
        if self.communities_loaded {
            return;
        }
        self.communities_loaded = true;
        let path = resolve_config_path(&self.config.graph_cache).join(COMMUNITY_REPORTS_FILE);
        match parse_community_reports(&path) {
            Ok(communities) => self.ui.set_communities(communities),
            Err(err) => {
                tracing::warn!(
                    path = %path.display(),
                    %err,
                    "community reports unreadable — Graph community view will be empty"
                );
                self.ui.set_communities(Vec::new());
            }
        }
    }
}

#[async_trait]
impl Plugin for LearningsPlugin {
    fn manifest(&self) -> &'static str {
        MANIFEST_TOML
    }

    /// One-shot init: parse the injected `[plugins.learnings]` table into the
    /// typed config, then scan the KB into the Browse view.
    async fn on_init(&mut self, _host: &HostClient, ctx: InitContext<'_>) -> Result<()> {
        self.apply_init_config(ctx.config);
        Ok(())
    }

    async fn render(&mut self, _host: &HostClient, params: RenderParams) -> Result<WireBuffer> {
        let (w, h) = match (params.viewport.width, params.viewport.height) {
            (0, _) | (_, 0) => FALLBACK_VIEWPORT,
            (w, h) => (w, h),
        };
        let area = RRect {
            x: 0,
            y: 0,
            width: w,
            height: h,
        };
        let mut rbuf = RBuffer::empty(area);
        render_ui(&mut rbuf, area, &self.ui);
        let wire = buffer_to_wire(&rbuf, area);
        // Advance the map's recentre animation for the NEXT frame. This frame
        // was painted at the current scale; ticking here (post-paint, under the
        // plugin's &mut self) drives the self-animation via `wants_redraw`.
        self.ui.tick_map_animation();
        Ok(wire)
    }

    /// Route a forwarded key into the UI. The host reserves global nav (Esc,
    /// etc.) and only forwards keys it hasn't consumed, so a no-op here for an
    /// unhandled key is correct.
    async fn handle_key(&mut self, _host: &HostClient, params: HandleKeyParams) -> Result<()> {
        // Build the Search context fresh from the resolved config: the injected
        // `qmd` runner + the collection/index the Search tab queries against.
        let ctx = SearchContext {
            runner: self.search_runner.as_ref(),
            collection: &self.config.qmd_collection,
            index: &self.config.qmd_index,
        };
        if self.ui.handle_key(&params.key.code, &ctx) {
            self.generation = self.generation.wrapping_add(1);
        }
        // Lazily load the Graph tab's community clusters the first time the user
        // reaches the Graph tab (keeps the `graph_cache` read off init + off any
        // session that never opens the graph).
        if self.ui.tab() == crate::ui::Tab::Graph {
            self.ensure_communities_loaded();
        }
        Ok(())
    }

    /// Route a forwarded mouse click into the radial map. Only a left-button
    /// press acts (select / recentre the clicked node); other events are
    /// ignored. Coordinates arrive in the plugin's own viewport space.
    async fn handle_mouse(&mut self, _host: &HostClient, params: HandleMouseParams) -> Result<()> {
        if matches!(
            params.mouse.kind,
            MouseKind::Down {
                button: MouseButton::Left
            }
        ) && self.ui.handle_mouse(params.mouse.col, params.mouse.row)
        {
            self.generation = self.generation.wrapping_add(1);
        }
        Ok(())
    }

    /// Surface the map's recentre animation to the host: while the transition is
    /// running, ask to be rendered again next tick without further input.
    fn wants_redraw(&self) -> bool {
        self.ui.wants_redraw()
    }
}

/// Convert a ratatui `Buffer` to the SDK's sparse [`WireBuffer`] cell
/// stream. Row-major so the `Vec<(Coord, Cell)>` is deterministic;
/// fully-blank default cells are dropped to keep the wire payload sparse.
/// Mirrors burndown's `buffer_to_wire`.
fn buffer_to_wire(rbuf: &RBuffer, area: RRect) -> WireBuffer {
    let mut wire = WireBuffer::new(area.width, area.height);
    for y in 0..area.height {
        for x in 0..area.width {
            let cell = rbuf.get(area.x + x, area.y + y);
            let symbol = cell.symbol();
            let fg = ratatui_color(cell.fg);
            let bg = ratatui_color(cell.bg);
            let modifier = ratatui_modifiers(cell.modifier);
            if symbol == " " && fg.is_none() && bg.is_none() && modifier == 0 {
                continue;
            }
            wire.push(
                Coord::new(x, y),
                Cell {
                    symbol: symbol.to_string(),
                    fg,
                    bg,
                    modifier,
                },
            );
        }
    }
    wire
}

fn ratatui_color(c: RColor) -> Option<Color> {
    match c {
        RColor::Reset => None,
        RColor::Black => Some(Color::rgb(0, 0, 0)),
        RColor::Red => Some(Color::rgb(170, 0, 0)),
        RColor::Green => Some(Color::rgb(0, 170, 0)),
        RColor::Yellow => Some(Color::rgb(170, 170, 0)),
        RColor::Blue => Some(Color::rgb(0, 0, 170)),
        RColor::Magenta => Some(Color::rgb(170, 0, 170)),
        RColor::Cyan => Some(Color::rgb(0, 170, 170)),
        RColor::Gray => Some(Color::rgb(170, 170, 170)),
        RColor::DarkGray => Some(Color::rgb(85, 85, 85)),
        RColor::LightRed => Some(Color::rgb(255, 85, 85)),
        RColor::LightGreen => Some(Color::rgb(85, 255, 85)),
        RColor::LightYellow => Some(Color::rgb(255, 255, 85)),
        RColor::LightBlue => Some(Color::rgb(85, 85, 255)),
        RColor::LightMagenta => Some(Color::rgb(255, 85, 255)),
        RColor::LightCyan => Some(Color::rgb(85, 255, 255)),
        RColor::White => Some(Color::rgb(255, 255, 255)),
        RColor::Indexed(_) => None,
        RColor::Rgb(r, g, b) => Some(Color::rgb(r, g, b)),
    }
}

fn ratatui_modifiers(m: RModifier) -> u16 {
    let mut out = 0_u16;
    if m.contains(RModifier::BOLD) {
        out |= 1;
    }
    if m.contains(RModifier::DIM) {
        out |= 2;
    }
    if m.contains(RModifier::ITALIC) {
        out |= 4;
    }
    if m.contains(RModifier::UNDERLINED) {
        out |= 8;
    }
    if m.contains(RModifier::REVERSED) {
        out |= 16;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The UI shell paints the padded title token in the panel header. The
    /// bare `TITLE_TOKEN` (`🧠 Learnings`) must appear in the first row.
    #[test]
    fn render_paints_title_into_buffer() {
        let area = RRect {
            x: 0,
            y: 0,
            width: 40,
            height: 8,
        };
        let mut rbuf = RBuffer::empty(area);
        let ui = LearningsUi::default();
        render_ui(&mut rbuf, area, &ui);
        let wire = buffer_to_wire(&rbuf, area);
        let row0: String = wire
            .cells
            .iter()
            .filter(|(c, _)| c.y == 0)
            .map(|(_, cell)| cell.symbol.as_str())
            .collect();
        assert!(
            row0.contains(TITLE_TOKEN),
            "title row must contain the {TITLE_TOKEN:?} token, got: {row0:?}"
        );
    }

    #[test]
    fn apply_init_config_updates_state() {
        let mut p = LearningsPlugin::default();
        p.apply_init_config(&serde_json::json!({ "learnings_dir": "/kb" }));
        assert_eq!(p.config().learnings_dir, "/kb");
    }
}
