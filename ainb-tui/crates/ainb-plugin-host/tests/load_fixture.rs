//! End-to-end Phase 1 acceptance tests.
//!
//! Two scenarios drive the host's contract:
//! 1. A baseline plugin that only uses always-on host fns loads, runs `_init`,
//!    and the log it emitted is observable on the host's `HostState`.
//! 2. A plugin that imports a capability-gated host fn fails to instantiate
//!    when the manifest didn't declare that capability — the "refused
//!    statically" property — but loads cleanly once the capability is granted.

use ainb_plugin_api::manifest::{CapabilitiesTable, PluginTable, ProvidesTable};
use ainb_plugin_api::Manifest;
use ainb_plugin_host::PluginHost;

const HELLO_WAT: &str = include_str!("fixtures/hello_world.wat");
const NEEDS_SESSIONS_WAT: &str = include_str!("fixtures/needs_sessions.wat");

fn baseline_manifest(name: &str) -> Manifest {
    Manifest {
        plugin: PluginTable {
            name: name.to_string(),
            version: "0.1.0".to_string(),
            ainb_min_version: "1.0.0".to_string(),
            description: None,
            author: None,
            license: None,
            homepage: None,
        },
        capabilities: CapabilitiesTable::default(),
        provides: ProvidesTable::default(),
    }
}

#[test]
fn baseline_plugin_loads_runs_init_and_logs() {
    let wasm = wat::parse_str(HELLO_WAT).expect("parse hello_world.wat");
    let mut host = PluginHost::new();

    let id = host
        .load_bytes(baseline_manifest("hello"), &wasm)
        .expect("baseline plugin loads under empty capability set");
    assert_eq!(id.0, "hello");

    let plugin = host.plugins().next().expect("one plugin loaded");
    let logs = &plugin.host_state().log_ring;
    assert_eq!(logs.len(), 1, "expected one log line from _init, got {logs:?}");
    assert_eq!(logs[0].level, 2);
    assert_eq!(logs[0].msg, "hi");
}

#[test]
fn capability_gate_refuses_unauthorised_plugin_at_instantiation() {
    let wasm = wat::parse_str(NEEDS_SESSIONS_WAT).expect("parse needs_sessions.wat");
    let mut host = PluginHost::new();

    // Manifest declares no capabilities, yet the plugin imports
    // `ainb_sessions_list`. wasmi must report the missing import.
    let result = host.load_bytes(baseline_manifest("snoop"), &wasm);
    let err = result.expect_err("must refuse plugin that imports a refused host-fn");
    let msg = format!("{err:?}");
    assert!(
        msg.contains("ainb_sessions_list") || msg.contains("instantiate") || msg.contains("import"),
        "error message should mention the unresolved import or instantiation failure; got: {msg}"
    );
}

#[test]
fn capability_gate_admits_authorised_plugin() {
    let wasm = wat::parse_str(NEEDS_SESSIONS_WAT).expect("parse needs_sessions.wat");
    let mut host = PluginHost::new();

    let mut manifest = baseline_manifest("snoop");
    manifest.capabilities.read_sessions = true;

    host.load_bytes(manifest, &wasm)
        .expect("plugin with read_sessions = true loads");
}
