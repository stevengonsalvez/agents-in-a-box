//! Marketplace + lockfile schema for the ainb plugin installer.
//!
//! Phase 4 user-facing surface: a marketplace is a JSON catalog at a known
//! repo path (`<repo>/.ainb-plugin/marketplace.json`) that lists plugins and
//! how to fetch their compiled `.wasm`. The host caches each registered
//! marketplace under `~/.agents-in-a-box/plugins/marketplaces/<name>/` and
//! mirrors the on-disk lockfile (`installed.toml`) so update flows can
//! detect new capability requests and re-prompt the user.
//!
//! Schema mirrors Claude Code's `.claude-plugin/marketplace.json` so authors
//! who already publish there can ship to ainb without learning a second
//! shape.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// One marketplace catalog. Deserialised from `marketplace.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Marketplace {
    /// Stable identifier — used as the on-disk directory name under
    /// `marketplaces/<name>/`. Authors should keep this URL-safe.
    pub name: String,
    /// Plugin entries. Order is informational only; lookup is by `name`.
    #[serde(default)]
    pub plugins: Vec<MarketplaceEntry>,
}

/// A single plugin entry inside a marketplace catalog.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarketplaceEntry {
    /// Plugin id (matches `[plugin].name` in the plugin's `plugin.toml`).
    pub name: String,
    /// Semver string (matches `[plugin].version`).
    pub version: String,
    /// Source repo URL — the marketplace is fetched via `git clone --depth 1`
    /// to read this catalog; the binary is fetched separately.
    pub repo: String,
    /// Tag or commit to read the manifest from. For binary download this is
    /// substituted into `release_url` as `{tag}`.
    pub git_ref: String,
    /// Relative path to `plugin.toml` inside the repo (e.g.
    /// `crates/ainb-plugin-burndown/plugin.toml`). Used for offline manifest
    /// inspection during install.
    pub manifest_path: String,
    /// Minimum host version this plugin is compatible with (semver).
    pub ainb_min_version: String,
    /// Optional template for fetching the compiled `.wasm` artefact.
    /// Substitutions: `{owner}`, `{repo}`, `{tag}`, `{plugin}`.
    /// When absent the host falls back to looking up the artefact via
    /// `release_url` on the marketplace itself; if both are absent install
    /// fails with a clear error.
    #[serde(default)]
    pub release_url: Option<String>,
}

impl Marketplace {
    /// Parse a `marketplace.json` source. Returns `serde_json::Error` rather
    /// than the host's `anyhow::Error` so callers can present targeted
    /// validation messages.
    pub fn from_json(s: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(s)
    }

    /// Find the entry matching `name`. Picks the highest semver-orderable
    /// version when multiple entries share a name (rare; primarily for
    /// transition windows when a plugin ships v0.1 + v0.2 simultaneously).
    #[must_use]
    pub fn lookup<'a>(
        &'a self,
        name: &str,
        version: Option<&str>,
    ) -> Option<&'a MarketplaceEntry> {
        let candidates: Vec<&MarketplaceEntry> =
            self.plugins.iter().filter(|p| p.name == name).collect();
        if candidates.is_empty() {
            return None;
        }
        if let Some(v) = version {
            return candidates.into_iter().find(|p| p.version == v);
        }
        // No version pinned: pick the lexicographically largest version
        // string. Phase 4 doesn't yet pull in a full semver crate; the
        // first-party catalog only ships a single version per plugin so the
        // simple max() is enough today.
        candidates.into_iter().max_by(|a, b| a.version.cmp(&b.version))
    }
}

/// One entry inside the on-disk lockfile (`installed.toml`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockEntry {
    /// Marketplace name this plugin was installed from.
    pub marketplace: String,
    /// Pinned version that was installed.
    pub version: String,
    /// ISO 8601 timestamp.
    pub installed_at: String,
    /// Capabilities the user explicitly approved at install time. Compared
    /// against the new plugin's manifest on `update` — any new capability
    /// triggers an interactive re-prompt.
    #[serde(default)]
    pub capabilities_approved: Vec<String>,
}

/// Top-level lockfile shape — flat map of `plugin_name -> LockEntry`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Lockfile {
    pub entries: BTreeMap<String, LockEntry>,
}

impl Lockfile {
    /// Parse a TOML lockfile source.
    pub fn from_toml(s: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(s)
    }

    /// Render the lockfile back to TOML.
    pub fn to_toml(&self) -> Result<String, toml::ser::Error> {
        toml::to_string(&self)
    }

    /// Add or replace an entry.
    pub fn insert(&mut self, plugin: impl Into<String>, entry: LockEntry) {
        self.entries.insert(plugin.into(), entry);
    }

    /// Remove an entry. Returns whether it was present.
    pub fn remove(&mut self, plugin: &str) -> bool {
        self.entries.remove(plugin).is_some()
    }

