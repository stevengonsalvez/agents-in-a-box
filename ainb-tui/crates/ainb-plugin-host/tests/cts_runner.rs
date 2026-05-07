//! Host Conformance Test Suite — runs each Phase 1.5 canary against
//! `PluginHost` and asserts the matching axis invariant.
//!
//! Anti-cheat: every canary's manifest name is a UUID-shaped string the
//! host can't recognise as "the special test plugin", and most canaries
//! emit a per-axis behavioural sentinel via `ainb_log` so the runner can
//! confirm the wasm actually executed (the host can't fake the marker
//! without parsing and replaying the canary).

use std::path::{Path, PathBuf};

use ainb_plugin_api::Manifest;
use ainb_plugin_host::PluginHost;

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/cts/fixtures");

fn fixture(name: &str) -> (Manifest, Vec<u8>) {
    let dir = Path::new(FIXTURES);
    let toml_path: PathBuf = dir.join(format!("{name}.toml"));
    let wasm_path: PathBuf = dir.join(format!("{name}.wasm"));
    let toml_src = std::fs::read_to_string(&toml_path).unwrap_or_else(|e| {
        panic!(
            "missing fixture {} ({e}). Run `cargo xtask build-canaries` first.",
            toml_path.display()
        )
    });
    let manifest = Manifest::from_toml(&toml_src)
        .unwrap_or_else(|e| panic!("parse {}: {e}", toml_path.display()));
    let wasm = std::fs::read(&wasm_path)
        .unwrap_or_else(|e| panic!("read {}: {e}", wasm_path.display()));
    (manifest, wasm)
}

fn logs_contain(host: &PluginHost, plugin_id: &str, needle: &[u8]) -> bool {
    let needle_str = std::str::from_utf8(needle).unwrap();
    host.get(plugin_id)
        .map(|p| {
            p.host_state()
                .log_ring
                .iter()
                .any(|line| line.msg.contains(needle_str))
        })
        .unwrap_or(false)
}

fn any_plugin_logs(host: &PluginHost, needle: &[u8]) -> bool {
    let needle_str = std::str::from_utf8(needle).unwrap();
    host.plugins().any(|p| {
        p.host_state()
            .log_ring
            .iter()
            .any(|line| line.msg.contains(needle_str))
    })
}

// ───────────────────────────────────────────────────────────────────────────
// Axis 1 — minimal screen
// ───────────────────────────────────────────────────────────────────────────
#[test]
fn axis_1_minimal_screen_loads_runs_init_and_render() {
    let (m, w) = fixture("cts-minimal-screen");
    let plugin_id = m.plugin.name.clone();
    let mut host = PluginHost::new();

    host.load_bytes(m, &w).expect("axis 1: minimal screen loads");

    // Behavioural anti-cheat: _init logged the per-canary marker.
    assert!(
        logs_contain(&host, &plugin_id, b"cts-7c8c4f01-minimal-screen-init-OK"),
        "axis 1: expected the canary's _init sentinel in log_ring"
    );

    // Render must succeed without trapping.
    host.render_plugin(&plugin_id)
        .expect("axis 1: _render must not trap");
    assert!(!host.get(&plugin_id).unwrap().is_degraded());
}

// ───────────────────────────────────────────────────────────────────────────
// Axis 6 — capability refused statically
// ───────────────────────────────────────────────────────────────────────────
#[test]
fn axis_6_capability_refused_at_instantiation() {
    let (m, w) = fixture("cts-cap-refused-network");
    let mut host = PluginHost::new();

    let err = host
        .load_bytes(m, &w)
        .expect_err("axis 6: instantiation must fail when network capability is refused");
    let chain = format!("{err:?}").to_lowercase();
    assert!(
        chain.contains("ainb_http_request") || chain.contains("import") || chain.contains("instantiate"),
        "axis 6: error chain must explain the unresolved import (got: {chain})"
    );

    // Anti-cheat: if the host secretly stubbed the capability instead of
    // refusing it, the plugin's _init would have run and emitted this
    // sentinel. It must NOT appear anywhere.
    assert!(
        !any_plugin_logs(&host, b"cts-2a91f0b2-cap-refused-network-init-RAN"),
        "axis 6: sentinel that proves _init illegally ran must not appear"
    );
}

