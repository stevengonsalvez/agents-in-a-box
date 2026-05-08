//! Convenience wrapper that builds a wasm32-wasip1 plugin from
//! source and runs the contract against it. Useful for plugins that
//! ship a `tests/conformance.rs` and don't want to script a separate
//! build step.

use std::path::PathBuf;
use std::process::Command;

use anyhow::{anyhow, Context};

use crate::{run_contract_v1, ConformanceReport};

/// Locate the plugin's package by name, build a release wasm via
/// `cargo build --release --target wasm32-wasip1 -p <pkg>`, then
/// drive [`run_contract_v1`] against the resulting wasm + the
/// supplied manifest.
///
/// `pkg` is the cargo package name; `manifest_toml` is the contents
/// of the plugin's `plugin.toml` (typically loaded with
/// `include_str!`).
///
/// # Errors
/// Returns `Err` when the build subprocess fails or its expected wasm
/// output is missing — caller bugs we can't paper over with a softer
/// failure.
pub fn run_contract_v1_self_build(
    pkg: &str,
    manifest_toml: &str,
) -> anyhow::Result<ConformanceReport> {
    let target_dir = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            // CARGO_MANIFEST_DIR is the *consumer* crate's manifest dir
            // when this crate is used as a dep, so we walk up from
            // `target/<...>` if it's set, otherwise use cwd.
            let mut p = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            p.push("target");
            p
        });

    let status = Command::new(env!("CARGO"))
        .args([
            "build",
            "--release",
            "--target",
            "wasm32-wasip1",
            "-p",
            pkg,
        ])
        .status()
        .context("invoke cargo build")?;
    if !status.success() {
        return Err(anyhow!(
            "cargo build --release --target wasm32-wasip1 -p {pkg} failed (exit {})",
            status.code().unwrap_or(-1)
        ));
    }

    let wasm_name = pkg.replace('-', "_") + ".wasm";
    let wasm_path = target_dir
        .join("wasm32-wasip1")
        .join("release")
        .join(&wasm_name);
    let wasm = std::fs::read(&wasm_path)
        .with_context(|| format!("read built wasm {}", wasm_path.display()))?;

    Ok(run_contract_v1(manifest_toml, &wasm))
}
