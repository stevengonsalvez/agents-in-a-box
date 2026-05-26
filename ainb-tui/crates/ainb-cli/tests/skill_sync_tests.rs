//! `ainb skill sync` integration tests — reconciles the manifest's
//! declared unit set with what the lockfile actually holds. Drives
//! against local-source fixtures so the suite stays offline.

use std::path::{Path, PathBuf};

use ainb_cli::{AddArgs, Command, InstallArgs, SkillCommand, SourceCommand, SyncArgs, dispatch};
use ainb_skill_core::lockfile::Lockfile;
use ainb_skill_core::manifest::{Manifest, UnitEntry};
use ainb_skill_core::paths::{lockfile_path_in, manifest_path_in};

static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn tmp_home() -> tempfile::TempDir {
    tempfile::Builder::new().prefix("ainb-skill-sync-").tempdir().expect("tempdir")
}

fn run(home: &Path, action: SkillCommand) -> (String, anyhow::Result<()>) {
    let mut buf = Vec::new();
    let res = dispatch(home, Command::Skill { action }, &mut buf);
    (String::from_utf8(buf).expect("utf8"), res)
}

fn with_all_tool_homes_under<R>(base: &Path, body: impl FnOnce() -> R) -> R {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    const VARS: &[&str] = &[
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
    for var in VARS {
        let tool = var.trim_start_matches("AINB_TOOL_HOME_").to_lowercase();
        let dir = base.join(&tool);
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var(var, &dir);
    }
    let r = body();
    for var in VARS {
        std::env::remove_var(var);
    }
    r
}

fn seed_source(home: &Path, name: &str) -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    let p: PathBuf = dir.path().join("skills/commit/SKILL.md");
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(
        &p,
        "---\nname: commit\ndescription: well-formed commits\n---\nbody\n",
    )
    .unwrap();
    let local_uri = format!("local:{}", dir.path().display());

    let mut buf = Vec::new();
    dispatch(
        home,
        Command::Source {
            action: SourceCommand::Add(AddArgs {
                uri: local_uri.clone(),
                name: Some(name.to_string()),
                kind: None,
            }),
        },
        &mut buf,
    )
    .expect("add source");

    let unit_uri = format!("{local_uri}@main/skills/commit");
    (dir, unit_uri)
}

#[test]
fn sync_with_aligned_state_is_noop() {
    let home = tmp_home();
    let (_src, unit_uri) = seed_source(home.path(), "src");
    let base = tempfile::tempdir().unwrap();

    with_all_tool_homes_under(base.path(), || {
        let (_o, res) = run(
            home.path(),
            SkillCommand::Install(InstallArgs {
                uri: unit_uri.clone(),
                targets: Some("claude".into()),
                dry_run: false,
                yes: true,
            }),
        );
        res.expect("install");

        // Declare the same unit in the manifest so it's aligned with
        // what install placed in the lockfile.
        let mut manifest = Manifest::load_from(&manifest_path_in(home.path())).unwrap();
        manifest.units.push(UnitEntry {
            uri: unit_uri.clone(),
            targets: None,
            shadowed_by: None,
        });
        manifest.save_to(&manifest_path_in(home.path())).unwrap();

        let (out, res) = run(
            home.path(),
            SkillCommand::Sync(SyncArgs {
                yes: false,
                dry_run: false,
            }),
        );
        res.expect("sync ok");
        assert!(out.contains("already in sync"), "got: {out}");
    });
}

#[test]
fn sync_installs_manifest_declared_missing_unit() {
    let home = tmp_home();
    let (_src, unit_uri) = seed_source(home.path(), "src-missing");
    let base = tempfile::tempdir().unwrap();

    with_all_tool_homes_under(base.path(), || {
        // Manifest declares the unit; lockfile is empty.
        let mut manifest = Manifest::load_from(&manifest_path_in(home.path())).unwrap();
        manifest.units.push(UnitEntry {
            uri: unit_uri.clone(),
            targets: Some(vec!["claude".into()]),
            shadowed_by: None,
        });
        manifest.save_to(&manifest_path_in(home.path())).unwrap();
        let lock_before = Lockfile::load_from(&lockfile_path_in(home.path())).unwrap();
        assert!(lock_before.units.is_empty());

        let (out, res) = run(
            home.path(),
            SkillCommand::Sync(SyncArgs {
                yes: true,
                dry_run: false,
            }),
        );
        res.expect("sync ok");
        assert!(out.contains("1 installed"), "got: {out}");

        let lock_after = Lockfile::load_from(&lockfile_path_in(home.path())).unwrap();
        assert_eq!(lock_after.units.len(), 1);
        assert_eq!(lock_after.units[0].declared_uri, unit_uri);
    });
}

