//! P4 data-layer tests — fs reader, GraphML parser, community json, qmd
//! shell behind a trait, and filter predicates.
//!
//! These drive the public `ainb_plugin_learnings::data` API against the
//! committed mini-KB under `tests/fixtures/kb/` so no test touches the
//! real `~/.learnings`. The one live-`qmd` smoke is `#[ignore]`d.

use std::path::{Path, PathBuf};

use ainb_plugin_learnings::data::{
    self, DataError, Entity, Filter, FilterField, LearningRecord, QmdSearch, Relationship,
    SearchHit,
};

/// Absolute path to the committed fixture KB directory.
fn kb_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("fixtures").join("kb")
}

fn kb_file(name: &str) -> PathBuf {
    kb_dir().join(name)
}

// ---------------------------------------------------------------------------
// parse_learning_record
// ---------------------------------------------------------------------------

#[test]
fn test_parse_learning_record_with_sidecar() {
    let rec = data::parse_learning_record(&kb_file("lrn-audit-after-rebase.md"))
        .expect("record with sidecar parses");

    assert_eq!(rec.id, "lrn-audit-after-rebase");
    assert_eq!(
        rec.title,
        "Audit after large rebase before continuing prior plan"
    );
    assert_eq!(rec.scope, "universal");
    assert!((rec.confidence - 0.85).abs() < f64::EPSILON);
    assert_eq!(rec.category, "process");
    assert!(rec.tags.contains(&"rebase".to_string()));
    assert_eq!(rec.source_tool.as_deref(), Some("claude"));
    // This record has no `project` in provenance.
    assert_eq!(rec.project, None);
    assert!(rec.key_insight.contains("git pull --rebase"));
    // Body is the markdown after the frontmatter.
    assert!(rec.body_md.contains("## Problem"));
    assert!(rec.body_md.contains("## Solution"));
    assert!(
        !rec.body_md.contains("title:"),
        "frontmatter must be stripped from body_md"
    );

    // Sidecar entities + relationships.
    assert_eq!(rec.entities.len(), 3);
    let names: Vec<&str> = rec.entities.iter().map(|e: &Entity| e.name.as_str()).collect();
    assert!(names.contains(&"audit-after-rebase"));
    let solves: Vec<&Relationship> =
        rec.relationships.iter().filter(|r| r.rel_type == "solves").collect();
    assert_eq!(solves.len(), 1);
    assert_eq!(solves[0].source, "audit-after-rebase");
    assert_eq!(solves[0].target, "stale plan execution");

    // Provenance round-trips.
    assert_eq!(
        rec.provenance.content_hash.as_deref(),
        Some("147209f82dae87c6")
    );
}

#[test]
fn test_parse_learning_record_project_from_provenance() {
    let rec = data::parse_learning_record(&kb_file("lrn-claude-plugin-autowire.md"))
        .expect("record parses");
    assert_eq!(rec.source_tool.as_deref(), Some("codex"));
    assert_eq!(rec.project.as_deref(), Some("agents-in-a-box"));
}

#[test]
fn test_parse_learning_record_missing_sidecar_yields_empty_entities() {
    // The empty-entities path: no `.entities.yaml` next to the `.md`.
    let rec = data::parse_learning_record(&kb_file("lrn-no-sidecar.md"))
        .expect("record without sidecar still parses (no panic)");
    assert_eq!(rec.id, "lrn-no-sidecar");
    assert!(rec.entities.is_empty());
    assert!(rec.relationships.is_empty());
    // Frontmatter fields still populated.
    assert_eq!(rec.scope, "universal");
    assert_eq!(rec.category, "noteworthy");
}

#[test]
fn test_parse_learning_record_missing_file_is_typed_error() {
    let err = data::parse_learning_record(&kb_file("does-not-exist.md"))
        .expect_err("missing file is a typed error, not a panic");
    assert!(matches!(err, DataError::Io { .. }));
}

// ---------------------------------------------------------------------------
// scan_learnings_dir
// ---------------------------------------------------------------------------

#[test]
fn test_scan_learnings_dir_returns_sorted_records_ignoring_non_md() {
    let recs = data::scan_learnings_dir(&kb_dir()).expect("scan succeeds");
    // 4 `lrn-*.md` records. Ignored: NOTES.txt (non-md), the `.entities.yaml`
    // sidecars (non-md), and README.md (a `.md` with no frontmatter → skipped
    // as a non-record, mirroring a real KB dir that carries a README).
    let ids: Vec<&str> = recs.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(
        ids,
        vec![
            "lrn-audit-after-rebase",
            "lrn-claude-plugin-autowire",
            "lrn-no-sidecar",
            "lrn-tokio-runtime-panic",
        ],
        "records must be sorted stably by id and exclude non-.md files"
    );
}

