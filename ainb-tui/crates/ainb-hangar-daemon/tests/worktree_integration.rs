//! P1.6 git-worktree integration tests.
//!
//! Uses the **real** `git` binary (CI matrix + ainb-tui already require it; no
//! mock-git fakery per the P1.6 acceptance criteria). Each test stands up a
//! throwaway source repo (the "repo cache") with one commit, then drives
//! [`prepare_worktree`](ainb_hangar_daemon::worktree::prepare_worktree) /
//! [`cleanup_worktree`](ainb_hangar_daemon::worktree::cleanup_worktree).

use std::path::Path;
use std::process::Command;

use ainb_hangar_core::clock::FixedClock;
use ainb_hangar_daemon::execenv::{prepare_env, short_id};
use ainb_hangar_daemon::worktree::{cleanup_worktree, prepare_worktree};
use ainb_hangar_store::repo::task::Task;
use tempfile::TempDir;

const BASE_MS: i64 = 1_700_000_000_000;
const WS_SLUG: &str = "alpha";

fn git(dir: &Path, args: &[&str]) -> std::process::Output {
    let out = Command::new("git").args(args).current_dir(dir).output().expect("spawn git");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

/// Create a non-bare source repo with a single commit; return its path.
fn seed_repo_cache() -> TempDir {
    let dir = TempDir::new().expect("repo cache tempdir");
    git(dir.path(), &["init", "-q", "-b", "main"]);
    git(dir.path(), &["config", "user.email", "t@example.com"]);
    git(dir.path(), &["config", "user.name", "Tester"]);
    std::fs::write(dir.path().join("README.md"), b"hello").expect("seed file");
    git(dir.path(), &["add", "README.md"]);
    git(dir.path(), &["commit", "-q", "-m", "init"]);
    dir
}

fn task_fixture(id: &str, issue_id: Option<&str>) -> Task {
    Task {
        id: id.to_string(),
        workspace_id: "ws-1".to_string(),
        runtime_id: "rt-1".to_string(),
        agent_id: "ag-1".to_string(),
        issue_id: issue_id.map(str::to_string),
        status: "dispatched".to_string(),
        result: None,
        session_id: None,
        work_dir: None,
        attempt: 1,
        max_attempts: 3,
        parent_task_id: None,
        failure_reason: None,
        priority: 0,
        created_at: BASE_MS,
        dispatched_at: Some(BASE_MS),
        started_at: None,
        finished_at: None,
        autopilot_run_id: None,
    }
}

/// `git worktree list` in the repo cache, as one string.
fn worktree_list(repo_cache: &Path) -> String {
    let out = git(repo_cache, &["worktree", "list"]);
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn worktree_add_creates_branch_for_task() {
    let home = TempDir::new().expect("home");
    let cache = seed_repo_cache();
    let task = task_fixture("01HZX0000000000000000ABCDE", Some("iss-1"));
    let env = prepare_env(&task, WS_SLUG, home.path(), &FixedClock(BASE_MS)).expect("env");

    let wt = prepare_worktree(&task, &env, cache.path())
        .expect("prepare_worktree")
        .expect("worktree created for issue task");

    let branch = format!("hangar/task/{}", short_id(&task.id));
    assert_eq!(wt.branch, branch);
    assert!(
        wt.workdir.join(".git").exists(),
        "worktree checkout present"
    );

    // Branch exists.
    let branches = git(cache.path(), &["branch", "--list", &branch]);
    assert!(
        String::from_utf8_lossy(&branches.stdout).contains(&branch),
        "branch {branch} must exist"
    );
    // Worktree registered.
    assert!(
        worktree_list(cache.path()).contains(env.workdir.to_str().unwrap()),
        "git worktree list must include the new workdir"
    );
}

#[test]
fn worktree_reuse_on_resume() {
    let home = TempDir::new().expect("home");
    let cache = seed_repo_cache();
    let mut task = task_fixture("01HZX0000000000000000ABCDE", Some("iss-1"));
    let env = prepare_env(&task, WS_SLUG, home.path(), &FixedClock(BASE_MS)).expect("env");

    let first = prepare_worktree(&task, &env, cache.path()).expect("first").expect("created");

    // Resume: prior_work_dir populated → reuse, no second `git worktree add`.
    task.work_dir = Some(first.workdir.to_string_lossy().into_owned());
    let second = prepare_worktree(&task, &env, cache.path()).expect("second").expect("reused");

    assert_eq!(first.workdir, second.workdir);
    // Exactly one extra worktree entry beyond the cache's own root.
    let count = worktree_list(cache.path()).lines().count();
    assert_eq!(count, 2, "resume must not register a second worktree");
}

#[test]
fn worktree_cleanup_runs_git_worktree_remove() {
    let home = TempDir::new().expect("home");
    let cache = seed_repo_cache();
    let task = task_fixture("01HZX0000000000000000ABCDE", Some("iss-1"));
    let env = prepare_env(&task, WS_SLUG, home.path(), &FixedClock(BASE_MS)).expect("env");

    let wt = prepare_worktree(&task, &env, cache.path()).expect("prepare").expect("created");
    assert!(worktree_list(cache.path()).contains(wt.workdir.to_str().unwrap()));

    cleanup_worktree(&wt, cache.path()).expect("cleanup_worktree");

    assert!(
        !worktree_list(cache.path()).contains(wt.workdir.to_str().unwrap()),
        "git worktree remove must deregister the workdir"
    );
    assert!(!wt.workdir.exists(), "worktree dir gone after remove");
}

#[test]
fn worktree_init_skipped_for_chat_task() {
    let home = TempDir::new().expect("home");
    let cache = seed_repo_cache();
    let task = task_fixture("01HZX0000000000000000ABCDE", None); // chat: no issue
    let env = prepare_env(&task, WS_SLUG, home.path(), &FixedClock(BASE_MS)).expect("env");

    let result = prepare_worktree(&task, &env, cache.path()).expect("prepare_worktree");

    assert!(result.is_none(), "chat task (no issue) → no worktree");
    // workdir stays empty (no .git), matching the reference's lazy `repo checkout`.
    assert!(env.workdir.is_dir());
    assert!(
        !env.workdir.join(".git").exists(),
        "no git invocation for chat task"
    );
    let entries = std::fs::read_dir(&env.workdir).expect("read workdir").count();
    assert_eq!(entries, 0, "chat task workdir starts empty");
}
