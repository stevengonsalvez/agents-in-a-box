// ABOUTME: Branch enumeration for the new-session base-branch picker.
//
// Two entry points:
//   * `list_repo_branches` — disk-only (git2): local heads + `refs/remotes/
//     origin/*`. Instant; safe to call on the event thread when the popup
//     opens (cached-first design — Stevie 2026-06-03 interview).
//   * `fetch_and_list` — `git fetch origin --prune` then re-list. Slow
//     (network); callers MUST run it on `spawn_blocking` and apply the result
//     via the tick-poll channel pattern (see `check_skills_load_complete`).
//
// Dedup rule: a remote branch `origin/x` is omitted when a local branch `x`
// exists — the local entry is the one offered (it's what a checkout would
// land on). Sections: remote first (default branch at the top), then local.

use std::path::Path;
use std::process::Command;

use git2::{BranchType, Repository};

/// One selectable row in the base-branch picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchEntry {
    /// Display ref — `origin/feature-x` for remote entries, `feature-x` for
    /// local ones. This is also the start-point handed to `git worktree add`.
    pub display: String,
    /// Local short name (`feature-x` for both kinds). The branch a
    /// checkout-direct selection would create / check out.
    pub short_name: String,
    /// True for `refs/remotes/origin/*` entries.
    pub is_remote: bool,
    /// True for the repo's default branch (origin/HEAD target, or main/master
    /// fallback). Sorted first within its section.
    pub is_default: bool,
}

/// List branches from on-disk refs only (no network). Remote section first
/// (default branch at the top, then alphabetical), then local branches that
/// don't shadow a remote entry's dedup. Returns an empty vec for non-repos.
#[must_use]
pub fn list_repo_branches(repo_path: &Path) -> Vec<BranchEntry> {
    let Ok(repo) = Repository::open(repo_path) else {
        return Vec::new();
    };

    let default_branch = default_branch_short_name(&repo);

    let mut locals: Vec<String> = Vec::new();
    if let Ok(branches) = repo.branches(Some(BranchType::Local)) {
        for (branch, _) in branches.flatten() {
            if let Ok(Some(name)) = branch.name() {
                locals.push(name.to_string());
            }
        }
    }

    let mut remotes: Vec<String> = Vec::new();
    if let Ok(branches) = repo.branches(Some(BranchType::Remote)) {
        for (branch, _) in branches.flatten() {
            if let Ok(Some(name)) = branch.name() {
                // Skip the symbolic `origin/HEAD` pointer — it duplicates the
                // default branch entry.
                if name.ends_with("/HEAD") {
                    continue;
                }
                remotes.push(name.to_string());
            }
        }
    }

    build_entries(remotes, locals, default_branch.as_deref())
}

/// `git fetch origin --prune`, then list. Blocking + network — run on
/// `spawn_blocking`. Fetch failure is non-fatal (offline-safe): the cached
/// refs are listed either way.
#[must_use]
pub fn fetch_and_list(repo_path: &Path) -> Vec<BranchEntry> {
    let _ = Command::new("git")
        .args(["fetch", "origin", "--prune"])
        .current_dir(repo_path)
        .output();
    list_repo_branches(repo_path)
}

/// Branch short names checked out in ANY worktree of `repo_path` — including
/// the repo's own main working copy. Parsed from `git worktree list
/// --porcelain`.
///
/// The ainb session list alone can't see the repo's own checkout (or a
/// manually-added worktree); without this, a checkout-direct pick of the
/// repo's current branch passes the picker guard and dies at
/// `git worktree add` with "already used by worktree" (review P1, PR #211).
#[must_use]
pub fn checked_out_branches(repo_path: &Path) -> Vec<String> {
    let Ok(output) = Command::new("git")
        .args(["worktree", "list", "--porcelain"])
        .current_dir(repo_path)
        .output()
    else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|l| l.strip_prefix("branch refs/heads/"))
        .map(str::to_string)
        .collect()
}

