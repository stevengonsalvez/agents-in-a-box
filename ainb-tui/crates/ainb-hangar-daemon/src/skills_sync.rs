//! P6.2 — the curated-skills directory importer behind `ainb hangar skills sync`.
//!
//! Walks an `ainb-toolkit` `skills/`-shaped tree where each skill lives at
//! `<root>/<name>/SKILL.md` (with optional supporting files nested beneath the
//! skill dir), parses the YAML frontmatter for the skill's `name`/`description`,
//! and upserts each skill workspace-scoped into the store via
//! [`SkillRepo::upsert_by_name`] (idempotent on `(workspace_id, name)`).
//!
//! # All-or-nothing
//!
//! The whole batch is **validated before any write**: every skill dir is walked,
//! parsed, and its assets collected up front. If any `SKILL.md` is malformed
//! (no frontmatter, or no `name:` key) the import aborts with
//! [`SyncError::Malformed`] and **nothing is persisted** — there is never a
//! partial import where some skills land and others don't. Only once the entire
//! batch parses cleanly do the upserts run.
//!
//! # Determinism
//!
//! Skill dirs are walked in sorted order and each skill's file list is collected
//! into an ordered `Vec<SkillFileInput>` (never a `HashMap`), so a re-run
//! produces byte-identical writes and stable `tracing` output.
//!
//! # Extensibility
//!
//! [`skills_sync_from`] is the entry point the CLI calls. The walk/parse/collect
//! pipeline is expressed behind the [`SkillImporter`] trait so a future
//! `UrlImporter` (skills.sh, GitHub) slots in without touching the upsert hot
//! path — [`ToolkitDirImporter`] is the v1 filesystem implementation.

use std::path::{Path, PathBuf};

use ainb_hangar_core::ids::{SkillId, WorkspaceId};
use ainb_hangar_core::skill::SkillFileInput;
use ainb_hangar_store::repo::skill::{SkillRepo, SkillRepoError};
use sqlx::SqlitePool;

/// The basename every skill directory must contain to be imported.
const SKILL_MD: &str = "SKILL.md";

/// Env var that, when set, overrides the curated-skills source directory.
/// Point it at a fetched `ainb-toolkit` checkout's flattened `skills/` dir.
pub const SOURCE_ENV: &str = "AINB_TOOLKIT_SKILLS_DIR";

/// The path suffix the source walk looks for under each ancestor of the current
/// working directory: a checked-out `ainb-toolkit` clone whose curated skills
/// live flattened at `skills/` (the canonical home — `toolkit/` no longer
/// exists in this monorepo).
const TOOLKIT_SUFFIX: &str = "ainb-toolkit/skills";

/// Resolve the default skills source directory for `ainb hangar skills sync`.
///
/// Resolution order:
/// 1. `$AINB_TOOLKIT_SKILLS_DIR` when set and non-empty (a fetched ainb-toolkit
///    `skills/` dir).
/// 2. Otherwise walk up from the current working directory looking for an
///    `ainb-toolkit/skills` checkout sitting in an ancestor directory.
///
/// Returns `None` when neither is found — the CLI then asks the operator for an
/// explicit `--source` (e.g. a path to a cloned ainb-toolkit `skills/` dir).
#[must_use]
pub fn default_source_dir() -> Option<PathBuf> {
    if let Some(raw) = std::env::var_os(SOURCE_ENV).filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(raw));
    }
    let cwd = std::env::current_dir().ok()?;
    for ancestor in cwd.ancestors() {
        let candidate = ancestor.join(TOOLKIT_SUFFIX);
        if candidate.is_dir() {
            return Some(candidate);
        }
    }
    None
}

/// One fully-parsed, ready-to-upsert skill: its name, optional description, and
/// the ordered file set (the `SKILL.md` body file plus any nested assets).
///
/// This is the intermediate the importer validates the *entire* batch into
/// before a single row is written — the all-or-nothing guarantee lives here.
#[derive(Debug, Clone)]
pub struct ParsedSkill {
    /// The skill name, taken verbatim from the frontmatter `name:` key. The
    /// store normalises it to kebab-case on write.
    pub name: String,
    /// Short description from the frontmatter `description:` key, if present.
    pub description: Option<String>,
    /// The `SKILL.md` body (everything after the frontmatter block).
    pub content: String,
    /// Ordered supporting files, including the `SKILL.md` file itself at
    /// `path = "SKILL.md"`. Sorted by relative path for byte-stable re-runs.
    pub files: Vec<SkillFileInput>,
}

