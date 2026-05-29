//! `ainb skill check` integration tests — the bead v12.E.2 tripwire.
//!
//! Drives the dispatch with a `MockBackend` so the network never gets
//! touched. Covers the tabular default output, the `--json` variant
//! and the per-source scoping argument.

use std::collections::BTreeMap;
use std::path::Path;

use ainb_cli::CheckArgs;
use ainb_skill_core::drift::{DriftStatus, MockBackend};
use ainb_skill_core::lockfile::{LockedSource, LockedUnit, Lockfile};
use ainb_skill_core::manifest::{Manifest, SourceEntry};
use ainb_skill_core::paths::{lockfile_path_in, manifest_path_in};

fn tmp_home() -> tempfile::TempDir {
    tempfile::Builder::new().prefix("ainb-skill-check-").tempdir().expect("tempdir")
}

fn seed_manifest(home: &Path, sources: Vec<SourceEntry>) {
    let manifest = Manifest {
        schema_version: 1,
        sources,
        units: Vec::new(),
        defaults: None,
        options: None,
    };
    manifest.save_to(&manifest_path_in(home)).expect("save manifest");
}

fn seed_lockfile(home: &Path, units: Vec<LockedUnit>) {
    let lockfile = Lockfile {
        schema_version: 2,
        generated_at: None,
        sources: vec![LockedSource {
            name: "acme".to_string(),
            uri: "gh:org/acme".to_string(),
            declared_ref: "main".to_string(),
            resolved_sha: None,
            fetched_at: None,
            fetched_path: None,
        }],
        units,
    };
    lockfile.save_to(&lockfile_path_in(home)).expect("save lockfile");
}

fn source(name: &str) -> SourceEntry {
    SourceEntry {
        name: name.to_string(),
        kind: None,
        uri: format!("gh:org/{name}"),
        r#ref: "main".to_string(),
        enabled: true,
        read_only: false,
        target_layout: Vec::new(),
    }
}

fn unit(declared_uri: &str, sha: &str) -> LockedUnit {
    LockedUnit {
        uri: declared_uri.to_string(),
        declared_uri: declared_uri.to_string(),
        kind: "skill".to_string(),
        sha: Some(sha.to_string()),
        deployed: BTreeMap::new(),
        usage: Default::default(),
    }
}

#[test]
fn check_tabular_renders_one_row_per_unit_with_status_glyph() {
    let home = tmp_home();
    seed_manifest(home.path(), vec![source("acme")]);
    seed_lockfile(
        home.path(),
        vec![
            unit("gh:org/acme@main/skills/foo", "deadbeef"),
            unit("gh:org/acme@main/skills/bar", "cafebabe"),
        ],
    );

    let mut mock = MockBackend::new();
    mock.expect("acme", "deadbeef", DriftStatus::InSync);
    mock.expect(
        "acme",
        "cafebabe",
        DriftStatus::Outdated { behind: 4 },
    );

    let mut out = Vec::new();
    ainb_cli::skill::run_check(
        home.path(),
        CheckArgs {
            source: None,
            json: false,
        },
        &mock,
        &mut out,
    )
    .expect("check ok");
    let text = String::from_utf8(out).expect("utf8");

    // Tripwire: each unit + its drift status appear in the table.
    assert!(
        text.contains("gh:org/acme@main/skills/foo"),
        "table missing foo URI:\n{text}"
    );
    assert!(
        text.contains("gh:org/acme@main/skills/bar"),
        "table missing bar URI:\n{text}"
    );
    assert!(
        text.contains("in-sync"),
        "table missing in-sync status:\n{text}"
    );
    assert!(
        text.contains("outdated"),
        "table missing outdated status:\n{text}"
    );
    // Behind-count surfaces in the human-readable column.
    assert!(text.contains("behind 4"), "missing behind count:\n{text}");
}

