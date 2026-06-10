//! P7 Search-tab tests — in-process render / handle_key over the fixture KB
//! with an INJECTED fake `QmdSearch` runner (no subprocess, no live qmd).
//!
//! These drive the real `LearningsPlugin` through the SDK `Server` via the
//! testkit. The plugin is built with [`LearningsPlugin::with_search_runner`]
//! carrying a fake runner that returns a captured/known set of `SearchHit`s,
//! so the ranked-result rendering is deterministic and never depends on a live
//! qmd index (per the SEARCH-SPECIFIC discipline: assert ranked-RESULTS
//! RENDERING with an injected fake, not live qmd output).
//!
//! The plugin is `init`ed with a `config` table pointing `learnings_dir` at the
//! committed fixture KB (`tests/fixtures/kb`) so `Enter` on a result can
//! resolve the hit back to a real fixture `LearningRecord` and open the SAME
//! Detail pane (P6).
//!
//! Behaviour under test (TDD plan §P7 / design mock C3):
//! - `/` (or `Tab` to the Search tab) opens a query input box bearing a unique
//!   prompt token; typed characters echo into the box.
//! - Submitting a non-empty query (`Enter`) renders ranked result rows (`id` /
//!   `title` / `score`) from the fake runner, in rank order — but ONLY after the
//!   non-blocking worker settles (submit shows a spinner first; the worker runs
//!   OFF the dispatch thread).
//! - `↑↓` move the result selection; `Enter` on a selected result opens the
//!   Detail pane for the resolved `LearningRecord`.
//! - An empty query / a query the runner returns no hits for renders an honest
//!   empty state.
//!
//! **Settling the async search.** Submit kicks a worker thread and returns
//! immediately; the result is polled back in on a later render. Tests drive the
//! settle with [`render_until`] — a bounded poll loop that re-renders until the
//! awaited token appears (or a deadline trips). This is deterministic for a fast
//! fake (the worker finishes in microseconds) and exercises the real
//! spinner → results transition, not a mocked-out one.
//!
//! Asserted EXACT unique tokens (never substring-OR):
//! - prompt        → `search:` (the query-box prompt)
//! - result id     → `#114073` (a qmd docid from the fake hits)
//! - result title  → `feedback_audit_after_rebase`
//! - result score  → `0.93`
//! - detail body   → `## Solution` (proves Enter opened the resolved record)
//! - empty state   → `no results`
//! - searching     → `Searching…` (the in-flight spinner line)
//! - timeout       → `search timed out` (the abandoned-search empty state)

use ainb_plugin_learnings::data::{DataError, QmdSearch};
use ainb_plugin_learnings::plugin::LearningsPlugin;
use ainb_plugin_protocol::params::{HandleKeyParams, KeyCode, KeyEvent};
use ainb_plugin_testkit::{Viewport, WireBuffer, methods, run_plugin};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

/// Absolute path to the committed fixture KB directory.
fn kb_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("fixtures").join("kb")
}

/// The `[plugins.learnings]` table the host would resolve, pointed at the
/// fixture KB so result→record resolution lands on a real fixture record.
fn fixture_config() -> serde_json::Value {
    serde_json::json!({
        "learnings_dir": kb_dir().display().to_string(),
    })
}

/// A fake `QmdSearch` runner returning a fixed JSON payload regardless of the
/// query — the injected-runner seam that keeps the ranked-result rendering
/// deterministic without spawning `qmd`. The first hit's `id` is set to a real
/// fixture record id so `Enter` on it resolves to that record's Detail pane.
struct FakeRunner {
    /// Raw `qmd query --json` stdout this runner echoes back.
    payload: String,
}

impl QmdSearch for FakeRunner {
    fn run_query(
        &self,
        _query: &str,
        _collection: &str,
        _index: &str,
    ) -> Result<String, DataError> {
        Ok(self.payload.clone())
    }
}

/// A runner that always returns an empty hit list (the no-results path).
struct EmptyRunner;

impl QmdSearch for EmptyRunner {
    fn run_query(
        &self,
        _query: &str,
        _collection: &str,
        _index: &str,
    ) -> Result<String, DataError> {
        Ok("[]".to_string())
    }
}

/// A runner that sleeps `delay` before returning `payload` — used to observe the
/// spinner BEFORE results land (the worker runs off the dispatch thread, so the
/// pane stays responsive while this sleeps).
struct SlowRunner {
    payload: String,
    delay: Duration,
}

