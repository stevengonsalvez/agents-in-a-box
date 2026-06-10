//! Search tab — live query box + ranked `qmd` results (P7).
//!
//! The Search tab is a two-mode view:
//!
//! - **Query input** (`/` focuses it): a single-line box bearing the `search:`
//!   prompt. Printable chars append to the query; `Backspace` edits it; `Enter`
//!   submits.
//! - **Results**: the ranked [`SearchHit`]s the `qmd` runner returned, rendered
//!   as `id · title · score` rows (rank order = the order the runner returned,
//!   which `qmd` sorts by score). `↑↓` move a `▶` selection; `Enter` on a
//!   result resolves the hit back to a [`LearningRecord`] and hands it to the
//!   shell to open the SAME Detail pane (P6). Selection uses the arrow keys
//!   only — `j`/`k` are reserved for typing into the query box (a user must be
//!   able to search for `jwt`, `kafka`, etc.).
//!
//! **Non-blocking submit (the load-bearing invariant).** `qmd` is slow — the
//! semantic `query` path runs LLM query-expansion (cold ~14 s) and can hang
//! indefinitely. The plugin's SDK dispatches `handle_key` INLINE on its single
//! reader-loop task, so it must never block: a synchronous subprocess in submit
//! would freeze the whole Learnings pane until `qmd` returns. So submit does NOT
//! run the subprocess — it transitions to [`SearchPhase::Searching`] with a
//! fresh monotonic token and returns a [`SearchRequest`] telling the plugin to
//! spawn the query on a worker thread. The plugin polls the worker each render
//! and feeds results back via [`SearchState::apply_stage_result`]; a result whose
//! token doesn't match the current [`query_seq`](SearchState) is dropped (that's
//! how a superseded query is coalesced / cancelled). An 8 s ceiling
//! ([`SEARCH_CEILING`]) bounds a hung `qmd` — past it the tab shows an honest
//! "search timed out" state.
//!
//! **Two-stage fast paint.** A single worker runs `qmd` TWICE per query, both
//! stages carrying the same token:
//!
//! 1. **BM25** ([`SearchStage::Bm25`], `qmd search --json`, no LLM — effectively
//!    instant): the first hits paint immediately. Before this paint the FULL
//!    `Searching…` spinner shows.
//! 2. **Semantic** ([`SearchStage::Semantic`], `qmd query --json -C 20`, the LLM
//!    rerank): swaps in the reranked results once it lands.
//!
//! Between the BM25 paint and the semantic swap the tab is in
//! [`SearchPhase::Refining`] — results are already visible, so only a SUBTLE
//! "refining…" indicator shows (not the full spinner; the user can already read
//! and act on the lexical results). If the semantic pass errors or times out
//! AFTER BM25 painted, the BM25 results STAY on screen and the refining
//! indicator clears — they are never blanked. If BM25 itself errors, the tab
//! falls through to the semantic pass; only if BOTH yield nothing does it show
//! the honest "no results" state.
//!
//! Result→record resolution maps a qmd hit (a `#docid` + a `qmd://…` file +
//! a title) back to a parsed fixture/real record via [`resolve_hit`], matching
//! the hit `id`, its `file` basename stem, or its `title` against the records.
//! An unresolvable hit (a doc the local KB doesn't carry) simply doesn't open —
//! a clean no-op, never a panic.
//!
//! All width math goes through `chars()` — never byte slicing — so a multibyte
//! query / title can't panic on a char boundary (the Rust UTF-8 truncate trap).
//!
//! [`QmdCli`]: crate::data::QmdCli

use std::time::{Duration, Instant};

use ratatui::buffer::Buffer as RBuffer;
use ratatui::layout::{Constraint, Direction, Layout, Rect as RRect};
use ratatui::style::{Modifier as RModifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph, Row, Table, Widget};

use ainb_plugin_sdk::KeyCode;

use super::{CORNFLOWER_BLUE, GOLD, LIST_HIGHLIGHT_BG, MUTED_GRAY, SELECTION_GREEN, SOFT_WHITE};
use crate::data::{DataError, LearningRecord, SearchHit};

/// The query-box prompt token. Unique to the Search input box so the tripwire
/// can lock an exact match (never a substring-OR).
pub(crate) const PROMPT: &str = "search:";

/// Upper bound on how long an in-flight `qmd` search may run before the tab
/// gives up and shows the honest timeout state. A hung `qmd` (or a cold
/// LLM-expansion that overruns) must not leave the user staring at a spinner
/// forever. The orphaned worker thread is left to finish on its own; its result
/// is ignored by token mismatch.
pub(crate) const SEARCH_CEILING: Duration = Duration::from_secs(8);

/// The braille spinner frames cycled while a search is in flight. Indexed by
/// `started.elapsed()` so the glyph advances every render tick.
const SPINNER_FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// How often the spinner glyph advances.
const SPINNER_TICK: Duration = Duration::from_millis(80);

/// Which stage of the two-stage search a worker result came from. Both stages
/// of one query carry the same token; the stage tag tells the UI whether this is
/// the fast first paint (BM25) or the reranked swap-in (semantic).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchStage {
    /// `qmd search --json` — BM25 full-text, the instant fast paint (stage 1).
    Bm25,
    /// `qmd query --json -C 20` — the LLM-reranked swap-in (stage 2, final).
    Semantic,
}

/// Lifecycle of a single submitted query. The worker runs OFF the dispatch
/// thread; the plugin drives the transitions
/// [`Idle`](SearchPhase::Idle) → [`Searching`](SearchPhase::Searching) →
/// [`Refining`](SearchPhase::Refining) → [`Done`](SearchPhase::Done) (the
/// `Refining` step is skipped when BM25 returns nothing useful).
#[derive(Debug, Clone, Copy, Default)]
enum SearchPhase {
    /// No query has been submitted (or the query was edited, invalidating the
    /// last one). The pre-submit hint renders. The default phase.
    #[default]
    Idle,
    /// A query is in flight and NOTHING has painted yet (before the BM25 result).
    /// The FULL `Searching…` spinner shows. `token` ties the eventual result back
    /// to THIS submit (a superseded query's stale result is dropped); `started`
    /// bounds the whole two-stage against [`SEARCH_CEILING`] and animates the
    /// spinner.
    Searching { token: u64, started: Instant },
    /// BM25 results have painted and are visible; the semantic rerank is still in
    /// flight. Only a SUBTLE "refining…" indicator shows — the results are
    /// already actionable, so the full spinner would be jarring. Carries the same
    /// `token` + `started` as the originating `Searching`, so the ceiling still
    /// covers the semantic pass and a superseded query is still droppable.
    Refining { token: u64, started: Instant },
    /// The query settled — semantic results applied, OR the semantic pass
    /// errored/timed-out while BM25 results stay on screen, OR both stages were
    /// empty, OR the search timed out before any paint. The results table /
    /// empty state renders; no indicator.
    Done,
}

