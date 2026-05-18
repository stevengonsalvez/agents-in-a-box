//! Tripwire: `ainb migrate --check` against a populated install
//! root produces a report but mutates NOTHING on disk. Catches:
//! `--check` mode accidentally invoking the wipe path, lockfile
//! reset leaking outside the `--clean` branch, doctor-style
//! side-effecting probes appearing under `--check`.
//!
//! No tmux dependency.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn ainb_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ainb"))
}

#[test]
fn migrate_check_is_read_only() {
    let bin = ainb_bin();
    let home = tempfile::tempdir().expect("home tempdir");
    let ainb_home = tempfile::tempdir().expect("ainb-home tempdir");
    let src = tempfile::tempdir().expect("src tempdir");

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
        "---\nname: commit\n---\nbody\n",
    )
    .unwrap();
    let local_uri = format!("local:{}", src.path().display());

    // source add + skill install — produces lockfile.units + on-disk state.
    let add = Command::new(&bin)
        .args(["source", "add", &local_uri, "--name", "fix"])
        .env("HOME", home.path())
        .env("AINB_HOME", ainb_home.path())
        .output()
        .expect("source add");
    assert!(add.status.success(), "source add failed");

    let unit_uri = format!("{local_uri}@main/skills/commit");
    let install = Command::new(&bin)
        .args(["skill", "install", &unit_uri, "--targets", "claude", "--yes"])
        .env("HOME", home.path())
        .env("AINB_HOME", ainb_home.path())
        .output()
        .expect("skill install");
    assert!(install.status.success(), "skill install failed");

    let deployed_file = ainb_home.path().join("tools/claude/skills/commit/SKILL.md");
    assert!(deployed_file.exists(), "install didn't land where expected: {}", deployed_file.display());

    // Snapshot lockfile bytes + the deployed file contents before
    // migrate --check.
    let lockfile_path = ainb_home.path().join("lock.yaml");
    let lock_before = fs::read(&lockfile_path).expect("read lockfile pre-check");
    let deployed_before = fs::read(&deployed_file).expect("read deployed pre-check");

    // Run migrate --check — must not mutate anything.
    let check = Command::new(&bin)
        .args(["migrate", "--check"])
        .env("HOME", home.path())
        .env("AINB_HOME", ainb_home.path())
        .output()
        .expect("migrate --check");
    assert!(
        check.status.success(),
        "migrate --check failed: stderr={}",
        String::from_utf8_lossy(&check.stderr)
    );

    let stdout = String::from_utf8_lossy(&check.stdout).to_string();

    // Positive: report mentions the tool and unit we installed.
    assert!(
        stdout.contains("claude") && stdout.contains("unit"),
        "migrate --check report missing expected markers:\n{stdout}"
    );

    // Negative: lockfile bytes unchanged.
    let lock_after = fs::read(&lockfile_path).expect("read lockfile post-check");
    assert_eq!(
        lock_before, lock_after,
        "migrate --check mutated the lockfile — spec §8.3 says it's read-only"
    );

    // Negative: deployed file bytes unchanged.
    let deployed_after = fs::read(&deployed_file).expect("read deployed post-check");
    assert_eq!(
        deployed_before, deployed_after,
        "migrate --check mutated a deployed file — that's --clean territory"
    );
}
