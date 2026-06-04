//! P5 Browse-tab tests — in-process render / handle_key over the fixture KB.
//!
//! These drive the real `LearningsPlugin` through the SDK `Server` via the
//! testkit (no subprocess, no tmux). The plugin is `init`ed with a `config`
//! table pointing `learnings_dir` at the committed fixture KB
//! (`tests/fixtures/kb`) so the Browse list shows deterministic fixture ids.
//!
//! Asserted EXACT unique tokens (never substring-OR):
//! - a fixture learning id row (`lrn-audit-after-rebase`),
//! - the tab bar (`Browse` / `Search` / `Graph`),
//! - the filter-chip bar (`scope` chip), narrowing on `f`,
//! - the `▶` selection indicator moving on `↓`.

use ainb_plugin_learnings::plugin::LearningsPlugin;
use ainb_plugin_protocol::params::{HandleKeyParams, KeyCode, KeyEvent};
use ainb_plugin_testkit::{Viewport, WireBuffer, methods, run_plugin};
use std::path::{Path, PathBuf};

/// Absolute path to the committed fixture KB directory.
fn kb_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("fixtures").join("kb")
}

/// The `[plugins.learnings]` table the host would resolve, pointed at the
/// fixture KB so the Browse list is deterministic.
fn fixture_config() -> serde_json::Value {
    serde_json::json!({
        "learnings_dir": kb_dir().display().to_string(),
    })
}

/// Render the whole buffer to a single newline-joined string, row by row,
/// so token assertions can match across cell boundaries within a row.
fn buffer_text(buf: &WireBuffer) -> String {
    let mut rows: Vec<String> = vec![String::new(); buf.height as usize];
    for (coord, cell) in &buf.cells {
        if let Some(row) = rows.get_mut(coord.y as usize) {
            // Pad to the cell's x so multi-cell tokens line up.
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

#[tokio::test]
async fn browse_renders_tabs_chips_and_a_fixture_id() {
    let mut h = run_plugin(LearningsPlugin::default());
    h.init_with_config(fixture_config()).await.expect("init with fixture config");

    let buf = h.render(VIEWPORT).await.expect("render ok");
    let text = buffer_text(&buf);

    // Tabbed shell: the three tab labels render.
    assert!(text.contains("Browse"), "Browse tab label missing:\n{text}");
    assert!(text.contains("Search"), "Search tab label missing:\n{text}");
    assert!(text.contains("Graph"), "Graph tab label missing:\n{text}");

    // Filter-chip bar: the scope facet chip renders.
    assert!(
        text.contains("scope"),
        "filter-chip bar (scope) missing:\n{text}"
    );

    // A known fixture learning id renders in the Browse list.
    assert!(
        text.contains("lrn-audit-after-rebase"),
        "fixture id row missing:\n{text}"
    );

    h.close().await.expect("clean close");
}

#[tokio::test]
async fn pressing_f_cycles_scope_chip_and_narrows_list() {
    let mut h = run_plugin(LearningsPlugin::default());
    h.init_with_config(fixture_config()).await.expect("init with fixture config");

    // Before filtering, the `project`-scoped record is visible.
    let before = buffer_text(&h.render(VIEWPORT).await.expect("render"));
    assert!(
        before.contains("lrn-tokio-runtime-panic"),
        "project-scoped record must be visible before filtering:\n{before}"
    );

    // Press `f` once: cycles the scope chip to the first concrete value
    // (`universal`). The list narrows to universal-scoped records, dropping
    // the `project`-scoped `lrn-tokio-runtime-panic`.
    h.send_notification(
        methods::PLUGIN_HANDLE_KEY,
        key_params(KeyCode::Char { ch: 'f' }),
    )
    .await
    .expect("send f");

    let after = buffer_text(&h.render(VIEWPORT).await.expect("render"));

    // The active scope chip now shows `universal`.
    assert!(
        after.contains("scope[universal]"),
        "scope chip must show the active `universal` value after `f`:\n{after}"
    );
    // A universal record stays.
    assert!(
        after.contains("lrn-audit-after-rebase"),
        "universal record must remain after scope=universal filter:\n{after}"
    );
    // The project-scoped record is filtered out.
    assert!(
        !after.contains("lrn-tokio-runtime-panic"),
        "project-scoped record must be hidden after scope=universal filter:\n{after}"
    );

    h.close().await.expect("clean close");
}

#[tokio::test]
async fn browse_renders_bottom_help_bar() {
    // P5 review (major): the Browse tab MUST render a bottom help/key-hint bar
    // (ainb-tui style guide mandatory pattern + design mock C3). It must list
    // the keys P5 actually implements now (f filter, Tab pane, ↑↓ move) and
    // stay HONEST about the keys deferred to P6–P8 (/ search, g graph, ⏎ open)
    // by rendering them muted/disabled. Exact unique tokens — never substring-OR.
    let mut h = run_plugin(LearningsPlugin::default());
    h.init_with_config(fixture_config()).await.expect("init with fixture config");

    let text = buffer_text(&h.render(VIEWPORT).await.expect("render ok"));

    // Implemented-now keys: each rendered as `<key> <description>`.
    for token in ["↑↓ move", "f filter", "Tab pane"] {
        assert!(
            text.contains(token),
            "active help-bar token {token:?} missing:\n{text}"
        );
    }

    // Deferred keys are still shown (so the bar reflects the design mock) but
    // muted/disabled until their phases wire them — present in the render.
    for token in ["⏎ open", "/ search", "g graph"] {
        assert!(
            text.contains(token),
            "deferred help-bar token {token:?} missing:\n{text}"
        );
    }

    h.close().await.expect("clean close");
}

#[tokio::test]
async fn arrow_down_moves_the_selection_indicator() {
    let mut h = run_plugin(LearningsPlugin::default());
    h.init_with_config(fixture_config()).await.expect("init with fixture config");

    // Initially the first row (sorted by id → lrn-audit-after-rebase) is
    // selected: the ▶ indicator precedes it.
    let first = buffer_text(&h.render(VIEWPORT).await.expect("render"));
    let audit_line = first
        .lines()
        .find(|l| l.contains("lrn-audit-after-rebase"))
        .expect("audit row present");
    assert!(
        audit_line.contains('▶'),
        "selection indicator must mark the first row initially:\n{first}"
    );

    // Press ↓: selection moves to the second row (lrn-claude-plugin-autowire).
    h.send_notification(methods::PLUGIN_HANDLE_KEY, key_params(KeyCode::Down))
        .await
        .expect("send down");

    let moved = buffer_text(&h.render(VIEWPORT).await.expect("render"));
    let audit_line = moved
        .lines()
        .find(|l| l.contains("lrn-audit-after-rebase"))
        .expect("audit row present");
    let autowire_line = moved
        .lines()
        .find(|l| l.contains("lrn-claude-plugin-autowire"))
        .expect("autowire row present");
    assert!(
        !audit_line.contains('▶'),
        "first row must lose the selection indicator after ↓:\n{moved}"
    );
    assert!(
        autowire_line.contains('▶'),
        "second row must gain the selection indicator after ↓:\n{moved}"
    );

    h.close().await.expect("clean close");
}
