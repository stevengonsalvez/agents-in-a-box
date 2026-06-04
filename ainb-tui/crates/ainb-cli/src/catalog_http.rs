//! Production catalog backend — `SkillsShHttpBackend` (bead ai-a20).
//!
//! Talks to the skills.sh catalog over HTTP via `reqwest::blocking`,
//! parsing the response into `Vec<CatalogHit>`. Mirrors `ainb-fetch`'s
//! blocking-client style so the CLI stays synchronous.
//!
//! # API key
//! Optional. Resolved (first hit wins):
//!   1. `AINB_SKILLS_API_KEY` env var.
//!   2. `[skills].api_key` in `<ainb_home>/config/config.toml`.
//!   3. none — search is attempted unauthenticated; a 401/403 surfaces
//!      [`CatalogError::AuthRequired`].
//!
//! # Test escape hatches (so the live binary stays offline in tripwires)
//!   * `AINB_CATALOG_MOCK=1` → return canned hits, NO network. This is
//!     what the live tmux tripwire sets so `[b] browse` works without a
//!     server.
//!   * `AINB_SKILLS_API_BASE=<url>` → reroute the base host at a local
//!     stub (`wiremock` / a throwaway server). Also keeps tests offline
//!     from the real skills.sh.
//!
//! Every test in the suite injects `MockCatalogBackend` directly, so
//! this module's network path is never exercised by `cargo test`.

use std::path::{Path, PathBuf};

use ainb_skill_core::catalog::{
    is_blank_query, rank_by_stars, CatalogBackend, CatalogError, CatalogHit, SkillsShUrlBuilder,
};

/// Env var carrying the catalog API key (takes precedence over config).
pub const ENV_API_KEY: &str = "AINB_SKILLS_API_KEY";
/// Env var rerouting the catalog base URL (test stub).
pub const ENV_API_BASE: &str = "AINB_SKILLS_API_BASE";
/// Env var forcing canned data with zero network (tripwire flag).
pub const ENV_MOCK: &str = "AINB_CATALOG_MOCK";
/// Env var letting a tripwire point the first canned hit's `install_uri`
/// at a LOCAL unit (e.g. the sandbox bare remote `git:file://…`) so the
/// `[b]` → Enter install path fetches locally — never the network.
pub const ENV_MOCK_INSTALL_URI: &str = "AINB_CATALOG_MOCK_INSTALL_URI";

/// Production catalog backend hitting skills.sh over HTTP.
///
/// The reqwest client is built **lazily** inside the network branch of
/// `search`, NOT in the constructor. This matters because the TUI builds
/// this backend on the event-loop thread (a tokio runtime context), and
/// constructing a `reqwest::blocking::Client` inside a tokio runtime
/// panics. In mock mode (`AINB_CATALOG_MOCK=1`) `search` returns before
/// ever touching reqwest, so the TUI's `[b]` browse modal stays panic-
/// free and offline.
pub struct SkillsShHttpBackend {
    builder: SkillsShUrlBuilder,
    /// When `true` (driven by `AINB_CATALOG_MOCK=1`), `search` returns
    /// canned hits without any network call.
    mock_mode: bool,
}

impl SkillsShHttpBackend {
    /// Build the backend from the ambient environment + the config under
    /// `ainb_home`. Reads the API key (env → config) and honours the
    /// `AINB_SKILLS_API_BASE` / `AINB_CATALOG_MOCK` test hooks. Builds NO
    /// network client — see the type docs for why.
    pub fn from_env(ainb_home: &Path) -> Self {
        let api_key = resolve_api_key(ainb_home);
        let builder = match std::env::var(ENV_API_BASE) {
            Ok(base) if !base.trim().is_empty() => {
                SkillsShUrlBuilder::with_base(base, api_key)
            }
            _ => SkillsShUrlBuilder::new(api_key),
        };
        let mock_mode = std::env::var(ENV_MOCK).as_deref() == Ok("1");
        Self { builder, mock_mode }
    }
}

