// ABOUTME: Regression tests for the 2026-09 repo-cache wipe — concurrent
// `clone_repo` calls against one cold repo, and the refuse-to-delete guard.
//
// The incident: `clone_repo` checked `is_cached()` and cloned without a lock, so
// N processes launching sessions against the same not-yet-cached repo all cloned
// into the same path; the losers' "clean up my partial clone" `remove_dir_all`
// deleted the WINNER's finished repo, orphaning every worktree cut from it and
// re-arming the race for the next launch. These tests drive real `git` against
// local repos (no network) and would fail against that code.

use ainb::git::{ParsedRepo, RemoteRepoManager, RepoSource};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Barrier, OnceLock};

/// Run `git` in `cwd` with a deterministic identity, isolated from any global
/// config, and assert it succeeded.
fn git_ok(cwd: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A bare repo with one commit, standing in for a remote so nothing here needs
/// the network.
fn init_bare_remote(tmp: &Path) -> PathBuf {
    let work = tmp.join("source");
    std::fs::create_dir_all(&work).unwrap();
    git_ok(&work, &["init", "-q", "-b", "main"]);
    std::fs::write(work.join("README.md"), "hello\n").unwrap();
    git_ok(&work, &["add", "README.md"]);
    git_ok(&work, &["commit", "-qm", "init"]);

    let bare = tmp.join("remote.git");
    git_ok(
        tmp,
        &[
            "clone",
            "--bare",
            "-q",
            work.to_str().unwrap(),
            bare.to_str().unwrap(),
        ],
    );
    bare
}

/// The `(source, parsed)` pair `clone_repo` takes: the source supplies the clone
/// URL (a local path here), the parsed components the cache location.
fn repo_inputs(bare: &Path) -> (RepoSource, ParsedRepo) {
    let source = RepoSource::LocalPath(bare.to_path_buf());
    let parsed = ParsedRepo {
        source: source.clone(),
        host: "github.com".to_string(),
        owner: "acme".to_string(),
        repo_name: "widget".to_string(),
    };
    (source, parsed)
}

/// How many callers race for the same cold repo. Eight is enough that the
/// pre-fix code lost every one of them on this machine, cheap enough (a local
/// clone of a one-commit repo) to stay a normal-speed test.
const RACERS: usize = 8;

static RECEIPT_LOG: OnceLock<PathBuf> = OnceLock::new();

/// Redirect the destructive-op receipt log to a per-run temp file and return it.
///
/// The override is a process-global env var, so every test in this binary calls
/// this first — before anything could write a receipt — and shares one file.
fn receipt_log() -> &'static Path {
    RECEIPT_LOG.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("ainb-receipts-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("destructive-ops.log");
        std::env::set_var("AINB_DESTRUCTIVE_OPS_LOG", &path);
        path
    })
}

/// Staging directories left behind under `parent` (there should never be any
/// once a clone has returned).
fn staging_leftovers(parent: &Path) -> Vec<String> {
    std::fs::read_dir(parent)
        .map(|entries| {
            entries
                .flatten()
                .map(|e| e.file_name().to_string_lossy().to_string())
                .filter(|name| name.contains(".clone-tmp-"))
                .collect()
        })
        .unwrap_or_default()
}

