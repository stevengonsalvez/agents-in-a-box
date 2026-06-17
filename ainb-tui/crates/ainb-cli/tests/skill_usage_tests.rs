//! `ainb skill usage` integration tests (bead v12.B.3).
//!
//! Install a unit into a sandboxed tool home (via `AINB_TOOL_HOME_<TOOL>`),
//! seed fake session-log JSONL under that home, then run `skill usage`
//! and assert the lockfile's per-unit `usage` record is refreshed. No
//! test touches a real `~/.claude` — every tool home is a tempdir bound
//! through the env override.

use std::path::{Path, PathBuf};

use ainb_cli::{AddArgs, Command, InstallArgs, SkillCommand, SourceCommand, UsageArgs, dispatch};
use ainb_skill_core::lockfile::{LockedUnit, Lockfile};
use ainb_skill_core::paths::lockfile_path_in;

static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn tmp_home() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("ainb-skill-usage-test-")
        .tempdir()
        .expect("tempdir")
}

fn run(home: &Path, action: SkillCommand) -> (String, anyhow::Result<()>) {
    let mut buf = Vec::new();
    let res = dispatch(home, Command::Skill { action }, &mut buf);
    (String::from_utf8(buf).expect("utf8"), res)
}

/// Create a `local:` source exposing `skills/<skill>/SKILL.md` and
/// return (tempdir keepalive, unit URI).
fn add_skill_source(home: &Path, name: &str, skill: &str) -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    let p: PathBuf = dir.path().join(format!("skills/{skill}/SKILL.md"));
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, format!("---\nname: {skill}\n---\nbody\n")).unwrap();
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
    (dir, format!("{local_uri}@main/skills/{skill}"))
}

fn with_tool_homes<R>(claude: &Path, codex: Option<&Path>, body: impl FnOnce() -> R) -> R {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    std::env::set_var("AINB_TOOL_HOME_CLAUDE", claude);
    if let Some(p) = codex {
        std::env::set_var("AINB_TOOL_HOME_CODEX", p);
    } else {
        std::env::remove_var("AINB_TOOL_HOME_CODEX");
    }
    std::env::remove_var("AINB_TOOL_HOME_COPILOT");
    let r = body();
    std::env::remove_var("AINB_TOOL_HOME_CLAUDE");
    std::env::remove_var("AINB_TOOL_HOME_CODEX");
    r
}

