//! `ainb skill library {list,add,new}` — the own-skill library
//! (bead ai-lgk).
//!
//! A first-class home for skills the user authored locally. State lives
//! in `library.yaml` (sibling to the manifest) — **no SQLite**. Every
//! mutating command resolves the path under a tool home via
//! `read_root_for(tool)` (honours the `AINB_TOOL_HOME_<TOOL>` sandbox
//! override) and stores a canonical tool-home-relative path
//! (`.claude/skills/<name>`) so the entry stays portable across
//! machines.

use std::io;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};

use ainb_adapters_tool::read_root_for;
use ainb_skill_core::UnitKind;
use ainb_skill_core::library::{Library, OwnedUnit, library_path_in};
use ainb_skill_core::lockfile::Lockfile;
use ainb_skill_core::manifest::{Manifest, SourceEntry};
use ainb_skill_core::paths::{lockfile_path_in, manifest_path_in};
use ainb_skill_core::resolve_pair;
use ainb_skill_core::uri::Uri;

use crate::LibraryCmd;

/// Default tool when none is supplied on the command line.
const DEFAULT_TOOL: &str = "claude";

/// Every tool name [`tool_dotdir`] has an explicit mapping for — used to
/// invert a stored library path's dotdir (e.g. `.codex`) back to its
/// tool name (e.g. `codex`) in [`split_tool_relative_path`]. Kept as a
/// literal list rather than reverse-matching `tool_dotdir`'s arms at
/// runtime.
const KNOWN_TOOLS: &[&str] = &[
    "claude", "codex", "copilot", "gemini", "cursor", "amazonq", "cline", "roo",
];

pub fn dispatch(home: &Path, cmd: LibraryCmd, out: &mut dyn io::Write) -> Result<()> {
    match cmd {
        LibraryCmd::List { json } => list(home, json, out),
        LibraryCmd::Add { path, tool } => add(home, &path, tool.as_deref(), out),
        LibraryCmd::New { name, tool } => new(home, &name, tool.as_deref(), out),
        LibraryCmd::Copy { uri, tool } => copy(home, &uri, tool.as_deref(), out),
        LibraryCmd::MarkSource { name } => mark_source(home, &name, out),
        LibraryCmd::UnmarkSource { name } => unmark_source(home, &name, out),
        LibraryCmd::Push { source } => push(home, &source, out),
        LibraryCmd::Pull { source } => pull(home, &source, out),
    }
}

/// `ainb skill library list [--json]` — print every owned unit.
fn list(home: &Path, json: bool, out: &mut dyn io::Write) -> Result<()> {
    let lib = Library::load_from(&library_path_in(home))?;

    if json {
        let rows: Vec<serde_json::Value> = lib
            .owned
            .iter()
            .map(|u| {
                serde_json::json!({
                    "name": u.name,
                    "kind": u.kind.as_str(),
                    "path": u.path,
                    "created": u.created,
                    "promoted_uri": u.promoted_uri,
                })
            })
            .collect();
        writeln!(out, "{}", serde_json::to_string_pretty(&rows)?)?;
        return Ok(());
    }

    if lib.owned.is_empty() {
        writeln!(
            out,
            "# no owned skills — `ainb skill library new <name>` to author one"
        )?;
        return Ok(());
    }

    writeln!(out, "{:<28}  {:<8}  {:<32}  deploy", "name", "kind", "path")?;
    writeln!(out, "{:-<28}  {:-<8}  {:-<32}  {:-<8}", "", "", "", "")?;
    for u in &lib.owned {
        let deploy = if u.promoted_uri.is_some() {
            "promoted"
        } else {
            "local"
        };
        writeln!(
            out,
            "{:<28}  {:<8}  {:<32}  {}",
            u.name,
            u.kind.as_str(),
            u.path,
            deploy
        )?;
    }
    writeln!(out, "# {} owned skill(s)", lib.owned.len())?;
    Ok(())
}

