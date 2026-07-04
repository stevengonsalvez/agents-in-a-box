//! Per-run working-directory provisioning from a card's `repo_ref` (spec F5).
//!
//! At dispatch a card's task carries a `repo_ref` (migration 0032): an absolute
//! path to a git repo, or the literal `scratch`. This module turns that into the
//! run's actual working directory, mirroring the New-Session `WorktreeManager`
//! contract (`ainb-core/src/git/worktree_manager.rs`) but implemented daemon-side
//! (the daemon cannot depend on ainb-core — the crate cycle) by shelling out to
//! the real `git` binary, exactly as [`crate::worktree`] does:
//!
//! ```text
//!  repo_ref = Some("/path/to/repo")  ──▶  volatile worktree
//!                                          <home>/.agents-in-a-box/worktrees/<slug>
//!                                          on branch  ainb/<slug>
//!                                          torn down on completion, KEPT if dirty
//!  repo_ref = Some("scratch")        ──▶  <home>/.agents-in-a-box/scratch/<slug>
//!                                          `git init` (idempotent), run IN PLACE
//!                                          (already isolated — never torn down)
//!  repo_ref = None                   ──▶  the fallback execenv workdir (a chat /
//!                                          autopilot task — the pre-F5 behaviour)
//! ```
//!
//! The `slug` is the caller's per-task key (the task short-id). Because it is
//! unique per task, two cards launched on the SAME repo provision two DISTINCT
//! worktrees on two DISTINCT `ainb/<slug>` branches — they never collide (the F5
//! "N tasks on one repo never collide" guarantee).
//!
//! # Teardown (keep-if-dirty)
//!
//! [`teardown`] removes a clean worktree (and deregisters it from the origin
//! repo) but KEEPS a dirty one — an agent that left uncommitted work does not
//! lose it. Scratch + fallback are never torn down (scratch is durable by design;
//! the fallback is the execenv the GC sweeper owns).

use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The provisioned working directory for a run, plus what teardown must do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunWorkdir {
    /// A volatile git worktree checked out from `repo` at `path` on `branch`.
    /// Torn down after the run unless it holds uncommitted changes.
    Worktree {
        /// The worktree checkout directory (the run's cwd).
        path: PathBuf,
        /// The branch checked out (`ainb/<slug>`).
        branch: String,
        /// The origin repo the worktree belongs to (needed to deregister it).
        repo: PathBuf,
    },
    /// The scratch repo, run in place. Durable — never torn down.
    Scratch {
        /// The scratch repo directory (the run's cwd).
        path: PathBuf,
    },
    /// No repo (a chat / autopilot task): run in the fallback execenv workdir.
    Fallback {
        /// The fallback working directory (the run's cwd).
        path: PathBuf,
    },
}

impl RunWorkdir {
    /// The directory the provider run should use as its cwd.
    #[must_use]
    pub fn path(&self) -> &Path {
        match self {
            Self::Worktree { path, .. } | Self::Scratch { path } | Self::Fallback { path } => path,
        }
    }
}

/// What [`teardown`] actually did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TeardownOutcome {
    /// A clean worktree was removed + deregistered.
    Removed,
    /// A dirty worktree was KEPT (uncommitted work preserved).
    KeptDirty,
    /// Nothing to tear down (scratch / fallback).
    NoOp,
}

/// The branch a worktree run checks out for `slug`.
#[must_use]
pub fn worktree_branch(slug: &str) -> String {
    format!("ainb/{slug}")
}

/// The `.agents-in-a-box` root under `home` (the daemon's `hangar_home`, which is
/// the `$AINB_HANGAR_HOME` override or the real `~`). Scratch + worktrees live
/// beside the execenv tree under this root.
fn ainb_root(home: &Path) -> PathBuf {
    home.join(".agents-in-a-box")
}

/// Provision the run's working directory for `repo_ref`.
///
/// - `Some("scratch")` → `<home>/.agents-in-a-box/scratch/<slug>`, `git init`ed
///   idempotently, run in place.
/// - `Some(path)` naming a git repo → a fresh worktree under
///   `<home>/.agents-in-a-box/worktrees/<slug>` on branch `ainb/<slug>`.
/// - `None` (or a blank ref) → the `fallback` execenv workdir, untouched.
///
/// Idempotent on resume: a worktree whose path already exists (a recovered task)
/// is reused rather than re-added; a scratch dir already `git init`ed is left as
/// is.
///
/// # Errors
///
/// Returns an [`io::Error`] if a directory cannot be created, `git init` /
/// `git worktree add` fails, or a worktree `repo_ref` is not a valid UTF-8 path.
pub fn provision(
    repo_ref: Option<&str>,
    slug: &str,
    home: &Path,
    fallback: &Path,
) -> io::Result<RunWorkdir> {
    let repo_ref = repo_ref.map(str::trim).filter(|s| !s.is_empty());
    match repo_ref {
        None => Ok(RunWorkdir::Fallback { path: fallback.to_path_buf() }),
        Some("scratch") => provision_scratch(slug, home),
        Some(path) => provision_worktree(Path::new(path), slug, home),
    }
}