fn install_then(home: &Path, unit_uri: &str, targets: Option<&str>) {
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

/// Seed a session-log JSONL file under a tool home.
fn seed_log(tool_home: &Path, rel: &str, contents: &str) {
    let path = tool_home.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
}

#[test]
fn usage_all_refreshes_units_and_writes_lockfile() {
    let home = tmp_home();
    let (_src, unit_uri) = add_skill_source(home.path(), "u1", "commit");
    let claude = tempfile::tempdir().unwrap();

    with_tool_homes(claude.path(), None, || {
        install_then(home.path(), &unit_uri, Some("claude"));
        // Two command-name invocations + one Skill tool_use = 3 total.
        seed_log(
            claude.path(),
            "projects/p/2026-05.jsonl",
            "{\"timestamp\":\"2026-05-01T10:00:00Z\",\"content\":\"<command-name>commit</command-name>\"}\n\
             {\"timestamp\":\"2026-05-02T10:00:00Z\",\"content\":\"<command-name>commit</command-name>\"}\n\
             {\"type\":\"tool_use\",\"name\":\"Skill\",\"input\":{\"skill\":\"commit\"},\"timestamp\":\"2026-05-03T09:00:00Z\"}\n",
        );

        let (out, res) = run(
            home.path(),
            SkillCommand::Usage(UsageArgs {
                unit_name: None,
                verbose: true,
            }),
        );
        res.expect("usage ok");
        assert!(
            out.contains("commit"),
            "verbose output names the unit: {out}"
        );
        assert!(out.contains('3'), "verbose output shows the count: {out}");

        let lock = Lockfile::load_from(&lockfile_path_in(home.path())).unwrap();
        assert_eq!(lock.units.len(), 1);
        assert_eq!(lock.units[0].usage.invocations, 3);
        assert_eq!(
            lock.units[0].usage.last_used_at.as_deref(),
            Some("2026-05-03T09:00:00Z")
        );
    });
}

#[test]
fn usage_single_unit_by_name() {
    let home = tmp_home();
    let (_src, unit_uri) = add_skill_source(home.path(), "u1", "commit");
    let claude = tempfile::tempdir().unwrap();

    with_tool_homes(claude.path(), None, || {
        install_then(home.path(), &unit_uri, Some("claude"));
        seed_log(
            claude.path(),
            "projects/p/log.jsonl",
            "{\"timestamp\":\"2026-05-01T10:00:00Z\",\"content\":\"<command-name>commit</command-name>\"}\n",
        );

        let (_out, res) = run(
            home.path(),
            SkillCommand::Usage(UsageArgs {
                unit_name: Some("commit".into()),
                verbose: false,
            }),
        );
        res.expect("usage ok");

        let lock = Lockfile::load_from(&lockfile_path_in(home.path())).unwrap();
        assert_eq!(lock.units[0].usage.invocations, 1);
    });
}

#[test]
fn usage_unknown_unit_errors() {
    let home = tmp_home();
    let (_src, unit_uri) = add_skill_source(home.path(), "u1", "commit");
    let claude = tempfile::tempdir().unwrap();

    with_tool_homes(claude.path(), None, || {
        install_then(home.path(), &unit_uri, Some("claude"));
        let (_out, res) = run(
            home.path(),
            SkillCommand::Usage(UsageArgs {
                unit_name: Some("does-not-exist".into()),
                verbose: false,
            }),
        );
        let err = res.unwrap_err().to_string();
        assert!(err.contains("does-not-exist"), "got: {err}");
    });
}

#[test]
fn usage_is_idempotent() {
    let home = tmp_home();
    let (_src, unit_uri) = add_skill_source(home.path(), "u1", "commit");
    let claude = tempfile::tempdir().unwrap();

    with_tool_homes(claude.path(), None, || {
        install_then(home.path(), &unit_uri, Some("claude"));
        seed_log(
            claude.path(),
            "projects/p/log.jsonl",
            "{\"timestamp\":\"2026-05-01T10:00:00Z\",\"content\":\"<command-name>commit</command-name>\"}\n",
        );

        run(
            home.path(),
            SkillCommand::Usage(UsageArgs {
                unit_name: None,
                verbose: false,
            }),
        )
        .1
        .expect("run 1");
        let after_first = Lockfile::load_from(&lockfile_path_in(home.path())).unwrap();

        run(
            home.path(),
            SkillCommand::Usage(UsageArgs {
                unit_name: None,
                verbose: false,
            }),
        )
        .1
        .expect("run 2");
        let after_second = Lockfile::load_from(&lockfile_path_in(home.path())).unwrap();

        assert_eq!(
            after_first, after_second,
            "second run must produce an identical lockfile"
        );
    });
}

#[test]
fn usage_skips_path_less_unit_without_misattributing() {
    // A unit URI with no trailing path (no derivable short name) must be
    // skipped, not queried with the whole URI as a bogus name (which
    // would silently record 0 and read as "scanned, never used").
    let home = tmp_home();
    let lock_path = lockfile_path_in(home.path());
    let lf = Lockfile {
        units: vec![LockedUnit {
            uri: "gh:org/repo@main".into(),
            declared_uri: "gh:org/repo@main".into(), // no `/path` segment
            kind: "skill".into(),
            sha: None,
            deployed: Default::default(),
            usage: Default::default(),
        }],
        ..Lockfile::default()
    };
    lf.save_to(&lock_path).unwrap();

    let (out, res) = run(
        home.path(),
        SkillCommand::Usage(UsageArgs {
            unit_name: None,
            verbose: true,
        }),
    );
    res.expect("usage ok");
    assert!(out.contains("skip"), "verbose should note the skip: {out}");
    assert!(out.contains("0 unit(s)"), "nothing refreshed: {out}");

    let lock = Lockfile::load_from(&lock_path).unwrap();
    assert!(lock.units[0].usage.is_empty(), "usage left untouched/empty");
}

#[test]
fn usage_aggregates_across_tools() {
    let home = tmp_home();
    let (_src, unit_uri) = add_skill_source(home.path(), "u1", "commit");
    let claude = tempfile::tempdir().unwrap();
    let codex = tempfile::tempdir().unwrap();

    with_tool_homes(claude.path(), Some(codex.path()), || {
        install_then(home.path(), &unit_uri, Some("claude,codex"));
        // 2 invocations on claude, 1 (later) on codex → 3 total, codex ts wins.
        seed_log(
            claude.path(),
            "projects/p/log.jsonl",
            "{\"timestamp\":\"2026-05-01T10:00:00Z\",\"content\":\"<command-name>commit</command-name>\"}\n\
             {\"timestamp\":\"2026-05-02T10:00:00Z\",\"content\":\"<command-name>commit</command-name>\"}\n",
        );
        seed_log(
            codex.path(),
            "sessions/s/log.jsonl",
            "{\"type\":\"tool_use\",\"name\":\"Skill\",\"input\":{\"skill\":\"commit\"},\"timestamp\":\"2026-06-01T00:00:00Z\"}\n",
        );

        run(
            home.path(),
            SkillCommand::Usage(UsageArgs {
                unit_name: None,
                verbose: false,
            }),
        )
        .1
        .expect("usage ok");

        let lock = Lockfile::load_from(&lockfile_path_in(home.path())).unwrap();
        assert_eq!(
            lock.units[0].usage.invocations, 3,
            "summed across claude + codex"
        );
        assert_eq!(
            lock.units[0].usage.last_used_at.as_deref(),
            Some("2026-06-01T00:00:00Z")
        );
    });
}