impl SearchPhase {
    /// The in-flight `(token, started)` of this phase, if any (both `Searching`
    /// and `Refining` are in-flight; `Idle`/`Done` are settled).
    fn in_flight(&self) -> Option<(u64, Instant)> {
        match *self {
            SearchPhase::Searching { token, started }
            | SearchPhase::Refining { token, started } => Some((token, started)),
            _ => None,
        }
    }
}

/// A request the plugin must service by spawning the `qmd` worker: run the
/// two-stage search (BM25 then semantic) for `query`, tagged with `token`, and
/// send each stage's `(token, stage, result)` back on the plugin's channel. The
/// plugin feeds them to [`SearchState::apply_stage_result`] on subsequent
/// renders.
#[derive(Debug, Clone)]
pub struct SearchRequest {
    /// The monotonic token identifying this submit. The worker echoes it on BOTH
    /// stages so a superseded query's results can be dropped.
    pub token: u64,
    /// The (trimmed-non-empty) query string to run.
    pub query: String,
}

/// Everything the Search tab needs for query metadata: the resolved collection
/// / index from [`LearningsConfig`]. The runner is NOT here any more — the
/// plugin owns it and runs the worker, so the UI stays pure view state with no
/// blocking subprocess.
///
/// [`LearningsConfig`]: crate::config::LearningsConfig
pub struct SearchContext<'a> {
    /// QMD collection to query (`config.qmd_collection`).
    pub collection: &'a str,
    /// QMD sqlite index path (`config.qmd_index`) — threaded for display/future
    /// use; the runner does not forward it as `--index` (see [`QmdCli`] docs).
    ///
    /// [`QmdCli`]: crate::data::QmdCli
    pub index: &'a str,
}

/// The outcome of routing a key into the Search tab: did state change, should
/// the shell open the Detail pane for a resolved record, and should it kick off
/// a `qmd` worker for a freshly-submitted query?
pub struct SearchKeyOutcome {
    /// `true` when the key mutated Search state (so the shell bumps its render
    /// generation).
    pub changed: bool,
    /// `Some(record)` when `Enter` on a selected result resolved a record the
    /// shell should open in the Detail pane. Cloned so the pane outlives the
    /// result list.
    pub open_record: Option<LearningRecord>,
    /// `Some(request)` when `Enter` submitted a fresh query the plugin must run
    /// on a worker thread (the non-blocking submit path).
    pub start_search: Option<SearchRequest>,
}

impl SearchKeyOutcome {
    /// A no-op outcome: nothing changed, nothing to open, nothing to run.
    fn unchanged() -> Self {
        Self {
            changed: false,
            open_record: None,
            start_search: None,
        }
    }

    /// State changed; nothing to open or run.
    fn changed() -> Self {
        Self {
            changed: true,
            open_record: None,
            start_search: None,
        }
    }
}

/// Search-tab view state: the query input + the ranked results + selection +
/// the in-flight search lifecycle.
#[derive(Debug, Default)]
pub struct SearchState {
    /// The query being composed in the input box.
    query: String,
    /// `true` once `/` has focused the query box (so the box + prompt render).
    /// Stays `true` after submit so editing the query re-runs a fresh search.
    focused: bool,
    /// The last completed query's ranked hits (empty until the first result).
    results: Vec<SearchHit>,
    /// `true` once a query has been submitted at least once — distinguishes the
    /// pre-submit hint state from a settled-but-empty (no-results) state.
    submitted: bool,
    /// Selected row index into [`results`](Self::results).
    selected: usize,
    /// Lifecycle of the current/last submitted query.
    phase: SearchPhase,
    /// Monotonic submit token. Bumped on every submit so a stale worker result
    /// (from a superseded query) can be recognised and dropped.
    query_seq: u64,
    /// `true` once the in-flight search overran [`SEARCH_CEILING`] — drives the
    /// honest "search timed out" empty state.
    timed_out: bool,
}

impl SearchState {
    /// `true` when the query box is focused (the input box + prompt render).
    #[must_use]
    pub const fn is_focused(&self) -> bool {
        self.focused
    }

    /// Focus the query box (the `/` action). Idempotent — focusing an already-
    /// focused box is a no-op that still counts as "handled" so a stray `/`
    /// doesn't fall through to another tab.
    pub fn focus(&mut self) {
        self.focused = true;
    }

    /// The currently-selected hit, if any.
    #[must_use]
    pub fn selected_hit(&self) -> Option<&SearchHit> {
        self.results.get(self.selected)
    }

    /// `true` while the FULL `Searching…` spinner is up — i.e. a query is in
    /// flight and NOTHING has painted yet (the pre-BM25 stage). This is the gate
    /// the help bar / results pane use to choose the spinner over the table.
    #[must_use]
    pub const fn is_searching(&self) -> bool {
        matches!(self.phase, SearchPhase::Searching { .. })
    }

    /// `true` while the subtle "refining…" indicator is up — i.e. BM25 results
    /// are visible and the semantic rerank is still upgrading them.
    #[must_use]
    pub const fn is_refining(&self) -> bool {
        matches!(self.phase, SearchPhase::Refining { .. })
    }

    /// `true` while EITHER stage of a two-stage search is in flight (spinner OR
    /// refining). The shell keeps ticking redraws while this holds so the spinner
    /// animates AND the plugin keeps polling the worker channel for the next
    /// stage — see `LearningsUi::wants_redraw`.
    #[must_use]
    pub const fn is_in_flight(&self) -> bool {
        matches!(
            self.phase,
            SearchPhase::Searching { .. } | SearchPhase::Refining { .. }
        )
    }

    /// Route a Search-tab key. `records` is the parsed KB used to resolve a
    /// selected hit back to a record on `Enter`.
    ///
    /// Routing while focused:
    /// - `Enter` — submit the query (kick off the non-blocking worker via
    ///   [`SearchKeyOutcome::start_search`]) when results aren't yet the focus;
    ///   if a result is selected, `Enter` resolves + opens it.
    /// - printable char — append to the query (including `j`/`k`, so a user can
    ///   search for `jwt`, `kafka`, …; selection is arrow-keys-only).
    /// - `Backspace` — delete the last query char (no-op when empty).
    /// - `↑↓` — move the result selection.
    ///
    /// The shell only routes keys here when the Search tab is active (and the
    /// Detail pane is closed), so unfocused presses other than `/` don't reach
    /// this method.
    pub fn handle_key(
        &mut self,
        code: &KeyCode,
        ctx: &SearchContext<'_>,
        records: &[LearningRecord],
    ) -> SearchKeyOutcome {
        match code {
            KeyCode::Enter => self.on_enter(ctx, records),
            KeyCode::Backspace => {
                if self.query.pop().is_some() {
                    self.invalidate_results();
                    SearchKeyOutcome::changed()
                } else {
                    SearchKeyOutcome::unchanged()
                }
            }
            KeyCode::Down => self.move_selection(1),
            KeyCode::Up => self.move_selection(-1),
            KeyCode::Char { ch } => {
                self.query.push(*ch);
                self.invalidate_results();
                SearchKeyOutcome::changed()
            }
            _ => SearchKeyOutcome::unchanged(),
        }
    }

