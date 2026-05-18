//! Tripwire: `ainb doctor` exits non-zero on a broken state and the
//! stdout names the specific failure. Spec §17 acceptance #4.
//! Catches: doctor swallowing errors silently, exit code regression
//! (returning 0 even with hash mismatches), output stops mentioning
//! the kind of issue found.
//!
//! No tmux dependency.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn ainb_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ainb"))
}

#[test]
fn doctor_exits_nonzero_on_missing_deployed_file() {
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

    // source add + skill install (under managed sandbox — keeps the
    // test deterministic; tier-3 covered by a sibling tripwire).
    let add = Command::new(&bin)
        .args(["source", "add", &local_uri, "--name", "fix"])
        .env("HOME", home.path())
        .env("AINB_HOME", ainb_home.path())
        .output()
        .expect("source add");
    assert!(
        add.status.success(),
        "source add failed: {}",
        String::from_utf8_lossy(&add.stderr)
    );

    let unit_uri = format!("{local_uri}@main/skills/commit");
    let install = Command::new(&bin)
        .args([
            "skill",
            "install",
            &unit_uri,
            "--targets",
            "claude",
            "--yes",
        ])
        .env("HOME", home.path())
        .env("AINB_HOME", ainb_home.path())
        .output()
        .expect("skill install");
    assert!(
        install.status.success(),
        "install failed: {}",
        String::from_utf8_lossy(&install.stderr)
    );

    // Find the deployed file (under the sandbox) and DELETE it to
    // break the lockfile's claim.
    let sandbox = ainb_home.path().join("tools/claude/skills/commit/SKILL.md");
    assert!(
        sandbox.exists(),
        "install didn't land where expected: {}",
        sandbox.display()
    );
    fs::remove_file(&sandbox).expect("nuke deployed file");

    // Doctor MUST detect the missing file.
    let doctor = Command::new(&bin)
        .args(["doctor", "--offline"])
        .env("HOME", home.path())
        .env("AINB_HOME", ainb_home.path())
        .output()
        .expect("doctor");

    // Positive: non-zero exit.
    assert!(
        !doctor.status.success(),
        "doctor returned 0 (success) on a known-broken state — spec §17 #4 regression. stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&doctor.stdout),
        String::from_utf8_lossy(&doctor.stderr)
    );

    let combined = format!(
        "{}\n{}",
        String::from_utf8_lossy(&doctor.stdout),
        String::from_utf8_lossy(&doctor.stderr)
    );

    // Negative: the verdict text should mention the specific kind of
    // problem (missing on disk / hash mismatch). A generic "doctor
    // failed" with no detail would be a regression in the issue
    // surfacing.
    assert!(
        combined.contains("missing on disk") || combined.contains("hash mismatch"),
        "doctor exited non-zero but stdout/stderr doesn't name the failure mode:\n{combined}"
    );
}
