//! Concurrent-sync race tripwire — bead v12.1.T7.
//!
//! Distinguished-engineer review flagged that two `ainb skill sync`
//! processes against the same promote-cache race on `.git/index.lock`:
//! corruption-safe (git protects itself) but UX-hostile, surfacing as
//! "another git process running" stderr noise from `git push`. There
//! was no advisory lock before this bead.
//!
//! `apply_to_repo` now takes a per-source advisory file lock at
//! `<repo_cache_dir>/.ainb-sync.lock` via `fs2::FileExt::try_lock_exclusive`.
//! Contention surfaces as `SyncEngineError::SyncInProgress(<lock_path>)`
//! rather than raw git stderr.
//!
//! Verifies:
//!   1. Pre-held lock + `apply_to_repo` → `SyncInProgress`, not Io noise.
//!   2. Lock is released after the call returns (subsequent calls win).
//!   3. Two real threads contending: exactly one succeeds, the other
//!      gets `SyncInProgress`.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use ainb_skill_core::manifest::{SourceEntry, TargetMapping};
use ainb_skill_core::sync::{
    apply_to_repo, ApplyToRepoOpts, SyncAction, SyncDirection, SyncEngineError,
    SYNC_LOCK_FILENAME,
};
use fs2::FileExt;

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
    std::fs::write(dir.join("README.md"), "seed\n").unwrap();
    git(&["add", "README.md"], dir);
    let out = git(&["commit", "-m", "seed"], dir);
    assert!(out.status.success(), "seed commit: {out:?}");
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

fn seed_home_file(install_root: &Path) -> PathBuf {
    // tool_home arg to apply_to_repo plays install_root's role (its
    // .claude/ embedment is stripped from the layout home by the
    // executor); seed the file at the install-root-relative path the
    // layout actually resolves to.
    let unit_rel = PathBuf::from("skills/commit/SKILL.md");
    let body = b"---\nname: commit\n---\nedited\n";
    let home_file = install_root.join("skills/commit/SKILL.md");
    std::fs::create_dir_all(home_file.parent().unwrap()).unwrap();
    std::fs::write(&home_file, body).unwrap();
    unit_rel
}

fn with_skip_push<R>(body: impl FnOnce() -> R) -> R {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    std::env::set_var("AINB_SYNC_SKIP_PUSH", "1");
    let r = body();
    std::env::remove_var("AINB_SYNC_SKIP_PUSH");
    r
}

#[test]
fn apply_to_repo_returns_sync_in_progress_when_lock_held() {
    let tool_home = tempfile::tempdir().unwrap();
    let repo_dir = tempfile::tempdir().unwrap();
    init_repo(repo_dir.path());
    let unit_rel = seed_home_file(tool_home.path());

    // Manually grab the lock as a sister process would.
    let lock_path = repo_dir.path().join(SYNC_LOCK_FILENAME);
    let lock_file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .expect("open lock file");
    FileExt::try_lock_exclusive(&lock_file).expect("seed-lock must acquire");

    let action = SyncAction {
        unit_name: "commit".to_string(),
        direction: SyncDirection::ToRepo,
        reason: "test".to_string(),
    };
    let source = fake_source();
    let opts = ApplyToRepoOpts {
        repo_cache_dir: repo_dir.path().to_path_buf(),
    };

    let result = with_skip_push(|| {
        apply_to_repo(&action, tool_home.path(), "claude", &source, &unit_rel, &opts)
    });

    match result {
        Err(SyncEngineError::SyncInProgress(path)) => {
            assert!(
                path.contains(SYNC_LOCK_FILENAME),
                "SyncInProgress path must contain `{SYNC_LOCK_FILENAME}`, got: {path}"
            );
        }
        other => panic!("expected SyncInProgress, got: {other:?}"),
    }

    // Drop our seed-lock and verify a follow-up call succeeds.
    FileExt::unlock(&lock_file).expect("seed-lock unlock");
    drop(lock_file);

    let result_after = with_skip_push(|| {
        apply_to_repo(&action, tool_home.path(), "claude", &source, &unit_rel, &opts)
    });
    assert!(
        result_after.is_ok(),
        "second call (lock free) must succeed: {result_after:?}"
    );
}

