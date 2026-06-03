// ABOUTME: Integration tests for star → remote-default-branch worktree creation.
// Drives real `git` against temp repos to prove a session launched from a star
// branches off the freshly-fetched remote default (origin/HEAD) — even when the
// cache's local `main` is stale or the repo's default branch is `master`. Also
// covers the favorites local→remote migration through the public crate API.

use ainb::git::{RemoteRepoManager, RepoSource};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Run `git` in `cwd` with deterministic identity + isolation from any global
/// config, returning the raw output.
fn git(cwd: &Path, args: &[&str]) -> std::process::Output {
    Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .expect("git runs")
}

fn git_ok(cwd: &Path, args: &[&str]) {
    let out = git(cwd, args);
    assert!(
        out.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
}

fn rev_parse(cwd: &Path, rev: &str) -> String {
    let out = git(cwd, &["rev-parse", rev]);
    assert!(
        out.status.success(),
        "rev-parse {rev} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn current_branch(cwd: &Path) -> String {
    String::from_utf8_lossy(&git(cwd, &["rev-parse", "--abbrev-ref", "HEAD"]).stdout)
        .trim()
        .to_string()
}

fn commit_file(repo: &Path, name: &str, contents: &str) {
    std::fs::write(repo.join(name), contents).unwrap();
    git_ok(repo, &["add", name]);
    git_ok(repo, &["commit", "-m", &format!("add {name}")]);
}

/// Build a source repo whose default branch is `default_branch`, with one commit.
fn init_source(tmp: &Path, default_branch: &str) -> PathBuf {
    let src = tmp.join("src");
    std::fs::create_dir_all(&src).unwrap();
    git_ok(&src, &["init", "-b", default_branch]);
    commit_file(&src, "a.txt", "A");
    src
}

#[test]
fn star_session_branches_off_fresh_remote_main_not_stale_local() {
    let tmp = tempfile::tempdir().unwrap();
    let src = init_source(tmp.path(), "main");

    let cache = tmp.path().join("cache");
    git_ok(
        tmp.path(),
        &["clone", src.to_str().unwrap(), cache.to_str().unwrap()],
    );

    // Advance the source's main AFTER cloning — the cache's local `main` is now
    // stale (still at commit A); its origin/main only refreshes on fetch, which
    // create_worktree_off_remote_default performs.
    commit_file(&src, "b.txt", "B");
    let src_tip = rev_parse(&src, "HEAD");
    let cache_local_main = rev_parse(&cache, "main");
    assert_ne!(
        cache_local_main, src_tip,
        "precondition: cache local main must be stale relative to source"
    );

    let mgr = RemoteRepoManager::new().unwrap();
    let wt = tmp.path().join("wt-main");
    let source = RepoSource::LocalPath(src.clone());
    let created = mgr
        .create_worktree_off_remote_default(&cache, &wt, "agents/test", &source)
        .expect("worktree off remote default");
    assert_eq!(created, wt);

    // The new agent branch is based on the freshly-fetched origin/main (commit
    // B), NOT the cache's stale local main (commit A).
    assert_eq!(
        rev_parse(&wt, "HEAD"),
        src_tip,
        "worktree must be based on fresh origin/main, not stale local main"
    );
    assert_eq!(current_branch(&wt), "agents/test");
}

#[test]
fn star_session_branches_off_master_when_default() {
    let tmp = tempfile::tempdir().unwrap();
    let src = init_source(tmp.path(), "master");

    let cache = tmp.path().join("cache");
    git_ok(
        tmp.path(),
        &["clone", src.to_str().unwrap(), cache.to_str().unwrap()],
    );
    let src_tip = rev_parse(&src, "HEAD");

    // The cache has NO local `main` — the old get_default_branch ladder
    // (local main → local master → current HEAD) is exactly what this replaces.
    let mgr = RemoteRepoManager::new().unwrap();
    let wt = tmp.path().join("wt-master");
    let source = RepoSource::LocalPath(src.clone());
    mgr.create_worktree_off_remote_default(&cache, &wt, "agents/x", &source)
        .expect("worktree off remote default (master)");

    assert_eq!(
        rev_parse(&wt, "HEAD"),
        src_tip,
        "worktree must be based on origin/master"
    );
    assert_eq!(current_branch(&wt), "agents/x");
}

#[test]
fn migration_rewrites_local_with_origin_and_drops_without() {
    use ainb::config::{Favorite, FavoriteSourceType, FavoritesStore};

    let tmp = tempfile::tempdir().unwrap();

    // A local repo WITH an origin remote → migrates to remote indicator.
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git_ok(&repo, &["init"]);
    git_ok(
        &repo,
        &["remote", "add", "origin", "https://github.com/o/r.git"],
    );

    // A path that is NOT a git repo → dropped.
    let orphan = tmp.path().join("not-a-repo");
    std::fs::create_dir_all(&orphan).unwrap();

    let mut store = FavoritesStore::default();
    store
        .add(Favorite::new(
            "withremote".into(),
            repo.display().to_string(),
            FavoriteSourceType::LocalPath,
        ))
        .unwrap();
    store
        .add(Favorite::new(
            "orphan".into(),
            orphan.display().to_string(),
            FavoriteSourceType::LocalPath,
        ))
        .unwrap();

    let report = store.migrate_local_to_remote();
    assert_eq!(report.migrated.len(), 1);
    assert_eq!(report.dropped, vec!["orphan".to_string()]);

    let migrated = store.get("withremote").expect("withremote survives");
    assert_eq!(migrated.source_type, FavoriteSourceType::GithubShorthand);
    assert_eq!(migrated.source, "o/r");
    assert!(store.get("orphan").is_none(), "orphan dropped");
}
