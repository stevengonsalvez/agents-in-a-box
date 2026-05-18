//! Lockfile load/save tests.

use std::collections::BTreeMap;

use ainb_skill_core::lockfile::{DeployedRef, LockedSource, LockedUnit, Lockfile};
use ainb_skill_core::paths::lockfile_path_in;

fn tmp_home() -> tempfile::TempDir {
    tempfile::Builder::new().prefix("ainb-lock-test-").tempdir().expect("tempdir")
}

#[test]
fn missing_file_yields_default() {
    let home = tmp_home();
    let path = lockfile_path_in(home.path());
    let l = Lockfile::load_from(&path).expect("load missing");
    assert_eq!(l, Lockfile::default());
}

#[test]
fn round_trip_with_deployed_and_skipped() {
    let home = tmp_home();
    let path = lockfile_path_in(home.path());

    let mut deployed = BTreeMap::new();
    deployed.insert(
        "claude".to_string(),
        DeployedRef::Deployed {
            path: "~/.claude/skills/commit".into(),
            file_hashes: {
                let mut h = BTreeMap::new();
                h.insert("SKILL.md".into(), "sha256:abc".into());
                h
            },
        },
    );
    deployed.insert(
        "codex".to_string(),
        DeployedRef::Skipped {
            reason: "codex adapter does not accept plugin kind".into(),
        },
    );

    let l = Lockfile {
        schema_version: 1,
        generated_at: Some("2026-05-14T20:00:00Z".into()),
        sources: vec![LockedSource {
            name: "toolkit".into(),
            uri: "gh:stevengonsalvez/ai-coder-rules".into(),
            declared_ref: "main".into(),
            resolved_sha: Some("0e9bc28a1b".into()),
            fetched_at: Some("2026-05-14T20:00:00Z".into()),
            fetched_path: Some("/Users/me/.agents-in-a-box/cache/toolkit/0e9bc28a".into()),
        }],
        units: vec![LockedUnit {
            uri: "gh:stevengonsalvez/ai-coder-rules@0e9bc28/toolkit/packages/skills/commit".into(),
            declared_uri: "gh:stevengonsalvez/ai-coder-rules@main/toolkit/packages/skills/commit"
                .into(),
            kind: "skill".into(),
            sha: Some("0e9bc28a1b".into()),
            deployed,
        }],
    };

    l.save_to(&path).expect("save");
    let reloaded = Lockfile::load_from(&path).expect("reload");
    assert_eq!(reloaded, l);
}

#[test]
fn pending_uninstall_round_trips() {
    let home = tmp_home();
    let path = lockfile_path_in(home.path());

    let mut deployed = BTreeMap::new();
    deployed.insert("claude".to_string(), DeployedRef::PendingUninstall);

    let l = Lockfile {
        units: vec![LockedUnit {
            uri: "gh:a/b@sha/x".into(),
            declared_uri: "gh:a/b@main/x".into(),
            kind: "skill".into(),
            sha: None,
            deployed,
        }],
        ..Lockfile::default()
    };

    l.save_to(&path).unwrap();
    let reloaded = Lockfile::load_from(&path).unwrap();
    assert_eq!(reloaded, l);
}

#[test]
fn mark_units_pending_uninstall_by_source_uri() {
    let mut l = Lockfile::default();

    let mut dep_a = BTreeMap::new();
    dep_a.insert(
        "claude".into(),
        DeployedRef::Deployed {
            path: "~/.claude/skills/x".into(),
            file_hashes: BTreeMap::new(),
        },
    );
    l.units.push(LockedUnit {
        uri: "gh:org/keep@sha/x".into(),
        declared_uri: "gh:org/keep@main/x".into(),
        kind: "skill".into(),
        sha: None,
        deployed: dep_a,
    });

    let mut dep_b = BTreeMap::new();
    dep_b.insert(
        "claude".into(),
        DeployedRef::Deployed {
            path: "~/.claude/skills/y".into(),
            file_hashes: BTreeMap::new(),
        },
    );
    dep_b.insert(
        "codex".into(),
        DeployedRef::Deployed {
            path: "~/.codex/skills/y".into(),
            file_hashes: BTreeMap::new(),
        },
    );
    l.units.push(LockedUnit {
        uri: "gh:org/drop@sha/y".into(),
        declared_uri: "gh:org/drop@main/y".into(),
        kind: "skill".into(),
        sha: None,
        deployed: dep_b,
    });

    let affected = l.mark_units_pending_uninstall_by_source_uri("gh:org/drop");
    assert_eq!(affected, 1);

    // The "keep" unit is untouched.
    let keep = l.units.iter().find(|u| u.declared_uri.contains("keep")).unwrap();
    assert!(matches!(
        keep.deployed.get("claude").unwrap(),
        DeployedRef::Deployed { .. }
    ));

    // The "drop" unit is flagged on every tool.
    let drop = l.units.iter().find(|u| u.declared_uri.contains("drop")).unwrap();
    assert!(drop.deployed.values().all(|d| matches!(d, DeployedRef::PendingUninstall)));
}

#[test]
fn rejects_invalid_yaml() {
    let home = tmp_home();
    let path = lockfile_path_in(home.path());
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, b"not: [valid").unwrap();
    let err = Lockfile::load_from(&path).unwrap_err();
    assert!(err.to_string().contains("invalid"), "got: {err}");
}
