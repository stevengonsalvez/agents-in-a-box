//! Catalog browse/search — bead ai-a20.
//!
//! Browse a remote skill catalog (skills.sh) before installing. Backed
//! by a [`CatalogBackend`] trait so the production path (`reqwest` →
//! skills.sh, in `ainb-cli`) and the test path ([`MockCatalogBackend`])
//! share one boundary. **Zero network in tests** — every test injects
//! the mock; the live binary uses canned data when `AINB_CATALOG_MOCK=1`
//! (or `AINB_SKILLS_API_BASE` points at a local stub).
//!
//! Results are **ephemeral** — there is NO SQLite, no cache file. A
//! browse returns a `Vec<CatalogHit>` that the caller renders + (on the
//! TUI) installs by routing the hit's `install_uri` through the existing
//! `ainb skill install` flow.
//!
//! ```text
//! ┌─────────────────┐     ┌───────────────────────────┐
//! │ CatalogBackend  │◀────│ SkillsShHttpBackend (prod) │ reqwest → skills.sh
//! │ (trait, Send+   │     ├───────────────────────────┤
//! │  Sync)          │◀────│ MockCatalogBackend (tests) │ canned Vec<CatalogHit>
//! │  fn search(q)   │     └───────────────────────────┘
//! └─────────────────┘
//!         │
//!         ▼
//!   Vec<CatalogHit { name, repo, stars, install_uri, description }>
//! ```

use serde::{Deserialize, Serialize};

/// Base URL of the public skills.sh catalog API. Overridable per
/// [`SkillsShUrlBuilder::with_base`] (the tmux tripwire points this at a
/// local stub so the live binary stays offline).
pub const SKILLS_SH_DEFAULT_BASE: &str = "https://skills.sh";

/// How a catalog entry installs. A `Skill` deploys a git-backed unit via the
/// `ainb source add` + `ainb skill install` flow; the other kinds install via
/// their own CLI by RUNNING a shell command, because npx skills, Claude Code
/// plugins, and MCP servers are not deployable skill files. For every
/// non-`Skill` kind, [`CatalogHit::install_uri`] carries that shell command
/// (not a unit URI) — the install router discriminates on this enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum CatalogEntryKind {
    /// A git-backed skill unit (`install_uri` is a `gh:`/`git:` unit URI).
    #[default]
    Skill,
    /// An npx-published skill (`install_uri` is its `npx skills add …` command).
    Npx,
    /// A Claude Code plugin (`install_uri` is its `claude plugin install …`).
    Plugin,
    /// An MCP server (`install_uri` is its `claude mcp add …` command).
    Mcp,
}

impl CatalogEntryKind {
    /// Whether installing this kind RUNS a shell command (vs the gh: unit
    /// flow). True for everything except [`CatalogEntryKind::Skill`].
    pub fn is_command(self) -> bool {
        !matches!(self, CatalogEntryKind::Skill)
    }

    /// Short badge label for the browse shelf (`skill` / `npx` / `plugin` /
    /// `mcp`).
    pub fn badge(self) -> &'static str {
        match self {
            CatalogEntryKind::Skill => "skill",
            CatalogEntryKind::Npx => "npx",
            CatalogEntryKind::Plugin => "plugin",
            CatalogEntryKind::Mcp => "mcp",
        }
    }
}

/// One catalog search hit. Field names are the wire contract for the
/// `--json` CLI output, so renaming breaks scripts — keep them stable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogHit {
    /// Unit name (the trailing folder / file stem — e.g. `commit`).
    pub name: String,
    /// Source repo in `owner/repo` form (e.g.
    /// `stevengonsalvez/agents-in-a-box`).
    pub repo: String,
    /// GitHub star count, used to rank results (descending).
    pub stars: u64,
    /// For a `skill` kind: the full unit URI to feed `ainb skill install`
    /// (e.g. `gh:owner/repo@main/skills/<name>`). For npx/plugin/mcp kinds:
    /// the shell command that installs it.
    pub install_uri: String,
    /// One-line human description.
    pub description: String,
    /// How this entry installs. Defaults to [`CatalogEntryKind::Skill`] so an
    /// older index / the skills.sh backend (which omit it) stay valid.
    #[serde(default)]
    pub kind: CatalogEntryKind,
}

