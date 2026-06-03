//! Semantic search: shell `qmd query --json` behind a trait.
//!
//! The Search tab runs the user's query through QMD's hybrid search. The shell
//! call sits behind the [`QmdSearch`] trait so tests inject a captured sample
//! (`tests/fixtures/kb/qmd_query_sample.json`) without spawning a subprocess;
//! [`QmdCli`] is the real runner used at runtime. The wire shape is fixed by
//! `qmd query --json` (a JSON array of `{docid, score, file, title, snippet}`,
//! captured 2026-06-04) and parsed by [`parse_qmd_json`].

use std::process::Command;

use serde::Deserialize;

use super::error::DataError;

/// A ranked semantic-search hit.
///
/// Maps the `qmd query --json` row to the design's `SearchHit{id, score,
/// title}`, keeping `file` as the locator the Detail pane uses to open the
/// underlying note.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchHit {
    /// QMD doc id (e.g. `#114073`).
    pub id: String,
    /// Relevance score (higher = more relevant).
    pub score: f64,
    /// Document title.
    pub title: String,
    /// `qmd://...` locator, if present in the row.
    pub file: Option<String>,
}

/// Raw `qmd query --json` row.
#[derive(Debug, Deserialize)]
struct RawHit {
    #[serde(default)]
    docid: String,
    #[serde(default)]
    score: f64,
    #[serde(default)]
    title: String,
    #[serde(default)]
    file: Option<String>,
}

/// Parse a `qmd query --json` payload into ranked [`SearchHit`]s.
///
/// The payload is a JSON array; rows are returned in the order QMD ranked them
/// (it sorts by score, so no re-sort here). Malformed JSON is a typed
/// [`DataError::Json`].
pub fn parse_qmd_json(json: &str) -> Result<Vec<SearchHit>, DataError> {
    let rows: Vec<RawHit> = serde_json::from_str(json).map_err(|e| DataError::Json {
        path: "<qmd output>".into(),
        detail: e.to_string(),
    })?;
    Ok(rows
        .into_iter()
        .map(|r| SearchHit {
            id: r.docid,
            score: r.score,
            title: r.title,
            file: r.file,
        })
        .collect())
}

/// Abstraction over the `qmd` shell so search is testable without a subprocess.
///
/// Implementors return the raw `qmd query --json` stdout for a query; the
/// caller ([`search`]) parses it. Tests supply a fake returning a captured
/// sample; production uses [`QmdCli`].
pub trait QmdSearch {
    /// Run `qmd query <query> --json` against `collection` using the sqlite
    /// `index` path, returning raw stdout JSON.
    fn run_query(&self, query: &str, collection: &str, index: &str) -> Result<String, DataError>;
}

/// Run a search through any [`QmdSearch`] runner and parse the result.
///
/// This is the single entry point the Search tab calls; swapping the runner
/// (real vs fake) changes nothing else.
pub fn search(
    runner: &dyn QmdSearch,
    query: &str,
    collection: &str,
    index: &str,
) -> Result<Vec<SearchHit>, DataError> {
    let raw = runner.run_query(query, collection, index)?;
    parse_qmd_json(&raw)
}

/// The real `qmd` runner: shells `qmd query <q> --json -c <collection>`.
///
/// **On `--index`:** QMD's `--index` flag takes a *named* index, not a sqlite
/// path, and passing the configured `qmd_index` path through it triggers a bug
/// in QMD's own `setIndexName` (verified live 2026-06-04). QMD already resolves
/// its index from its own config, so the runner does **not** forward `--index`;
/// the configured `index` value is threaded through the trait for the Search
/// tab's display + future use, but the CLI relies on QMD's default index. The
/// live smoke test exercises this end-to-end.
#[derive(Debug, Default, Clone)]
pub struct QmdCli {
    /// Binary name / path to invoke (defaults to `qmd` on `PATH`).
    binary: Option<String>,
}

impl QmdCli {
    /// Override the `qmd` binary (test seam / non-PATH installs).
    #[must_use]
    pub fn with_binary(binary: impl Into<String>) -> Self {
        Self {
            binary: Some(binary.into()),
        }
    }

    fn program(&self) -> &str {
        self.binary.as_deref().unwrap_or("qmd")
    }
}

impl QmdSearch for QmdCli {
    fn run_query(&self, query: &str, collection: &str, _index: &str) -> Result<String, DataError> {
        let mut cmd = Command::new(self.program());
        cmd.arg("query").arg(query).arg("--json");
        if !collection.is_empty() {
            cmd.arg("-c").arg(collection);
        }
        // `_index` is intentionally not forwarded — see the type doc: QMD's
        // `--index` takes a named index (not the configured sqlite path) and
        // path-valued `--index` trips a bug in QMD itself. QMD resolves its
        // index from its own config.

        let output = cmd
            .output()
            .map_err(|e| DataError::Subprocess(format!("spawning {}: {e}", self.program())))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(DataError::Subprocess(format!(
                "qmd exited {}: {}",
                output.status,
                stderr.trim()
            )));
        }
        String::from_utf8(output.stdout)
            .map_err(|e| DataError::Subprocess(format!("qmd stdout not utf-8: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_empty_array_yields_no_hits() {
        assert!(parse_qmd_json("[]").expect("empty array").is_empty());
    }

    #[test]
    fn parse_rejects_non_array() {
        let err = parse_qmd_json("{\"docid\": \"x\"}").expect_err("object is not an array");
        assert!(matches!(err, DataError::Json { .. }));
    }

    #[test]
    fn qmd_cli_program_defaults_to_qmd() {
        assert_eq!(QmdCli::default().program(), "qmd");
        assert_eq!(QmdCli::with_binary("/opt/qmd").program(), "/opt/qmd");
    }
}
