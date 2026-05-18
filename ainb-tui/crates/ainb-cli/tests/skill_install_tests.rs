//! `ainb skill install` integration tests — drives the full
//! parse → resolve → plan → diff → apply → lockfile flow against
//! local-fixture sources so the suite stays offline.

use std::path::{Path, PathBuf};

use ainb_cli::{AddArgs, Command, InstallArgs, SkillCommand, SourceCommand, dispatch};
use ainb_skill_core::lockfile::Lockfile;
use ainb_skill_core::paths::lockfile_path_in;

static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn tmp_home() -> tempfile::TempDir {
    tempfile::Builder::new().prefix("ainb-skill-test-").tempdir().expect("tempdir")
}

fn run(home: &Path, action: SkillCommand) -> (String, anyhow::Result<()>) {
    let mut buf = Vec::new();
    let res = dispatch(home, Command::Skill { action }, &mut buf);
    (String::from_utf8(buf).expect("utf8"), res)
}

/// Build a source fixture with one skill, return (uri, fixture dir,
/// full unit-uri).
fn add_local_skill_source(home: &Path, name: &str) -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    let p: PathBuf = dir.path().join("skills/commit/SKILL.md");
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(
        &p,
        "---\nname: commit\ndescription: well-formed commits\n---\nbody\n",
    )
    .unwrap();
    let local_uri = format!("local:{}", dir.path().display());

    // `source add` runs fetch + adapter detect.
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

/// Every adapter `all_adapters()` registers. Tests that don't pass
/// `--targets` need each of these env-overridden so nothing falls
/// back to the user's real `$AINB_HOME/tools/...`.
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

fn with_tool_homes<R>(
    claude_home: Option<&Path>,
    codex_home: Option<&Path>,
    copilot_home: Option<&Path>,
    body: impl FnOnce() -> R,
) -> R {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    for var in ALL_TOOL_ENV_VARS {
        std::env::remove_var(var);
    }
    if let Some(p) = claude_home {
        std::env::set_var("AINB_TOOL_HOME_CLAUDE", p);
    }
    if let Some(p) = codex_home {
        std::env::set_var("AINB_TOOL_HOME_CODEX", p);
    }
    if let Some(p) = copilot_home {
        std::env::set_var("AINB_TOOL_HOME_COPILOT", p);
    }
    let r = body();
    for var in ALL_TOOL_ENV_VARS {
        std::env::remove_var(var);
    }
    r
}

/// Set every tool's install root to its own tempdir under `base`.
/// Returns the per-tool path map so callers can assert against the
/// resulting layout.
fn with_all_tool_homes_under<R>(
    base: &Path,
    body: impl FnOnce(&std::collections::BTreeMap<&'static str, PathBuf>) -> R,
) -> R {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut paths = std::collections::BTreeMap::new();
    for var in ALL_TOOL_ENV_VARS {
        let tool = var.trim_start_matches("AINB_TOOL_HOME_").to_lowercase();
        let dir = base.join(&tool);
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var(var, &dir);
        paths.insert(stable_tool_name(&tool), dir);
    }
    let r = body(&paths);
    for var in ALL_TOOL_ENV_VARS {
        std::env::remove_var(var);
    }
    r
}

/// Map the lowercased env-var stem back to the stable adapter name.
/// Most adapters round-trip directly (`claude` ↔ `CLAUDE`) but
/// `claude-desktop` arrives here as `claude_desktop` because the
/// env-var-name transform maps hyphens to underscores.
fn stable_tool_name(env_lowercase: &str) -> &'static str {
    match env_lowercase {
        "claude" => "claude",
        "codex" => "codex",
        "copilot" => "copilot",
        "gemini" => "gemini",
        "cursor" => "cursor",
        "amazonq" => "amazonq",
        "claude_desktop" => "claude-desktop",
        "cline" => "cline",
        "roo" => "roo",
        other => panic!("unknown tool env stem: {other}"),
    }
}

#[test]
fn install_to_all_v1_tools() {
    // Default install hits every adapter in all_adapters(). For a
    // generic skill, every v1 tool that accepts UnitKind::Skill (= 8
    // of 9; claude-desktop declines) gets a Deployed entry. The one
    // tool that doesn't accept skills gets a Skipped entry.
    let home = tmp_home();
    let (_src, unit_uri) = add_local_skill_source(home.path(), "fixture");

    let base = tempfile::tempdir().unwrap();
    with_all_tool_homes_under(base.path(), |tool_paths| {
        let (out, res) = run(
            home.path(),
            SkillCommand::Install(InstallArgs {
                uri: unit_uri.clone(),
                targets: None,
                dry_run: false,
                yes: true,
            }),
        );
        res.expect("install ok");
        assert!(
            out.contains("installed") && out.contains("8 tool(s)"),
            "got: {out}"
        );
        assert!(out.contains("1 skipped"), "got: {out}");

        // Every accepting tool got the SKILL.md.
        for (tool, dir) in tool_paths {
            let path = dir.join("skills/commit/SKILL.md");
            if *tool == "claude-desktop" {
                assert!(
                    !path.exists(),
                    "claude-desktop declines skills — must not deploy at {path:?}"
                );
            } else {
                assert!(path.exists(), "missing under: {tool} → {path:?}");
            }
        }

        // Lockfile has one unit; 8 deployed + 1 skipped entries.
        let lock = Lockfile::load_from(&lockfile_path_in(home.path())).unwrap();
        assert_eq!(lock.units.len(), 1);
        assert_eq!(lock.units[0].deployed.len(), 9);
        assert!(matches!(
            lock.units[0].deployed.get("claude-desktop").unwrap(),
            ainb_skill_core::DeployedRef::Skipped { .. }
        ));
    });
}

