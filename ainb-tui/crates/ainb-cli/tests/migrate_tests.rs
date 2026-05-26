//! `ainb migrate ...` integration tests. Three subcommands —
//! --check, --clean [--backup], --from-bootstrap — each driven
//! against fully-sandboxed adapter homes and local-fixture toolkit
//! directories so the suite never touches a real `~/.claude`.

use std::path::{Path, PathBuf};

use ainb_cli::{AddArgs, Command, InstallArgs, MigrateArgs, SkillCommand, SourceCommand, dispatch};
use ainb_skill_core::manifest::{Manifest, UnitEntry};
use ainb_skill_core::paths::manifest_path_in;

static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

const ALL_TOOL_ENV_VARS: &[&str] = &[
    "AINB_TOOL_HOME_CLAUDE",
    "AINB_TOOL_HOME_CODEX",
    "AINB_TOOL_HOME_COPILOT",
    "AINB_TOOL_HOME_GEMINI",
    "AINB_TOOL_HOME_CURSOR",
    "AINB_TOOL_HOME_AMAZONQ",
    "AINB_TOOL_HOME_CLAUDE_DESKTOP",
    "AINB_TOOL_HOME_CLINE",
    "AINB_TOOL_HOME_ROO",
];

fn tmp_home() -> tempfile::TempDir {
    tempfile::Builder::new().prefix("ainb-migrate-").tempdir().expect("tempdir")
}

fn run_migrate(home: &Path, args: MigrateArgs) -> (String, anyhow::Result<()>) {
    let mut buf = Vec::new();
    let res = dispatch(home, Command::Migrate(args), &mut buf);
    (String::from_utf8(buf).expect("utf8"), res)
}

fn with_tool_homes<R>(base: &Path, body: impl FnOnce() -> R) -> R {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    for var in ALL_TOOL_ENV_VARS {
        let tool = var.trim_start_matches("AINB_TOOL_HOME_").to_lowercase();
        let dir = base.join(&tool);
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var(var, &dir);
    }
    let r = body();
    for var in ALL_TOOL_ENV_VARS {
        std::env::remove_var(var);
    }
    r
}

/// Seed a local toolkit fixture with `external-dependencies.yaml` and
/// a few skills under `packages/skills/`. Returns the toolkit root.
fn seed_toolkit_fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let toolkit = dir.path();

    // Two bundled skills under packages/skills/{commit,herdr}.
    for name in ["commit", "herdr"] {
        let p: PathBuf = toolkit.join(format!("packages/skills/{name}/SKILL.md"));
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(
            &p,
            format!("---\nname: {name}\ndescription: {name} skill\n---\nbody\n"),
        )
        .unwrap();
    }

    let yaml = r#"
version: "1.0.0"
bundled-skills:
  - name: commit
    path: packages/skills/commit
    purpose: "commit messages"
  - name: herdr
    path: packages/skills/herdr
    purpose: "herder"
agent-skills: []
"#;
    std::fs::write(toolkit.join("external-dependencies.yaml"), yaml).unwrap();
    dir
}

#[test]
fn migrate_check_empty_roots_reports_no_units() {
    let home = tmp_home();
    let base = tempfile::tempdir().unwrap();
    with_tool_homes(base.path(), || {
        // Adapter env vars now point at empty subdirs; list_installed
        // returns nothing for each.
        let (out, res) = run_migrate(
            home.path(),
            MigrateArgs {
                check: true,
                clean: false,
                backup: false,
                from_bootstrap: false,
                toolkit_root: None,
                yes: false,
                dry_run: false,
            },
        );
        res.expect("check ok");
        assert!(out.contains("# total: 0 unit(s)"), "got: {out}");
        // Every adapter shows up either as (empty) or 0 unit(s) line.
        for tool in [
            "claude",
            "codex",
            "copilot",
            "gemini",
            "cursor",
            "amazonq",
            "claude-desktop",
            "cline",
            "roo",
        ] {
            assert!(out.contains(tool), "missing tool {tool} in: {out}");
        }
    });
}

