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

use std::sync::Arc;
use std::sync::mpsc::{self, Receiver};

use async_trait::async_trait;

use ainb_plugin_sdk::{
    Cell, CliOutput, Color, Coord, HandleKeyParams, HandleMouseParams, HostClient, InitContext,
    KeyCode, MouseButton, MouseKind, Plugin, RenderParams, Result, WireBuffer, topics,
};
use ratatui::buffer::Buffer as RBuffer;
use ratatui::layout::Rect as RRect;
use ratatui::style::{Color as RColor, Modifier as RModifier};

use crate::config::LearningsConfig;
use crate::data::{
    DataError, QmdCli, QmdSearch, SearchCancel, SearchHit, SearchMode, parse_community_reports,
    resolve_config_path, scan_learnings_dir_report, search_cancellable,
};
use crate::ui::{LearningsUi, SearchContext, SearchRequest, SearchStage, render as render_ui};

/// One STAGE of a two-stage worker result handed back over the channel: the
/// submit `token` it answers, which `stage` (BM25 fast paint vs semantic rerank)
/// produced it, and the parsed hits (or a typed error the UI degrades to empty).
type SearchOutcome = (
    u64,
    SearchStage,
    std::result::Result<Vec<SearchHit>, DataError>,
);

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
    /// Injected `qmd` runner — `QmdCli` in production, a fake in tests. Held
    /// behind an `Arc` so the search worker thread can clone a handle and run the
    /// (slow) subprocess OFF the dispatch thread without borrowing `self`.
    search_runner: Arc<dyn QmdSearch + Send + Sync>,
    /// Receiver for the in-flight search worker's result, `Some` only while a
    /// search is running. The plugin polls it (non-blocking) each render and
    /// drops it once the result is applied / the search times out / is
    /// superseded.
    search_rx: Option<Receiver<SearchOutcome>>,
    /// Kill handle for the in-flight worker's `qmd` children, `Some` only
    /// while a search is running. Cancelling SIGKILLs + reaps the live child
    /// and makes the worker skip its remaining stage. Replaced on every submit
    /// (kill-before-spawn: at most ONE live qmd lineage at a time).
    search_cancel: Option<Arc<SearchCancel>>,
    /// `true` once the Graph tab's community clusters have been lazily loaded
    /// from `graph_cache` (memoized so re-entry doesn't re-read).
    communities_loaded: bool,
    /// Freshness witness, bumped once per state-mutating `handle_key`.
    generation: u64,
}