#[test]
fn test_scan_learnings_dir_missing_dir_is_typed_error() {
    let err =
        data::scan_learnings_dir(&kb_dir().join("nope")).expect_err("missing dir is a typed error");
    assert!(matches!(err, DataError::Io { .. }));
}

// ---------------------------------------------------------------------------
// parse_graphml
// ---------------------------------------------------------------------------

#[test]
fn test_parse_graphml_entities_and_typed_edges() {
    let graph = data::parse_graphml(&kb_file("graph_chunk_entity_relation.graphml"))
        .expect("graphml parses");

    // 3 nodes; quote-wrapping stripped from ids.
    let entity_ids: Vec<&str> = graph.entities.iter().map(|e| e.id.as_str()).collect();
    assert!(entity_ids.contains(&"AUDIT-AFTER-REBASE"));
    assert!(entity_ids.contains(&"STALE-PLAN-EXECUTION"));
    assert!(entity_ids.contains(&"GIT-PULL-REBASE"));
    assert_eq!(graph.entities.len(), 3);

    // 3 edges. The first carries a typed rel_type `solves`.
    assert_eq!(graph.edges.len(), 3);
    let solves = graph
        .edges
        .iter()
        .find(|e| e.rel_type == "solves")
        .expect("a `solves` typed edge exists");
    assert_eq!(solves.source, "AUDIT-AFTER-REBASE");
    assert_eq!(solves.target, "STALE-PLAN-EXECUTION");

    // The untyped edge falls back to the default rel_type.
    let untyped = graph
        .edges
        .iter()
        .find(|e| e.source == "AUDIT-AFTER-REBASE" && e.target == "GIT-PULL-REBASE")
        .expect("untyped edge present");
    assert_eq!(untyped.rel_type, "relates_to");
}

#[test]
fn test_parse_graphml_malformed_is_typed_error_not_panic() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let bad = tmp.path().join("broken.graphml");
    std::fs::write(&bad, b"<graphml><node id=\"x\"></graphml-unclosed").expect("write");
    let err = data::parse_graphml(&bad).expect_err("malformed xml is a typed error");
    assert!(matches!(err, DataError::GraphMl { .. }));
}

// ---------------------------------------------------------------------------
// parse_community_reports
// ---------------------------------------------------------------------------

#[test]
fn test_parse_community_reports() {
    let comms = data::parse_community_reports(&kb_file("kv_store_community_reports.json"))
        .expect("community reports parse");
    assert_eq!(comms.len(), 2);
    // Sorted by id so the order is deterministic.
    assert_eq!(comms[0].id, "1");
    assert_eq!(comms[0].title, "Rebase discipline");
    assert_eq!(comms[0].level, 0);
    assert_eq!(comms[0].nodes.len(), 3);
    assert!((comms[1].rating - 8.0).abs() < f64::EPSILON);
}

#[test]
fn test_parse_community_reports_malformed_is_typed_error() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let bad = tmp.path().join("bad.json");
    std::fs::write(&bad, b"{ not valid json").expect("write");
    let err = data::parse_community_reports(&bad).expect_err("malformed json is a typed error");
    assert!(matches!(err, DataError::Json { .. }));
}

// ---------------------------------------------------------------------------
// qmd search (parse + trait)
// ---------------------------------------------------------------------------

#[test]
fn test_qmd_query_parse_from_captured_sample() {
    let sample = std::fs::read_to_string(kb_file("qmd_query_sample.json")).expect("read sample");
    let hits: Vec<SearchHit> = data::parse_qmd_json(&sample).expect("captured sample parses");
    assert_eq!(hits.len(), 3);
    // Ranked: first hit is the highest score.
    assert_eq!(hits[0].id, "#114073");
    assert!((hits[0].score - 0.93).abs() < 1e-9);
    assert_eq!(hits[0].title, "feedback_audit_after_rebase");
    assert_eq!(
        hits[0].file.as_deref(),
        Some("qmd://learnings/feedback-audit-after-rebase.md")
    );
    // Lowest score last.
    assert!(hits[2].score < hits[0].score);
}

/// The qmd shell is wrapped behind a trait so tests inject a fake runner
/// returning the captured sample — no subprocess.
#[test]
fn test_qmd_search_trait_uses_runner_output() {
    struct FakeRunner(String);
    impl QmdSearch for FakeRunner {
        fn run_query(
            &self,
            _query: &str,
            _collection: &str,
            _index: &str,
        ) -> Result<String, DataError> {
            Ok(self.0.clone())
        }
    }
    let sample = std::fs::read_to_string(kb_file("qmd_query_sample.json")).expect("read sample");
    let runner = FakeRunner(sample);
    let hits = data::search(
        &runner,
        "audit after rebase",
        "learnings",
        "~/.cache/qmd/index.sqlite",
        data::SearchMode::Semantic,
    )
    .expect("search via trait");
    assert_eq!(hits.len(), 3);
    assert_eq!(hits[0].id, "#114073");
}

