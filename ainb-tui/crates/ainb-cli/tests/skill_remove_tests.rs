//! `ainb skill remove` integration tests — install then remove and
//! assert files are gone and lockfile is updated.

use std::path::{Path, PathBuf};

use ainb_cli::{
    dispatch, AddArgs, Command, InstallArgs, RemoveSkillArgs, SkillCommand, SourceCommand,
};
use ainb_skill_core::lockfile::Lockfile;
use ainb_skill_core::paths::lockfile_path_in;

static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn tmp_home() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("ainb-skill-remove-test-")
        .tempdir()
        .expect("tempdir")
}

fn run(home: &Path, action: SkillCommand) -> (String, anyhow::Result<()>) {
    let mut buf = Vec::new();
    let res = dispatch(home, Command::Skill { action }, &mut buf);
    (String::from_utf8(buf).expect("utf8"), res)
}

fn add_skill_source(home: &Path, name: &str) -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    let p: PathBuf = dir.path().join("skills/commit/SKILL.md");
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, "---\nname: commit\n---\nbody\n").unwrap();
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
    .unwrap();
    (dir, format!("{local_uri}@main/skills/commit"))
}

fn with_tool_homes<R>(
    claude_home: &Path,
    codex_home: Option<&Path>,
    body: impl FnOnce() -> R,
) -> R {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    std::env::set_var("AINB_TOOL_HOME_CLAUDE", claude_home);
    if let Some(p) = codex_home {
        std::env::set_var("AINB_TOOL_HOME_CODEX", p);
    } else {
        std::env::remove_var("AINB_TOOL_HOME_CODEX");
    }
    // Disable copilot so all_adapters() only hits claude (+codex).
    std::env::remove_var("AINB_TOOL_HOME_COPILOT");
    let r = body();
    std::env::remove_var("AINB_TOOL_HOME_CLAUDE");
    std::env::remove_var("AINB_TOOL_HOME_CODEX");
    r
}

fn install_then(
    home: &Path,
    unit_uri: &str,
    targets: Option<&str>,
) {
    let mut buf = Vec::new();
    dispatch(
        home,
        Command::Skill {
            action: SkillCommand::Install(InstallArgs {
                uri: unit_uri.to_string(),
                targets: targets.map(str::to_string),
                dry_run: false,
                yes: true,
            }),
        },
        &mut buf,
    )
    .expect("install");
}

#[test]
fn remove_deletes_files_and_drops_unit() {
    let home = tmp_home();
    let (_src, unit_uri) = add_skill_source(home.path(), "rm");
    let claude_dst = tempfile::tempdir().unwrap();

    with_tool_homes(claude_dst.path(), None, || {
        install_then(home.path(), &unit_uri, Some("claude"));
        assert!(claude_dst.path().join("skills/commit/SKILL.md").exists());

        let (out, res) = run(
            home.path(),
            SkillCommand::Remove(RemoveSkillArgs {
                uri: unit_uri.clone(),
                targets: None,
                dry_run: false,
                yes: true,
            }),
        );
        res.expect("remove ok");
        assert!(out.contains("removed"), "got: {out}");
        assert!(!claude_dst.path().join("skills/commit/SKILL.md").exists());

        // Empty parent dir pruned.
        assert!(!claude_dst.path().join("skills/commit").exists());

        // LockedUnit gone.
        let lock = Lockfile::load_from(&lockfile_path_in(home.path())).unwrap();
        assert!(lock.units.is_empty());
    });
}

#[test]
fn remove_targets_flag_preserves_other_tools() {
    let home = tmp_home();
    let (_src, unit_uri) = add_skill_source(home.path(), "rm2");
    let claude_dst = tempfile::tempdir().unwrap();
    let codex_dst = tempfile::tempdir().unwrap();

    with_tool_homes(claude_dst.path(), Some(codex_dst.path()), || {
        install_then(home.path(), &unit_uri, Some("claude,codex"));
        assert!(claude_dst.path().join("skills/commit/SKILL.md").exists());
        assert!(codex_dst.path().join("skills/commit/SKILL.md").exists());

        // Remove only from claude.
        let (out, res) = run(
            home.path(),
            SkillCommand::Remove(RemoveSkillArgs {
                uri: unit_uri.clone(),
                targets: Some("claude".into()),
                dry_run: false,
                yes: true,
            }),
        );
        res.expect("remove ok");
        assert!(out.contains("1 tool(s)"), "got: {out}");

        assert!(!claude_dst.path().join("skills/commit/SKILL.md").exists());
        assert!(
            codex_dst.path().join("skills/commit/SKILL.md").exists(),
            "codex deployment should survive partial remove"
        );

        // Lockfile keeps the unit but only the codex deployed entry.
        let lock = Lockfile::load_from(&lockfile_path_in(home.path())).unwrap();
        assert_eq!(lock.units.len(), 1);
        let unit = &lock.units[0];
        assert!(unit.deployed.contains_key("codex"));
        assert!(!unit.deployed.contains_key("claude"));
    });
}

#[test]
fn remove_dry_run_does_not_mutate() {
    let home = tmp_home();
    let (_src, unit_uri) = add_skill_source(home.path(), "dry");
    let claude_dst = tempfile::tempdir().unwrap();

    with_tool_homes(claude_dst.path(), None, || {
        install_then(home.path(), &unit_uri, Some("claude"));
        let (out, res) = run(
            home.path(),
            SkillCommand::Remove(RemoveSkillArgs {
                uri: unit_uri.clone(),
                targets: None,
                dry_run: true,
                yes: false,
            }),
        );
        res.expect("dry-run ok");
        assert!(out.contains("dry-run"), "got: {out}");
        // File still there.
        assert!(claude_dst.path().join("skills/commit/SKILL.md").exists());
        // Lockfile unchanged.
        let lock = Lockfile::load_from(&lockfile_path_in(home.path())).unwrap();
        assert_eq!(lock.units.len(), 1);
    });
}

#[test]
fn remove_errors_when_unit_not_installed() {
    let home = tmp_home();
    let (_, res) = run(
        home.path(),
        SkillCommand::Remove(RemoveSkillArgs {
            uri: "gh:nobody/nothing@main/skills/foo".into(),
            targets: None,
            dry_run: false,
            yes: true,
        }),
    );
    let err = res.unwrap_err().to_string();
    assert!(err.contains("not in the lockfile"), "got: {err}");
}

#[test]
fn remove_without_yes_or_dry_run_errors() {
    let home = tmp_home();
    let (_src, unit_uri) = add_skill_source(home.path(), "noprompt");
    let claude_dst = tempfile::tempdir().unwrap();

    with_tool_homes(claude_dst.path(), None, || {
        install_then(home.path(), &unit_uri, Some("claude"));
        let (_out, res) = run(
            home.path(),
            SkillCommand::Remove(RemoveSkillArgs {
                uri: unit_uri.clone(),
                targets: None,
                dry_run: false,
                yes: false,
            }),
        );
        let err = res.unwrap_err().to_string();
        assert!(err.contains("--yes") || err.contains("dry-run"), "got: {err}");
    });
}
