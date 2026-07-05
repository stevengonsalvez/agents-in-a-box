//! `ainb skill promote <unit-name> --to <git-uri>` — push a `local:`
//! orphan into a git-backed source repo and rewrite the manifest URI
//! to point at the new remote.
//!
//! Spec: `.agents/goals/ainb-skill-manager-v1.1-discovery-spec.md`
//! §`ainb skill promote — Detailed Design`. Locked behaviours:
//!
//! 1. **Persistent clone cache** at
//!    `<AINB_HOME>/promote-cache/<sanitised-remote>/`. Cloned once,
//!    reused (`git pull --ff-only`) on subsequent runs.
//! 2. **Direct push to the default branch.** No PR creation; no
//!    feature branch. Default branch is resolved via `gh repo view`
//!    for `gh:` URIs or `git ls-remote --symref` for everything else.
//! 3. **Local files preserved.** `~/.<tool>/skills/<name>/` is left
//!    in place and registered as the deployed instance in the
//!    lockfile with sha256 `file_hashes`. Future `ainb skill update`
//!    pulls upstream, compares hashes, no-ops if unchanged.
//! 4. **`gh` is a hard requirement only for `gh:` URIs.** `file://`
//!    and other generic git URLs skip the `gh` pre-flight entirely
//!    (the test surface uses `file://` against a local bare repo).
//!
//! Pre-flight bails before any disk write whenever it can. Push
//! failures leave the local commit on disk in the cache but do
//! **not** rewrite the manifest, so a manual `cd <cache> && git
//! push` is a safe retry.
//!
//! **Network reachability is exercised during `--dry-run` too** —
//! default-branch resolution shells out to `gh repo view` (or
//! `git ls-remote --symref`) before the dry-run early exit so the
//! plan can show the resolved branch. Truly offline dry-runs require
//! `--to <uri>@<branch>` (explicit branch skips resolution).
//!
//! **Concurrent promotes** against the same `--to` repo are not
//! locked. Two parallel runs sharing `<AINB_HOME>/promote-cache/<key>/`
//! will race on the clone-or-pull → copy → commit → push sequence.
//! Run promotes serially; cache locking is a v1.1.1 follow-up.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command as ProcCommand, Output};

use anyhow::{Context, Result, anyhow, bail};

use ainb_adapters_tool::plan::hex_sha256;
use ainb_skill_core::lockfile::{DeployedRef, LockedUnit, Lockfile};
use ainb_skill_core::manifest::{Manifest, SourceEntry};
use ainb_skill_core::paths::{lockfile_path_in, manifest_path_in};
use ainb_skill_core::uri::{SourceType, Uri};

use crate::PromoteArgs;

/// Default tool name when the local URI's locator carries no
/// recognisable `.<tool>` segment.
const DEFAULT_TOOL: &str = "claude";

/// Adapter names mirrored from `ainb_adapters_tool` — used to map a
/// local URI like `~/.codex/skills` back to the tool slug we want in
/// the lockfile's deployed map.
const KNOWN_TOOLS: &[&str] = &[
    "claude",
    "codex",
    "copilot",
    "gemini",
    "cursor",
    "amazonq",
    "claude-desktop",
    "cline",
    "roo",
];

