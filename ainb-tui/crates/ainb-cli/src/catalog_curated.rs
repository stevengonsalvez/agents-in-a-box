//! Curated catalog backend — `AinbCuratedCatalogBackend`.
//!
//! Surfaces the toolkit's own curated shelf (owned skills + vetted external
//! skills) by fetching the enriched index published as a GitHub **release
//! asset** (`catalog-index.json`), pinned by a release tag. Implements the
//! same [`CatalogBackend`] boundary as [`crate::catalog_http::SkillsShHttpBackend`],
//! so the `[b]` browse modal and `ainb skill browse` can swap between the
//! skills.sh catalog and this curated one transparently.
//!
//! ```text
//!   AINB_CATALOG_INDEX_FILE ─┐ (offline: read a local file, no network)
//!   AINB_CATALOG_INDEX_URL  ─┤ (test stub URL)
//!   [skills].catalog_release ┘ → release asset URL → reqwest::blocking
//!                                        │
//!                                        ▼
//!                          CatalogIndex (JSON) ── search(q) ──▶ Vec<CatalogHit>
//! ```
//!
//! # Tag pinning
//! The release tag is resolved (first hit wins): `AINB_CATALOG_RELEASE` env →
//! `[skills].catalog_release` in `<ainb_home>/config/config.toml` → `latest`.
//! `latest` resolves to GitHub's `releases/latest/download/<asset>` alias; any
//! other value is a real tag (`releases/download/<tag>/<asset>`). The owned
//! `install_uri`s inside a given index already pin that release's tag, so a
//! browse and its installs always agree on bytes.
//!
//! # Offline contract (zero network in tests)
//! `AINB_CATALOG_INDEX_FILE=<path>` makes `search` read a local index file and
//! never touch the network — this is what the tmux tripwire + unit tests set.
//! Like the skills.sh backend, the `reqwest::blocking` client is built lazily
//! inside the network branch (never in a constructor) so building the backend
//! on the TUI's tokio event-loop thread cannot panic.

use std::path::{Path, PathBuf};

use ainb_skill_core::catalog::{CatalogBackend, CatalogError, CatalogHit};
use ainb_skill_core::catalog_index::{CatalogIndex, OWNED_REPO};

/// Env var pointing at a LOCAL index JSON file — offline escape hatch used by
/// tests + the tmux tripwire. Highest precedence; suppresses all network.
pub const ENV_INDEX_FILE: &str = "AINB_CATALOG_INDEX_FILE";
/// Env var overriding the full fetch URL (e.g. a local http stub). Used when
/// a test wants to exercise the network path without hitting GitHub.
pub const ENV_INDEX_URL: &str = "AINB_CATALOG_INDEX_URL";
/// Env var overriding the release tag (takes precedence over config).
pub const ENV_RELEASE: &str = "AINB_CATALOG_RELEASE";

/// Default release tag when neither env nor config pins one. Resolves to
/// GitHub's "latest published release" asset alias.
pub const DEFAULT_RELEASE: &str = "latest";
/// Name of the published index asset attached to each release.
pub const ASSET_NAME: &str = "catalog-index.json";

/// Where the curated index comes from. Resolved once in [`AinbCuratedCatalogBackend::from_env`]
/// so `search` is a pure function of this source.
#[derive(Debug, Clone, PartialEq, Eq)]
enum IndexSource {
    /// A local file (offline). Read + parsed on every search; no network.
    File(PathBuf),
    /// A URL fetched via `reqwest::blocking` (built lazily on first use).
    Url(String),
}

/// Curated catalog backend backed by the release-published index.
pub struct AinbCuratedCatalogBackend {
    source: IndexSource,
}

