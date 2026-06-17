//! `ainb skill browse <query>` integration tests — bead ai-a20.
//!
//! Drives `ainb_cli::skill::browse` with an injected `MockCatalogBackend`
//! so the network is NEVER touched. Covers the ranked table, the
//! `--json` shape, and the empty-query message. The production
//! `SkillsShHttpBackend` (reqwest → skills.sh) is exercised only by its
//! own pure URL-builder assertions in `ainb-skill-core`.

use std::path::Path;

use ainb_cli::BrowseArgs;
use ainb_skill_core::catalog::{CatalogHit, MockCatalogBackend};

fn tmp_home() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("ainb-skill-browse-")
        .tempdir()
        .expect("tempdir")
}

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

fn run_browse(home: &Path, args: BrowseArgs, backend: &MockCatalogBackend) -> String {
    let mut out = Vec::new();
    ainb_cli::skill::browse(home, args, backend, &mut out).expect("browse ok");
    String::from_utf8(out).expect("utf8")
}

// ── Test 7: ranked table ──────────────────────────────────────────────

#[test]
fn cli_browse_prints_ranked_table() {
    let home = tmp_home();
    let backend = MockCatalogBackend::with_hits(vec![
        hit("low-star", "org/low", 7),
        hit("top-star", "org/top", 980),
        hit("mid-star", "org/mid", 250),
    ]);
    let text = run_browse(
        home.path(),
        BrowseArgs {
            query: "diagram".to_string(),
            catalog: ainb_cli::CatalogChoice::Skills,
            json: false,
        },
        &backend,
    );

    // All three names + their repos + install URIs appear.
    assert!(text.contains("top-star"), "missing top-star:\n{text}");
    assert!(text.contains("mid-star"), "missing mid-star:\n{text}");
    assert!(text.contains("low-star"), "missing low-star:\n{text}");
    assert!(text.contains("org/top"), "missing repo:\n{text}");
    assert!(text.contains("980"), "missing star count:\n{text}");
    assert!(
        text.contains("gh:org/top@main/skills/top-star"),
        "missing install_uri:\n{text}"
    );

    // Ranked: top-star (980) must appear before mid-star (250) before
    // low-star (7).
    let top = text.find("top-star").expect("top present");
    let mid = text.find("mid-star").expect("mid present");
    let low = text.find("low-star").expect("low present");
    assert!(
        top < mid && mid < low,
        "rows not ranked by stars desc:\n{text}"
    );
}

// ── Test 8: --json shape ──────────────────────────────────────────────

#[test]
fn cli_browse_json_shape() {
    let home = tmp_home();
    let backend = MockCatalogBackend::with_hits(vec![
        hit("alpha", "org/alpha", 5),
        hit("beta", "org/beta", 88),
    ]);
    let text = run_browse(
        home.path(),
        BrowseArgs {
            query: "anything".to_string(),
            catalog: ainb_cli::CatalogChoice::Skills,
            json: true,
        },
        &backend,
    );

    // Parse it back — it must be a JSON array of CatalogHit objects,
    // ranked stars-desc (beta first).
    let parsed: Vec<CatalogHit> = serde_json::from_str(&text).expect("valid json array");
    assert_eq!(parsed.len(), 2, "two hits:\n{text}");
    assert_eq!(
        parsed[0].name, "beta",
        "highest stars first in json:\n{text}"
    );
    assert_eq!(parsed[1].name, "alpha");
    assert_eq!(parsed[0].stars, 88);
    assert_eq!(parsed[0].install_uri, "gh:org/beta@main/skills/beta");
}

// ── Test 9: empty-query message ───────────────────────────────────────

