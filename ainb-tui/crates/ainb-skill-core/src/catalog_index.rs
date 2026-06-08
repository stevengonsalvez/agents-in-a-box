//! Curated catalog index — the enriched, release-published manifest that
//! `AinbCuratedCatalogBackend` (in `ainb-cli`) fetches and renders in the
//! `[b]` browse modal.
//!
//! Two concerns live here, both **pure** (no filesystem, no network) so the
//! whole transform is unit-testable and the offline contract holds:
//!
//! 1. The wire types ([`CatalogIndex`], [`CatalogIndexEntry`]) serialized to
//!    a single JSON asset published per GitHub release.
//! 2. The transforms that turn raw inputs into entries — a `SKILL.md`
//!    frontmatter block ([`parse_skill_frontmatter`]) and the install-URI
//!    builders ([`owned_install_uri`], [`external_install_uri`]).
//!
//! The filesystem walk that feeds these (glob `toolkit/packages/skills/*`,
//! read `external-dependencies.yaml`) lives in the `xtask` generator — this
//! module never touches disk.
//!
//! ```text
//! ┌──────────────────────┐   build_*    ┌───────────────┐  to_hits/search
//! │ SKILL.md frontmatter │─────────────▶│ CatalogIndex  │────────────────▶ Vec<CatalogHit>
//! │ external-deps.yaml    │              │ (JSON asset)  │
//! └──────────────────────┘              └───────────────┘
//! ```
//!
//! # Install-URI invariant
//! Every catalog `install_uri` MUST carry a `@ref/path` suffix: the existing
//! `ainb skill install` flow rejects a URI with no path (`unit URI has no
//! path`). So a repo-root-only external skill cannot be expressed as a single
//! installable entry and is omitted by the generator.

use serde::{Deserialize, Serialize};

use crate::catalog::CatalogHit;

/// `owner/repo` slug of the toolkit itself — the source for every owned
/// (toolkit-authored) catalog entry.
pub const OWNED_REPO: &str = "stevengonsalvez/agents-in-a-box";

/// Repo-relative directory holding the toolkit's owned skills. The reorg
/// only moved a handful of dev-tooling skills to repo-root `.claude/skills`;
/// the curated user-facing set still lives here (matching
/// `toolkit/bin/generate-catalog.sh`, which globs the same path).
pub const OWNED_SKILLS_REPO_DIR: &str = "toolkit/packages/skills";

/// Schema version of the published index. Bumped only on a breaking change
/// to [`CatalogIndexEntry`] so an older `AinbCuratedCatalogBackend` can
/// reject an index it cannot parse rather than silently dropping fields.
pub const SCHEMA_VERSION: u32 = 1;

/// Whether an entry is authored by the toolkit (`Owned`) or a vetted
/// third-party skill folded in from `external-dependencies.yaml`
/// (`External`). Drives the shelf's origin badge and is the axis the
/// success criteria check ("shows BOTH owned and vetted-external").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CatalogOrigin {
    /// Authored by this toolkit; installs from the pinned GitHub release.
    Owned,
    /// A vetted third-party skill; installs from its real upstream repo.
    External,
}

/// One curated catalog entry. A superset of [`CatalogHit`] that also records
/// provenance ([`CatalogOrigin`]); [`CatalogIndexEntry::to_hit`] projects it
/// down to the hit shape the browse/install flow already consumes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogIndexEntry {
    /// Unit name (the skill folder stem, e.g. `commit`).
    pub name: String,
    /// One-line human description (owned: `SKILL.md` `description`;
    /// external: the entry's `purpose`).
    pub description: String,
    /// Source repo in `owner/repo` form.
    pub repo: String,
    /// Full unit URI to feed `ainb skill install` — always carries a
    /// `@ref/path` suffix (see the module-level install-URI invariant).
    pub install_uri: String,
    /// Provenance — owned (toolkit) vs vetted external.
    pub origin: CatalogOrigin,
    /// GitHub star count for ranking; `0` for owned (all equal, so the
    /// curated insertion order is preserved by the stable sort).
    #[serde(default)]
    pub stars: u64,
}