/// `ainb skill library add <path> [--tool t]` — ingest an existing
/// on-disk skill folder. Refuses paths outside the tool home.
fn add(home: &Path, path: &Path, tool: Option<&str>, out: &mut dyn io::Write) -> Result<()> {
    let tool = tool.unwrap_or(DEFAULT_TOOL);
    let tool_root = read_root_for(tool);

    // Canonicalise both sides so symlinks / `..` segments don't dodge
    // the containment check. Fall back to the raw path when canonicalise
    // fails (e.g. the path doesn't exist) — `bail` below catches that.
    let abs = canonical_or_self(path);
    let root_abs = canonical_or_self(&tool_root);
    if !abs.starts_with(&root_abs) {
        bail!(
            "refusing to add `{}` — it is outside the `{}` tool home (`{}`). \
             Only skills under a tool home can be registered.",
            path.display(),
            tool,
            tool_root.display()
        );
    }

    if !abs.is_dir() {
        bail!(
            "`{}` is not a directory — point at a skill folder",
            path.display()
        );
    }

    let name = abs
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| anyhow!("cannot derive a skill name from `{}`", path.display()))?
        .to_string();

    let rel = tool_home_relative_path(tool, "skills", &name);
    register_owned(home, &name, UnitKind::Skill, &rel, out)
}

/// `ainb skill library new <name> [--tool t]` — scaffold a fresh
/// `SKILL.md` under the tool's skills dir and register it.
fn new(home: &Path, name: &str, tool: Option<&str>, out: &mut dyn io::Write) -> Result<()> {
    let tool = tool.unwrap_or(DEFAULT_TOOL);
    let name = name.trim();
    if name.is_empty() {
        bail!("skill name must not be empty");
    }

    let tool_root = read_root_for(tool);
    let dir = tool_root.join("skills").join(name);
    if dir.exists() {
        bail!(
            "`{}` already exists — pick a fresh name or use `library add`",
            dir.display()
        );
    }
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("creating skill folder `{}`", dir.display()))?;

    let skill_md = scaffold_skill_md(name);
    std::fs::write(dir.join("SKILL.md"), skill_md)
        .with_context(|| format!("writing SKILL.md for `{name}`"))?;

    let rel = tool_home_relative_path(tool, "skills", name);
    register_owned(home, name, UnitKind::Skill, &rel, out)
}

/// `ainb skill library copy <uri> [--tool t]` — copy a unit from ANY
/// configured source into the user's library. Resolves the source unit
/// dir the same way `install` does, deploys it under the tool's skills
/// dir (idempotent — skipped when already deployed there), then
/// registers it as an owned unit.
fn copy(home: &Path, uri_str: &str, tool: Option<&str>, out: &mut dyn io::Write) -> Result<()> {
    let tool = tool.unwrap_or(DEFAULT_TOOL);
    let uri = Uri::parse(uri_str).with_context(|| format!("parsing `{uri_str}`"))?;

    let resolution = crate::skill::resolve_unit_dir(home, &uri)?;
    let src_dir = resolution.unit_dir();
    let name = src_dir
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| anyhow!("cannot derive a unit name from `{}`", src_dir.display()))?
        .to_string();

    let dest = read_root_for(tool).join("skills").join(&name);
    if dest.exists() {
        writeln!(
            out,
            "# `{name}` already deployed at `{}` — skipping copy, registering only",
            dest.display()
        )?;
    } else {
        crate::promote::copy_dir_recursive(&src_dir, &dest)
            .with_context(|| format!("copying `{}` -> `{}`", src_dir.display(), dest.display()))?;
    }

    let rel = tool_home_relative_path(tool, "skills", &name);
    register_owned(home, &name, resolution.kind, &rel, out)
}