    /// Invalidate any previously-submitted results because the query was
    /// edited. Without this a user who submits `foo`, then edits the query
    /// to `bar`, and hits `Enter` would OPEN `foo`'s stale top hit instead
    /// of re-querying. Clearing `results` + resetting the phase to `Idle`
    /// forces the next `Enter` back onto the submit path so it runs a fresh
    /// search for the edited query. Bumping `query_seq` also makes any
    /// still-in-flight worker's result a stale token (dropped on arrival).
    fn invalidate_results(&mut self) {
        self.results.clear();
        self.submitted = false;
        self.selected = 0;
        self.phase = SearchPhase::Idle;
        self.timed_out = false;
        // Supersede any in-flight query so its eventual result is ignored.
        self.query_seq = self.query_seq.wrapping_add(1);
    }

    /// `Enter` handler: if a settled result is selected, resolve + open it;
    /// otherwise submit the current query (kick off the worker).
    fn on_enter(
        &mut self,
        _ctx: &SearchContext<'_>,
        records: &[LearningRecord],
    ) -> SearchKeyOutcome {
        // Before the FIRST paint (the BM25 spinner stage) there's nothing to
        // open and Enter must NOT re-submit, so it's a no-op. Once BM25 has
        // painted (the `Refining` stage) results ARE visible, so Enter opens the
        // selected hit just like the settled state — the `submitted && !results`
        // branch below covers both `Refining` and `Done`.
        if self.is_searching() {
            return SearchKeyOutcome::unchanged();
        }
        if self.submitted && !self.results.is_empty() {
            if let Some(record) = self.selected_hit().and_then(|h| resolve_hit(h, records).cloned())
            {
                return SearchKeyOutcome {
                    changed: true,
                    open_record: Some(record),
                    start_search: None,
                };
            }
            // Selected hit didn't resolve to a local record — clean no-op.
            return SearchKeyOutcome::unchanged();
        }

        // Otherwise: submit the query (non-blocking).
        self.submit()
    }

    /// Begin a fresh search: transition to [`SearchPhase::Searching`] with a new
    /// token and ask the plugin (via [`SearchKeyOutcome::start_search`]) to run
    /// the `qmd` worker. Does NOT spawn the subprocess — that would block the
    /// dispatch thread. An empty query short-circuits to the settled no-results
    /// state without a worker.
    fn submit(&mut self) -> SearchKeyOutcome {
        self.submitted = true;
        self.selected = 0;
        self.results.clear();
        self.timed_out = false;
        self.query_seq = self.query_seq.wrapping_add(1);

        let query = self.query.trim().to_string();
        if query.is_empty() {
            // Empty query → no worker, settle immediately on the empty state.
            self.phase = SearchPhase::Done;
            return SearchKeyOutcome::changed();
        }

        let token = self.query_seq;
        self.phase = SearchPhase::Searching {
            token,
            started: Instant::now(),
        };
        SearchKeyOutcome {
            changed: true,
            open_record: None,
            start_search: Some(SearchRequest { token, query }),
        }
    }

    /// Apply one STAGE of a two-stage worker result back into the view. The
    /// plugin calls this each render with whatever the worker channel yielded
    /// (BM25 first, semantic second). A result whose `token` doesn't match the
    /// current in-flight token is DROPPED — that's how a superseded (coalesced)
    /// query's stale results are discarded (across BOTH stages). Returns `true`
    /// if the result was applied (so the plugin bumps its render generation).
    ///
    /// Stage handling:
    /// - **BM25** (stage 1): if it returns hits, paint them and enter
    ///   [`SearchPhase::Refining`] (results visible, semantic still upgrading).
    ///   If it errors or is empty, STAY in [`SearchPhase::Searching`] (spinner
    ///   up) and wait for the semantic pass — BM25 failing must not blank the
    ///   pane or settle early, because the semantic pass may still have hits.
    /// - **Semantic** (stage 2, final): swap in the reranked hits and settle to
    ///   [`SearchPhase::Done`]. On an error, KEEP whatever's on screen (the BM25
    ///   results, if any) and settle — never blank a visible result set. If both
    ///   stages were empty, the settled empty state shows "no results".
    pub fn apply_stage_result(
        &mut self,
        token: u64,
        stage: SearchStage,
        result: Result<Vec<SearchHit>, DataError>,
    ) -> bool {
        // Only the current in-flight token's result counts (either stage).
        let Some((cur, started)) = self.phase.in_flight() else {
            return false;
        };
        if cur != token {
            return false;
        }

        match stage {
            SearchStage::Bm25 => match result {
                Ok(hits) if !hits.is_empty() => {
                    // Fast paint: show the lexical hits, keep refining.
                    self.results = hits;
                    self.selected = 0;
                    self.timed_out = false;
                    self.phase = SearchPhase::Refining { token, started };
                    true
                }
                Ok(_) => false, // empty BM25 — stay on the spinner for semantic.
                Err(err) => {
                    tracing::warn!(%err, "qmd BM25 search failed — awaiting semantic");
                    false // BM25 error — stay on the spinner for semantic.
                }
            },
            SearchStage::Semantic => {
                match result {
                    Ok(hits) => {
                        // Reranked swap-in (may be empty → settled no-results).
                        self.results = hits;
                        self.selected = 0;
                    }
                    Err(err) => {
                        // Keep whatever painted (the BM25 results, if any) — do
                        // NOT blank a visible set on a semantic error.
                        tracing::warn!(%err, "qmd semantic search failed — keeping BM25 results");
                    }
                }
                self.timed_out = false;
                self.phase = SearchPhase::Done; // refining cleared.
                true
            }
        }
    }

    /// Check the in-flight search against [`SEARCH_CEILING`] (covering the WHOLE
    /// two-stage). The plugin calls this each render. On deadline:
    /// - **Before any paint** (`Searching`): give up to the honest "search timed
    ///   out" empty state.
    /// - **After BM25 painted** (`Refining`): KEEP the BM25 results on screen,
    ///   just clear the refining indicator and settle — the semantic pass overran
    ///   but the user still has the lexical hits.
    ///
    /// Either way the query is superseded so the orphaned worker's late results
    /// are dropped. Returns `true` if a timeout fired (so the plugin bumps its
    /// render generation + stops the worker). A no-op when settled or within the
    /// ceiling.
    pub fn check_timeout(&mut self) -> bool {
        match self.phase {
            SearchPhase::Searching { started, .. } => {
                if started.elapsed() <= SEARCH_CEILING {
                    return false;
                }
                // Nothing painted → honest timeout empty state.
                self.results.clear();
                self.selected = 0;
                self.timed_out = true;
            }
            SearchPhase::Refining { started, .. } => {
                if started.elapsed() <= SEARCH_CEILING {
                    return false;
                }
                // BM25 already painted → keep results, just stop refining.
                self.timed_out = false;
            }
            _ => return false,
        }
        self.phase = SearchPhase::Done;
        // Supersede so a late worker result for this query is dropped.
        self.query_seq = self.query_seq.wrapping_add(1);
        true
    }