impl CatalogIndexEntry {
    /// Project to the [`CatalogHit`] shape the browse/install flow renders.
    /// Drops only [`Self::origin`], which the hit doesn't model.
    pub fn to_hit(&self) -> CatalogHit {
        CatalogHit {
            name: self.name.clone(),
            repo: self.repo.clone(),
            stars: self.stars,
            install_uri: self.install_uri.clone(),
            description: self.description.clone(),
        }
    }
}

/// The published catalog index — a release asset fetched by
/// `AinbCuratedCatalogBackend` and searched in-memory (no SQLite, no cache).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogIndex {
    /// [`SCHEMA_VERSION`] at generation time.
    pub schema_version: u32,
    /// The release tag every owned `install_uri` pins (e.g. `v1.4.0`), so a
    /// browse done against a given index always installs matching bytes.
    pub release_tag: String,
    /// All curated entries, pre-sorted (owned-first, then by name) so a
    /// stable search preserves a deterministic shelf order.
    pub entries: Vec<CatalogIndexEntry>,
}

impl CatalogIndex {
    /// Build an index, sorting `entries` into the canonical shelf order
    /// (owned before external, then name ascending) so the JSON asset is
    /// byte-stable and the rendered shelf is deterministic.
    pub fn new(release_tag: impl Into<String>, mut entries: Vec<CatalogIndexEntry>) -> Self {
        sort_entries(&mut entries);
        Self {
            schema_version: SCHEMA_VERSION,
            release_tag: release_tag.into(),
            entries,
        }
    }

    /// Filter the shelf by `query` and project to [`CatalogHit`]s, preserving
    /// the index's canonical order.
    ///
    /// - A **blank** query returns the WHOLE shelf (the curated set is small
    ///   and local — showing everything is the point of browsing). This is a
    ///   deliberate, documented departure from the skills.sh backend, whose
    ///   blank query is a network no-op returning `[]`.
    /// - A non-blank query is a case-insensitive substring match against the
    ///   entry name OR description.
    pub fn search(&self, query: &str) -> Vec<CatalogHit> {
        let needle = query.trim().to_lowercase();
        self.entries
            .iter()
            .filter(|e| {
                needle.is_empty()
                    || e.name.to_lowercase().contains(&needle)
                    || e.description.to_lowercase().contains(&needle)
            })
            .map(CatalogIndexEntry::to_hit)
            .collect()
    }

    /// Serialize to deterministic, pretty JSON (trailing newline) for the
    /// release asset. Stable across runs given identical inputs.
    pub fn to_json(&self) -> String {
        let mut s = serde_json::to_string_pretty(self).expect("CatalogIndex serializes");
        s.push('\n');
        s
    }

    /// Parse a published index asset, rejecting a schema this build is too
    /// old to understand.
    pub fn from_json(text: &str) -> Result<Self, String> {
        let index: CatalogIndex =
            serde_json::from_str(text).map_err(|e| format!("invalid catalog index JSON: {e}"))?;
        if index.schema_version > SCHEMA_VERSION {
            return Err(format!(
                "catalog index schema_version {} is newer than this build supports ({SCHEMA_VERSION}); upgrade ainb",
                index.schema_version
            ));
        }
        Ok(index)
    }
}

/// Canonical shelf order: owned entries first, then external, each group
/// sorted by name ascending. `Owned` sorts before `External` because the
/// derived `Ord` on the field order would not — so key on an explicit rank.
fn sort_entries(entries: &mut [CatalogIndexEntry]) {
    entries.sort_by(|a, b| {
        origin_rank(a.origin)
            .cmp(&origin_rank(b.origin))
            .then_with(|| a.name.cmp(&b.name))
    });
}

/// Owned ranks before external in the shelf.
fn origin_rank(o: CatalogOrigin) -> u8 {
    match o {
        CatalogOrigin::Owned => 0,
        CatalogOrigin::External => 1,
    }
}