/// `ainb skill library mark-source <name>` — opt a manifest source into
/// git-native two-way library sync (`push`/`pull`).
fn mark_source(home: &Path, name: &str, out: &mut dyn io::Write) -> Result<()> {
    require_manifest_source(home, name)?;
    let lib_path = library_path_in(home);
    let mut lib = Library::load_from(&lib_path)?;
    let inserted = lib.mark_source(name);
    lib.save_to(&lib_path)?;
    if inserted {
        writeln!(out, "marked `{name}` as a library source")?;
    } else {
        writeln!(out, "`{name}` is already a library source")?;
    }
    Ok(())
}

/// `ainb skill library unmark-source <name>` — the inverse of `mark-source`.
fn unmark_source(home: &Path, name: &str, out: &mut dyn io::Write) -> Result<()> {
    require_manifest_source(home, name)?;
    let lib_path = library_path_in(home);
    let mut lib = Library::load_from(&lib_path)?;
    let removed = lib.unmark_source(name);
    lib.save_to(&lib_path)?;
    if removed {
        writeln!(out, "unmarked `{name}` as a library source")?;
    } else {
        writeln!(out, "`{name}` was not a library source")?;
    }
    Ok(())
}

/// Bail unless `name` is a source declared in the manifest — guards
/// `mark-source`/`unmark-source` against typos.
fn require_manifest_source(home: &Path, name: &str) -> Result<()> {
    let manifest = Manifest::load_from(&manifest_path_in(home))?;
    if !manifest.sources.iter().any(|s| s.name == name) {
        bail!("no source named `{name}` in the manifest — run `ainb source add` first");
    }
    Ok(())
}

/// `ainb skill library push <source>` — publish local library edits to
/// a library source's git remote.
///
/// Two passes cover the two ways content can enter the library:
///  1. The same bidirectional content sync `ainb skill sync` uses
///     (scoped to this source, to-repo only) — covers units that went
///     through `install` and are tracked in the lockfile.
///  2. [`stage_owned_units`] — copies every *owned* unit
///     (`library add`/`new`/`copy`) that resolves to this source's
///     `target_layout` directly into the checkout. Those units are
///     registered in `library.yaml`, not the lockfile, so pass 1 never
///     sees them; without this pass `push` would report success while
///     silently publishing nothing for them.
///
/// Whatever either pass staged is then committed + pushed in one shot.
fn push(home: &Path, source_name: &str, out: &mut dyn io::Write) -> Result<()> {
    let source = require_library_source(home, source_name)?;

    let manifest = Manifest::load_from(&manifest_path_in(home))?;
    let lockfile = Lockfile::load_from(&lockfile_path_in(home))?;
    let sync_args = crate::SyncArgs {
        source_or_unit: Some(source_name.to_string()),
        yes: true,
        dry_run: false,
        to_home: false,
        to_repo: true,
    };
    crate::skill::bidirectional_content_sync(home, &manifest, &lockfile, &sync_args, out)?;

    let checkout = crate::skill::ensure_repo_clone(home, &source)?;
    let staged = stage_owned_units(home, &source, &checkout, out)?;
    if staged > 0 {
        writeln!(out, "# staged {staged} owned-library unit(s) for publish")?;
    }
    git_add_commit_push(&checkout, "ainb: sync library edits", &source, out)
}

