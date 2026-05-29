//! SyncPlanner — decide, per unit, which way `ainb skill sync` should
//! reconcile home ↔ repo state (spec §Phase D, bead v12.D.1).
//!
//! [`plan_sync`] is a **pure** function: it performs no I/O and consults
//! no globals, so identical inputs always yield an identical plan. The
//! impure work — hashing the on-disk home/repo files, reading their
//! mtimes, resolving the source repo's HEAD — is the caller's job and
//! arrives pre-computed as [`UnitSnapshot`]s. The [`SyncEngine`] (beads
//! v12.D.2/D.3) then *executes* the returned [`SyncAction`]s; this module
//! only plans.
//!
//! # Decision model
//!
//! For each unit the planner first checks **eligibility**: the unit's
//! repo-relative path must be covered by the source's effective folder
//! layout (the passed `mappings`, or the source's own `target_layout`,
//! falling back to the bootstrap defaults). An uncovered unit can't be
//! placed on either side, so it is a [`NoOp`].
//!
//! Eligible units are decided by **git refs when available**, else by
//! **mtime fallback**:
//!
//! ```text
//!                         ┌── deployed_sha + both content_sha present ──┐
//!   home == repo                         → NoOp   (already in sync)
//!   home != deployed, repo == deployed   → ToRepo (local edit to push)
//!   repo != deployed, home == deployed   → ToHome (upstream change)
//!   home != deployed, repo != deployed   → NoOp   (conflict — manual)
//!                         └── otherwise (no deployed_sha / no sha) ──────┘
//!   home.mtime > repo.mtime              → ToRepo
//!   repo.mtime > home.mtime              → ToHome
//!   equal / indeterminate                → NoOp
//! ```
//!
//! Presence asymmetry short-circuits the above: a unit present on only
//! one side syncs toward the missing side. Finally, a `ToRepo` against a
//! `read_only` source is downgraded to `NoOp` — externally-managed
//! sources never receive writes.
//!
//! [`NoOp`]: SyncDirection::NoOp
//! [`SyncEngine`]: https://github.com/stevengonsalvez/agents-in-a-box

use std::fs;
use std::path::{Path, PathBuf};

use crate::manifest::{SourceEntry, TargetMapping};
use crate::mapping::resolve_pair;

/// Which way a unit should be reconciled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncDirection {
    /// Copy the home file up into the source repo (publish a local edit).
    ToRepo,
    /// Copy the repo file down into the tool home (adopt an upstream change).
    ToHome,
    /// Leave both sides untouched.
    NoOp,
}

/// One planned reconciliation for a single unit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncAction {
    /// Unit this action concerns (the [`UnitSnapshot::unit_name`]).
    pub unit_name: String,
    /// Direction to sync, or [`SyncDirection::NoOp`].
    pub direction: SyncDirection,
    /// Human-readable explanation of why this direction was chosen — the
    /// CLI surfaces it under `--dry-run`.
    pub reason: String,
}

/// Observed state of one side (home or repo) of a unit.
///
/// `content_sha` is the file's content hash (e.g. a git blob SHA) when
/// the caller could compute it; it drives the precise git-ref comparison.
/// `mtime` (unix seconds) is the coarser signal used only when refs are
/// unavailable. Both are optional so a caller can supply whichever it
/// cheaply has.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SideSnapshot {
    /// Content hash of this side's file, if computed.
    pub content_sha: Option<String>,
    /// Modification time (unix seconds), for the mtime-fallback path.
    pub mtime: Option<i64>,
}

/// Pre-observed facts about one unit, gathered by the (impure) caller.
///
/// `deployed_sha` is the SHA last written to home, read from the
/// lockfile's `LockedUnit` (its `sha` field). `home` / `repo` are `None`
/// when the unit is absent on that side.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnitSnapshot {
    /// Logical unit name (e.g. `commit`).
    pub unit_name: String,
    /// Repo-relative path used to test layout eligibility (e.g.
    /// `skills/commit/SKILL.md`).
    pub unit_path: PathBuf,
    /// SHA last deployed to home, from `LockedUnit`. `None` when the unit
    /// was never locked — forces the mtime-fallback path.
    pub deployed_sha: Option<String>,
    /// Home-side snapshot; `None` when the unit is absent on home.
    pub home: Option<SideSnapshot>,
    /// Repo-side snapshot; `None` when the unit is absent in the repo.
    pub repo: Option<SideSnapshot>,
}

