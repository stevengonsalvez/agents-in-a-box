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
//!  repo_ref = Some("scratch")        ──▶  <home>/.agents-in-a-box/scratch/<scratch_slug>
//!                                          `git init` (idempotent), run IN PLACE
//!                                          (already isolated — never torn down)
//!  repo_ref = None                   ──▶  the fallback execenv workdir (a chat /
//!                                          autopilot task — the pre-F5 behaviour)
//! ```
//!
//! # Two slugs: per-RUN worktree vs per-CARD scratch
//!
//! The `slug` is the caller's per-task key (the FULL task id, tcp vpm). Because it
//! is unique per task, two cards launched on the SAME repo provision two DISTINCT
//! worktrees on two DISTINCT `ainb/<slug>` branches — they never collide (the F5
//! "N tasks on one repo never collide" guarantee, now airtight: no truncated prefix
//! can alias two ids).
//!
//! Scratch is different: it is the card's durable scratch space (F2 intent), so it
//! is keyed on the caller's `scratch_slug` — a per-(card, agent) key, STABLE across
//! reruns and DISTINCT per concurrent squad member. A per-run slug would hand every
//! rerun a fresh empty dir, silently losing the prior run's scratch work; the
//! (card, agent) slug reuses the same `scratch/<scratch_slug>` dir so a rerun
//! continues where the last run left off, while a squad fan-out — whose members
//! share the issue but hold distinct agents — still gets a dir per member and never
//! races two runs in one working tree.
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