pub fn dispatch(home: &Path, args: PromoteArgs, out: &mut dyn io::Write) -> Result<()> {
    let manifest_path = manifest_path_in(home);
    let lockfile_path = lockfile_path_in(home);
    let mut manifest = Manifest::load_from(&manifest_path)?;

    // ── 1. Locate the unit in the manifest. ──────────────────────────
    let (unit_idx, old_uri) = find_unit(&manifest, &args.unit_name)?;

    if old_uri.source_type != SourceType::Local {
        bail!(
            "`{}` is already on a non-local source (`{}`). Promote only \
             rewrites `local:` orphans.",
            args.unit_name,
            old_uri.display()
        );
    }

    // ── 2. Parse `--to` into a remote target. ────────────────────────
    let target = RemoteTarget::parse(&args.to)?;

    // ── 3. Resolve the on-disk source dir (the unit's files). ────────
    let source_dir = resolve_source_dir(&old_uri, &args.unit_name)?;

    // ── 4. `gh` pre-flight for `gh:` URIs only. ──────────────────────
    if matches!(target.kind, TargetKind::Gh { .. }) {
        check_gh_installed()?;
        check_gh_auth()?;
    }

    // ── 5. Resolve default branch. ───────────────────────────────────
    let branch = resolve_default_branch(&target)?;

    // ── 6. Tool name inference (for the deployed-map key). ───────────
    let tool = infer_tool(&old_uri);

    // ── 7. Print plan + early exits. ─────────────────────────────────
    let new_uri = target.unit_uri(&branch, &args.unit_name);
    let new_source_uri = target.source_uri();
    writeln!(out, "# promote {}", args.unit_name)?;
    writeln!(out, "  from: {}", old_uri.display())?;
    writeln!(out, "  to:   {}", new_uri.display())?;
    writeln!(out, "  remote: {} (branch {})", target.git_url, branch)?;
    writeln!(out, "  files: {}", source_dir.display())?;
    writeln!(out, "  tool: {tool}")?;

    if args.dry_run {
        writeln!(
            out,
            "# dry-run: not cloning, committing, pushing, or rewriting"
        )?;
        return Ok(());
    }
    if !args.yes {
        bail!("interactive confirmation not wired — pass --yes (or --dry-run)");
    }

    // ── 8. Clone or pull the cache. ──────────────────────────────────
    let cache = clone_or_pull(home, &target)?;

    // ── 9. Copy the unit into <cache>/skills/<name>/. ────────────────
    let dest = cache.join("skills").join(&args.unit_name);
    copy_dir_recursive(&source_dir, &dest)
        .with_context(|| format!("copying `{}` -> `{}`", source_dir.display(), dest.display()))?;

    // ── 10. Commit. ──────────────────────────────────────────────────
    let message = args
        .message
        .clone()
        .unwrap_or_else(|| default_commit_message(&args.unit_name, &tool));
    git_add_and_commit(&cache, &args.unit_name, &message)?;

    // ── 11. Push (failure leaves commit local, no manifest write). ──
    git_push(&cache, &branch).with_context(
        || "push failed — manifest NOT rewritten; cd into the cache and retry `git push`",
    )?;

    // ── 12. Capture new sha + compute file hashes. ───────────────────
    let new_sha = git_rev_parse_head(&cache)?;
    let file_hashes = compute_file_hashes(&source_dir)?;

    // ── 13. Rewrite manifest + lockfile. ────────────────────────────
    manifest.units[unit_idx].uri = new_uri.display();
    add_source_if_absent(&mut manifest, &new_source_uri, &target, &branch);
    manifest.save_to(&manifest_path)?;

    let mut lockfile = Lockfile::load_from(&lockfile_path)?;
    let new_uri_str = new_uri.display();
    let old_uri_str = old_uri.display();
    lockfile.units.retain(|u| u.declared_uri != old_uri_str);
    let mut deployed: BTreeMap<String, DeployedRef> = BTreeMap::new();
    deployed.insert(
        tool.clone(),
        DeployedRef::Deployed {
            path: source_dir.to_string_lossy().to_string(),
            file_hashes,
        },
    );
    lockfile.units.push(LockedUnit {
        uri: new_uri_str.clone(),
        declared_uri: new_uri_str.clone(),
        kind: "skill".to_string(),
        sha: Some(new_sha.clone()),
        deployed,
        usage: Default::default(),
    });
    lockfile.save_to(&lockfile_path)?;

    writeln!(
        out,
        "\u{2713} promoted {} -> {} (sha {})",
        args.unit_name,
        new_uri_str,
        short_sha(&new_sha)
    )?;
    writeln!(
        out,
        "  Local files preserved at {} as deployed instance.",
        source_dir.display()
    )?;
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────
// Target parsing
// ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
enum TargetKind {
    /// `gh:user/repo` — production path. Uses real `gh` for auth +
    /// default-branch lookup.
    Gh { user: String, repo: String },
    /// `file:///<path>` — direct git URL. No `gh` involvement.
    File { path: PathBuf },
}

#[derive(Debug, Clone)]
struct RemoteTarget {
    kind: TargetKind,
    /// URL suitable for `git clone` / `git ls-remote`.
    git_url: String,
    /// Optional caller-supplied branch override (parsed from
    /// `--to <uri>@<branch>`).
    explicit_branch: Option<String>,
}

impl RemoteTarget {
    fn parse(input: &str) -> Result<Self> {
        if let Some(rest) = input.strip_prefix("gh:") {
            // `gh:user/repo[@branch]`. Locator must be `user/repo`.
            let (locator, branch) = match rest.split_once('@') {
                Some((l, b)) if !b.is_empty() => (l, Some(b.to_string())),
                Some(_) => bail!("`{input}`: empty branch after `@`"),
                None => (rest, None),
            };
            let (user, repo) = locator
                .split_once('/')
                .ok_or_else(|| anyhow!("`{input}` is not a valid `gh:user/repo` URI"))?;
            if user.is_empty() || repo.is_empty() {
                bail!("`{input}`: gh URI must be `gh:<user>/<repo>`");
            }
            let user = user.to_string();
            let repo = repo.to_string();
            Ok(RemoteTarget {
                git_url: format!("https://github.com/{user}/{repo}.git"),
                kind: TargetKind::Gh { user, repo },
                explicit_branch: branch,
            })
        } else if let Some(rest) = input.strip_prefix("file://") {
            // `file:///path[@branch]`. We split on the FIRST `@` after
            // the scheme stripper so a `/tmp/foo@bar` locator parses
            // cleanly.
            let (path_str, branch) = match rest.rsplit_once('@') {
                Some((p, b)) if !b.is_empty() && !b.contains('/') && !p.is_empty() => {
                    (p, Some(b.to_string()))
                }
                _ => (rest, None),
            };
            if path_str.is_empty() {
                bail!("`{input}`: empty path after `file://`");
            }
            Ok(RemoteTarget {
                git_url: format!("file://{path_str}"),
                kind: TargetKind::File {
                    path: PathBuf::from(path_str),
                },
                explicit_branch: branch,
            })
        } else {
            bail!(
                "`--to` must be `gh:user/repo[@branch]` or `file:///path[@branch]`, \
                 got `{input}`"
            )
        }
    }

    /// Build the source-level URI used as the manifest `SourceEntry.uri`.
    fn source_uri(&self) -> String {
        match &self.kind {
            TargetKind::Gh { user, repo } => format!("gh:{user}/{repo}"),
            TargetKind::File { path } => format!("git:file://{}", path.display()),
        }
    }

    /// Build the full unit URI for the manifest (`<source>@<branch>/skills/<name>`).
    fn unit_uri(&self, branch: &str, unit_name: &str) -> Uri {
        let (source_type, locator) = match &self.kind {
            TargetKind::Gh { user, repo } => (SourceType::Gh, format!("{user}/{repo}")),
            TargetKind::File { path } => (SourceType::Git, format!("file://{}", path.display())),
        };
        Uri {
            source_type,
            locator,
            ref_: Some(branch.to_string()),
            path: Some(format!("skills/{unit_name}")),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────
// Unit / manifest lookups
// ─────────────────────────────────────────────────────────────────────

fn find_unit(manifest: &Manifest, unit_name: &str) -> Result<(usize, Uri)> {
    validate_unit_name(unit_name)?;

    let mut matches: Vec<(usize, Uri)> = Vec::new();
    for (idx, entry) in manifest.units.iter().enumerate() {
        let Ok(uri) = Uri::parse(&entry.uri) else {
            continue;
        };
        if uri
            .path
            .as_deref()
            .map(|p| p == unit_name || p.ends_with(&format!("/{unit_name}")))
            .unwrap_or(false)
        {
            matches.push((idx, uri));
        }
    }

    match matches.len() {
        0 => bail!("no unit named `{unit_name}` in the manifest"),
        1 => Ok(matches.remove(0)),
        n => bail!(
            "ambiguous unit name `{unit_name}` — matched {n} units. Pass the full URI instead."
        ),
    }
}

/// Resolve the actual on-disk directory for a `local:` URI's unit.
/// Locator `~/.claude/skills@head/foo` -> `$HOME/.claude/skills/foo/`.
///
/// Defense in depth: after joining the unit name, canonicalises both
/// the locator and the result and verifies the result still lives
/// under the locator. Catches symlinks that redirect outside the
/// nominal source root and hand-edited manifests with traversal
/// segments embedded in the locator itself.
fn resolve_source_dir(uri: &Uri, unit_name: &str) -> Result<PathBuf> {
    let locator_raw = expand_home(&uri.locator)?;
    let locator = locator_raw.canonicalize().with_context(|| {
        format!(
            "locator `{}` cannot be canonicalised",
            locator_raw.display()
        )
    })?;

    let candidate = locator.join(unit_name);
    if !candidate.exists() {
        bail!(
            "source dir for `{unit_name}` does not exist: {}",
            candidate.display()
        );
    }
    let canonical = candidate
        .canonicalize()
        .with_context(|| format!("canonicalising `{}`", candidate.display()))?;
    if !canonical.is_dir() {
        bail!(
            "source path for `{unit_name}` is not a directory: {}",
            canonical.display()
        );
    }
    if !canonical.starts_with(&locator) {
        bail!(
            "source dir `{}` resolves outside the locator `{}` (symlink or traversal); refusing",
            canonical.display(),
            locator.display()
        );
    }
    Ok(canonical)
}

/// Reject empty / traversal / shell-meta unit names before any
/// path join. Mirrors the spec failure-mode row "Unit name has
/// reserved chars / not on disk — bail before clone".
fn validate_unit_name(unit_name: &str) -> Result<()> {
    if unit_name.is_empty() {
        bail!("unit name is empty");
    }
    if unit_name == "." || unit_name == ".." {
        bail!("`{unit_name}` is a reserved path traversal name");
    }
    for ch in unit_name.chars() {
        let ok = ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.');
        if !ok {
            bail!("`{unit_name}` contains reserved character `{ch}` (only [A-Za-z0-9._-] allowed)");
        }
    }
    if unit_name.starts_with('.') {
        bail!("`{unit_name}` must not begin with `.`");
    }
    Ok(())
}

/// Expand a leading `~/` against `$HOME`. Absolute paths and other
/// inputs pass through unchanged.
fn expand_home(s: &str) -> Result<PathBuf> {
    if let Some(rest) = s.strip_prefix("~/") {
        let home = std::env::var("HOME")
            .map_err(|_| anyhow!("$HOME is unset — cannot expand `~/{rest}`"))?;
        Ok(PathBuf::from(home).join(rest))
    } else if s == "~" {
        let home = std::env::var("HOME").map_err(|_| anyhow!("$HOME is unset"))?;
        Ok(PathBuf::from(home))
    } else {
        Ok(PathBuf::from(s))
    }
}

/// Pick the tool slug from a `local:` URI's locator path. Falls back
/// to `claude` when no `.<known-tool>` segment is present.
fn infer_tool(uri: &Uri) -> String {
    for seg in uri.locator.split('/') {
        if let Some(name) = seg.strip_prefix('.') {
            if KNOWN_TOOLS.contains(&name) {
                return name.to_string();
            }
        }
    }
    DEFAULT_TOOL.to_string()
}

// ─────────────────────────────────────────────────────────────────────
// gh / git shell-outs
// ─────────────────────────────────────────────────────────────────────

fn check_gh_installed() -> Result<()> {
    let out = ProcCommand::new("gh")
        .arg("--version")
        .output()
        .map_err(|_| anyhow!("gh CLI required. Install: brew install gh"))?;
    if !out.status.success() {
        bail!("gh CLI required. Install: brew install gh");
    }
    Ok(())
}

fn check_gh_auth() -> Result<()> {
    let out = ProcCommand::new("gh")
        .arg("auth")
        .arg("status")
        .output()
        .context("running `gh auth status`")?;
    if !out.status.success() {
        bail!("Authenticate: gh auth login");
    }
    Ok(())
}

fn resolve_default_branch(target: &RemoteTarget) -> Result<String> {
    if let Some(b) = &target.explicit_branch {
        return Ok(b.clone());
    }
    match &target.kind {
        TargetKind::Gh { user, repo } => {
            let out = ProcCommand::new("gh")
                .args([
                    "repo",
                    "view",
                    &format!("{user}/{repo}"),
                    "--json",
                    "defaultBranchRef",
                ])
                .output()
                .context("running `gh repo view`")?;
            if !out.status.success() {
                bail!(
                    "Cannot resolve repo `gh:{user}/{repo}`: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                );
            }
            let body: serde_json::Value =
                serde_json::from_slice(&out.stdout).context("parsing `gh repo view` JSON")?;
            body.get("defaultBranchRef")
                .and_then(|v| v.get("name"))
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .ok_or_else(|| anyhow!("`gh repo view` JSON missing defaultBranchRef.name"))
        }
        TargetKind::File { .. } => {
            // `git ls-remote --symref <url> HEAD` -> first line is
            // `ref: refs/heads/<branch>\tHEAD`. An empty repo with no
            // HEAD ref returns no `ref:` lines — bail explicitly so
            // the user gets a clean error instead of a downstream
            // "src refspec main does not match any" from `git push`.
            let out = ProcCommand::new("git")
                .args(["ls-remote", "--symref", &target.git_url, "HEAD"])
                .output()
                .context("running `git ls-remote --symref`")?;
            if !out.status.success() {
                bail!(
                    "Cannot reach git remote `{}`: {}",
                    target.git_url,
                    String::from_utf8_lossy(&out.stderr).trim()
                );
            }
            let stdout = String::from_utf8_lossy(&out.stdout);
            for line in stdout.lines() {
                if let Some(rest) = line.strip_prefix("ref: refs/heads/") {
                    if let Some((branch, _)) = rest.split_once('\t') {
                        return Ok(branch.to_string());
                    }
                    return Ok(rest.to_string());
                }
            }
            bail!(
                "remote `{}` has no HEAD ref — pass an explicit `@branch` in --to",
                target.git_url
            )
        }
    }
}

fn clone_or_pull(home: &Path, target: &RemoteTarget) -> Result<PathBuf> {
    let key = sanitise_cache_key(&target.git_url);
    let cache = home.join("promote-cache").join(&key);
    if cache.join(".git").is_dir() {
        let out = ProcCommand::new("git")
            .arg("-C")
            .arg(&cache)
            .args(["pull", "--ff-only"])
            .output()
            .context("running `git pull --ff-only`")?;
        require_success(&out, "git pull --ff-only")?;
    } else {
        if let Some(parent) = cache.parent() {
            fs::create_dir_all(parent)?;
        }
        // Refuse argv-smuggled URLs (a leading `-` would flow through
        // as an option, e.g. `--upload-pack=...`) and stop option
        // parsing with an explicit `--` before the positional values.
        if target.git_url.starts_with('-') {
            bail!("refusing remote URL starting with '-': {}", target.git_url);
        }
        let out = ProcCommand::new("git")
            .args(["clone", "--"])
            .arg(&target.git_url)
            .arg(&cache)
            .output()
            .context("running `git clone`")?;
        require_success(&out, "git clone")?;
    }
    Ok(cache)
}

fn git_add_and_commit(cache: &Path, unit_name: &str, message: &str) -> Result<()> {
    let rel = format!("skills/{unit_name}");
    let out_add = ProcCommand::new("git")
        .arg("-C")
        .arg(cache)
        .args(["add", "--", &rel])
        .output()
        .context("running `git add`")?;
    require_success(&out_add, "git add")?;

    ensure_committer_identity(cache)?;

    // `LC_ALL=C` forces English diagnostics so the "nothing to commit"
    // fast-path below works regardless of the user's locale.
    //
    // `-c commit.gpgsign=false` is intentional: promote uses a
    // synthetic committer identity when none is configured (see
    // `ensure_committer_identity`), and gpg signing with a placeholder
    // identity would either fail outright or sign with the user's key
    // attached to a different email — neither is useful. Users who
    // want a signed history can `git commit --amend -S` the cache
    // checkout after promote completes. Documented as a v1.1.1 follow-up.
    let out_commit = ProcCommand::new("git")
        .env("LC_ALL", "C")
        .arg("-C")
        .arg(cache)
        .args(["-c", "commit.gpgsign=false", "commit", "-m", message])
        .output()
        .context("running `git commit`")?;
    if !out_commit.status.success() {
        let stderr = String::from_utf8_lossy(&out_commit.stderr);
        let stdout = String::from_utf8_lossy(&out_commit.stdout);
        // A "nothing to commit" outcome is treated as success: caller
        // re-promoted an identical tree; downstream still rewrites the
        // manifest. Anything else is a hard error.
        if stdout.contains("nothing to commit") || stderr.contains("nothing to commit") {
            return Ok(());
        }
        bail!("git commit failed: {} {}", stdout.trim(), stderr.trim());
    }
    Ok(())
}

/// Pick a committer identity. Order of preference:
///   1. Whatever git already resolves (global `user.email`/`user.name`).
///   2. `GIT_AUTHOR_EMAIL` / `GIT_AUTHOR_NAME` environment variables.
///   3. A static `ainb-promote@example.invalid` placeholder — used
///      only in CI / hermetic test environments with no git identity
///      configured at all.
///
/// Configures the cache repo only — never touches the user's global
/// git config. Surfaces config-write failures via `require_success`
/// so the caller learns about read-only repos / permission errors
/// instead of seeing them silently become "Author identity unknown"
/// later in `git commit`.
pub(crate) fn ensure_committer_identity(cache: &Path) -> Result<()> {
    fn has_global(key: &str) -> bool {
        ProcCommand::new("git")
            .args(["config", "--get", key])
            .output()
            .map(|o| o.status.success() && !o.stdout.trim_ascii().is_empty())
            .unwrap_or(false)
    }
    let resolve = |key: &str, env_key: &str, fallback: &str| -> String {
        if has_global(key) {
            // Leave it alone — `git commit` will inherit from the
            // user's global config.
            return String::new();
        }
        if let Ok(v) = std::env::var(env_key) {
            if !v.is_empty() {
                return v;
            }
        }
        fallback.to_string()
    };
    let email = resolve(
        "user.email",
        "GIT_AUTHOR_EMAIL",
        "ainb-promote@example.invalid",
    );
    let name = resolve("user.name", "GIT_AUTHOR_NAME", "ainb promote");
    // Refuse argv-smuggled identity values from GIT_AUTHOR_EMAIL /
    // GIT_AUTHOR_NAME — `--file=/tmp/evil` would otherwise let `git
    // config` write the value to an attacker-chosen file.
    if email.starts_with('-') {
        bail!("refusing argv-smuggled user.email: `{email}`");
    }
    if name.starts_with('-') {
        bail!("refusing argv-smuggled user.name: `{name}`");
    }
    if !email.is_empty() {
        let out = ProcCommand::new("git")
            .arg("-C")
            .arg(cache)
            .args(["config", "--", "user.email", &email])
            .output()
            .context("setting cache repo user.email")?;
        require_success(&out, "git config user.email")?;
    }
    if !name.is_empty() {
        let out = ProcCommand::new("git")
            .arg("-C")
            .arg(cache)
            .args(["config", "--", "user.name", &name])
            .output()
            .context("setting cache repo user.name")?;
        require_success(&out, "git config user.name")?;
    }
    Ok(())
}

fn git_push(cache: &Path, branch: &str) -> Result<()> {
    // Refuse argv-smuggled branch names (e.g. a hostile remote's
    // defaultBranchRef set to `--force`) and stop option parsing
    // with `--` before the positional values.
    if branch.starts_with('-') {
        bail!("refusing argv-smuggled push branch: `{branch}`");
    }
    let out = ProcCommand::new("git")
        .arg("-C")
        .arg(cache)
        .args(["push", "--", "origin", branch])
        .output()
        .context("running `git push`")?;
    require_success(&out, "git push")?;
    Ok(())
}

fn git_rev_parse_head(cache: &Path) -> Result<String> {
    let out = ProcCommand::new("git")
        .arg("-C")
        .arg(cache)
        .args(["rev-parse", "HEAD"])
        .output()
        .context("running `git rev-parse HEAD`")?;
    require_success(&out, "git rev-parse HEAD")?;
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn require_success(out: &Output, what: &str) -> Result<()> {
    if !out.status.success() {
        bail!(
            "`{}` failed: {} {}",
            what,
            String::from_utf8_lossy(&out.stdout).trim(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────
// Filesystem helpers
// ─────────────────────────────────────────────────────────────────────

/// Recursive directory copy. Creates `dst` (and parents) as needed.
/// Preserves the relative tree under `src`. Uses [`fs::symlink_metadata`]
/// so symlinks are **not** followed — they are skipped, not copied
/// as their target. This is deliberate: a symlink redirect would be
/// a surprising escape from the nominal source root, and skill dirs
/// are expected to be regular file trees only.
pub(crate) fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(src)
        .with_context(|| format!("reading metadata for `{}`", src.display()))?;
    if !meta.is_dir() {
        bail!("source `{}` is not a directory", src.display());
    }
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        let entry_meta = fs::symlink_metadata(&from)
            .with_context(|| format!("reading metadata for `{}`", from.display()))?;
        let ft = entry_meta.file_type();
        if ft.is_symlink() {
            // Skip — see fn-level doc.
            continue;
        }
        if ft.is_dir() {
            copy_dir_recursive(&from, &to)?;
        } else if ft.is_file() {
            fs::copy(&from, &to)
                .with_context(|| format!("copy {} -> {}", from.display(), to.display()))?;
        }
        // Sockets / fifos / etc. are silently skipped (same as before).
    }
    Ok(())
}

fn compute_file_hashes(dir: &Path) -> Result<BTreeMap<String, String>> {
    let mut out: BTreeMap<String, String> = BTreeMap::new();
    walk_files(dir, dir, &mut out)?;
    Ok(out)
}

fn walk_files(root: &Path, cur: &Path, into: &mut BTreeMap<String, String>) -> Result<()> {
    for entry in fs::read_dir(cur)? {
        let entry = entry?;
        let p = entry.path();
        if p.is_dir() {
            walk_files(root, &p, into)?;
        } else if p.is_file() {
            let bytes = fs::read(&p)?;
            let rel = p
                .strip_prefix(root)
                .map(|r| r.to_string_lossy().to_string())
                .unwrap_or_else(|_| p.to_string_lossy().to_string());
            into.insert(rel, format!("sha256:{}", hex_sha256(&bytes)));
        }
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────
// Manifest helpers
// ─────────────────────────────────────────────────────────────────────

fn add_source_if_absent(
    manifest: &mut Manifest,
    source_uri: &str,
    target: &RemoteTarget,
    branch: &str,
) {
    if manifest.sources.iter().any(|s| s.uri == source_uri) {
        return;
    }
    let name = match &target.kind {
        TargetKind::Gh { user, repo } => format!("{user}-{repo}"),
        TargetKind::File { path } => sanitise_cache_key(&format!("file://{}", path.display())),
    };
    let kind = match &target.kind {
        TargetKind::Gh { .. } => Some("gh".to_string()),
        TargetKind::File { .. } => Some("git".to_string()),
    };
    manifest.sources.push(SourceEntry {
        name,
        kind,
        uri: source_uri.to_string(),
        r#ref: branch.to_string(),
        enabled: true,
        read_only: false,
        target_layout: Vec::new(),
    });
}

// ─────────────────────────────────────────────────────────────────────
// Pure utilities
// ─────────────────────────────────────────────────────────────────────

fn default_commit_message(unit_name: &str, tool: &str) -> String {
    let ts = epoch_seconds_label();
    format!(
        "feat({unit_name}): promote from local\n\n\
         Auto-promoted by `ainb skill promote` from ~/.{tool}/skills/{unit_name}/\n\
         on {ts}."
    )
}

/// Stable timestamp label for commit messages. Uses epoch-seconds
/// rather than a real RFC-3339 string so promote does not depend on
/// `chrono` / `time`. Full ISO-8601 is a v1.1.1 follow-up.
fn epoch_seconds_label() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("{now} UTC (epoch seconds)")
}

fn short_sha(s: &str) -> String {
    s.chars().take(8).collect()
}

/// Turn a git URL into a filesystem-safe directory name for the
/// promote-cache. Non-alphanumeric runs collapse into a single `_`.
fn sanitise_cache_key(url: &str) -> String {
    let mut out = String::with_capacity(url.len());
    let mut prev_was_sep = false;
    for ch in url.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '.' {
            out.push(ch);
            prev_was_sep = false;
        } else if !prev_was_sep {
            out.push('_');
            prev_was_sep = true;
        }
    }
    while out.ends_with('_') {
        out.pop();
    }
    while out.starts_with('_') {
        out.remove(0);
    }
    if out.is_empty() {
        out.push_str("repo");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_gh_uri() {
        let t = RemoteTarget::parse("gh:steven/my-skills").unwrap();
        assert!(
            matches!(t.kind, TargetKind::Gh { ref user, ref repo } if user == "steven" && repo == "my-skills")
        );
        assert_eq!(t.git_url, "https://github.com/steven/my-skills.git");
        assert!(t.explicit_branch.is_none());
    }

    #[test]
    fn parse_gh_uri_with_branch() {
        let t = RemoteTarget::parse("gh:steven/my-skills@develop").unwrap();
        assert_eq!(t.explicit_branch.as_deref(), Some("develop"));
    }

    #[test]
    fn parse_file_uri() {
        let t = RemoteTarget::parse("file:///tmp/upstream.git").unwrap();
        assert!(matches!(t.kind, TargetKind::File { .. }));
        assert_eq!(t.git_url, "file:///tmp/upstream.git");
        assert!(t.explicit_branch.is_none());
    }

    #[test]
    fn parse_file_uri_with_branch() {
        let t = RemoteTarget::parse("file:///tmp/upstream.git@dev").unwrap();
        assert_eq!(t.explicit_branch.as_deref(), Some("dev"));
        assert_eq!(t.git_url, "file:///tmp/upstream.git");
    }

    #[test]
    fn parse_invalid_uri() {
        assert!(RemoteTarget::parse("https://example.com/repo.git").is_err());
        assert!(RemoteTarget::parse("gh:no-slash").is_err());
        assert!(RemoteTarget::parse("file://").is_err());
    }

    #[test]
    fn sanitise_cache_key_examples() {
        assert_eq!(
            sanitise_cache_key("https://github.com/foo/bar.git"),
            "https_github.com_foo_bar.git"
        );
        assert_eq!(
            sanitise_cache_key("file:///tmp/upstream.git"),
            "file_tmp_upstream.git"
        );
    }

    #[test]
    fn infer_tool_picks_known_segment() {
        let uri = Uri::parse("local:/tmp/sandbox/.claude/skills@head/foo").unwrap();
        assert_eq!(infer_tool(&uri), "claude");
        let uri = Uri::parse("local:/tmp/.codex/skills@head/bar").unwrap();
        assert_eq!(infer_tool(&uri), "codex");
        let uri = Uri::parse("local:/tmp/random/path@head/baz").unwrap();
        assert_eq!(infer_tool(&uri), DEFAULT_TOOL);
    }
}