#[test]
fn install_targets_flag_filters_tools() {
    let home = tmp_home();
    let (_src, unit_uri) = add_local_skill_source(home.path(), "fix");

    let claude_dst = tempfile::tempdir().unwrap();
    let codex_dst = tempfile::tempdir().unwrap();

    with_tool_homes(
        Some(claude_dst.path()),
        Some(codex_dst.path()),
        None,
        || {
            let (out, res) = run(
                home.path(),
                SkillCommand::Install(InstallArgs {
                    uri: unit_uri.clone(),
                    targets: Some("claude".into()),
                    dry_run: false,
                    yes: true,
                }),
            );
            res.expect("install ok");
            assert!(out.contains("1 tool(s)"), "got: {out}");
            assert!(claude_dst.path().join("skills/commit/SKILL.md").exists());
            assert!(
                !codex_dst.path().join("skills/commit/SKILL.md").exists(),
                "codex shouldn't have been touched"
            );
        },
    );
}

#[test]
fn install_dry_run_does_not_mutate() {
    let home = tmp_home();
    let (_src, unit_uri) = add_local_skill_source(home.path(), "drytest");
    let claude_dst = tempfile::tempdir().unwrap();

    with_tool_homes(Some(claude_dst.path()), None, None, || {
        let (out, res) = run(
            home.path(),
            SkillCommand::Install(InstallArgs {
                uri: unit_uri.clone(),
                targets: Some("claude".into()),
                dry_run: true,
                yes: false,
            }),
        );
        res.expect("dry run ok");
        assert!(out.contains("+++"), "expected diff lines, got: {out}");
        assert!(out.contains("dry-run"), "got: {out}");
        assert!(!claude_dst.path().join("skills/commit/SKILL.md").exists());

        let lock = Lockfile::load_from(&lockfile_path_in(home.path())).unwrap();
        assert!(lock.units.is_empty(), "dry-run must not mutate lockfile");
    });
}

#[test]
fn install_without_yes_or_dry_run_errors() {
    let home = tmp_home();
    let (_src, unit_uri) = add_local_skill_source(home.path(), "interactive");
    let claude_dst = tempfile::tempdir().unwrap();

    with_tool_homes(Some(claude_dst.path()), None, None, || {
        let (_out, res) = run(
            home.path(),
            SkillCommand::Install(InstallArgs {
                uri: unit_uri.clone(),
                targets: Some("claude".into()),
                dry_run: false,
                yes: false,
            }),
        );
        let err = res.unwrap_err().to_string();
        assert!(
            err.contains("--yes") || err.contains("dry-run"),
            "got: {err}"
        );
    });
}

#[test]
fn install_rejects_uri_without_path() {
    let home = tmp_home();
    let (_, res) = run(
        home.path(),
        SkillCommand::Install(InstallArgs {
            uri: "gh:org/repo@main".into(),
            targets: None,
            dry_run: false,
            yes: true,
        }),
    );
    let err = res.unwrap_err().to_string();
    assert!(err.contains("unit URI"), "got: {err}");
}

#[test]
fn install_errors_when_no_matching_source() {
    let home = tmp_home();
    let (_, res) = run(
        home.path(),
        SkillCommand::Install(InstallArgs {
            uri: "gh:nobody/nothing@main/skills/foo".into(),
            targets: None,
            dry_run: false,
            yes: true,
        }),
    );
    let err = res.unwrap_err().to_string();
    assert!(err.contains("no enabled source"), "got: {err}");
}

#[test]
fn install_skips_unsupported_kinds_per_matrix() {
    // A plugin can't be installed to copilot. Use a fixture with a
    // plugin unit; targeting copilot should report it as skipped
    // but still succeed (zero applied, one skipped — or three since
    // we hit all adapters by default; copilot will skip while
    // claude accepts).
    let home = tmp_home();
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("plugins/reflect")).unwrap();
    std::fs::write(
        dir.path().join("plugins/reflect/plugin.json"),
        r#"{"name":"reflect"}"#,
    )
    .unwrap();
    let local_uri = format!("local:{}", dir.path().display());

    let mut buf = Vec::new();
    dispatch(
        home.path(),
        Command::Source {
            action: SourceCommand::Add(AddArgs {
                uri: local_uri.clone(),
                name: Some("p".into()),
                kind: None,
            }),
        },
        &mut buf,
    )
    .unwrap();

    let unit_uri = format!("{local_uri}@main/plugins/reflect");
    let claude_dst = tempfile::tempdir().unwrap();
    let copilot_dst = tempfile::tempdir().unwrap();

    with_tool_homes(
        Some(claude_dst.path()),
        None,
        Some(copilot_dst.path()),
        || {
            let (out, res) = run(
                home.path(),
                SkillCommand::Install(InstallArgs {
                    uri: unit_uri.clone(),
                    targets: Some("claude,copilot".into()),
                    dry_run: false,
                    yes: true,
                }),
            );
            res.expect("install ok");
            assert!(out.contains("skipped copilot"), "got: {out}");
            assert!(claude_dst.path().join("plugins/reflect/plugin.json").exists());
            assert!(
                !copilot_dst.path().join("plugins/reflect/plugin.json").exists(),
                "copilot must not receive plugin"
            );
            let lock = Lockfile::load_from(&lockfile_path_in(home.path())).unwrap();
            let entry = &lock.units[0];
            assert!(matches!(
                entry.deployed.get("copilot").unwrap(),
                ainb_skill_core::DeployedRef::Skipped { .. }
            ));
            assert!(matches!(
                entry.deployed.get("claude").unwrap(),
                ainb_skill_core::DeployedRef::Deployed { .. }
            ));
        },
    );
}