#[test]
fn check_json_emits_parseable_status_per_unit() {
    let home = tmp_home();
    seed_manifest(home.path(), vec![source("acme")]);
    seed_lockfile(
        home.path(),
        vec![
            unit("gh:org/acme@main/skills/foo", "deadbeef"),
            unit("gh:org/acme@main/skills/bar", "cafebabe"),
        ],
    );

    let mut mock = MockBackend::new();
    mock.expect("acme", "deadbeef", DriftStatus::InSync);
    mock.expect(
        "acme",
        "cafebabe",
        DriftStatus::Diverged { ahead: 1, behind: 2 },
    );

    let mut out = Vec::new();
    ainb_cli::skill::run_check(
        home.path(),
        CheckArgs {
            source: None,
            json: true,
        },
        &mock,
        &mut out,
    )
    .expect("check ok");
    let text = String::from_utf8(out).expect("utf8");

    let value: serde_json::Value =
        serde_json::from_str(text.trim()).expect("json parse");
    let arr = value.as_array().expect("top-level array");
    assert_eq!(arr.len(), 2, "two rows expected, got: {text}");

    let by_unit: BTreeMap<String, &serde_json::Value> = arr
        .iter()
        .map(|v| (v["unit"].as_str().unwrap().to_string(), v))
        .collect();

    let foo = by_unit["gh:org/acme@main/skills/foo"];
    assert_eq!(foo["status"], "in-sync");

    let bar = by_unit["gh:org/acme@main/skills/bar"];
    assert_eq!(bar["status"], "diverged");
    assert_eq!(bar["ahead"], 1);
    assert_eq!(bar["behind"], 2);
}

#[test]
fn check_with_source_filter_scopes_to_named_source() {
    let home = tmp_home();
    seed_manifest(home.path(), vec![source("acme"), source("widgets")]);

    // Two sources, two units.
    let lockfile = Lockfile {
        schema_version: 2,
        generated_at: None,
        sources: vec![
            LockedSource {
                name: "acme".to_string(),
                uri: "gh:org/acme".to_string(),
                declared_ref: "main".to_string(),
                resolved_sha: None,
                fetched_at: None,
                fetched_path: None,
            },
            LockedSource {
                name: "widgets".to_string(),
                uri: "gh:org/widgets".to_string(),
                declared_ref: "main".to_string(),
                resolved_sha: None,
                fetched_at: None,
                fetched_path: None,
            },
        ],
        units: vec![
            unit("gh:org/acme@main/skills/foo", "deadbeef"),
            unit("gh:org/widgets@main/skills/bar", "cafebabe"),
        ],
    };
    lockfile.save_to(&lockfile_path_in(home.path())).expect("save lockfile");

    let mut mock = MockBackend::new();
    // Only register acme; widgets shouldn't be queried.
    mock.expect("acme", "deadbeef", DriftStatus::InSync);

    let mut out = Vec::new();
    ainb_cli::skill::run_check(
        home.path(),
        CheckArgs {
            source: Some("acme".to_string()),
            json: false,
        },
        &mock,
        &mut out,
    )
    .expect("check ok with scope");
    let text = String::from_utf8(out).expect("utf8");
    assert!(text.contains("gh:org/acme@main/skills/foo"));
    assert!(
        !text.contains("gh:org/widgets@main/skills/bar"),
        "widgets should be filtered out:\n{text}"
    );
}

#[test]
fn check_empty_lockfile_emits_human_marker() {
    let home = tmp_home();
    seed_manifest(home.path(), vec![source("acme")]);
    // No lockfile written.
    let mock = MockBackend::new();
    let mut out = Vec::new();
    ainb_cli::skill::run_check(
        home.path(),
        CheckArgs {
            source: None,
            json: false,
        },
        &mock,
        &mut out,
    )
    .expect("check ok on empty lockfile");
    let text = String::from_utf8(out).expect("utf8");
    assert!(
        text.contains("no units"),
        "expected empty-lockfile marker, got: {text}"
    );
}
