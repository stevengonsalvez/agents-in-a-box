//! Structural store fence: `sqlx` may appear in exactly ONE workspace crate.
//!
//! Repo functions are the single door to the store. A crate that adds `sqlx`
//! is reaching around them for raw SQL, which is how schema knowledge leaks out
//! of `ainb-hangar-store` and how a migration stops being the one place that
//! defines the shape. This is a structural check over `cargo metadata`, not a
//! grep: a dependency added under a rename, a feature, or a target-specific
//! table is still a dependency and is still caught.

use std::collections::BTreeSet;
use std::process::Command;

/// The crates allowed to depend on `sqlx`.
///
/// The plan's rule is "`ainb-hangar-store` ONLY". The TREE already has two more
/// (DRIFT, recorded here rather than papered over): `ainb-hangar-daemon` and
/// `ainb` (the package name of `crates/ainb-core`, whose hangar workspace
/// bootstrap issues raw SQL, see its Cargo.toml comment). They are
/// grandfathered by NAME so each is reviewable, and the fence still does its
/// real job: it stops the set growing. NOTHING may be added here without
/// deleting the raw SQL that made it necessary.
const ALLOWED: &[&str] = &["ainb-hangar-store", "ainb-hangar-daemon", "ainb"];

#[test]
fn sqlx_appears_only_in_the_crates_that_own_raw_sql() {
    let output = Command::new(env!("CARGO"))
        .args(["metadata", "--format-version", "1", "--no-deps"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("cargo metadata");
    assert!(
        output.status.success(),
        "cargo metadata failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let metadata: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("metadata json");

    let offenders: BTreeSet<String> = metadata["packages"]
        .as_array()
        .expect("packages")
        .iter()
        .filter(|package| {
            package["dependencies"].as_array().is_some_and(|dependencies| {
                dependencies.iter().any(|dependency| dependency["name"] == "sqlx")
            })
        })
        .filter_map(|package| package["name"].as_str().map(ToString::to_string))
        .filter(|name| !ALLOWED.contains(&name.as_str()))
        .collect();

    assert!(
        offenders.is_empty(),
        "these crates declare sqlx directly: {offenders:?}. \
         The store's repo functions are the ONE door to the database; \
         add the query to ainb-hangar-store/src/repo/ instead of reaching \
         for raw SQL here."
    );
}

#[test]
fn this_crate_is_not_one_of_them() {
    let manifest = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"))
        .expect("read own manifest");
    assert!(
        !manifest.lines().any(|line| line.trim_start().starts_with("sqlx")),
        "ainb-acp must reach the store only through repo functions"
    );
}

#[test]
fn ci_lane_verification_throwaway() {
    // Deliberate red proving the ainb-acp CI lane executes this crate's
    // tests; reverted before merge, run link recorded in the PR.
    panic!("throwaway: ainb-acp lane fires");
}
