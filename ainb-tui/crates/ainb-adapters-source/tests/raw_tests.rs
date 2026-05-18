//! RawAdapter integration tests — build a tempdir fixture matching
//! the convention layout and assert every kind is discovered.

use std::path::Path;

use ainb_adapters_source::{RawAdapter, SourceAdapter};

fn write(p: &Path, content: &str) {
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(p, content).unwrap();
}

fn make_fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    write(
        &root.join("skills/commit/SKILL.md"),
        "---\nname: commit\ndescription: well-formatted commits\ntags: [git, workflow]\nrequires: [git]\n---\nBody.\n",
    );
    write(
        &root.join("skills/review-pr/SKILL.md"),
        "# no frontmatter — name falls back to dir\n",
    );

    write(
        &root.join("plugins/reflect/plugin.json"),
        r#"{"name":"reflect","description":"self-improvement","version":"1.0.0"}"#,
    );

    write(
        &root.join("agents/researcher.md"),
        "---\nname: researcher\ndescription: deep search\n---\nbody",
    );

    write(
        &root.join("commands/run.md"),
        "---\nname: run\ndescription: shell out\n---\n",
    );

    write(
        &root.join("hooks/pre-commit/hook.yaml"),
        "name: pre-commit\ndescription: lint on save\nevent: PreToolUse\n",
    );

    write(
        &root.join("mcp-servers.yaml"),
        "- name: filesystem\n  description: fs server\n- name: github\n  description: gh api\n",
    );

    write(
        &root.join("statuslines.yaml"),
        "- name: cost\n  description: token usage\n",
    );

    dir
}

#[test]
fn detect_requires_a_convention_dir() {
    let dir = tempfile::tempdir().unwrap();
    assert!(
        !RawAdapter::new().detect(dir.path()),
        "empty dir should not match"
    );
    std::fs::create_dir(dir.path().join("skills")).unwrap();
    assert!(RawAdapter::new().detect(dir.path()), "skills/ should match");
}

#[test]
fn detect_matches_via_top_level_yaml() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("mcp-servers.yaml"), "[]").unwrap();
    assert!(RawAdapter::new().detect(dir.path()));
}

#[test]
fn detect_is_false_for_missing_path() {
    assert!(!RawAdapter::new().detect(Path::new("/definitely/not/a/dir")));
}

#[test]
fn lists_all_seven_kinds() {
    let fixture = make_fixture();
    let units = RawAdapter::new().list_units(fixture.path()).unwrap();

    let by_kind: std::collections::BTreeMap<String, Vec<String>> =
        units.iter().fold(Default::default(), |mut acc, u| {
            acc.entry(u.kind.clone()).or_default().push(u.name.clone());
            acc
        });

    assert_eq!(
        by_kind.get("skill").unwrap().len(),
        2,
        "expected commit + review-pr"
    );
    assert!(by_kind.get("skill").unwrap().contains(&"commit".to_string()));
    assert!(by_kind.get("skill").unwrap().contains(&"review-pr".to_string()));
    assert_eq!(by_kind.get("plugin").unwrap(), &vec!["reflect".to_string()]);
    assert_eq!(
        by_kind.get("agent").unwrap(),
        &vec!["researcher".to_string()]
    );
    assert_eq!(by_kind.get("command").unwrap(), &vec!["run".to_string()]);
    assert_eq!(
        by_kind.get("hook").unwrap(),
        &vec!["pre-commit".to_string()]
    );
    assert_eq!(
        by_kind.get("mcp-server").unwrap(),
        &vec!["filesystem".to_string(), "github".to_string()]
    );
    assert_eq!(
        by_kind.get("statusline").unwrap(),
        &vec!["cost".to_string()]
    );
}

#[test]
fn skill_frontmatter_fields_populate_descriptor() {
    let fixture = make_fixture();
    let units = RawAdapter::new().list_units(fixture.path()).unwrap();
    let commit = units.iter().find(|u| u.kind == "skill" && u.name == "commit").unwrap();
    assert_eq!(
        commit.description.as_deref(),
        Some("well-formatted commits")
    );
    assert_eq!(commit.tags, vec!["git", "workflow"]);
    assert_eq!(commit.requires, vec!["git"]);
    assert_eq!(commit.path, "skills/commit");
}

#[test]
fn resolve_unit_returns_matching_descriptor() {
    let fixture = make_fixture();
    let resolved = RawAdapter::new().resolve_unit(fixture.path(), "skills/commit").unwrap();
    assert_eq!(resolved.descriptor.name, "commit");
    assert_eq!(resolved.descriptor.kind, "skill");
}

#[test]
fn resolve_unit_errors_on_missing_path() {
    let fixture = make_fixture();
    let err = RawAdapter::new().resolve_unit(fixture.path(), "skills/nope").unwrap_err();
    assert!(err.to_string().contains("no unit at path"), "got: {err}");
}

#[test]
fn empty_directory_yields_empty_list() {
    let dir = tempfile::tempdir().unwrap();
    let units = RawAdapter::new().list_units(dir.path()).unwrap();
    assert!(units.is_empty());
}
