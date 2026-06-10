//! P3 scaffold tests — manifest schema, empty render, config injection.
//!
//! Three legs of the phase RED set (TDD plan §P3):
//!
//! 1. `test_manifest_parses` — the on-disk `manifest.toml` decodes into
//!    the protocol `Manifest` and declares the exact surface the design
//!    spec (§C1) requires: `screens = ["learnings"]`, `read_paths`,
//!    `spawn_subprocess = ["qmd", "learnings"]`, `write_plugin_data`,
//!    `event_bus`, `/recall` + `/memory` commands, NO `cli_namespaces`,
//!    and a `[config]` schema with the four KB-path keys.
//! 2. `test_empty_render_smoke` — render on an 80×24 viewport (driven
//!    in-process through the SDK `Server` via the testkit) returns a
//!    `WireBuffer` carrying the unique title token `🧠 Learnings`.
//! 3. `test_init_consumes_config` — the typed parse path the plugin's
//!    `on_init` consumes turns an injected `[plugins.learnings]` table
//!    into a `LearningsConfig`, defaulting absent keys.

use ainb_plugin_learnings::config::LearningsConfig;
use ainb_plugin_learnings::plugin::LearningsPlugin;
use ainb_plugin_protocol::manifest::{CapabilityGrant, ConfigKind, Manifest, SpawnMode};
use ainb_plugin_testkit::{Viewport, run_plugin};

const MANIFEST_TOML: &str = include_str!("../manifest.toml");

/// The literal title token the empty-state shell must paint. Asserted as
/// a literal (not via the crate's `TITLE_TOKEN` const) so a regression in
/// that const can't make the test tautologically pass.
const EXPECTED_TITLE_TOKEN: &str = "🧠 Learnings";

#[test]
fn test_manifest_parses() {
    let m: Manifest = toml::from_str(MANIFEST_TOML).expect("manifest.toml must decode");

    // Identity + ABI.
    assert_eq!(m.plugin.name, "learnings");
    assert_eq!(m.plugin.version, "0.1.0");
    assert_eq!(m.plugin.abi_version, 2);

    // Capabilities (design §C1 / Part B).
    assert_eq!(
        m.capabilities.read_paths.allow_list(),
        Some(&["~/.learnings".to_string(), "~/.cache/qmd".to_string()][..]),
        "read_paths must be the path-scoped list form",
    );
    assert!(
        m.capabilities.read_paths.is_granted(),
        "read_paths list form must be granted",
    );
    assert_eq!(
        m.capabilities.spawn_subprocess.allow_list(),
        Some(&["qmd".to_string(), "learnings".to_string()][..]),
        "spawn_subprocess must allow qmd + learnings",
    );
    assert!(
        matches!(
            m.capabilities.write_plugin_data,
            CapabilityGrant::Bool(true)
        ),
        "write_plugin_data required",
    );
    assert!(
        matches!(m.capabilities.event_bus, CapabilityGrant::Bool(true)),
        "event_bus required",
    );

    // Surfaces (design §C1 / §C4). TUI-only — NO cli_namespaces.
    assert_eq!(m.provides.screens, vec!["learnings".to_string()]);
    assert_eq!(
        m.provides.commands,
        vec!["/recall".to_string(), "/memory".to_string()],
    );
    assert!(
        m.provides.cli_namespaces.is_empty(),
        "learnings is TUI-only — must NOT declare a cli_namespace",
    );
    assert!(m.subscribes.snapshots.is_empty());

    // Lifecycle.
    assert_eq!(m.lifecycle.spawn, SpawnMode::Lazy);
    assert_eq!(m.lifecycle.idle_reap_secs, 600);

    // [config] schema (design §A1) — the four KB-path keys, in order.
    let keys: Vec<&str> = m.config.iter().map(|f| f.key.as_str()).collect();
    assert_eq!(
        keys,
        [
            "learnings_dir",
            "qmd_index",
            "qmd_collection",
            "graph_cache"
        ],
        "config schema must declare the four KB keys in spec order",
    );
    let by_key = |k: &str| m.config.iter().find(|f| f.key == k).expect("config key present");
    assert_eq!(by_key("learnings_dir").kind, ConfigKind::Path);
    assert_eq!(by_key("qmd_index").kind, ConfigKind::Path);
    assert_eq!(by_key("qmd_collection").kind, ConfigKind::String);
    assert_eq!(by_key("graph_cache").kind, ConfigKind::Path);
    // Defaults wired (used by the host Settings UI + plugin fallback).
    assert_eq!(
        by_key("learnings_dir").default,
        "~/.learnings/documents/learnings"
    );
    assert_eq!(by_key("qmd_collection").default, "learnings");
}

#[tokio::test]
async fn test_empty_render_smoke() {
    let mut h = run_plugin(LearningsPlugin::default());

    let init = h.init().await.expect("init ok");
    assert_eq!(init["name"], "learnings", "manifest name echo");
    assert_eq!(init["version"], "0.1.0", "manifest version echo");

    let buf = h.render(Viewport::new(80, 24)).await.expect("render ok");
    assert_eq!(buf.width, 80);
    assert_eq!(buf.height, 24);

    // The title token must render in the first row. Build the row-0 string
    // from the sparse cell stream and assert the EXACT unique token —
    // never a substring-OR.
    let row0: String = buf
        .cells
        .iter()
        .filter(|(c, _)| c.y == 0)
        .map(|(_, cell)| cell.symbol.as_str())
        .collect();
    assert!(
        row0.contains(EXPECTED_TITLE_TOKEN),
        "empty render must paint the title token {EXPECTED_TITLE_TOKEN:?} in row 0; got: {row0:?}"
    );

    h.close().await.expect("clean close");
}

#[test]
fn test_init_consumes_config() {
    // Absent / empty injection → all schema defaults (ABI-2 back-compat).
    let defaults = LearningsConfig::from_init_config(&serde_json::Value::Null);
    assert_eq!(defaults, LearningsConfig::default());

    // A resolved `[plugins.learnings]` table parses into the typed struct,
    // overriding only the keys present and defaulting the rest.
    let injected = serde_json::json!({
        "learnings_dir": "/srv/kb/learnings",
        "qmd_collection": "team-notes"
    });
    let cfg = LearningsConfig::from_init_config(&injected);
    assert_eq!(cfg.learnings_dir, "/srv/kb/learnings");
    assert_eq!(cfg.qmd_collection, "team-notes");
    assert_eq!(cfg.qmd_index, "~/.cache/qmd/index.sqlite");
    assert_eq!(cfg.graph_cache, "~/.learnings/nano_graphrag_cache");

    // The plugin's `apply_init_config` (the call site `on_init` uses)
    // lands the parsed config on the plugin state.
    let mut p = LearningsPlugin::default();
    p.apply_init_config(&injected);
    assert_eq!(p.config().learnings_dir, "/srv/kb/learnings");
}
