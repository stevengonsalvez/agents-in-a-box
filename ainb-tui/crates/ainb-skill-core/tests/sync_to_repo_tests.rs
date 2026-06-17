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
use ainb_skill_core::sync::{ApplyToRepoOpts, SyncAction, SyncDirection, apply_to_repo};
use ainb_skill_core::{SandboxLayout, SandboxTier, build_skill_manager_sandbox};

/// Wraps a `TempDir` + the `SandboxLayout` it backs so callers can
/// hold one binding (keeping the tempdir alive) and reach the layout
/// paths through Deref. The fixture's `claude_home` plays
/// `install_root_for("claude")`'s role — `apply_to_repo` reads + writes
/// under it after the dot-dir strip.
struct SandboxGuard {
    _tempdir: tempfile::TempDir,
    layout: SandboxLayout,
}

impl SandboxGuard {
    fn new() -> Self {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let layout = build_skill_manager_sandbox(tempdir.path(), SandboxTier::Minimal)
            .expect("sandbox fixture");
        Self {
            _tempdir: tempdir,
            layout,
        }
    }

    fn claude_home(&self) -> &Path {
        &self.layout.claude_home
    }

    fn bare_remote(&self) -> &Path {
        &self.layout.bare_remote
    }
}

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
    let sb = SandboxGuard::new();
    let tool_home = sb.claude_home();
    let repo_dir = tempfile::tempdir().unwrap();
    init_repo(repo_dir.path());
    let head_before = rev_parse_head(repo_dir.path());

    // Seed the home-side file (the local edit we want to publish).
    let body = b"---\nname: commit\n---\nedited\n";
    // tool_home now plays install_root's role (the .claude/-prefix is
    // stripped from the layout's home by apply_to_repo); seed the file
    // at the install-root-relative path the layout resolves to.
    let home_file = tool_home.join("skills/commit/SKILL.md");
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
        apply_to_repo(&action, tool_home, "claude", &source, &unit_path, &opts).expect("apply");
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
    let sb = SandboxGuard::new();
    let tool_home = sb.claude_home();
    let repo_dir = tempfile::tempdir().unwrap();
    init_repo(repo_dir.path());

    let body = b"body bytes\n";
    // tool_home now plays install_root's role (the .claude/-prefix is
    // stripped from the layout's home by apply_to_repo); seed the file
    // at the install-root-relative path the layout resolves to.
    let home_file = tool_home.join("skills/commit/SKILL.md");
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
        apply_to_repo(&action, tool_home, "claude", &source, &unit_path, &opts).expect("apply 1");
    });
    let head_first = rev_parse_head(repo_dir.path());

    // Re-apply with identical bytes — should NOT create a new commit.
    with_skip_push(|| {
        apply_to_repo(&action, tool_home, "claude", &source, &unit_path, &opts).expect("apply 2");
    });
    let head_second = rev_parse_head(repo_dir.path());
    assert_eq!(
        head_first, head_second,
        "second apply with same bytes must not create a new commit"
    );
}

#[test]
fn apply_to_repo_skips_non_to_repo_directions() {
    let sb = SandboxGuard::new();
    let tool_home = sb.claude_home();
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
        apply_to_repo(&action, tool_home, "claude", &source, &unit_path, &opts).expect("noop");
    });
    let head_after = rev_parse_head(repo_dir.path());
    assert_eq!(head_before, head_after, "NoOp must leave HEAD untouched");
}

