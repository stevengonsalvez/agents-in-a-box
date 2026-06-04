//! Learning-record reader: `.md` frontmatter + optional `.entities.yaml`.
//!
//! Mirrors the real `~/.learnings/documents/.../<id>.md` shape: a YAML
//! frontmatter block delimited by `---` carrying `title / category / type /
//! scope / confidence / key_insight / tags / provenance{...}`, followed by the
//! markdown body. An optional `<id>.entities.yaml` sidecar carries the extracted
//! `entities[]` + `relationships[]`. A missing sidecar yields empty vectors —
//! never a panic — because entity extraction can lag ingestion.

use std::path::Path;

use serde::{Deserialize, Serialize};

use super::{error::DataError, io_err};

/// A single learning record assembled from a `.md` + its `.entities.yaml`.
///
/// Field set is fixed by the design spec §C2. Optional provenance fields
/// (`source_tool`, `project`) are flattened up from `provenance` for the
/// filter facets so callers don't reach through `provenance` for them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LearningRecord {
    /// Stable id = the `.md` filename stem (e.g. `lrn-audit-after-rebase`).
    pub id: String,
    /// Human title from frontmatter `title`.
    pub title: String,
    /// `scope` facet (e.g. `universal` / `project`).
    pub scope: String,
    /// `confidence` in `[0, 1]`.
    pub confidence: f64,
    /// `category` facet (e.g. `process` / `reference`).
    pub category: String,
    /// Free-form tags.
    pub tags: Vec<String>,
    /// `provenance.source_tool` lifted up as a filter facet.
    pub source_tool: Option<String>,
    /// `provenance.project` lifted up as a filter facet (absent on most).
    pub project: Option<String>,
    /// One-line takeaway from frontmatter `key_insight`.
    pub key_insight: String,
    /// Markdown body after the frontmatter (the `## Problem` / `## Solution`).
    pub body_md: String,
    /// Entities from the `.entities.yaml` sidecar (empty if absent).
    pub entities: Vec<Entity>,
    /// Relationships from the sidecar (empty if absent).
    pub relationships: Vec<Relationship>,
    /// Full provenance block.
    pub provenance: Provenance,
}

/// A graph entity extracted from a learning (from the `.entities.yaml` sidecar).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entity {
    /// Entity name (the node key).
    pub name: String,
    /// Entity type (`pattern` / `tool` / `error` / `concept` / ...).
    #[serde(rename = "type", default)]
    pub entity_type: String,
    /// Free-text description.
    #[serde(default)]
    pub description: String,
}

/// A typed relationship between two entities (from the sidecar).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Relationship {
    /// Source entity name.
    pub source: String,
    /// Target entity name.
    pub target: String,
    /// Relationship type (`solves` / `caused_by` / `requires` / `relates_to`).
    #[serde(rename = "type", default)]
    pub rel_type: String,
    /// Free-text description.
    #[serde(default)]
    pub description: String,
    /// Optional 1–10 strength weight.
    #[serde(default)]
    pub strength: Option<u32>,
}

/// Provenance of a learning: where it came from and when it was ingested.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Provenance {
    /// Agent tool that produced the source (`claude` / `codex` / ...).
    #[serde(default)]
    pub source_tool: Option<String>,
    /// Absolute path of the original source note.
    #[serde(default)]
    pub source_path: Option<String>,
    /// Content hash of the source at ingest time.
    #[serde(default)]
    pub content_hash: Option<String>,
    /// RFC3339 ingest timestamp.
    #[serde(default)]
    pub ingested_at: Option<String>,
    /// Originating project, when scoped (else absent).
    #[serde(default)]
    pub project: Option<String>,
}

/// Raw frontmatter shape decoded straight from the YAML block. Kept separate
/// from [`LearningRecord`] so the public record can flatten/derive fields.
#[derive(Debug, Default, Deserialize)]
struct FrontMatter {
    #[serde(default)]
    title: String,
    #[serde(default)]
    scope: String,
    #[serde(default)]
    confidence: f64,
    #[serde(default)]
    category: String,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    key_insight: String,
    #[serde(default)]
    provenance: Provenance,
}