    /// Test-only seam: backdate the in-flight phase's `started` past the
    /// ceiling so the next [`Self::check_timeout`] fires without waiting out
    /// the real 8 s — the same trick the in-module timeout tests use, exposed
    /// for the plugin's kill-on-timeout test.
    #[cfg(test)]
    pub(crate) fn force_timeout_eligible(&mut self) {
        if let SearchPhase::Searching { started, .. } | SearchPhase::Refining { started, .. } =
            &mut self.phase
        {
            *started = Instant::now() - (SEARCH_CEILING + Duration::from_secs(1));
        }
    }

    /// The braille spinner glyph for the current PRE-PAINT search, advanced by
    /// `started.elapsed()` so it animates across render ticks. `None` once BM25
    /// has painted (the `Refining` stage uses the subtle indicator instead) or
    /// when settled.
    fn spinner_glyph(&self) -> Option<&'static str> {
        let SearchPhase::Searching { started, .. } = self.phase else {
            return None;
        };
        let frame = (started.elapsed().as_millis() / SPINNER_TICK.as_millis()) as usize
            % SPINNER_FRAMES.len();
        Some(SPINNER_FRAMES[frame])
    }

    /// Move the result selection by `delta` (clamped to the result bounds).
    fn move_selection(&mut self, delta: isize) -> SearchKeyOutcome {
        if self.results.is_empty() {
            return SearchKeyOutcome::unchanged();
        }
        let last = self.results.len() - 1;
        let next = (self.selected as isize + delta).clamp(0, last as isize) as usize;
        if next == self.selected {
            return SearchKeyOutcome::unchanged();
        }
        self.selected = next;
        SearchKeyOutcome::changed()
    }
}

/// Resolve a qmd [`SearchHit`] back to a parsed [`LearningRecord`].
///
/// qmd hits carry a `#docid`, a `qmd://…/<stem>.md` file locator, and a title.
/// None of those is guaranteed to equal a record's `id` (the `.md` filename
/// stem). Resolution tries, in order:
/// 1. hit `id` == record `id` (the fake test seeds this exact match),
/// 2. the hit `file` basename stem == record `id` (the real qmd path shape),
/// 3. hit `title` == record `title`.
///
/// Returns `None` when no record matches — a hit for a doc the local KB doesn't
/// carry. The caller treats `None` as "nothing to open" (a clean no-op).
fn resolve_hit<'a>(hit: &SearchHit, records: &'a [LearningRecord]) -> Option<&'a LearningRecord> {
    // 1. Direct id match.
    if let Some(rec) = records.iter().find(|r| r.id == hit.id) {
        return Some(rec);
    }
    // 2. file basename stem == record id.
    if let Some(stem) = hit.file.as_deref().and_then(file_stem) {
        if let Some(rec) = records.iter().find(|r| r.id == stem) {
            return Some(rec);
        }
    }
    // 3. title match.
    records.iter().find(|r| r.title == hit.title)
}

/// The filename stem of a `qmd://…/<name>.md` (or plain path) locator — the
/// last `/`-segment with a trailing `.md` stripped. `None` for an empty
/// locator. Used only for hit→record resolution.
fn file_stem(file: &str) -> Option<String> {
    let last = file.rsplit('/').next()?;
    if last.is_empty() {
        return None;
    }
    Some(last.strip_suffix(".md").unwrap_or(last).to_string())
}

/// Render the Search tab into `area`: the query box, then the ranked results
/// (or the honest empty state), then the bottom help bar.
pub fn render(buf: &mut RBuffer, area: RRect, state: &SearchState) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // query box (bordered)
            Constraint::Min(1),    // results / empty state
            Constraint::Length(1), // help bar
        ])
        .split(area);

    render_query_box(buf, rows[0], state);
    render_results(buf, rows[1], state);
    render_help_bar(buf, rows[2], state);
}

/// Render the bordered query input box: `search: <query>▏` with a block cursor
/// glyph so the focus point is visible. The `search:` prompt is the unique
/// token the tripwire locks.
fn render_query_box(buf: &mut RBuffer, area: RRect, state: &SearchState) {
    let outer = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(if state.is_focused() {
            GOLD
        } else {
            CORNFLOWER_BLUE
        }));
    let inner = outer.inner(area);
    outer.render(area, buf);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let cursor = if state.is_focused() { "▏" } else { "" };
    let line = Line::from(vec![
        Span::styled(
            format!("{PROMPT} "),
            Style::default().fg(GOLD).add_modifier(RModifier::BOLD),
        ),
        Span::styled(state.query.clone(), Style::default().fg(SOFT_WHITE)),
        Span::styled(cursor.to_string(), Style::default().fg(GOLD)),
    ]);
    Paragraph::new(line).render(inner, buf);
}

/// Render the ranked results as an `id · title · score` table, the animated
/// "Searching…" line while a query is in flight, or the honest empty state when
/// there are none.
fn render_results(buf: &mut RBuffer, area: RRect, state: &SearchState) {
    if let Some(glyph) = state.spinner_glyph() {
        render_searching(buf, area, glyph);
        return;
    }
    if state.results.is_empty() {
        render_empty(buf, area, state);
        return;
    }

    let rows: Vec<Row> = state
        .results
        .iter()
        .enumerate()
        .map(|(i, hit)| {
            let is_sel = i == state.selected;
            let marker = if is_sel { "▶" } else { " " };
            let style = if is_sel {
                Style::default()
                    .fg(SELECTION_GREEN)
                    .bg(LIST_HIGHLIGHT_BG)
                    .add_modifier(RModifier::BOLD)
            } else {
                Style::default().fg(SOFT_WHITE)
            };
            Row::new(vec![
                Span::styled(marker, style),
                Span::styled(hit.id.clone(), style),
                Span::styled(hit.title.clone(), style),
                Span::styled(format!("{:.2}", hit.score), style),
            ])
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Length(2),  // ▶ marker
            Constraint::Length(10), // #docid
            Constraint::Min(20),    // title
            Constraint::Length(6),  // score
        ],
    );
    Widget::render(table, area, buf);
}

/// The animated in-flight state: a cycling braille spinner + the `Searching…`
/// token (which the tests lock). The glyph advances with `started.elapsed()`,
/// so the host's redraw ticks animate it.
fn render_searching(buf: &mut RBuffer, area: RRect, glyph: &str) {
    Paragraph::new(Line::from(vec![
        Span::raw("  "),
        Span::styled(
            glyph.to_string(),
            Style::default().fg(GOLD).add_modifier(RModifier::BOLD),
        ),
        Span::styled(
            " Searching…",
            Style::default().fg(MUTED_GRAY).add_modifier(RModifier::ITALIC),
        ),
    ]))
    .render(area, buf);
}