/// Copy every deployed *owned* library unit (`library add`/`new`/`copy`)
/// into `checkout` at its repo-side convention path, so `push` actually
/// publishes locally-authored skills that were never routed through the
/// lockfile-driven content sync (finding: push silently dropped these).
///
/// For each owned unit whose deployed directory exists on this machine,
/// the repo-side target directory is resolved via
/// [`resolve_owned_unit_repo_dir`] — the same `target_layout` mapping
/// [`ainb_skill_core::apply_to_repo`] uses for lockfile-driven units, so
/// the convention (`skills/<name>`, a flat layout, …) always matches
/// wherever this source already keeps its skills. When no mapping
/// covers the unit, it is **not** silently skipped: a warning naming
/// the unit is printed so the omission is visible.
///
/// Returns the count of units actually staged.
fn stage_owned_units(
    home: &Path,
    source: &SourceEntry,
    checkout: &Path,
    out: &mut dyn io::Write,
) -> Result<usize> {
    let lib = Library::load_from(&library_path_in(home))?;
    let mut staged = 0usize;
    for unit in &lib.owned {
        let (tool, unit_tool_rel) = split_tool_relative_path(&unit.path);
        let unit_dir = read_root_for(&tool).join(&unit_tool_rel);
        if !unit_dir.is_dir() {
            // Not deployed on this machine (registered elsewhere, or
            // moved) — nothing to publish, not an error.
            continue;
        }

        match resolve_owned_unit_repo_dir(source, &unit_tool_rel, &unit_dir) {
            Some(repo_dir) => {
                crate::promote::copy_dir_recursive(&unit_dir, &checkout.join(&repo_dir))
                    .with_context(|| {
                        format!(
                            "staging owned skill `{}` -> `{}`",
                            unit.name,
                            repo_dir.display()
                        )
                    })?;
                staged += 1;
            }
            None => {
                writeln!(
                    out,
                    "# warning: cannot determine a publish path for owned skill `{}` — \
                     `{}` has no target_layout mapping covering it; NOT pushed",
                    unit.name, source.name
                )?;
            }
        }
    }
    Ok(staged)
}

/// Resolve the repo-relative directory a locally-authored library unit
/// should publish to inside a source's checkout, by trying
/// [`resolve_pair`] — the same `target_layout` resolution
/// [`ainb_skill_core::apply_to_repo`] uses for lockfile-driven units —
/// against each file under `unit_dir` until one resolves.
///
/// `unit_tool_rel` is the unit's directory relative to the tool root
/// (dotdir already stripped, e.g. `skills/my-skill`). Returns `None`
/// when nothing under `unit_dir` matches any mapping the source
/// declares (e.g. a `target_layout` that simply doesn't cover this
/// unit's kind) — callers must not silently drop that case; see
/// [`stage_owned_units`].
fn resolve_owned_unit_repo_dir(
    source: &SourceEntry,
    unit_tool_rel: &Path,
    unit_dir: &Path,
) -> Option<PathBuf> {
    for rel_in_unit in unit_files_relative(unit_dir) {
        let file_tool_rel = unit_tool_rel.join(&rel_in_unit);
        let Some((_, repo_rel)) = resolve_pair(source, &file_tool_rel) else {
            continue;
        };
        // `resolve_pair` re-roots the tail (the part of the path after
        // the layout's static prefix) unchanged, so the mapped file's
        // path ends with exactly `rel_in_unit`'s components — strip
        // them back off to recover the unit's mapped directory. Same
        // trick as `remap_unit_install_path` in skill.rs, repo side.
        let n = rel_in_unit.components().count();
        let comps: Vec<_> = repo_rel.components().collect();
        if comps.len() < n {
            continue;
        }
        return Some(comps[..comps.len() - n].iter().collect());
    }
    None
}

/// Every regular file under `dir`, as paths relative to `dir`. Symlinks
/// are skipped — `copy_dir_recursive` (which actually stages the unit)
/// applies the same policy, so a path this walker resolves a publish
/// location from can never be a symlink the copier then refuses.
fn unit_files_relative(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    collect_files_relative(dir, Path::new(""), &mut out);
    out
}

fn collect_files_relative(base: &Path, rel: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(base.join(rel)) else {
        return;
    };
    for entry in entries.flatten() {
        let child_rel = rel.join(entry.file_name());
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            collect_files_relative(base, &child_rel, out);
        } else if file_type.is_file() {
            out.push(child_rel);
        }
    }
}

