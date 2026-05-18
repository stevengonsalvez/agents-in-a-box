//! `ainb doctor` integration tests — drives the health-check suite
//! over local-fixture state so every check fires offline.

use std::path::Path;

use ainb_cli::{AddArgs, Command, DoctorArgs, InstallArgs, SkillCommand, SourceCommand, dispatch};

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
    tempfile::Builder::new().prefix("ainb-doctor-").tempdir().expect("tempdir")
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

fn run_doctor(home: &Path, args: DoctorArgs) -> (String, anyhow::Result<()>) {
    let mut buf = Vec::new();
    let res = dispatch(home, Command::Doctor(args), &mut buf);
    (String::from_utf8(buf).expect("utf8"), res)
}

fn seed_install(home: &Path) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("skills/commit")).unwrap();
    std::fs::write(
        dir.path().join("skills/commit/SKILL.md"),
        "---\nname: commit\n---\n",
    )
    .unwrap();
    let local_uri = format!("local:{}", dir.path().display());

    let mut buf = Vec::new();
    dispatch(
        home,
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
        home,
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
    dir
}

#[test]
fn doctor_empty_home_passes() {
    let home = tmp_home();
    let base = tempfile::tempdir().unwrap();
    // Sandbox every adapter so we don't drift-detect against the
    // developer's real ~/.agents-in-a-box/tools/ directory (which
    // would carry prior installations).
    with_tool_homes(base.path(), || {
        let (out, res) = run_doctor(home.path(), DoctorArgs { offline: true });
        res.expect("doctor ok");
        assert!(out.contains("manifest"), "got: {out}");
        assert!(out.contains("lockfile"), "got: {out}");
        assert!(out.contains("all checks passed"), "got: {out}");
    });
}

#[test]
fn doctor_after_clean_install_passes() {
    let home = tmp_home();
    let base = tempfile::tempdir().unwrap();
    with_tool_homes(base.path(), || {
        let _src = seed_install(home.path());
        let (out, res) = run_doctor(home.path(), DoctorArgs { offline: true });
        res.expect("doctor ok");
        assert!(out.contains("all checks passed"), "got: {out}");
        assert!(out.contains("OK"), "got: {out}");
    });
}

#[test]
fn doctor_detects_missing_deployed_file() {
    let home = tmp_home();
    let base = tempfile::tempdir().unwrap();
    with_tool_homes(base.path(), || {
        let _src = seed_install(home.path());
        // Delete the deployed file underneath.
        std::fs::remove_file(base.path().join("claude/skills/commit/SKILL.md")).unwrap();

        let (out, res) = run_doctor(home.path(), DoctorArgs { offline: true });
        let err = res.unwrap_err().to_string();
        assert!(err.contains("issue"), "expected doctor failure, got: {err}");
        assert!(out.contains("missing on disk"), "got: {out}");
    });
}

#[test]
fn doctor_detects_file_hash_mismatch() {
    let home = tmp_home();
    let base = tempfile::tempdir().unwrap();
    with_tool_homes(base.path(), || {
        let _src = seed_install(home.path());
        // Corrupt the deployed file content.
        std::fs::write(
            base.path().join("claude/skills/commit/SKILL.md"),
            "tampered",
        )
        .unwrap();

        let (out, res) = run_doctor(home.path(), DoctorArgs { offline: true });
        let err = res.unwrap_err().to_string();
        assert!(err.contains("issue"), "got: {err}");
        assert!(out.contains("hash mismatch"), "got: {out}");
    });
}

#[test]
fn doctor_detects_untracked_deploy() {
    let home = tmp_home();
    let base = tempfile::tempdir().unwrap();
    with_tool_homes(base.path(), || {
        let _src = seed_install(home.path());
        // Drop an extra unit straight onto disk the lockfile doesn't know.
        std::fs::create_dir_all(base.path().join("claude/skills/stowaway")).unwrap();
        std::fs::write(
            base.path().join("claude/skills/stowaway/SKILL.md"),
            "---\nname: stowaway\n---\n",
        )
        .unwrap();

        let (out, res) = run_doctor(home.path(), DoctorArgs { offline: true });
        let err = res.unwrap_err().to_string();
        assert!(err.contains("issue"), "got: {err}");
        assert!(out.contains("untracked deploy"), "got: {out}");
    });
}

#[test]
fn doctor_offline_skips_reachability() {
    // Even with `--offline`, the manifest/lockfile/file checks still
    // run. The point of this test is to confirm that the
    // reachability lines are not emitted (= run_fetcher isn't
    // called for each enabled source).
    let home = tmp_home();
    let base = tempfile::tempdir().unwrap();
    with_tool_homes(base.path(), || {
        let _src = seed_install(home.path());
        let (out, _res) = run_doctor(home.path(), DoctorArgs { offline: true });
        assert!(
            !out.contains("reachability:"),
            "offline mode should skip reachability lines: {out}"
        );
    });
}

#[test]
fn doctor_online_fetches_each_source() {
    let home = tmp_home();
    let base = tempfile::tempdir().unwrap();
    with_tool_homes(base.path(), || {
        let _src = seed_install(home.path());
        let (out, res) = run_doctor(home.path(), DoctorArgs { offline: false });
        // Local sources always succeed; doctor reports `fetch OK`.
        res.expect("doctor ok");
        assert!(out.contains("fetch OK"), "got: {out}");
    });
}