/// The honest empty state. Distinguishes the pre-submit hint ("type a query…"),
/// a timed-out search ("search timed out"), and a submitted-but-empty result
/// ("no results"). Each renders a distinct exact token so the tests/tripwire can
/// assert them precisely.
fn render_empty(buf: &mut RBuffer, area: RRect, state: &SearchState) {
    let text = if state.timed_out {
        // The search overran SEARCH_CEILING and was abandoned.
        "  search timed out"
    } else if state.submitted {
        // Settled (empty query OR a query the runner had no hits for).
        "  no results"
    } else {
        "  type a query and press ⏎ to search"
    };
    Paragraph::new(Line::from(Span::styled(
        text,
        Style::default().fg(MUTED_GRAY).add_modifier(RModifier::ITALIC),
    )))
    .render(area, buf);
}

/// Bottom help bar for the Search tab. `⏎ search`/`⏎ open` reflect the live
/// two-stage Enter; `↑↓ select` is live once there are results; while the FIRST
/// (BM25) stage is still running the bar reads `searching…`; once BM25 has
/// painted and the semantic rerank is upgrading it, a subtle `⟳ refining…`
/// marker trails the live select/open keys (results stay actionable).
fn render_help_bar(buf: &mut RBuffer, area: RRect, state: &SearchState) {
    let mut spans = vec![Span::raw(" ")];
    if state.is_searching() {
        spans.extend(help_key("⏎", "searching…"));
        spans.extend(help_key("Bksp", "edit"));
    } else if state.results.is_empty() {
        spans.extend(help_key("⏎", "search"));
        spans.extend(help_key("Bksp", "edit"));
    } else {
        spans.extend(help_key("↑↓", "select"));
        spans.extend(help_key("⏎", "open"));
        spans.extend(help_key("Bksp", "edit"));
    }
    spans.extend(help_key("Tab", "pane"));
    // Subtle refining marker: results are visible + actionable, the semantic
    // rerank is still upgrading them. Distinct from the full `Searching…`
    // spinner (which only shows pre-first-paint). The `refining…` token is the
    // one tests lock.
    if state.is_refining() {
        spans.push(Span::styled(
            "  ⟳ refining…",
            Style::default().fg(CORNFLOWER_BLUE).add_modifier(RModifier::ITALIC),
        ));
    }
    Paragraph::new(Line::from(spans)).render(area, buf);
}

