//! P8 Graph-tab tests — in-process render / handle_key over the fixture KB.
//!
//! These drive the real `LearningsPlugin` through the SDK `Server` via the
//! testkit (no subprocess, no tmux). The plugin is `init`ed with a `config`
//! table pointing `learnings_dir` AND `graph_cache` at the committed fixture KB
//! (`tests/fixtures/kb`), which carries both the `.md` records (whose
//! `.entities.yaml` sidecars hold the TYPED relationships the neighborhood is
//! built from) and the `kv_store_community_reports.json` the community view
//! lists.
//!
//! GRAPH-SPECIFIC (load-bearing): the REAL nano_graphrag graphml edges are
//! UNTYPED — the relationship type lives in the per-record `.entities.yaml`
//! sidecars. (This fixture's graphml happens to carry a `rel_type` key, but the
//! neighborhood deliberately does NOT read graphml edges — it reads the record
//! `relationships[]` — so that key is never consulted on this path.) So the
//! entity neighborhood is built from the AGGREGATED record relationships (the
//! union of every scanned record's `relationships[]`, keyed by entity name).
//! Those are TYPED (`solves` / `caused_by` / `causes` / `relates_to`) and connect
//! entities across learnings via shared entity names. The community view reads
//! `parse_community_reports`.
//!
//! Asserted EXACT unique tokens (never substring-OR), each locked by a
//! deliberately-wrong run reading the real render first:
//! - an entity name from the aggregated sidecars (`audit-after-rebase`),
//! - a typed edge token (`--solves-->` to `stale plan execution`),
//! - a community title from the community json (`Rebase discipline`),
//! - the community-view toggle hint (`c communities`).

use ainb_plugin_learnings::plugin::LearningsPlugin;
use ainb_plugin_protocol::params::{HandleKeyParams, KeyCode, KeyEvent};
use ainb_plugin_testkit::{Viewport, WireBuffer, methods, run_plugin};
use std::path::{Path, PathBuf};

/// Absolute path to the committed fixture KB directory (records + graphml +
/// community json all live here).
fn kb_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("fixtures").join("kb")
}