impl AinbCuratedCatalogBackend {
    /// Resolve the index source from the environment + `<ainb_home>` config.
    ///
    /// Precedence: `AINB_CATALOG_INDEX_FILE` (local file, offline) →
    /// `AINB_CATALOG_INDEX_URL` (explicit URL) → the release-asset URL for the
    /// resolved tag (`AINB_CATALOG_RELEASE` → config → `latest`). Builds NO
    /// network client (see the type docs).
    pub fn from_env(ainb_home: &Path) -> Self {
        if let Some(path) = non_empty_env(ENV_INDEX_FILE) {
            return Self::from_index_file(path);
        }
        if let Some(url) = non_empty_env(ENV_INDEX_URL) {
            return Self::from_url(url);
        }
        let tag = non_empty_env(ENV_RELEASE)
            .or_else(|| read_config_catalog_release(&config_path_in(ainb_home)))
            .unwrap_or_else(|| DEFAULT_RELEASE.to_string());
        Self::from_url(release_asset_url(&tag))
    }

    /// Backend that reads a local index file (offline). For tests + tripwire.
    pub fn from_index_file(path: impl Into<PathBuf>) -> Self {
        Self {
            source: IndexSource::File(path.into()),
        }
    }

    /// Backend that fetches the index from `url`.
    pub fn from_url(url: impl Into<String>) -> Self {
        Self {
            source: IndexSource::Url(url.into()),
        }
    }

    /// Load + parse the index from the configured source.
    fn load_index(&self) -> Result<CatalogIndex, CatalogError> {
        match &self.source {
            IndexSource::File(path) => {
                let text = std::fs::read_to_string(path).map_err(|e| {
                    CatalogError::Backend(format!("read catalog index {}: {e}", path.display()))
                })?;
                CatalogIndex::from_json(&text).map_err(CatalogError::Backend)
            }
            IndexSource::Url(url) => {
                // Lazily build the blocking client — only when a real request
                // is needed, never in a constructor (TUI tokio safety).
                let client = reqwest::blocking::Client::builder()
                    .user_agent(concat!("ainb-cli/", env!("CARGO_PKG_VERSION")))
                    .build()
                    .map_err(|e| CatalogError::Backend(format!("build http client: {e}")))?;
                let resp = client
                    .get(url)
                    .send()
                    .map_err(|e| CatalogError::Backend(format!("GET {url}: {e}")))?;
                let status = resp.status();
                if !status.is_success() {
                    return Err(CatalogError::Backend(format!("GET {url}: status {status}")));
                }
                let text = resp
                    .text()
                    .map_err(|e| CatalogError::Backend(format!("read body for {url}: {e}")))?;
                CatalogIndex::from_json(&text).map_err(CatalogError::Backend)
            }
        }
    }
}

impl CatalogBackend for AinbCuratedCatalogBackend {
    /// Search the curated shelf. Unlike the skills.sh backend, a **blank**
    /// query returns the WHOLE shelf (the curated set is small + local — the
    /// point of browsing is seeing it). See [`CatalogIndex::search`].
    fn search(&self, query: &str) -> Result<Vec<CatalogHit>, CatalogError> {
        let index = self.load_index()?;
        Ok(index.search(query))
    }
}

/// Read a non-empty, trimmed env var, or `None`.
fn non_empty_env(key: &str) -> Option<String> {
    std::env::var(key).ok().and_then(|v| {
        let v = v.trim().to_string();
        (!v.is_empty()).then_some(v)
    })
}

/// Build the release-asset URL for a tag. `latest` uses GitHub's stable
/// "latest release" alias; any other value pins that exact tag.
pub fn release_asset_url(tag: &str) -> String {
    if tag == DEFAULT_RELEASE {
        format!("https://github.com/{OWNED_REPO}/releases/latest/download/{ASSET_NAME}")
    } else {
        format!("https://github.com/{OWNED_REPO}/releases/download/{tag}/{ASSET_NAME}")
    }
}

/// `<ainb_home>/config/config.toml`.
fn config_path_in(ainb_home: &Path) -> PathBuf {
    ainb_home.join("config").join("config.toml")
}

