//! P6 Detail-pane tests — in-process render / handle_key over the fixture KB.
//!
//! Drives the real `LearningsPlugin` through the SDK `Server` via the testkit
//! (no subprocess, no tmux). The plugin is `init`ed with a `config` table
//! pointing `learnings_dir` at the committed fixture KB so the Browse list +
//! Detail pane render deterministic fixture content.
//!
//! Behaviour under test (TDD plan §P6 / design mock C3):
//! - Pressing `Enter` on the selected Browse row opens a Detail view that
//!   renders the record's full `body_md`, an entities list, the typed
//!   relationships (an `X --solves--> Y` line sourced from
//!   `record.relationships` — the `.entities.yaml` sidecar, NOT the graphml),
//!   and a provenance line (source_tool / path / confidence).
//! - `Esc` (and `Backspace`) close Detail back to the list with the prior
//!   selection preserved.
//!
//! NOTE on the close key: the host reserves `Esc` (it pops the whole plugin
//! screen back to home and never forwards Esc to the plugin — see
//! `ainb-core/src/app/screens/builtin.rs::is_host_reserved_key`). Burndown's
//! exemplar therefore binds its in-plugin "pop" to `Backspace`. The Detail
//! pane mirrors that: `Backspace` is the host-forwarded close key, and the
//! plugin ALSO handles `Esc` defensively so a direct injection (this testkit,
//! which bypasses the host reservation) closes too. The tmux tripwire uses
//! `Backspace`, because a real Esc there would pop straight to home.
//!
//! Asserted EXACT unique tokens (never substring-OR):
//! - body          → `## Solution`
//! - entity        → `git pull --rebase`
//! - relationship  → `--solves-->` + `stale plan execution`
//! - provenance    → `feedback_audit_after_rebase.md`

use ainb_plugin_learnings::plugin::LearningsPlugin;
use ainb_plugin_protocol::params::{HandleKeyParams, KeyCode, KeyEvent};
use ainb_plugin_testkit::{Viewport, WireBuffer, methods, run_plugin};
use std::path::{Path, PathBuf};

/// Absolute path to the committed fixture KB directory.
fn kb_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("fixtures").join("kb")
}