/// Plan how to reconcile every `unit` between its home and repo copies.
///
/// Pure: no filesystem, network, or global access. Returns exactly one
/// [`SyncAction`] per input unit, in input order.
///
/// `source` supplies the `read_only` flag (which suppresses `ToRepo`) and
/// its `target_layout` (the eligibility fallback when `mappings` is
/// empty). `mappings`, when non-empty, overrides the source's layout for
/// eligibility — letting callers scope a sync to a subset of globs.
pub fn plan_sync(
    source: &SourceEntry,
    mappings: &[TargetMapping],
    units: &[UnitSnapshot],
) -> Vec<SyncAction> {
    // Effective layout: explicit `mappings` win; otherwise the source's
    // own. `resolve_pair` itself falls back to the bootstrap defaults when
    // the chosen layout is empty, so legacy sources still resolve.
    let scoped;
    let layout_source: &SourceEntry = if mappings.is_empty() {
        source
    } else {
        scoped = SourceEntry {
            target_layout: mappings.to_vec(),
            ..source.clone()
        };
        &scoped
    };

    units.iter().map(|u| plan_unit(source, layout_source, u)).collect()
}

/// Decide the action for a single unit. `source` carries policy
/// (`read_only`); `layout_source` carries the effective `target_layout`
/// for the eligibility check.
fn plan_unit(source: &SourceEntry, layout_source: &SourceEntry, u: &UnitSnapshot) -> SyncAction {
    let act = |direction, reason: &str| SyncAction {
        unit_name: u.unit_name.clone(),
        direction,
        reason: reason.to_string(),
    };

    // Eligibility: the unit must map somewhere under the layout.
    if resolve_pair(layout_source, &u.unit_path).is_none() {
        return act(
            SyncDirection::NoOp,
            "no target_layout mapping covers this unit's path",
        );
    }

    let raw = decide_direction(u);

    // Read-only sources never receive writes; downgrade ToRepo.
    if raw.direction == SyncDirection::ToRepo && source.read_only {
        return act(
            SyncDirection::NoOp,
            "source is read_only (externally managed); refusing to push to repo",
        );
    }

    act(raw.direction, &raw.reason)
}

/// The core direction decision, before the read-only policy gate. Returns
/// a `(direction, reason)` pair (the unit name is attached by the caller).
struct Decision {
    direction: SyncDirection,
    reason: String,
}

fn decide_direction(u: &UnitSnapshot) -> Decision {
    let d = |direction, reason: &str| Decision {
        direction,
        reason: reason.to_string(),
    };

    match (&u.home, &u.repo) {
        // Present on neither side — nothing to do.
        (None, None) => d(SyncDirection::NoOp, "unit absent on both home and repo"),
        // Only on home — publish it upstream.
        (Some(_), None) => d(SyncDirection::ToRepo, "unit exists on home but not in repo"),
        // Only in repo — pull it down.
        (None, Some(_)) => d(
            SyncDirection::ToHome,
            "unit exists in repo but not deployed to home",
        ),
        // Present on both — compare.
        (Some(home), Some(repo)) => decide_both_present(u.deployed_sha.as_deref(), home, repo),
    }
}

/// Both sides present: prefer git-ref comparison, fall back to mtime.
fn decide_both_present(
    deployed_sha: Option<&str>,
    home: &SideSnapshot,
    repo: &SideSnapshot,
) -> Decision {
    let d = |direction, reason: &str| Decision {
        direction,
        reason: reason.to_string(),
    };

    // Git-ref path: needs a deployed_sha *and* a content_sha on both sides.
    if let (Some(deployed), Some(home_sha), Some(repo_sha)) = (
        deployed_sha,
        home.content_sha.as_deref(),
        repo.content_sha.as_deref(),
    ) {
        if home_sha == repo_sha {
            return d(SyncDirection::NoOp, "home and repo content identical");
        }
        let home_changed = home_sha != deployed;
        let repo_changed = repo_sha != deployed;
        return match (home_changed, repo_changed) {
            (true, false) => d(
                SyncDirection::ToRepo,
                "home modified since deploy; repo still at deployed sha",
            ),
            (false, true) => d(
                SyncDirection::ToHome,
                "repo updated upstream since deploy; home still at deployed sha",
            ),
            (true, true) => d(
                SyncDirection::NoOp,
                "conflict: home and repo both diverged from the deployed sha",
            ),
            // Both equal `deployed` yet differ from each other is
            // impossible; treated as already-synced for totality.
            (false, false) => d(SyncDirection::NoOp, "home and repo both at deployed sha"),
        };
    }

    // Mtime fallback: no usable refs.
    match (home.mtime, repo.mtime) {
        (Some(h), Some(r)) if h > r => d(
            SyncDirection::ToRepo,
            "no deployed sha; home newer by mtime",
        ),
        (Some(h), Some(r)) if r > h => d(
            SyncDirection::ToHome,
            "no deployed sha; repo newer by mtime",
        ),
        (Some(_), Some(_)) => d(SyncDirection::NoOp, "no deployed sha; equal mtime"),
        _ => d(
            SyncDirection::NoOp,
            "indeterminate: neither comparable shas nor mtimes on both sides",
        ),
    }
}