#[test]
fn cli_browse_empty_query_message() {
    let home = tmp_home();
    let backend = MockCatalogBackend::with_hits(vec![hit("x", "org/x", 1)]);
    let text = run_browse(
        home.path(),
        BrowseArgs {
            query: "   ".to_string(),
            catalog: ainb_cli::CatalogChoice::Skills,
            json: false,
        },
        &backend,
    );
    // The empty/whitespace query is a no-op that prints a hint, not an
    // error, and not the seeded hit.
    assert!(
        text.contains("empty query") || text.contains("no query") || text.contains("type a query"),
        "expected an empty-query hint:\n{text}"
    );
    assert!(
        !text.contains("org/x"),
        "empty query must not return hits:\n{text}"
    );
}

// ── Curated catalog: blank query lists the WHOLE shelf, offline ───────

#[test]
fn cli_browse_curated_blank_lists_full_shelf() {
    use ainb_cli::catalog_curated::AinbCuratedCatalogBackend;
    use ainb_skill_core::catalog_index::{CatalogIndex, CatalogIndexEntry, CatalogOrigin};

    let home = tmp_home();
    let index = CatalogIndex::new(
        "v1.5.0",
        vec![
            CatalogIndexEntry {
                name: "commit".to_string(),
                description: "git commits".to_string(),
                repo: "stevengonsalvez/agents-in-a-box".to_string(),
                install_uri:
                    "gh:stevengonsalvez/agents-in-a-box@v1.5.0/toolkit/packages/skills/commit"
                        .to_string(),
                origin: CatalogOrigin::Owned,
                stars: 0,
                kind: ainb_skill_core::catalog::CatalogEntryKind::Skill,
            },
            CatalogIndexEntry {
                name: "ui-ux-pro-max".to_string(),
                description: "UI/UX design intelligence".to_string(),
                repo: "nextlevelbuilder/ui-ux-pro-max-skill".to_string(),
                install_uri: "gh:nextlevelbuilder/ui-ux-pro-max-skill@2.2.1/.claude/skills"
                    .to_string(),
                origin: CatalogOrigin::External,
                stars: 0,
                kind: ainb_skill_core::catalog::CatalogEntryKind::Skill,
            },
        ],
    );
    let index_path = home.path().join("catalog-index.json");
    std::fs::write(&index_path, index.to_json()).unwrap();

    // Curated backend reads the local file — zero network.
    let backend = AinbCuratedCatalogBackend::from_index_file(&index_path);
    let mut out = Vec::new();
    ainb_cli::skill::browse(
        home.path(),
        BrowseArgs {
            query: "   ".to_string(), // blank → full shelf for the curated catalog
            catalog: ainb_cli::CatalogChoice::Ainb,
            json: false,
        },
        &backend,
        &mut out,
    )
    .expect("curated browse ok");
    let text = String::from_utf8(out).expect("utf8");

    // Both an owned and a vetted-external entry appear, with their real
    // upstream install URIs — the success-criteria "BOTH" check.
    assert!(text.contains("commit"), "missing owned entry:\n{text}");
    assert!(
        text.contains("gh:stevengonsalvez/agents-in-a-box@v1.5.0/toolkit/packages/skills/commit"),
        "missing owned install_uri:\n{text}"
    );
    assert!(
        text.contains("ui-ux-pro-max"),
        "missing external entry:\n{text}"
    );
    assert!(
        text.contains("gh:nextlevelbuilder/ui-ux-pro-max-skill@2.2.1/.claude/skills"),
        "missing external install_uri:\n{text}"
    );
    assert!(
        text.contains("curated skill(s)"),
        "missing curated footer:\n{text}"
    );
}

// ── Bonus: a backend error surfaces as a CLI error (not a panic) ──────

#[test]
fn cli_browse_surfaces_backend_error() {
    use ainb_skill_core::catalog::CatalogError;
    let home = tmp_home();
    let backend = MockCatalogBackend::failing(CatalogError::Backend("boom".into()));
    let mut out = Vec::new();
    let result = ainb_cli::skill::browse(
        home.path(),
        BrowseArgs {
            query: "q".to_string(),
            catalog: ainb_cli::CatalogChoice::Skills,
            json: false,
        },
        &backend,
        &mut out,
    );
    assert!(result.is_err(), "backend error propagates as a CLI Err");
}
