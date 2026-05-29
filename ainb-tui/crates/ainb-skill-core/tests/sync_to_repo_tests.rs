//! `SyncEngine::apply_to_repo` tripwire — bead v12.D.3.
//!
//! Seeds a tool-home file + a non-bare git repo (clone) that
//! `apply_to_repo` treats as the "promote-cache" for the source. The
//! executor copies home → repo path, `git add`, `git commit`, then
//! pushes (gated). We set `AINB_SYNC_SKIP_PUSH=1` for the test so the
//! commit lands locally without hitting a network remote.
//!
//! Verifies:
//!   1. After apply, the cache repo has a new commit on HEAD.
//!   2. The commit message is `sync: <unit-name>`.
//!   3. The file at repo_rel inside the cache matches the home bytes.
//!   4. Idempotency: a second apply with unchanged home bytes is a
//!      "nothing to commit" no-op (no error, HEAD unchanged).

use std::path::{Path, PathBuf};
use std::process::Command;

use ainb_skill_core::manifest::{SourceEntry, TargetMapping};
use ainb_skill_core::sync::{
    apply_to_repo, ApplyToRepoOpts, SyncAction, SyncDirection,
};

static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn git(args: &[&str], cwd: &Path) -> std::process::Output {
    Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_AUTHOR_NAME", "ainb-test")
        .env("GIT_AUTHOR_EMAIL", "ainb-test@example.invalid")
        .env("GIT_COMMITTER_NAME", "ainb-test")
        .env("GIT_COMMITTER_EMAIL", "ainb-test@example.invalid")
        .output()
        .expect("git")
}

fn init_repo(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    let out = git(&["init", "-b", "main"], dir);
    assert!(out.status.success(), "git init: {out:?}");
    git(&["config", "user.email", "ainb-test@example.invalid"], dir);
    git(&["config", "user.name", "ainb-test"], dir);
    // Seed an initial commit so the working tree has a HEAD.
    std::fs::write(dir.join("README.md"), "seed\n").unwrap();
    git(&["add", "README.md"], dir);
    let out = git(&["commit", "-m", "seed"], dir);
    assert!(out.status.success(), "seed commit: {out:?}");
}

