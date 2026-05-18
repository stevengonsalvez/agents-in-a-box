//! Git fetcher integration test — uses a local bare repo as the
//! "remote" so the test doesn't hit the network.

use std::path::PathBuf;

use ainb_fetch::{Fetcher, GitFetcher};
use ainb_skill_core::Uri;

/// Create a fresh temp repo on disk with one commit on the `main`
/// branch carrying a single file `greeting.txt` → `hello\n`. Returns
/// the repo's on-disk path so it can be used as a `file://` clone
/// source.
fn make_seed_repo() -> tempfile::TempDir {
    let dir = tempfile::Builder::new().prefix("ainb-git-seed-").tempdir().expect("tempdir");
    let repo = git2::Repository::init(dir.path()).expect("init");
    std::fs::write(dir.path().join("greeting.txt"), b"hello\n").unwrap();

    let mut index = repo.index().unwrap();
    index.add_path(std::path::Path::new("greeting.txt")).unwrap();
    let tree_id = index.write_tree().unwrap();
    let tree = repo.find_tree(tree_id).unwrap();
    let sig = git2::Signature::now("test", "test@example.com").unwrap();
    let commit_id = repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[]).unwrap();

    // Make sure the branch is called `main` regardless of the user's
    // default git config.
    repo.branch("main", &repo.find_commit(commit_id).unwrap(), true).unwrap();
    repo.set_head("refs/heads/main").unwrap();

    dir
}

fn cache_dir() -> tempfile::TempDir {
    tempfile::Builder::new().prefix("ainb-git-cache-").tempdir().expect("tempdir")
}

#[test]
fn clones_local_remote_via_file_url() {
    let seed = make_seed_repo();
    let cache = cache_dir();

    let url = format!("git:file://{}@main", seed.path().display());
    let uri = Uri::parse(&url).unwrap_or_else(|e| panic!("parse `{url}`: {e}"));
    let fetched = GitFetcher::new().fetch(&uri, "local-test", cache.path()).expect("fetch");

    assert!(
        fetched.path.exists(),
        "fetched dir missing: {:?}",
        fetched.path
    );
    assert!(
        fetched.path.ancestors().any(|p| p == cache.path().join("local-test")),
        "fetched path not under cache/<source-name>: {:?}",
        fetched.path
    );

    let greeting: PathBuf = fetched.path.join("greeting.txt");
    let content = std::fs::read_to_string(&greeting)
        .unwrap_or_else(|e| panic!("greeting.txt missing under {:?}: {e}", fetched.path));
    assert_eq!(content, "hello\n");

    assert_eq!(fetched.resolved_sha.len(), 40, "expected full SHA-1");
    assert_eq!(
        fetched.path.file_name().unwrap().to_string_lossy(),
        fetched.resolved_sha[..8],
        "cache leaf should be short SHA"
    );
}

#[test]
fn second_fetch_is_idempotent() {
    let seed = make_seed_repo();
    let cache = cache_dir();

    let url = format!("git:file://{}@main", seed.path().display());
    let uri = Uri::parse(&url).unwrap();

    let first = GitFetcher::new().fetch(&uri, "idem", cache.path()).unwrap();
    let second = GitFetcher::new().fetch(&uri, "idem", cache.path()).unwrap();

    assert_eq!(first.path, second.path);
    assert_eq!(first.resolved_sha, second.resolved_sha);
    assert!(!cache.path().join("idem").join(".staging").exists());
}

#[test]
fn missing_ref_errors_cleanly() {
    let seed = make_seed_repo();
    let cache = cache_dir();

    let url = format!("git:file://{}@no-such-branch", seed.path().display());
    let uri = Uri::parse(&url).unwrap();
    let err = GitFetcher::new().fetch(&uri, "bad-ref", cache.path()).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("no-such-branch") || msg.contains("resolve"),
        "got: {msg}"
    );
}

#[test]
#[ignore = "needs network: set AINB_TEST_NETWORK=1 and use --ignored"]
fn fetches_real_github_repo() {
    if std::env::var("AINB_TEST_NETWORK").as_deref() != Ok("1") {
        return;
    }
    let cache = cache_dir();
    let uri = Uri::parse("gh:rust-lang/log@master").unwrap();
    let fetched = GitFetcher::new().fetch(&uri, "rust-log", cache.path()).expect("network fetch");
    assert!(fetched.path.exists());
}