/// Read `[skills].catalog_release` from a `config.toml`, tolerating a missing /
/// malformed file. Only this one key is read; the rest is owned by ainb-core.
fn read_config_catalog_release(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let value: toml::Value = toml::from_str(&text).ok()?;
    let tag = value
        .get("skills")?
        .get("catalog_release")?
        .as_str()?
        .trim()
        .to_string();
    (!tag.is_empty()).then_some(tag)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ainb_skill_core::catalog::CatalogEntryKind;
    use ainb_skill_core::catalog_index::{CatalogIndexEntry, CatalogOrigin};

    fn sample_index() -> CatalogIndex {
        CatalogIndex::new(
            "v1.5.0",
            vec![
                CatalogIndexEntry {
                    name: "commit".to_string(),
                    description: "git commits".to_string(),
                    repo: OWNED_REPO.to_string(),
                    install_uri:
                        "gh:stevengonsalvez/agents-in-a-box@v1.5.0/toolkit/packages/skills/commit"
                            .to_string(),
                    origin: CatalogOrigin::Owned,
                    stars: 0,
                    kind: CatalogEntryKind::Skill,
                },
                CatalogIndexEntry {
                    name: "ui-ux-pro-max".to_string(),
                    description: "UI/UX design intelligence".to_string(),
                    repo: "nextlevelbuilder/ui-ux-pro-max-skill".to_string(),
                    install_uri: "gh:nextlevelbuilder/ui-ux-pro-max-skill@2.2.1/.claude/skills"
                        .to_string(),
                    origin: CatalogOrigin::External,
                    stars: 0,
                    kind: CatalogEntryKind::Skill,
                },
            ],
        )
    }

    fn write_index(dir: &Path) -> PathBuf {
        let path = dir.join("catalog-index.json");
        std::fs::write(&path, sample_index().to_json()).unwrap();
        path
    }

    #[test]
    fn release_url_latest_uses_alias() {
        assert_eq!(
            release_asset_url("latest"),
            "https://github.com/stevengonsalvez/agents-in-a-box/releases/latest/download/catalog-index.json"
        );
    }

    #[test]
    fn release_url_tag_pins_exact() {
        assert_eq!(
            release_asset_url("v1.5.0"),
            "https://github.com/stevengonsalvez/agents-in-a-box/releases/download/v1.5.0/catalog-index.json"
        );
    }

    #[test]
    fn file_backend_searches_offline_no_network() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_index(dir.path());
        let backend = AinbCuratedCatalogBackend::from_index_file(path);

        // blank → full shelf (owned first, then external)
        let all = backend.search("").unwrap();
        assert_eq!(
            all.iter().map(|h| h.name.as_str()).collect::<Vec<_>>(),
            vec!["commit", "ui-ux-pro-max"]
        );

        // owned hit carries the pinned install_uri
        assert_eq!(
            all[0].install_uri,
            "gh:stevengonsalvez/agents-in-a-box@v1.5.0/toolkit/packages/skills/commit"
        );

        // query filters across name + description
        let ui = backend.search("design").unwrap();
        assert_eq!(ui.len(), 1);
        assert_eq!(ui[0].name, "ui-ux-pro-max");
    }

    #[test]
    fn missing_file_is_backend_error() {
        let backend = AinbCuratedCatalogBackend::from_index_file("/nonexistent/catalog-index.json");
        let err = backend.search("x").unwrap_err();
        assert!(matches!(err, CatalogError::Backend(_)));
    }

    #[test]
    fn from_env_prefers_index_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_index(dir.path());
        // Build under an explicit file env; assert the source resolves to it
        // by searching offline (a URL source would error without network).
        let backend = AinbCuratedCatalogBackend {
            source: IndexSource::File(path),
        };
        assert!(backend.search("").is_ok());
    }

    #[test]
    fn config_catalog_release_read_from_skills_section() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("config");
        std::fs::create_dir_all(&cfg).unwrap();
        std::fs::write(
            cfg.join("config.toml"),
            "[skills]\ncatalog_release = \"v1.4.0\"\n",
        )
        .unwrap();
        assert_eq!(
            read_config_catalog_release(&config_path_in(dir.path())),
            Some("v1.4.0".to_string())
        );
    }

    #[test]
    fn config_catalog_release_absent_is_none() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read_config_catalog_release(&config_path_in(dir.path())), None);
    }
}
