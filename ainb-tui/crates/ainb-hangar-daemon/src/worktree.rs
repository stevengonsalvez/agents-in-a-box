//! Git-worktree integration for per-task working dirs (P1.6).
//!
//! Issue-driven tasks execute inside a dedicated `git worktree` so the agent's
//! checkout, branch, and dirty state are isolated from every other task and from
//! the shared repo cache. Hangar shells out to the real `git` binary (simpler
//! and truer to the reference's behaviour than a `git2` re-implementation, and the CI
//! matrix + ainb-tui already require `git`).
//!
//! The branch is `hangar/task/{task_id}` — keyed on the FULL task id (tcp vpm) so a
//! human (or `git worktree list`) can map a checkout back to its task at a glance,
//! with no truncated-slug collision risk.
//!
//! # Chat tasks
//!
//! A task with **no** `issue_id` is a chat / autopilot task. The reference leaves its
//! `workdir/` empty until the agent explicitly calls its `repo checkout`, so
//! [`prepare_worktree`] performs no git invocation and returns [`None`]. The
//! [`crate::execenv::prepare_env`] step has already created the empty `workdir/`.
//!
//! # Resume
//!
//! A resumed task carries its previous `work_dir` in [`Task::work_dir`]. When set
//! and matching the planned workdir, [`prepare_worktree`] reuses the existing
//! checkout instead of issuing a second `git worktree add` (which would fail —
//! the path is already registered).

use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use ainb_hangar_store::repo::task::Task;

use crate::execenv::ExecEnv;

/// A registered git worktree for one task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Worktree {
    /// The checkout directory (equals [`ExecEnv::workdir`]).
    pub workdir: PathBuf,
    /// The branch checked out in the worktree (`hangar/task/{task_id}`).
    pub branch: String,
}

/// The branch name a task's worktree checks out — `hangar/task/{task_id}`, keyed on
/// the FULL task id (tcp vpm) so no truncated slug can hand two runs one branch.
#[must_use]
fn task_branch(task: &Task) -> String {
    format!("hangar/task/{}", task.id)
}

/// Prepare the git worktree for a task, or `None` for chat tasks.
///
/// For an issue-driven task this runs `git worktree add {workdir} -b
/// hangar/task/{task_id}` against `repo_cache`, creating the per-task branch and
/// registering the checkout. For a chat task (no `issue_id`) it performs no git
/// invocation and returns [`None`], leaving `env.workdir` empty.
///
/// Idempotent on resume: if [`Task::work_dir`] is already set to the planned
/// workdir (a recovered task), the existing checkout is reused — no second
/// `git worktree add`.
///
/// # Errors
///
/// Returns an [`io::Error`] if spawning `git` fails or `git worktree add`
/// returns non-zero.
pub fn prepare_worktree(
    task: &Task,
    env: &ExecEnv,
    repo_cache: &Path,
) -> io::Result<Option<Worktree>> {
    // Chat tasks never get a worktree; their workdir stays empty until the agent
    // checks something out itself (the reference's `repo checkout`).
    if task.issue_id.is_none() {
        return Ok(None);
    }

    let branch = task_branch(task);
    let wt = Worktree {
        workdir: env.workdir.clone(),
        branch,
    };

    // Resume: a recovered task already has its worktree on disk. Reuse it rather
    // than re-running `git worktree add` (which would fail — path registered).
    let resuming = task.work_dir.as_deref().is_some_and(|prior| Path::new(prior) == env.workdir);
    if resuming || env.workdir.join(".git").exists() {
        return Ok(Some(wt));
    }

    run_git(
        repo_cache,
        &[
            "worktree",
            "add",
            env.workdir.to_str().ok_or_else(non_utf8_path)?,
            "-b",
            &wt.branch,
        ],
    )?;

    Ok(Some(wt))
}

/// Deregister and delete a task's worktree.
///
/// Runs `git worktree remove --force {workdir}` against `repo_cache` so git
/// drops its registration, then removes any residue. Tolerant of an
/// already-removed worktree (idempotent cleanup).
///
/// # Errors
///
/// Returns an [`io::Error`] if spawning `git` fails.
pub fn cleanup_worktree(wt: &Worktree, repo_cache: &Path) -> io::Result<()> {
    let workdir = wt.workdir.to_str().ok_or_else(non_utf8_path)?;
    // `--force` discards uncommitted state; we are tearing the task down anyway.
    // A non-zero exit here (e.g. already removed) is tolerated — the goal is the
    // post-condition "this worktree is gone", which a prior removal satisfies.
    let _ = Command::new("git")
        .args(["worktree", "remove", "--force", workdir])
        .current_dir(repo_cache)
        .output()?;

    // Belt-and-braces: ensure the dir is gone even if git left residue.
    match std::fs::remove_dir_all(&wt.workdir) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Run a `git` subcommand in `cwd`, erroring on a non-zero exit.
fn run_git(cwd: &Path, args: &[&str]) -> io::Result<()> {
    let out = Command::new("git").args(args).current_dir(cwd).output()?;
    if out.status.success() {
        return Ok(());
    }
    Err(io::Error::other(format!(
        "git {args:?} failed ({}): {}",
        out.status,
        String::from_utf8_lossy(&out.stderr).trim()
    )))
}

/// Construct the error used when a workdir path is not valid UTF-8 (git args
/// must be passed as `&str`).
fn non_utf8_path() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "workdir path is not valid UTF-8",
    )
}
