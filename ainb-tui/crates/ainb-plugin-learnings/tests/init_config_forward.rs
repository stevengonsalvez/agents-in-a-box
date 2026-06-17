//! P3b — the SDK must forward the resolved `PluginInitParams.config` into
//! `Plugin::on_init`, and the learnings plugin must apply it.
//!
//! P1 already injects the resolved `[plugins.learnings]` table onto
//! `PluginInitParams.config` on the wire, and the plugin already has a
//! tested [`LearningsPlugin::apply_init_config`]. But the SDK `on_init`
//! contract historically forwarded only `granted_capabilities`, so the
//! plugin's `on_init` was a no-op and the injected config never reached
//! runtime state.
//!
//! This drives the **real** wire path through the SDK `Server` (via the
//! testkit) with a non-default config payload and asserts the plugin's
//! internal [`LearningsConfig`] reflects it after init — the end-to-end
//! proof that config now flows host → wire → `on_init` → plugin state.

use ainb_plugin_learnings::config::LearningsConfig;
use ainb_plugin_learnings::plugin::LearningsPlugin;
use ainb_plugin_testkit::run_plugin_with_state;

/// Init with a non-default `[plugins.learnings]` table and require the
/// plugin's resolved config to reflect every overridden key, while absent
/// keys keep their schema defaults.
#[tokio::test]
async fn on_init_applies_injected_config_to_plugin_state() {
    let (mut h, state) = run_plugin_with_state(LearningsPlugin::default());

    // Sanity: before init the plugin holds the schema defaults.
    assert_eq!(
        state.lock().await.config(),
        &LearningsConfig::default(),
        "fresh plugin must start at schema defaults"
    );

    // Drive `plugin/init` with a resolved table that overrides two keys.
    let injected = serde_json::json!({
        "learnings_dir": "/tmp/x",
        "qmd_collection": "team-notes",
    });
    let init = h.init_with_config(injected.clone()).await.expect("init ok");
    assert_eq!(init["name"], "learnings", "manifest name echo");

    // After init, the plugin's internal config must reflect the injection.
    // FAILS today: `on_init` is a no-op, so the SDK never forwards the
    // config and `apply_init_config` is never called.
    let guard = state.lock().await;
    let cfg = guard.config();
    assert_eq!(
        cfg.learnings_dir, "/tmp/x",
        "on_init must apply the injected learnings_dir"
    );
    assert_eq!(
        cfg.qmd_collection, "team-notes",
        "on_init must apply the injected qmd_collection"
    );
    // Keys absent from the injection keep their schema defaults.
    assert_eq!(
        cfg.qmd_index, "~/.cache/qmd/index.sqlite",
        "absent key keeps schema default"
    );
    assert_eq!(
        cfg.graph_cache, "~/.learnings/nano_graphrag_cache",
        "absent key keeps schema default"
    );
    drop(guard);

    h.close().await.expect("clean close");
}