impl CatalogBackend for SkillsShHttpBackend {
    fn search(&self, query: &str) -> Result<Vec<CatalogHit>, CatalogError> {
        if is_blank_query(query) {
            return Ok(Vec::new());
        }
        // Tripwire / offline escape hatch — canned data, never a socket,
        // never a reqwest client. This is the only branch the TUI ever
        // reaches (it always sets AINB_CATALOG_MOCK=1).
        if self.mock_mode {
            return Ok(canned_hits(query));
        }

        // Lazily build the blocking client — only when a real request is
        // actually needed. The CLI dispatch path is NOT inside a tokio
        // runtime, so this is safe there.
        let client = reqwest::blocking::Client::builder()
            .user_agent(concat!("ainb-cli/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| CatalogError::Backend(format!("build http client: {e}")))?;

        let url = self.builder.search_url(query);
        let mut req = client.get(&url);
        if let Some(header) = self.builder.auth_header() {
            req = req.header(reqwest::header::AUTHORIZATION, header);
        }
        let resp = req
            .send()
            .map_err(|e| CatalogError::Backend(format!("GET {url}: {e}")))?;
        let status = resp.status();
        if status == reqwest::StatusCode::UNAUTHORIZED
            || status == reqwest::StatusCode::FORBIDDEN
        {
            return Err(CatalogError::AuthRequired);
        }
        if !status.is_success() {
            return Err(CatalogError::Backend(format!("GET {url}: status {status}")));
        }
        let body = resp
            .text()
            .map_err(|e| CatalogError::Backend(format!("read body for {url}: {e}")))?;
        let mut hits = parse_search_response(&body)?;
        rank_by_stars(&mut hits);
        Ok(hits)
    }
}

/// Parse a skills.sh `/api/search` JSON body into `Vec<CatalogHit>`.
///
/// Accepts either a bare top-level array or an object with a `results`
/// (or `hits`) array, so a minor server-shape change doesn't break the
/// client. Kept pure so it can be unit-tested without a socket.
pub fn parse_search_response(body: &str) -> Result<Vec<CatalogHit>, CatalogError> {
    let value: serde_json::Value = serde_json::from_str(body)
        .map_err(|e| CatalogError::Backend(format!("invalid JSON response: {e}")))?;
    let array = if value.is_array() {
        value
    } else {
        value
            .get("results")
            .or_else(|| value.get("hits"))
            .cloned()
            .unwrap_or(serde_json::Value::Array(Vec::new()))
    };
    serde_json::from_value(array)
        .map_err(|e| CatalogError::Backend(format!("unexpected result shape: {e}")))
}

/// Resolve the API key: env first, then `[skills].api_key` in
/// `<ainb_home>/config/config.toml`. Returns `None` when neither is set.
fn resolve_api_key(ainb_home: &Path) -> Option<String> {
    if let Ok(key) = std::env::var(ENV_API_KEY) {
        let key = key.trim().to_string();
        if !key.is_empty() {
            return Some(key);
        }
    }
    read_config_api_key(&config_path_in(ainb_home))
}

/// `<ainb_home>/config/config.toml` — the user-level config file.
fn config_path_in(ainb_home: &Path) -> PathBuf {
    ainb_home.join("config").join("config.toml")
}

/// Read `[skills].api_key` from a `config.toml`, tolerating a missing /
/// malformed file (returns `None`). Only the one key is read; the rest
/// of the config is owned by `ainb-core`.
fn read_config_api_key(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let value: toml::Value = toml::from_str(&text).ok()?;
    let key = value
        .get("skills")?
        .get("api_key")?
        .as_str()?
        .trim()
        .to_string();
    (!key.is_empty()).then_some(key)
}

/// Canned hits for `AINB_CATALOG_MOCK=1`. Deterministic + ranked so the
/// live tmux tripwire's pane assertions are stable. Distinct, obvious
/// names so substring checks on the captured pane are unambiguous.
///
/// When `AINB_CATALOG_MOCK_INSTALL_URI` is set, the highest-ranked hit's
/// `install_uri` is replaced with that value — a tripwire points it at
/// the sandbox bare remote so `[b]` → Enter installs from a LOCAL repo
/// (zero network).
fn canned_hits(_query: &str) -> Vec<CatalogHit> {
    let top_install_uri = std::env::var(ENV_MOCK_INSTALL_URI)
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| {
            "gh:stevengonsalvez/agents-in-a-box@main/skills/catalog-demo-skill".to_string()
        });
    let mut hits = vec![
        CatalogHit {
            name: "catalog-demo-skill".to_string(),
            repo: "stevengonsalvez/agents-in-a-box".to_string(),
            stars: 4242,
            install_uri: top_install_uri,
            description: "Canned catalog hit for offline tripwire mode.".to_string(),
        },
        CatalogHit {
            name: "catalog-demo-other".to_string(),
            repo: "octocat/hello-skills".to_string(),
            stars: 99,
            install_uri: "gh:octocat/hello-skills@main/skills/catalog-demo-other".to_string(),
            description: "Second canned hit.".to_string(),
        },
    ];
    rank_by_stars(&mut hits);
    hits
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_accepts_bare_array() {
        let body = r#"[{"name":"a","repo":"o/a","stars":3,
            "install_uri":"gh:o/a@main/skills/a","description":"d"}]"#;
        let hits = parse_search_response(body).expect("parsed");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].name, "a");
    }

    #[test]
    fn parse_accepts_results_envelope() {
        let body = r#"{"results":[{"name":"b","repo":"o/b","stars":7,
            "install_uri":"gh:o/b@main/skills/b","description":"d"}]}"#;
        let hits = parse_search_response(body).expect("parsed");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].name, "b");
    }

    #[test]
    fn parse_invalid_json_is_backend_error() {
        let err = parse_search_response("not json").expect_err("err");
        assert!(matches!(err, CatalogError::Backend(_)));
    }

    #[test]
    fn config_api_key_read_from_skills_section() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("config");
        std::fs::create_dir_all(&cfg).unwrap();
        std::fs::write(
            cfg.join("config.toml"),
            "[skills]\napi_key = \"sk-from-config\"\n",
        )
        .unwrap();
        assert_eq!(
            read_config_api_key(&config_path_in(dir.path())),
            Some("sk-from-config".to_string())
        );
    }

    #[test]
    fn config_api_key_absent_is_none() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read_config_api_key(&config_path_in(dir.path())), None);
    }
}