/// THE regression test. Eight callers race to clone the same cold repo from a
/// standing start: every one must succeed and the populated cache must survive.
///
/// Pre-fix this failed on both counts — the losers' `git clone` hit "destination
/// path already exists and is not an empty directory", and their cleanup wiped
/// the winner's clone, leaving an empty cache and orphaned worktrees.
#[test]
fn concurrent_clones_of_one_cold_repo_all_succeed_and_leave_the_cache_populated() {
    receipt_log();
    let tmp = tempfile::tempdir().unwrap();
    let bare = init_bare_remote(tmp.path());
    let cache_dir = tmp.path().join("repos");

    let manager = RemoteRepoManager::with_cache_dir(cache_dir.clone()).unwrap();
    let (_, parsed) = repo_inputs(&bare);
    let cache_path = manager.get_cache_path(&parsed);
    assert!(!manager.is_cached(&parsed), "cache starts cold");

    // A barrier keeps the start staggered by as little as possible, so all eight
    // are inside the check-then-clone window at once.
    let barrier = Barrier::new(RACERS);
    let results: Vec<Result<PathBuf, String>> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..RACERS)
            .map(|_| {
                let cache_dir = cache_dir.clone();
                let bare = bare.clone();
                let barrier = &barrier;
                scope.spawn(move || {
                    // A manager per thread: two ainb processes each build their
                    // own, so sharing one here would test less than production.
                    let manager = RemoteRepoManager::with_cache_dir(cache_dir).unwrap();
                    let (source, parsed) = repo_inputs(&bare);
                    barrier.wait();
                    manager.clone_repo(&source, &parsed).map_err(|e| e.to_string())
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });

    for (i, result) in results.iter().enumerate() {
        let path = result.as_ref().unwrap_or_else(|e| panic!("racer {i} failed to clone: {e}"));
        assert_eq!(
            *path, cache_path,
            "racer {i} resolved a different cache path"
        );
    }

    // The shared clone is intact and holds the remote's content — not an empty
    // directory left by a loser's cleanup.
    assert!(cache_path.join(".git").exists(), "cache lost its .git");
    assert!(
        cache_path.join("README.md").exists(),
        "cache lost the remote's content"
    );
    assert!(manager.is_cached(&parsed), "repo reports as cached");

    // Exactly one clone: the losers reused the winner's, they did not each
    // publish a copy, and no staging dir was orphaned.
    let owner_dir = cache_path.parent().unwrap();
    assert!(
        staging_leftovers(owner_dir).is_empty(),
        "staging dirs left behind: {:?}",
        staging_leftovers(owner_dir)
    );
    let published: Vec<_> = std::fs::read_dir(owner_dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|name| !name.starts_with('.'))
        .collect();
    assert_eq!(published, vec!["widget".to_string()], "exactly one clone");
}

/// A repo whose worktrees are live is never deleted — the deletion errors and
/// the clone (with its worktrees) stays on disk.
#[test]
fn removing_a_repo_with_live_worktrees_is_refused() {
    let receipt = receipt_log();
    let tmp = tempfile::tempdir().unwrap();
    let bare = init_bare_remote(tmp.path());
    let manager = RemoteRepoManager::with_cache_dir(tmp.path().join("repos")).unwrap();
    let (source, parsed) = repo_inputs(&bare);

    let cache_path = manager.clone_repo(&source, &parsed).unwrap();
    let worktree = tmp.path().join("worktrees").join("agents-feature");
    git_ok(
        &cache_path,
        &[
            "worktree",
            "add",
            "-b",
            "agents/feature",
            worktree.to_str().unwrap(),
        ],
    );
    assert!(worktree.join("README.md").exists(), "worktree checked out");

    let err = manager
        .remove_cached_repo(&parsed)
        .expect_err("deleting a repo with live worktrees must fail");
    assert!(
        err.to_string().contains("live worktree"),
        "error should name the reason, got: {err}"
    );
    assert!(cache_path.join(".git").exists(), "the repo survived");
    assert!(worktree.join("README.md").exists(), "the worktree survived");

    // The refusal left a receipt outside the ainb tree.
    let log = std::fs::read_to_string(receipt).unwrap_or_default();
    let line = log
        .lines()
        .find(|l| l.contains("remove_cached_repo") && l.contains("widget"))
        .unwrap_or_else(|| panic!("no receipt for the refused delete in:\n{log}"));
    assert!(
        line.contains("REFUSED"),
        "receipt should record the refusal: {line}"
    );
    assert!(
        line.contains("pid="),
        "receipt should record the pid: {line}"
    );
}

/// A clone that fails leaves nothing behind — no half-populated cache that
/// `is_cached()` would later mistake for a warm one, and no staging dir.
#[test]
fn a_failed_clone_leaves_no_cache_and_no_staging_dir() {
    receipt_log();
    let tmp = tempfile::tempdir().unwrap();
    let not_a_repo = tmp.path().join("not-a-repo");
    std::fs::create_dir_all(&not_a_repo).unwrap();

    let manager = RemoteRepoManager::with_cache_dir(tmp.path().join("repos")).unwrap();
    let (source, parsed) = repo_inputs(&not_a_repo);

    assert!(
        manager.clone_repo(&source, &parsed).is_err(),
        "cloning a non-repo fails"
    );
    let cache_path = manager.get_cache_path(&parsed);
    assert!(!cache_path.exists(), "no partial cache left behind");
    assert!(!manager.is_cached(&parsed), "still reports as cold");
    let owner_dir = cache_path.parent().unwrap();
    assert!(
        staging_leftovers(owner_dir).is_empty(),
        "staging dirs left behind: {:?}",
        staging_leftovers(owner_dir)
    );
}
