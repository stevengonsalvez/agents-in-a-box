//! Radial-map integration tests — in-process render / handle_key / handle_mouse
//! over the fixture KB, driving the real `LearningsPlugin` through the SDK
//! `Server` via the testkit (no subprocess, no tmux).
//!
//! These complement the tmux tripwire: they assert the same deterministic
//! tokens the tripwire locks (the radial map's centre/neighbour boxes, a typed
//! edge label, recentre changing the centre, `h` flipping the node count, the
//! `[+N more]` overflow + `e` expansion, `o` opening Detail, and a mouse click
//! recentring) but exercise the full plugin pipeline directly.

use ainb_plugin_learnings::plugin::LearningsPlugin;
use ainb_plugin_protocol::params::{
    HandleKeyParams, HandleMouseParams, KeyCode, KeyEvent, MouseButton, MouseEvent, MouseKind,
};
use ainb_plugin_testkit::{Viewport, WireBuffer, methods, run_plugin};
use std::path::{Path, PathBuf};

fn kb_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("fixtures").join("kb")
}

fn fixture_config() -> serde_json::Value {
    serde_json::json!({
        "learnings_dir": kb_dir().display().to_string(),
        "graph_cache": kb_dir().display().to_string(),
    })
}

/// A separate KB holding only the `zzz-graph-hub` record (18 neighbours) so the
/// overflow/`e` path can be exercised without disturbing the shared fixture KB
/// the other tests (and the data-layer tests) count.
fn hub_config() -> serde_json::Value {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("kb-hub");
    serde_json::json!({
        "learnings_dir": dir.display().to_string(),
        "graph_cache": dir.display().to_string(),
    })
}

/// Whole buffer to a newline-joined string, row by row (so tokens match across
/// cell boundaries within a row).
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

/// Find the `(col, row)` where `token` starts in the rendered buffer.
fn find_token_xy(buf: &WireBuffer, token: &str) -> Option<(u16, u16)> {
    let mut rows: Vec<String> = vec![String::new(); buf.height as usize];
    for (coord, cell) in &buf.cells {
        if let Some(row) = rows.get_mut(coord.y as usize) {
            while row.chars().count() < coord.x as usize {
                row.push(' ');
            }
            row.push_str(&cell.symbol);
        }
    }
    for (y, row) in rows.iter().enumerate() {
        if let Some(byte_idx) = row.find(token) {
            let col = row[..byte_idx].chars().count();
            return Some((col as u16, y as u16));
        }
    }
    None
}

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

fn click_params(col: u16, row: u16) -> HandleMouseParams {
    HandleMouseParams {
        screen_id: "learnings".to_string(),
        mouse: MouseEvent {
            kind: MouseKind::Down {
                button: MouseButton::Left,
            },
            col,
            row,
            mods: 0,
        },
        generation: 0,
    }
}

const VIEWPORT: Viewport = Viewport {
    width: 120,
    height: 40,
};

/// Diagonal arrowheads only appear on the radial edges — never in the help-bar
/// legend (which uses the cardinals `↑↓←→`), so they're a safe arrowhead token.
const DIAGONAL_ARROWS: [char; 4] = ['↗', '↖', '↘', '↙'];

async fn init_over_fixture() -> ainb_plugin_testkit::Harness {
    let mut h = run_plugin(LearningsPlugin::default());
    h.init_with_config(fixture_config()).await.expect("init with fixture config");
    h
}

async fn send_key(h: &mut ainb_plugin_testkit::Harness, code: KeyCode) {
    h.send_notification(methods::PLUGIN_HANDLE_KEY, key_params(code))
        .await
        .expect("send key");
}

/// Reach the Graph tab (`g`) and toggle into the radial map (`v`).
async fn enter_map(h: &mut ainb_plugin_testkit::Harness) {
    send_key(h, KeyCode::Char { ch: 'g' }).await;
    send_key(h, KeyCode::Char { ch: 'v' }).await;
}

#[tokio::test]
async fn v_opens_radial_map_with_centre_neighbour_and_typed_edge() {
    let mut h = init_over_fixture().await;
    enter_map(&mut h).await;
    let map = buffer_text(&h.render(VIEWPORT).await.expect("render map"));

    assert!(
        map.contains("centre: audit-after-rebase"),
        "map header:\n{map}"
    );
    assert!(map.contains("[audit-after-rebase]"), "centre box:\n{map}");
    assert!(
        map.contains("[stale plan execution]"),
        "neighbour box:\n{map}"
    );
    assert!(map.contains("solves"), "typed edge label:\n{map}");

    h.close().await.expect("clean close");
}

#[tokio::test]
async fn enter_recentres_on_the_selected_neighbour() {
    let mut h = init_over_fixture().await;
    enter_map(&mut h).await;
    // Down selects the ring neighbour; Enter recentres on it.
    send_key(&mut h, KeyCode::Down).await;
    send_key(&mut h, KeyCode::Enter).await;
    let map = buffer_text(&h.render(VIEWPORT).await.expect("render recentred"));
    assert!(
        map.contains("centre: stale plan execution"),
        "centre token must change to the recentred entity:\n{map}"
    );
    h.close().await.expect("clean close");
}

