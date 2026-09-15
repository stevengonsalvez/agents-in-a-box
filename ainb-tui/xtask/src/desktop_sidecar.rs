//! `cargo xtask stage-desktop-sidecar [--release]`: build the hangar daemon and
//! stage it where the desktop bundle's `bundle.externalBin` expects it,
//! `crates/ainb-desktop/binaries/ainb-hangar-daemon-<target triple>`.
//!
//! Tauri resolves an external binary by its target-triple suffix at build time
//! and installs it next to the app executable without the suffix, which is
//! where the sidecar supervisor looks for it.

use std::env;
use std::fs;
use std::process::Command;

use anyhow::{Context, Result, anyhow, bail};

const DAEMON: &str = "ainb-hangar-daemon";

pub fn run(args: impl Iterator<Item = String>) -> Result<()> {
    let mut release = false;
    for arg in args {
        match arg.as_str() {
            "--release" => release = true,
            other => bail!("stage-desktop-sidecar: unknown argument {other:?}"),
        }
    }
    let root = crate::workspace_root()?;
    let cargo = env::var_os("CARGO").unwrap_or_else(|| "cargo".into());

    let mut build = Command::new(&cargo);
    build.current_dir(&root).args(["build", "-p", DAEMON, "--bin", DAEMON]);
    if release {
        build.arg("--release");
    }
    println!(
        "[xtask] cargo build -p {DAEMON}{}",
        if release { " --release" } else { "" }
    );
    let status = build.status().context("spawn cargo build for the daemon")?;
    if !status.success() {
        bail!("daemon build failed (exit {status})");
    }

    let triple = host_triple()?;
    let profile = if release { "release" } else { "debug" };
    let exe = if cfg!(windows) { ".exe" } else { "" };
    let target_dir =
        env::var_os("CARGO_TARGET_DIR").map_or_else(|| root.join("target"), Into::into);
    let built = target_dir.join(profile).join(format!("{DAEMON}{exe}"));
    if !built.is_file() {
        bail!(
            "the build reported success but {} is missing",
            built.display()
        );
    }
    let staged_dir = root.join("crates/ainb-desktop/binaries");
    fs::create_dir_all(&staged_dir).with_context(|| format!("create {}", staged_dir.display()))?;
    let staged = staged_dir.join(format!("{DAEMON}-{triple}{exe}"));
    fs::copy(&built, &staged)
        .with_context(|| format!("copy {} to {}", built.display(), staged.display()))?;
    println!("[xtask] staged {}", staged.display());
    Ok(())
}

/// The host target triple, as `rustc -vV` reports it.
fn host_triple() -> Result<String> {
    let rustc = env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let output = Command::new(rustc).arg("-vV").output().context("run rustc -vV")?;
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .map(str::to_string)
        .ok_or_else(|| anyhow!("rustc -vV printed no host triple"))
}