// ───────────────────────────────────────────────────────────────────────────
// Axis 7 — _init panic is isolated; host stays usable
// ───────────────────────────────────────────────────────────────────────────
#[test]
fn axis_7_init_panic_does_not_take_down_host() {
    let panicker = fixture("cts-panics-on-init");
    let healthy = fixture("cts-minimal-screen");
    let healthy_id = healthy.0.plugin.name.clone();

    let mut host = PluginHost::new();
    let outcome = host.load_all(vec![panicker, healthy]);

    assert_eq!(
        outcome.failed.len(),
        1,
        "axis 7: panicking canary must land in failed list, got {outcome:?}",
    );
    assert!(
        outcome.failed[0].0.starts_with("cts-d31abe45-panics-on-init"),
        "axis 7: failed entry must name the panicking canary, got {:?}",
        outcome.failed[0].0
    );
    assert_eq!(outcome.loaded.len(), 1, "axis 7: healthy canary must still load");
    assert!(host.get(&healthy_id).is_some(), "axis 7: host alive + holds healthy plugin");
}

// ───────────────────────────────────────────────────────────────────────────
// Axis 8 — _render panic marks plugin degraded; host keeps drawing
// ───────────────────────────────────────────────────────────────────────────
#[test]
fn axis_8_render_panic_marks_plugin_degraded() {
    let (m, w) = fixture("cts-panics-on-render");
    let plugin_id = m.plugin.name.clone();
    let mut host = PluginHost::new();

    host.load_bytes(m, &w).expect("axis 8: _init must succeed");
    assert!(
        logs_contain(&host, &plugin_id, b"cts-50f7c819-panics-on-render-init-OK"),
        "axis 8: _init sentinel must be present"
    );

    let first = host.render_plugin(&plugin_id);
    assert!(first.is_err(), "axis 8: first _render must surface an error");
    assert!(
        host.get(&plugin_id).unwrap().is_degraded(),
        "axis 8: plugin must be marked degraded after trap"
    );

    // Second render against a degraded plugin must be a no-op, not another trap.
    host.render_plugin(&plugin_id)
        .expect("axis 8: subsequent _render on degraded plugin must be Ok");
}

// ───────────────────────────────────────────────────────────────────────────
// Axis 11 — wrong ainb_min_version is refused with an actionable error
// ───────────────────────────────────────────────────────────────────────────
#[test]
fn axis_11_wrong_min_version_refused() {
    let (m, w) = fixture("cts-wrong-min-version");
    let mut host = PluginHost::new();
    let err = host.load_bytes(m, &w).expect_err("axis 11: load must fail");
    let chain = format!("{err:?}");
    assert!(
        chain.contains("999.0.0"),
        "axis 11: error must name the requested version, got: {chain}"
    );
    assert!(
        chain.to_lowercase().contains("host"),
        "axis 11: error must mention the host version mismatch, got: {chain}"
    );

    // Host must still load other plugins after this rejection.
    let (h, hw) = fixture("cts-minimal-screen");
    host.load_bytes(h, &hw)
        .expect("axis 11: host stays usable for further loads");
}

// ───────────────────────────────────────────────────────────────────────────
// Axis 12 — two-plugin coexistence + cross-plugin event delivery
// ───────────────────────────────────────────────────────────────────────────
#[test]
fn axis_12_pair_publish_and_subscriber_receives() {
    let pair_a = fixture("cts-pair-a");
    let pair_b = fixture("cts-pair-b");
    let pair_a_id = pair_a.0.plugin.name.clone();
    let pair_b_id = pair_b.0.plugin.name.clone();

    let mut host = PluginHost::new();
    host.load_bytes(pair_b.0, &pair_b.1).expect("pair-b loads");
    host.load_bytes(pair_a.0, &pair_a.1).expect("pair-a loads");

    // pair-b subscribed in _init; pair-a publishes inside _tick.
    assert!(
        logs_contain(&host, &pair_b_id, b"cts-eb102a73-pair-b-subscribed-cts.ping"),
        "axis 12: pair-b's subscription sentinel must be present"
    );
    assert!(
        logs_contain(&host, &pair_a_id, b"cts-9d4f88a0-pair-a-init-OK"),
        "axis 12: pair-a's _init sentinel must be present"
    );

    host.tick_plugin(&pair_a_id).expect("axis 12: pair-a tick must succeed");
    let queued = host.shared().event_queue.lock().unwrap().len();
    assert_eq!(queued, 1, "axis 12: tick must produce one queued event");

    host.pump_events().expect("axis 12: pump must dispatch cleanly");

    assert!(
        logs_contain(&host, &pair_b_id, b"cts-eb102a73-pair-b-received-event"),
        "axis 12: pair-b's _handle_event sentinel must be present after pump_events"
    );
}