    /// Look up an entry without removing it.
    #[must_use]
    pub fn get(&self, plugin: &str) -> Option<&LockEntry> {
        self.entries.get(plugin)
    }
}

/// Compare two capability lists and return the set of capabilities present
/// in `incoming` but not in `approved`. Strings are compared verbatim.
#[must_use]
pub fn new_capabilities(approved: &[String], incoming: &[String]) -> Vec<String> {
    incoming
        .iter()
        .filter(|c| !approved.iter().any(|a| a == *c))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_marketplace() {
        let src = r#"{
            "name": "ainb-plugins",
            "plugins": [
                {
                    "name": "burndown",
                    "version": "0.1.0",
                    "repo": "https://github.com/example/repo",
                    "git_ref": "main",
                    "manifest_path": "plugin.toml",
                    "ainb_min_version": "1.1.0"
                }
            ]
        }"#;
        let m = Marketplace::from_json(src).expect("parses");
        assert_eq!(m.name, "ainb-plugins");
        assert_eq!(m.plugins.len(), 1);
        assert_eq!(m.plugins[0].name, "burndown");
        assert!(m.plugins[0].release_url.is_none());
    }

    #[test]
    fn parses_marketplace_with_release_url_template() {
        let src = r#"{
            "name": "third-party",
            "plugins": [
                {
                    "name": "extra",
                    "version": "0.2.0",
                    "repo": "https://example.com/r",
                    "git_ref": "v0.2.0",
                    "manifest_path": "p.toml",
                    "ainb_min_version": "1.1.0",
                    "release_url": "https://example.com/{plugin}-{tag}.wasm"
                }
            ]
        }"#;
        let m = Marketplace::from_json(src).expect("parses");
        assert_eq!(
            m.plugins[0].release_url.as_deref(),
            Some("https://example.com/{plugin}-{tag}.wasm")
        );
    }

    #[test]
    fn lookup_picks_explicit_version() {
        let m = Marketplace {
            name: "x".into(),
            plugins: vec![
                MarketplaceEntry {
                    name: "p".into(),
                    version: "0.1.0".into(),
                    repo: "r".into(),
                    git_ref: "v0.1.0".into(),
                    manifest_path: "p.toml".into(),
                    ainb_min_version: "1.1.0".into(),
                    release_url: None,
                },
                MarketplaceEntry {
                    name: "p".into(),
                    version: "0.2.0".into(),
                    repo: "r".into(),
                    git_ref: "v0.2.0".into(),
                    manifest_path: "p.toml".into(),
                    ainb_min_version: "1.1.0".into(),
                    release_url: None,
                },
            ],
        };
        let pinned = m.lookup("p", Some("0.1.0")).expect("found");
        assert_eq!(pinned.version, "0.1.0");
        let latest = m.lookup("p", None).expect("found");
        assert_eq!(latest.version, "0.2.0");
        assert!(m.lookup("missing", None).is_none());
    }

    #[test]
    fn rejects_malformed_json() {
        let bad = r#"{ "name": "broken", "plugins": [ { "name": "x" } "#; // truncated
        assert!(Marketplace::from_json(bad).is_err());
    }

    #[test]
    fn rejects_missing_required_fields() {
        let bad = r#"{ "name": "ok", "plugins": [ { "name": "x" } ] }"#;
        // version + repo + git_ref + manifest_path + ainb_min_version are required.
        assert!(Marketplace::from_json(bad).is_err());
    }

    #[test]
    fn lockfile_round_trip() {
        let mut lf = Lockfile::default();
        lf.insert(
            "burndown",
            LockEntry {
                marketplace: "ainb-plugins".into(),
                version: "0.1.0".into(),
                installed_at: "2026-05-08T00:00:00Z".into(),
                capabilities_approved: vec![
                    "read_sessions".into(),
                    "write_plugin_data".into(),
                ],
            },
        );
        let s = lf.to_toml().expect("encodes");
        let back = Lockfile::from_toml(&s).expect("decodes");
        assert_eq!(lf, back);
        assert!(back.get("burndown").is_some());
    }

    #[test]
    fn diff_capabilities_finds_new_ones() {
        let approved = vec!["read_sessions".to_string()];
        let incoming = vec![
            "read_sessions".to_string(),
            "write_plugin_data".to_string(),
            "network".to_string(),
        ];
        let new = new_capabilities(&approved, &incoming);
        assert_eq!(new, vec!["write_plugin_data", "network"]);
    }

    #[test]
    fn diff_capabilities_handles_revoked_set() {
        let approved = vec![
            "read_sessions".to_string(),
            "write_plugin_data".to_string(),
        ];
        let incoming = vec!["read_sessions".to_string()];
        // Removed caps don't trigger a re-prompt; only new ones do.
        assert!(new_capabilities(&approved, &incoming).is_empty());
    }
}
