//! `ainb usage` CLI dispatch through the burndown plugin.
//!
//! Verifies the round-trip:
//!   host pushes UsageData -> dispatch_cli("usage", argv)
//!   -> plugin parses argv via clap, calls execute_for_plugin
//!   -> plugin's println! -> host fd_write capture -> stdout buffer
//!
//! Compared byte-for-byte against the in-tree
//! `cli::usage::report_json` for the same fixture (the same identity
//! the snapshot baseline asserts).

#![cfg(feature = "test-support")]
#![allow(missing_docs)]

use std::path::PathBuf;

use ainb::test_support::{cli_usage_report_json, sample_usage_data};
use ainb_plugin_api::PluginEvent;

fn workspace_dist() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .and_then(|p| p.parent())
        .map(|p| p.join("dist").join("plugins"))
        .expect("workspace root resolvable")
}

#[test]
fn plugin_cli_report_json_matches_in_tree() {
    let dist = workspace_dist();
    if !dist.join("burndown").join("plugin.wasm").exists() {
        eprintln!(
            "skipping: dist/plugins/burndown/plugin.wasm missing — \
             run scripts/build-plugins.sh"
        );
        return;
    }
    std::env::set_var("AINB_PLUGIN_ROOT", &dist);

    let (mut host, outcome) = ainb::plugins::init_plugin_host();
    assert!(outcome.failed.is_empty(), "plugin must load: {:?}", outcome.failed);

    // Push the same fixture the in-tree path uses.
    let data = sample_usage_data();
    let payload = serde_json::to_value(&data).expect("UsageData -> json");
    let ev = PluginEvent::Custom {
        topic: "burndown.usage_data".into(),
        payload,
    };
    let bytes = rmp_serde::to_vec_named(&ev).unwrap();
    host.dispatch_event_bytes("burndown", &bytes).unwrap();

    // Run `usage report --format=json` through the plugin's CLI handler.
    // argv mirrors what the host's dispatch sends — the subcommand only,
    // not the leading "usage" token (the plugin's clap synthesises a
    // prog-name itself).
    let argv = vec![
        "report".to_string(),
        "--format=json".to_string(),
    ];
    let (stdout, stderr) = host.dispatch_cli("burndown", "usage", &argv).unwrap();

    assert!(stderr.is_empty(),
        "stderr should be empty for happy path, got: {}",
        String::from_utf8_lossy(&stderr));
    let plugin_out = String::from_utf8(stdout).expect("plugin stdout is utf8");

    // In-tree comparator: build the same JSON via the host's report_json
    // helper and pretty-print it the same way the plugin does.
    let expected_value = cli_usage_report_json(&data);
    let expected = serde_json::to_string_pretty(&expected_value).unwrap() + "\n";

    assert_eq!(plugin_out, expected,
        "plugin CLI report JSON must match in-tree report_json byte-for-byte");

    std::env::remove_var("AINB_PLUGIN_ROOT");
}
