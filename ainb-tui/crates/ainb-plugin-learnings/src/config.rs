//! Typed plugin config resolved from the injected `[plugins.learnings]`
//! table.
//!
//! The host resolves the user's `config.toml` `[plugins.learnings]` table
//! (layered project → user → system, P1) and injects it as a
//! `serde_json::Value` on `PluginInitParams.config`. The plugin parses
//! that value into [`LearningsConfig`] in `on_init` via
//! [`LearningsConfig::from_init_config`], falling back to the manifest
//! `[config]` defaults for any key the user has not overridden.
//!
//! Defaults here MUST match the `default =` values in `manifest.toml`
//! `[[config]]` so an un-configured plugin behaves identically whether
//! the host injected an empty table or the resolved-from-defaults one.
//!
//! `~`-expansion of paths is deferred to the data layer (P4) — this
//! struct stores the raw configured strings so the Settings round-trip
//! (P2) is lossless.

use serde::{Deserialize, Serialize};

/// Default learnings notes directory. Mirrors `manifest.toml`
/// `[[config]] key = "learnings_dir"`.
const DEFAULT_LEARNINGS_DIR: &str = "~/.learnings/documents/learnings";
/// Default QMD sqlite index path. Mirrors `manifest.toml`
/// `[[config]] key = "qmd_index"`.
const DEFAULT_QMD_INDEX: &str = "~/.cache/qmd/index.sqlite";
/// Default QMD collection name. Mirrors `manifest.toml`
/// `[[config]] key = "qmd_collection"`.
const DEFAULT_QMD_COLLECTION: &str = "learnings";
/// Default nano_graphrag cache dir. Mirrors `manifest.toml`
/// `[[config]] key = "graph_cache"`.
const DEFAULT_GRAPH_CACHE: &str = "~/.learnings/nano_graphrag_cache";

/// Resolved learnings plugin config.
///
/// One field per manifest `[[config]]` key. Every field has a
/// schema-matching default so a partial (or absent) injected table still
/// yields a fully-populated config.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LearningsConfig {
    /// Directory holding the `.md` + `.entities.yaml` learning notes.
    #[serde(default = "default_learnings_dir")]
    pub learnings_dir: String,
    /// Path to the QMD sqlite vector index.
    #[serde(default = "default_qmd_index")]
    pub qmd_index: String,
    /// QMD collection name to query.
    #[serde(default = "default_qmd_collection")]
    pub qmd_collection: String,
    /// nano_graphrag cache directory (graphml + community json live here).
    #[serde(default = "default_graph_cache")]
    pub graph_cache: String,
}

fn default_learnings_dir() -> String {
    DEFAULT_LEARNINGS_DIR.to_string()
}
fn default_qmd_index() -> String {
    DEFAULT_QMD_INDEX.to_string()
}
fn default_qmd_collection() -> String {
    DEFAULT_QMD_COLLECTION.to_string()
}
fn default_graph_cache() -> String {
    DEFAULT_GRAPH_CACHE.to_string()
}

impl Default for LearningsConfig {
    fn default() -> Self {
        Self {
            learnings_dir: default_learnings_dir(),
            qmd_index: default_qmd_index(),
            qmd_collection: default_qmd_collection(),
            graph_cache: default_graph_cache(),
        }
    }
}

impl LearningsConfig {
    /// Parse the config the host injected on `PluginInitParams.config`.
    ///
    /// Accepts the JSON `null` / `{}` an un-configured (ABI-2 back-compat)
    /// host sends — both resolve to [`LearningsConfig::default`]. Any
    /// recognised key present in the table overrides its default; absent
    /// keys keep the schema default. Unknown keys are ignored (forward
    /// compat — a newer config schema must not break an older plugin).
    ///
    /// This is the single call site `on_init` uses to consume the
    /// injected config. Kept pure (no `&self`, no host I/O) so the unit
    /// test can exercise every branch without a running `Server`.
    #[must_use]
    pub fn from_init_config(value: &serde_json::Value) -> Self {
        // `null` (the ABI-2 default when the host omits config) → all
        // defaults. A non-object value is treated the same way rather
        // than erroring, so a malformed injection degrades gracefully to
        // the documented defaults instead of failing init.
        if !value.is_object() {
            return Self::default();
        }
        serde_json::from_value(value.clone()).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_manifest_schema() {
        // The struct defaults must equal the manifest `[[config]] default`
        // values so an un-configured plugin behaves the same whether the
        // host injects an empty table or the resolved-from-defaults one.
        let c = LearningsConfig::default();
        assert_eq!(c.learnings_dir, "~/.learnings/documents/learnings");
        assert_eq!(c.qmd_index, "~/.cache/qmd/index.sqlite");
        assert_eq!(c.qmd_collection, "learnings");
        assert_eq!(c.graph_cache, "~/.learnings/nano_graphrag_cache");
    }

    #[test]
    fn from_null_or_empty_yields_defaults() {
        // ABI-2 back-compat: host that omits config sends JSON null.
        assert_eq!(
            LearningsConfig::from_init_config(&serde_json::Value::Null),
            LearningsConfig::default()
        );
        // Empty object (host resolved an empty `[plugins.learnings]`).
        assert_eq!(
            LearningsConfig::from_init_config(&serde_json::json!({})),
            LearningsConfig::default()
        );
    }

    #[test]
    fn partial_table_overrides_only_present_keys() {
        let injected = serde_json::json!({
            "learnings_dir": "/custom/kb",
            "qmd_collection": "my-notes"
        });
        let c = LearningsConfig::from_init_config(&injected);
        assert_eq!(c.learnings_dir, "/custom/kb");
        assert_eq!(c.qmd_collection, "my-notes");
        // Untouched keys keep the schema defaults.
        assert_eq!(c.qmd_index, "~/.cache/qmd/index.sqlite");
        assert_eq!(c.graph_cache, "~/.learnings/nano_graphrag_cache");
    }

    #[test]
    fn unknown_keys_ignored_for_forward_compat() {
        let injected = serde_json::json!({
            "learnings_dir": "/x",
            "future_key_not_in_schema": "ignored"
        });
        let c = LearningsConfig::from_init_config(&injected);
        assert_eq!(c.learnings_dir, "/x");
        assert_eq!(c.qmd_index, "~/.cache/qmd/index.sqlite");
    }
}