#[test]
#[ignore = "live qmd smoke — requires qmd on PATH + a populated index"]
fn test_qmd_live_smoke() {
    // Exercises the real qmd shell runner end-to-end. Ignored by default so
    // the suite is hermetic; run with `--ignored` on a machine with qmd.
    let runner = data::QmdCli::default();
    let hits = data::search(
        &runner,
        "audit after rebase",
        "learnings",
        "~/.cache/qmd/index.sqlite",
        data::SearchMode::Semantic,
    )
    .expect("live qmd query");
    assert!(!hits.is_empty(), "live qmd returned no hits");
}

// ---------------------------------------------------------------------------
// filter predicates
// ---------------------------------------------------------------------------

fn all_records() -> Vec<LearningRecord> {
    data::scan_learnings_dir(&kb_dir()).expect("scan")
}

#[test]
fn test_filter_by_scope() {
    let recs = all_records();
    let f = Filter::new().with(FilterField::Scope, "universal");
    let kept: Vec<&str> = f.apply(&recs).iter().map(|r| r.id.as_str()).collect();
    assert_eq!(
        kept,
        vec![
            "lrn-audit-after-rebase",
            "lrn-claude-plugin-autowire",
            "lrn-no-sidecar",
        ]
    );
    assert!(!kept.contains(&"lrn-tokio-runtime-panic"));
}

#[test]
fn test_filter_by_min_confidence() {
    let recs = all_records();
    let f = Filter::new().with_min_confidence(0.8);
    let kept: Vec<&str> = f.apply(&recs).iter().map(|r| r.id.as_str()).collect();
    // 0.85, 1.0, 0.8 pass; 0.7 (no-sidecar) fails.
    assert!(kept.contains(&"lrn-audit-after-rebase"));
    assert!(kept.contains(&"lrn-tokio-runtime-panic"));
    assert!(kept.contains(&"lrn-claude-plugin-autowire"));
    assert!(!kept.contains(&"lrn-no-sidecar"));
}

#[test]
fn test_filter_by_category_and_source() {
    let recs = all_records();
    let by_cat = Filter::new().with(FilterField::Category, "reference");
    let cat_ids: Vec<&str> = by_cat.apply(&recs).iter().map(|r| r.id.as_str()).collect();
    assert_eq!(
        cat_ids,
        vec!["lrn-claude-plugin-autowire", "lrn-tokio-runtime-panic"]
    );

    let by_src = Filter::new().with(FilterField::Source, "codex");
    let src_ids: Vec<&str> = by_src.apply(&recs).iter().map(|r| r.id.as_str()).collect();
    assert_eq!(src_ids, vec!["lrn-claude-plugin-autowire"]);
}

#[test]
fn test_filter_by_project() {
    let recs = all_records();
    let f = Filter::new().with(FilterField::Project, "agents-in-a-box");
    let ids: Vec<&str> = f.apply(&recs).iter().map(|r| r.id.as_str()).collect();
    assert_eq!(ids, vec!["lrn-claude-plugin-autowire"]);
}

#[test]
fn test_filter_combined_predicates_intersect() {
    let recs = all_records();
    let f = Filter::new()
        .with(FilterField::Scope, "universal")
        .with(FilterField::Category, "reference");
    let ids: Vec<&str> = f.apply(&recs).iter().map(|r| r.id.as_str()).collect();
    // universal AND reference → only the autowire record.
    assert_eq!(ids, vec!["lrn-claude-plugin-autowire"]);
}

#[test]
fn test_empty_filter_keeps_all() {
    let recs = all_records();
    let f = Filter::new();
    assert_eq!(f.apply(&recs).len(), recs.len());
}

// ---------------------------------------------------------------------------
// tilde expansion (paths confined to LearningsConfig dirs)
// ---------------------------------------------------------------------------

#[test]
fn test_expand_tilde() {
    let home = dirs::home_dir().expect("home dir");
    assert_eq!(
        data::expand_tilde("~/.learnings/documents"),
        home.join(".learnings/documents")
    );
    // A leading `~` only expands as a path segment, not mid-string.
    assert_eq!(data::expand_tilde("/abs/path"), PathBuf::from("/abs/path"));
    assert_eq!(data::expand_tilde("~"), home);
}
