//! Host-integration: the `hangar-tui` plugin manifest is discoverable
//! by the subprocess plugin runtime and all four P3 host caps survive
//! parsing.
//!
//! P3.6 acceptance (`docs/hangar/phases/P3.md`): "Host integration test
//! (in ainb-core plugin discovery): scan finds `hangar-tui`, parses
//! manifest, all 4 new caps validated."
//!
//! This stages the *real* `ainb-plugin-hangar/manifest.toml` (pulled in
//! at compile time via `include_str!`, so a manifest edit that breaks
//! the schema fails this test) into a tempdir laid out the way
//! `discover_plugin_root()` expects — `<root>/<name>/manifest.toml` —
//! then runs the runtime's discovery scanner over it.

use std::fs;

use ainb_plugin_protocol::manifest::CapabilityGrant;
use ainb_plugin_runtime::registry::discover;

/// The canonical manifest shipped with the plugin crate. Compiled in so
/// the test tracks the real file, not a hand-maintained copy.
const HANGAR_MANIFEST: &str = include_str!("../../ainb-plugin-hangar/manifest.toml");

#[test]
fn discovery_finds_hangar_and_validates_all_four_caps() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("dist").join("plugins");
    let plugin_dir = root.join("hangar-tui");
    fs::create_dir_all(&plugin_dir).unwrap();
    fs::write(plugin_dir.join("manifest.toml"), HANGAR_MANIFEST).unwrap();

    let found = discover(&root).expect("discovery must succeed");

    let hangar = found
        .iter()
        .find(|p| p.id.as_str() == "hangar-tui")
        .expect("discovery must surface the hangar-tui plugin");

    let caps = &hangar.manifest.capabilities;

    // All four P3 host caps parsed as list-form allow-lists.
    assert_eq!(
        caps.event_stream_subscribe.allow_list().unwrap(),
        ["workspace:*", "stream:*", "managed:*", "socket:*"],
        "event_stream_subscribe cap must validate"
    );
    assert_eq!(
        caps.spawn_managed_subprocess.allow_list().unwrap(),
        ["ainb-hangar-daemon"],
        "spawn_managed_subprocess cap must validate"
    );
    assert_eq!(
        caps.unix_socket_dial.allow_list().unwrap(),
        ["~/.agents-in-a-box/hangar.sock", "${XDG_RUNTIME_DIR}/ainb-hangar.sock"],
        "unix_socket_dial cap must validate"
    );
    assert_eq!(
        caps.secrets_read.allow_list().unwrap(),
        ["anthropic_api_key", "openai_api_key"],
        "secrets:read cap must validate"
    );

    // The two bool-form caps survive too.
    assert!(matches!(caps.event_bus, CapabilityGrant::Bool(true)));
    assert!(matches!(
        caps.write_plugin_data,
        CapabilityGrant::Bool(true)
    ));
}

#[test]
fn hangar_manifest_advertises_lazy_spawn_and_screen() {
    let m: ainb_plugin_protocol::manifest::Manifest =
        toml::from_str(HANGAR_MANIFEST).expect("manifest parses");
    assert_eq!(
        m.lifecycle.spawn,
        ainb_plugin_protocol::manifest::SpawnMode::Lazy
    );
    assert_eq!(m.provides.screens, ["hangar"]);
    assert_eq!(m.provides.cli_namespaces, ["hangar"]);
}