use ainb_hangar_store::repo::task::TaskRepo;
use sqlx::SqlitePool;

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
/// - `Some("scratch")` → `<home>/.agents-in-a-box/scratch/<scratch_slug>`,
///   `git init`ed idempotently, run in place. Keyed on the per-CARD `scratch_slug`
///   (stable across reruns), NOT the per-run `slug`.
/// - `Some(path)` naming a git repo → a fresh worktree under
///   `<home>/.agents-in-a-box/worktrees/<slug>` on branch `ainb/<slug>`.
/// - `None` (or a blank ref) → the `fallback` execenv workdir, untouched.
///
/// Idempotent on resume: a worktree whose path already exists (a recovered task)
/// is reused rather than re-added; a scratch dir already `git init`ed is left as
/// is — and a scratch RERUN reuses the same `scratch_slug` dir, so the card's
/// durable scratch work carries across runs.
///
/// # Errors
///
/// Returns an [`io::Error`] if a directory cannot be created, `git init` /
/// `git worktree add` fails, or a worktree `repo_ref` is not a valid UTF-8 path.
pub fn provision(
    repo_ref: Option<&str>,
    slug: &str,
    scratch_slug: &str,
    home: &Path,
    fallback: &Path,
) -> io::Result<RunWorkdir> {
    let repo_ref = repo_ref.map(str::trim).filter(|s| !s.is_empty());
    match repo_ref {
        None => Ok(RunWorkdir::Fallback {
            path: fallback.to_path_buf(),
        }),
        Some("scratch") => provision_scratch(scratch_slug, home),
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

/// Whether the run's worktree branch carries commits its base does not — the
/// "did this run produce a durable artifact?" signal the card's branch surfacing
/// keys on (tcp T2). Returns the count (used only as `> 0`).
///
/// For a [`RunWorkdir::Worktree`] this is `git rev-list --count HEAD..ainb/<slug>`
/// against the ORIGIN repo: the commits reachable from the branch but NOT from the
/// origin's `HEAD` — exactly the agent's private commits. This is correct for a
/// fresh worktree (HEAD is the fork point), a resumed one, and an origin whose
/// HEAD advanced during the run (the agent's commits are never on HEAD, so they
/// are never miscounted to zero); an unrelated-history origin merely OVER-counts,
/// which is harmless for the `> 0` decision. It needs no stored base SHA — so it
/// works for a recovered task with no provisioning state — and no `merge-base`
/// (so there is no "no common ancestor" failure path that could drop a real
/// branch). The branch ref lives in the shared repository, so the query works
/// whether or not the worktree checkout has been torn down. Scratch + fallback
/// runs never produce a surfaced branch and return `0`.
///
/// # Errors
///
/// Returns an [`io::Error`] if the `git` invocation fails to spawn. A non-zero
/// `git` exit degrades to `0` (no branch surfaced) rather than erroring, so a
/// finalize is never blocked.
pub fn commits_ahead(wd: &RunWorkdir) -> io::Result<u64> {
    let RunWorkdir::Worktree { branch, repo, .. } = wd else {
        return Ok(0);
    };
    let range = format!("HEAD..{branch}");
    let Some(count) = git_capture(repo, &["rev-list", "--count", &range])? else {
        return Ok(0);
    };
    Ok(count.trim().parse::<u64>().unwrap_or(0))
}

/// Run a `git` subcommand in `cwd`, returning its trimmed stdout on success, or
/// `None` on a non-zero exit (a tolerated "could not determine" outcome the
/// caller degrades from). Errors only if the process fails to spawn.
fn git_capture(cwd: &Path, args: &[&str]) -> io::Result<Option<String>> {
    let out = Command::new("git").args(args).current_dir(cwd).output()?;
    if out.status.success() {
        Ok(Some(String::from_utf8_lossy(&out.stdout).into_owned()))
    } else {
        Ok(None)
    }
}

/// The tally of one [`sweep_orphan_worktrees`] pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WorktreeGcReport {
    /// Worktrees removed + deregistered this pass (task terminal AND tree clean).
    pub removed: u64,
    /// Worktrees KEPT because their tree is dirty (uncommitted agent work preserved).
    pub kept_dirty: u64,
    /// Worktrees left alone because the task is still active, unknown, or its status
    /// could not be read (never delete on uncertainty).
    pub kept_active: u64,
}

/// Reclaim orphaned task worktrees under `<home>/.agents-in-a-box/worktrees/` (tcp
/// yjj): a Ctrl-C mid-run or a crash between finalize and [`teardown`] leaves the
/// run's git worktree on disk even though its task row is terminal. This is the
/// scheduled backstop that sweeps them.
///
/// Each `worktrees/<slug>` dir is named after its run's FULL task id (tcp vpm), so
/// the sweep resolves the task by that id and, per worktree:
///   - **remove + deregister** it (mirroring [`teardown`]) when the task is TERMINAL
///     (`done` / `failed` / `cancelled`) AND the checkout is CLEAN;
///   - **keep** it when the checkout is DIRTY (uncommitted agent work is never
///     silently dropped — the same keep-if-dirty guarantee [`teardown`] gives);
///   - **keep** it when the task is still active, unknown (a legacy-slug or
///     hand-made dir), or its status could not be read — the sweep never deletes a
///     worktree it cannot prove is a finished run's leak;
///   - **keep** it when the task still has a LIVE registered run in this process
///     ([`CancelRegistry::is_live`]) — a manual board transition can terminal-mark
///     a task whose provider is still executing in the checkout, and deleting a
///     live run's cwd would corrupt it (codex F4).
///
/// Removal deregisters the worktree from its origin repo (`git worktree prune`
/// against the resolved shared git dir) so no stale registration lingers.
///
/// [`CancelRegistry::is_live`]: crate::cancel::CancelRegistry::is_live
///
/// # Errors
///
/// Returns an [`io::Error`] only if the `worktrees/` tree is present but unreadable,
/// a dirtiness probe cannot spawn `git`, or a clean worktree's removal fails — the
/// scheduler logs and swallows it (a GC pass must never down the daemon).
pub async fn sweep_orphan_worktrees(
    pool: &SqlitePool,
    home: &Path,
) -> io::Result<WorktreeGcReport> {
    let root = ainb_root(home).join("worktrees");
    let mut report = WorktreeGcReport::default();

    // A daemon that never provisioned a worktree has no `worktrees/` dir — a no-op.
    let entries = match std::fs::read_dir(&root) {
        Ok(rd) => rd,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(report),
        Err(e) => return Err(e),
    };
    let mut worktrees: Vec<PathBuf> = Vec::new();
    for entry in entries {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            worktrees.push(entry.path());
        }
    }
    worktrees.sort();

    for wt in worktrees {
        let Some(slug) = wt.file_name().and_then(|s| s.to_str()) else {
            report.kept_active += 1;
            continue;
        };
        let terminal = match TaskRepo::get_by_id(pool, slug).await {
            Ok(Some(task)) => is_terminal_status(&task.status),
            // Unknown id (legacy-slug / hand-made dir) or a store fault → never
            // delete on uncertainty; leave it for a human or the next pass.
            Ok(None) => false,
            Err(e) => {
                tracing::warn!(error = %e, slug, "worktree gc: task lookup failed; keeping");
                false
            }
        };
        if !terminal {
            report.kept_active += 1;
            continue;
        }
        // Liveness guard (codex F4): a manually terminal-marked task can still have
        // its provider running in this checkout; never delete a live run's cwd.
        if crate::cancel::registry().is_live(slug) {
            report.kept_active += 1;
            continue;
        }
        if is_dirty(&wt)? {
            report.kept_dirty += 1;
            continue;
        }
        remove_and_deregister_worktree(&wt)?;
        report.removed += 1;
    }
    Ok(report)
}