#[test]
fn migrate_check_reports_installed_units() {
    let home = tmp_home();
    let base = tempfile::tempdir().unwrap();
    let src = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(src.path().join("skills/commit")).unwrap();
    std::fs::write(
        src.path().join("skills/commit/SKILL.md"),
        "---\nname: commit\n---\n",
    )
    .unwrap();
    let local_uri = format!("local:{}", src.path().display());

    with_tool_homes(base.path(), || {
        // Add the source + install once so list_installed has something
        // to find.
        let mut buf = Vec::new();
        dispatch(
            home.path(),
            Command::Source {
                action: SourceCommand::Add(AddArgs {
                    uri: local_uri.clone(),
                    name: Some("fix".into()),
                    kind: None,
                }),
            },
            &mut buf,
        )
        .unwrap();
        dispatch(
            home.path(),
            Command::Skill {
                action: SkillCommand::Install(InstallArgs {
                    uri: format!("{local_uri}@main/skills/commit"),
                    targets: Some("claude,codex".into()),
                    dry_run: false,
                    yes: true,
                }),
            },
            &mut buf,
        )
        .unwrap();

        let (out, res) = run_migrate(
            home.path(),
            MigrateArgs {
                check: true,
                clean: false,
                backup: false,
                from_bootstrap: false,
                toolkit_root: None,
                yes: false,
                dry_run: false,
            },
        );
        res.expect("check ok");
        assert!(out.contains("claude: 1 unit"), "got: {out}");
        assert!(out.contains("codex: 1 unit"), "got: {out}");
    });
}

#[test]
fn migrate_clean_errors_when_manifest_has_no_units() {
    let home = tmp_home();
    let base = tempfile::tempdir().unwrap();
    with_tool_homes(base.path(), || {
        let (_out, res) = run_migrate(
            home.path(),
            MigrateArgs {
                check: false,
                clean: true,
                backup: false,
                from_bootstrap: false,
                toolkit_root: None,
                yes: true,
                dry_run: false,
            },
        );
        let err = res.unwrap_err().to_string();
        assert!(err.contains("manifest declares no units"), "got: {err}");
    });
}

#[test]
fn migrate_clean_wipes_and_syncs_from_manifest() {
    let home = tmp_home();
    let base = tempfile::tempdir().unwrap();
    let src = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(src.path().join("skills/commit")).unwrap();
    std::fs::write(
        src.path().join("skills/commit/SKILL.md"),
        "---\nname: commit\n---\nv1\n",
    )
    .unwrap();
    let local_uri = format!("local:{}", src.path().display());
    let unit_uri = format!("{local_uri}@main/skills/commit");

    with_tool_homes(base.path(), || {
        // Install, then drop a stray file that doesn't belong.
        let mut buf = Vec::new();
        dispatch(
            home.path(),
            Command::Source {
                action: SourceCommand::Add(AddArgs {
                    uri: local_uri.clone(),
                    name: Some("fix".into()),
                    kind: None,
                }),
            },
            &mut buf,
        )
        .unwrap();
        dispatch(
            home.path(),
            Command::Skill {
                action: SkillCommand::Install(InstallArgs {
                    uri: unit_uri.clone(),
                    targets: Some("claude".into()),
                    dry_run: false,
                    yes: true,
                }),
            },
            &mut buf,
        )
        .unwrap();
        std::fs::write(
            base.path().join("claude/skills/commit/STRAY.md"),
            "i don't belong",
        )
        .unwrap();
        assert!(base.path().join("claude/skills/commit/STRAY.md").exists());

        // Make the manifest declare the same unit so clean → sync
        // reinstalls it after the wipe.
        let mut manifest = Manifest::load_from(&manifest_path_in(home.path())).unwrap();
        manifest.units.push(UnitEntry {
            uri: unit_uri.clone(),
            targets: Some(vec!["claude".into()]),
            shadowed_by: None,
        });
        manifest.save_to(&manifest_path_in(home.path())).unwrap();

        let (out, res) = run_migrate(
            home.path(),
            MigrateArgs {
                check: false,
                clean: true,
                backup: false,
                from_bootstrap: false,
                toolkit_root: None,
                yes: true,
                dry_run: false,
            },
        );
        res.expect("clean ok");
        assert!(out.contains("wiped"), "got: {out}");
        assert!(
            out.contains("installed") || out.contains("install"),
            "got: {out}"
        );

        // Stray gone, real unit re-installed.
        assert!(
            !base.path().join("claude/skills/commit/STRAY.md").exists(),
            "stray file must be wiped"
        );
        assert!(
            base.path().join("claude/skills/commit/SKILL.md").exists(),
            "real unit must be reinstalled"
        );
    });
}