/// Build the install URI for an owned skill, pinning the release `tag`:
/// `gh:stevengonsalvez/agents-in-a-box@<tag>/toolkit/packages/skills/<name>`.
pub fn owned_install_uri(tag: &str, name: &str) -> String {
    format!("gh:{OWNED_REPO}@{tag}/{OWNED_SKILLS_REPO_DIR}/{name}")
}

/// Build the install URI for a vetted external skill.
///
/// `repo` is the `owner/repo` slug (github prefix already stripped). `git_ref`
/// pins the upstream (a tag/branch; falls back to `main`). `subpath` locates
/// the unit within the repo and is REQUIRED — without it the URI has no
/// `/path` and `ainb skill install` rejects it. Returns `None` when no
/// subpath is known, signalling the generator to omit the entry.
pub fn external_install_uri(repo: &str, git_ref: Option<&str>, subpath: Option<&str>) -> Option<String> {
    let subpath = subpath.map(str::trim).filter(|s| !s.is_empty())?;
    let git_ref = git_ref
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("main");
    let subpath = subpath.trim_start_matches('/');
    Some(format!("gh:{repo}@{git_ref}/{subpath}"))
}

/// Strip a `https://github.com/owner/repo[.git]` URL down to the `owner/repo`
/// slug used by the `gh:` URI scheme. Returns `None` for non-github URLs
/// (e.g. clawhub `source:` entries), which the generator then skips.
pub fn github_slug(repo_url: &str) -> Option<String> {
    let rest = repo_url
        .trim()
        .strip_prefix("https://github.com/")
        .or_else(|| repo_url.trim().strip_prefix("http://github.com/"))?;
    let rest = rest.trim_end_matches('/').trim_end_matches(".git");
    let mut parts = rest.split('/');
    let owner = parts.next().filter(|s| !s.is_empty())?;
    let repo = parts.next().filter(|s| !s.is_empty())?;
    Some(format!("{owner}/{repo}"))
}

/// The `name` + `description` extracted from a `SKILL.md` YAML frontmatter
/// block. Unknown frontmatter keys (`user-invocable`, etc.) are ignored.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct SkillFrontmatter {
    pub name: String,
    #[serde(default)]
    pub description: String,
}