/// Exercise the push path against a bare local repo acting as the
/// "remote". Distinguished-engineer review (M4) flagged that every
/// other test sets `AINB_SYNC_SKIP_PUSH=1`, so the argv-smuggle check
/// at the head of the push branch + the `git push -- origin <ref>`
/// wire are entirely uncovered. This test removes that gate and
/// asserts the bare remote sees the new commit.
#[test]
fn apply_to_repo_pushes_to_real_local_bare_remote() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    std::env::remove_var("AINB_SYNC_SKIP_PUSH");

    // Use the fixture-built bare remote — already initialised with one
    // seed commit on main (`skills/initial-skill/SKILL.md`). No more
    // hand-rolled `git init --bare` + initial-push dance in the test.
    let sb = SandboxGuard::new();
    let tool_home = sb.claude_home();
    let bare = sb.bare_remote();

    // Clone the bare into a working tree we can commit + push against.
    let repo_dir = tempfile::tempdir().unwrap();
    let clone_out = Command::new("git")
        .args([
            "clone",
            "--",
            bare.to_str().unwrap(),
            repo_dir.path().to_str().unwrap(),
        ])
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .expect("git clone");
    assert!(clone_out.status.success(), "git clone: {clone_out:?}");
    git(
        &["config", "user.email", "ainb-test@example.invalid"],
        repo_dir.path(),
    );
    git(&["config", "user.name", "ainb-test"], repo_dir.path());
    let bare_head_before = {
        let out = git(&["rev-parse", "main"], bare);
        assert!(out.status.success(), "bare rev-parse: {out:?}");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };

    let body = b"---\nname: commit\n---\npublished via sync\n";
    // tool_home now plays install_root's role (the .claude/-prefix is
    // stripped from the layout's home by apply_to_repo); seed the file
    // at the install-root-relative path the layout resolves to.
    let home_file = tool_home.join("skills/commit/SKILL.md");
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

    apply_to_repo(&action, tool_home, "claude", &source, &unit_path, &opts).expect("apply");

    let bare_head_after = {
        let out = git(&["rev-parse", "main"], bare);
        assert!(out.status.success(), "bare rev-parse after: {out:?}");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    assert_ne!(
        bare_head_before, bare_head_after,
        "bare remote HEAD must advance after push"
    );

    let subj = git(&["log", "-1", "--pretty=%s", "main"], bare);
    assert!(subj.status.success(), "bare log: {subj:?}");
    let msg = String::from_utf8_lossy(&subj.stdout).trim().to_string();
    assert_eq!(msg, "sync: commit", "bare remote commit subject");
}

/// Argv-smuggle hardening — TO_REPO push refuses ref values starting
/// with `-`. Pairs with the production fix at sync.rs::apply_to_repo.
#[test]
fn apply_to_repo_rejects_argv_smuggled_ref() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    std::env::remove_var("AINB_SYNC_SKIP_PUSH");

    let sb = SandboxGuard::new();
    let tool_home = sb.claude_home();
    let repo_dir = tempfile::tempdir().unwrap();
    init_repo(repo_dir.path());

    let body = b"body\n";
    // tool_home now plays install_root's role (the .claude/-prefix is
    // stripped from the layout's home by apply_to_repo); seed the file
    // at the install-root-relative path the layout resolves to.
    let home_file = tool_home.join("skills/commit/SKILL.md");
    std::fs::create_dir_all(home_file.parent().unwrap()).unwrap();
    std::fs::write(&home_file, body).unwrap();

    let mut source = fake_source();
    source.r#ref = "--upload-pack=cmd".into();

    let action = SyncAction {
        unit_name: "commit".into(),
        direction: SyncDirection::ToRepo,
        reason: "smuggle attempt".into(),
    };
    let unit_path = PathBuf::from("skills/commit/SKILL.md");
    let opts = ApplyToRepoOpts {
        repo_cache_dir: repo_dir.path().to_path_buf(),
    };
    let err = apply_to_repo(&action, tool_home, "claude", &source, &unit_path, &opts)
        .expect_err("argv-smuggled ref must be refused");
    let msg = err.to_string().to_lowercase();
    assert!(
        msg.contains("argv") || msg.contains("smuggled") || msg.contains("ref"),
        "expected argv-smuggle reject message; got: {err}"
    );
}

#[test]
fn apply_to_repo_errors_when_home_file_missing() {
    let sb = SandboxGuard::new();
    let tool_home = sb.claude_home();
    let repo_dir = tempfile::tempdir().unwrap();
    init_repo(repo_dir.path());

    // The fixture pre-seeds `skills/commit/SKILL.md` for other tests;
    // pick a unit name the fixture does NOT seed so the home-file-
    // missing path actually exercises.
    let action = SyncAction {
        unit_name: "missing-skill".into(),
        direction: SyncDirection::ToRepo,
        reason: "first publish".into(),
    };
    let source = fake_source();
    let unit_path = PathBuf::from("skills/missing-skill/SKILL.md");
    let opts = ApplyToRepoOpts {
        repo_cache_dir: repo_dir.path().to_path_buf(),
    };
    with_skip_push(|| {
        let err =
            apply_to_repo(&action, tool_home, "claude", &source, &unit_path, &opts).unwrap_err();
        let msg = err.to_string().to_lowercase();
        assert!(
            msg.contains("home") || msg.contains("not found") || msg.contains("no such file"),
            "got: {err}"
        );
    });
}
