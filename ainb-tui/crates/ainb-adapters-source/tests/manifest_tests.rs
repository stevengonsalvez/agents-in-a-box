//! Source-side ManifestAdapter tests.

use ainb_adapters_source::{ManifestAdapter, SourceAdapter};

fn write_manifest(dir: &std::path::Path, body: &str) {
    std::fs::write(dir.join("manifest.yaml"), body).unwrap();
}

#[test]
fn detect_finds_manifest_yaml() {
    let dir = tempfile::tempdir().unwrap();
    assert!(!ManifestAdapter::new().detect(dir.path()));
    write_manifest(dir.path(), "units: []\n");
    assert!(ManifestAdapter::new().detect(dir.path()));
}

#[test]
fn parses_units_with_full_metadata() {
    let dir = tempfile::tempdir().unwrap();
    write_manifest(
        dir.path(),
        r#"
units:
  - path: skills/commit
    kind: skill
    name: commit
    description: well-formatted commits
    tags: [git, workflow]
    requires: [git]
  - path: plugins/reflect
    kind: plugin
    name: reflect
    description: self improvement
"#,
    );
    let units = ManifestAdapter::new().list_units(dir.path()).unwrap();
    assert_eq!(units.len(), 2);

    let commit = units.iter().find(|u| u.name == "commit").unwrap();
    assert_eq!(commit.kind, "skill");
    assert_eq!(
        commit.description.as_deref(),
        Some("well-formatted commits")
    );
    assert_eq!(commit.tags, vec!["git", "workflow"]);
    assert_eq!(commit.requires, vec!["git"]);
}

#[test]
fn name_defaults_to_basename_of_path() {
    let dir = tempfile::tempdir().unwrap();
    write_manifest(
        dir.path(),
        "units:\n  - path: skills/no-name-field\n    kind: skill\n",
    );
    let units = ManifestAdapter::new().list_units(dir.path()).unwrap();
    assert_eq!(units[0].name, "no-name-field");
}

#[test]
fn empty_units_list_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    write_manifest(dir.path(), "schema_version: 1\nunits: []\n");
    assert!(ManifestAdapter::new().list_units(dir.path()).unwrap().is_empty());
}

#[test]
fn rejects_invalid_yaml() {
    let dir = tempfile::tempdir().unwrap();
    write_manifest(dir.path(), "units: [not valid");
    let err = ManifestAdapter::new().list_units(dir.path()).unwrap_err();
    assert!(err.to_string().contains("parsing"), "got: {err}");
}

#[test]
fn resolve_unit_returns_descriptor() {
    let dir = tempfile::tempdir().unwrap();
    write_manifest(
        dir.path(),
        "units:\n  - path: skills/x\n    kind: skill\n    name: x\n",
    );
    let resolved = ManifestAdapter::new().resolve_unit(dir.path(), "skills/x").unwrap();
    assert_eq!(resolved.descriptor.name, "x");
    assert_eq!(resolved.descriptor.kind, "skill");
}