/// What an import produced — one `(name, id)` pair per upserted skill, in
/// import order. Returned by [`skills_sync_from`] and rendered by the CLI.
#[derive(Debug, Default, Clone)]
pub struct SyncReport {
    /// The skills written, as `(name, stable id)`, in deterministic order.
    pub imported: Vec<(String, SkillId)>,
}

/// Failure surface for a skill import.
#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    /// The import root could not be walked / read.
    #[error("reading skills source {path}: {source}")]
    Io {
        /// The path that failed to read.
        path: PathBuf,
        /// The underlying IO error.
        source: std::io::Error,
    },
    /// A `SKILL.md` was missing its frontmatter or its `name:` key. The whole
    /// batch is aborted — nothing is written.
    #[error("malformed SKILL.md (missing frontmatter or `name:`): {path}")]
    Malformed {
        /// The offending `SKILL.md` path.
        path: PathBuf,
    },
    /// Two skill dirs in the batch normalise to the same `(workspace, name)`
    /// key — ambiguous, so the batch is aborted before any write.
    #[error("duplicate skill name `{name}` in import batch ({first} and {second})")]
    DuplicateName {
        /// The colliding (normalised) name.
        name: String,
        /// First dir that produced it.
        first: PathBuf,
        /// Second dir that produced it.
        second: PathBuf,
    },
    /// A store write failed.
    #[error(transparent)]
    Repo(#[from] SkillRepoError),
}

/// Parse + collect a directory tree into ready-to-upsert [`ParsedSkill`]s.
///
/// Implementors do the IO-bound walk/parse; the shared [`skills_sync_from`]
/// driver owns validation (uniqueness) and the workspace-scoped upserts, so a
/// future `UrlImporter` reuses the exact same write path.
pub trait SkillImporter {
    /// Walk the source and return every parsed skill, in deterministic order.
    ///
    /// # Errors
    ///
    /// Returns [`SyncError::Io`] if the source cannot be read or
    /// [`SyncError::Malformed`] if any `SKILL.md` lacks a `name:`.
    fn collect(&self) -> Result<Vec<ParsedSkill>, SyncError>;
}

/// v1 importer: a local `ainb-toolkit` `skills/`-shaped directory.
pub struct ToolkitDirImporter<'a> {
    /// The directory holding `<name>/SKILL.md` skill dirs.
    root: &'a Path,
}

impl<'a> ToolkitDirImporter<'a> {
    /// Build an importer over a skills root directory.
    #[must_use]
    pub const fn new(root: &'a Path) -> Self {
        Self { root }
    }
}

impl SkillImporter for ToolkitDirImporter<'_> {
    fn collect(&self) -> Result<Vec<ParsedSkill>, SyncError> {
        let mut dirs = walk_skill_dirs(self.root)?;
        dirs.sort();
        let mut out = Vec::with_capacity(dirs.len());
        for dir in dirs {
            out.push(parse_skill_dir(&dir)?);
        }
        Ok(out)
    }
}

/// Import every skill under `root` into `workspace`, idempotently.
///
/// Validates the **whole batch** (every `SKILL.md` parses + names are unique)
/// before writing anything, then upserts each skill via
/// [`SkillRepo::upsert_by_name`]. Re-running updates existing rows in place
/// (matched on `(workspace_id, name)`) rather than forking duplicates.
///
/// # Errors
///
/// - [`SyncError::Io`] — the source could not be read.
/// - [`SyncError::Malformed`] — a `SKILL.md` lacks frontmatter / a `name:` key
///   (the batch is aborted, nothing written).
/// - [`SyncError::DuplicateName`] — two dirs collide on the normalised name.
/// - [`SyncError::Repo`] — a store write failed.
pub async fn skills_sync_from(
    pool: &SqlitePool,
    workspace: &WorkspaceId,
    root: &Path,
) -> Result<SyncReport, SyncError> {
    let importer = ToolkitDirImporter::new(root);
    let parsed = importer.collect()?;
    validate_unique(&parsed)?;

    // Only after the whole batch validates do we touch the store. Each upsert is
    // itself transactional in the repo, and the pre-validation guarantees we
    // never start writing a batch that would fail partway on a malformed entry.
    let mut report = SyncReport::default();
    for skill in parsed {
        let id = SkillRepo::upsert_by_name(
            pool,
            workspace,
            &skill.name,
            skill.description.as_deref(),
            Some(&skill.content),
            skill.files.clone(),
        )
        .await?;
        tracing::info!(
            skill.name = %skill.name,
            skill.files = skill.files.len(),
            "imported skill"
        );
        report.imported.push((skill.name, id));
    }
    Ok(report)
}