/// Parse the leading YAML frontmatter block of a `SKILL.md`. Returns `None`
/// when the file has no `---`-fenced frontmatter or it omits `name`.
///
/// Tolerant of a leading blank line / BOM before the opening fence so a
/// stray newline (seen on a few skills) doesn't drop the entry.
pub fn parse_skill_frontmatter(md: &str) -> Option<SkillFrontmatter> {
    let trimmed = md.trim_start_matches('\u{feff}').trim_start_matches('\n');
    let body = trimmed.strip_prefix("---")?;
    // The opening fence is followed by a newline; the block ends at the next
    // line that is exactly `---`.
    let body = body.strip_prefix('\n').or_else(|| body.strip_prefix("\r\n"))?;
    let end = body.find("\n---")?;
    let yaml = &body[..end];
    serde_yaml_ng::from_str::<SkillFrontmatter>(yaml)
        .ok()
        .filter(|f| !f.name.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owned(name: &str) -> CatalogIndexEntry {
        CatalogIndexEntry {
            name: name.to_string(),
            description: format!("{name} desc"),
            repo: OWNED_REPO.to_string(),
            install_uri: owned_install_uri("v1.0.0", name),
            origin: CatalogOrigin::Owned,
            stars: 0,
        }
    }

    fn external(name: &str, stars: u64) -> CatalogIndexEntry {
        CatalogIndexEntry {
            name: name.to_string(),
            description: format!("{name} purpose"),
            repo: format!("acme/{name}"),
            install_uri: format!("gh:acme/{name}@v1/.claude/skills"),
            origin: CatalogOrigin::External,
            stars,
        }
    }

    #[test]
    fn owned_uri_pins_tag_and_skill_path() {
        assert_eq!(
            owned_install_uri("v1.4.0", "commit"),
            "gh:stevengonsalvez/agents-in-a-box@v1.4.0/toolkit/packages/skills/commit"
        );
    }

    #[test]
    fn external_uri_requires_subpath() {
        assert_eq!(external_install_uri("acme/x", Some("v2"), None), None);
        assert_eq!(
            external_install_uri("acme/x", Some("v2"), Some(".claude/skills")),
            Some("gh:acme/x@v2/.claude/skills".to_string())
        );
        // ref defaults to main; leading slash on subpath is normalized.
        assert_eq!(
            external_install_uri("acme/x", None, Some("/skills/foo")),
            Some("gh:acme/x@main/skills/foo".to_string())
        );
    }

    #[test]
    fn github_slug_strips_prefix_and_suffix() {
        assert_eq!(
            github_slug("https://github.com/nextlevelbuilder/ui-ux-pro-max-skill"),
            Some("nextlevelbuilder/ui-ux-pro-max-skill".to_string())
        );
        assert_eq!(
            github_slug("https://github.com/o/r.git/"),
            Some("o/r".to_string())
        );
        assert_eq!(github_slug("https://clawhub.ai/d4vinci/x"), None);
    }

    #[test]
    fn frontmatter_parses_name_and_description() {
        let md = "---\nname: commit\ndescription: Create well-formatted git commits\nuser-invocable: true\n---\n\n# Commit\n";
        let fm = parse_skill_frontmatter(md).expect("parsed");
        assert_eq!(fm.name, "commit");
        assert_eq!(fm.description, "Create well-formatted git commits");
    }

    #[test]
    fn frontmatter_tolerates_leading_blank_line() {
        let md = "\n---\nname: graphify\ndescription: turn input into a graph\n---\nbody";
        let fm = parse_skill_frontmatter(md).expect("parsed");
        assert_eq!(fm.name, "graphify");
    }

    #[test]
    fn frontmatter_none_without_fence() {
        assert!(parse_skill_frontmatter("# no frontmatter here").is_none());
    }

    #[test]
    fn new_sorts_owned_before_external_then_by_name() {
        let index = CatalogIndex::new(
            "v1.0.0",
            vec![external("zeta", 10), owned("beta"), external("alpha", 5), owned("alpha")],
        );
        let order: Vec<(&str, CatalogOrigin)> =
            index.entries.iter().map(|e| (e.name.as_str(), e.origin)).collect();
        assert_eq!(
            order,
            vec![
                ("alpha", CatalogOrigin::Owned),
                ("beta", CatalogOrigin::Owned),
                ("alpha", CatalogOrigin::External),
                ("zeta", CatalogOrigin::External),
            ]
        );
    }

    #[test]
    fn search_blank_returns_full_shelf_in_order() {
        let index = CatalogIndex::new("v1.0.0", vec![owned("commit"), external("scraper", 3)]);
        let hits = index.search("   ");
        assert_eq!(
            hits.iter().map(|h| h.name.as_str()).collect::<Vec<_>>(),
            vec!["commit", "scraper"]
        );
    }

    #[test]
    fn search_matches_name_or_description_case_insensitive() {
        let index = CatalogIndex::new(
            "v1.0.0",
            vec![
                owned("commit"),
                CatalogIndexEntry {
                    description: "GIT helper".to_string(),
                    ..owned("handover")
                },
            ],
        );
        // name match
        assert_eq!(index.search("COMMIT").len(), 1);
        // description match (case-insensitive) hits handover via "git"
        let git = index.search("git");
        assert!(git.iter().any(|h| h.name == "handover"), "{git:?}");
    }

    #[test]
    fn json_roundtrips_and_rejects_future_schema() {
        let index = CatalogIndex::new("v1.0.0", vec![owned("commit"), external("scraper", 3)]);
        let json = index.to_json();
        assert!(json.ends_with('\n'));
        let back = CatalogIndex::from_json(&json).expect("roundtrip");
        assert_eq!(back, index);

        let future = json.replace("\"schema_version\": 1", "\"schema_version\": 999");
        assert!(CatalogIndex::from_json(&future).is_err());
    }
}