/// Whether a task status token is terminal (`done` / `failed` / `cancelled`), the
/// set the DB `CHECK` constraint pairs with the active `queued`/`dispatched`/
/// `running`. A terminal task's worktree is a teardown-eligible leak.
fn is_terminal_status(status: &str) -> bool {
    matches!(status, "done" | "failed" | "cancelled")
}

/// Remove a clean orphan worktree and prune its origin registration.
///
/// Resolves the origin's shared git dir BEFORE deleting the checkout (so the prune
/// can still find it), removes the tree, then best-effort `git worktree prune`s the
/// now-stale admin entry. A prune failure leaves only a harmless dangling
/// registration, never an error.
fn remove_and_deregister_worktree(worktree: &Path) -> io::Result<()> {
    let common = worktree_common_git_dir(worktree);
    match std::fs::remove_dir_all(worktree) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    if let Some(gitdir) = common.as_ref().and_then(|p| p.to_str()) {
        let _ = Command::new("git").args(["--git-dir", gitdir, "worktree", "prune"]).output()?;
    }
    Ok(())
}

/// Resolve the shared (origin) git dir of a linked worktree — `<origin>/.git` — so
/// its registration can be pruned after the checkout is removed. `None` when the dir
/// is not a git worktree or `git` cannot resolve it.
fn worktree_common_git_dir(worktree: &Path) -> Option<PathBuf> {
    let out = Command::new("git")
        .args(["rev-parse", "--git-common-dir"])
        .current_dir(worktree)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let raw = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if raw.is_empty() {
        return None;
    }
    let p = Path::new(&raw);
    let abs = if p.is_absolute() {
        p.to_path_buf()
    } else {
        worktree.join(p)
    };
    std::fs::canonicalize(&abs).ok().or(Some(abs))
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

    /// A scratch ref git-inits an isolated repo under the home, idempotently. The
    /// dir is keyed on the per-CARD `scratch_slug`, NOT the per-run slug.
    #[test]
    fn scratch_git_inits_idempotently() {
        let home = tempfile::tempdir().unwrap();
        let fallback = home.path().join("fallback");
        let a = provision(Some("scratch"), "run-1", "card-1", home.path(), &fallback).unwrap();
        let RunWorkdir::Scratch { path } = &a else {
            panic!("expected scratch, got {a:?}");
        };
        assert!(path.join(".git").exists(), "scratch repo is git-inited");
        assert!(
            path.ends_with(".agents-in-a-box/scratch/card-1"),
            "scratch under ~/.agents-in-a-box/scratch/<scratch_slug>: {path:?}"
        );
        // A second provision is a no-op reuse (idempotent), not a re-init error.
        let b = provision(Some("scratch"), "run-1", "card-1", home.path(), &fallback).unwrap();
        assert_eq!(a, b);
        // Teardown never removes a scratch repo.
        assert_eq!(teardown(&a).unwrap(), TeardownOutcome::NoOp);
        assert!(path.exists(), "scratch survives teardown");
    }

    /// A scratch RERUN reuses the SAME dir across a fresh per-run slug, so the
    /// card's durable scratch work is not lost (F2 intent). The prior run's file
    /// survives into the next run's provisioned dir.
    #[test]
    fn scratch_reuses_the_card_dir_across_reruns() {
        let home = tempfile::tempdir().unwrap();
        let fallback = home.path().join("fallback");

        // Run 1 of card-x provisions scratch and leaves a file behind.
        let first = provision(Some("scratch"), "run-1", "card-x", home.path(), &fallback).unwrap();
        std::fs::write(first.path().join("notes.txt"), "prior scratch work").unwrap();

        // Run 2 of the SAME card (a DIFFERENT per-run slug) reuses the same dir.
        let second = provision(Some("scratch"), "run-2", "card-x", home.path(), &fallback).unwrap();
        assert_eq!(
            first.path(),
            second.path(),
            "a rerun reuses the card's scratch dir"
        );
        assert_eq!(
            std::fs::read_to_string(second.path().join("notes.txt")).unwrap(),
            "prior scratch work",
            "the prior run's scratch work survives into the rerun"
        );
    }

    /// DISTINCT scratch slugs (e.g. two squad members on one issue, keyed
    /// (issue, agent)) provision DISTINCT dirs, so parallel members never race in a
    /// shared working tree — the isolation half of the reuse guarantee.
    #[test]
    fn scratch_isolates_distinct_slugs() {
        let home = tempfile::tempdir().unwrap();
        let fallback = home.path().join("fallback");
        let a = provision(
            Some("scratch"),
            "run-a",
            "card-x-agentA",
            home.path(),
            &fallback,
        )
        .unwrap();
        let b = provision(
            Some("scratch"),
            "run-b",
            "card-x-agentB",
            home.path(),
            &fallback,
        )
        .unwrap();
        assert_ne!(
            a.path(),
            b.path(),
            "distinct scratch slugs get distinct dirs"
        );
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
        assert!(
            path.join("README.md").exists(),
            "the repo's tree is checked out"
        );
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

        let a = provision(Some(repo_ref), "slug-a", "slug-a", &home, &fallback).unwrap();
        let b = provision(Some(repo_ref), "slug-b", "slug-b", &home, &fallback).unwrap();
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

        let wd = provision(
            Some(repo.to_str().unwrap()),
            "clean",
            "clean",
            &home,
            &fallback,
        )
        .unwrap();
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

        let wd = provision(
            Some(repo.to_str().unwrap()),
            "dirty",
            "dirty",
            &home,
            &fallback,
        )
        .unwrap();
        // Leave an uncommitted change in the worktree.
        std::fs::write(wd.path().join("new.txt"), "work in progress").unwrap();
        assert_eq!(teardown(&wd).unwrap(), TeardownOutcome::KeptDirty);
        assert!(wd.path().exists(), "a dirty worktree is kept, not deleted");
        assert!(
            wd.path().join("new.txt").exists(),
            "the uncommitted work survives"
        );
    }

    /// A worktree run with NO commits reports 0 ahead; one that commits reports
    /// the exact commit count — the tcp T2 signal the card's branch surfacing
    /// keys on. Scratch + fallback always report 0 (never a surfaced branch).
    #[test]
    fn commits_ahead_counts_only_committed_work() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        init_repo_with_commit(&repo);
        let home = tmp.path().join("home");
        let fallback = home.join("fallback");

        // Fresh worktree, no agent commits yet → 0 ahead (nothing to surface).
        let wd = provision(
            Some(repo.to_str().unwrap()),
            "ahead",
            "ahead",
            &home,
            &fallback,
        )
        .unwrap();
        assert_eq!(commits_ahead(&wd).unwrap(), 0, "a no-commit run is 0 ahead");

        // The agent commits inside its worktree → the branch is ahead of its base.
        run_git(wd.path(), &["config", "user.email", "a@b.com"]).unwrap();
        run_git(wd.path(), &["config", "user.name", "a"]).unwrap();
        std::fs::write(wd.path().join("work.txt"), "agent output").unwrap();
        run_git(wd.path(), &["add", "."]).unwrap();
        run_git(wd.path(), &["commit", "--quiet", "-m", "agent work"]).unwrap();
        assert_eq!(
            commits_ahead(&wd).unwrap(),
            1,
            "one agent commit is 1 ahead"
        );

        // The origin's HEAD advancing during the run must NOT mask the agent's
        // commit (the count is HEAD..branch, and the agent's commit is never on
        // HEAD): the branch is still surfaced.
        std::fs::write(repo.join("origin.txt"), "origin moved").unwrap();
        run_git(&repo, &["add", "."]).unwrap();
        run_git(&repo, &["commit", "--quiet", "-m", "origin advances"]).unwrap();
        assert!(
            commits_ahead(&wd).unwrap() >= 1,
            "an origin HEAD that moved forward must not drop the run's branch"
        );

        // Scratch + fallback never surface a branch.
        let scratch = provision(Some("scratch"), "s", "s", &home, &fallback).unwrap();
        assert_eq!(commits_ahead(&scratch).unwrap(), 0);
        let fb = provision(None, "c", "c", &home, &fallback).unwrap();
        assert_eq!(commits_ahead(&fb).unwrap(), 0);
    }

    /// No repo_ref → the fallback execenv workdir, no git, never torn down.
    #[test]
    fn no_repo_uses_fallback() {
        let home = tempfile::tempdir().unwrap();
        let fallback = home.path().join("execenv-workdir");
        let wd = provision(None, "chat", "chat", home.path(), &fallback).unwrap();
        assert_eq!(
            wd,
            RunWorkdir::Fallback {
                path: fallback.clone()
            }
        );
        assert_eq!(wd.path(), fallback);
        assert_eq!(teardown(&wd).unwrap(), TeardownOutcome::NoOp);
        // A blank ref is treated the same as None.
        let blank = provision(Some("  "), "chat", "chat", home.path(), &fallback).unwrap();
        assert_eq!(blank, RunWorkdir::Fallback { path: fallback });
    }
}