// ──────────────────────────────────────────────────────────────────────
// SyncEngine — TO_HOME path (bead v12.D.2).
//
// `plan_sync` decides *what* to do; `apply_to_home` *does* it for the
// upstream-pull direction. It is deliberately I/O-narrow:
//
//   1. Skip when the action's direction is not `ToHome` (so callers can
//      pass every action through the same loop without pre-filtering).
//   2. Resolve `(home_rel, repo_rel)` via [`resolve_pair`] — must
//      succeed (the action would never have been planned otherwise; we
//      re-check so misuse can't silently write to the wrong place).
//   3. Ask the [`ContentFetcher`] for the bytes at `repo_rel`@ref.
//   4. Write atomically (tmp + rename) under `tool_home/home_rel`,
//      creating parents as needed. Idempotent: re-applying the same
//      action with the same fetched bytes leaves the file at byte-for-
//      byte equal content.
//
// The fetcher dependency stays local to `sync` (rather than reusing
// `ainb-fetch::Fetcher`) so `ainb-skill-core` does not depend on
// `ainb-fetch`. The CLI / TUI layer wraps a real fetcher into this
// trait at call time.
// ──────────────────────────────────────────────────────────────────────

/// Errors raised by the sync executors.
#[derive(Debug, thiserror::Error)]
pub enum SyncEngineError {
    /// Could not resolve the unit's path under the source's effective
    /// folder layout — the caller's plan must have desynced from the
    /// layout. Carries the unit-relative path that failed.
    #[error("no target_layout mapping covers `{0}`")]
    LayoutNoMatch(String),

    /// Forwarded from the [`ContentFetcher`].
    #[error("fetcher error: {0}")]
    Fetch(#[from] FetchError),

    /// Filesystem I/O while writing the home file.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Errors raised by a [`ContentFetcher`] implementation.
#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    /// Generic, formatted upstream error. Implementations format their
    /// own context (URL, ref, status); the executor surfaces it through
    /// `SyncEngineError::Fetch`.
    #[error("{0}")]
    Other(String),
}

/// Minimal byte-level fetch contract used by [`apply_to_home`].
///
/// Implementations resolve `(ref_name, repo_path)` to the corresponding
/// file content. Path is repo-relative (forward-slash); ref is a git
/// branch/tag/SHA as it appears in the source's manifest entry.
pub trait ContentFetcher {
    fn fetch_content(
        &self,
        ref_name: &str,
        repo_path: &Path,
    ) -> std::result::Result<Vec<u8>, FetchError>;
}

/// Apply one [`SyncDirection::ToHome`] action: write the upstream file
/// bytes to the mapped home path under `tool_home`.
///
/// Non-`ToHome` actions short-circuit to `Ok(())` so callers can sweep
/// a full plan through one loop without dispatching on direction.
///
/// Idempotent: re-running with an unchanged upstream leaves the home
/// file byte-for-byte identical. Writes are atomic (write `*.tmp` next
/// to the target, rename into place) so an interrupted apply never
/// leaves a half-written home file.
///
/// `source` supplies the effective `target_layout` (via [`resolve_pair`])
/// and the git `ref` to pass into the fetcher. `unit_path` is the
/// repo-relative path of the unit being synced; the caller usually
/// gets it from the [`UnitSnapshot`] that fed `plan_sync`.
pub fn apply_to_home(
    action: &SyncAction,
    tool_home: &Path,
    source: &SourceEntry,
    unit_path: &Path,
    fetcher: &dyn ContentFetcher,
) -> std::result::Result<(), SyncEngineError> {
    if action.direction != SyncDirection::ToHome {
        return Ok(());
    }

    let (home_rel, repo_rel) = resolve_pair(source, unit_path)
        .ok_or_else(|| SyncEngineError::LayoutNoMatch(path_lossy(unit_path)))?;

    let bytes = fetcher.fetch_content(&source.r#ref, &repo_rel)?;
    let target = tool_home.join(&home_rel);
    write_atomic(&target, &bytes)?;
    Ok(())
}

/// Display-friendly `Path` → `String` that prefers forward-slash for
/// portability inside error messages.
fn path_lossy(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

/// Write `bytes` to `target` atomically: create parents, write to a
/// sibling `*.tmp`, fsync, then rename into place. Idempotent for
/// identical inputs.
fn write_atomic(target: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = target.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    let mut tmp = target.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    {
        let mut f = fs::File::create(&tmp)?;
        use std::io::Write;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, target)
}
