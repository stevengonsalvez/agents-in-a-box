//! Data layer for the learnings browser (P4).
//!
//! Hybrid access to the three KB sources, per the design spec §C2:
//!
//! - **Browse / Detail / filters** read the `learnings_dir/*.md` notes plus
//!   their `*.entities.yaml` sidecars directly off disk
//!   ([`record`] — [`parse_learning_record`], [`scan_learnings_dir`]).
//! - **Graph** reads the nano_graphrag `*.graphml` + `kv_store_community_reports.json`
//!   directly ([`graph`] — [`parse_graphml`], [`parse_community_reports`]).
//! - **Search** shells `qmd query --json` behind the [`QmdSearch`] trait so
//!   tests inject a fixed sample ([`search`] — [`parse_qmd_json`], [`QmdCli`]).
//! - [`filter`] holds the pure scope/confidence/category/source/project
//!   predicates the Browse tab applies.
//!
//! Every reader takes an explicit path. The plugin only ever calls them with
//! paths derived from its [`LearningsConfig`](crate::config::LearningsConfig)
//! (tilde-expanded via [`expand_tilde`]), which the host has covered by the
//! `read_paths` capability envelope — the readers themselves perform no path
//! discovery outside the path handed to them.

mod error;
mod filter;
mod graph;
mod record;
mod search;

use std::path::{Path, PathBuf};

pub use error::DataError;
pub use filter::{Filter, FilterField};
pub use graph::{
    Community, DEFAULT_REL_TYPE, Edge, Graph, GraphEntity, parse_community_reports, parse_graphml,
};
pub use record::{
    Entity, LearningRecord, Provenance, Relationship, ScanReport, parse_learning_record,
    scan_learnings_dir, scan_learnings_dir_report,
};
#[cfg(test)]
pub(crate) use search::test_proc;
pub use search::{
    QmdCli, QmdSearch, SearchCancel, SearchHit, SearchMode, parse_qmd_json, search,
    search_cancellable,
};

/// Expand a leading `~` (alone or as the first path segment) to the user's
/// home directory.
///
/// `~` and `~/...` expand; any other input is returned unchanged. A `~` that
/// is not the first segment (e.g. `/x/~/y`) is left alone — only a leading
/// home reference is meaningful. If the home dir can't be resolved, the input
/// is returned verbatim rather than failing (a degenerate-environment
/// fallback; the configured path then simply won't exist and the reader
/// surfaces a typed [`DataError::Io`]).
#[must_use]
pub fn expand_tilde(path: impl AsRef<str>) -> PathBuf {
    let raw = path.as_ref();
    let Some(home) = dirs::home_dir() else {
        return PathBuf::from(raw);
    };
    if raw == "~" {
        return home;
    }
    if let Some(rest) = raw.strip_prefix("~/") {
        return home.join(rest);
    }
    PathBuf::from(raw)
}

/// Resolve a configured (possibly tilde-prefixed) path string to an absolute
/// [`PathBuf`]. Thin alias over [`expand_tilde`] kept distinct so call sites
/// in later phases read as "resolve this config value".
#[must_use]
pub fn resolve_config_path(configured: &str) -> PathBuf {
    expand_tilde(configured)
}

/// Internal helper: map a `std::io::Error` for `path` into a typed
/// [`DataError::Io`] carrying the offending path for diagnostics.
fn io_err(path: &Path, source: std::io::Error) -> DataError {
    DataError::Io {
        path: path.display().to_string(),
        detail: source.to_string(),
    }
}
