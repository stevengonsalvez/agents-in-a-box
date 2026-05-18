//! `ainb skill update` integration tests — drives re-fetch, drift
//! detection, and apply pipelines against local-fixture sources so
//! the suite stays offline.

use std::path::{Path, PathBuf};

use ainb_cli::{AddArgs, Command, InstallArgs, SkillCommand, SourceCommand, UpdateArgs, dispatch};
use ainb_skill_core::DeployedRef;
use ainb_skill_core::lockfile::Lockfile;
use ainb_skill_core::paths::lockfile_path_in;

static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn tmp_home() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("ainb-skill-update-")
        .tempdir()
        .expect("tempdir")
}

fn run(home: &Path, action: SkillCommand) -> (String, anyhow::Result<()>) {
    let mut buf = Vec::new();
    let res = dispatch(home, Command::Skill { action }, &mut buf);
    (String::from_utf8(buf).expect("utf8"), res)
}

fn with_claude_only<R>(claude_home: &Path, body: impl FnOnce() -> R) -> R {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // Sandbox every adapter under a private tempdir so an
    // unconfigured tool doesn't leak into the real $AINB_HOME.
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
    let scratch = tempfile::tempdir().unwrap();
    for var in VARS {
        std::env::set_var(var, scratch.path().join(var));
    }
    // Override claude with the caller-controlled tempdir.
    std::env::set_var("AINB_TOOL_HOME_CLAUDE", claude_home);
    let r = body();
    for var in VARS {
        std::env::remove_var(var);
    }
    r
}

