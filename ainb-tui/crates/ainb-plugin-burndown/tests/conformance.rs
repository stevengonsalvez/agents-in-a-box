//! Dogfood the publishable CTS harness against burndown's own wasm.
//!
//! Skips with a printed note when the wasm hasn't been built yet —
//! `scripts/build-plugins.sh` (or any equivalent
//! `cargo build --release --target wasm32-wasip1 -p ainb-plugin-burndown`)
//! is the prerequisite. The skip-on-missing-wasm pattern lets fresh
//! checkouts run `cargo test` without first wiring WASI_SDK_PATH.

use std::path::PathBuf;

use ainb_plugin_cts::run_contract_v1;

fn wasm_path() -> PathBuf {
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
            p.push("target");
            p
        });
    target
        .join("wasm32-wasip1")
        .join("release")
        .join("ainb_plugin_burndown.wasm")
}

#[test]
fn burndown_passes_v1_contract() {
    let manifest = include_str!("../plugin.toml");
    let path = wasm_path();
    if !path.exists() {
        eprintln!(
            "[skip] {} not present — run scripts/build-plugins.sh first",
            path.display()
        );
        return;
    }
    let wasm = std::fs::read(&path).expect("read built burndown wasm");
    let report = run_contract_v1(manifest, &wasm);
    assert!(
        report.is_passing(),
        "burndown must pass every v1 axis:\n{report}"
    );
}
