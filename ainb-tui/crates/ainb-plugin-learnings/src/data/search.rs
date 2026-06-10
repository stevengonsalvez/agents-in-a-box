//! Semantic search: shell `qmd query --json` behind a trait.
//!
//! The Search tab runs the user's query through QMD's hybrid search. The shell
//! call sits behind the [`QmdSearch`] trait so tests inject a captured sample
//! (`tests/fixtures/kb/qmd_query_sample.json`) without spawning a subprocess;
//! [`QmdCli`] is the real runner used at runtime. The wire shape is fixed by
//! `qmd query --json` (a JSON array of `{docid, score, file, title, snippet}`,
//! captured 2026-06-04) and parsed by [`parse_qmd_json`].
//!
//! **Two search modes, one wire shape.** `qmd` exposes two ranked-search paths
//! that emit the SAME `--json` row shape, so both reuse [`parse_qmd_json`]:
//!
//! - [`SearchMode::Bm25`] → `qmd search <q> --json`: BM25 full-text only, NO
//!   LLM, effectively instant. Used for the two-stage "fast paint".
//! - [`SearchMode::Semantic`] → `qmd query <q> --json`: the hybrid
//!   LLM-expansion + rerank path. Higher quality, but cold-slow (~14 s while
//!   `qmd` runs its query-expansion). Swapped in to replace the BM25 paint once
//!   it lands.
//!
//! Crucially, neither call runs on the plugin's dispatch thread — the Search
//! tab fires them on a worker thread and polls for the result, so a slow (or
//! hung) `qmd` never freezes the Learnings pane (see [`crate::ui`] search).

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

/// Which `qmd` search path to run. Both emit the same `--json` row shape, so
/// [`parse_qmd_json`] is reusable across both.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SearchMode {
    /// `qmd search <q> --json` — BM25 full-text, NO LLM, effectively instant.
    /// The two-stage "fast paint" first stage.
    Bm25,
    /// `qmd query <q> --json` — hybrid LLM-expansion + rerank. Higher quality
    /// but cold-slow; the default and the second-stage swap-in.
    #[default]
    Semantic,
}

/// Abstraction over the `qmd` shell so search is testable without a subprocess.
///
/// Implementors return the raw `qmd query --json` stdout for a query; the
/// caller ([`search`]) parses it. Tests supply a fake returning a captured
/// sample; production uses [`QmdCli`].
pub trait QmdSearch {
    /// Run `qmd query <query> --json` against `collection` using the sqlite
    /// `index` path, returning raw stdout JSON. The semantic (hybrid) path.
    fn run_query(&self, query: &str, collection: &str, index: &str) -> Result<String, DataError>;

    /// Run the BM25 full-text path (`qmd search <query> --json`). Defaults to
    /// [`Self::run_query`] so existing fakes (which only implement `run_query`)
    /// keep working unchanged — they simply return the same payload for both
    /// modes, which is exactly what a deterministic test fake wants. The real
    /// [`QmdCli`] overrides this to shell the faster `qmd search` sub-command.
    fn run_bm25(&self, query: &str, collection: &str, index: &str) -> Result<String, DataError> {
        self.run_query(query, collection, index)
    }
}

/// Run a search through any [`QmdSearch`] runner and parse the result.
///
/// This is the single entry point the Search tab calls; swapping the runner
/// (real vs fake) changes nothing else. `mode` selects the BM25 fast path vs
/// the semantic rerank path — both parse through [`parse_qmd_json`].
pub fn search(
    runner: &dyn QmdSearch,
    query: &str,
    collection: &str,
    index: &str,
    mode: SearchMode,
) -> Result<Vec<SearchHit>, DataError> {
    let raw = match mode {
        SearchMode::Bm25 => runner.run_bm25(query, collection, index),
        SearchMode::Semantic => runner.run_query(query, collection, index),
    }?;
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

    /// Spawn `qmd` with the given pre-built argument list, capturing stdout as
    /// the parsed JSON payload. Shared by the BM25 (`search`) and semantic
    /// (`query`) paths so both surface the same typed subprocess errors.
    fn run_args(&self, args: &[&str]) -> Result<String, DataError> {
        let output = Command::new(self.program())
            .args(args)
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

impl QmdSearch for QmdCli {
    fn run_query(&self, query: &str, collection: &str, _index: &str) -> Result<String, DataError> {
        // `qmd query <q> --json [-c <collection>] -C 20`. `_index` is
        // intentionally not forwarded — see the type doc: QMD's `--index` takes
        // a named index (not the configured sqlite path) and path-valued
        // `--index` trips a bug in QMD itself. QMD resolves its index from its
        // own config. The `-C 20` candidate-limit caps how many BM25 candidates
        // the LLM reranks, trimming the cold rerank time without losing the
        // top results.
        let mut args: Vec<&str> = vec!["query", query, "--json"];
        if !collection.is_empty() {
            args.extend_from_slice(&["-c", collection]);
        }
        args.extend_from_slice(&["-C", "20"]);
        self.run_args(&args)
    }

    fn run_bm25(&self, query: &str, collection: &str, _index: &str) -> Result<String, DataError> {
        // `qmd search <q> --json [-c <collection>]` — BM25 full-text, no LLM
        // expansion / rerank, so it returns effectively instantly. Same `--json`
        // row shape as `query`, so the caller parses it the same way. `_index`
        // is unused for the same reason as the semantic path.
        let mut args: Vec<&str> = vec!["search", query, "--json"];
        if !collection.is_empty() {
            args.extend_from_slice(&["-c", collection]);
        }
        self.run_args(&args)
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
