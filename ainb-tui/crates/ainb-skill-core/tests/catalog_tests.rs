//! Catalog browse/search (`ainb skill browse`) — bead ai-a20.
//!
//! Pure-function + mock-backend tests. ZERO network: every test drives
//! the [`MockCatalogBackend`] (feature `test-fixtures`) or asserts the
//! pure URL builder. The production [`SkillsShHttpBackend`] (reqwest →
//! skills.sh) lives in `ainb-cli` and is never exercised here.

use ainb_skill_core::catalog::{
    CatalogBackend, CatalogError, CatalogHit, MockCatalogBackend, SkillsShUrlBuilder, rank_by_stars,
};

fn hit(name: &str, repo: &str, stars: u64) -> CatalogHit {
    CatalogHit {
        name: name.to_string(),
        repo: repo.to_string(),
        stars,
        install_uri: format!("gh:{repo}@main/skills/{name}"),
        description: format!("{name} skill"),
        kind: ainb_skill_core::catalog::CatalogEntryKind::Skill,
    }
}

// ── Test 1: CatalogHit serde round-trips ──────────────────────────────

#[test]
fn catalog_hit_serde_roundtrips() {
    let h = hit("commit", "stevengonsalvez/agents-in-a-box", 1234);
    let json = serde_json::to_string(&h).expect("serialize");
    // Field names are the wire contract for `--json` output.
    assert!(json.contains("\"name\":\"commit\""), "name field: {json}");
    assert!(
        json.contains("\"repo\":\"stevengonsalvez/agents-in-a-box\""),
        "repo: {json}"
    );
    assert!(json.contains("\"stars\":1234"), "stars: {json}");
    assert!(json.contains("\"install_uri\""), "install_uri: {json}");
    assert!(json.contains("\"description\""), "description: {json}");

    let back: CatalogHit = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(back, h, "round-trip equality");
}

// ── Test 2: MockCatalogBackend returns canned hits ────────────────────

#[test]
fn mock_backend_returns_canned_hits() {
    let mock = MockCatalogBackend::with_hits(vec![
        hit("alpha", "org/alpha", 10),
        hit("beta", "org/beta", 20),
    ]);
    let out = mock.search("anything").expect("search ok");
    assert_eq!(out.len(), 2, "both canned hits returned");
    // The mock returns its canned hits ranked (stars desc) — same
    // contract the production backend honours.
    assert_eq!(out[0].name, "beta", "higher-starred first");
    assert_eq!(out[1].name, "alpha");
}

// ── Test 3: ranking is pure + testable (stars desc) ───────────────────

#[test]
fn search_ranks_by_stars_desc() {
    let mut hits = vec![
        hit("low", "org/low", 3),
        hit("high", "org/high", 99),
        hit("mid", "org/mid", 42),
    ];
    rank_by_stars(&mut hits);
    let names: Vec<&str> = hits.iter().map(|h| h.name.as_str()).collect();
    assert_eq!(names, vec!["high", "mid", "low"], "descending by stars");
}

#[test]
fn rank_by_stars_is_stable_on_ties() {
    // Equal stars keep insertion order so the ranking is deterministic.
    let mut hits = vec![
        hit("first", "org/first", 5),
        hit("second", "org/second", 5),
        hit("third", "org/third", 5),
    ];
    rank_by_stars(&mut hits);
    let names: Vec<&str> = hits.iter().map(|h| h.name.as_str()).collect();
    assert_eq!(names, vec!["first", "second", "third"], "stable on ties");
}

// ── Test 4: empty query returns empty, not error ──────────────────────

#[test]
fn empty_query_returns_empty_not_error() {
    let mock = MockCatalogBackend::with_hits(vec![hit("x", "org/x", 1)]);
    let out = mock.search("").expect("empty query is Ok, not Err");
    assert!(out.is_empty(), "empty query yields zero hits");

    // Whitespace-only is treated the same as empty.
    let out2 = mock.search("   ").expect("whitespace query is Ok");
    assert!(out2.is_empty(), "whitespace-only query yields zero hits");
}

// ── Test 5: api error surfaces typed error (no panic) ─────────────────

#[test]
fn api_error_surfaces_typed_error() {
    let mock =
        MockCatalogBackend::failing(CatalogError::Backend("skills.sh returned 500".to_string()));
    let err = mock.search("anything").expect_err("backend error surfaces");
    assert!(
        matches!(err, CatalogError::Backend(_)),
        "typed Backend error: {err:?}"
    );

    // An auth-gated mock surfaces the dedicated AuthRequired variant.
    let auth_mock = MockCatalogBackend::failing(CatalogError::AuthRequired);
    let auth_err = auth_mock.search("q").expect_err("auth error surfaces");
    assert!(
        matches!(auth_err, CatalogError::AuthRequired),
        "AuthRequired: {auth_err:?}"
    );
    // The error message must point the user at the env / config knob.
    let msg = auth_err.to_string();
    assert!(
        msg.contains("AINB_SKILLS_API_KEY") || msg.contains("api_key"),
        "auth error message names the key knob: {msg}"
    );
}

// ── Test 6: skills.sh URL builds with api key (pure, no network) ───────

#[test]
fn skills_sh_url_builds_with_api_key() {
    // Default base is the public skills.sh API.
    let builder = SkillsShUrlBuilder::new(None);
    let url = builder.search_url("graph layout");
    assert!(
        url.starts_with("https://skills.sh/api/search"),
        "base url: {url}"
    );
    assert!(url.contains("q=graph"), "query encoded: {url}");
    // Spaces are percent-encoded, not left raw.
    assert!(!url.contains("graph layout"), "spaces encoded: {url}");

    // With a key configured, the Authorization header carries the bearer
    // token; the URL itself never leaks the key.
    let builder = SkillsShUrlBuilder::new(Some("sk-test-123".to_string()));
    let header = builder.auth_header();
    assert_eq!(
        header.as_deref(),
        Some("Bearer sk-test-123"),
        "bearer header built from the key"
    );
    let url = builder.search_url("x");
    assert!(
        !url.contains("sk-test-123"),
        "key never appears in the url: {url}"
    );

    // No key → no auth header.
    let builder = SkillsShUrlBuilder::new(None);
    assert_eq!(builder.auth_header(), None, "no key → no header");
}

#[test]
fn url_builder_honours_custom_base() {
    // The AINB_SKILLS_API_BASE override (used by the tmux tripwire's
    // local stub) reroutes the base host.
    let builder = SkillsShUrlBuilder::with_base("http://127.0.0.1:9999", None);
    let url = builder.search_url("q");
    assert!(
        url.starts_with("http://127.0.0.1:9999/api/search"),
        "custom base: {url}"
    );
}