#[tokio::test]
async fn h_flips_the_node_count_between_hops() {
    let mut h = init_over_fixture().await;
    enter_map(&mut h).await;
    // hop 1: audit-after-rebase has exactly one direct neighbour.
    let hop1 = buffer_text(&h.render(VIEWPORT).await.expect("render hop1"));
    assert!(hop1.contains("nodes:1"), "hop 1 node count:\n{hop1}");
    // h → hop 2 reaches git pull --rebase via stale plan execution.
    send_key(&mut h, KeyCode::Char { ch: 'h' }).await;
    let hop2 = buffer_text(&h.render(VIEWPORT).await.expect("render hop2"));
    assert!(hop2.contains("nodes:2"), "hop 2 node count:\n{hop2}");
    h.close().await.expect("clean close");
}

#[tokio::test]
async fn mouse_click_on_a_neighbour_recentres() {
    let mut h = init_over_fixture().await;
    enter_map(&mut h).await;
    // Render first so the plugin stashes the map's painted rect for hit-testing,
    // then click where the neighbour box actually rendered.
    let map = h.render(VIEWPORT).await.expect("render map");
    let (col, row) =
        find_token_xy(&map, "[stale plan execution]").expect("neighbour box on screen");
    h.send_notification(methods::PLUGIN_HANDLE_MOUSE, click_params(col, row))
        .await
        .expect("send click");
    let after = buffer_text(&h.render(VIEWPORT).await.expect("render after click"));
    assert!(
        after.contains("centre: stale plan execution"),
        "a click on the neighbour box must recentre on it:\n{after}"
    );
    h.close().await.expect("clean close");
}

#[tokio::test]
async fn o_opens_detail_for_the_entity() {
    let mut h = init_over_fixture().await;
    enter_map(&mut h).await;
    // audit-after-rebase is cited by exactly one learning → `o` opens Detail.
    send_key(&mut h, KeyCode::Char { ch: 'o' }).await;
    let detail = buffer_text(&h.render(VIEWPORT).await.expect("render detail"));
    assert!(
        detail.contains("Audit after large rebase"),
        "Detail pane must show the learning's title after `o`:\n{detail}"
    );
    h.close().await.expect("clean close");
}

#[tokio::test]
async fn o_on_the_overflow_node_is_a_clean_noop() {
    // Centre the hub (18 neighbours → a `[+N more]` overflow), select that
    // synthetic node (Down into the ring, then Left wraps to the last slot =
    // overflow), and press `o`. The overflow isn't a real entity, so nothing
    // opens — the map stays put.
    let mut h = run_plugin(LearningsPlugin::default());
    h.init_with_config(hub_config()).await.expect("init with hub config");
    send_key(&mut h, KeyCode::Char { ch: 'g' }).await;
    send_key(&mut h, KeyCode::Char { ch: 'v' }).await;
    send_key(&mut h, KeyCode::Down).await; // first ring node
    send_key(&mut h, KeyCode::Left).await; // wrap back to the overflow node
    send_key(&mut h, KeyCode::Char { ch: 'o' }).await;
    let after = buffer_text(&h.render(VIEWPORT).await.expect("render after o"));
    assert!(
        after.contains("centre: zzz-graph-hub"),
        "`o` on the overflow node must not open Detail/picker — the map stays:\n{after}"
    );
    h.close().await.expect("clean close");
}

#[tokio::test]
async fn hub_overflows_and_e_expands_it() {
    // Init over the hub-only KB so the lone entity is the centre on `v`.
    let mut h = run_plugin(LearningsPlugin::default());
    h.init_with_config(hub_config()).await.expect("init with hub config");
    send_key(&mut h, KeyCode::Char { ch: 'g' }).await;
    send_key(&mut h, KeyCode::Char { ch: 'v' }).await;
    let capped = buffer_text(&h.render(VIEWPORT).await.expect("render hub map"));
    assert!(
        capped.contains("centre: zzz-graph-hub"),
        "centred on the hub:\n{capped}"
    );
    // 18 neighbours, cap 15 → a `[+3 more]` overflow node.
    assert!(capped.contains("[+3 more]"), "overflow node:\n{capped}");
    // A hub fans many edges out at angles → at least one diagonal arrowhead
    // (cardinals would collide with the help-bar legend; diagonals never do).
    assert!(
        DIAGONAL_ARROWS.iter().any(|a| capped.contains(*a)),
        "a directed edge must render a diagonal arrowhead:\n{capped}"
    );

    // `e` expands the overflow — the `[+N more]` node is gone.
    send_key(&mut h, KeyCode::Char { ch: 'e' }).await;
    let expanded = buffer_text(&h.render(VIEWPORT).await.expect("render expanded"));
    assert!(
        !expanded.contains("[+3 more]"),
        "`e` must expand the overflow node away:\n{expanded}"
    );
    h.close().await.expect("clean close");
}
