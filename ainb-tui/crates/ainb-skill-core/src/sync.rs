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

use fs2::FileExt;

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

    /// Another `ainb skill sync` (in this process or another) already
    /// holds the per-source advisory lock at
    /// `<repo_cache_dir>/.ainb-sync.lock`. Carries the lock path so the
    /// caller can surface "sync in progress on <path>" rather than the
    /// inscrutable git-stderr error that would otherwise emerge from the
    /// underlying `.git/index.lock` race.
    #[error("sync already in progress (lock held at `{0}`)")]
    SyncInProgress(String),
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

// ──────────────────────────────────────────────────────────────────────
// SyncEngine — TO_REPO path (bead v12.D.3).
//
// Symmetric to `apply_to_home`: applies a `SyncDirection::ToRepo`
// action by copying the home-side file into the source repo's local
// clone (the `promote-cache` checkout owned by the CLI layer),
// `git add`-ing the file, committing with subject `sync: <unit-name>`,
// and pushing — unless `AINB_SYNC_SKIP_PUSH=1` is set, in which case
// the commit lands locally only (used by tests + dry-runs).
//
// The clone lifecycle (clone-or-pull, sanitised cache key, gh auth
// pre-flight) lives in the CLI orchestrator (D.4); this executor only
// drives copy + commit + push against an already-checked-out repo.
// That keeps the core crate free of process-level network policy.
//
// Idempotency: a re-apply with byte-identical home content surfaces
// "nothing to commit" from git, which the executor treats as success.
// HEAD stays at the prior commit; the manifest/lockfile bookkeeping
// the caller does on top sees no state change.
// ──────────────────────────────────────────────────────────────────────

/// Env var that suppresses `git push` after the local commit. Used by
/// integration tests (no upstream remote configured) and by future
/// CLI dry-run paths that want to exercise the commit logic without
/// hitting the network.
pub const SYNC_SKIP_PUSH_ENV: &str = "AINB_SYNC_SKIP_PUSH";

/// Path of the per-source advisory lock held for the duration of
/// [`apply_to_repo`], relative to `<repo_cache_dir>`. Lives inside the
/// repo's `.git/` so git treats it as metadata (never shows up in
/// `git status`, never gets accidentally staged or pushed) while still
/// being a stable inode shared by every `ainb skill sync` invocation
/// against the same promote-cache.
pub const SYNC_LOCK_FILENAME: &str = ".git/ainb-sync.lock";

/// RAII guard for the per-source advisory lock. Holding the guard keeps
/// the kernel lock alive; dropping it releases the lock by closing the
/// underlying file descriptor (fs2 does not separate unlock from close
/// in its `flock`-backed implementation, and we rely on Drop to release
/// rather than calling `unlock_*` explicitly so a panic inside
/// `apply_to_repo` still hands the lock back).
struct SyncLockGuard {
    _file: fs::File,
}

/// Acquire the per-source advisory lock at
/// `<repo_cache_dir>/.ainb-sync.lock`. Non-blocking: if another holder
/// already owns the lock, returns
/// [`SyncEngineError::SyncInProgress`] immediately rather than waiting
/// — the TUI must not freeze on a sister process's git operation.
fn acquire_sync_lock(repo_cache_dir: &Path) -> std::result::Result<SyncLockGuard, SyncEngineError> {
    let lock_path = repo_cache_dir.join(SYNC_LOCK_FILENAME);
    if let Some(parent) = lock_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)?;
    match FileExt::try_lock_exclusive(&file) {
        Ok(()) => Ok(SyncLockGuard { _file: file }),
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
            Err(SyncEngineError::SyncInProgress(lock_path.display().to_string()))
        }
        Err(e) => Err(SyncEngineError::Io(e)),
    }
}

/// Configuration for [`apply_to_repo`].
#[derive(Debug, Clone)]
pub struct ApplyToRepoOpts {
    /// Path to the already-cloned source repo checkout (typically
    /// `<AINB_HOME>/promote-cache/<sanitised-remote>/`). The CLI layer
    /// owns the clone-or-pull; we only mutate / commit / push from
    /// here.
    pub repo_cache_dir: PathBuf,
}

