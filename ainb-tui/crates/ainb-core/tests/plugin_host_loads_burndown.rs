//! Integration smoke test: PluginHost can load the bundled burndown plugin.
//!
//! Phase 3 acceptance gate (host loads it in test). This test points
//! `AINB_PLUGIN_ROOT` at the workspace's `dist/plugins/` directory if it
//! exists and asserts that `init_plugin_host` returns the burndown plugin
//! in `outcome.loaded`. Skipped (with a soft pass) when `dist/plugins/`
//! hasn't been built yet — keeps the test useful in CI without forcing
//! every developer to run the WASI toolchain.

use std::path::PathBuf;

#[test]
fn loads_burndown_plugin_when_dist_present() {
    let dist = workspace_dist_plugins();
    let burndown = dist.join("burndown");
    if !burndown.join("plugin.wasm").exists() {
        eprintln!(
            "skipping: dist/plugins/burndown/plugin.wasm not built yet \
             (run scripts/build-plugins.sh to enable this test)"
        );
        return;
    }

    std::env::set_var("AINB_PLUGIN_ROOT", &dist);
    let (mut host, outcome) = ainb::plugins::init_plugin_host();

    assert!(
        outcome.failed.is_empty(),
        "expected zero plugin load failures, got: {:?}",
        outcome
            .failed
            .iter()
            .map(|(n, e)| format!("{n}: {e:#}"))
            .collect::<Vec<_>>()
    );
    assert_eq!(outcome.loaded.len(), 1, "expected exactly one plugin loaded");
    assert_eq!(
        outcome.loaded[0].to_string(),
        "burndown",
        "expected burndown plugin id"
    );
    assert!(
        host.get("burndown").is_some(),
        "host should expose the loaded plugin by id"
    );

    // Drive the plugin's `_render` end-to-end and assert the host stashed
    // a Screen-target WireBuffer. This is the round-trip the cutover relies
    // on: plugin builds buffer → ainb_render_buffer → host stash → take_render.
    host.render_plugin("burndown").expect("_render runs cleanly");
    let buf = host
        .take_render("burndown", ainb_plugin_api::RenderTarget::Screen)
        .expect("plugin painted to Screen target");
    assert!(buf.is_consistent(), "WireBuffer width*height matches cells.len()");
    // _render targets 80x24 (matches snapshot baselines). The plugin paints
    // through `crate::ui::render`; without a UsageData snapshot pushed in
    // via `_handle_event`, the screen renders the loading state — which
    // still produces a fully-populated 80*24 buffer.
    assert_eq!(buf.width, 80, "wire buffer width matches plugin render area");
    assert_eq!(buf.height, 24, "wire buffer height matches plugin render area");
    assert_eq!(buf.cells.len(), 80 * 24, "buffer cell count");

    std::env::remove_var("AINB_PLUGIN_ROOT");
}

fn workspace_dist_plugins() -> PathBuf {
    // CARGO_MANIFEST_DIR points at crates/ainb-core; the workspace root is
    // two parents up.
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .and_then(|p| p.parent())
        .map(|p| p.join("dist").join("plugins"))
        .expect("workspace root resolvable")
}
