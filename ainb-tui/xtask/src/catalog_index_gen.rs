//! `cargo xtask gen-catalog-index` — emit the enriched curated-catalog index.
//!
//! Walks the toolkit's owned skills (`toolkit/packages/skills/*/SKILL.md`)
//! and the vetted external skill sections of `external-dependencies.yaml`,
//! enriches each with a `description` + an installable `install_uri`, and
//! writes a single deterministic JSON asset (`toolkit/catalog-index.json`)
//! that `AinbCuratedCatalogBackend` fetches per GitHub release.
//!
//! All transforms (frontmatter parse, install-URI construction, sort) live in
//! `ainb_skill_core::catalog_index`; this file is just the filesystem shell.
//!
//! Usage:
//!   cargo xtask gen-catalog-index [--release-tag <tag>] [--out <path>]
//!
//! `--release-tag` pins every owned `install_uri` (default `latest` for dev
//! runs; the release workflow passes the real tag). `--out` overrides the
//! output path (default `toolkit/catalog-index.json`).

use std::fs;
use std::path::{Path, PathBuf};

use ainb_skill_core::catalog_index::{
    external_install_uri, github_slug, owned_install_uri, parse_skill_frontmatter, CatalogIndex,
    CatalogIndexEntry, CatalogOrigin, OWNED_REPO,
};
use anyhow::{anyhow, bail, Context, Result};
use serde_yaml_ng::Value;

/// External sections of `external-dependencies.yaml` that carry installable
/// *skills* (vs plugins / npx / mcp servers, which aren't a single `gh:` unit
/// and are deferred). Each is a YAML sequence of skill entries.
const SKILL_SECTIONS: &[&str] = &["agent-skills", "nanoclaw-skills", "security-skills"];

/// Entry point for the subcommand. `args` is the post-subcommand argv tail.
pub fn run(args: impl Iterator<Item = String>) -> Result<()> {
    let opts = Options::parse(args)?;
    let root = repo_root()?;

    let owned = owned_entries(&root, &opts.release_tag)?;
    let (external, skipped) = external_entries(&root)?;

    let owned_count = owned.len();
    let external_count = external.len();

    let mut entries = owned;
    entries.extend(external);
    let index = CatalogIndex::new(&opts.release_tag, entries);

    let out = opts
        .out
        .unwrap_or_else(|| root.join("toolkit").join("catalog-index.json"));
    if let Some(parent) = out.parent() {
        fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    fs::write(&out, index.to_json()).with_context(|| format!("write {}", out.display()))?;

    eprintln!("[xtask] wrote {}", out.display());
    eprintln!("  release tag:     {}", opts.release_tag);
    eprintln!("  owned skills:    {owned_count}");
    eprintln!("  external skills: {external_count}");
    if !skipped.is_empty() {
        eprintln!("  skipped (no github repo or no subpath): {}", skipped.len());
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
}

impl Options {
    fn parse(mut args: impl Iterator<Item = String>) -> Result<Self> {
        let mut release_tag = "latest".to_string();
        let mut out = None;
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--release-tag" => {
                    release_tag = args
                        .next()
                        .ok_or_else(|| anyhow!("--release-tag needs a value"))?;
                }
                "--out" => {
                    out = Some(PathBuf::from(
                        args.next().ok_or_else(|| anyhow!("--out needs a value"))?,
                    ));
                }
                other => bail!("unknown gen-catalog-index arg {other:?}"),
            }
        }
        if release_tag.trim().is_empty() {
            bail!("--release-tag must not be empty");
        }
        Ok(Self { release_tag, out })
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

/// Build owned entries from each `toolkit/packages/skills/<name>/SKILL.md`.
fn owned_entries(root: &Path, tag: &str) -> Result<Vec<CatalogIndexEntry>> {
    let skills_dir = root.join("toolkit").join("packages").join("skills");
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
        });
    }
    Ok(entries)
}

/// Build external entries from the skill sections of
/// `external-dependencies.yaml`. Returns `(entries, skipped_names)` so the
/// caller can report what was dropped (no github repo, or no installable
/// subpath) rather than silently truncating.
fn external_entries(root: &Path) -> Result<(Vec<CatalogIndexEntry>, Vec<String>)> {
    let path = root.join("toolkit").join("external-dependencies.yaml");
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
            let name = item
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("<unnamed>")
                .to_string();

            // Only github `repo:` URLs map to a `gh:` URI; clawhub `source:`
            // and the rest are deferred.
            let slug = item
                .get("repo")
                .and_then(Value::as_str)
                .and_then(github_slug);
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

            let description = item
                .get("purpose")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string();

            entries.push(CatalogIndexEntry {
                name,
                description,
                repo: slug,
                install_uri,
                origin: CatalogOrigin::External,
                stars: 0,
            });
        }
    }
    Ok((entries, skipped))
}
