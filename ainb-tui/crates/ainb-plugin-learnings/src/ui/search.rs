//! Search tab — live query box + ranked `qmd` results (P7).
//!
//! The Search tab is a two-mode view:
//!
//! - **Query input** (`/` focuses it): a single-line box bearing the `search:`
//!   prompt. Printable chars append to the query; `Backspace` edits it; `Enter`
//!   submits.
//! - **Results**: the ranked [`SearchHit`]s the [`QmdSearch`] runner returned,
//!   rendered as `id · title · score` rows (rank order = the order the runner
//!   returned, which `qmd` sorts by score). `↑↓` move a `▶` selection; `Enter`
//!   on a result resolves the hit back to a [`LearningRecord`] and hands it to
//!   the shell to open the SAME Detail pane (P6). Selection uses the arrow keys
//!   only — `j`/`k` are reserved for typing into the query box (a user must be
//!   able to search for `jwt`, `kafka`, etc.).
//!
//! The qmd shell sits behind the [`QmdSearch`] trait so this module never
//! spawns a subprocess directly — it's handed a runner (real [`QmdCli`] at
//! runtime, a fake in tests) via a [`SearchContext`]. That keeps ranked-result
//! rendering deterministic under test without a live qmd index.
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

use ratatui::buffer::Buffer as RBuffer;
use ratatui::layout::{Constraint, Direction, Layout, Rect as RRect};
use ratatui::style::{Modifier as RModifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph, Row, Table, Widget};

use ainb_plugin_sdk::KeyCode;

use super::{CORNFLOWER_BLUE, GOLD, LIST_HIGHLIGHT_BG, MUTED_GRAY, SELECTION_GREEN, SOFT_WHITE};
use crate::data::{LearningRecord, QmdSearch, SearchHit, search as run_search};

/// The query-box prompt token. Unique to the Search input box so the tripwire
/// can lock an exact match (never a substring-OR).
pub(crate) const PROMPT: &str = "search:";

/// Everything the Search tab needs to actually run a query: the injected qmd
/// runner plus the resolved collection / index from [`LearningsConfig`]. Built
/// fresh by the plugin per `handle_key` so the runner is borrowed, not owned by
/// the UI (the runner lives on the plugin; the UI is pure view state).
///
/// [`LearningsConfig`]: crate::config::LearningsConfig
pub struct SearchContext<'a> {
    /// The qmd runner (real [`QmdCli`](crate::data::QmdCli) or a test fake).
    pub runner: &'a dyn QmdSearch,
    /// QMD collection to query (`config.qmd_collection`).
    pub collection: &'a str,
    /// QMD sqlite index path (`config.qmd_index`) — threaded for display/future
    /// use; the runner does not forward it as `--index` (see [`QmdCli`] docs).
    ///
    /// [`QmdCli`]: crate::data::QmdCli
    pub index: &'a str,
}

/// The outcome of routing a key into the Search tab: did state change, and
/// should the shell open the Detail pane for a resolved record?
pub struct SearchKeyOutcome {
    /// `true` when the key mutated Search state (so the shell bumps its render
    /// generation).
    pub changed: bool,
    /// `Some(record)` when `Enter` on a selected result resolved a record the
    /// shell should open in the Detail pane. Cloned so the pane outlives the
    /// result list.
    pub open_record: Option<LearningRecord>,
}

impl SearchKeyOutcome {
    /// A no-op outcome: nothing changed, nothing to open.
    fn unchanged() -> Self {
        Self {
            changed: false,
            open_record: None,
        }
    }

    /// State changed; nothing to open.
    fn changed() -> Self {
        Self {
            changed: true,
            open_record: None,
        }
    }
}

