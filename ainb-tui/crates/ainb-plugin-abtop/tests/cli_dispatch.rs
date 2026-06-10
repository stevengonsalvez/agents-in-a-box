//! Integration tests for the `ainb abtop` CLI surface
//! ([`AbtopPlugin::cli_dispatch`]).
//!
//! Drives the full dispatch path against a stub `abtop` binary staged
//! in a tempdir + injected as the plugin's resolved path. Covers:
//!   - `ainb abtop` returns the captured text snapshot
//!   - `--format json` is handled/stripped (abtop has no JSON mode, so
//!     it's a no-op passthrough — still exit 0 with text)
//!   - missing-abtop → exit 1 + install hint on stdout
//!   - unknown namespace → exit 2
//!
//! Gated on cfg(unix): stubs are `#!/bin/sh` scripts marked executable.
//! Support matrix is macOS + Linux.

#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};

use ainb_plugin_abtop::plugin::AbtopPlugin;
use ainb_plugin_abtop::state::Lifecycle;

/// Stage a stub `abtop` that prints a fake snapshot header on
/// `--once` and exits 0. The stub is deliberately a fast, non-TTY
/// printer — unlike the real abtop TUI it returns immediately, so the
/// dispatcher's exec path is exercised deterministically.
fn stage_once_stub(dir: &Path) -> PathBuf {
    let bin = dir.join("abtop");
    let script = "#!/bin/sh\nif [ \"$1\" = \"--once\" ]; then\n  printf 'abtop \\342\\200\\224 3 sessions, 0 mcp servers\\n'\n  exit 0\nfi\nprintf 'stub-abtop: unexpected args: %s\\n' \"$*\" >&2\nexit 1\n".to_string();
    fs::write(&bin, script).expect("write stub");
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(&bin).expect("stat").permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&bin, perms).expect("chmod");
    bin
}

/// Build a Ready plugin whose abtop_path points at a freshly-staged
/// stub. Returns (plugin, tempdir) — keep the tempdir alive for the
/// test's duration.
fn ready_plugin() -> (AbtopPlugin, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let stub = stage_once_stub(dir.path());
    let mut p = AbtopPlugin::default();
    p.set_ready_for_test(stub);
    (p, dir)
}

fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| (*s).to_string()).collect()
}

#[tokio::test]
async fn cli_abtop_returns_text_output() {
    let (mut p, _dir) = ready_plugin();

    let out = p.cli_dispatch_core("abtop", &argv(&[])).await;
    assert_eq!(out.exit_code, 0);
    let s = String::from_utf8_lossy(&out.stdout);
    // The stub prints the em-dash header just like the real abtop.
    assert!(s.contains("abtop —"), "text output: {s:?}");
    assert!(s.contains("sessions"));
}

#[tokio::test]
async fn cli_abtop_format_json_is_stripped_and_passes_through() {
    let (mut p, _dir) = ready_plugin();

    // abtop has no JSON mode; `--format json` is stripped before clap
    // sees it and the dispatcher still returns the text passthrough.
    let out = p.cli_dispatch_core("abtop", &argv(&["--format", "json"])).await;
    assert_eq!(
        out.exit_code,
        0,
        "stderr: {:?}",
        String::from_utf8_lossy(&out.stderr)
    );
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains("abtop —"), "text output: {s:?}");
}

#[tokio::test]
async fn cli_abtop_missing_binary_exits_one() {
    // Default plugin: lifecycle Unknown, abtop_path None.
    let mut p = AbtopPlugin::default();
    assert_eq!(p.lifecycle_for_test(), &Lifecycle::Unknown);

    let out = p.cli_dispatch_core("abtop", &argv(&[])).await;
    assert_eq!(out.exit_code, 1);
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(
        s.contains("not found") && s.contains("Install"),
        "missing hint should mention install: {s:?}",
    );
    assert!(
        s.contains("cargo install abtop"),
        "missing hint should include the universal fallback: {s:?}",
    );
}

#[tokio::test]
async fn cli_abtop_unknown_namespace_exits_two() {
    let (mut p, _dir) = ready_plugin();

    let out = p.cli_dispatch_core("bogus", &argv(&[])).await;
    assert_eq!(out.exit_code, 2);
    assert!(!out.stderr.is_empty(), "usage error on stderr");
}

#[tokio::test]
async fn cli_abtop_help_exits_zero_without_binary() {
    // `--help` short-circuits before the binary is needed.
    let mut p = AbtopPlugin::default();
    let out = p.cli_dispatch_core("abtop", &argv(&["--help"])).await;
    assert_eq!(out.exit_code, 0);
    assert!(!out.stdout.is_empty(), "help text on stdout");
}