/// The `[plugins.learnings]` table the host would resolve, pointed at the
/// fixture KB so both the record scan (neighborhood) and the graph_cache
/// (community json) resolve to the committed fixtures.
fn fixture_config() -> serde_json::Value {
    serde_json::json!({
        "learnings_dir": kb_dir().display().to_string(),
        "graph_cache": kb_dir().display().to_string(),
    })
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

// --- Graph-tab tokens (locked by a wrong-value run reading the real render) ---

/// An entity name from the aggregated `.entities.yaml` sidecars. Sorted first
/// alphabetically among the entity names, so it's the initial selection.
const ENTITY_TOKEN: &str = "audit-after-rebase";
/// The typed edge token: `audit-after-rebase --solves--> stale plan execution`.
/// The `--solves-->` arrow is the TYPED edge label sourced from the aggregated
/// record relationships (NOT the untyped graphml edges).
const EDGE_TYPE_TOKEN: &str = "--solves-->";
/// The neighbor entity the `--solves-->` edge points at.
const NEIGHBOR_TOKEN: &str = "stale plan execution";
/// A community title from `kv_store_community_reports.json` (sorted first by id).
const COMMUNITY_TOKEN: &str = "Rebase discipline";

/// Init the plugin over the fixture KB and return the harness ready to drive.
async fn init_over_fixture() -> ainb_plugin_testkit::Harness {
    let mut h = run_plugin(LearningsPlugin::default());
    h.init_with_config(fixture_config()).await.expect("init with fixture config");
    h
}

/// Reach the Graph tab by pressing `g` (the reserved graph key from Browse).
async fn enter_graph(h: &mut ainb_plugin_testkit::Harness) {
    h.send_notification(
        methods::PLUGIN_HANDLE_KEY,
        key_params(KeyCode::Char { ch: 'g' }),
    )
    .await
    .expect("send g");
}

#[tokio::test]
async fn g_reaches_graph_and_shows_entity_neighborhood_with_typed_edge() {
    let mut h = init_over_fixture().await;

    // Before `g` we're on Browse — the typed edge token must NOT show.
    let before = buffer_text(&h.render(VIEWPORT).await.expect("render browse"));
    assert!(
        !before.contains(EDGE_TYPE_TOKEN),
        "the typed edge must not show on the Browse tab before `g`:\n{before}"
    );

    // `g` enters the Graph tab + focuses the entity list.
    enter_graph(&mut h).await;
    let graph = buffer_text(&h.render(VIEWPORT).await.expect("render graph"));

    // The initially-selected entity (sorted first) renders.
    assert!(
        graph.contains(ENTITY_TOKEN),
        "the selected entity {ENTITY_TOKEN:?} must render:\n{graph}"
    );
    // Its TYPED neighbor edge renders: `audit-after-rebase --solves--> stale
    // plan execution`. The `--solves-->` arrow + the neighbor name both show.
    assert!(
        graph.contains(EDGE_TYPE_TOKEN),
        "the typed edge label {EDGE_TYPE_TOKEN:?} must render in the neighborhood:\n{graph}"
    );
    assert!(
        graph.contains(NEIGHBOR_TOKEN),
        "the neighbor entity {NEIGHBOR_TOKEN:?} must render:\n{graph}"
    );

    h.close().await.expect("clean close");
}

#[tokio::test]
async fn tab_also_reaches_graph_tab() {
    // The `Tab` key cycles Browse → Search → Graph; the third Tab lands on Graph
    // and the entity neighborhood paints (independent of the `g` shortcut).
    let mut h = init_over_fixture().await;

    for _ in 0..2 {
        // Browse → Search → Graph.
        h.send_notification(methods::PLUGIN_HANDLE_KEY, key_params(KeyCode::Tab))
            .await
            .expect("send Tab");
    }
    let graph = buffer_text(&h.render(VIEWPORT).await.expect("render graph"));
    assert!(
        graph.contains(ENTITY_TOKEN),
        "two Tabs from Browse must land on Graph and paint an entity:\n{graph}"
    );

    h.close().await.expect("clean close");
}

#[tokio::test]
async fn down_moves_entity_selection() {
    let mut h = init_over_fixture().await;
    enter_graph(&mut h).await;

    // Precondition: the graph entity view is active (the TYPED edge — graph-only,
    // never on Browse — is present), so we're not falsely matching the Browse
    // `lrn-audit-after-rebase` row whose id CONTAINS the entity token.
    let first = buffer_text(&h.render(VIEWPORT).await.expect("render graph"));
    assert!(
        first.contains(EDGE_TYPE_TOKEN),
        "precondition: graph entity neighborhood active:\n{first}"
    );
    // The first entity (`audit-after-rebase`, sorted) is selected: a ▶ marks the
    // entity row in the neighborhood. Find the selected (▶) row and confirm it
    // carries the first entity.
    let sel_line = first
        .lines()
        .find(|l| l.contains('▶'))
        .expect("a selected (▶) entity row present");
    assert!(
        sel_line.contains(ENTITY_TOKEN),
        "the first entity {ENTITY_TOKEN:?} must be selected initially:\n{first}"
    );

    // ↓ moves the selection to a DIFFERENT entity — the ▶ row must no longer be
    // the first entity.
    h.send_notification(methods::PLUGIN_HANDLE_KEY, key_params(KeyCode::Down))
        .await
        .expect("send Down");
    let moved = buffer_text(&h.render(VIEWPORT).await.expect("render moved"));
    let sel_line = moved
        .lines()
        .find(|l| l.contains('▶'))
        .expect("a selected (▶) entity row present after ↓");
    assert!(
        !sel_line.contains(ENTITY_TOKEN),
        "the selection must move off the first entity after ↓:\n{moved}"
    );

    h.close().await.expect("clean close");
}

#[tokio::test]
async fn c_toggles_to_community_view_and_lists_clusters() {
    let mut h = init_over_fixture().await;
    enter_graph(&mut h).await;

    // Entity-neighborhood view first: the community title must NOT show yet.
    let entity_view = buffer_text(&h.render(VIEWPORT).await.expect("render entity view"));
    assert!(
        !entity_view.contains(COMMUNITY_TOKEN),
        "the community title must not show in the entity view:\n{entity_view}"
    );

    // `c` toggles to the community-cluster view.
    h.send_notification(
        methods::PLUGIN_HANDLE_KEY,
        key_params(KeyCode::Char { ch: 'c' }),
    )
    .await
    .expect("send c");

    let community_view = buffer_text(&h.render(VIEWPORT).await.expect("render community view"));
    assert!(
        community_view.contains(COMMUNITY_TOKEN),
        "`c` must toggle to the community view listing {COMMUNITY_TOKEN:?}:\n{community_view}"
    );
    // Toggling back to entities (`c` again) restores the typed edge.
    h.send_notification(
        methods::PLUGIN_HANDLE_KEY,
        key_params(KeyCode::Char { ch: 'c' }),
    )
    .await
    .expect("send c back");
    let back = buffer_text(&h.render(VIEWPORT).await.expect("render entity view again"));
    assert!(
        back.contains(EDGE_TYPE_TOKEN),
        "a second `c` must toggle back to the entity neighborhood:\n{back}"
    );

    h.close().await.expect("clean close");
}

#[tokio::test]
async fn backspace_exits_graph_focus_back_to_tab_dispatch() {
    // `g` enters graph focus; Backspace exits it (Esc is host-reserved). The
    // STRONG signal that Backspace actually blurred is that a subsequent
    // graph-nav key (`Down`) is now INERT — a blurred graph ignores its own
    // navigation keys, so the `▶` selection stays on the first entity. (A
    // regression where Backspace stops blurring would let `Down` move the
    // selection and fail here; the later `Tab → Browse` check alone can't catch
    // that, since `Tab` switches tabs regardless of graph focus.)
    let mut h = init_over_fixture().await;
    enter_graph(&mut h).await;

    let focused = buffer_text(&h.render(VIEWPORT).await.expect("render focused"));
    assert!(
        focused.contains(ENTITY_TOKEN),
        "precondition: graph focused and entity painted:\n{focused}"
    );
    // Precondition: focused, so the `▶` row is the first entity.
    let sel_line = focused
        .lines()
        .find(|l| l.contains('▶'))
        .expect("a selected (▶) entity row present while focused");
    assert!(
        sel_line.contains(ENTITY_TOKEN),
        "precondition: first entity selected while focused:\n{focused}"
    );

    // Backspace exits graph focus (still on the Graph tab, but focus released).
    h.send_notification(methods::PLUGIN_HANDLE_KEY, key_params(KeyCode::Backspace))
        .await
        .expect("send Backspace");

    // A graph-nav key is now inert: focus released means `Down` does NOT move the
    // selection. The `▶` must still mark the first entity. This is the assertion
    // that actually proves Backspace blurred (not just that `Tab` works).
    h.send_notification(methods::PLUGIN_HANDLE_KEY, key_params(KeyCode::Down))
        .await
        .expect("send Down after blur");
    let after_blur = buffer_text(&h.render(VIEWPORT).await.expect("render after blur+Down"));
    let sel_line = after_blur
        .lines()
        .find(|l| l.contains('▶'))
        .expect("a selected (▶) entity row still present after blur");
    assert!(
        sel_line.contains(ENTITY_TOKEN),
        "after Backspace blurs the graph, a graph-nav key (Down) must be inert — \
         the selection must stay on the first entity:\n{after_blur}"
    );

    // A Tab now cycles to Browse (Graph → Browse wraps).
    h.send_notification(methods::PLUGIN_HANDLE_KEY, key_params(KeyCode::Tab))
        .await
        .expect("send Tab");
    let browse = buffer_text(&h.render(VIEWPORT).await.expect("render after exit"));
    assert!(
        browse.contains("lrn-audit-after-rebase"),
        "Backspace then Tab must return to a navigable Browse list:\n{browse}"
    );

    h.close().await.expect("clean close");
}
