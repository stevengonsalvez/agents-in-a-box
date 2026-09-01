// ABOUTME: Regression tests for the 2026-09 repo-cache wipe: concurrent
// `clone_repo` calls against one cold repo, plus the refuse-to-delete guard.
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
use std::sync::Barrier;
use tempfile::TempDir;

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

/// One test's world: a temp dir, a bare "remote", a cache root and a receipt log
/// path. The receipt log is INJECTED into every manager rather than set through
/// the environment, so nothing here mutates process-global state that other
/// threads in this binary are concurrently reading.
struct Fixture {
    tmp: TempDir,
    bare: PathBuf,
    cache_dir: PathBuf,
    receipt_log: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let bare = init_bare_remote(tmp.path());
        let cache_dir = tmp.path().join("repos");
        let receipt_log = tmp.path().join("receipts").join("destructive-ops.log");
        Self {
            tmp,
            bare,
            cache_dir,
            receipt_log,
        }
    }

    fn manager(&self) -> RemoteRepoManager {
        RemoteRepoManager::with_cache_dir(self.cache_dir.clone())
            .unwrap()
            .with_receipt_log(self.receipt_log.clone())
    }

    fn inputs(&self) -> (RepoSource, ParsedRepo) {
        repo_inputs(&self.bare)
    }

    fn receipts(&self) -> String {
        std::fs::read_to_string(&self.receipt_log).unwrap_or_default()
    }
}

/// How many callers race for the same cold repo. Eight is enough that the
/// pre-fix code lost every one of them on this machine, cheap enough (a local
/// clone of a one-commit repo) to stay a normal-speed test.
const RACERS: usize = 8;