/// The `[plugins.learnings]` table the host would resolve, pointed at the
/// fixture KB so the Browse list + Detail pane are deterministic.
fn fixture_config() -> serde_json::Value {
    serde_json::json!({
        "learnings_dir": kb_dir().display().to_string(),
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

// --- Detail-pane tokens (first row = `lrn-audit-after-rebase`, sorted by id) ---

/// A body token: a `##` heading present only in the record's markdown body
/// (never in the list / chips / help bar). Proves the full `body_md` rendered.
const BODY_TOKEN: &str = "## Solution";
/// An entity name from the `.entities.yaml` sidecar. Chosen to NOT be a
/// substring of any learning id (`audit-after-rebase` would alias the id).
const ENTITY_TOKEN: &str = "git pull --rebase";
/// The typed-relationship arrow (sidecar `relationships[].type`). Combined
/// with [`REL_TARGET_TOKEN`] for an exact, unique relationship assertion.
const REL_ARROW_TOKEN: &str = "--solves-->";
/// The target entity of the asserted relationship.
const REL_TARGET_TOKEN: &str = "stale plan execution";
/// A provenance token: the source_path basename — extremely unique, present
/// only in the Detail provenance line.
const PROVENANCE_TOKEN: &str = "feedback_audit_after_rebase.md";

/// Helper: init the plugin over the fixture KB and return the harness.
async fn init_over_fixture() -> ainb_plugin_testkit::Harness {
    let mut h = run_plugin(LearningsPlugin::default());
    h.init_with_config(fixture_config()).await.expect("init with fixture config");
    h
}

#[tokio::test]
async fn enter_opens_detail_with_body_entities_rels_and_provenance() {
    let mut h = init_over_fixture().await;

    // Before Enter, the Browse list is shown — the Detail body token must NOT
    // be present (the list renders ids + confidence, not `## Solution`).
    let list = buffer_text(&h.render(VIEWPORT).await.expect("render list"));
    assert!(
        !list.contains(BODY_TOKEN),
        "Browse list must not render the Detail body token before Enter:\n{list}"
    );

    // Press Enter on the selected (first) row → open Detail.
    h.send_notification(methods::PLUGIN_HANDLE_KEY, key_params(KeyCode::Enter))
        .await
        .expect("send Enter");

    let detail = buffer_text(&h.render(VIEWPORT).await.expect("render detail"));

    // Full body rendered (a `##` heading from the markdown body).
    assert!(
        detail.contains(BODY_TOKEN),
        "Detail must render the record body token {BODY_TOKEN:?}:\n{detail}"
    );
    // Entities list (from the `.entities.yaml` sidecar).
    assert!(
        detail.contains(ENTITY_TOKEN),
        "Detail must render the entity {ENTITY_TOKEN:?}:\n{detail}"
    );
    // Typed relationship `X --solves--> Y` (sidecar, NOT graphml).
    assert!(
        detail.contains(REL_ARROW_TOKEN),
        "Detail must render the typed-relationship arrow {REL_ARROW_TOKEN:?}:\n{detail}"
    );
    assert!(
        detail.contains(REL_TARGET_TOKEN),
        "Detail must render the relationship target {REL_TARGET_TOKEN:?}:\n{detail}"
    );
    // Provenance line (source_tool / path / confidence).
    assert!(
        detail.contains(PROVENANCE_TOKEN),
        "Detail must render the provenance source path {PROVENANCE_TOKEN:?}:\n{detail}"
    );

    h.close().await.expect("clean close");
}

#[tokio::test]
async fn esc_closes_detail_back_to_list() {
    // The testkit injects keys directly (bypassing the host's Esc reservation),
    // so Esc reaches the plugin here. The Detail pane handles it as a close.
    let mut h = init_over_fixture().await;

    h.send_notification(methods::PLUGIN_HANDLE_KEY, key_params(KeyCode::Enter))
        .await
        .expect("send Enter");
    let detail = buffer_text(&h.render(VIEWPORT).await.expect("render detail"));
    assert!(
        detail.contains(BODY_TOKEN),
        "precondition: Detail open with body token:\n{detail}"
    );

    // Esc closes Detail → back to the list (body token gone, list id back).
    h.send_notification(methods::PLUGIN_HANDLE_KEY, key_params(KeyCode::Esc))
        .await
        .expect("send Esc");
    let back = buffer_text(&h.render(VIEWPORT).await.expect("render after esc"));
    assert!(
        !back.contains(BODY_TOKEN),
        "Esc must close the Detail pane (body token gone):\n{back}"
    );
    assert!(
        back.contains("lrn-audit-after-rebase"),
        "after closing Detail the Browse list must be back:\n{back}"
    );

    h.close().await.expect("clean close");
}

#[tokio::test]
async fn backspace_closes_detail_back_to_list() {
    // Backspace is the host-FORWARDED close key (Esc is host-reserved). This
    // is the path the real TUI + tmux tripwire exercise.
    let mut h = init_over_fixture().await;

    h.send_notification(methods::PLUGIN_HANDLE_KEY, key_params(KeyCode::Enter))
        .await
        .expect("send Enter");
    let detail = buffer_text(&h.render(VIEWPORT).await.expect("render detail"));
    assert!(
        detail.contains(BODY_TOKEN),
        "precondition: Detail open with body token:\n{detail}"
    );

    h.send_notification(methods::PLUGIN_HANDLE_KEY, key_params(KeyCode::Backspace))
        .await
        .expect("send Backspace");
    let back = buffer_text(&h.render(VIEWPORT).await.expect("render after backspace"));
    assert!(
        !back.contains(BODY_TOKEN),
        "Backspace must close the Detail pane (body token gone):\n{back}"
    );
    assert!(
        back.contains("lrn-audit-after-rebase"),
        "after closing Detail the Browse list must be back:\n{back}"
    );

    h.close().await.expect("clean close");
}

#[tokio::test]
async fn detail_preserves_selection_after_close() {
    // Move selection to the 2nd row, open Detail there, close it, and confirm
    // the selection is still on the 2nd row (selection preserved across the
    // open→close round-trip).
    let mut h = init_over_fixture().await;

    // ↓ moves selection to row 2 (lrn-claude-plugin-autowire).
    h.send_notification(methods::PLUGIN_HANDLE_KEY, key_params(KeyCode::Down))
        .await
        .expect("send Down");

    // Enter opens Detail for the SECOND record. Its body has a distinct
    // heading set; assert its unique key_insight-derived body token.
    h.send_notification(methods::PLUGIN_HANDLE_KEY, key_params(KeyCode::Enter))
        .await
        .expect("send Enter");
    let detail = buffer_text(&h.render(VIEWPORT).await.expect("render detail row2"));
    assert!(
        detail.contains("${CLAUDE_PLUGIN_ROOT}"),
        "Detail must render the SECOND record's body (autowire), proving Enter \
         opened the SELECTED row, not row 0:\n{detail}"
    );

    // Backspace closes → list, with selection still on row 2 (the ▶ marks the
    // autowire row, not the audit row).
    h.send_notification(methods::PLUGIN_HANDLE_KEY, key_params(KeyCode::Backspace))
        .await
        .expect("send Backspace");
    let back = buffer_text(&h.render(VIEWPORT).await.expect("render after close"));
    let autowire_line = back
        .lines()
        .find(|l| l.contains("lrn-claude-plugin-autowire"))
        .expect("autowire row present after close");
    assert!(
        autowire_line.contains('▶'),
        "selection must be preserved on row 2 after the Detail round-trip:\n{back}"
    );

    h.close().await.expect("clean close");
}