#[test]
fn migrate_clean_backup_snapshots_existing_state() {
    let home = tmp_home();
    let base = tempfile::tempdir().unwrap();
    let src = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(src.path().join("skills/commit")).unwrap();
    std::fs::write(
        src.path().join("skills/commit/SKILL.md"),
        "---\nname: commit\n---\n",
    )
    .unwrap();
    let local_uri = format!("local:{}", src.path().display());
    let unit_uri = format!("{local_uri}@main/skills/commit");

    with_tool_homes(base.path(), || {
        let mut buf = Vec::new();
        dispatch(
            home.path(),
            Command::Source {
                action: SourceCommand::Add(AddArgs {
                    uri: local_uri.clone(),
                    name: Some("fix".into()),
                    kind: None,
                }),
            },
            &mut buf,
        )
        .unwrap();
        dispatch(
            home.path(),
            Command::Skill {
                action: SkillCommand::Install(InstallArgs {
                    uri: unit_uri.clone(),
                    targets: Some("claude".into()),
                    dry_run: false,
                    yes: true,
                }),
            },
            &mut buf,
        )
        .unwrap();

        let mut manifest = Manifest::load_from(&manifest_path_in(home.path())).unwrap();
        manifest.units.push(UnitEntry {
            uri: unit_uri.clone(),
            targets: Some(vec!["claude".into()]),
            shadowed_by: None,
        });
        manifest.save_to(&manifest_path_in(home.path())).unwrap();

        let (out, res) = run_migrate(
            home.path(),
            MigrateArgs {
                check: false,
                clean: true,
                backup: true,
                from_bootstrap: false,
                toolkit_root: None,
                yes: true,
                dry_run: false,
            },
        );
        res.expect("clean ok");
        assert!(out.contains("backup"), "got: {out}");

        let backups_root = home.path().join("backups");
        let entries: Vec<_> = std::fs::read_dir(&backups_root).unwrap().flatten().collect();
        assert_eq!(entries.len(), 1, "expected one backup dir");
        let snapshot = entries[0].path();
        assert!(
            snapshot.join("claude/skills/commit/SKILL.md").exists(),
            "backup must contain the pre-wipe SKILL.md at {snapshot:?}"
        );
    });
}

#[test]
fn migrate_clean_dry_run_does_not_wipe() {
    let home = tmp_home();
    let base = tempfile::tempdir().unwrap();
    let src = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(src.path().join("skills/commit")).unwrap();
    std::fs::write(
        src.path().join("skills/commit/SKILL.md"),
        "---\nname: commit\n---\n",
    )
    .unwrap();
    let local_uri = format!("local:{}", src.path().display());

    with_tool_homes(base.path(), || {
        let mut buf = Vec::new();
        dispatch(
            home.path(),
            Command::Source {
                action: SourceCommand::Add(AddArgs {
                    uri: local_uri.clone(),
                    name: Some("fix".into()),
                    kind: None,
                }),
            },
            &mut buf,
        )
        .unwrap();
        dispatch(
            home.path(),
            Command::Skill {
                action: SkillCommand::Install(InstallArgs {
                    uri: format!("{local_uri}@main/skills/commit"),
                    targets: Some("claude".into()),
                    dry_run: false,
                    yes: true,
                }),
            },
            &mut buf,
        )
        .unwrap();

        // Declare the same unit in the manifest so --clean doesn't bail
        // on "manifest has no units".
        let mut manifest = Manifest::load_from(&manifest_path_in(home.path())).unwrap();
        manifest.units.push(UnitEntry {
            uri: format!("{local_uri}@main/skills/commit"),
            targets: Some(vec!["claude".into()]),
            shadowed_by: None,
        });
        manifest.save_to(&manifest_path_in(home.path())).unwrap();

        let (out, res) = run_migrate(
            home.path(),
            MigrateArgs {
                check: false,
                clean: true,
                backup: false,
                from_bootstrap: false,
                toolkit_root: None,
                yes: false,
                dry_run: true,
            },
        );
        res.expect("dry-run ok");
        assert!(out.contains("dry-run"), "got: {out}");
        // Pre-existing file still present.
        assert!(
            base.path().join("claude/skills/commit/SKILL.md").exists(),
            "dry-run must not wipe"
        );
    });
}

