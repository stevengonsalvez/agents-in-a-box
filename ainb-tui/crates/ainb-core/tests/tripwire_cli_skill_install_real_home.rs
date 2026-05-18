//! Tripwire: `ainb skill install … --targets claude --yes` with
//! `AINB_USE_REAL_HOMES=1` + a tempdir `$HOME` lands the file at
//! the tier-3 path `$HOME/.claude/skills/<name>/SKILL.md`. Catches:
//! tier-3 resolution regression, env-name hyphen→underscore mishap,
//! `AINB_USE_REAL_HOMES` opt-in flag accidentally removed.
//!
//! No tmux dependency — exercises the CLI directly via process spawn.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn ainb_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ainb"))
}

#[test]
fn skill_install_with_real_homes_lands_under_home_dot_claude() {
    let bin = ainb_bin();
    let home = tempfile::tempdir().expect("home tempdir");
    let ainb_home = tempfile::tempdir().expect("ainb-home tempdir");
    let src = tempfile::tempdir().expect("src tempdir");

    // Seed onboarding skip + a minimal local source.
    fs::create_dir_all(home.path().join(".agents-in-a-box/config")).unwrap();
    fs::write(
        home.path().join(".agents-in-a-box/config/onboarding.toml"),
        format!(
            r#"completed = true
completed_at = "2026-05-11T00:00:00+00:00"
version = "{ver}"
skipped_dependencies = []
git_directories = []
"#,
            ver = env!("CARGO_PKG_VERSION"),
        ),
    )
    .unwrap();

    fs::create_dir_all(src.path().join("skills/commit")).unwrap();
    fs::write(
        src.path().join("skills/commit/SKILL.md"),
        "---\nname: commit\ndescription: tripwire fixture\n---\nbody\n",
    )
    .unwrap();

    let local_uri = format!("local:{}", src.path().display());

    // Run `ainb source add` first.
    let add = Command::new(&bin)
        .args(["source", "add", &local_uri, "--name", "fix"])
        .env("HOME", home.path())
        .env("AINB_HOME", ainb_home.path())
        .env("AINB_USE_REAL_HOMES", "1")
        .output()
        .expect("ainb source add");
    assert!(
        add.status.success(),
        "source add failed: stdout={} stderr={}",
        String::from_utf8_lossy(&add.stdout),
        String::from_utf8_lossy(&add.stderr)
    );

    // Now install for the claude target. AINB_USE_REAL_HOMES=1 +
    // HOME=<tempdir> means tier-3 resolution should write under
    // `<HOME>/.claude/skills/commit/SKILL.md`.
    let unit_uri = format!("{local_uri}@main/skills/commit");
    let install = Command::new(&bin)
        .args(["skill", "install", &unit_uri, "--targets", "claude", "--yes"])
        .env("HOME", home.path())
        .env("AINB_HOME", ainb_home.path())
        .env("AINB_USE_REAL_HOMES", "1")
        .output()
        .expect("ainb skill install");
    assert!(
        install.status.success(),
        "skill install failed: stdout={} stderr={}",
        String::from_utf8_lossy(&install.stdout),
        String::from_utf8_lossy(&install.stderr)
    );

    // Positive: file landed at the real-home path.
    let landed = home.path().join(".claude/skills/commit/SKILL.md");
    assert!(
        landed.exists(),
        "expected install under tier-3 real-home path {}; ainb_home would have been wrong place to land. stdout:\n{}",
        landed.display(),
        String::from_utf8_lossy(&install.stdout)
    );

    // Negative: must NOT also have landed under the managed sandbox
    // (mixing tiers would indicate the precedence logic broke).
    let sandbox = ainb_home.path().join("tools/claude/skills/commit/SKILL.md");
    assert!(
        !sandbox.exists(),
        "install also wrote to sandbox at {} — tier precedence broken",
        sandbox.display()
    );
}
