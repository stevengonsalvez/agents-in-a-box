//! Manifest parse test.
//!
//! Asserts the on-disk `manifest.toml` decodes into the protocol's
//! `Manifest` shape and declares the exact capabilities + surfaces the
//! Phase 1 spec requires. Acts as the schema contract gate so a future
//! capricious edit to the manifest can't silently shift surface area.

use ainb_plugin_protocol::manifest::{CapabilityGrant, Manifest, SpawnMode};

const MANIFEST_TOML: &str = include_str!("../manifest.toml");

#[test]
fn manifest_decodes_and_matches_spec() {
    let m: Manifest = toml::from_str(MANIFEST_TOML).expect("manifest.toml must decode");

    // Identity + ABI.
    assert_eq!(m.plugin.name, "abtop");
    assert_eq!(m.plugin.version, "0.1.0");
    assert_eq!(m.plugin.abi_version, 2);

    // Capabilities — list form so future runtime enforcement is correct.
    assert_eq!(
        m.capabilities.spawn_subprocess.allow_list(),
        Some(&["abtop".to_string()][..]),
        "spawn_subprocess must be the list form `[\"abtop\"]`",
    );
    // No event bus — abtop publishes no snapshots and subscribes to none.
    assert!(
        matches!(m.capabilities.event_bus, CapabilityGrant::Bool(false)),
        "event_bus must be false — abtop has no data plane",
    );
    assert!(matches!(
        m.capabilities.read_sessions,
        CapabilityGrant::Bool(false)
    ));
    assert!(matches!(
        m.capabilities.write_plugin_data,
        CapabilityGrant::Bool(false)
    ));
    assert!(matches!(
        m.capabilities.network,
        CapabilityGrant::Bool(false)
    ));

    // Surfaces.
    assert_eq!(m.provides.screens, vec!["abtop".to_string()]);
    assert_eq!(m.provides.commands, vec!["/abtop".to_string()]);
    assert_eq!(m.provides.cli_namespaces, vec!["abtop".to_string()]);
    assert!(
        m.provides.snapshots.is_empty(),
        "abtop publishes no snapshots"
    );
    assert!(m.subscribes.snapshots.is_empty());

    // Lifecycle.
    assert_eq!(m.lifecycle.spawn, SpawnMode::Lazy);
    assert_eq!(m.lifecycle.idle_reap_secs, 600);
}