/// Create (or reuse) the scratch repo `<home>/.agents-in-a-box/scratch/<slug>`
/// and `git init` it if it is not already a repo. Idempotent.
fn provision_scratch(slug: &str, home: &Path) -> io::Result<RunWorkdir> {
    let path = ainb_root(home).join("scratch").join(slug);
    std::fs::create_dir_all(&path)?;
    if !path.join(".git").exists() {
        run_git(&path, &["init", "--quiet"])?;
    }
    Ok(RunWorkdir::Scratch { path })
}

/// Add a fresh worktree for `repo` at `<home>/.agents-in-a-box/worktrees/<slug>`
/// on branch `ainb/<slug>`, or reuse an already-registered one (resume).
fn provision_worktree(repo: &Path, slug: &str, home: &Path) -> io::Result<RunWorkdir> {
    let path = ainb_root(home).join("worktrees").join(slug);
    let branch = worktree_branch(slug);
    let wt = RunWorkdir::Worktree {
        path: path.clone(),
        branch: branch.clone(),
        repo: repo.to_path_buf(),
    };

    // Resume: an existing checkout is reused rather than re-added (a second
    // `git worktree add` on a registered path fails).
    if path.join(".git").exists() {
        return Ok(wt);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let path_str = path.to_str().ok_or_else(non_utf8_path)?;
    run_git(repo, &["worktree", "add", path_str, "-b", &branch])?;
    Ok(wt)
}

/// Tear down a run's working directory (spec F5 keep-if-dirty).
///
/// A [`RunWorkdir::Worktree`] is removed + deregistered when clean, but KEPT when
/// it holds uncommitted changes (the agent's work is preserved). Scratch +
/// fallback are no-ops.
///
/// # Errors
///
/// Returns an [`io::Error`] only if a clean worktree cannot be removed. A dirty
/// worktree is kept (never an error), and an already-gone worktree is a no-op.
pub fn teardown(wd: &RunWorkdir) -> io::Result<TeardownOutcome> {
    let RunWorkdir::Worktree { path, repo, .. } = wd else {
        return Ok(TeardownOutcome::NoOp);
    };
    if !path.exists() {
        return Ok(TeardownOutcome::NoOp);
    }
    if is_dirty(path)? {
        return Ok(TeardownOutcome::KeptDirty);
    }
    // Clean → deregister from the origin repo, then remove any residue.
    let path_str = path.to_str().ok_or_else(non_utf8_path)?;
    // Best-effort deregister; a non-zero exit (already-removed) is tolerated
    // because the post-condition we assert is "the worktree dir is gone".
    let _ = Command::new("git")
        .args(["worktree", "remove", path_str])
        .current_dir(repo)
        .output()?;
    match std::fs::remove_dir_all(path) {
        Ok(()) => Ok(TeardownOutcome::Removed),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(TeardownOutcome::Removed),
        Err(e) => Err(e),
    }
}

/// Whether a worktree holds uncommitted changes (`git status --porcelain`
/// non-empty). A git failure is treated as "dirty" (conservative — never delete
/// a checkout we cannot prove is clean).
fn is_dirty(workdir: &Path) -> io::Result<bool> {
    let out = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(workdir)
        .output()?;
    if !out.status.success() {
        return Ok(true);
    }
    Ok(!out.stdout.is_empty())
}

/// Run a `git` subcommand in `cwd`, erroring on a non-zero exit.
fn run_git(cwd: &Path, args: &[&str]) -> io::Result<()> {
    let out = Command::new("git").args(args).current_dir(cwd).output()?;
    if out.status.success() {
        return Ok(());
    }
    Err(io::Error::other(format!(
        "git {args:?} in {} failed ({}): {}",
        cwd.display(),
        out.status,
        String::from_utf8_lossy(&out.stderr).trim()
    )))
}

/// The error used when a path is not valid UTF-8 (git args must be `&str`).
fn non_utf8_path() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, "path is not valid UTF-8")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Init a git repo at `dir` with one commit so `git worktree add -b` has a
    /// HEAD to branch from.
    fn init_repo_with_commit(dir: &Path) {
        std::fs::create_dir_all(dir).unwrap();
        run_git(dir, &["init", "--quiet"]).unwrap();
        run_git(dir, &["config", "user.email", "t@e.com"]).unwrap();
        run_git(dir, &["config", "user.name", "t"]).unwrap();
        std::fs::write(dir.join("README.md"), "hi").unwrap();
        run_git(dir, &["add", "."]).unwrap();
        run_git(dir, &["commit", "--quiet", "-m", "init"]).unwrap();
    }

    /// A scratch ref git-inits an isolated repo under the home, idempotently.
    #[test]
    fn scratch_git_inits_idempotently() {
        let home = tempfile::tempdir().unwrap();
        let fallback = home.path().join("fallback");
        let a = provision(Some("scratch"), "card-1", home.path(), &fallback).unwrap();
        let RunWorkdir::Scratch { path } = &a else {
            panic!("expected scratch, got {a:?}");
        };
        assert!(path.join(".git").exists(), "scratch repo is git-inited");
        assert!(
            path.ends_with(".agents-in-a-box/scratch/card-1"),
            "scratch under ~/.agents-in-a-box/scratch/<slug>: {path:?}"
        );
        // A second provision is a no-op reuse (idempotent), not a re-init error.
        let b = provision(Some("scratch"), "card-1", home.path(), &fallback).unwrap();
        assert_eq!(a, b);
        // Teardown never removes a scratch repo.
        assert_eq!(teardown(&a).unwrap(), TeardownOutcome::NoOp);
        assert!(path.exists(), "scratch survives teardown");
    }

    /// A real repo provisions a worktree on branch `ainb/<slug>`.
    #[test]
    fn worktree_on_ainb_slug_branch() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        init_repo_with_commit(&repo);
        let home = tmp.path().join("home");
        let fallback = home.join("fallback");

        let wd = provision(
            Some(repo.to_str().unwrap()),
            "slug-a",
            &home,
            &fallback,
        )
        .unwrap();
        let RunWorkdir::Worktree { path, branch, .. } = &wd else {
            panic!("expected worktree, got {wd:?}");
        };
        assert_eq!(branch, "ainb/slug-a");
        assert!(path.join(".git").exists(), "worktree is a checkout");
        assert!(path.join("README.md").exists(), "the repo's tree is checked out");
        // The branch really exists in the repo.
        let branches = Command::new("git")
            .args(["branch", "--list", "ainb/slug-a"])
            .current_dir(&repo)
            .output()
            .unwrap();
        assert!(
            String::from_utf8_lossy(&branches.stdout).contains("ainb/slug-a"),
            "the ainb/<slug> branch exists in the origin repo"
        );
    }

    /// Two cards on the SAME repo provision two DISTINCT worktrees + branches —
    /// the F5 "N tasks on one repo never collide" guarantee.
    #[test]
    fn concurrent_same_repo_distinct_worktrees() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        init_repo_with_commit(&repo);
        let home = tmp.path().join("home");
        let fallback = home.join("fallback");
        let repo_ref = repo.to_str().unwrap();

        let a = provision(Some(repo_ref), "slug-a", &home, &fallback).unwrap();
        let b = provision(Some(repo_ref), "slug-b", &home, &fallback).unwrap();
        assert_ne!(a.path(), b.path(), "distinct worktree dirs");
        let (RunWorkdir::Worktree { branch: ba, .. }, RunWorkdir::Worktree { branch: bb, .. }) =
            (&a, &b)
        else {
            panic!("both must be worktrees");
        };
        assert_eq!(ba, "ainb/slug-a");
        assert_eq!(bb, "ainb/slug-b");
        assert!(a.path().exists() && b.path().exists());
    }

    /// A clean worktree is removed + deregistered on teardown.
    #[test]
    fn teardown_removes_clean_worktree() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        init_repo_with_commit(&repo);
        let home = tmp.path().join("home");
        let fallback = home.join("fallback");

        let wd = provision(Some(repo.to_str().unwrap()), "clean", &home, &fallback).unwrap();
        let path = wd.path().to_path_buf();
        assert_eq!(teardown(&wd).unwrap(), TeardownOutcome::Removed);
        assert!(!path.exists(), "clean worktree removed");
        // git no longer lists it.
        let listed = Command::new("git")
            .args(["worktree", "list"])
            .current_dir(&repo)
            .output()
            .unwrap();
        assert!(
            !String::from_utf8_lossy(&listed.stdout).contains("clean"),
            "the worktree is deregistered from the origin repo"
        );
    }

    /// A DIRTY worktree is KEPT on teardown (uncommitted work preserved).
    #[test]
    fn teardown_keeps_dirty_worktree() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        init_repo_with_commit(&repo);
        let home = tmp.path().join("home");
        let fallback = home.join("fallback");

        let wd = provision(Some(repo.to_str().unwrap()), "dirty", &home, &fallback).unwrap();
        // Leave an uncommitted change in the worktree.
        std::fs::write(wd.path().join("new.txt"), "work in progress").unwrap();
        assert_eq!(teardown(&wd).unwrap(), TeardownOutcome::KeptDirty);
        assert!(wd.path().exists(), "a dirty worktree is kept, not deleted");
        assert!(wd.path().join("new.txt").exists(), "the uncommitted work survives");
    }

    /// No repo_ref → the fallback execenv workdir, no git, never torn down.
    #[test]
    fn no_repo_uses_fallback() {
        let home = tempfile::tempdir().unwrap();
        let fallback = home.path().join("execenv-workdir");
        let wd = provision(None, "chat", home.path(), &fallback).unwrap();
        assert_eq!(wd, RunWorkdir::Fallback { path: fallback.clone() });
        assert_eq!(wd.path(), fallback);
        assert_eq!(teardown(&wd).unwrap(), TeardownOutcome::NoOp);
        // A blank ref is treated the same as None.
        let blank = provision(Some("  "), "chat", home.path(), &fallback).unwrap();
        assert_eq!(blank, RunWorkdir::Fallback { path: fallback });
    }
}