/// Every branch short name that would make `git worktree add` fail because it
/// is already checked out somewhere. Two sources, deduped:
///   1. `list_all_worktrees()` — branches in ainb's own by-session worktrees
///      (scans the by-session symlinks → real git branch), across ALL repos.
///   2. `checked_out_branches(repo_path)` — the repo's own main checkout plus
///      any manually-added worktree, which the ainb session list can't see.
///
/// This is the single source of truth for the Configure screen's collision
/// guard (the inline "⚠ in use" marker + the pre-launch block). Both the
/// repo-picker entry and the restart entry MUST build `existing_branches` from
/// here — using the legacy `list_worktrees()` alone silently misses every
/// modern by-name worktree, so an in-use branch slips the guard and dies at
/// `git worktree add` (Stevie 2026-06-06: feat/ota re-launch slipped through).
///
/// `repo_path` is `None` ONLY for non-local sources (e.g. an SSH session),
/// which have no local checkout to scan. For any `LocalPath` repo pass
/// `Some(path)` — passing `None` there would silently drop the repo's own
/// checkout + manual worktrees from the guard.
#[must_use]
pub fn in_use_branch_names(repo_path: Option<&Path>) -> Vec<String> {
    use std::collections::HashSet;

    use super::worktree_manager::WorktreeManager;

    // A set: the only consumers (`branch_collision`, the picker `in_use`
    // mark, `derive_branch_name`'s collision-avoidance) do membership checks,
    // never ordered display — so dedup is free and `main`/`master` appearing
    // across many sessions collapses to one entry.
    let mut names: HashSet<String> = WorktreeManager::new()
        .ok()
        .and_then(|m| m.list_all_worktrees().ok())
        .map(|infos| infos.into_iter().map(|(_, i)| i.branch_name).collect())
        .unwrap_or_default();

    if let Some(path) = repo_path {
        for b in checked_out_branches(path) {
            names.insert(b);
        }
    }

    names.into_iter().collect()
}

/// Pure assembly: dedup remote-vs-local, tag the default, sort sections.
/// Split out for testability without a fixture repo.
fn build_entries(
    remotes: Vec<String>,
    locals: Vec<String>,
    default_branch: Option<&str>,
) -> Vec<BranchEntry> {
    let mut entries: Vec<BranchEntry> = Vec::new();

    for remote in remotes {
        let short = remote.split_once('/').map_or(remote.as_str(), |(_, s)| s);
        // Dedup: a local branch with the same short name shadows the remote
        // entry (checkout would land on the local branch anyway).
        if locals.iter().any(|l| l == short) {
            continue;
        }
        entries.push(BranchEntry {
            display: remote.clone(),
            short_name: short.to_string(),
            is_remote: true,
            is_default: Some(short) == default_branch,
        });
    }

    for local in locals {
        entries.push(BranchEntry {
            is_default: Some(local.as_str()) == default_branch,
            display: local.clone(),
            short_name: local,
            is_remote: false,
        });
    }

    // Remote section first; default branch tops its section; alphabetical
    // within. `sort_by` is stable so equal keys keep insertion order.
    entries.sort_by(|a, b| {
        b.is_remote
            .cmp(&a.is_remote)
            .then(b.is_default.cmp(&a.is_default))
            .then(a.display.cmp(&b.display))
    });
    entries
}

