//! Semver gate for `ainb-plugin-protocol`'s public wire surface.
//!
//! Ten crates depend on the protocol crate and nothing detected a wire-type
//! change landing without a version bump. This test renders the crate's
//! public surface from source (see `ainb_plugin_cts_v2::wire_surface`) and
//! compares it byte-for-byte with the committed
//! `crates/ainb-plugin-protocol/wire-surface.lock`.
//!
//! Break it deliberately to see it bite: rename a field on any `pub struct`
//! in `ainb-plugin-protocol/src/params.rs` and run this test.
//!
//! Regenerate after an INTENTIONAL wire change:
//!
//! ```text
//! # 1. bump `version` in crates/ainb-plugin-protocol/Cargo.toml
//! # 2. UPDATE_WIRE_SURFACE=1 cargo test -p ainb-plugin-cts-v2 --test wire_surface_gate
//! ```
//!
//! Step 1 is not advisory: the regenerator refuses to stamp a changed
//! surface while the version is unchanged, which is what makes this a semver
//! gate rather than a golden file.
//!
//! ## Why not `cargo-semver-checks`
//!
//! It was evaluated, not dismissed. `cargo-semver-checks 0.49.0` installs and
//! runs against this crate in about 13s:
//!
//! ```text
//! cargo semver-checks --manifest-path crates/ainb-plugin-protocol/Cargo.toml \
//!     --baseline-rev origin/main
//! ```
//!
//! Two things rule it out as THE gate here:
//!
//! 1. It checks the Rust API, not the wire. Renaming a serde key, for example
//!    `#[serde(rename = "secrets:read")]` to `"secrets_read"`, breaks every
//!    shipped plugin manifest and does not touch the Rust API at all.
//!    cargo-semver-checks reports `196 checks: 196 pass` and
//!    `no semver update required`; this gate reports the changed line.
//! 2. It needs a baseline revision, so it cannot run as part of `cargo test`
//!    and is meaningless on `main` itself, where the baseline is HEAD.
//!
//! The two are complements, not alternatives. Wiring cargo-semver-checks into
//! CI with a merge-base baseline belongs to Phase 5, which owns the workflow
//! changes; this gate is what a plain `cargo test` can enforce today.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use ainb_plugin_cts_v2::wire_surface;

/// Directory of the `ainb-plugin-protocol` crate.
fn protocol_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates/ dir")
        .join("ainb-plugin-protocol")
}

/// The `[package] version` declared by `ainb-plugin-protocol`.
fn protocol_version(dir: &Path) -> String {
    let manifest =
        std::fs::read_to_string(dir.join("Cargo.toml")).expect("read protocol Cargo.toml");
    let parsed: toml::Value = toml::from_str(&manifest).expect("parse protocol Cargo.toml");
    parsed
        .get("package")
        .and_then(|p| p.get("version"))
        .and_then(toml::Value::as_str)
        .expect("ainb-plugin-protocol must declare a literal [package] version")
        .to_owned()
}

fn lock_path(dir: &Path) -> PathBuf {
    dir.join("wire-surface.lock")
}

/// Parse `major.minor.patch` for an ordering comparison without pulling in a
/// semver dependency.
fn version_triple(v: &str) -> (u64, u64, u64) {
    let mut parts = v.split(['.', '-', '+']).map(|p| p.parse::<u64>().unwrap_or(0));
    (
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
    )
}

/// First-difference report between two surface documents.
fn diff(committed: &str, rendered: &str) -> String {
    let old: Vec<&str> = committed.lines().collect();
    let new: Vec<&str> = rendered.lines().collect();
    let mut out = String::new();
    let first = (0..old.len().max(new.len())).find(|&i| old.get(i) != new.get(i)).unwrap_or(0);
    let _ = writeln!(out, "first divergence at surface line {}", first + 1);
    for i in first.saturating_sub(2)..(first + 6).min(old.len().max(new.len())) {
        match (old.get(i), new.get(i)) {
            (Some(o), Some(n)) if o == n => {
                let _ = writeln!(out, "  {o}");
            }
            (o, n) => {
                if let Some(o) = o {
                    let _ = writeln!(out, "- {o}");
                }
                if let Some(n) = n {
                    let _ = writeln!(out, "+ {n}");
                }
            }
        }
    }
    let removed = old.iter().filter(|l| !new.contains(*l)).count();
    let added = new.iter().filter(|l| !old.contains(*l)).count();
    let _ = writeln!(
        out,
        "\n{removed} line(s) only in the committed lock, {added} line(s) only in the current source"
    );
    out
}

#[test]
fn protocol_wire_surface_matches_committed_lock() {
    let dir = protocol_dir();
    let version = protocol_version(&dir);
    let rendered = wire_surface::render(&dir, &version).expect("render wire surface");
    let lock = lock_path(&dir);

    if std::env::var_os("UPDATE_WIRE_SURFACE").is_some() {
        regenerate(&lock, &rendered, &version);
        return;
    }

    let committed = std::fs::read_to_string(&lock).unwrap_or_else(|e| {
        panic!(
            "missing wire-surface lock at {}: {e}\n\
             Generate it with: UPDATE_WIRE_SURFACE=1 cargo test -p ainb-plugin-cts-v2 --test wire_surface_gate",
            lock.display()
        )
    });

    let committed_surface = wire_surface::surface_only(&committed);
    let rendered_surface = wire_surface::surface_only(&rendered);
    assert!(
        committed_surface == rendered_surface,
        "ainb-plugin-protocol's public wire surface changed but {} was not regenerated.\n\
         Ten crates depend on this surface. Bump `version` in {}/Cargo.toml, then run:\n  \
         UPDATE_WIRE_SURFACE=1 cargo test -p ainb-plugin-cts-v2 --test wire_surface_gate\n\n{}",
        lock.display(),
        dir.display(),
        diff(&committed_surface, &rendered_surface)
    );

    // The lock records the version the surface was last stamped at. The
    // crate may move ahead of it (a release with no wire change), but never
    // behind it.
    let recorded = wire_surface::recorded_version(&committed).expect("lock version header");
    assert!(
        version_triple(&version) >= version_triple(recorded),
        "ainb-plugin-protocol version {version} is BEHIND the version recorded in \
         {} ({recorded}); a wire surface cannot un-ship",
        lock.display()
    );
}

/// Write a new lock, refusing while the surface has changed and the version
/// has not. This refusal is the semver half of the gate: without it, an
/// author could silence a wire change by regenerating.
fn regenerate(lock: &Path, rendered: &str, version: &str) {
    if let Ok(committed) = std::fs::read_to_string(lock) {
        let changed =
            wire_surface::surface_only(&committed) != wire_surface::surface_only(rendered);
        let recorded = wire_surface::recorded_version(&committed).expect("lock version header");
        assert!(
            !(changed && recorded == version),
            "REFUSING to regenerate {}: the wire surface changed but \
             ainb-plugin-protocol is still at version {version}.\n\
             Ten crates consume this surface. Bump the version first, then re-run.",
            lock.display()
        );
    }
    std::fs::write(lock, rendered).expect("write wire-surface lock");
    eprintln!("wrote {} at protocol version {version}", lock.display());
}