/// Build a source fixture with one skill, add it to the manifest,
/// return the source directory + the unit URI.
fn seed_local_source(home: &Path, name: &str, body: &str) -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    let p: PathBuf = dir.path().join("skills/commit/SKILL.md");
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(
        &p,
        format!("---\nname: commit\ndescription: well-formed commits\n---\n{body}\n"),
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

/// Mutate the SKILL.md body and force the source root's mtime to
/// move forward so the local fetcher's pseudo-SHA (which hashes
/// `path + root-dir mtime`) diverges on the next fetch.
fn bump_skill_body(source: &tempfile::TempDir, new_body: &str) {
    let p = source.path().join("skills/commit/SKILL.md");
    std::fs::write(
        &p,
        format!("---\nname: commit\ndescription: well-formed commits\n---\n{new_body}\n"),
    )
    .unwrap();
    // Create + remove a marker file at the root to bump the source
    // directory's mtime. Writing a child file does not change the
    // parent dir's mtime on most filesystems, but creating /
    // removing an entry in it does.
    let marker = source.path().join(".ainb-mtime-bump");
    std::fs::write(&marker, b"x").unwrap();
    std::fs::remove_file(&marker).unwrap();
}

#[test]
fn update_check_reports_no_drift_after_fresh_install() {
    let home = tmp_home();
    let (_src, unit_uri) = seed_local_source(home.path(), "src1", "body-v1");
    let claude_dst = tempfile::tempdir().unwrap();

    with_claude_only(claude_dst.path(), || {
        let (_o, res) = run(
            home.path(),
            SkillCommand::Install(InstallArgs {
                uri: unit_uri.clone(),
                targets: Some("claude".into()),
                dry_run: false,
                yes: true,
            }),
        );
        res.expect("install ok");

        let (out, res) = run(
            home.path(),
            SkillCommand::Update(UpdateArgs {
                uri: None,
                all: false,
                check: true,
                yes: false,
                dry_run: false,
            }),
        );
        res.expect("update --check ok");
        assert!(out.contains("up to date"), "got: {out}");
        assert!(out.contains("0 drift"), "got: {out}");
    });
}

#[test]
fn update_check_reports_drift_when_source_mutates() {
    let home = tmp_home();
    let (src, unit_uri) = seed_local_source(home.path(), "src2", "body-v1");
    let claude_dst = tempfile::tempdir().unwrap();

    with_claude_only(claude_dst.path(), || {
        let (_o, res) = run(
            home.path(),
            SkillCommand::Install(InstallArgs {
                uri: unit_uri.clone(),
                targets: Some("claude".into()),
                dry_run: false,
                yes: true,
            }),
        );
        res.expect("install ok");

        bump_skill_body(&src, "body-v2-after-edit");

        let (out, res) = run(
            home.path(),
            SkillCommand::Update(UpdateArgs {
                uri: None,
                all: false,
                check: true,
                yes: false,
                dry_run: false,
            }),
        );
        res.expect("update --check ok");
        assert!(out.contains("drift"), "expected drift report, got: {out}");
        assert!(out.contains("1 drift"), "got: {out}");
    });
}

#[test]
fn update_applies_new_content_and_bumps_lockfile_sha() {
    let home = tmp_home();
    let (src, unit_uri) = seed_local_source(home.path(), "src3", "body-v1");
    let claude_dst = tempfile::tempdir().unwrap();

    with_claude_only(claude_dst.path(), || {
        let (_o, res) = run(
            home.path(),
            SkillCommand::Install(InstallArgs {
                uri: unit_uri.clone(),
                targets: Some("claude".into()),
                dry_run: false,
                yes: true,
            }),
        );
        res.expect("install ok");

        let lock_before = Lockfile::load_from(&lockfile_path_in(home.path())).unwrap();
        let sha_before = lock_before.units[0].sha.clone();
        assert!(sha_before.is_some(), "install must record a SHA");

        bump_skill_body(&src, "body-v2-after-edit");

        let (out, res) = run(
            home.path(),
            SkillCommand::Update(UpdateArgs {
                uri: Some(unit_uri.clone()),
                all: false,
                check: false,
                yes: true,
                dry_run: false,
            }),
        );
        res.expect("update apply ok");
        assert!(out.contains("updated 1 unit"), "got: {out}");

        // File on disk now carries the new body.
        let installed =
            std::fs::read_to_string(claude_dst.path().join("skills/commit/SKILL.md")).unwrap();
        assert!(
            installed.contains("body-v2-after-edit"),
            "deployed file didn't refresh: {installed}"
        );

        let lock_after = Lockfile::load_from(&lockfile_path_in(home.path())).unwrap();
        assert_ne!(
            lock_after.units[0].sha, sha_before,
            "lockfile SHA must advance after update"
        );
    });
}

#[test]
fn update_all_skips_unchanged_units() {
    let home = tmp_home();
    let (src1, unit_a) = seed_local_source(home.path(), "src-a", "alpha");
    let (_src2, unit_b) = seed_local_source(home.path(), "src-b", "beta");
    let claude_dst = tempfile::tempdir().unwrap();

    with_claude_only(claude_dst.path(), || {
        for u in [&unit_a, &unit_b] {
            let (_, r) = run(
                home.path(),
                SkillCommand::Install(InstallArgs {
                    uri: u.clone(),
                    targets: Some("claude".into()),
                    dry_run: false,
                    yes: true,
                }),
            );
            r.expect("install");
        }

        // Mutate only src-a.
        bump_skill_body(&src1, "alpha-2");

        let (out, res) = run(
            home.path(),
            SkillCommand::Update(UpdateArgs {
                uri: None,
                all: true,
                check: false,
                yes: true,
                dry_run: false,
            }),
        );
        res.expect("update --all ok");
        assert!(out.contains("updated 1 unit"), "got: {out}");
        assert!(
            out.contains(&unit_b) && out.contains("up to date"),
            "src-b should be reported as up to date, got: {out}"
        );
    });
}

#[test]
fn update_without_uri_or_all_errors() {
    let home = tmp_home();
    let (_src, _unit_uri) = seed_local_source(home.path(), "src-err", "x");
    let claude_dst = tempfile::tempdir().unwrap();

    with_claude_only(claude_dst.path(), || {
        let (_out, res) = run(
            home.path(),
            SkillCommand::Update(UpdateArgs {
                uri: None,
                all: false,
                check: false,
                yes: false,
                dry_run: false,
            }),
        );
        let err = res.unwrap_err().to_string();
        assert!(
            err.contains("specify a unit URI") || err.contains("--all"),
            "got: {err}"
        );
    });
}

#[test]
fn update_dry_run_does_not_mutate_lockfile() {
    let home = tmp_home();
    let (src, unit_uri) = seed_local_source(home.path(), "src-dry", "body-v1");
    let claude_dst = tempfile::tempdir().unwrap();

    with_claude_only(claude_dst.path(), || {
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

        let lock_before = Lockfile::load_from(&lockfile_path_in(home.path())).unwrap();
        bump_skill_body(&src, "body-v2");

        let (out, res) = run(
            home.path(),
            SkillCommand::Update(UpdateArgs {
                uri: Some(unit_uri.clone()),
                all: false,
                check: false,
                yes: false,
                dry_run: true,
            }),
        );
        res.expect("dry-run ok");
        assert!(out.contains("dry-run"), "got: {out}");

        let lock_after = Lockfile::load_from(&lockfile_path_in(home.path())).unwrap();
        assert_eq!(
            lock_after.units[0].sha, lock_before.units[0].sha,
            "dry-run must not bump SHA"
        );

        let installed = match lock_after.units[0].deployed.get("claude") {
            Some(DeployedRef::Deployed { file_hashes, .. }) => file_hashes.clone(),
            other => panic!("expected Deployed, got: {other:?}"),
        };
        assert_eq!(
            installed,
            match lock_before.units[0].deployed.get("claude") {
                Some(DeployedRef::Deployed { file_hashes, .. }) => file_hashes.clone(),
                other => panic!("expected Deployed before, got: {other:?}"),
            },
            "dry-run must not rewrite file hashes"
        );
    });
}