#[test]
fn migrate_from_bootstrap_seeds_manifest_with_toolkit_source_and_units() {
    let home = tmp_home();
    let toolkit = seed_toolkit_fixture();

    let (out, res) = run_migrate(
        home.path(),
        MigrateArgs {
            check: false,
            clean: false,
            backup: false,
            from_bootstrap: true,
            toolkit_root: Some(toolkit.path().to_path_buf()),
            yes: true,
            dry_run: false,
        },
    );
    res.expect("from-bootstrap ok");
    assert!(out.contains("source `toolkit`"), "got: {out}");
    assert!(out.contains("unit entries added: 2"), "got: {out}");

    let manifest = Manifest::load_from(&manifest_path_in(home.path())).unwrap();
    assert!(manifest.sources.iter().any(|s| s.name == "toolkit"));
    assert_eq!(manifest.units.len(), 2);
    let uris: Vec<_> = manifest.units.iter().map(|u| u.uri.clone()).collect();
    assert!(uris.iter().any(|u| u.contains("packages/skills/commit")));
    assert!(uris.iter().any(|u| u.contains("packages/skills/herdr")));
}

#[test]
fn migrate_from_bootstrap_is_idempotent() {
    let home = tmp_home();
    let toolkit = seed_toolkit_fixture();

    for _ in 0..2 {
        let (_o, res) = run_migrate(
            home.path(),
            MigrateArgs {
                check: false,
                clean: false,
                backup: false,
                from_bootstrap: true,
                toolkit_root: Some(toolkit.path().to_path_buf()),
                yes: true,
                dry_run: false,
            },
        );
        res.expect("from-bootstrap ok");
    }
    let manifest = Manifest::load_from(&manifest_path_in(home.path())).unwrap();
    assert_eq!(
        manifest.sources.iter().filter(|s| s.name == "toolkit").count(),
        1,
        "toolkit source must not duplicate"
    );
    assert_eq!(manifest.units.len(), 2, "units must not duplicate");
}

#[test]
fn migrate_from_bootstrap_errors_on_missing_yaml() {
    let home = tmp_home();
    let empty = tempfile::tempdir().unwrap();

    let (_out, res) = run_migrate(
        home.path(),
        MigrateArgs {
            check: false,
            clean: false,
            backup: false,
            from_bootstrap: true,
            toolkit_root: Some(empty.path().to_path_buf()),
            yes: true,
            dry_run: false,
        },
    );
    let err = res.unwrap_err().to_string();
    assert!(err.contains("external-dependencies.yaml"), "got: {err}");
}

#[test]
fn migrate_without_any_mode_errors() {
    let home = tmp_home();
    let (_out, res) = run_migrate(
        home.path(),
        MigrateArgs {
            check: false,
            clean: false,
            backup: false,
            from_bootstrap: false,
            toolkit_root: None,
            yes: false,
            dry_run: false,
        },
    );
    let err = res.unwrap_err().to_string();
    assert!(
        err.contains("--check") && err.contains("--clean"),
        "got: {err}"
    );
}
