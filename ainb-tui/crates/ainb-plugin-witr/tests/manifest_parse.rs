//! Manifest parse test.
//!
//! Asserts the on-disk `manifest.toml` decodes into the protocol's
//! `Manifest` shape and declares the exact capabilities + surfaces the
//! v1 spec requires. Acts as the schema contract gate so a future
//! capricious edit to the manifest can't silently shift surface area.

use ainb_plugin_protocol::manifest::{CapabilityGrant, Manifest, SpawnMode};

const MANIFEST_TOML: &str = include_str!("../manifest.toml");

#[test]
fn manifest_decodes_and_matches_spec() {
    let m: Manifest = toml::from_str(MANIFEST_TOML).expect("manifest.toml must decode");

    // Identity + ABI.
    assert_eq!(m.plugin.name, "witr");
    assert_eq!(m.plugin.version, "0.1.0");
    assert_eq!(m.plugin.abi_version, 2);

    // Capabilities — list form so future runtime enforcement is correct.
    assert_eq!(
        m.capabilities.spawn_subprocess.allow_list(),
        Some(&["witr".to_string()][..]),
        "spawn_subprocess must be the list form `[\"witr\"]`",
    );
    assert!(
        matches!(m.capabilities.event_bus, CapabilityGrant::Bool(true)),
        "event_bus capability required for host/snapshot/publish",
    );

    // Surfaces.
    assert_eq!(m.provides.screens, vec!["witr".to_string()]);
    assert_eq!(m.provides.commands, vec!["/witr".to_string()]);
    assert_eq!(m.provides.cli_namespaces, vec!["witr".to_string()]);
    assert_eq!(m.provides.snapshots, vec!["witr.snapshot".to_string()]);
    assert!(m.subscribes.snapshots.is_empty());

    // Lifecycle.
    assert_eq!(m.lifecycle.spawn, SpawnMode::Lazy);
    assert_eq!(m.lifecycle.idle_reap_secs, 600);
}