impl Default for LearningsPlugin {
    fn default() -> Self {
        Self::with_search_runner(Arc::new(QmdCli::default()))
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
    ///
    /// The runner is wrapped in an [`Arc`] so the search worker thread can share
    /// it (run the subprocess off the dispatch thread). Fields are spelled out
    /// (no `..Self::default()` update) because the type implements `Drop` —
    /// functional record update can't move out of a `Drop` type (E0509).
    #[must_use]
    pub fn with_search_runner(runner: Arc<dyn QmdSearch + Send + Sync>) -> Self {
        Self {
            config: LearningsConfig::default(),
            ui: LearningsUi::default(),
            search_runner: runner,
            search_rx: None,
            search_cancel: None,
            communities_loaded: false,
            generation: 0,
        }
    }

    /// Host-free body of the `learnings` CLI namespace. Synchronous — it
    /// shells `qmd` once (no two-stage worker / cancellation; this is a
    /// one-shot process). Public (doc-hidden) so tests can drive it with a
    /// fake [`QmdSearch`] runner.
    ///
    /// Surface: `learnings search <query...> [--bm25] [-k N] [--format json]`.
    #[doc(hidden)]
    pub fn cli_dispatch_core(&self, namespace: &str, argv: &[String]) -> CliOutput {
        const USAGE: &str =
            "usage: ainb learnings search <query...> [--bm25] [-k N] [--format json]\n";
        if namespace != "learnings" {
            return CliOutput {
                stdout: Vec::new(),
                stderr: format!("learnings: unknown namespace `{namespace}`\n").into_bytes(),
                exit_code: 2,
            };
        }

        let mut json = false;
        let mut bm25 = false;
        let mut limit = 10usize;
        let mut rest: Vec<&str> = Vec::new();
        let mut it = argv.iter();
        while let Some(a) = it.next() {
            match a.as_str() {
                "--format" => {
                    json = it.next().map(String::as_str) == Some("json");
                }
                "--bm25" => bm25 = true,
                "-k" | "--limit" => match it.next().map(|s| s.parse::<usize>()) {
                    Some(Ok(n)) => limit = n,
                    _ => {
                        return CliOutput {
                            stdout: Vec::new(),
                            stderr: format!("learnings: -k/--limit needs a number\n{USAGE}")
                                .into_bytes(),
                            exit_code: 2,
                        };
                    }
                },
                "-h" | "--help" => return CliOutput::ok(USAGE),
                other => rest.push(other),
            }
        }

        if rest.first().copied() != Some("search") {
            return CliOutput {
                stdout: Vec::new(),
                stderr: USAGE.as_bytes().to_vec(),
                exit_code: 2,
            };
        }
        let query = rest[1..].join(" ");
        if query.trim().is_empty() {
            return CliOutput {
                stdout: Vec::new(),
                stderr: USAGE.as_bytes().to_vec(),
                exit_code: 2,
            };
        }

        let mode = if bm25 {
            SearchMode::Bm25
        } else {
            SearchMode::Semantic
        };
        match search_cancellable(
            self.search_runner.as_ref(),
            &query,
            &self.config.qmd_collection,
            &self.config.qmd_index,
            mode,
            &SearchCancel::new(),
        ) {
            Ok(hits) => {
                let hits: Vec<SearchHit> = hits.into_iter().take(limit).collect();
                if json {
                    let arr: Vec<_> = hits
                        .iter()
                        .map(|h| {
                            serde_json::json!({
                                "id": h.id,
                                "score": h.score,
                                "title": h.title,
                                "file": h.file,
                            })
                        })
                        .collect();
                    let body = serde_json::to_string_pretty(&arr).unwrap_or_else(|_| "[]".into());
                    CliOutput::ok(format!("{body}\n").into_bytes())
                } else if hits.is_empty() {
                    CliOutput::ok(b"no results\n".to_vec())
                } else {
                    let mut out = String::new();
                    for h in &hits {
                        out.push_str(&format!("{:>7.2}  {}  {}\n", h.score, h.id, h.title));
                    }
                    CliOutput::ok(out.into_bytes())
                }
            }
            Err(e) => CliOutput::err(format!("learnings search failed: {e}\n").into_bytes()),
        }
    }

    /// Spawn the TWO-STAGE `qmd` search for `request` on a dedicated worker
    /// thread and arm the result channel. Runtime-agnostic: uses `std::thread` +
    /// `mpsc` (the in-module + integration tests call into the plugin without
    /// guaranteeing a tokio runtime on the calling thread, so `spawn_blocking` is
    /// unsafe here).
    ///
    /// **Kill-before-spawn.** The previous lineage is cancelled FIRST — its
    /// cancelled flag is set and its live `qmd` child is SIGKILLed + reaped —
    /// so at most ONE qmd lineage is alive at a time. Without this, rapid
    /// edit+Enter cycles pile up N concurrent `qmd` processes whose results
    /// are all discarded by token mismatch.
    ///
    /// The single worker runs `qmd` twice, both tagged with the SAME `token`:
    ///
    /// 1. **BM25** (`run_bm25` / `qmd search --json`, no LLM — effectively
    ///    instant): sent first for the fast first paint.
    /// 2. **Semantic** (`run_query` / `qmd query --json -C 20`, the LLM rerank):
    ///    sent second to swap in the reranked results — skipped entirely when
    ///    the lineage was cancelled between the stages.
    ///
    /// The plugin polls the receiver each render and applies each stage in turn
    /// ([`Self::poll_search`]); the channel is kept until the SEMANTIC stage
    /// lands (or the ceiling fires), so both stages reach the UI.
    fn start_search_worker(&mut self, request: SearchRequest) {
        self.cancel_in_flight_search();
        let cancel = Arc::new(SearchCancel::new());
        self.search_cancel = Some(Arc::clone(&cancel));
        let (tx, rx) = mpsc::channel::<SearchOutcome>();
        let runner = Arc::clone(&self.search_runner);
        let collection = self.config.qmd_collection.clone();
        let index = self.config.qmd_index.clone();
        let SearchRequest { token, query } = request;
        std::thread::spawn(move || {
            // Stage 1 — BM25 fast paint (no LLM, returns ~instantly).
            let bm25 = search_cancellable(
                runner.as_ref(),
                &query,
                &collection,
                &index,
                SearchMode::Bm25,
                &cancel,
            );
            // A failed send means the receiver is already gone (superseded /
            // timed-out search) — bail without running the slow semantic pass.
            if tx.send((token, SearchStage::Bm25, bm25)).is_err() {
                return;
            }
            // Superseded between the stages? Skip the slow semantic pass
            // entirely — its result would be dropped by token mismatch anyway.
            if cancel.is_cancelled() {
                return;
            }
            // Stage 2 — semantic rerank (the slow LLM path).
            let semantic = search_cancellable(
                runner.as_ref(),
                &query,
                &collection,
                &index,
                SearchMode::Semantic,
                &cancel,
            );
            let _ = tx.send((token, SearchStage::Semantic, semantic));
        });
        self.search_rx = Some(rx);
    }

    /// Tear down the in-flight search worker, if any: set its cancelled flag,
    /// SIGKILL + reap its live `qmd` child, and drop the result channel. The
    /// worker thread observes the cancelled flag / dead channel and exits
    /// instead of running (or finishing) the slow semantic stage.
    fn cancel_in_flight_search(&mut self) {
        if let Some(cancel) = self.search_cancel.take() {
            cancel.cancel();
        }
        self.search_rx = None;
    }

    /// Poll the in-flight two-stage search worker (non-blocking) and enforce the
    /// timeout. Called at the top of every render so a settled stage paints on
    /// the next frame and a hung search is abandoned at the ceiling. Returns
    /// `true` if any stage / the timeout applied a state change (so the caller
    /// bumps generation).
    fn poll_search(&mut self) -> bool {
        let mut changed = false;
        // 1. Drain every stage result that landed this frame (BM25 then
        // semantic) — both may arrive between two renders. The channel is
        // retained until the SEMANTIC stage is applied, so the BM25 fast paint
        // doesn't prematurely close it.
        while let Some(rx) = self.search_rx.as_ref() {
            match rx.try_recv() {
                Ok((token, stage, result)) => {
                    changed |= self.ui.apply_search_result(token, stage, result);
                    // The semantic stage is the worker's last message — drop the
                    // channel (and the now-finished lineage's kill handle) once
                    // it's been delivered.
                    if stage == SearchStage::Semantic {
                        self.search_rx = None;
                        self.search_cancel = None;
                    }
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    // Worker ended without sending the remaining stage(s) — drop
                    // the dead channel (the UI keeps whatever already painted).
                    self.search_rx = None;
                    self.search_cancel = None;
                }
            }
        }
        // 2. Enforce the ceiling on whatever's still in flight (covers the WHOLE
        // two-stage; if BM25 painted, the UI keeps those results on timeout).
        if self.ui.check_search_timeout() {
            changed = true;
            // Kill the hung `qmd` child + drop the channel — the worker then
            // exits instead of burning CPU on a result nobody will read.
            self.cancel_in_flight_search();
        }
        changed
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

impl Drop for LearningsPlugin {
    /// Kill any in-flight `qmd` child on plugin teardown — a hung search must
    /// not outlive its owner (and tests must not leak sleeper children).
    fn drop(&mut self) {
        self.cancel_in_flight_search();
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

    /// Headless `ainb learnings search <query>` — runs the same `qmd` search
    /// the TUI Search tab uses and prints ranked hits as text or JSON.
    async fn cli_dispatch(
        &mut self,
        _host: &HostClient,
        namespace: &str,
        argv: &[String],
    ) -> Result<CliOutput> {
        Ok(self.cli_dispatch_core(namespace, argv))
    }

    async fn render(&mut self, _host: &HostClient, params: RenderParams) -> Result<WireBuffer> {
        // Poll the search worker + enforce its timeout BEFORE painting so a
        // settled (or timed-out) search shows on THIS frame, not the next. The
        // generation bump keeps the host's freshness witness honest when a
        // result lands between key presses (driven by `wants_redraw` ticks).
        if self.poll_search() {
            self.generation = self.generation.wrapping_add(1);
        }

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

    /// Route a forwarded key into the UI. The host forwards every non-reserved
    /// key here (Esc included — it is plugin-owned, not host-reserved). The UI
    /// pops its own internal levels (detail drawer, picker, focused graph); at
    /// the root view, an unconsumed Esc must close the screen, so we publish
    /// `ui.close_request` for the host to pop back to wherever the panel was
    /// opened from. Without it the user is trapped on the panel (only Ctrl+C
    /// quits). Mirrors the burndown plugin's root-Esc path.
    async fn handle_key(&mut self, host: &HostClient, params: HandleKeyParams) -> Result<()> {
        // Build the Search context fresh from the resolved config: the
        // collection/index the Search tab threads into its query. The `qmd`
        // runner is NOT in the context — a search submit returns a request and
        // the plugin runs the runner on a worker thread (so this dispatch thread
        // never blocks on the slow subprocess).
        let ctx = SearchContext {
            collection: &self.config.qmd_collection,
            index: &self.config.qmd_index,
        };
        let outcome = self.ui.handle_key(&params.key.code, &ctx);
        if outcome.changed {
            self.generation = self.generation.wrapping_add(1);
        }
        // Root-level Esc: the UI consumed nothing (`changed == false`), so there
        // was no inner level to pop — ask the host to close this screen. Best
        // effort; if the publish is lost the user just presses Esc again.
        if matches!(params.key.code, KeyCode::Esc) && !outcome.changed {
            let req = topics::UiCloseRequest {
                screen_id: params.screen_id.clone(),
            };
            let payload = serde_json::to_vec(&req).unwrap_or_default();
            let _ = host.snapshot_publish(topics::UI_CLOSE_REQUEST, payload).await;
        }
        // A fresh Search submit: kick the `qmd` worker OFF this thread. The
        // result is polled back in on the next render.
        if let Some(request) = outcome.start_search {
            self.start_search_worker(request);
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

    #[test]
    fn cli_dispatch_core_search_text_json_and_errors() {
        // Fake runner returns a canned `qmd query --json` payload.
        struct Fake;
        impl QmdSearch for Fake {
            fn run_query(
                &self,
                _q: &str,
                _c: &str,
                _i: &str,
            ) -> std::result::Result<String, DataError> {
                Ok(
                    r##"[{"docid":"#42","score":2.5,"title":"Redis pooling","file":"qmd://x"}]"##
                        .to_string(),
                )
            }
        }
        let p = LearningsPlugin::with_search_runner(Arc::new(Fake));
        let args = |s: &str| s.split_whitespace().map(String::from).collect::<Vec<_>>();

        // text mode → title line, exit 0
        let out = p.cli_dispatch_core("learnings", &args("search redis pooling"));
        assert_eq!(out.exit_code, 0);
        assert!(String::from_utf8_lossy(&out.stdout).contains("Redis pooling"));

        // json mode → parseable array carrying the docid, exit 0
        let out = p.cli_dispatch_core("learnings", &args("search redis --format json"));
        assert_eq!(out.exit_code, 0);
        let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("valid json");
        assert_eq!(v[0]["id"], "#42");

        // missing verb → usage error, exit 2
        assert_eq!(p.cli_dispatch_core("learnings", &args("")).exit_code, 2);
        // wrong namespace → exit 2
        assert_eq!(p.cli_dispatch_core("nope", &args("search x")).exit_code, 2);
    }

    // ---- qmd child kill: timeout + kill-before-spawn ----

    use std::path::Path;
    use std::time::{Duration, Instant};

    use ainb_plugin_sdk::KeyCode;

    use crate::data::test_proc::{pid_alive, write_script};

    /// A plugin whose runner shells `script` as the `qmd` binary — the "slow
    /// qmd" fixture path (a real subprocess, not a fake).
    fn plugin_with_script(script: &Path) -> LearningsPlugin {
        LearningsPlugin::with_search_runner(Arc::new(QmdCli::with_binary(
            script.display().to_string(),
        )))
    }

    /// Drive the UI to a submitted query (`/` focus, type, Enter) and kick the
    /// REAL worker thread, mirroring `handle_key`'s submit path.
    fn submit_search(p: &mut LearningsPlugin, query: &str) {
        let ctx = SearchContext {
            collection: "",
            index: "",
        };
        p.ui.handle_key(&KeyCode::Char { ch: '/' }, &ctx);
        for ch in query.chars() {
            p.ui.handle_key(&KeyCode::Char { ch }, &ctx);
        }
        let out = p.ui.handle_key(&KeyCode::Enter, &ctx);
        p.start_search_worker(out.start_search.expect("submit yields a worker request"));
    }

    /// Bounded-poll until the in-flight worker registers its `qmd` child with
    /// the CURRENT cancel handle, returning the child's pid.
    fn wait_for_child_pid(p: &LearningsPlugin) -> u32 {
        let cancel = p.search_cancel.as_ref().expect("in-flight cancel handle");
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(pid) = cancel.child_id() {
                return pid;
            }
            assert!(
                Instant::now() < deadline,
                "qmd child never spawned/registered"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// TIMEOUT KILL: when the search ceiling fires, `poll_search` must SIGKILL
    /// the in-flight `qmd` child — not merely drop the channel and leave an
    /// orphan burning CPU on a result nobody will read.
    #[test]
    fn timeout_kills_the_in_flight_qmd_child() {
        let dir = tempfile::tempdir().expect("tempdir");
        let script = write_script(dir.path(), "slow-qmd.sh", "sleep 30");
        let mut p = plugin_with_script(&script);
        submit_search(&mut p, "slow");

        let pid = wait_for_child_pid(&p);
        assert!(
            pid_alive(pid),
            "precondition: the slow qmd child is running"
        );

        // Force the in-flight phase past the ceiling (the same seam the UI's
        // own timeout tests use), then poll — the timeout arm must kill.
        p.ui.force_search_timeout_eligible();
        assert!(p.poll_search(), "the timeout must register a state change");
        assert!(
            p.search_rx.is_none(),
            "the worker channel is dropped on timeout"
        );
        assert!(
            p.search_cancel.is_none(),
            "the kill handle is cleared on timeout"
        );
        assert!(
            !pid_alive(pid),
            "the qmd child must be SIGKILLed + reaped when the search times out"
        );
    }

    /// KILL-BEFORE-SPAWN: a new submit must kill the superseded query's `qmd`
    /// child BEFORE spawning its own, bounding the system to at most ONE live
    /// qmd lineage no matter how fast the user edit+Enter cycles.
    #[test]
    fn new_submit_kills_the_superseded_querys_qmd_child() {
        let dir = tempfile::tempdir().expect("tempdir");
        let script = write_script(dir.path(), "slow-qmd.sh", "sleep 30");
        let mut p = plugin_with_script(&script);

        // Submit #1 → its slow child spawns.
        submit_search(&mut p, "one");
        let pid1 = wait_for_child_pid(&p);
        assert!(pid_alive(pid1), "precondition: query #1's child is running");

        // Edit + re-submit immediately (the rapid edit+Enter cycle).
        let ctx = SearchContext {
            collection: "",
            index: "",
        };
        p.ui.handle_key(&KeyCode::Char { ch: '2' }, &ctx);
        let out = p.ui.handle_key(&KeyCode::Enter, &ctx);
        p.start_search_worker(out.start_search.expect("re-submit yields a request"));

        // Kill-before-spawn: query #1's child is ALREADY dead when the new
        // worker starts (the cancel runs synchronously before the spawn).
        assert!(
            !pid_alive(pid1),
            "the superseded query's qmd child must be killed before the new spawn"
        );

        // Query #2 proceeds: its own child spawns under the NEW kill handle.
        let pid2 = wait_for_child_pid(&p);
        assert_ne!(pid1, pid2, "query #2 runs its own child");
        assert!(pid_alive(pid2), "the live query's child keeps running");

        // Teardown kills the live child too — no 30 s sleeper outlives the
        // test (the Drop impl cancels the in-flight lineage).
        drop(p);
        assert!(!pid_alive(pid2), "drop must kill the remaining child");
    }
}