/// Search-tab view state: the query input + the ranked results + selection.
#[derive(Debug, Default)]
pub struct SearchState {
    /// The query being composed in the input box.
    query: String,
    /// `true` once `/` has focused the query box (so the box + prompt render).
    /// Stays `true` after submit so editing the query re-runs a fresh search.
    focused: bool,
    /// The last submitted query's ranked hits (empty until the first submit).
    results: Vec<SearchHit>,
    /// `true` once a query has been submitted at least once — distinguishes the
    /// pre-submit hint state from a submitted-but-empty (no-results) state.
    submitted: bool,
    /// Selected row index into [`results`](Self::results).
    selected: usize,
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

    /// Route a Search-tab key. `records` is the parsed KB used to resolve a
    /// selected hit back to a record on `Enter`.
    ///
    /// Routing while focused:
    /// - `Enter` — submit the query (run the search) when results aren't yet
    ///   the focus; if a result is selected, `Enter` resolves + opens it.
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
    /// of re-querying. Clearing `results` + resetting `submitted` forces
    /// the next `Enter` back onto the submit path so it runs a fresh search
    /// for the edited query.
    fn invalidate_results(&mut self) {
        self.results.clear();
        self.submitted = false;
        self.selected = 0;
    }

    /// `Enter` handler: if a result is selected, resolve + open it; otherwise
    /// submit the current query (run the search).
    fn on_enter(
        &mut self,
        ctx: &SearchContext<'_>,
        records: &[LearningRecord],
    ) -> SearchKeyOutcome {
        // After a submit with results, a second Enter opens the selected hit.
        if self.submitted && !self.results.is_empty() {
            if let Some(record) = self.selected_hit().and_then(|h| resolve_hit(h, records).cloned())
            {
                return SearchKeyOutcome {
                    changed: true,
                    open_record: Some(record),
                };
            }
            // Selected hit didn't resolve to a local record — clean no-op.
            return SearchKeyOutcome::unchanged();
        }

        // Otherwise: submit the query.
        self.submit(ctx);
        SearchKeyOutcome::changed()
    }

