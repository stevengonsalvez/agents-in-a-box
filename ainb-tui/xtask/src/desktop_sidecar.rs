//! `cargo xtask stage-desktop-sidecar [--release] [--target <triple>]`: build
//! the hangar daemon and stage it where the desktop bundle's
//! `bundle.externalBin` expects it,
//! `crates/ainb-desktop/binaries/ainb-hangar-daemon-<target triple>`.
//!
//! Tauri resolves an external binary by its target-triple suffix at build time
//! and installs it next to the app executable without the suffix, which is
//! where the sidecar supervisor looks for it.
//!
//! `--target` is for the cross-compiled leg of the release matrix (the x64
//! macOS bundle is built on an arm64 runner, as the CLI already is): the daemon
//! is built with `cargo build --target <triple>`, read from
//! `target/<triple>/<profile>/`, and staged under that triple rather than the
//! host's.

use std::env;
use std::fs;
use std::process::Command;

use anyhow::{Context, Result, anyhow, bail};

const DAEMON: &str = "ainb-hangar-daemon";

/// What the arguments asked for.
#[derive(Debug, PartialEq, Eq)]
struct Options {
    release: bool,
    /// `Some` only when `--target` was given; the host triple otherwise.
    target: Option<String>,
}

fn parse_args(args: impl Iterator<Item = String>) -> Result<Options> {
    let mut options = Options {
        release: false,
        target: None,
    };
    let mut args = args.peekable();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--release" => options.release = true,
            "--target" => {
                let triple = args
                    .next()
                    .filter(|value| !value.starts_with("--"))
                    .ok_or_else(|| anyhow!("stage-desktop-sidecar: --target needs a triple"))?;
                options.target = Some(triple);
            }
            other => bail!("stage-desktop-sidecar: unknown argument {other:?}"),
        }
    }
    Ok(options)
}

pub fn run(args: impl Iterator<Item = String>) -> Result<()> {
    let options = parse_args(args)?;
    let root = crate::workspace_root()?;
    let cargo = env::var_os("CARGO").unwrap_or_else(|| "cargo".into());

    let mut build = Command::new(&cargo);
    build.current_dir(&root).args(["build", "-p", DAEMON, "--bin", DAEMON]);
    if options.release {
        build.arg("--release");
    }
    if let Some(triple) = &options.target {
        build.args(["--target", triple]);
    }
    println!(
        "[xtask] cargo build -p {DAEMON}{}{}",
        if options.release { " --release" } else { "" },
        options
            .target
            .as_deref()
            .map(|triple| format!(" --target {triple}"))
            .unwrap_or_default()
    );
    let status = build.status().context("spawn cargo build for the daemon")?;
    if !status.success() {
        bail!("daemon build failed (exit {status})");
    }

    let cross = options.target.is_some();
    let triple = match options.target {
        Some(triple) => triple,
        None => host_triple()?,
    };
    let profile = if options.release { "release" } else { "debug" };
    let exe = if cfg!(windows) { ".exe" } else { "" };
    let mut target_dir =
        env::var_os("CARGO_TARGET_DIR").map_or_else(|| root.join("target"), Into::into);
    // `cargo build --target` writes under `target/<triple>/`; a host build does
    // not, so the two layouts differ by exactly that one directory.
    if cross {
        target_dir.push(&triple);
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Options> {
        parse_args(args.iter().map(|arg| (*arg).to_string()))
    }

    #[test]
    fn no_arguments_is_a_debug_host_build() {
        assert_eq!(
            parse(&[]).unwrap(),
            Options {
                release: false,
                target: None
            }
        );
    }

    #[test]
    fn release_and_target_are_read_in_any_order() {
        let expected = Options {
            release: true,
            target: Some("x86_64-apple-darwin".into()),
        };
        assert_eq!(
            parse(&["--release", "--target", "x86_64-apple-darwin"]).unwrap(),
            expected
        );
        assert_eq!(
            parse(&["--target", "x86_64-apple-darwin", "--release"]).unwrap(),
            expected
        );
    }

    #[test]
    fn target_without_a_triple_is_refused() {
        assert!(parse(&["--target"]).is_err());
        assert!(parse(&["--target", "--release"]).is_err());
    }

    #[test]
    fn an_unknown_argument_is_refused() {
        assert!(parse(&["--profile", "release"]).is_err());
    }
}
