//! `cargo xtask gen-catalog-index` — emit the enriched curated-catalog index.
//!
//! Walks an **ainb-toolkit** checkout's owned skills (`<toolkit-root>/skills/*/SKILL.md`)
//! and the vetted external skill sections of its `external-dependencies.yaml`,
//! enriches each with a `description` + an installable `install_uri`, and
//! writes a single deterministic JSON asset (default `<repo>/catalog-index.json`)
//! that `AinbCuratedCatalogBackend` fetches per agents-in-a-box GitHub release.
//!
//! The curated skills live in the standalone `stevengonsalvez/ainb-toolkit`
//! repo (flattened layout — `toolkit/` no longer exists here). Option B: the
//! agents-in-a-box release CI clones `ainb-toolkit@<tag>` and points
//! `--toolkit-root` at that checkout, then publishes the index as a release
//! asset.
//!
//! All transforms (frontmatter parse, install-URI construction, sort) live in
//! `ainb_skill_core::catalog_index`; this file is just the filesystem shell.
//!
//! Usage:
//!   cargo xtask gen-catalog-index --toolkit-root <ainb-toolkit-checkout> \
//!     [--release-tag <tag>] [--out <path>]
//!
//! `--toolkit-root` (or env `AINB_TOOLKIT_DIR`) is the cloned ainb-toolkit root
//! holding `skills/` + `external-dependencies.yaml`. `--release-tag` is the
//! **ainb-toolkit** git ref pinned into every owned `install_uri`
//! (`gh:stevengonsalvez/ainb-toolkit@<ref>/skills/<name>`) and used as the
//! index's metadata label — so it MUST match the `ainb-toolkit@<ref>` the
//! checkout was cloned at (default `latest` for dev runs; the release workflow
//! passes the pinned ainb-toolkit tag). `--out` overrides the output path
//! (default `<repo>/catalog-index.json`).

use std::fs;
use std::path::{Path, PathBuf};

use ainb_skill_core::catalog::CatalogEntryKind;
use ainb_skill_core::catalog_index::{
    CatalogIndex, CatalogIndexEntry, CatalogOrigin, OWNED_REPO, external_install_uri, github_slug,
    owned_install_uri, parse_skill_frontmatter,
};
use anyhow::{Context, Result, anyhow, bail};
use serde_yaml_ng::Value;

/// External sections of `external-dependencies.yaml` that carry git-backed
/// *skills* installable as a single `gh:` unit. Each is a YAML sequence.
const SKILL_SECTIONS: &[&str] = &["agent-skills", "nanoclaw-skills", "security-skills"];

/// External sections whose entries install by RUNNING their documented
/// `install:` shell command (npx / claude plugin / claude mcp) rather than via
/// the `gh:` unit flow. `(section, kind, repo_field)` — `repo_field` is the
/// YAML key that holds the `owner/repo`-ish provenance shown in the shelf.
const COMMAND_SECTIONS: &[(&str, CatalogEntryKind, &str)] = &[
    ("npx-skills", CatalogEntryKind::Npx, "repo"),
    ("claude-plugins", CatalogEntryKind::Plugin, "marketplace"),
    ("mcp-servers", CatalogEntryKind::Mcp, "repo"),
];