/// Resolve the default branch's SHORT name (`main`, not `origin/main`).
/// Ladder: `origin/HEAD` symbolic-ref → local `main` → local `master`.
fn default_branch_short_name(repo: &Repository) -> Option<String> {
    if let Ok(reference) = repo.find_reference("refs/remotes/origin/HEAD") {
        if let Some(target) = reference.symbolic_target() {
            if let Some(short) = target.strip_prefix("refs/remotes/origin/") {
                return Some(short.to_string());
            }
        }
    }
    for cand in ["main", "master"] {
        if repo.find_branch(cand, BranchType::Local).is_ok() {
            return Some(cand.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(entries: &[BranchEntry]) -> Vec<&str> {
        entries.iter().map(|e| e.display.as_str()).collect()
    }

    #[test]
    fn remote_section_first_default_on_top() {
        let entries = build_entries(
            vec![
                "origin/zeta".into(),
                "origin/main".into(),
                "origin/alpha".into(),
            ],
            vec!["local-only".into()],
            Some("main"),
        );
        assert_eq!(
            names(&entries),
            vec!["origin/main", "origin/alpha", "origin/zeta", "local-only"]
        );
        assert!(entries[0].is_default);
        assert!(!entries[0].short_name.contains('/'));
        assert_eq!(entries[0].short_name, "main");
    }

    #[test]
    fn local_branch_shadows_remote_counterpart() {
        let entries = build_entries(
            vec!["origin/feature-x".into(), "origin/main".into()],
            vec!["feature-x".into()],
            Some("main"),
        );
        // origin/feature-x deduped away; local feature-x remains.
        assert_eq!(names(&entries), vec!["origin/main", "feature-x"]);
        assert!(!entries[1].is_remote);
    }

    #[test]
    fn default_tag_applies_to_local_when_no_remote() {
        let entries = build_entries(vec![], vec!["dev".into(), "main".into()], Some("main"));
        assert_eq!(names(&entries), vec!["main", "dev"]);
        assert!(entries[0].is_default);
    }

    #[test]
    fn nested_branch_names_keep_full_short_name() {
        let entries = build_entries(vec!["origin/fix/login".into()], vec![], None);
        assert_eq!(entries[0].short_name, "fix/login");
        assert_eq!(entries[0].display, "origin/fix/login");
    }

    #[test]
    fn list_repo_branches_empty_for_non_repo() {
        let tmp = tempfile::TempDir::new().unwrap();
        assert!(list_repo_branches(tmp.path()).is_empty());
    }

    #[test]
    fn checked_out_branches_empty_for_non_repo() {
        let tmp = tempfile::TempDir::new().unwrap();
        assert!(checked_out_branches(tmp.path()).is_empty());
    }

    #[test]
    fn checked_out_branches_sees_main_checkout_and_added_worktrees() {
        // Real repo: the main checkout's branch must be reported even with
        // zero ainb-managed worktrees — that's the review-P1 regression
        // (checkout-direct pick of the repo's current branch must be
        // markable as in-use).
        let tmp = tempfile::TempDir::new().unwrap();
        let repo_path = tmp.path().join("repo");
        std::fs::create_dir(&repo_path).unwrap();
        let git = |args: &[&str], cwd: &Path| {
            let out = Command::new("git").args(args).current_dir(cwd).output().unwrap();
            assert!(
                out.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        git(&["init", "-b", "main"], &repo_path);
        git(&["config", "user.email", "t@t"], &repo_path);
        git(&["config", "user.name", "t"], &repo_path);
        git(&["commit", "--allow-empty", "-m", "init"], &repo_path);
        git(&["branch", "feature-x"], &repo_path);

        let checked = checked_out_branches(&repo_path);
        assert_eq!(checked, vec!["main".to_string()], "main checkout reported");

        // A second worktree's branch shows up too.
        let wt = tmp.path().join("wt");
        git(
            &["worktree", "add", wt.to_str().unwrap(), "feature-x"],
            &repo_path,
        );
        let checked = checked_out_branches(&repo_path);
        assert!(checked.contains(&"main".to_string()));
        assert!(checked.contains(&"feature-x".to_string()));
    }

    #[test]
    fn in_use_branch_names_includes_repo_checkout_branches() {
        // The collision guard's source of truth must include the repo's own
        // checked-out branches (main + any added worktree). The global
        // by-session worktree list is merged in too, so assert superset rather
        // than equality to stay independent of the host's ainb state
        // (Stevie 2026-06-06: feat/ota slipped a guard built from the legacy
        // list_worktrees() alone).
        let tmp = tempfile::TempDir::new().unwrap();
        let repo_path = tmp.path().join("repo");
        std::fs::create_dir(&repo_path).unwrap();
        let git = |args: &[&str], cwd: &Path| {
            let out = Command::new("git").args(args).current_dir(cwd).output().unwrap();
            assert!(
                out.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        git(&["init", "-b", "main"], &repo_path);
        git(&["config", "user.email", "t@t"], &repo_path);
        git(&["config", "user.name", "t"], &repo_path);
        git(&["commit", "--allow-empty", "-m", "init"], &repo_path);
        git(&["branch", "feature-x"], &repo_path);
        let wt = tmp.path().join("wt");
        git(
            &["worktree", "add", wt.to_str().unwrap(), "feature-x"],
            &repo_path,
        );

        let names = in_use_branch_names(Some(&repo_path));
        assert!(names.contains(&"main".to_string()), "repo checkout in list");
        assert!(
            names.contains(&"feature-x".to_string()),
            "added worktree branch in list"
        );
    }

    #[test]
    fn in_use_branch_names_none_skips_repo_scan() {
        // The SSH/non-local path: no `repo_path`, so the helper returns only
        // the global by-session worktree list and must NOT attempt a
        // `git worktree list` against any path. Host-independent: we only
        // assert it returns without panicking (the `if let Some` arm is
        // skipped entirely).
        let _ = in_use_branch_names(None);
    }
}