/// Raw sidecar shape decoded from `<id>.entities.yaml`.
#[derive(Debug, Default, Deserialize)]
struct Sidecar {
    #[serde(default)]
    entities: Vec<Entity>,
    #[serde(default)]
    relationships: Vec<Relationship>,
}

/// Parse a single learning record from its `.md` path.
///
/// Reads the frontmatter + body from `md_path`, then looks for a sibling
/// `<stem>.entities.yaml` sidecar; a missing sidecar is **not** an error — the
/// record comes back with empty `entities`/`relationships`. A missing `.md`,
/// malformed frontmatter, or malformed YAML is a typed [`DataError`].
pub fn parse_learning_record(md_path: &Path) -> Result<LearningRecord, DataError> {
    let raw = std::fs::read_to_string(md_path).map_err(|e| io_err(md_path, e))?;
    let (front_yaml, body) = split_frontmatter(&raw).ok_or_else(|| DataError::Frontmatter {
        path: md_path.display().to_string(),
        reason: "missing or unbalanced `---` frontmatter delimiters".into(),
    })?;

    let front: FrontMatter = serde_yaml::from_str(front_yaml).map_err(|e| DataError::Yaml {
        path: md_path.display().to_string(),
        detail: e.to_string(),
    })?;

    let id = md_path.file_stem().and_then(|s| s.to_str()).unwrap_or_default().to_string();

    let (entities, relationships) = load_sidecar(md_path)?;

    Ok(LearningRecord {
        id,
        title: front.title,
        scope: front.scope,
        confidence: front.confidence,
        category: front.category,
        tags: front.tags,
        source_tool: front.provenance.source_tool.clone(),
        project: front.provenance.project.clone(),
        key_insight: front.key_insight,
        body_md: body.to_string(),
        entities,
        relationships,
        provenance: front.provenance,
    })
}

/// Outcome of a directory scan: the parsed records plus a count of `.md`
/// files that *looked like* records but failed to parse (corrupt notes).
///
/// The count deliberately excludes non-record `.md` files (a `README.md` with
/// no frontmatter is expected in a real KB and is not an error) — only a note
/// that opened a frontmatter block but whose YAML / sidecar was malformed is
/// counted, so the Browse status line can honestly report `N notes failed to
/// parse` without crying wolf over every README.
#[derive(Debug, Clone, Default)]
pub struct ScanReport {
    /// Successfully parsed records, sorted stably by `id`.
    pub records: Vec<LearningRecord>,
    /// Number of `.md` files that were records (had a frontmatter block) but
    /// failed to parse — corrupt frontmatter YAML or an unreadable sidecar.
    pub failed_count: usize,
}

/// Scan a learnings directory, parsing every `*.md` into a [`LearningRecord`].
///
/// Thin wrapper over [`scan_learnings_dir_report`] that discards the
/// corrupt-note count — kept for callers that only want the records (the P4
/// data-layer tests, simple consumers). See that function for the full
/// skip/count semantics.
pub fn scan_learnings_dir(dir: &Path) -> Result<Vec<LearningRecord>, DataError> {
    Ok(scan_learnings_dir_report(dir)?.records)
}

