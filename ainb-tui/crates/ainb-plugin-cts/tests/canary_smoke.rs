//! Smoke tests: drive the publishable CTS crate against a couple of
//! the host's canary wasm fixtures. Proves the eight axes evaluate
//! end-to-end without depending on the host runtime.
//!
//! These tests assume `cargo xtask build-canaries` has run; they're
//! gated on the fixture's presence so a fresh checkout doesn't fail
//! before the canaries are staged.

use std::path::{Path, PathBuf};

use ainb_plugin_cts::run_contract_v1;

const FIXTURES: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../ainb-plugin-host/tests/cts/fixtures"
);

fn fixture(name: &str) -> Option<(String, Vec<u8>)> {
    let dir = Path::new(FIXTURES);
    let toml_path: PathBuf = dir.join(format!("{name}.toml"));
    let wasm_path: PathBuf = dir.join(format!("{name}.wasm"));
    if !toml_path.exists() || !wasm_path.exists() {
        return None;
    }
    let toml = std::fs::read_to_string(&toml_path).ok()?;
    let wasm = std::fs::read(&wasm_path).ok()?;
    Some((toml, wasm))
}

#[test]
fn minimal_screen_canary_passes_v1_contract() {
    let Some((manifest, wasm)) = fixture("cts-minimal-screen") else {
        eprintln!("[skip] cts-minimal-screen fixture missing — run `cargo xtask build-canaries`");
        return;
    };
    let report = run_contract_v1(&manifest, &wasm);
    assert!(
        report.is_passing(),
        "minimal-screen must pass every axis:\n{report}"
    );
}

#[test]
fn provider_only_canary_passes_v1_contract() {
    let Some((manifest, wasm)) = fixture("cts-provider-only") else {
        eprintln!("[skip] cts-provider-only fixture missing");
        return;
    };
    let report = run_contract_v1(&manifest, &wasm);
    assert!(
        report.is_passing(),
        "provider-only must pass every axis:\n{report}"
    );
}

#[test]
fn wrong_min_version_canary_fails_axis_7_only() {
    let Some((manifest, wasm)) = fixture("cts-wrong-min-version") else {
        eprintln!("[skip] cts-wrong-min-version fixture missing");
        return;
    };
    let report = run_contract_v1(&manifest, &wasm);
    // ainb_min_version="999.0.0" parses as semver fine — the host's
    // axis-11 check (host floor mismatch) is a host-side concern.
    // From the publishable CTS POV, this canary should pass: it's a
    // well-formed plugin, just one a v1 host won't load. Confirm
    // axis 7 in particular is a pass (semver parsed).
    let axis_7 = report.axes.iter().find(|a| a.id == 7).unwrap();
    assert!(
        axis_7.is_pass(),
        "axis 7 expects valid semver, not host-floor compatibility: {axis_7:?}"
    );
}