/// Entry point for the subcommand. `args` is the post-subcommand argv tail.
pub fn run(args: impl Iterator<Item = String>) -> Result<()> {
    let opts = Options::parse(args)?;
    let root = repo_root()?;
    let toolkit_root = &opts.toolkit_root;

    let owned = owned_entries(toolkit_root, &opts.release_tag)?;
    let (external, mut skipped) = external_entries(toolkit_root)?;
    let (commands, cmd_skipped) = command_entries(toolkit_root)?;
    skipped.extend(cmd_skipped);

    let owned_count = owned.len();
    let external_count = external.len();
    let command_count = commands.len();

    let mut entries = owned;
    entries.extend(external);
    entries.extend(commands);
    let index = CatalogIndex::new(&opts.release_tag, entries);

    let out = opts.out.unwrap_or_else(|| root.join("catalog-index.json"));
    if let Some(parent) = out.parent() {
        fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    fs::write(&out, index.to_json()).with_context(|| format!("write {}", out.display()))?;

    eprintln!("[xtask] wrote {}", out.display());
    eprintln!("  release tag:      {}", opts.release_tag);
    eprintln!("  owned skills:     {owned_count}");
    eprintln!("  external skills:  {external_count}");
    eprintln!("  command entries:  {command_count} (npx / plugin / mcp)");
    if !skipped.is_empty() {
        eprintln!(
            "  skipped (no github repo / no subpath / no install cmd): {}",
            skipped.len()
        );
        for s in &skipped {
            eprintln!("    - {s}");
        }
    }
    eprintln!("  total entries:   {}", index.entries.len());
    Ok(())
}

/// Parsed CLI options for the subcommand.
struct Options {
    release_tag: String,
    out: Option<PathBuf>,
    /// The cloned ainb-toolkit checkout root (holds `skills/` +
    /// `external-dependencies.yaml`).
    toolkit_root: PathBuf,
}

impl Options {
    fn parse(mut args: impl Iterator<Item = String>) -> Result<Self> {
        let mut release_tag = "latest".to_string();
        let mut out = None;
        let mut toolkit_root: Option<PathBuf> = None;
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--release-tag" => {
                    release_tag =
                        args.next().ok_or_else(|| anyhow!("--release-tag needs a value"))?;
                }
                "--out" => {
                    out = Some(PathBuf::from(
                        args.next().ok_or_else(|| anyhow!("--out needs a value"))?,
                    ));
                }
                "--toolkit-root" => {
                    toolkit_root = Some(PathBuf::from(
                        args.next().ok_or_else(|| anyhow!("--toolkit-root needs a value"))?,
                    ));
                }
                other => bail!("unknown gen-catalog-index arg {other:?}"),
            }
        }
        if release_tag.trim().is_empty() {
            bail!("--release-tag must not be empty");
        }
        // Input root: --toolkit-root flag, else $AINB_TOOLKIT_DIR. The curated
        // skills live in the standalone ainb-toolkit repo, so there is no local
        // default — CI clones ainb-toolkit@<tag> and passes the checkout path.
        let toolkit_root = toolkit_root
            .or_else(|| std::env::var_os("AINB_TOOLKIT_DIR").map(PathBuf::from))
            .ok_or_else(|| {
                anyhow!(
                    "no ainb-toolkit checkout: pass --toolkit-root <path> or set \
                     AINB_TOOLKIT_DIR (clone stevengonsalvez/ainb-toolkit@<tag>)"
                )
            })?;
        if !toolkit_root.is_dir() {
            bail!(
                "ainb-toolkit root `{}` does not exist or is not a directory",
                toolkit_root.display()
            );
        }
        Ok(Self {
            release_tag,
            out,
            toolkit_root,
        })
    }
}

/// Repo root = the workspace root's parent (the workspace lives at
/// `<repo>/ainb-tui`, this crate at `<repo>/ainb-tui/xtask`).
fn repo_root() -> Result<PathBuf> {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR")); // <repo>/ainb-tui/xtask
    manifest
        .parent() // <repo>/ainb-tui
        .and_then(Path::parent) // <repo>
        .map(Path::to_path_buf)
        .ok_or_else(|| anyhow!("cannot resolve repo root from {}", manifest.display()))
}

