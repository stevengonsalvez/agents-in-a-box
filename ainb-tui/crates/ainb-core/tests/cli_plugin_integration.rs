//! End-to-end integration tests for the Phase 7d-cli subcommands.
//!
//! Drives the real `ainb` binary (located via the
//! `CARGO_BIN_EXE_ainb` env var the test harness sets) against a
//! tempdir-backed plugin fixture. Three behaviours under test:
//!
//! * `ainb plugin lint <path>` — a directory with a v2 manifest + an
//!   executable binary lints clean.
//! * `ainb plugin lint <bad path>` — a missing binary fails the lint
//!   with a non-zero exit and an actionable error in stdout.
//! * `ainb plugin watch <unknown>` / `ainb plugin tail <unknown>` —
//!   exit non-zero with a 'not registered' message when no plugin of
//!   that id is present (the runtime stays empty under
//!   `AINB_PLUGIN_ROOT` pointing at a missing path).
//!
//! Why no in-test plugin process: spinning a real fixture binary
//! requires `cargo build`-ing the runtime crate's internal fixture,
//! whose `CARGO_BIN_EXE_*` env var is only exposed inside that crate's
//! tests. The runtime e2e tests already cover the live-process path.
//! Here we only need to verify the CLI's wiring end-to-end.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn ainb_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ainb"))
}

fn write_plugin_fixture(root: &std::path::Path, name: &str, with_binary: bool) -> PathBuf {
    let dir = root.join(name);
    fs::create_dir_all(&dir).unwrap();
    let manifest = format!(
        r#"
[plugin]
name = "{name}"
version = "0.1.0"
abi_version = 2
description = "integration test fixture"
"#
    );
    fs::write(dir.join("manifest.toml"), manifest).unwrap();
    if with_binary {
        // Symlink to /bin/sh so the binary exists, is executable, and
        // responds to --version with a sane line. Skipped on non-unix —
        // those environments already aren't covered by Phase 7's
        // process spawning either (PR_SET_PDEATHSIG / setpgid are
        // unix-only in the runtime).
        #[cfg(unix)]
        std::os::unix::fs::symlink("/bin/sh", dir.join(name)).unwrap();
        #[cfg(not(unix))]
        let _ = with_binary;
    }
    dir
}

#[test]
#[cfg(unix)]
fn plugin_lint_clean_fixture_exits_zero() {
    let tmp = tempfile::tempdir().unwrap();
    let plugin_dir = write_plugin_fixture(tmp.path(), "fixture", true);

    let out = Command::new(ainb_bin())
        .args(["plugin", "lint"])
        .arg(&plugin_dir)
        .env("AINB_DISABLE_PLUGINS", "1") // lint with path doesn't need runtime
        .env("HOME", tmp.path()) // sandbox any HOME-scoped lookups
        .output()
        .expect("spawn ainb");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "expected exit 0; stdout={stdout}\nstderr={stderr}"
    );
    assert!(
        stdout.contains("lint: fixture"),
        "expected 'lint: fixture' header; stdout={stdout}"
    );
    assert!(
        !stdout.contains("err  "),
        "no error rows expected on a clean fixture; stdout={stdout}"
    );
}

#[test]
#[cfg(unix)]
fn plugin_lint_missing_binary_exits_nonzero() {
    let tmp = tempfile::tempdir().unwrap();
    let plugin_dir = write_plugin_fixture(tmp.path(), "fixture", /* with_binary */ false);

    let out = Command::new(ainb_bin())
        .args(["plugin", "lint"])
        .arg(&plugin_dir)
        .env("AINB_DISABLE_PLUGINS", "1")
        .env("HOME", tmp.path())
        .output()
        .expect("spawn ainb");

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(!out.status.success(), "expected non-zero exit on missing binary");
    assert!(
        stdout.contains("err") && stdout.contains("binary_exists"),
        "expected binary_exists error in report; stdout={stdout}"
    );
}

#[test]
fn plugin_watch_unknown_plugin_exits_nonzero() {
    let tmp = tempfile::tempdir().unwrap();
    let out = Command::new(ainb_bin())
        .args(["plugin", "watch", "definitely-not-installed", "--duration", "1"])
        .env("AINB_PLUGIN_ROOT", "/definitely/not/a/real/path")
        .env("HOME", tmp.path())
        .output()
        .expect("spawn ainb");

    assert!(!out.status.success(), "expected non-zero exit");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        combined.contains("not registered") || combined.contains("ainb plugin list"),
        "expected actionable 'not registered' message; got: {combined}"
    );
}

#[test]
fn plugin_tail_unknown_plugin_exits_nonzero() {
    let tmp = tempfile::tempdir().unwrap();
    let out = Command::new(ainb_bin())
        .args([
            "plugin",
            "tail",
            "definitely-not-installed",
            "--duration",
            "1",
        ])
        .env("AINB_PLUGIN_ROOT", "/definitely/not/a/real/path")
        .env("HOME", tmp.path())
        .output()
        .expect("spawn ainb");

    assert!(!out.status.success(), "expected non-zero exit");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        combined.contains("not registered") || combined.contains("ainb plugin list"),
        "expected actionable 'not registered' message; got: {combined}"
    );
}

#[test]
fn plugin_lint_invalid_level_exits_nonzero() {
    // Sanity: completely bogus arg shape should be rejected by clap.
    let out = Command::new(ainb_bin())
        .args(["plugin", "tail", "x", "--level", "shouty-as-fuck"])
        .output()
        .expect("spawn ainb");
    // Either clap rejects it (usage error) or our parse_level does.
    // Either way the exit is non-zero — the contract is 'don't pretend
    // it was understood'.
    assert!(!out.status.success(), "expected non-zero exit");
}