#[test]
fn sync_removes_orphan_lockfile_unit() {
    let home = tmp_home();
    let (_src, unit_uri) = seed_source(home.path(), "src-orphan");
    let base = tempfile::tempdir().unwrap();

    with_all_tool_homes_under(base.path(), || {
        let (_o, res) = run(
            home.path(),
            SkillCommand::Install(InstallArgs {
                uri: unit_uri.clone(),
                targets: Some("claude".into()),
                dry_run: false,
                yes: true,
            }),
        );
        res.expect("install");

        // Manifest does NOT declare the unit → orphan.
        let manifest = Manifest::load_from(&manifest_path_in(home.path())).unwrap();
        assert!(
            manifest.units.is_empty(),
            "manifest must not list units yet"
        );

        let (out, res) = run(
            home.path(),
            SkillCommand::Sync(SyncArgs {
                yes: true,
                dry_run: false,
            }),
        );
        res.expect("sync ok");
        assert!(out.contains("1 removed"), "got: {out}");

        let lock_after = Lockfile::load_from(&lockfile_path_in(home.path())).unwrap();
        assert!(
            lock_after.units.is_empty(),
            "orphan must be cleared from lockfile: {:?}",
            lock_after.units
        );
        assert!(
            !base.path().join("claude/skills/commit/SKILL.md").exists(),
            "orphan file must be removed from disk"
        );
    });
}

#[test]
fn sync_dry_run_reports_plan_without_mutating() {
    let home = tmp_home();
    let (_src, unit_uri) = seed_source(home.path(), "src-dry");
    let base = tempfile::tempdir().unwrap();

    with_all_tool_homes_under(base.path(), || {
        // Declare in manifest, do not install — sync should plan an
        // install but dry-run must not execute it.
        let mut manifest = Manifest::load_from(&manifest_path_in(home.path())).unwrap();
        manifest.units.push(UnitEntry {
            uri: unit_uri.clone(),
            targets: Some(vec!["claude".into()]),
            shadowed_by: None,
        });
        manifest.save_to(&manifest_path_in(home.path())).unwrap();

        let (out, res) = run(
            home.path(),
            SkillCommand::Sync(SyncArgs {
                yes: false,
                dry_run: true,
            }),
        );
        res.expect("dry-run ok");
        assert!(out.contains("install:  1"), "got: {out}");
        assert!(out.contains("dry-run"), "got: {out}");

        let lock_after = Lockfile::load_from(&lockfile_path_in(home.path())).unwrap();
        assert!(
            lock_after.units.is_empty(),
            "dry-run must not mutate lockfile: {:?}",
            lock_after.units
        );
    });
}

#[test]
fn sync_without_yes_or_dry_run_errors_when_work_pending() {
    let home = tmp_home();
    let (_src, unit_uri) = seed_source(home.path(), "src-interactive");
    let base = tempfile::tempdir().unwrap();

    with_all_tool_homes_under(base.path(), || {
        let mut manifest = Manifest::load_from(&manifest_path_in(home.path())).unwrap();
        manifest.units.push(UnitEntry {
            uri: unit_uri.clone(),
            targets: Some(vec!["claude".into()]),
            shadowed_by: None,
        });
        manifest.save_to(&manifest_path_in(home.path())).unwrap();

        let (_out, res) = run(
            home.path(),
            SkillCommand::Sync(SyncArgs {
                yes: false,
                dry_run: false,
            }),
        );
        let err = res.unwrap_err().to_string();
        assert!(
            err.contains("--yes") || err.contains("dry-run"),
            "got: {err}"
        );
    });
}