impl QmdSearch for SlowRunner {
    fn run_query(
        &self,
        _query: &str,
        _collection: &str,
        _index: &str,
    ) -> Result<String, DataError> {
        std::thread::sleep(self.delay);
        Ok(self.payload.clone())
    }
}

/// A runner that sleeps WELL past the search ceiling (8 s) before returning, so
/// the timeout fires first. The eventual result is dropped by token mismatch.
struct HangRunner;

impl QmdSearch for HangRunner {
    fn run_query(
        &self,
        _query: &str,
        _collection: &str,
        _index: &str,
    ) -> Result<String, DataError> {
        // Longer than SEARCH_CEILING; the timeout abandons the search first.
        std::thread::sleep(Duration::from_secs(30));
        Ok("[]".to_string())
    }
}

/// A runner that sleeps `delay`, then returns a single hit whose `title`
/// ECHOES the query verbatim. Used by the coalescing test: each submit's
/// result is identifiable by the query it was built from, so the test can
/// prove the SUPERSEDED query's result never renders. The slow stage also
/// guarantees both submits are in flight before either settles.
struct EchoRunner {
    delay: Duration,
}

impl QmdSearch for EchoRunner {
    fn run_query(&self, query: &str, _collection: &str, _index: &str) -> Result<String, DataError> {
        std::thread::sleep(self.delay);
        Ok(serde_json::json!([
            { "docid": "#echo", "score": 0.99, "title": format!("echo:{query}") }
        ])
        .to_string())
    }
}

/// Fake `qmd query --json` payload. Row 0's `docid` is a real fixture record id
/// (`lrn-audit-after-rebase`) so selecting it resolves to a fixture record and
/// opens its Detail pane. The remaining rows carry distinct ids/titles/scores
/// so the rank order (descending score) is observable, and one row's `file`
/// basename stem also points at a fixture record (resolution-by-file path).
fn fake_payload() -> String {
    serde_json::json!([
        {
            "docid": "#114073",
            "score": 0.93,
            "file": "qmd://learnings/lrn-audit-after-rebase.md",
            "title": "feedback_audit_after_rebase"
        },
        {
            "docid": "#8980c5",
            "score": 0.56,
            "file": "qmd://learnings/lrn-claude-plugin-autowire.md",
            "title": "Claude plugin autowire"
        },
        {
            "docid": "#5862eb",
            "score": 0.46,
            "file": "qmd://learnings/memory-cfddd3.md",
            "title": "Memory Index"
        }
    ])
    .to_string()
}

/// Render the whole buffer to a single newline-joined string, row by row, so
/// token assertions can match across cell boundaries within a row.
fn buffer_text(buf: &WireBuffer) -> String {
    let mut rows: Vec<String> = vec![String::new(); buf.height as usize];
    for (coord, cell) in &buf.cells {
        if let Some(row) = rows.get_mut(coord.y as usize) {
            while row.chars().count() < coord.x as usize {
                row.push(' ');
            }
            row.push_str(&cell.symbol);
        }
    }
    rows.join("\n")
}