/// Scan a learnings directory, returning the parsed records **and** a count of
/// corrupt notes.
///
/// Non-`.md` files (and the `.entities.yaml` sidecars themselves) are ignored
/// by extension. For the `.md` files, two distinct skip paths are
/// distinguished (the P4-review carry-in):
///
/// - **Not a record** — no frontmatter block at all (e.g. a stray `README.md`).
///   Surfaced as [`DataError::Frontmatter`]; skipped at `debug` and **not**
///   counted, because real KB dirs carry such files and they are not data loss.
/// - **Corrupt record** — a frontmatter block opened but its YAML, the file
///   read, or the `.entities.yaml` sidecar was malformed
///   ([`DataError::Yaml`] / [`DataError::Io`]). Skipped at `warn` and counted
///   in [`ScanReport::failed_count`] so the UI can surface soft data loss.
///
/// One corrupt note never blanks the whole list. Records come back sorted by
/// `id` (the OS directory order is not stable). A non-existent / unreadable
/// *directory* is still a hard [`DataError::Io`].
pub fn scan_learnings_dir_report(dir: &Path) -> Result<ScanReport, DataError> {
    let entries = std::fs::read_dir(dir).map_err(|e| io_err(dir, e))?;

    let mut records = Vec::new();
    let mut failed_count = 0usize;
    for entry in entries {
        let entry = entry.map_err(|e| io_err(dir, e))?;
        let path = entry.path();
        // `.entities.yaml` has a `.yaml` extension, not `.md`, so the
        // extension check alone excludes the sidecars and any NOTES.txt.
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        match parse_learning_record(&path) {
            Ok(rec) => records.push(rec),
            // Non-record `.md` (no frontmatter delimiters) — expected in real
            // KB dirs (README etc.). Skip quietly; do NOT count as data loss.
            Err(err @ DataError::Frontmatter { .. }) => {
                tracing::debug!(path = %path.display(), %err, "skipping non-record .md (no frontmatter)");
            }
            // Corrupt record: a real note that failed to parse. Surface it as
            // soft data loss so the UI can report a count.
            Err(err) => {
                failed_count += 1;
                tracing::warn!(path = %path.display(), %err, "skipping corrupt learning note");
            }
        }
    }
    records.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(ScanReport {
        records,
        failed_count,
    })
}

/// Split a `.md` body into `(frontmatter_yaml, markdown_body)`.
///
/// Expects a leading `---` line, a closing `---` line, and the body after it.
/// Returns `None` if the opening or closing delimiter is absent. Tolerant of a
/// leading BOM / blank lines before the first `---`.
fn split_frontmatter(raw: &str) -> Option<(&str, &str)> {
    let trimmed = raw.trim_start_matches('\u{feff}');
    let rest = trimmed.strip_prefix("---\n").or_else(|| {
        // Tolerate `---\r\n`.
        trimmed.strip_prefix("---\r\n")
    })?;
    // Find the closing delimiter at the start of a line.
    let close = find_closing_delimiter(rest)?;
    let (front, after) = rest.split_at(close.start);
    let body = &after[close.len..];
    Some((front, body.trim_start_matches('\n')))
}

/// Position of a closing `---` delimiter line within `rest`.
struct Delim {
    start: usize,
    len: usize,
}

/// Locate the closing `---` line: a line consisting solely of `---`.
fn find_closing_delimiter(rest: &str) -> Option<Delim> {
    let mut offset = 0usize;
    for line in rest.split_inclusive('\n') {
        let content = line.trim_end_matches(['\n', '\r']);
        if content == "---" {
            return Some(Delim {
                start: offset,
                len: line.len(),
            });
        }
        offset += line.len();
    }
    None
}

/// Load the `<stem>.entities.yaml` sidecar next to `md_path`.
///
/// A missing sidecar returns `(empty, empty)`. A present-but-malformed sidecar
/// is a typed [`DataError::Yaml`].
fn load_sidecar(md_path: &Path) -> Result<(Vec<Entity>, Vec<Relationship>), DataError> {
    let sidecar_path = md_path.with_extension("entities.yaml");
    let raw = match std::fs::read_to_string(&sidecar_path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((Vec::new(), Vec::new())),
        Err(e) => return Err(io_err(&sidecar_path, e)),
    };
    let sidecar: Sidecar = serde_yaml::from_str(&raw).map_err(|e| DataError::Yaml {
        path: sidecar_path.display().to_string(),
        detail: e.to_string(),
    })?;
    Ok((sidecar.entities, sidecar.relationships))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_frontmatter_separates_yaml_and_body() {
        let raw = "---\ntitle: x\n---\n\n## Body\ntext\n";
        let (front, body) = split_frontmatter(raw).expect("split");
        assert_eq!(front, "title: x\n");
        assert!(body.starts_with("## Body"));
    }

    #[test]
    fn split_frontmatter_none_without_closing() {
        assert!(split_frontmatter("---\ntitle: x\nno closing").is_none());
    }

    #[test]
    fn split_frontmatter_none_without_opening() {
        assert!(split_frontmatter("title: x\n---\n").is_none());
    }
}