/// Build owned entries from each `<toolkit-root>/skills/<name>/SKILL.md`.
fn owned_entries(toolkit_root: &Path, tag: &str) -> Result<Vec<CatalogIndexEntry>> {
    let skills_dir = toolkit_root.join("skills");
    let mut dirs: Vec<PathBuf> = fs::read_dir(&skills_dir)
        .with_context(|| format!("read {}", skills_dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();

    let mut entries = Vec::with_capacity(dirs.len());
    for dir in dirs {
        let skill_md = dir.join("SKILL.md");
        let Ok(text) = fs::read_to_string(&skill_md) else {
            continue; // not a skill dir (no SKILL.md)
        };
        let Some(fm) = parse_skill_frontmatter(&text) else {
            eprintln!(
                "[xtask] warn: {} has no parseable frontmatter — skipped",
                skill_md.display()
            );
            continue;
        };
        let name = fm.name.trim().to_string();
        entries.push(CatalogIndexEntry {
            install_uri: owned_install_uri(tag, &name),
            name,
            description: fm.description.trim().to_string(),
            repo: OWNED_REPO.to_string(),
            origin: CatalogOrigin::Owned,
            stars: 0,
            kind: CatalogEntryKind::Skill,
        });
    }
    Ok(entries)
}

/// Build external entries from the skill sections of
/// `external-dependencies.yaml`. Returns `(entries, skipped_names)` so the
/// caller can report what was dropped (no github repo, or no installable
/// subpath) rather than silently truncating.
fn external_entries(toolkit_root: &Path) -> Result<(Vec<CatalogIndexEntry>, Vec<String>)> {
    let path = toolkit_root.join("external-dependencies.yaml");
    let text = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    let doc: Value =
        serde_yaml_ng::from_str(&text).with_context(|| format!("parse {}", path.display()))?;

    let mut entries = Vec::new();
    let mut skipped = Vec::new();

    for section in SKILL_SECTIONS {
        let Some(seq) = doc.get(*section).and_then(Value::as_sequence) else {
            continue;
        };
        for item in seq {
            let name = item.get("name").and_then(Value::as_str).unwrap_or("<unnamed>").to_string();

            // Only github `repo:` URLs map to a `gh:` URI; clawhub `source:`
            // and the rest are deferred.
            let slug = item.get("repo").and_then(Value::as_str).and_then(github_slug);
            let Some(slug) = slug else {
                skipped.push(format!("{section}/{name}"));
                continue;
            };

            let git_ref = item.get("version").and_then(Value::as_str);
            // A single skill lives at `subpath`; a `multi-subpath` dir holds
            // several but is still a real upstream location to install from.
            let subpath = item
                .get("subpath")
                .and_then(Value::as_str)
                .or_else(|| item.get("multi-subpath").and_then(Value::as_str));

            let Some(install_uri) = external_install_uri(&slug, git_ref, subpath) else {
                skipped.push(format!("{section}/{name}"));
                continue;
            };

            let description =
                item.get("purpose").and_then(Value::as_str).unwrap_or("").trim().to_string();

            entries.push(CatalogIndexEntry {
                name,
                description,
                repo: slug,
                install_uri,
                origin: CatalogOrigin::External,
                stars: 0,
                kind: CatalogEntryKind::Skill,
            });
        }
    }
    Ok((entries, skipped))
}

/// Build command-install entries (npx / claude-plugin / mcp) from
/// `external-dependencies.yaml`. Unlike skills, these install by RUNNING their
/// documented `install:` command (the install router shells out), so the
/// `install_uri` carries that command — newlines collapsed to `; ` so it stays
/// a single shell-runnable line. Returns `(entries, skipped_names)`.
fn command_entries(toolkit_root: &Path) -> Result<(Vec<CatalogIndexEntry>, Vec<String>)> {
    let path = toolkit_root.join("external-dependencies.yaml");
    let text = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    let doc: Value =
        serde_yaml_ng::from_str(&text).with_context(|| format!("parse {}", path.display()))?;

    let mut entries = Vec::new();
    let mut skipped = Vec::new();

    for (section, kind, repo_field) in COMMAND_SECTIONS {
        let Some(seq) = doc.get(*section).and_then(Value::as_sequence) else {
            continue;
        };
        for item in seq {
            let name = item.get("name").and_then(Value::as_str).unwrap_or("<unnamed>").to_string();

            // The documented install command is mandatory — without it there
            // is nothing to run, so drop (and report) the entry.
            let Some(cmd) = item.get("install").and_then(Value::as_str) else {
                skipped.push(format!("{section}/{name}"));
                continue;
            };
            let install_uri = collapse_command(cmd);
            if install_uri.is_empty() {
                skipped.push(format!("{section}/{name}"));
                continue;
            }

            let repo = item
                .get(*repo_field)
                .and_then(Value::as_str)
                .map(|s| github_slug(s).unwrap_or_else(|| s.trim().to_string()))
                .unwrap_or_default();
            let description =
                item.get("purpose").and_then(Value::as_str).unwrap_or("").trim().to_string();

            entries.push(CatalogIndexEntry {
                name,
                description,
                repo,
                install_uri,
                origin: CatalogOrigin::External,
                stars: 0,
                kind: *kind,
            });
        }
    }
    Ok((entries, skipped))
}

/// Collapse a possibly-multi-line `install:` block into a single shell line:
/// trim each line, drop blanks, and join with `; ` so `sh -c` runs them in
/// order. Semantically equivalent to the newline-separated form for sh.
fn collapse_command(cmd: &str) -> String {
    cmd.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("; ")
}

#[cfg(test)]
mod tests {
    use super::collapse_command;

    #[test]
    fn collapse_single_line_is_unchanged() {
        assert_eq!(
            collapse_command("npx skills add vercel-labs/agent-browser --yes"),
            "npx skills add vercel-labs/agent-browser --yes"
        );
    }

    #[test]
    fn collapse_multi_line_joins_with_semicolons() {
        let block = "claude plugin marketplace add steveyegge/beads\nclaude plugin install beads\n";
        assert_eq!(
            collapse_command(block),
            "claude plugin marketplace add steveyegge/beads; claude plugin install beads"
        );
    }

    #[test]
    fn collapse_drops_blank_lines_and_trims() {
        assert_eq!(collapse_command("\n  a \n\n  b  \n"), "a; b");
    }
}
