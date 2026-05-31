//! P9.3 meta-tripwire — guards the Hangar tripwire suite against silent
//! deletion / rename.
//!
//! Every other `tripwire_*` test proves one user-visible behaviour. This one
//! proves the *suite itself* has not shrunk: it discovers every Hangar
//! `tripwire_*.rs` integration-test binary across the three workspace
//! locations and asserts the discovered count is at least a baseline captured
//! on first run.
//!
//! ```text
//!  crates/ainb-hangar-daemon/tests/tripwire_*.rs   (P4/P7/P8/P9 daemon set)
//!  crates/ainb-hangar-store/tests/tripwire_*.rs    (P0 migration determinism)
//!  plugins/hangar-tui/tests/tripwire_*.rs          (P3.8/P4.10 plugin roundtrip)
//!         │
//!         ▼  walk dirs, count tripwire_*.rs (skip *_common.rs helpers)
//!  assert discovered >= BASELINE
//! ```
//!
//! ## Why walk files, not parse `cargo test --list`
//!
//! Each `tripwire_*.rs` under a `tests/` dir is its own cargo test *binary*.
//! Walking the directories is hermetic (no recompile, no cargo subprocess, no
//! feature-flag juggling for the `#![cfg(feature = "otlp")]` member) and maps
//! 1:1 onto the thing we want to defend: the set of tripwire binaries
//! `scripts/hangar/run_all_tripwires.sh` runs. Adding a tripwire only ever
//! raises the count; deleting or renaming one drops it below baseline and reds
//! this test, forcing an intentional baseline bump.
//!
//! ## Baseline
//!
//! `BASELINE_TRIPWIRES` was captured by running the discovery once and reading
//! the actual count (25 on the P9.3 commit), per `feedback_dont_guess_test_constants`.
//! When you ADD a tripwire, this test keeps passing (count rises). When you
//! INTENTIONALLY remove one, bump the baseline down in the same commit so the
//! drop is reviewed, never silent.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

/// Hangar tripwire binaries present at the P9.3 commit. See module docs for the
/// capture procedure. Lower-bound assertion: adding tripwires is always fine.
const BASELINE_TRIPWIRES: usize = 25;

/// `tests/` directories that hold Hangar tripwires, relative to the cargo
/// workspace root (`ainb-tui/`).
const TRIPWIRE_DIRS: &[&str] = &[
    "crates/ainb-hangar-daemon/tests",
    "crates/ainb-hangar-store/tests",
    // The Hangar plugin lives OUTSIDE the cargo workspace, at repo-root
    // `plugins/hangar-tui/` (sibling of `ainb-tui/`), so it is reached via the
    // workspace root's parent.
    "../plugins/hangar-tui/tests",
];

/// Workspace root = the directory containing this daemon crate's parent
/// `crates/` dir. `CARGO_MANIFEST_DIR` is `<ws>/crates/ainb-hangar-daemon`.
fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("daemon manifest dir has a <ws>/crates/<crate> shape")
        .to_path_buf()
}

/// Collect the basenames of every `tripwire_*.rs` test binary across the
/// Hangar tripwire dirs. `*_common.rs` files are shared helpers (no `#[test]`,
/// not their own binary) and are excluded — exactly as the runner script does.
fn discover_tripwires(ws: &Path) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    for rel in TRIPWIRE_DIRS {
        let dir = ws.join(rel);
        let entries = match fs::read_dir(&dir) {
            Ok(e) => e,
            // A missing tripwire dir is itself a regression — surface it.
            Err(err) => panic!("cannot read tripwire dir {}: {err}", dir.display()),
        };
        for entry in entries {
            let path = entry.expect("dir entry").path();
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if !name.starts_with("tripwire_") || !name.ends_with(".rs") {
                continue;
            }
            if name.ends_with("_common.rs") {
                continue;
            }
            // Key by `dir::stem` so identically-named files in different crates
            // never collide in the set.
            found.insert(format!("{rel}::{name}"));
        }
    }
    found
}

#[test]
fn hangar_tripwire_suite_has_not_shrunk() {
    let ws = workspace_root();
    let tripwires = discover_tripwires(&ws);
    let count = tripwires.len();

    assert!(
        count >= BASELINE_TRIPWIRES,
        "Hangar tripwire suite shrank: discovered {count} tripwire binaries, \
         baseline is {BASELINE_TRIPWIRES}. A tripwire was deleted or renamed. \
         If intentional, lower BASELINE_TRIPWIRES in the same commit so the \
         drop is reviewed.\nDiscovered:\n  {}",
        tripwires.into_iter().collect::<Vec<_>>().join("\n  ")
    );
}