/// Errors raised by a [`CatalogBackend`]. Distinguishes a transient
/// backend failure (`Backend`) from a missing-credentials condition
/// (`AuthRequired`) so the CLI / TUI can give the right hint.
#[derive(Debug, thiserror::Error)]
pub enum CatalogError {
    /// The backend could not return results (HTTP error, malformed
    /// response, transport failure). Free-form so the production backend
    /// can wrap reqwest's error string without nesting types.
    #[error("catalog backend error: {0}")]
    Backend(String),

    /// The catalog API demands a credential the user has not configured.
    /// The message names both knobs so the fix is obvious.
    #[error(
        "catalog requires authentication — set AINB_SKILLS_API_KEY or \
         [skills].api_key in config.toml"
    )]
    AuthRequired,
}

/// Catalog-search backend. Implementations decide how to turn a query
/// into a ranked `Vec<CatalogHit>`.
///
/// `Send + Sync` so the backend can be shipped to a background thread if
/// the TUI ever moves the search off the event loop. The contract:
///
/// - An **empty / whitespace-only** query returns `Ok(vec![])` — never
///   an error (it's a no-op, not a failure).
/// - Results SHOULD be ranked by stars descending; callers may re-rank
///   via [`rank_by_stars`] defensively.
pub trait CatalogBackend: Send + Sync {
    /// Search the catalog for units matching `query`. See the trait docs
    /// for the empty-query + ranking contract.
    fn search(&self, query: &str) -> Result<Vec<CatalogHit>, CatalogError>;
}

/// Rank hits by star count, highest first. Stable on ties (equal-star
/// hits keep their input order), so the result is deterministic for a
/// given input — important for the tripwire's pane assertions.
pub fn rank_by_stars(hits: &mut [CatalogHit]) {
    // `sort_by_key` is stable, so equal keys preserve insertion order.
    // `Reverse` flips to descending without an explicit comparator.
    hits.sort_by_key(|h| std::cmp::Reverse(h.stars));
}

/// Whether a query is effectively empty (blank or whitespace-only).
/// Shared by every backend so the empty-query contract is enforced in
/// one place.
pub fn is_blank_query(query: &str) -> bool {
    query.trim().is_empty()
}

/// Pure builder for the skills.sh request URL + auth header. Holds no
/// network state — every method is a string transform, so it's fully
/// unit-testable without reqwest or a socket.
#[derive(Debug, Clone)]
pub struct SkillsShUrlBuilder {
    base: String,
    api_key: Option<String>,
}

impl SkillsShUrlBuilder {
    /// Build against the default public base ([`SKILLS_SH_DEFAULT_BASE`]).
    pub fn new(api_key: Option<String>) -> Self {
        Self::with_base(SKILLS_SH_DEFAULT_BASE, api_key)
    }

    /// Build against an explicit base URL (trailing slash tolerated).
    /// Used by the tmux tripwire's local stub via `AINB_SKILLS_API_BASE`.
    pub fn with_base(base: impl Into<String>, api_key: Option<String>) -> Self {
        let mut base = base.into();
        while base.ends_with('/') {
            base.pop();
        }
        Self { base, api_key }
    }

    /// `GET <base>/api/search?q=<percent-encoded-query>`. The API key is
    /// NEVER placed in the URL — it travels in the `Authorization`
    /// header (see [`Self::auth_header`]).
    pub fn search_url(&self, query: &str) -> String {
        format!("{}/api/search?q={}", self.base, percent_encode_query(query))
    }

    /// `GET <base>/api/trending`. Same base + no query.
    pub fn trending_url(&self) -> String {
        format!("{}/api/trending", self.base)
    }

    /// The `Authorization: Bearer <key>` header value, or `None` when no
    /// key is configured (search may still work unauthenticated).
    pub fn auth_header(&self) -> Option<String> {
        self.api_key.as_ref().map(|k| format!("Bearer {k}"))
    }
}

