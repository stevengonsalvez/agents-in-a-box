//! Shared helpers for the P2 Beads round-trip tripwire (and later phases).
//!
//! Lives at `tests/common/mod.rs` (not `tests/common.rs`) so Cargo treats it as
//! a shared module included via `mod common;` rather than compiling it as its own
//! test binary. These helpers cover the real-`bd` setup the P2.6 tripwire needs:
//! probing for the binary and initialising a fresh, isolated beads root.

#![allow(dead_code)] // not every including test uses every helper

use std::path::{Path, PathBuf};
use std::process::Command;

/// Whether the real `bd` binary is resolvable on `PATH`.
///
/// The round-trip tripwire requires the genuine tracker; a test that calls this
/// should skip-with-hint (rather than fail) when it returns `false`, so local dev
/// without `bd` installed stays green.
#[must_use]
pub fn bd_available() -> bool {
    which::which("bd").is_ok()
}

/// `bd init` a fresh `$BEADS_DIR` under `home`, returning the beads root path.
///
/// `BEADS_DIR` is set explicitly (worktree discipline — `bd` cannot infer the
/// root from cwd inside a git worktree) and the command's cwd is pointed at
/// `home` so it does not latch onto an ambient repo's beads db. Panics if `bd
/// init` fails — a tripwire cannot proceed without a tracker root.
///
/// # Panics
///
/// Panics if the `bd init` subprocess cannot be spawned or exits non-zero.
#[must_use]
pub fn bd_init(home: &Path) -> PathBuf {
    let beads_dir = home.join(".beads");
    let status = Command::new("bd")
        .arg("init")
        .env("BEADS_DIR", &beads_dir)
        .current_dir(home)
        .status()
        .expect("spawn bd init");
    assert!(status.success(), "bd init failed in {}", beads_dir.display());
    beads_dir
}