/// Apply one [`SyncDirection::ToRepo`] action: copy the home file into
/// the source repo cache, commit, push (unless `AINB_SYNC_SKIP_PUSH` is
/// set).
///
/// Non-`ToRepo` actions short-circuit to `Ok(())` so callers can sweep
/// a plan through one loop. Idempotent for byte-identical home content
/// — git's own "nothing to commit" is treated as success.
pub fn apply_to_repo(
    action: &SyncAction,
    tool_home: &Path,
    source: &SourceEntry,
    unit_path: &Path,
    opts: &ApplyToRepoOpts,
) -> std::result::Result<(), SyncEngineError> {
    if action.direction != SyncDirection::ToRepo {
        return Ok(());
    }

    // Acquire the per-source advisory lock BEFORE any cache mutation.
    // Two concurrent `ainb skill sync` invocations against the same
    // source would otherwise race on `.git/index.lock` — corruption-
    // safe (git protects itself) but UX-hostile, surfacing as
    // "another git process running" stderr noise. Holding `_lock` for
    // the lifetime of this call keeps the second caller out until the
    // first finishes its commit + push.
    let _lock = acquire_sync_lock(&opts.repo_cache_dir)?;

    let (home_rel, repo_rel) = resolve_pair(source, unit_path)
        .ok_or_else(|| SyncEngineError::LayoutNoMatch(path_lossy(unit_path)))?;

    // Read the home-side bytes — surface a clean error when the home
    // file is missing (caller's plan must have desynced from disk).
    let home_file = tool_home.join(&home_rel);
    let bytes = fs::read(&home_file).map_err(|e| {
        SyncEngineError::Io(std::io::Error::new(
            e.kind(),
            format!("read home file `{}`: {e}", home_file.display()),
        ))
    })?;

    // Copy into the repo cache (atomic write — same machinery as TO_HOME).
    let repo_target = opts.repo_cache_dir.join(&repo_rel);
    write_atomic(&repo_target, &bytes)?;

    // git add — path is repo-relative, forward-slash regardless of host.
    let repo_rel_str = path_lossy(&repo_rel);
    let cache = &opts.repo_cache_dir;
    let out_add = git_capture(cache, &["add", "--", &repo_rel_str])?;
    require_success(&out_add, "git add")?;

    ensure_committer_identity(cache)?;

    // git commit. LC_ALL=C forces English diagnostics so the "nothing
    // to commit" fast-path works regardless of locale. We forbid
    // gpg-signing because the committer identity may be the
    // ainb-promote placeholder and a placeholder + GPG combo is never
    // useful.
    let commit_msg = format!("sync: {}", action.unit_name);
    let out_commit = git_with_env(
        cache,
        &["-c", "commit.gpgsign=false", "commit", "-m", &commit_msg],
        &[("LC_ALL", "C")],
    )?;
    if !out_commit.status.success() {
        let stderr = String::from_utf8_lossy(&out_commit.stderr);
        let stdout = String::from_utf8_lossy(&out_commit.stdout);
        // "nothing to commit" → idempotent re-apply; success.
        if stdout.contains("nothing to commit") || stderr.contains("nothing to commit") {
            return Ok(());
        }
        return Err(SyncEngineError::Io(std::io::Error::other(format!(
            "git commit failed: {} {}",
            stdout.trim(),
            stderr.trim()
        ))));
    }

    // git push — gated by env var so tests + dry-runs can stay offline.
    if std::env::var(SYNC_SKIP_PUSH_ENV).ok().as_deref() == Some("1") {
        return Ok(());
    }
    // Refuse argv-smuggled refspecs (e.g. `--force` or `--receive-pack=cmd`)
    // and stop option parsing with `--` before the positional values.
    if source.r#ref.starts_with('-') {
        return Err(SyncEngineError::Io(std::io::Error::other(format!(
            "refusing argv-smuggled ref: `{}`",
            source.r#ref
        ))));
    }
    let out_push = git_capture(cache, &["push", "--", "origin", &source.r#ref])?;
    require_success(&out_push, "git push")?;
    Ok(())
}

/// Run `git <args>` in `cwd`, capturing stdout + stderr. Bubbles up
/// io errors via `SyncEngineError::Io`.
///
/// `GIT_TERMINAL_PROMPT=0` + `GIT_ASKPASS=/bin/true` are forced so an
/// unreachable / private remote fails fast instead of freezing the
/// TUI on a terminal credential prompt — mirrors the drift backend's
/// hardening (commit f34d851).
fn git_capture(cwd: &Path, args: &[&str]) -> std::result::Result<std::process::Output, SyncEngineError> {
    std::process::Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ASKPASS", "/bin/true")
        .output()
        .map_err(SyncEngineError::Io)
}

/// Run `git <args>` in `cwd` with extra env vars set (e.g.
/// `LC_ALL=C`). Same capture shape as [`git_capture`], including the
/// no-prompt env block — explicit `envs` entries override these
/// defaults.
fn git_with_env(
    cwd: &Path,
    args: &[&str],
    envs: &[(&str, &str)],
) -> std::result::Result<std::process::Output, SyncEngineError> {
    let mut cmd = std::process::Command::new("git");
    cmd.args(args)
        .current_dir(cwd)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ASKPASS", "/bin/true");
    for (k, v) in envs {
        cmd.env(k, v);
    }
    cmd.output().map_err(SyncEngineError::Io)
}

/// Bail with a synthetic I/O error when a git subcommand exited
/// non-zero. Mirrors the helper in `ainb-cli::promote` so the executor
/// surfaces the same shape of error message ("git push failed: ...").
fn require_success(out: &std::process::Output, what: &str) -> std::result::Result<(), SyncEngineError> {
    if out.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    Err(SyncEngineError::Io(std::io::Error::other(format!(
        "{what} failed: {} {}",
        stdout.trim(),
        stderr.trim()
    ))))
}

/// Configure a placeholder committer identity on the cache repo only
/// when global git config does not already supply one. We never touch
/// the user's global config — only the cache repo's `.git/config`.
/// Mirrors `ainb-cli::promote::ensure_committer_identity` so hermetic
/// CI runs with no global identity still produce a clean commit.
fn ensure_committer_identity(cache: &Path) -> std::result::Result<(), SyncEngineError> {
    fn has_global(key: &str) -> bool {
        std::process::Command::new("git")
            .args(["config", "--get", key])
            .output()
            .map(|o| o.status.success() && !o.stdout.trim_ascii().is_empty())
            .unwrap_or(false)
    }
    let resolve = |key: &str, env_key: &str, fallback: &str| -> String {
        if has_global(key) {
            return String::new();
        }
        if let Ok(v) = std::env::var(env_key) {
            if !v.is_empty() {
                return v;
            }
        }
        fallback.to_string()
    };
    let email = resolve("user.email", "GIT_AUTHOR_EMAIL", "ainb-sync@example.invalid");
    let name = resolve("user.name", "GIT_AUTHOR_NAME", "ainb sync");
    // Refuse argv-smuggled identity values. Without the leading-dash
    // reject, a controlled `GIT_AUTHOR_EMAIL=--file=/tmp/evil` env
    // would let `git config` write the value to an attacker-chosen
    // file inside the cache repo.
    if email.starts_with('-') {
        return Err(SyncEngineError::Io(std::io::Error::other(format!(
            "refusing argv-smuggled user.email: `{email}`"
        ))));
    }
    if name.starts_with('-') {
        return Err(SyncEngineError::Io(std::io::Error::other(format!(
            "refusing argv-smuggled user.name: `{name}`"
        ))));
    }
    if !email.is_empty() {
        let out = git_capture(cache, &["config", "--", "user.email", &email])?;
        require_success(&out, "git config user.email")?;
    }
    if !name.is_empty() {
        let out = git_capture(cache, &["config", "--", "user.name", &name])?;
        require_success(&out, "git config user.name")?;
    }
    Ok(())
}