/// Reject a batch where two dirs normalise to the same skill name (the upsert
/// would otherwise silently clobber one with the other, hiding a real conflict).
fn validate_unique(parsed: &[ParsedSkill]) -> Result<(), SyncError> {
    use ainb_hangar_core::skill::SkillName;
    let mut seen: std::collections::HashMap<String, PathBuf> = std::collections::HashMap::new();
    for skill in parsed {
        // A name that fails normalisation surfaces at upsert time as a
        // `SkillRepoError::Name`; here we only guard intra-batch collisions, so
        // skip un-normalisable names (the repo rejects them downstream).
        let Ok(normalised) = SkillName::new(&skill.name) else {
            continue;
        };
        let key = normalised.as_str().to_string();
        let dir = PathBuf::from(&skill.name);
        if let Some(first) = seen.get(&key) {
            return Err(SyncError::DuplicateName {
                name: key,
                first: first.clone(),
                second: dir,
            });
        }
        seen.insert(key, dir);
    }
    Ok(())
}

/// List every immediate subdirectory of `root` that contains a `SKILL.md`.
///
/// Top-level loose files (a `README.md`, `index.json`, …) and subdirectories
/// without a `SKILL.md` are ignored — only the `<name>/SKILL.md` shape imports.
fn walk_skill_dirs(root: &Path) -> Result<Vec<PathBuf>, SyncError> {
    let mut dirs = Vec::new();
    let entries = std::fs::read_dir(root).map_err(|source| SyncError::Io {
        path: root.to_path_buf(),
        source,
    })?;
    for entry in entries {
        let entry = entry.map_err(|source| SyncError::Io {
            path: root.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        if path.is_dir() && path.join(SKILL_MD).is_file() {
            dirs.push(path);
        }
    }
    Ok(dirs)
}

/// Parse one skill directory: read+split its `SKILL.md`, then collect every
/// file beneath the dir (including the `SKILL.md` itself) as an ordered file set.
fn parse_skill_dir(dir: &Path) -> Result<ParsedSkill, SyncError> {
    let skill_md = dir.join(SKILL_MD);
    let raw = std::fs::read_to_string(&skill_md).map_err(|source| SyncError::Io {
        path: skill_md.clone(),
        source,
    })?;
    let parsed = parse_skill_md(&raw, &skill_md)?;
    let files = collect_assets(dir)?;
    Ok(ParsedSkill {
        name: parsed.0,
        description: parsed.1,
        content: parsed.2,
        files,
    })
}

/// Split a `SKILL.md` into `(name, description, body)`.
///
/// Expects a leading YAML frontmatter block delimited by `---` lines; the
/// frontmatter must carry a non-empty `name:`. Returns [`SyncError::Malformed`]
/// (pointing at `path`) when the block is absent or `name:` is missing/empty.
fn parse_skill_md(raw: &str, path: &Path) -> Result<(String, Option<String>, String), SyncError> {
    let (frontmatter, body) = split_frontmatter(raw).ok_or_else(|| SyncError::Malformed {
        path: path.to_path_buf(),
    })?;

    let doc: serde_yaml::Value =
        serde_yaml::from_str(frontmatter).map_err(|_| SyncError::Malformed {
            path: path.to_path_buf(),
        })?;

    let name = doc
        .get("name")
        .and_then(serde_yaml::Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| SyncError::Malformed {
            path: path.to_path_buf(),
        })?
        .to_string();

    let description = doc
        .get("description")
        .and_then(serde_yaml::Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    Ok((name, description, body.to_string()))
}

/// Split a document into its leading `---`-delimited YAML frontmatter and the
/// remaining body. Returns `None` if there is no well-formed frontmatter block
/// (no leading `---`, or no closing `---`).
fn split_frontmatter(raw: &str) -> Option<(&str, &str)> {
    let rest = raw.strip_prefix("---")?;
    // The delimiter line may be `---\n` or `---\r\n`.
    let rest = rest.strip_prefix('\n').or_else(|| rest.strip_prefix("\r\n"))?;
    // Find the closing delimiter line.
    let close = find_closing_delim(rest)?;
    let frontmatter = &rest[..close.start];
    let body = &rest[close.end..];
    Some((frontmatter, body.trim_start_matches(['\n', '\r'])))
}

/// Byte range of the closing `---` delimiter line within `s`.
struct DelimSpan {
    start: usize,
    end: usize,
}

/// Locate the first line that is exactly `---` (the closing frontmatter fence).
fn find_closing_delim(s: &str) -> Option<DelimSpan> {
    let mut offset = 0usize;
    for line in s.split_inclusive('\n') {
        let trimmed = line.trim_end_matches(['\n', '\r']);
        if trimmed == "---" {
            return Some(DelimSpan {
                start: offset,
                end: offset + line.len(),
            });
        }
        offset += line.len();
    }
    None
}

/// Collect every regular file beneath `dir` (recursively) as an ordered
/// [`SkillFileInput`] keyed by its path relative to `dir`.
///
/// Paths use `/` separators (normalised from the platform separator) so the
/// stored layout is portable, and the result is sorted for deterministic
/// re-runs. The `SKILL.md` body file is included at `path = "SKILL.md"`.
fn collect_assets(dir: &Path) -> Result<Vec<SkillFileInput>, SyncError> {
    let mut files = Vec::new();
    walk_files(dir, dir, &mut files)?;
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(files)
}

/// Depth-first walk collecting regular files under `current`, keyed relative to
/// `base`.
fn walk_files(base: &Path, current: &Path, out: &mut Vec<SkillFileInput>) -> Result<(), SyncError> {
    let entries = std::fs::read_dir(current).map_err(|source| SyncError::Io {
        path: current.to_path_buf(),
        source,
    })?;
    for entry in entries {
        let entry = entry.map_err(|source| SyncError::Io {
            path: current.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        if path.is_dir() {
            walk_files(base, &path, out)?;
        } else if path.is_file() {
            let rel = path
                .strip_prefix(base)
                .unwrap_or(&path)
                .components()
                .map(|c| c.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            let content = std::fs::read_to_string(&path).map_err(|source| SyncError::Io {
                path: path.clone(),
                source,
            })?;
            out.push(SkillFileInput { path: rel, content });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_frontmatter_extracts_block_and_body() {
        let (fm, body) = split_frontmatter("---\nname: x\n---\n\nhello\n").expect("split");
        assert!(fm.contains("name: x"));
        assert_eq!(body, "hello\n");
    }

    #[test]
    fn split_frontmatter_none_without_leading_fence() {
        assert!(split_frontmatter("no frontmatter here\n").is_none());
    }

    #[test]
    fn split_frontmatter_none_without_closing_fence() {
        assert!(split_frontmatter("---\nname: x\nno close\n").is_none());
    }

    #[test]
    fn parse_skill_md_reads_name_and_description() {
        let path = Path::new("a/SKILL.md");
        let (name, desc, body) = parse_skill_md(
            "---\nname: commit\ndescription: do commits\n---\n\nbody\n",
            path,
        )
        .expect("parse");
        assert_eq!(name, "commit");
        assert_eq!(desc.as_deref(), Some("do commits"));
        assert_eq!(body, "body\n");
    }

    #[test]
    fn parse_skill_md_malformed_without_name() {
        let path = Path::new("a/SKILL.md");
        let err = parse_skill_md("---\ndescription: nope\n---\n\nbody\n", path)
            .expect_err("missing name");
        assert!(matches!(err, SyncError::Malformed { .. }));
    }

    #[test]
    fn parse_skill_md_malformed_without_frontmatter() {
        let path = Path::new("a/SKILL.md");
        let err =
            parse_skill_md("just a body, no frontmatter\n", path).expect_err("no frontmatter");
        assert!(matches!(err, SyncError::Malformed { .. }));
    }

    #[test]
    fn validate_unique_rejects_collision() {
        let mk = |name: &str| ParsedSkill {
            name: name.to_string(),
            description: None,
            content: String::new(),
            files: vec![],
        };
        // `Code Reviewer` and `code-reviewer` normalise to the same key.
        let batch = vec![mk("Code Reviewer"), mk("code-reviewer")];
        let err = validate_unique(&batch).expect_err("collision");
        assert!(matches!(err, SyncError::DuplicateName { .. }));
    }

    #[test]
    fn validate_unique_allows_distinct() {
        let mk = |name: &str| ParsedSkill {
            name: name.to_string(),
            description: None,
            content: String::new(),
            files: vec![],
        };
        validate_unique(&[mk("commit"), mk("brainstorm")]).expect("distinct ok");
    }
}
