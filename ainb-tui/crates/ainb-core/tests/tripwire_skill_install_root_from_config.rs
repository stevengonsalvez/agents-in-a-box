//! Tripwire: `general.skill_install_real_homes = false` in config.toml actually
//! reaches `ainb skill install`.
//!
//! Two failures this catches, both of which make the key inert while the
//! settings screen reports it saved:
//!
//! 1. `export_env_bridge` running inside `tokio_main`. `skill` / `source` /
//!    `search` return through `ainb_cli::run()` in `main()` BEFORE `tokio_main`
//!    is ever entered, and `ainb-cli` does not depend on `ainb`, so the bridge
//!    never ran for the one command this key governs and the skill landed in
//!    the real `~/.claude/skills/` anyway.
//! 2. The `AINB_USE_REAL_HOMES` gate in `ainb-adapters-tool` being dropped
//!    again. It had no reader at all before P2b: real homes were
//!    unconditional and the documented opt-out did nothing.
//!
//! Exercised through the real binary, because the bug is in `main()`'s ordering
//! and no in-process test can observe it.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn ainb_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ainb"))
}

/// A `$HOME` with onboarding skipped and the given `config.toml` body.
fn seeded_home(body: &str) -> tempfile::TempDir {
    let home = tempfile::tempdir().expect("home tempdir");
    let config_dir = home.path().join(".agents-in-a-box/config");
    fs::create_dir_all(&config_dir).unwrap();
    fs::write(
        config_dir.join("onboarding.toml"),
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
    fs::write(config_dir.join("config.toml"), body).unwrap();
    home
}

/// A local source directory holding one skill.
fn seeded_source() -> tempfile::TempDir {
    let src = tempfile::tempdir().expect("src tempdir");
    fs::create_dir_all(src.path().join("skills/commit")).unwrap();
    fs::write(
        src.path().join("skills/commit/SKILL.md"),
        "---\nname: commit\ndescription: tripwire fixture\n---\nbody\n",
    )
    .unwrap();
    src
}

#[test]
fn skill_install_honours_the_config_key_without_the_env_var() {
    let bin = ainb_bin();
    let home = seeded_home("[general]\nskill_install_real_homes = false\n");
    let ainb_home = tempfile::tempdir().expect("ainb-home tempdir");
    let src = seeded_source();
    let local_uri = format!("local:{}", src.path().display());

    // Deliberately NO `AINB_USE_REAL_HOMES`: the config key is the only signal.
    let run = |args: &[&str]| {
        Command::new(&bin)
            .args(args)
            .env("HOME", home.path())
            .env("AINB_HOME", ainb_home.path())
            .env_remove("AINB_USE_REAL_HOMES")
            .output()
            .expect("run ainb")
    };

    let add = run(&["source", "add", &local_uri, "--name", "fix"]);
    assert!(
        add.status.success(),
        "source add failed: stdout={} stderr={}",
        String::from_utf8_lossy(&add.stdout),
        String::from_utf8_lossy(&add.stderr)
    );

    let unit_uri = format!("{local_uri}@main/skills/commit");
    let install = run(&[
        "skill",
        "install",
        &unit_uri,
        "--targets",
        "claude",
        "--yes",
    ]);
    assert!(
        install.status.success(),
        "skill install failed: stdout={} stderr={}",
        String::from_utf8_lossy(&install.stdout),
        String::from_utf8_lossy(&install.stderr)
    );

    let sandbox = ainb_home.path().join("tools/claude/skills/commit/SKILL.md");
    let real_home = home.path().join(".claude/skills/commit/SKILL.md");
    assert!(
        sandbox.exists(),
        "`general.skill_install_real_homes = false` must route the install into \
         the managed sandbox at {}; stdout:\n{}",
        sandbox.display(),
        String::from_utf8_lossy(&install.stdout)
    );
    assert!(
        !real_home.exists(),
        "the skill must NOT also land in the real tool home at {}",
        real_home.display()
    );
}

/// The shipped default is unchanged: with no key and no env var, an install
/// still lands where the tool actually looks.
#[test]
fn the_default_still_installs_into_the_real_tool_home() {
    let bin = ainb_bin();
    let home = seeded_home("");
    let ainb_home = tempfile::tempdir().expect("ainb-home tempdir");
    let src = seeded_source();
    let local_uri = format!("local:{}", src.path().display());

    let run = |args: &[&str]| {
        Command::new(&bin)
            .args(args)
            .env("HOME", home.path())
            .env("AINB_HOME", ainb_home.path())
            .env_remove("AINB_USE_REAL_HOMES")
            .output()
            .expect("run ainb")
    };

    assert!(run(&["source", "add", &local_uri, "--name", "fix"]).status.success());
    let unit_uri = format!("{local_uri}@main/skills/commit");
    let install = run(&[
        "skill",
        "install",
        &unit_uri,
        "--targets",
        "claude",
        "--yes",
    ]);
    assert!(
        install.status.success(),
        "skill install failed: stderr={}",
        String::from_utf8_lossy(&install.stderr)
    );

    assert!(
        home.path().join(".claude/skills/commit/SKILL.md").exists(),
        "the default must still install where the tool looks; stdout:\n{}",
        String::from_utf8_lossy(&install.stdout)
    );
}