/// A live help-bar entry: gold key glyph + muted description.
fn help_key(key: &str, desc: &str) -> [Span<'static>; 3] {
    [
        Span::styled(
            key.to_string(),
            Style::default().fg(GOLD).add_modifier(RModifier::BOLD),
        ),
        Span::styled(format!(" {desc}"), Style::default().fg(MUTED_GRAY)),
        Span::styled("  ", Style::default().fg(MUTED_GRAY)),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::QmdSearch;

    /// A fake runner returning a fixed payload — the in-module unit seam. Used
    /// only to PARSE a payload into hits the way the worker would, so the unit
    /// tests can settle a submit synchronously without a thread.
    struct Fake(String);
    impl QmdSearch for Fake {
        fn run_query(&self, _q: &str, _c: &str, _i: &str) -> Result<String, DataError> {
            Ok(self.0.clone())
        }
    }

    fn record(id: &str, title: &str) -> LearningRecord {
        LearningRecord {
            id: id.into(),
            title: title.into(),
            scope: "universal".into(),
            confidence: 0.9,
            category: "process".into(),
            tags: vec![],
            source_tool: None,
            project: None,
            key_insight: String::new(),
            body_md: String::new(),
            entities: vec![],
            relationships: vec![],
            provenance: crate::data::Provenance::default(),
        }
    }

    fn ctx() -> SearchContext<'static> {
        SearchContext {
            collection: "learnings",
            index: "~/.cache/qmd/index.sqlite",
        }
    }

    /// Run the fake through both `qmd` stages (BM25 then semantic) for a request,
    /// applying each stage to the state — the in-module stand-in for the plugin's
    /// two-stage worker + render poll.
    fn settle_both_stages(
        st: &mut SearchState,
        fake: &Fake,
        c: &SearchContext<'_>,
        req: &SearchRequest,
    ) {
        let bm25 = crate::data::search(
            fake,
            &req.query,
            c.collection,
            c.index,
            crate::data::SearchMode::Bm25,
        );
        st.apply_stage_result(req.token, SearchStage::Bm25, bm25);
        let semantic = crate::data::search(
            fake,
            &req.query,
            c.collection,
            c.index,
            crate::data::SearchMode::Semantic,
        );
        st.apply_stage_result(req.token, SearchStage::Semantic, semantic);
    }

    /// Drive a submit to completion synchronously: route the key, then run the
    /// fake through both stages and apply them. Returns the original outcome so
    /// callers can inspect `open_record`.
    fn submit_and_settle(
        st: &mut SearchState,
        fake: &Fake,
        c: &SearchContext<'_>,
        recs: &[LearningRecord],
    ) -> SearchKeyOutcome {
        let out = st.handle_key(&KeyCode::Enter, c, recs);
        if let Some(req) = &out.start_search {
            settle_both_stages(st, fake, c, req);
        }
        out
    }

    #[test]
    fn typing_appends_and_backspace_edits_query() {
        let c = ctx();
        let mut st = SearchState::default();
        st.focus();
        for ch in "abc".chars() {
            st.handle_key(&KeyCode::Char { ch }, &c, &[]);
        }
        assert_eq!(st.query, "abc");
        let out = st.handle_key(&KeyCode::Backspace, &c, &[]);
        assert!(out.changed);
        assert_eq!(st.query, "ab");
        // Backspace on empty is a no-op.
        st.query.clear();
        let out = st.handle_key(&KeyCode::Backspace, &c, &[]);
        assert!(!out.changed);
    }

    #[test]
    fn submit_transitions_to_searching_then_applies_ranked_results() {
        let payload = serde_json::json!([
            {"docid": "#1", "score": 0.9, "title": "Top"},
            {"docid": "#2", "score": 0.4, "title": "Low"}
        ])
        .to_string();
        let fake = Fake(payload);
        let c = ctx();
        let mut st = SearchState::default();
        st.focus();
        for ch in "query".chars() {
            st.handle_key(&KeyCode::Char { ch }, &c, &[]);
        }
        // Submit does NOT block / apply results — it transitions to Searching
        // and hands back a request for the plugin to run on a worker.
        let out = st.handle_key(&KeyCode::Enter, &c, &[]);
        assert!(st.is_searching(), "submit transitions to Searching");
        assert!(st.results.is_empty(), "no results yet while searching");
        let req = out.start_search.expect("submit yields a worker request");
        assert_eq!(req.query, "query");

        // Apply the BM25 stage → results paint, phase goes Refining (fast paint).
        let bm25 = crate::data::search(
            &fake,
            &req.query,
            c.collection,
            c.index,
            crate::data::SearchMode::Bm25,
        );
        assert!(st.apply_stage_result(req.token, SearchStage::Bm25, bm25));
        assert!(st.is_refining(), "BM25 paint enters the refining stage");
        assert!(
            !st.is_searching(),
            "the full spinner is gone after BM25 paints"
        );
        assert_eq!(st.results.len(), 2);

        // Apply the semantic stage → swaps in the reranked set, settles to Done.
        let semantic = crate::data::search(
            &fake,
            &req.query,
            c.collection,
            c.index,
            crate::data::SearchMode::Semantic,
        );
        assert!(st.apply_stage_result(req.token, SearchStage::Semantic, semantic));
        assert!(!st.is_in_flight(), "semantic stage settles the search");
        assert_eq!(st.results.len(), 2);
        assert_eq!(st.results[0].id, "#1");
        assert_eq!(st.results[1].id, "#2");
        assert_eq!(st.selected, 0);
    }

    #[test]
    fn empty_query_submit_settles_without_worker() {
        let c = ctx();
        let mut st = SearchState::default();
        st.focus();
        // Submit with empty query → no worker request, settles immediately,
        // submitted=true, not searching.
        let out = st.handle_key(&KeyCode::Enter, &c, &[]);
        assert!(out.start_search.is_none(), "empty query spawns no worker");
        assert!(!st.is_searching());
        assert!(st.results.is_empty());
        assert!(st.submitted);
    }

    #[test]
    fn enter_on_result_resolves_and_opens_record() {
        let payload =
            serde_json::json!([{"docid": "lrn-x", "score": 0.9, "title": "T"}]).to_string();
        let fake = Fake(payload);
        let c = ctx();
        let recs = vec![record("lrn-x", "T")];
        let mut st = SearchState::default();
        st.focus();
        for ch in "q".chars() {
            st.handle_key(&KeyCode::Char { ch }, &c, &recs);
        }
        // First Enter submits + settle the worker result.
        submit_and_settle(&mut st, &fake, &c, &recs);
        assert_eq!(st.results.len(), 1);
        // Second Enter opens the resolved record.
        let out = st.handle_key(&KeyCode::Enter, &c, &recs);
        let opened = out.open_record.expect("Enter on a result opens a record");
        assert_eq!(opened.id, "lrn-x");
    }

    #[test]
    fn resolve_hit_matches_by_file_stem_when_id_differs() {
        let hit = SearchHit {
            id: "#abc".into(),
            score: 0.5,
            title: "unrelated title".into(),
            file: Some("qmd://learnings/lrn-y.md".into()),
        };
        let recs = vec![record("lrn-y", "Some title")];
        let resolved = resolve_hit(&hit, &recs).expect("resolves by file stem");
        assert_eq!(resolved.id, "lrn-y");
    }

    #[test]
    fn j_and_k_type_into_query_box_not_navigate() {
        // Regression: `j`/`k` must be typed into the focused query box, not
        // consumed as result-selection movement. A user must be able to search
        // for `jwt`, `kafka`, `json`, `jenkins`, `kotlin`, etc.
        let payload = serde_json::json!([
            {"docid": "#1", "score": 0.9, "title": "a"},
            {"docid": "#2", "score": 0.5, "title": "b"}
        ])
        .to_string();
        let fake = Fake(payload);
        let c = ctx();
        let mut st = SearchState::default();
        st.focus();

        // Type every char of a query that contains both `j` and `k`.
        for ch in "jwt kafka".chars() {
            let out = st.handle_key(&KeyCode::Char { ch }, &c, &[]);
            assert!(out.changed, "typing {ch:?} must mutate the query box");
        }
        assert_eq!(
            st.query, "jwt kafka",
            "every char (incl. `j` and `k`) must append to the query box"
        );

        // With results present, `j`/`k` must STILL type — they must not move the
        // selection. Seed results via a settled submit, then confirm `j` appends
        // and the selection stays put.
        let mut st = SearchState::default();
        st.focus();
        for ch in "query".chars() {
            st.handle_key(&KeyCode::Char { ch }, &c, &[]);
        }
        submit_and_settle(&mut st, &fake, &c, &[]);
        assert_eq!(st.results.len(), 2, "precondition: results present");
        assert_eq!(st.selected, 0, "precondition: first result selected");

        let out = st.handle_key(&KeyCode::Char { ch: 'j' }, &c, &[]);
        assert!(
            out.changed,
            "typing `j` with results present must still type"
        );
        assert_eq!(st.query, "queryj", "`j` must append, not navigate");
        assert_eq!(st.selected, 0, "`j` must NOT move the result selection");

        let out = st.handle_key(&KeyCode::Char { ch: 'k' }, &c, &[]);
        assert!(
            out.changed,
            "typing `k` with results present must still type"
        );
        assert_eq!(st.query, "queryjk", "`k` must append, not navigate");
        assert_eq!(st.selected, 0, "`k` must NOT move the result selection");
    }

    #[test]
    fn editing_query_after_submit_re_runs_search_not_opens_stale_result() {
        // Regression: after a query is submitted and results populate, editing
        // the query (typing or backspace) must INVALIDATE the stale results so
        // the next Enter RE-SUBMITS the fresh query — it must NOT open the old
        // query's selected result. Otherwise a user who types `foo`, hits Enter,
        // then edits to `bar` and hits Enter would open `foo`'s top hit.
        let payload =
            serde_json::json!([{"docid": "lrn-stale", "score": 0.9, "title": "Stale"}]).to_string();
        let fake = Fake(payload);
        let c = ctx();
        let recs = vec![record("lrn-stale", "Stale")];
        let mut st = SearchState::default();
        st.focus();

        // Submit query #1 → settle results, submitted=true.
        for ch in "foo".chars() {
            st.handle_key(&KeyCode::Char { ch }, &c, &recs);
        }
        submit_and_settle(&mut st, &fake, &c, &recs);
        assert_eq!(st.results.len(), 1, "precondition: query #1 has results");
        assert!(st.submitted, "precondition: query #1 submitted");

        // Edit the query (append a char). This must invalidate the stale results
        // so the next Enter re-submits rather than opening `foo`'s stale hit.
        st.handle_key(&KeyCode::Char { ch: 'x' }, &c, &recs);
        assert!(
            st.results.is_empty(),
            "appending a char after submit must clear the stale results"
        );
        assert!(
            !st.submitted,
            "appending a char after submit must reset `submitted` so Enter re-queries"
        );

        // Next Enter must RE-SUBMIT (kick a fresh worker), NOT open the stale
        // record. The submit transitions to Searching and yields a request;
        // nothing opened.
        let out = st.handle_key(&KeyCode::Enter, &c, &recs);
        assert!(
            out.open_record.is_none(),
            "Enter after editing the query must re-query, not open the stale result"
        );
        assert!(
            out.start_search.is_some() && st.is_searching(),
            "Enter after editing must re-run the search (new worker request)"
        );
        // Settle it so the next Backspace edit starts from a clean settled state.
        let req = out.start_search.unwrap();
        settle_both_stages(&mut st, &fake, &c, &req);
        assert_eq!(st.results.len(), 1, "re-submit applied fresh results");

        // Backspace must ALSO invalidate results (same staleness hazard).
        st.handle_key(&KeyCode::Backspace, &c, &recs);
        assert!(
            st.results.is_empty(),
            "Backspace after submit must clear the stale results"
        );
        assert!(
            !st.submitted,
            "Backspace after submit must reset `submitted` so Enter re-queries"
        );
        let out = st.handle_key(&KeyCode::Enter, &c, &recs);
        assert!(
            out.open_record.is_none(),
            "Enter after a backspace edit must re-query, not open the stale result"
        );
    }

    #[test]
    fn down_up_move_selection_within_bounds() {
        let payload = serde_json::json!([
            {"docid": "#1", "score": 0.9, "title": "a"},
            {"docid": "#2", "score": 0.5, "title": "b"}
        ])
        .to_string();
        let fake = Fake(payload);
        let c = ctx();
        let mut st = SearchState::default();
        st.focus();
        for ch in "q".chars() {
            st.handle_key(&KeyCode::Char { ch }, &c, &[]);
        }
        submit_and_settle(&mut st, &fake, &c, &[]);
        assert_eq!(st.selected, 0);
        st.handle_key(&KeyCode::Down, &c, &[]);
        assert_eq!(st.selected, 1);
        // Down at the bottom is a clamped no-op.
        let out = st.handle_key(&KeyCode::Down, &c, &[]);
        assert!(!out.changed);
        st.handle_key(&KeyCode::Up, &c, &[]);
        assert_eq!(st.selected, 0);
    }

    #[test]
    fn stale_token_result_is_dropped_coalescing_superseded_query() {
        // Two submits in flight: applying the FIRST token's result after the
        // SECOND submit has bumped the token must be dropped (the coalescing /
        // cancellation of a superseded query).
        let c = ctx();
        let mut st = SearchState::default();
        st.focus();
        for ch in "first".chars() {
            st.handle_key(&KeyCode::Char { ch }, &c, &[]);
        }
        let first = st.handle_key(&KeyCode::Char { ch: '!' }, &c, &[]); // mutate then submit
        let _ = first;
        let out1 = st.handle_key(&KeyCode::Enter, &c, &[]);
        let req1 = out1.start_search.expect("first submit yields a request");

        // Edit + re-submit BEFORE applying req1 → req1 is now superseded.
        st.handle_key(&KeyCode::Char { ch: '2' }, &c, &[]);
        let out2 = st.handle_key(&KeyCode::Enter, &c, &[]);
        let req2 = out2.start_search.expect("second submit yields a request");
        assert_ne!(req1.token, req2.token, "tokens must be distinct");

        // Apply the STALE first result (either stage) → dropped (still searching,
        // no results) — coalescing covers BOTH stages of the superseded query.
        let stale = Ok(vec![SearchHit {
            id: "#stale".into(),
            score: 0.9,
            title: "stale".into(),
            file: None,
        }]);
        assert!(
            !st.apply_stage_result(req1.token, SearchStage::Bm25, stale),
            "a superseded token's result must be dropped"
        );
        assert!(st.is_searching(), "still searching for the live query");
        assert!(st.results.is_empty(), "no stale results applied");

        // Apply the LIVE second result's semantic stage → lands.
        let live = Ok(vec![SearchHit {
            id: "#live".into(),
            score: 0.9,
            title: "live".into(),
            file: None,
        }]);
        assert!(st.apply_stage_result(req2.token, SearchStage::Semantic, live));
        assert_eq!(st.results.len(), 1);
        assert_eq!(st.results[0].id, "#live");
    }

    #[test]
    fn check_timeout_abandons_a_hung_search_to_an_honest_state() {
        // A Searching phase whose `started` is already past the ceiling settles
        // to the timed-out state when checked.
        let c = ctx();
        let mut st = SearchState::default();
        st.focus();
        for ch in "slow".chars() {
            st.handle_key(&KeyCode::Char { ch }, &c, &[]);
        }
        let out = st.handle_key(&KeyCode::Enter, &c, &[]);
        let req = out.start_search.expect("submit yields a request");

        // Within the ceiling: no timeout.
        assert!(!st.check_timeout(), "fresh search must not time out");
        assert!(st.is_searching());

        // Force the started instant past the ceiling, then check.
        if let SearchPhase::Searching { started, .. } = &mut st.phase {
            *started = Instant::now() - (SEARCH_CEILING + Duration::from_secs(1));
        }
        assert!(st.check_timeout(), "an overrun search must time out");
        assert!(!st.is_searching(), "timeout settles the phase");
        assert!(st.timed_out, "timeout flag set for the honest empty state");

        // A late worker result (either stage) for the timed-out token is dropped.
        let late = Ok(vec![SearchHit {
            id: "#late".into(),
            score: 0.9,
            title: "late".into(),
            file: None,
        }]);
        assert!(
            !st.apply_stage_result(req.token, SearchStage::Bm25, late),
            "a result for a timed-out query must be dropped"
        );
        assert!(st.results.is_empty());
    }

    #[test]
    fn spinner_glyph_present_only_while_searching() {
        let c = ctx();
        let mut st = SearchState::default();
        st.focus();
        assert!(st.spinner_glyph().is_none(), "idle has no spinner");
        for ch in "q".chars() {
            st.handle_key(&KeyCode::Char { ch }, &c, &[]);
        }
        st.handle_key(&KeyCode::Enter, &c, &[]);
        assert!(
            st.spinner_glyph().is_some(),
            "searching renders a spinner glyph"
        );
    }

    // ---- Two-stage BM25 fast paint ----

    /// Submit a query and return the live token (state is left in `Searching`).
    fn submit_query(st: &mut SearchState, c: &SearchContext<'_>, query: &str) -> u64 {
        st.focus();
        for ch in query.chars() {
            st.handle_key(&KeyCode::Char { ch }, c, &[]);
        }
        let out = st.handle_key(&KeyCode::Enter, c, &[]);
        out.start_search.expect("submit yields a request").token
    }

    fn hit(id: &str) -> SearchHit {
        SearchHit {
            id: id.into(),
            score: 0.9,
            title: id.into(),
            file: None,
        }
    }

    #[test]
    fn bm25_paints_then_semantic_swaps_in_reranked_results() {
        let c = ctx();
        let mut st = SearchState::default();
        let token = submit_query(&mut st, &c, "rebase");
        assert!(st.spinner_glyph().is_some(), "pre-paint: full spinner up");

        // BM25 lands → lexical hits paint, phase = Refining (no full spinner).
        assert!(st.apply_stage_result(token, SearchStage::Bm25, Ok(vec![hit("#bm25")])));
        assert!(st.is_refining(), "BM25 paint enters Refining");
        assert!(
            st.spinner_glyph().is_none(),
            "no full spinner during refining"
        );
        assert_eq!(st.results.len(), 1);
        assert_eq!(st.results[0].id, "#bm25", "BM25 lexical hit is visible");

        // Semantic lands → reranked set REPLACES the BM25 set, phase = Done.
        assert!(st.apply_stage_result(
            token,
            SearchStage::Semantic,
            Ok(vec![hit("#sem-a"), hit("#sem-b")])
        ));
        assert!(!st.is_in_flight(), "semantic settles the search");
        assert!(!st.is_refining());
        assert_eq!(st.results.len(), 2, "reranked set swapped in");
        assert_eq!(st.results[0].id, "#sem-a");
    }

    #[test]
    fn refining_active_between_bm25_paint_and_semantic_swap() {
        let c = ctx();
        let mut st = SearchState::default();
        let token = submit_query(&mut st, &c, "rebase");
        assert!(!st.is_refining(), "not refining before any paint");
        st.apply_stage_result(token, SearchStage::Bm25, Ok(vec![hit("#bm25")]));
        assert!(st.is_refining(), "refining between BM25 and semantic");
        st.apply_stage_result(token, SearchStage::Semantic, Ok(vec![hit("#sem")]));
        assert!(
            !st.is_refining(),
            "refining cleared after the semantic swap"
        );
    }

    #[test]
    fn semantic_error_after_bm25_keeps_bm25_results_visible() {
        let c = ctx();
        let mut st = SearchState::default();
        let token = submit_query(&mut st, &c, "rebase");
        st.apply_stage_result(token, SearchStage::Bm25, Ok(vec![hit("#bm25")]));
        assert!(st.is_refining());

        // Semantic ERRORS → keep the BM25 results, just settle + stop refining.
        let err = Err(DataError::Subprocess("qmd died".into()));
        assert!(st.apply_stage_result(token, SearchStage::Semantic, err));
        assert!(!st.is_in_flight(), "settled after semantic error");
        assert!(!st.is_refining(), "refining cleared");
        assert_eq!(st.results.len(), 1, "BM25 results stay on screen");
        assert_eq!(st.results[0].id, "#bm25");
        assert!(!st.timed_out, "an error is not a timeout");
    }

    #[test]
    fn bm25_error_stays_on_spinner_then_semantic_lands() {
        let c = ctx();
        let mut st = SearchState::default();
        let token = submit_query(&mut st, &c, "rebase");

        // BM25 errors → must NOT settle / blank; stay on the full spinner.
        let err = Err(DataError::Subprocess("bm25 died".into()));
        assert!(
            !st.apply_stage_result(token, SearchStage::Bm25, err),
            "a BM25 error must not change state (await semantic)"
        );
        assert!(
            st.is_searching(),
            "still on the full spinner after BM25 error"
        );
        assert!(st.results.is_empty());

        // Semantic then lands with hits → they paint and settle.
        assert!(st.apply_stage_result(token, SearchStage::Semantic, Ok(vec![hit("#sem")])));
        assert!(!st.is_in_flight());
        assert_eq!(st.results.len(), 1);
        assert_eq!(st.results[0].id, "#sem");
    }

    #[test]
    fn empty_bm25_stays_on_spinner_until_semantic() {
        let c = ctx();
        let mut st = SearchState::default();
        let token = submit_query(&mut st, &c, "rebase");

        // Empty BM25 → no paint, stay on the spinner (semantic may still hit).
        assert!(
            !st.apply_stage_result(token, SearchStage::Bm25, Ok(vec![])),
            "empty BM25 must not settle early"
        );
        assert!(st.is_searching(), "still searching after empty BM25");

        // Semantic also empty → settle to the honest no-results empty state.
        assert!(st.apply_stage_result(token, SearchStage::Semantic, Ok(vec![])));
        assert!(!st.is_in_flight());
        assert!(st.results.is_empty());
        assert!(st.submitted, "submitted → render_empty shows `no results`");
        assert!(!st.timed_out);
    }

    #[test]
    fn timeout_after_bm25_keeps_results_and_clears_refining() {
        let c = ctx();
        let mut st = SearchState::default();
        let token = submit_query(&mut st, &c, "rebase");
        st.apply_stage_result(token, SearchStage::Bm25, Ok(vec![hit("#bm25")]));
        assert!(st.is_refining());

        // Force the refining `started` past the ceiling → timeout fires, but the
        // BM25 results MUST stay (not blanked) and refining clears.
        if let SearchPhase::Refining { started, .. } = &mut st.phase {
            *started = Instant::now() - (SEARCH_CEILING + Duration::from_secs(1));
        }
        assert!(
            st.check_timeout(),
            "the refining stage times out at the ceiling"
        );
        assert!(!st.is_in_flight(), "settled after the timeout");
        assert!(!st.is_refining());
        assert_eq!(st.results.len(), 1, "BM25 results survive the timeout");
        assert_eq!(st.results[0].id, "#bm25");
        assert!(
            !st.timed_out,
            "a refining timeout keeps results — NOT the timed-out empty state"
        );

        // A late semantic result for the (now superseded) token is dropped.
        assert!(!st.apply_stage_result(token, SearchStage::Semantic, Ok(vec![hit("#late")])));
        assert_eq!(
            st.results[0].id, "#bm25",
            "late semantic must not overwrite"
        );
    }

    #[test]
    fn coalescing_drops_both_stages_of_a_superseded_query() {
        let c = ctx();
        let mut st = SearchState::default();
        let token1 = submit_query(&mut st, &c, "first");

        // Apply BM25 for query #1 → it paints (Refining).
        st.apply_stage_result(token1, SearchStage::Bm25, Ok(vec![hit("#q1-bm25")]));
        assert!(st.is_refining());

        // Edit + re-submit → query #2 supersedes #1 (new token, back to spinner).
        st.handle_key(&KeyCode::Char { ch: '2' }, &c, &[]);
        let out2 = st.handle_key(&KeyCode::Enter, &c, &[]);
        let token2 = out2.start_search.expect("second submit yields a request").token;
        assert_ne!(token1, token2);
        assert!(st.is_searching(), "back to the full spinner for query #2");
        assert!(st.results.is_empty(), "the edit cleared query #1's results");

        // BOTH stages of the stale query #1 are now dropped.
        assert!(!st.apply_stage_result(token1, SearchStage::Bm25, Ok(vec![hit("#q1-late-bm25")])));
        assert!(!st.apply_stage_result(
            token1,
            SearchStage::Semantic,
            Ok(vec![hit("#q1-late-sem")])
        ));
        assert!(st.results.is_empty(), "no stale query #1 result applied");
        assert!(st.is_searching(), "still awaiting query #2");

        // Query #2's stages land normally.
        st.apply_stage_result(token2, SearchStage::Bm25, Ok(vec![hit("#q2-bm25")]));
        assert_eq!(st.results[0].id, "#q2-bm25");
        st.apply_stage_result(token2, SearchStage::Semantic, Ok(vec![hit("#q2-sem")]));
        assert_eq!(st.results[0].id, "#q2-sem");
        assert!(!st.is_in_flight());
    }
}
