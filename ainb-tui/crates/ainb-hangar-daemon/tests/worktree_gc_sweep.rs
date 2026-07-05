//! tcp yjj: orphan task-worktree GC sweep.
//!
//! A Ctrl-C mid-run or a crash between finalize and teardown leaves a run's git
//! worktree under `{home}/.agents-in-a-box/worktrees/<task_id>/` even though its
//! task row is terminal. [`sweep_orphan_worktrees`] is the scheduled backstop that
//! reclaims those — but only a TERMINAL task's CLEAN worktree; a dirty tree (holding
//! uncommitted agent work) and an active / unknown task's tree are left alone.
//!
//! Uses the **real** `git` binary (the P1.6 convention in this crate) so the
//! `git worktree` registration + prune are exercised for real.

use std::path::{Path, PathBuf};
use std::process::Command;

use ainb_hangar_daemon::workdir_provision::sweep_orphan_worktrees;
use ainb_hangar_store::Store;
use sqlx::SqlitePool;
use tempfile::TempDir;

const WS: &str = "ws-a";

fn git(dir: &Path, args: &[&str]) -> std::process::Output {
    let out = Command::new("git").args(args).current_dir(dir).output().expect("spawn git");
    assert!(out.status.success(), "git {args:?} failed: {}", String::from_utf8_lossy(&out.stderr));
    out
}

/// A non-bare origin repo with one commit — the repo worktrees are added from.
fn seed_origin() -> TempDir {
    let dir = TempDir::new().expect("origin tempdir");
    git(dir.path(), &["init", "-q", "-b", "main"]);
    git(dir.path(), &["config", "user.email", "t@example.com"]);
    git(dir.path(), &["config", "user.name", "Tester"]);
    std::fs::write(dir.path().join("README.md"), b"hello").expect("seed file");
    git(dir.path(), &["add", "README.md"]);
    git(dir.path(), &["commit", "-q", "-m", "init"]);
    dir
}

fn worktrees_root(home: &Path) -> PathBuf {
    home.join(".agents-in-a-box").join("worktrees")
}

/// Add a real git worktree at `worktrees/<slug>` on branch `ainb/<slug>` from
/// `origin` (mirroring `workdir_provision::provision_worktree`). Returns its path.
fn add_worktree(origin: &Path, home: &Path, slug: &str) -> PathBuf {
    let path = worktrees_root(home).join(slug);
    std::fs::create_dir_all(path.parent().unwrap()).expect("mkdir worktrees root");
    git(
        origin,
        &["worktree", "add", "-q", "-b", &format!("ainb/{slug}"), path.to_str().unwrap()],
    );
    path
}

/// Whether `origin` still registers a worktree whose path contains `slug`.
fn registration_present(origin: &Path, slug: &str) -> bool {
    let out = git(origin, &["worktree", "list", "--porcelain"]);
    String::from_utf8_lossy(&out.stdout).contains(slug)
}

/// Open a store under the isolated home and seed one task per `(id, status)` with
/// the minimal FK chain so `get_by_id` resolves the status the sweep keys on.
async fn seed_store(home: &Path, tasks: &[(&str, &str)]) -> Store {
    let store = Store::open_in(&home.join(".agents-in-a-box")).await.expect("store");
    let pool = store.pool();
    seed_chain(pool).await;
    for (id, status) in tasks {
        sqlx::query(
            "INSERT INTO agent_task_queue (id, workspace_id, runtime_id, agent_id, status, created_at) \
             VALUES (?, ?, 'rt', 'ag', ?, 0)",
        )
        .bind(id).bind(WS).bind(status)
        .execute(pool).await.unwrap();
    }
    store
}

async fn seed_chain(pool: &SqlitePool) {
    sqlx::query("INSERT OR IGNORE INTO workspace (id, slug, name, created_at) VALUES (?, ?, ?, 0)")
        .bind(WS).bind(WS).bind(WS).execute(pool).await.unwrap();
    sqlx::query("INSERT OR IGNORE INTO user (id, email, created_at) VALUES ('u','u@e.com',0)")
        .execute(pool).await.unwrap();
    sqlx::query("INSERT OR IGNORE INTO agent_runtime (id, workspace_id, daemon_id, provider, runtime_mode, status) VALUES ('rt', ?, 'd','claude','local','online')")
        .bind(WS).execute(pool).await.unwrap();
    sqlx::query("INSERT OR IGNORE INTO agent (id, workspace_id, name, runtime_id, instructions, visibility, owner_id) VALUES ('ag', ?, 'A','rt','x','workspace','u')")
        .bind(WS).execute(pool).await.unwrap();
}

/// The core contract: a terminal + clean worktree is removed and deregistered; a
/// terminal + DIRTY one is kept; an active one is untouched; an unknown-task dir is
/// kept (never delete on uncertainty).
#[tokio::test]
async fn sweep_removes_terminal_clean_keeps_dirty_and_active() {
    let origin = seed_origin();
    let home = TempDir::new().expect("home");

    // Four ULID-shaped slugs; the worktree dir is named after the FULL task id.
    let done = "01JB2K3W4ATERMCLEANAAAAAAA";
    let dirty = "01JB2K3W4ATERMDIRTYBBBBBBB";
    let active = "01JB2K3W4ARUNNINGCCCCCCCCC";
    let unknown = "01JB2K3W4AUNKNOWNDDDDDDDDD"; // no task row

    let done_wt = add_worktree(origin.path(), home.path(), done);
    let dirty_wt = add_worktree(origin.path(), home.path(), dirty);
    let active_wt = add_worktree(origin.path(), home.path(), active);
    let unknown_wt = add_worktree(origin.path(), home.path(), unknown);

    // Leave uncommitted work in the dirty worktree.
    std::fs::write(dirty_wt.join("scratch.txt"), b"agent work in progress").unwrap();

    let store = seed_store(
        home.path(),
        &[(done, "done"), (dirty, "failed"), (active, "running")],
    )
    .await;

    let report = sweep_orphan_worktrees(store.pool(), home.path()).await.expect("sweep");

    // Terminal + clean → removed + deregistered.
    assert!(!done_wt.exists(), "a terminal, clean worktree is reclaimed");
    assert!(!registration_present(origin.path(), done), "its git registration is pruned");

    // Terminal + dirty → kept (uncommitted work preserved).
    assert!(dirty_wt.is_dir(), "a dirty worktree is preserved");
    assert!(registration_present(origin.path(), dirty), "its registration is kept");

    // Active → untouched.
    assert!(active_wt.is_dir(), "a running task's worktree is left alone");

    // Unknown task id → kept (no positive terminal proof).
    assert!(unknown_wt.is_dir(), "an unknown-task worktree is never deleted");

    assert_eq!(report.removed, 1, "exactly one worktree removed");
    assert_eq!(report.kept_dirty, 1, "exactly one kept dirty");
    assert_eq!(report.kept_active, 2, "the active + unknown dirs are kept");
}

/// A home that never provisioned a worktree has no `worktrees/` dir — a clean
/// no-op, not an error.
#[tokio::test]
async fn sweep_absent_tree_is_noop() {
    let home = TempDir::new().expect("home");
    let store = Store::open_in(&home.path().join(".agents-in-a-box")).await.expect("store");
    let report = sweep_orphan_worktrees(store.pool(), home.path()).await.expect("sweep");
    assert_eq!(report.removed, 0);
    assert_eq!(report.kept_dirty, 0);
    assert_eq!(report.kept_active, 0);
}