/// Reverse of [`tool_dotdir`]: split a stored library path
/// (`.claude/skills/foo`) into `(tool, tool-root-relative-rest)`
/// (`("claude", "skills/foo")`). A path that doesn't start with a
/// recognisable dotdir segment (unexpected, but not fatal) defaults to
/// [`DEFAULT_TOOL`] with the path taken as-is, rather than failing the
/// whole push over one malformed entry.
fn split_tool_relative_path(path: &str) -> (String, PathBuf) {
    let p = Path::new(path);
    let mut comps = p.components();
    let first = comps.next();
    let rest = comps.as_path().to_path_buf();
    match first.and_then(|c| c.as_os_str().to_str()) {
        Some(dotdir) if dotdir.starts_with('.') && dotdir.len() > 1 => {
            let tool = KNOWN_TOOLS
                .iter()
                .copied()
                .find(|&t| tool_dotdir(t) == dotdir)
                .map(|t| t.to_string())
                .unwrap_or_else(|| dotdir.trim_start_matches('.').to_string());
            (tool, rest)
        }
        _ => (DEFAULT_TOOL.to_string(), p.to_path_buf()),
    }
}

/// `ainb skill library pull <source>` — rebase-pull a library source's
/// git remote, then sync its (possibly updated) content back down into
/// the tool home.
fn pull(home: &Path, source_name: &str, out: &mut dyn io::Write) -> Result<()> {
    let source = require_library_source(home, source_name)?;
    let checkout = crate::skill::ensure_repo_clone(home, &source)?;
    git_pull_rebase(&checkout)?;

    let manifest = Manifest::load_from(&manifest_path_in(home))?;
    let lockfile = Lockfile::load_from(&lockfile_path_in(home))?;
    let sync_args = crate::SyncArgs {
        source_or_unit: Some(source_name.to_string()),
        yes: true,
        dry_run: false,
        to_home: true,
        to_repo: false,
    };
    crate::skill::bidirectional_content_sync(home, &manifest, &lockfile, &sync_args, out)?;
    writeln!(out, "pulled `{source_name}` and synced to home")?;
    Ok(())
}

/// Bail unless `name` is both declared in the manifest AND marked as a
/// library source; on success returns the owned `SourceEntry` (cloned
/// out of the manifest borrow) so the caller can resolve its checkout.
fn require_library_source(home: &Path, name: &str) -> Result<SourceEntry> {
    let lib = Library::load_from(&library_path_in(home))?;
    if !lib.is_library_source(name) {
        bail!(
            "`{name}` is not a library source — run \
             `ainb skill library mark-source {name}` first"
        );
    }
    let manifest = Manifest::load_from(&manifest_path_in(home))?;
    manifest
        .sources
        .iter()
        .find(|s| s.name == name)
        .cloned()
        .ok_or_else(|| anyhow!("no source named `{name}` in the manifest"))
}

/// A `git -C <checkout>` command pre-hardened against interactive
/// credential prompts: `GIT_TERMINAL_PROMPT=0` + `GIT_ASKPASS=/bin/true`
/// so an auth-required / unreachable remote fails fast with a plain
/// error instead of blocking forever on a terminal prompt — mirrors
/// `ainb_skill_core::sync`'s `git_capture` hardening (commit f34d851).
fn git_hardened(checkout: &Path) -> std::process::Command {
    let mut cmd = std::process::Command::new("git");
    cmd.arg("-C")
        .arg(checkout)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ASKPASS", "/bin/true");
    cmd
}