/// Re-render in a bounded poll loop until `token` appears in the rendered text,
/// returning that text. This is how a test settles the async search: submit
/// kicks a worker thread, the result is polled back in on a later render, so the
/// test renders until the awaited token lands (or a deadline trips and the test
/// fails with the last frame). A tiny sleep between polls lets the worker thread
/// make progress without busy-spinning.
async fn render_until(
    h: &mut ainb_plugin_testkit::Harness,
    token: &str,
    deadline: Duration,
) -> String {
    let start = std::time::Instant::now();
    loop {
        let text = buffer_text(&h.render(VIEWPORT).await.expect("render poll"));
        if text.contains(token) {
            return text;
        }
        if start.elapsed() >= deadline {
            panic!("token {token:?} did not appear within {deadline:?}; last frame:\n{text}");
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Build a `plugin/handle_key` notification for a single key with no mods.
fn key_params(code: KeyCode) -> HandleKeyParams {
    HandleKeyParams {
        screen_id: "learnings".to_string(),
        key: KeyEvent {
            code,
            mods: 0,
            kind: ainb_plugin_protocol::params::KeyKind::Press,
        },
        generation: 0,
    }
}

const VIEWPORT: Viewport = Viewport {
    width: 120,
    height: 40,
};

// --- Search-tab tokens (locked by a wrong-value run reading the real render)
// ---

/// The query-box prompt token — unique to the Search input box.
const PROMPT_TOKEN: &str = "search:";
/// A qmd docid from the fake hits (rank 0). Unique to a rendered result row.
const RESULT_ID_TOKEN: &str = "#114073";
/// The rank-0 hit title (verbatim from the fake payload).
const RESULT_TITLE_TOKEN: &str = "feedback_audit_after_rebase";
/// The rank-0 hit score, formatted to 2dp by the result row.
const RESULT_SCORE_TOKEN: &str = "0.93";
/// A Detail-body token — present only once Enter resolves a result to a record
/// and opens its Detail pane.
const DETAIL_BODY_TOKEN: &str = "## Solution";
/// The honest empty-state token (no query / no hits).
const EMPTY_TOKEN: &str = "no results";
/// The in-flight spinner token (rendered while the worker is running).
const SEARCHING_TOKEN: &str = "Searching…";
/// The abandoned-search empty-state token (search overran the ceiling).
const TIMEOUT_TOKEN: &str = "search timed out";
/// Generous deadline for settling a fast fake worker (microseconds in practice).
const SETTLE: Duration = Duration::from_secs(5);

/// Init a plugin (with the fake runner) over the fixture KB and return the
/// harness ready to drive.
async fn init_with_fake() -> ainb_plugin_testkit::Harness {
    let plugin = LearningsPlugin::with_search_runner(Arc::new(FakeRunner {
        payload: fake_payload(),
    }));
    let mut h = run_plugin(plugin);
    h.init_with_config(fixture_config()).await.expect("init with fixture config");
    h
}

/// Send a `/` to open / focus the Search input box.
async fn open_search(h: &mut ainb_plugin_testkit::Harness) {
    h.send_notification(
        methods::PLUGIN_HANDLE_KEY,
        key_params(KeyCode::Char { ch: '/' }),
    )
    .await
    .expect("send /");
}

/// Type each char of `query` into the focused query box.
async fn type_query(h: &mut ainb_plugin_testkit::Harness, query: &str) {
    for ch in query.chars() {
        h.send_notification(methods::PLUGIN_HANDLE_KEY, key_params(KeyCode::Char { ch }))
            .await
            .expect("send query char");
    }
}

#[tokio::test]
async fn slash_opens_query_box_and_typed_chars_echo() {
    let mut h = init_with_fake().await;

    // Before `/`, the Search query box is not shown (we're on Browse).
    let before = buffer_text(&h.render(VIEWPORT).await.expect("render browse"));
    assert!(
        !before.contains(PROMPT_TOKEN),
        "the Search prompt must not show on the Browse tab before `/`:\n{before}"
    );

    // `/` focuses the Search input box bearing the prompt token.
    open_search(&mut h).await;
    let opened = buffer_text(&h.render(VIEWPORT).await.expect("render search open"));
    assert!(
        opened.contains(PROMPT_TOKEN),
        "`/` must open a query box bearing the {PROMPT_TOKEN:?} prompt:\n{opened}"
    );

    // Typed characters echo into the box.
    type_query(&mut h, "rebase").await;
    let typed = buffer_text(&h.render(VIEWPORT).await.expect("render typed"));
    assert!(
        typed.contains("rebase"),
        "typed query chars must echo into the box:\n{typed}"
    );

    h.close().await.expect("clean close");
}

#[tokio::test]
async fn typed_query_with_j_and_k_echoes_both_letters() {
    // Regression: `j`/`k` must be typed into the focused query box, not consumed
    // as result-selection movement — a user must be able to search for `jwt`,
    // `kafka`, `json`, `kubernetes`, etc. Drive the real plugin render path.
    let mut h = init_with_fake().await;

    open_search(&mut h).await;
    type_query(&mut h, "jwt kafka").await;

    let typed = buffer_text(&h.render(VIEWPORT).await.expect("render typed jk"));
    assert!(
        typed.contains("jwt kafka"),
        "a query containing `j` and `k` must echo verbatim into the box \
         (not move the result selection):\n{typed}"
    );

    h.close().await.expect("clean close");
}

#[tokio::test]
async fn submitting_query_renders_ranked_result_rows() {
    let mut h = init_with_fake().await;

    open_search(&mut h).await;
    type_query(&mut h, "audit rebase").await;

    // Submit (Enter) → the non-blocking worker runs off-thread; ranked results
    // land on a later render. Poll until the top hit appears.
    h.send_notification(methods::PLUGIN_HANDLE_KEY, key_params(KeyCode::Enter))
        .await
        .expect("send Enter submit");

    let results = render_until(&mut h, RESULT_ID_TOKEN, SETTLE).await;

    // id / title / score of the top hit all render.
    assert!(
        results.contains(RESULT_ID_TOKEN),
        "ranked result id {RESULT_ID_TOKEN:?} must render:\n{results}"
    );
    assert!(
        results.contains(RESULT_TITLE_TOKEN),
        "ranked result title {RESULT_TITLE_TOKEN:?} must render:\n{results}"
    );
    assert!(
        results.contains(RESULT_SCORE_TOKEN),
        "ranked result score {RESULT_SCORE_TOKEN:?} must render:\n{results}"
    );

    // Rank order: the top-scored hit (#114073, 0.93) renders ABOVE the
    // lowest-scored hit (#5862eb, 0.46).
    let top_line = results
        .lines()
        .position(|l| l.contains(RESULT_ID_TOKEN))
        .expect("top hit row present");
    let low_line = results
        .lines()
        .position(|l| l.contains("#5862eb"))
        .expect("low hit row present");
    assert!(
        top_line < low_line,
        "results must be in rank order (top score first):\n{results}"
    );

    h.close().await.expect("clean close");
}

#[tokio::test]
async fn down_then_enter_opens_detail_for_selected_result() {
    let mut h = init_with_fake().await;

    open_search(&mut h).await;
    type_query(&mut h, "audit rebase").await;
    h.send_notification(methods::PLUGIN_HANDLE_KEY, key_params(KeyCode::Enter))
        .await
        .expect("submit query");

    // Results land on a later render (worker off-thread); poll until they do.
    let results = render_until(&mut h, RESULT_ID_TOKEN, SETTLE).await;
    assert!(
        !results.contains(DETAIL_BODY_TOKEN),
        "the result list must not render the Detail body before opening:\n{results}"
    );

    // The top result (#114073) resolves to `lrn-audit-after-rebase`. Enter on
    // the selected (first) result opens the SAME Detail pane (P6) for it.
    h.send_notification(methods::PLUGIN_HANDLE_KEY, key_params(KeyCode::Enter))
        .await
        .expect("open detail for result");

    let detail = buffer_text(&h.render(VIEWPORT).await.expect("render detail"));
    assert!(
        detail.contains(DETAIL_BODY_TOKEN),
        "Enter on a result must open the resolved record's Detail pane \
         (body {DETAIL_BODY_TOKEN:?}):\n{detail}"
    );

    // Backspace closes the Detail pane back to the results list (the P6 close
    // key — Esc is host-reserved).
    h.send_notification(methods::PLUGIN_HANDLE_KEY, key_params(KeyCode::Backspace))
        .await
        .expect("close detail");
    let back = buffer_text(&h.render(VIEWPORT).await.expect("render after close"));
    assert!(
        !back.contains(DETAIL_BODY_TOKEN),
        "Backspace must close Detail back to the results list:\n{back}"
    );
    assert!(
        back.contains(RESULT_ID_TOKEN),
        "after closing Detail the results list must be back:\n{back}"
    );

    h.close().await.expect("clean close");
}

#[tokio::test]
async fn down_moves_result_selection_before_open() {
    let mut h = init_with_fake().await;

    open_search(&mut h).await;
    type_query(&mut h, "audit").await;
    h.send_notification(methods::PLUGIN_HANDLE_KEY, key_params(KeyCode::Enter))
        .await
        .expect("submit query");

    // Initially the first result (#114073) is selected (▶ precedes it). Poll the
    // off-thread worker to completion first.
    let first = render_until(&mut h, RESULT_ID_TOKEN, SETTLE).await;
    let top_line = first
        .lines()
        .find(|l| l.contains(RESULT_ID_TOKEN))
        .expect("top result row present");
    assert!(
        top_line.contains('▶'),
        "the first result must be selected initially:\n{first}"
    );

    // ↓ moves the selection to the second result (#8980c5).
    h.send_notification(methods::PLUGIN_HANDLE_KEY, key_params(KeyCode::Down))
        .await
        .expect("send Down");
    let moved = buffer_text(&h.render(VIEWPORT).await.expect("render moved"));
    let top_line = moved
        .lines()
        .find(|l| l.contains(RESULT_ID_TOKEN))
        .expect("top result row present");
    let second_line = moved
        .lines()
        .find(|l| l.contains("#8980c5"))
        .expect("second result row present");
    assert!(
        !top_line.contains('▶'),
        "first result must lose selection after ↓:\n{moved}"
    );
    assert!(
        second_line.contains('▶'),
        "second result must gain selection after ↓:\n{moved}"
    );

    h.close().await.expect("clean close");
}

#[tokio::test]
async fn empty_query_renders_honest_empty_state() {
    let mut h = init_with_fake().await;

    open_search(&mut h).await;
    // Submit with no query typed → honest empty state (no rows, no panic).
    h.send_notification(methods::PLUGIN_HANDLE_KEY, key_params(KeyCode::Enter))
        .await
        .expect("submit empty");
    let empty = buffer_text(&h.render(VIEWPORT).await.expect("render empty"));
    assert!(
        empty.contains(EMPTY_TOKEN),
        "an empty query must render an honest empty state ({EMPTY_TOKEN:?}):\n{empty}"
    );
    assert!(
        !empty.contains(RESULT_ID_TOKEN),
        "no result rows must render for an empty query:\n{empty}"
    );

    h.close().await.expect("clean close");
}

#[tokio::test]
async fn no_hit_query_renders_honest_empty_state() {
    // A runner that returns zero hits → the no-results empty state.
    let plugin = LearningsPlugin::with_search_runner(Arc::new(EmptyRunner));
    let mut h = run_plugin(plugin);
    h.init_with_config(fixture_config()).await.expect("init");

    open_search(&mut h).await;
    type_query(&mut h, "nothing matches this").await;
    h.send_notification(methods::PLUGIN_HANDLE_KEY, key_params(KeyCode::Enter))
        .await
        .expect("submit query");

    // The worker settles to zero hits off-thread; poll until the empty state
    // shows (it replaces the spinner once the result lands).
    let empty = render_until(&mut h, EMPTY_TOKEN, SETTLE).await;
    assert!(
        empty.contains(EMPTY_TOKEN),
        "a query with no hits must render the honest empty state \
         ({EMPTY_TOKEN:?}):\n{empty}"
    );

    h.close().await.expect("clean close");
}

#[tokio::test]
async fn backspace_edits_query_then_no_op_when_empty() {
    let mut h = init_with_fake().await;

    open_search(&mut h).await;
    type_query(&mut h, "abc").await;
    let typed = buffer_text(&h.render(VIEWPORT).await.expect("render typed"));
    assert!(typed.contains("abc"), "query echoes `abc`:\n{typed}");

    // Backspace deletes the last char while the query box is focused + non-empty.
    h.send_notification(methods::PLUGIN_HANDLE_KEY, key_params(KeyCode::Backspace))
        .await
        .expect("send backspace");
    let edited = buffer_text(&h.render(VIEWPORT).await.expect("render edited"));
    assert!(
        edited.contains("ab") && !edited.lines().any(|l| l.contains("abc")),
        "Backspace must delete the last query char (`abc` → `ab`):\n{edited}"
    );

    h.close().await.expect("clean close");
}

#[tokio::test]
async fn submit_shows_spinner_first_then_results_when_worker_settles() {
    // The load-bearing non-blocking proof: a slow runner means the FIRST render
    // after Enter shows the spinner (the dispatch thread did NOT block on qmd),
    // and a LATER render — once the off-thread worker settles — shows the
    // results. If submit blocked, the pane would freeze for the 200 ms instead
    // of painting a spinner.
    let plugin = LearningsPlugin::with_search_runner(Arc::new(SlowRunner {
        payload: fake_payload(),
        delay: Duration::from_millis(200),
    }));
    let mut h = run_plugin(plugin);
    h.init_with_config(fixture_config()).await.expect("init slow");

    open_search(&mut h).await;
    type_query(&mut h, "audit").await;
    h.send_notification(methods::PLUGIN_HANDLE_KEY, key_params(KeyCode::Enter))
        .await
        .expect("submit slow query");

    // First render right after submit: the spinner is up, results are NOT yet
    // present (the worker is still sleeping).
    let searching = buffer_text(&h.render(VIEWPORT).await.expect("render searching"));
    assert!(
        searching.contains(SEARCHING_TOKEN),
        "the first render after submit must show the spinner (non-blocking):\n{searching}"
    );
    assert!(
        !searching.contains(RESULT_ID_TOKEN),
        "results must NOT be present while the worker is still running:\n{searching}"
    );

    // Poll until the worker settles and the results replace the spinner.
    let results = render_until(&mut h, RESULT_ID_TOKEN, SETTLE).await;
    assert!(
        !results.contains(SEARCHING_TOKEN),
        "the spinner must be gone once results land:\n{results}"
    );
    assert!(
        results.contains(RESULT_TITLE_TOKEN),
        "the settled results must render the top hit title:\n{results}"
    );

    h.close().await.expect("clean close");
}

#[tokio::test]
async fn two_submits_coalesce_only_latest_results_render() {
    // Coalescing / cancellation: a slow first submit is superseded by a second
    // submit before the first settles. Only the SECOND query's results may
    // render — the first query's (stale-token) result is dropped on arrival.
    let plugin = LearningsPlugin::with_search_runner(Arc::new(EchoRunner {
        delay: Duration::from_millis(150),
    }));
    let mut h = run_plugin(plugin);
    h.init_with_config(fixture_config()).await.expect("init echo");

    open_search(&mut h).await;
    // Submit #1 (query "first") — kicks a slow worker.
    type_query(&mut h, "first").await;
    h.send_notification(methods::PLUGIN_HANDLE_KEY, key_params(KeyCode::Enter))
        .await
        .expect("submit first");
    // Confirm we're searching (spinner up), then edit + submit #2 before #1
    // settles. Editing invalidates + supersedes the in-flight query.
    let mid = buffer_text(&h.render(VIEWPORT).await.expect("render mid"));
    assert!(
        mid.contains(SEARCHING_TOKEN),
        "submit #1 must be in flight (spinner up):\n{mid}"
    );
    // Append to make query "firstSECOND", a distinct query/token.
    type_query(&mut h, "SECOND").await;
    h.send_notification(methods::PLUGIN_HANDLE_KEY, key_params(KeyCode::Enter))
        .await
        .expect("submit second");

    // Settle: poll until the SECOND query's echoed title renders.
    let results = render_until(&mut h, "echo:firstSECOND", SETTLE).await;
    assert!(
        !results.contains("echo:first\n") && !results.lines().any(|l| l.contains("echo:first ")),
        "the superseded first query's result must NOT render:\n{results}"
    );
    // Belt-and-braces: the only echoed title present is the second query's.
    let echo_titles: Vec<&str> = results.lines().filter(|l| l.contains("echo:")).collect();
    assert!(
        echo_titles.iter().all(|l| l.contains("echo:firstSECOND")),
        "only the latest query's results may render:\n{results}"
    );

    h.close().await.expect("clean close");
}

#[tokio::test]
async fn hung_search_times_out_to_honest_state() {
    // A runner that hangs well past the 8 s ceiling. The search must abandon to
    // the honest "search timed out" state — NOT spin forever. The orphaned
    // worker finishes on its own; its result is dropped by token mismatch.
    let plugin = LearningsPlugin::with_search_runner(Arc::new(HangRunner));
    let mut h = run_plugin(plugin);
    h.init_with_config(fixture_config()).await.expect("init hang");

    open_search(&mut h).await;
    type_query(&mut h, "this will hang").await;
    h.send_notification(methods::PLUGIN_HANDLE_KEY, key_params(KeyCode::Enter))
        .await
        .expect("submit hanging query");

    // While in flight, the spinner is up.
    let searching = buffer_text(&h.render(VIEWPORT).await.expect("render searching"));
    assert!(
        searching.contains(SEARCHING_TOKEN),
        "the hanging search must show the spinner while in flight:\n{searching}"
    );

    // Poll past the 8 s ceiling until the timeout state shows (generous margin).
    let timed_out = render_until(&mut h, TIMEOUT_TOKEN, Duration::from_secs(12)).await;
    assert!(
        !timed_out.contains(SEARCHING_TOKEN),
        "the spinner must clear once the search times out:\n{timed_out}"
    );

    h.close().await.expect("clean close");
}