    /// Run the current query through the injected runner, replacing the results
    /// and resetting the selection. An empty query short-circuits to no results
    /// (no subprocess). A runner error degrades to an empty result set rather
    /// than failing the render — the empty state is the honest user-visible
    /// outcome.
    fn submit(&mut self, ctx: &SearchContext<'_>) {
        self.submitted = true;
        self.selected = 0;
        if self.query.trim().is_empty() {
            self.results.clear();
            return;
        }
        self.results = match run_search(ctx.runner, &self.query, ctx.collection, ctx.index) {
            Ok(hits) => hits,
            Err(err) => {
                tracing::warn!(query = %self.query, %err, "qmd search failed — empty results");
                Vec::new()
            }
        };
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

/// Render the ranked results as an `id · title · score` table, or the honest
/// empty state when there are none.
fn render_results(buf: &mut RBuffer, area: RRect, state: &SearchState) {
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

/// The honest empty state. Distinguishes the pre-submit hint ("type a query…")
/// from a submitted-but-empty result ("no results"). Both render the `no
/// results` token only in the no-hit case so the tests/tripwire can assert it
/// precisely.
fn render_empty(buf: &mut RBuffer, area: RRect, state: &SearchState) {
    let text = if state.submitted {
        // Submitted (empty query OR a query the runner had no hits for).
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
/// two-stage Enter; `↑↓ select` is live once there are results.
fn render_help_bar(buf: &mut RBuffer, area: RRect, state: &SearchState) {
    let mut spans = vec![Span::raw(" ")];
    if state.results.is_empty() {
        spans.extend(help_key("⏎", "search"));
        spans.extend(help_key("Bksp", "edit"));
    } else {
        spans.extend(help_key("↑↓", "select"));
        spans.extend(help_key("⏎", "open"));
        spans.extend(help_key("Bksp", "edit"));
    }
    spans.extend(help_key("Tab", "pane"));
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
    use crate::data::DataError;

    /// A fake runner returning a fixed payload — the in-module unit seam.
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

    fn ctx<'a>(runner: &'a dyn QmdSearch) -> SearchContext<'a> {
        SearchContext {
            runner,
            collection: "learnings",
            index: "~/.cache/qmd/index.sqlite",
        }
    }

    #[test]
    fn typing_appends_and_backspace_edits_query() {
        let fake = Fake("[]".into());
        let c = ctx(&fake);
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
    fn submit_populates_ranked_results() {
        let payload = serde_json::json!([
            {"docid": "#1", "score": 0.9, "title": "Top"},
            {"docid": "#2", "score": 0.4, "title": "Low"}
        ])
        .to_string();
        let fake = Fake(payload);
        let c = ctx(&fake);
        let mut st = SearchState::default();
        st.focus();
        for ch in "query".chars() {
            st.handle_key(&KeyCode::Char { ch }, &c, &[]);
        }
        st.handle_key(&KeyCode::Enter, &c, &[]);
        assert_eq!(st.results.len(), 2);
        assert_eq!(st.results[0].id, "#1");
        assert_eq!(st.results[1].id, "#2");
        assert_eq!(st.selected, 0);
    }

    #[test]
    fn empty_query_submit_clears_results() {
        let payload = serde_json::json!([{"docid": "#1", "score": 0.9, "title": "x"}]).to_string();
        let fake = Fake(payload);
        let c = ctx(&fake);
        let mut st = SearchState::default();
        st.focus();
        // Submit with empty query → no subprocess, no results, but submitted.
        st.handle_key(&KeyCode::Enter, &c, &[]);
        assert!(st.results.is_empty());
        assert!(st.submitted);
    }

    #[test]
    fn enter_on_result_resolves_and_opens_record() {
        let payload =
            serde_json::json!([{"docid": "lrn-x", "score": 0.9, "title": "T"}]).to_string();
        let fake = Fake(payload);
        let c = ctx(&fake);
        let recs = vec![record("lrn-x", "T")];
        let mut st = SearchState::default();
        st.focus();
        for ch in "q".chars() {
            st.handle_key(&KeyCode::Char { ch }, &c, &recs);
        }
        // First Enter submits.
        st.handle_key(&KeyCode::Enter, &c, &recs);
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
        let c = ctx(&fake);
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
        // selection. Seed results via a submit, then confirm `j` appends and the
        // selection stays put.
        let mut st = SearchState::default();
        st.focus();
        for ch in "query".chars() {
            st.handle_key(&KeyCode::Char { ch }, &c, &[]);
        }
        st.handle_key(&KeyCode::Enter, &c, &[]);
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
        let c = ctx(&fake);
        let recs = vec![record("lrn-stale", "Stale")];
        let mut st = SearchState::default();
        st.focus();

        // Submit query #1 → results populate, submitted=true.
        for ch in "foo".chars() {
            st.handle_key(&KeyCode::Char { ch }, &c, &recs);
        }
        st.handle_key(&KeyCode::Enter, &c, &recs);
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

        // Next Enter must RE-SUBMIT (run the search again), NOT open the stale
        // record. After re-submit the fresh results are present and nothing
        // opened.
        let out = st.handle_key(&KeyCode::Enter, &c, &recs);
        assert!(
            out.open_record.is_none(),
            "Enter after editing the query must re-query, not open the stale result"
        );
        assert_eq!(
            st.results.len(),
            1,
            "Enter after editing must re-run the search"
        );

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
        let c = ctx(&fake);
        let mut st = SearchState::default();
        st.focus();
        for ch in "q".chars() {
            st.handle_key(&KeyCode::Char { ch }, &c, &[]);
        }
        st.handle_key(&KeyCode::Enter, &c, &[]);
        assert_eq!(st.selected, 0);
        st.handle_key(&KeyCode::Down, &c, &[]);
        assert_eq!(st.selected, 1);
        // Down at the bottom is a clamped no-op.
        let out = st.handle_key(&KeyCode::Down, &c, &[]);
        assert!(!out.changed);
        st.handle_key(&KeyCode::Up, &c, &[]);
        assert_eq!(st.selected, 0);
    }
}
