// ABOUTME: Tripwire for the ancestor walks escaping into the process CWD.
//
// `Path::ancestors()` on a RELATIVE path terminates at `""`, and
// `Path::new("").join(".git")` is `".git"`, which the filesystem resolves
// against the process's current directory. Both ancestor walks introduced with
// the plain-checkout fix therefore ended by probing the CWD:
//
//   * `InteractiveSessionManager::get_source_repository` would attribute the
//     session to whatever repository `ainb` happened to be launched from, so a
//     bogus path would render as a real workspace instead of "(broken)".
//   * `cli::run::classify_session_root` would classify the LAUNCH directory,
//     making `ainb run`'s no-isolation warning fire (or stay silent) based on
//     a tree that is not the session's.
//
// Its own test binary on purpose: the only way to prove a CWD escape is to
// control the CWD, and `std::env::set_current_dir` is process-global. `cargo
// test` runs the tests of ONE binary as threads of ONE process, so doing this
// inside the lib test binary (1600+ tests, several of which read the CWD)
// would be a race. One test, one process, no race.

use std::path::{Path, PathBuf};
use std::process::Command;

use ainb::cli::run::{SessionRoot, classify_session_root};
use ainb::interactive::InteractiveSessionManager;

fn git_ok(cwd: &Path, args: &[&str]) -> bool {
    Command::new("git")
        .args([
            "-c",
            "user.name=ainb-test",
            "-c",
            "user.email=ainb@test.invalid",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .current_dir(cwd)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Build a real plain checkout (`.git` is a DIRECTORY) and return its root.
fn plain_checkout(root: &Path) -> PathBuf {
    let repo = root.join("cwd-repo");
    std::fs::create_dir_all(&repo).unwrap();
    assert!(git_ok(&repo, &["init"]), "git init");
    std::fs::write(repo.join("README.md"), "hi").unwrap();
    assert!(git_ok(&repo, &["add", "README.md"]), "git add");
    assert!(git_ok(&repo, &["commit", "-m", "init"]), "git commit");
    assert!(repo.join(".git").is_dir(), "precondition: plain checkout");
    repo
}

#[test]
fn relative_paths_never_resolve_against_the_process_cwd() {
    if !Command::new("git").arg("--version").output().is_ok_and(|o| o.status.success()) {
        eprintln!("SKIP: git unavailable");
        return;
    }

    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().canonicalize().expect("canonicalize tempdir");
    let repo = plain_checkout(&root);

    // Stand the process inside a real repository. Without the guards, every
    // relative path below walks up to `""` and finds THIS `.git`.
    std::env::set_current_dir(&repo).expect("chdir into the checkout");

    // The paths are deliberately nonexistent: nothing about them is inside a
    // repository, so the only way to a non-None / non-NotAGitRepo answer is the
    // CWD escape.
    for relative in ["no-such-dir", "no/such/nested/dir", "."] {
        assert_eq!(
            InteractiveSessionManager::get_source_repository(Path::new(relative)),
            None,
            "relative path {relative:?} must not resolve to the process CWD's repository"
        );
        assert_eq!(
            classify_session_root(Path::new(relative)),
            SessionRoot::NotAGitRepo,
            "relative path {relative:?} must not be classified from the process CWD"
        );
    }

    // Control: the SAME directory, addressed absolutely, still resolves. The
    // guard rejects ambient resolution, not the checkout itself.
    assert_eq!(
        InteractiveSessionManager::get_source_repository(&repo),
        Some(repo.clone()),
        "an absolute path to the checkout still resolves"
    );
    assert_eq!(
        classify_session_root(&repo),
        SessionRoot::SharedCheckout,
        "an absolute path to the checkout is still classified as unisolated"
    );

    // Leave the process where the harness expects it. The tempdir is about to
    // be deleted, and a CWD inside a deleted directory breaks later cleanup.
    std::env::set_current_dir(&root).expect("chdir out of the checkout");
}