/// Minimal percent-encoding for query-string values: keeps the
/// RFC 3986 unreserved set (`A-Z a-z 0-9 - _ . ~`) verbatim and
/// percent-encodes every other byte. Avoids a `urlencoding` /
/// `percent-encoding` crate dependency for a single query param.
fn percent_encode_query(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

// ---------------------------------------------------------------------
// Test scaffolding — feature-gated so production binaries never link the
// mock backend's code or canned bytes.
// ---------------------------------------------------------------------

/// Stub catalog backend that returns canned hits (or a canned error).
/// Used by unit + integration tests + the live tmux tripwire so the
/// browse flow never hits skills.sh.
///
/// `pub` (behind `test` / `test-fixtures`) so tests in other crates
/// (`ainb-cli`, `ainb-core` tripwires) share one harness.
#[cfg(any(test, feature = "test-fixtures"))]
#[derive(Debug, Clone, Default)]
pub struct MockCatalogBackend {
    hits: Vec<CatalogHit>,
    /// When set, `search` returns this error for any non-blank query.
    error: Option<MockError>,
}

/// Cloneable mirror of the subset of [`CatalogError`] the mock can
/// replay (the real error isn't `Clone` because it carries free-form
/// strings the production backend fills in).
#[cfg(any(test, feature = "test-fixtures"))]
#[derive(Debug, Clone)]
enum MockError {
    Backend(String),
    AuthRequired,
}

#[cfg(any(test, feature = "test-fixtures"))]
impl MockCatalogBackend {
    /// A backend that returns `hits` (ranked by stars) for any non-blank
    /// query.
    pub fn with_hits(hits: Vec<CatalogHit>) -> Self {
        Self { hits, error: None }
    }

    /// A backend that fails every non-blank query with `error`.
    pub fn failing(error: CatalogError) -> Self {
        let error = match error {
            CatalogError::Backend(m) => MockError::Backend(m),
            CatalogError::AuthRequired => MockError::AuthRequired,
        };
        Self {
            hits: Vec::new(),
            error: Some(error),
        }
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
impl CatalogBackend for MockCatalogBackend {
    fn search(&self, query: &str) -> Result<Vec<CatalogHit>, CatalogError> {
        // Empty-query contract holds even for the failing mock: a blank
        // query is a no-op, never an error.
        if is_blank_query(query) {
            return Ok(Vec::new());
        }
        if let Some(err) = &self.error {
            return Err(match err {
                MockError::Backend(m) => CatalogError::Backend(m.clone()),
                MockError::AuthRequired => CatalogError::AuthRequired,
            });
        }
        let mut hits = self.hits.clone();
        rank_by_stars(&mut hits);
        Ok(hits)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(name: &str, stars: u64) -> CatalogHit {
        CatalogHit {
            name: name.to_string(),
            repo: format!("org/{name}"),
            stars,
            install_uri: format!("gh:org/{name}@main/skills/{name}"),
            description: format!("{name} skill"),
            kind: CatalogEntryKind::Skill,
        }
    }

    #[test]
    fn rank_orders_descending() {
        let mut hits = vec![hit("a", 1), hit("b", 9), hit("c", 5)];
        rank_by_stars(&mut hits);
        assert_eq!(
            hits.iter().map(|h| h.name.as_str()).collect::<Vec<_>>(),
            vec!["b", "c", "a"]
        );
    }

    #[test]
    fn blank_query_is_blank() {
        assert!(is_blank_query(""));
        assert!(is_blank_query("   "));
        assert!(!is_blank_query("x"));
    }

    #[test]
    fn entry_kind_defaults_to_skill_and_serdes_lowercase() {
        assert_eq!(CatalogEntryKind::default(), CatalogEntryKind::Skill);
        assert_eq!(
            serde_json::to_string(&CatalogEntryKind::Npx).unwrap(),
            "\"npx\""
        );
        let k: CatalogEntryKind = serde_json::from_str("\"plugin\"").unwrap();
        assert_eq!(k, CatalogEntryKind::Plugin);
    }

    #[test]
    fn entry_kind_is_command_only_for_non_skill() {
        assert!(!CatalogEntryKind::Skill.is_command());
        assert!(CatalogEntryKind::Npx.is_command());
        assert!(CatalogEntryKind::Plugin.is_command());
        assert!(CatalogEntryKind::Mcp.is_command());
    }

    #[test]
    fn hit_kind_defaults_to_skill_when_absent_in_json() {
        // A pre-expansion index / skills.sh response omits `kind`.
        let body = r#"{"name":"a","repo":"o/a","stars":1,
            "install_uri":"gh:o/a@main/skills/a","description":"d"}"#;
        let hit: CatalogHit = serde_json::from_str(body).unwrap();
        assert_eq!(hit.kind, CatalogEntryKind::Skill);
    }

    #[test]
    fn search_url_encodes_spaces_and_omits_key() {
        let b = SkillsShUrlBuilder::new(Some("secret".into()));
        let url = b.search_url("a b");
        assert!(url.contains("q=a%20b"), "{url}");
        assert!(!url.contains("secret"), "{url}");
    }
}
