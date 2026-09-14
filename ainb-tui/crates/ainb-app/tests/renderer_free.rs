//! Boundary fence: `ainb-app` must build without a terminal renderer.
//!
//! The crate exists so a desktop or web host can drive the same state machine
//! the TUI draws. ratatui or crossterm anywhere in its normal dependency tree,
//! declared directly or pulled in through another crate, ties it back to a
//! terminal. Two checks, because they fail differently: the manifest check
//! names the line that was added, the resolve-graph check catches a transitive
//! edge the manifest cannot show. A third keeps the reducer's event enum
//! behind `dispatch` in every build a host ships.

use std::collections::{BTreeSet, HashMap};
use std::process::Command;

const RENDERER_CRATES: &[&str] = &["ratatui", "crossterm"];

#[test]
fn the_manifest_declares_no_renderer_crate() {
    let manifest = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"))
        .expect("read ainb-app manifest");
    let declared: Vec<&str> = manifest
        .lines()
        .map(str::trim_start)
        .filter(|line| {
            RENDERER_CRATES.iter().any(|name| {
                line.strip_prefix(name)
                    .is_some_and(|rest| rest.trim_start().starts_with(['=', '.']))
            })
        })
        .collect();
    assert!(
        declared.is_empty(),
        "ainb-app declares a renderer crate: {declared:?}. Keep drawing code in ainb-core."
    );
}

#[test]
fn no_renderer_crate_is_reachable_through_normal_dependencies() {
    let output = Command::new(env!("CARGO"))
        .args(["metadata", "--format-version", "1"])
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

    let names: HashMap<&str, &str> = metadata["packages"]
        .as_array()
        .expect("packages")
        .iter()
        .filter_map(|package| Some((package["id"].as_str()?, package["name"].as_str()?)))
        .collect();
    let root = names
        .iter()
        .find_map(|(id, name)| (*name == "ainb-app").then_some(*id))
        .expect("ainb-app in metadata");
    let nodes: HashMap<&str, &serde_json::Value> = metadata["resolve"]["nodes"]
        .as_array()
        .expect("resolve nodes")
        .iter()
        .filter_map(|node| Some((node["id"].as_str()?, node)))
        .collect();

    // Walk only edges with at least one normal (non-dev, non-build) kind.
    let mut seen = BTreeSet::new();
    let mut stack = vec![root];
    while let Some(id) = stack.pop() {
        if !seen.insert(id) {
            continue;
        }
        let Some(deps) = nodes.get(id).and_then(|node| node["deps"].as_array()) else {
            continue;
        };
        for dep in deps {
            let normal = dep["dep_kinds"]
                .as_array()
                .is_some_and(|kinds| kinds.iter().any(|kind| kind["kind"].is_null()));
            if let (true, Some(pkg)) = (normal, dep["pkg"].as_str()) {
                stack.push(pkg);
            }
        }
    }

    let reached: BTreeSet<&str> = seen
        .iter()
        .filter_map(|id| names.get(id).copied())
        .filter(|name| RENDERER_CRATES.contains(name))
        .collect();
    assert!(
        reached.is_empty(),
        "ainb-app reaches {reached:?} through its normal dependencies; \
         `cargo tree -p ainb-app -e normal -i <crate>` shows the path"
    );
}

/// `test-support` makes the reducer's event enum public so integration tests
/// can assert on resolved events. A build any host ships must never enable
/// it, or a host could match on that enum again instead of dispatching.
#[test]
fn test_support_is_off_in_the_normal_dependency_graph() {
    let output = Command::new(env!("CARGO"))
        .args([
            "tree",
            "--workspace",
            "--edges",
            "normal",
            "--invert",
            "ainb-app",
            "--format",
            "{p} {f}",
            "--prefix",
            "none",
            "--depth",
            "0",
        ])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("cargo tree");
    assert!(
        output.status.success(),
        "cargo tree failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let tree = String::from_utf8_lossy(&output.stdout);
    let features = tree
        .lines()
        .find(|line| line.starts_with("ainb-app "))
        .and_then(|line| line.rsplit(' ').next())
        .unwrap_or_else(|| panic!("ainb-app missing from cargo tree output:\n{tree}"));
    assert!(
        !features.split(',').any(|feature| feature == "test-support"),
        "a normal dependency enables ainb-app/test-support ({features}); \
         `cargo tree --workspace -e normal -i ainb-app -e features` shows which"
    );
}