/// Staging directories left behind under `parent`. There should never be any
/// once a clone has returned.
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
/// Pre-fix this failed on both counts: the losers' `git clone` hit "destination
/// path already exists and is not an empty directory", and their cleanup wiped
/// the winner's clone, leaving an empty cache and orphaned worktrees.
#[test]
fn concurrent_clones_of_one_cold_repo_all_succeed_and_leave_the_cache_populated() {
    let fx = Fixture::new();
    let manager = fx.manager();
    let (_, parsed) = fx.inputs();
    let cache_path = manager.get_cache_path(&parsed);
    assert!(!manager.is_cached(&parsed), "cache starts cold");

    // A barrier keeps the start staggered by as little as possible, so all eight
    // are inside the check-then-clone window at once.
    let barrier = Barrier::new(RACERS);
    let results: Vec<Result<PathBuf, String>> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..RACERS)
            .map(|_| {
                let cache_dir = fx.cache_dir.clone();
                let receipt_log = fx.receipt_log.clone();
                let bare = fx.bare.clone();
                let barrier = &barrier;
                scope.spawn(move || {
                    // A manager per thread: two ainb processes each build their
                    // own, so sharing one here would test less than production.
                    let manager = RemoteRepoManager::with_cache_dir(cache_dir)
                        .unwrap()
                        .with_receipt_log(receipt_log);
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

    // The shared clone is intact and holds the remote's content, not an empty
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

/// A repo whose worktrees are live is never deleted: the deletion errors and the
/// clone (with its worktrees) stays on disk, with a receipt recording the refusal.
#[test]
fn removing_a_repo_with_live_worktrees_is_refused() {
    let fx = Fixture::new();
    let manager = fx.manager();
    let (source, parsed) = fx.inputs();

    let cache_path = manager.clone_repo(&source, &parsed).unwrap();
    let worktree = fx.tmp.path().join("worktrees").join("agents-feature");
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
    let log = fx.receipts();
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
    assert!(
        !log.contains("REMOVED"),
        "nothing was removed, so no receipt may claim one:\n{log}"
    );
}

/// A worktree directory deleted by hand leaves its registration behind until
/// someone prunes. That stale entry must NOT latch the guard shut forever.
///
/// Pre-review the guard counted `.git/worktrees/` entries, so this repo reported
/// one "live" worktree with zero live worktrees and every later delete failed.
#[test]
fn a_stale_worktree_registration_does_not_jam_the_guard() {
    let fx = Fixture::new();
    let manager = fx.manager();
    let (source, parsed) = fx.inputs();

    let cache_path = manager.clone_repo(&source, &parsed).unwrap();
    let worktree = fx.tmp.path().join("worktrees").join("gone");
    git_ok(
        &cache_path,
        &[
            "worktree",
            "add",
            "-b",
            "agents/gone",
            worktree.to_str().unwrap(),
        ],
    );

    // The user deletes the checkout directly. git keeps the registration.
    std::fs::remove_dir_all(&worktree).unwrap();
    assert!(
        cache_path.join(".git").join("worktrees").exists(),
        "the stale registration is still on disk (that is the point)"
    );

    manager
        .remove_cached_repo(&parsed)
        .expect("a registration with no checkout must not block the delete");
    assert!(!cache_path.exists(), "the repo was actually removed");
}

/// The guard fails CLOSED. When it cannot determine whether worktrees link to a
/// path, it refuses rather than assuming none, because the assumption is the one
/// that destroys data.
#[test]
fn an_unreadable_repo_is_refused_rather_than_assumed_empty() {
    let fx = Fixture::new();
    let manager = fx.manager();
    let (source, parsed) = fx.inputs();

    let cache_path = manager.clone_repo(&source, &parsed).unwrap();

    // Replace the .git directory with a file git cannot make sense of. This is
    // the shape a `--separate-git-dir` clone or a linked worktree has, where a
    // `read_dir` of `.git/worktrees` returns ENOTDIR rather than a count.
    std::fs::remove_dir_all(cache_path.join(".git")).unwrap();
    std::fs::write(cache_path.join(".git"), "not a gitfile\n").unwrap();

    let err = manager
        .remove_cached_repo(&parsed)
        .expect_err("an undeterminable repo must be refused, not deleted");
    assert!(
        err.to_string().contains("cannot determine") || err.to_string().contains("cannot run git"),
        "error should say the check failed, got: {err}"
    );
    assert!(cache_path.exists(), "the repo was not deleted");
}

/// A path that escapes the cache root is refused even though the caller asked
/// for it by name. `get_cache_path` joins regex-parsed URL components, so an
/// owner of `..` would otherwise walk out of the cache and delete elsewhere.
#[test]
fn a_path_escaping_the_cache_root_is_refused() {
    let fx = Fixture::new();
    let manager = fx.manager();

    // A directory outside the cache root that must survive. The cache root is
    // `<tmp>/repos`, so `../precious/inner` climbs out of it into `<tmp>`.
    let outsider = fx.tmp.path().join("precious").join("inner");
    std::fs::create_dir_all(&outsider).unwrap();
    std::fs::write(outsider.join("keep.txt"), "important").unwrap();

    let traversal = ParsedRepo {
        source: RepoSource::LocalPath(fx.bare.clone()),
        host: "..".to_string(),
        owner: "precious".to_string(),
        repo_name: "inner".to_string(),
    };
    assert_eq!(
        manager.get_cache_path(&traversal).canonicalize().unwrap(),
        outsider.canonicalize().unwrap(),
        "the traversal really does resolve outside the cache (else this proves nothing)"
    );

    let err = manager
        .remove_cached_repo(&traversal)
        .expect_err("a path outside the cache root must be refused");
    assert!(
        err.to_string().contains("not inside the repo cache"),
        "error should name the containment breach, got: {err}"
    );
    assert!(
        outsider.join("keep.txt").exists(),
        "the directory outside the cache survived"
    );
}

/// A clone still succeeds when the filesystem cannot provide the advisory lock.
///
/// The lock is an optimisation (staging plus atomic rename is what makes
/// concurrency safe), so losing it must degrade to redundant work, never to a
/// failed session launch. Simulated by parking a directory where the lock file
/// goes, which makes opening it for write fail the way a lockless filesystem
/// makes `flock` fail.
#[test]
fn a_clone_still_succeeds_when_the_lock_cannot_be_taken() {
    let fx = Fixture::new();
    let manager = fx.manager();
    let (source, parsed) = fx.inputs();
    let cache_path = manager.get_cache_path(&parsed);

    std::fs::create_dir_all(cache_path.parent().unwrap()).unwrap();
    std::fs::create_dir_all(cache_path.parent().unwrap().join(".widget.clone-lock")).unwrap();

    let path = manager
        .clone_repo(&source, &parsed)
        .expect("an unavailable lock must not fail the clone");
    assert!(path.join("README.md").exists(), "the clone still landed");
    assert!(
        staging_leftovers(cache_path.parent().unwrap()).is_empty(),
        "no staging dir orphaned on the lockless path"
    );
}

/// Staging directories abandoned by a killed process are swept, not accumulated.
///
/// Every crashed clone used to leave a full partial checkout beside the repo
/// forever, since cleanup only ever targeted the exact pid+nonce path of the
/// current run.
#[test]
fn an_abandoned_staging_dir_is_swept_on_the_next_clone() {
    let fx = Fixture::new();
    let manager = fx.manager();
    let (source, parsed) = fx.inputs();
    let cache_path = manager.get_cache_path(&parsed);
    let owner_dir = cache_path.parent().unwrap();

    // A corpse from a run that died mid-clone, under a pid that is not ours.
    std::fs::create_dir_all(owner_dir).unwrap();
    let corpse = owner_dir.join(".widget.clone-tmp-999999-0");
    std::fs::create_dir_all(corpse.join("subdir")).unwrap();
    std::fs::write(corpse.join("subdir").join("partial.txt"), "half a clone").unwrap();

    manager.clone_repo(&source, &parsed).unwrap();

    assert!(!corpse.exists(), "the abandoned staging dir was swept");
    assert!(
        staging_leftovers(owner_dir).is_empty(),
        "no staging dirs remain: {:?}",
        staging_leftovers(owner_dir)
    );
    assert!(cache_path.join("README.md").exists(), "the clone landed");
}

/// A clone that fails leaves nothing behind: no half-populated cache that
/// `is_cached()` would later mistake for a warm one, and no staging dir.
#[test]
fn a_failed_clone_leaves_no_cache_and_no_staging_dir() {
    let fx = Fixture::new();
    let not_a_repo = fx.tmp.path().join("not-a-repo");
    std::fs::create_dir_all(&not_a_repo).unwrap();

    let manager = fx.manager();
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