/// `git -C <checkout> pull --rebase`. Surfaces stderr verbatim on
/// failure (conflicts left as standard git conflict markers — no
/// custom merge UI).
fn git_pull_rebase(checkout: &Path) -> Result<()> {
    let out = git_hardened(checkout)
        .args(["pull", "--rebase"])
        .output()
        .context("running `git pull --rebase`")?;
    if !out.status.success() {
        bail!(
            "git pull --rebase failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

/// `git add -A && git commit -m <message> && git push -- origin
/// HEAD:<source.ref>` in `checkout`. A "nothing to commit" commit
/// outcome is treated as success (no-op — the content sync pass +
/// owned-unit staging already published everything). Reuses
/// `promote`'s committer-identity fallback so a hermetic CI run with no
/// global git identity still produces a clean commit.
///
/// Pushes via an explicit `HEAD:<ref>` refspec rather than a bare `git
/// push`: `ensure_repo_clone` never checks out `source.ref` (it just
/// clones/pulls the remote's default branch), so pushing "whatever
/// branch happens to be checked out" would silently land on the wrong
/// remote branch for any source whose ref isn't the default. `HEAD:<ref>`
/// pushes the checkout's current commit to `source.ref` regardless of
/// what the local branch is named — mirrors `apply_to_repo`'s explicit
/// `origin <source.ref>` push target, adapted to not depend on the
/// local branch name matching.
fn git_add_commit_push(
    checkout: &Path,
    message: &str,
    source: &SourceEntry,
    out: &mut dyn io::Write,
) -> Result<()> {
    let add = git_hardened(checkout)
        .args(["add", "-A"])
        .output()
        .context("running `git add -A`")?;
    if !add.status.success() {
        bail!(
            "git add -A failed: {}",
            String::from_utf8_lossy(&add.stderr).trim()
        );
    }

    crate::promote::ensure_committer_identity(checkout)?;

    let commit = git_hardened(checkout)
        .env("LC_ALL", "C")
        .args(["-c", "commit.gpgsign=false", "commit", "-m", message])
        .output()
        .context("running `git commit`")?;
    if !commit.status.success() {
        let stderr = String::from_utf8_lossy(&commit.stderr);
        let stdout = String::from_utf8_lossy(&commit.stdout);
        if stdout.contains("nothing to commit") || stderr.contains("nothing to commit") {
            writeln!(out, "# nothing to commit — library edits already in sync")?;
            return Ok(());
        }
        bail!("git commit failed: {} {}", stdout.trim(), stderr.trim());
    }

    // Refuse an argv-smuggled refspec (e.g. a ref starting with `-`
    // masquerading as a push flag) — mirrors the same guard in
    // `ainb_skill_core::sync::apply_to_repo`.
    if source.r#ref.starts_with('-') {
        bail!("refusing argv-smuggled ref: `{}`", source.r#ref);
    }
    let refspec = format!("HEAD:{}", source.r#ref);
    let push = git_hardened(checkout)
        .args(["push", "--", "origin", &refspec])
        .output()
        .context("running `git push`")?;
    if !push.status.success() {
        bail!(
            "git push failed: {}",
            String::from_utf8_lossy(&push.stderr).trim()
        );
    }
    writeln!(
        out,
        "pushed library edits for `{}` to `{}`",
        checkout.display(),
        source.r#ref
    )?;
    Ok(())
}

/// Shared registration tail: load `library.yaml`, register (dedup by
/// name), save, and print a one-line summary.
fn register_owned(
    home: &Path,
    name: &str,
    kind: UnitKind,
    rel_path: &str,
    out: &mut dyn io::Write,
) -> Result<()> {
    let lib_path = library_path_in(home);
    let mut lib = Library::load_from(&lib_path)?;
    let inserted = lib.register(OwnedUnit {
        name: name.to_string(),
        kind,
        path: rel_path.to_string(),
        created: now_rfc3339(),
        promoted_uri: None,
    });
    lib.save_to(&lib_path)
        .with_context(|| format!("saving library at `{}`", lib_path.display()))?;
    let verb = if inserted { "registered" } else { "updated" };
    writeln!(out, "{verb} own-skill `{name}` → {rel_path}")?;
    Ok(())
}

/// Scaffold the bytes of a minimal, parseable `SKILL.md` with the
/// frontmatter the discovery walkers + adapters expect.
fn scaffold_skill_md(name: &str) -> String {
    format!(
        "---\nname: {name}\nkind: skill\ndescription: \"TODO: describe {name}.\"\n---\n\n\
         # {name}\n\n\
         <!-- Authored locally via `ainb skill library new`. Fill in the body. -->\n"
    )
}

/// Canonical tool-home-relative display path
/// (e.g. `.claude/skills/my-skill`). The dotdir is derived from a stable
/// tool→dotdir map so the stored path is portable regardless of where
/// the tool home actually resolves on this machine (real home, sandbox
/// override, …).
fn tool_home_relative_path(tool: &str, subdir: &str, name: &str) -> String {
    format!("{}/{subdir}/{name}", tool_dotdir(tool))
}

/// Stable tool → home-dotdir mapping. Matches `real_home_for` in
/// `ainb-adapters-tool` so the canonical relative path lines up with the
/// real on-disk layout. Unknown tools fall back to a dotted name
/// (`.{tool}`) so the path stays informative without a hard failure.
fn tool_dotdir(tool: &str) -> String {
    match tool {
        "claude" => ".claude".to_string(),
        "codex" => ".codex".to_string(),
        "copilot" => ".copilot".to_string(),
        "gemini" => ".gemini".to_string(),
        "cursor" => ".cursor".to_string(),
        "amazonq" => ".aws/amazonq".to_string(),
        "cline" => ".cline".to_string(),
        "roo" => ".roo".to_string(),
        other => format!(".{other}"),
    }
}

fn canonical_or_self(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

/// Current wall-clock time as an RFC 3339 string. Uses
/// `std::time::SystemTime` formatted as a UTC instant without pulling in
/// `chrono` at the CLI layer.
fn now_rfc3339() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    // Minimal RFC 3339 rendering from a Unix timestamp (UTC). Avoids a
    // chrono dependency for a single timestamp; the value is only used
    // for display + round-trip, not parsed back by ainb.
    format_unix_utc(secs)
}

/// Render a Unix timestamp (seconds, UTC) as `YYYY-MM-DDTHH:MM:SSZ`.
fn format_unix_utc(secs: u64) -> String {
    // Days since epoch + seconds-of-day.
    let days = secs / 86_400;
    let sod = secs % 86_400;
    let (hh, mm, ss) = (sod / 3600, (sod % 3600) / 60, sod % 60);
    let (y, mo, d) = civil_from_days(days as i64);
    format!("{y:04}-{mo:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}Z")
}

/// Howard Hinnant's `civil_from_days` — convert days-since-epoch to a
/// proleptic Gregorian (year, month, day). Public-domain algorithm.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Finding 2 (git commands hang on private/unreachable remotes):
    /// every git `Command` built through [`git_hardened`] must disable
    /// interactive credential prompts, or an auth-required remote
    /// blocks `push`/`pull` forever on a terminal prompt.
    #[test]
    fn git_hardened_disables_interactive_prompts() {
        let cmd = git_hardened(Path::new("/tmp/some-checkout"));
        let env_value = |key: &str| -> Option<String> {
            cmd.get_envs()
                .find(|(k, _)| k.to_str() == Some(key))
                .and_then(|(_, v)| v)
                .and_then(|v| v.to_str())
                .map(str::to_string)
        };
        assert_eq!(
            env_value("GIT_TERMINAL_PROMPT").as_deref(),
            Some("0"),
            "must disable the terminal credential prompt"
        );
        assert_eq!(
            env_value("GIT_ASKPASS").as_deref(),
            Some("/bin/true"),
            "must short-circuit GUI/askpass credential helpers"
        );
    }

    #[test]
    fn split_tool_relative_path_recognises_known_dotdirs() {
        assert_eq!(
            split_tool_relative_path(".claude/skills/my-skill"),
            ("claude".to_string(), PathBuf::from("skills/my-skill"))
        );
        assert_eq!(
            split_tool_relative_path(".codex/skills/other"),
            ("codex".to_string(), PathBuf::from("skills/other"))
        );
    }

    #[test]
    fn split_tool_relative_path_defaults_claude_for_unrecognisable_input() {
        // No leading dotdir at all — falls back to the default tool
        // rather than failing the whole push over one malformed entry.
        assert_eq!(
            split_tool_relative_path("skills/my-skill"),
            ("claude".to_string(), PathBuf::from("skills/my-skill"))
        );
    }
}