fn rev_parse_head(dir: &Path) -> String {
    let out = git(&["rev-parse", "HEAD"], dir);
    assert!(out.status.success(), "rev-parse: {out:?}");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn fake_source() -> SourceEntry {
    SourceEntry {
        name: "skills-src".to_string(),
        kind: Some("gh".to_string()),
        uri: "gh:owner/repo".to_string(),
        r#ref: "main".to_string(),
        enabled: true,
        read_only: false,
        target_layout: vec![TargetMapping {
            glob: "skills/*/SKILL.md".to_string(),
            home: PathBuf::from(".claude/skills"),
            repo: PathBuf::from("skills"),
        }],
    }
}

fn with_skip_push<R>(body: impl FnOnce() -> R) -> R {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    std::env::set_var("AINB_SYNC_SKIP_PUSH", "1");
    let r = body();
    std::env::remove_var("AINB_SYNC_SKIP_PUSH");
    r
}

#[test]
fn apply_to_repo_copies_commits_and_advances_head() {
    let tool_home = tempfile::tempdir().unwrap();
    let repo_dir = tempfile::tempdir().unwrap();
    init_repo(repo_dir.path());
    let head_before = rev_parse_head(repo_dir.path());

    // Seed the home-side file (the local edit we want to publish).
    let body = b"---\nname: commit\n---\nedited\n";
    let home_file = tool_home.path().join(".claude/skills/commit/SKILL.md");
    std::fs::create_dir_all(home_file.parent().unwrap()).unwrap();
    std::fs::write(&home_file, body).unwrap();

    let action = SyncAction {
        unit_name: "commit".into(),
        direction: SyncDirection::ToRepo,
        reason: "home modified since deploy".into(),
    };
    let source = fake_source();
    let unit_path = PathBuf::from("skills/commit/SKILL.md");
    let opts = ApplyToRepoOpts {
        repo_cache_dir: repo_dir.path().to_path_buf(),
    };

    with_skip_push(|| {
        apply_to_repo(&action, tool_home.path(), &source, &unit_path, &opts).expect("apply");
    });

    let head_after = rev_parse_head(repo_dir.path());
    assert_ne!(head_before, head_after, "HEAD must advance after commit");

    // File landed at repo path.
    let landed = repo_dir.path().join("skills/commit/SKILL.md");
    assert!(landed.exists(), "file must land in repo cache");
    assert_eq!(std::fs::read(&landed).unwrap(), body, "bytes match");

    // Commit message matches `sync: <unit-name>`.
    let msg_out = git(&["log", "-1", "--pretty=%s"], repo_dir.path());
    let msg = String::from_utf8_lossy(&msg_out.stdout).trim().to_string();
    assert_eq!(msg, "sync: commit", "commit subject");
}

#[test]
fn apply_to_repo_is_idempotent_when_bytes_unchanged() {
    let tool_home = tempfile::tempdir().unwrap();
    let repo_dir = tempfile::tempdir().unwrap();
    init_repo(repo_dir.path());

    let body = b"body bytes\n";
    let home_file = tool_home.path().join(".claude/skills/commit/SKILL.md");
    std::fs::create_dir_all(home_file.parent().unwrap()).unwrap();
    std::fs::write(&home_file, body).unwrap();

    let action = SyncAction {
        unit_name: "commit".into(),
        direction: SyncDirection::ToRepo,
        reason: "first publish".into(),
    };
    let source = fake_source();
    let unit_path = PathBuf::from("skills/commit/SKILL.md");
    let opts = ApplyToRepoOpts {
        repo_cache_dir: repo_dir.path().to_path_buf(),
    };

    with_skip_push(|| {
        apply_to_repo(&action, tool_home.path(), &source, &unit_path, &opts).expect("apply 1");
    });
    let head_first = rev_parse_head(repo_dir.path());

    // Re-apply with identical bytes — should NOT create a new commit.
    with_skip_push(|| {
        apply_to_repo(&action, tool_home.path(), &source, &unit_path, &opts).expect("apply 2");
    });
    let head_second = rev_parse_head(repo_dir.path());
    assert_eq!(
        head_first, head_second,
        "second apply with same bytes must not create a new commit"
    );
}

#[test]
fn apply_to_repo_skips_non_to_repo_directions() {
    let tool_home = tempfile::tempdir().unwrap();
    let repo_dir = tempfile::tempdir().unwrap();
    init_repo(repo_dir.path());
    let head_before = rev_parse_head(repo_dir.path());

    let action = SyncAction {
        unit_name: "commit".into(),
        direction: SyncDirection::NoOp,
        reason: "in sync".into(),
    };
    let source = fake_source();
    let unit_path = PathBuf::from("skills/commit/SKILL.md");
    let opts = ApplyToRepoOpts {
        repo_cache_dir: repo_dir.path().to_path_buf(),
    };
    with_skip_push(|| {
        apply_to_repo(&action, tool_home.path(), &source, &unit_path, &opts).expect("noop");
    });
    let head_after = rev_parse_head(repo_dir.path());
    assert_eq!(head_before, head_after, "NoOp must leave HEAD untouched");
}

#[test]
fn apply_to_repo_errors_when_home_file_missing() {
    let tool_home = tempfile::tempdir().unwrap();
    let repo_dir = tempfile::tempdir().unwrap();
    init_repo(repo_dir.path());

    let action = SyncAction {
        unit_name: "commit".into(),
        direction: SyncDirection::ToRepo,
        reason: "first publish".into(),
    };
    let source = fake_source();
    let unit_path = PathBuf::from("skills/commit/SKILL.md");
    let opts = ApplyToRepoOpts {
        repo_cache_dir: repo_dir.path().to_path_buf(),
    };
    with_skip_push(|| {
        let err = apply_to_repo(&action, tool_home.path(), &source, &unit_path, &opts).unwrap_err();
        let msg = err.to_string().to_lowercase();
        assert!(
            msg.contains("home") || msg.contains("not found") || msg.contains("no such file"),
            "got: {err}"
        );
    });
}