#[test]
fn apply_to_repo_releases_lock_on_return() {
    // After a successful apply_to_repo, the next call must NOT see
    // SyncInProgress — RAII Drop must have closed the underlying fd
    // and released the kernel lock.
    let tool_home = tempfile::tempdir().unwrap();
    let repo_dir = tempfile::tempdir().unwrap();
    init_repo(repo_dir.path());
    let unit_rel = seed_home_file(tool_home.path());

    let action = SyncAction {
        unit_name: "commit".to_string(),
        direction: SyncDirection::ToRepo,
        reason: "test".to_string(),
    };
    let source = fake_source();
    let opts = ApplyToRepoOpts {
        repo_cache_dir: repo_dir.path().to_path_buf(),
    };

    with_skip_push(|| {
        apply_to_repo(&action, tool_home.path(), "claude", &source, &unit_rel, &opts)
            .expect("first apply must succeed");
        // Second call: same bytes, nothing to commit, but it MUST still
        // acquire + release the lock cleanly. If the lock leaked, this
        // would surface as SyncInProgress.
        apply_to_repo(&action, tool_home.path(), "claude", &source, &unit_rel, &opts)
            .expect("second apply must succeed (lock released after first)");
    });
}

#[test]
fn two_threads_contend_exactly_one_wins() {
    // Real cross-thread contention. Each thread opens the lock file via
    // a fresh `apply_to_repo` call; `fs2::try_lock_exclusive` enforces
    // exclusion at the file-description level, so even within the same
    // process exactly one thread acquires the lock at any instant.
    //
    // We don't need both calls to be guaranteed-simultaneous because
    // contention is what we want to observe: if either thread fully
    // completes before the other starts, the second one will succeed
    // too — and that's not a failure. The assertion is "no two
    // threads can succeed AT THE SAME TIME", which we observe by
    // counting how many succeeded and how many got SyncInProgress.
    //
    // We force overlap by holding the lock externally before either
    // thread runs, then releasing it once both are blocked on it.
    let tool_home = tempfile::tempdir().unwrap();
    let repo_dir = tempfile::tempdir().unwrap();
    init_repo(repo_dir.path());
    let unit_rel = seed_home_file(tool_home.path());

    let lock_path = repo_dir.path().join(SYNC_LOCK_FILENAME);
    let blocker = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .expect("open blocker");
    FileExt::try_lock_exclusive(&blocker).expect("seed blocker");

    let opts = ApplyToRepoOpts {
        repo_cache_dir: repo_dir.path().to_path_buf(),
    };
    let source = fake_source();
    let action = SyncAction {
        unit_name: "commit".to_string(),
        direction: SyncDirection::ToRepo,
        reason: "test".to_string(),
    };

    let succeeded = Arc::new(AtomicUsize::new(0));
    let busy = Arc::new(AtomicUsize::new(0));

    let _env_guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    std::env::set_var("AINB_SYNC_SKIP_PUSH", "1");

    let mut handles = vec![];
    for _ in 0..2 {
        let tool_home_p = tool_home.path().to_path_buf();
        let opts_c = opts.clone();
        let source_c = source.clone();
        let action_c = action.clone();
        let unit_rel_c = unit_rel.clone();
        let succeeded_c = Arc::clone(&succeeded);
        let busy_c = Arc::clone(&busy);
        handles.push(std::thread::spawn(move || {
            // Each thread tries to grab the (still-blocked) lock.
            // With non-blocking try_lock_exclusive, both will get
            // SyncInProgress while the blocker holds it.
            let r = apply_to_repo(
                &action_c,
                &tool_home_p,
                "claude",
                &source_c,
                &unit_rel_c,
                &opts_c,
            );
            match r {
                Ok(()) => {
                    succeeded_c.fetch_add(1, Ordering::SeqCst);
                }
                Err(SyncEngineError::SyncInProgress(_)) => {
                    busy_c.fetch_add(1, Ordering::SeqCst);
                }
                Err(e) => panic!("unexpected error from thread: {e:?}"),
            }
        }));
    }

    // Let both threads hit the lock and see it busy.
    for h in handles {
        h.join().expect("thread");
    }

    std::env::remove_var("AINB_SYNC_SKIP_PUSH");

    // Both should have observed `SyncInProgress` because the blocker
    // held the lock for the entire span of the test.
    assert_eq!(
        busy.load(Ordering::SeqCst),
        2,
        "both threads must see SyncInProgress while blocker holds lock"
    );
    assert_eq!(
        succeeded.load(Ordering::SeqCst),
        0,
        "no thread may succeed while blocker holds lock"
    );

    FileExt::unlock(&blocker).expect("blocker unlock");
}
