//! `cargo xtask` — workspace automation.
//!
//! Subcommands:
//! * `build-canaries` — compile every CTS canary plugin to wasm32-wasip1
//!   release and copy the resulting `.wasm` + `plugin.toml` into
//!   `crates/ainb-plugin-host/tests/cts/fixtures/<canary>.{wasm,toml}` so
//!   the host's `cts_runner` integration test can `include_bytes!` /
//!   `include_str!` them without depending on cargo-build state.
//! * `clean-canaries` — remove fixtures + the sub-workspace's `target/`.
//! * `ci-lint` — assert `.github/workflows/ci.yml` satisfies the real Hangar
//!   e2e CI contract (the `hangar-e2e` job). See [`ci_lint`].
//!
//! No external CLI parser dependency on purpose — this binary is meant to
//! be cheap to compile and stay out of the way.

mod ci_lint;

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, anyhow, bail};

/// Each canary's directory name matches its Cargo crate name. The wasm
/// output basename is the same name with hyphens turned into underscores
/// (cargo's cdylib convention).
const CANARIES: &[&str] = &[
    "cts-minimal-screen",
    "cts-cap-refused-network",
    "cts-panics-on-init",
    "cts-panics-on-render",
    "cts-wrong-min-version",
    "cts-pair-a",
    "cts-pair-b",
    "cts-sidebar-only",
    "cts-statusline-only",
    "cts-command-only",
    "cts-provider-only",
    "cts-infinite-tick",
    "cts-malformed-buffer",
    "cts-fs-read-with-cap",
    "cts-fs-read-without-cap",
];

fn main() -> Result<()> {
    let mut args = env::args().skip(1);
    let cmd = args
        .next()
        .ok_or_else(|| anyhow!("usage: cargo xtask <build-canaries|clean-canaries|ci-lint>"))?;
    match cmd.as_str() {
        "build-canaries" => build_canaries(),
        "clean-canaries" => clean_canaries(),
        "ci-lint" => ci_lint::run(),
        other => bail!("unknown xtask subcommand {other:?}"),
    }
}

fn workspace_root() -> Result<PathBuf> {
    // CARGO_MANIFEST_DIR for the xtask crate is `<workspace>/xtask`. Pop
    // one level to get the workspace root.
    let me = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    me.parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| anyhow!("xtask manifest dir has no parent"))
}

fn canaries_dir(root: &Path) -> PathBuf {
    root.join("crates/ainb-plugin-host/tests/cts/canaries")
}

fn fixtures_dir(root: &Path) -> PathBuf {
    root.join("crates/ainb-plugin-host/tests/cts/fixtures")
}

fn build_canaries() -> Result<()> {
    let root = workspace_root()?;
    let canaries = canaries_dir(&root);
    let fixtures = fixtures_dir(&root);
    let manifest = canaries.join("Cargo.toml");

    println!("[xtask] cargo build --release --target wasm32-wasip1 (canary sub-workspace)");
    let status = Command::new(env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
        .arg("build")
        .arg("--release")
        .arg("--target")
        .arg("wasm32-wasip1")
        .arg("--manifest-path")
        .arg(&manifest)
        .status()
        .context("spawn cargo build for canaries")?;
    if !status.success() {
        bail!("canary build failed (exit {status})");
    }

    fs::create_dir_all(&fixtures).with_context(|| format!("create {}", fixtures.display()))?;

    let wasm_release_dir = canaries.join("target/wasm32-wasip1/release");
    for canary in CANARIES {
        let wasm_basename = canary.replace('-', "_");
        let src_wasm = wasm_release_dir.join(format!("{wasm_basename}.wasm"));
        let dst_wasm = fixtures.join(format!("{canary}.wasm"));
        let src_toml = canaries.join(canary).join("plugin.toml");
        let dst_toml = fixtures.join(format!("{canary}.toml"));

        if !src_wasm.is_file() {
            bail!("expected wasm artifact missing: {}", src_wasm.display());
        }
        if !src_toml.is_file() {
            bail!("expected plugin.toml missing: {}", src_toml.display());
        }
        fs::copy(&src_wasm, &dst_wasm)
            .with_context(|| format!("copy {} -> {}", src_wasm.display(), dst_wasm.display()))?;
        fs::copy(&src_toml, &dst_toml)
            .with_context(|| format!("copy {} -> {}", src_toml.display(), dst_toml.display()))?;
        println!("[xtask] {canary} → fixtures/{canary}.{{wasm,toml}}");
    }
    println!(
        "[xtask] {} canaries staged in {}",
        CANARIES.len(),
        fixtures.display()
    );
    Ok(())
}

fn clean_canaries() -> Result<()> {
    let root = workspace_root()?;
    let fixtures = fixtures_dir(&root);
    let canary_target = canaries_dir(&root).join("target");
    for p in [&fixtures, &canary_target] {
        if p.exists() {
            fs::remove_dir_all(p).with_context(|| format!("rm -rf {}", p.display()))?;
            println!("[xtask] removed {}", p.display());
        }
    }
    Ok(())
}
