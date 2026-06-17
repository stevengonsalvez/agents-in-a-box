//! P5 CARRY-IN (from the P4 review): the directory scan must distinguish a
//! `.md` that is **not a record** (no frontmatter, e.g. a README → skipped
//! quietly) from a **corrupt record** (a `.md` whose frontmatter YAML fails
//! to parse → counted as a parse failure so the Browse status line can show
//! `N notes failed to parse`).
//!
//! `scan_learnings_dir` keeps its `Vec<LearningRecord>` shape (P4 callers
//! unchanged); `scan_learnings_dir_report` is the richer entry point that
//! also returns the corrupt-note count.

use std::fs;

use ainb_plugin_learnings::data::{self, ScanReport};

/// A directory containing one good record, one non-record README (no
/// frontmatter), and one corrupt record (opening `---` but malformed YAML
/// inside the frontmatter block) must:
/// - return the one good record,
/// - count exactly ONE corrupt note (the malformed-frontmatter `.md`),
/// - NOT count the README (it's a non-record, skipped quietly).
#[test]
fn scan_report_counts_corrupt_but_not_non_record_md() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path();

    // Good record.
    fs::write(
        dir.join("lrn-good.md"),
        "---\ntitle: \"Good\"\nscope: \"universal\"\nconfidence: 0.9\ncategory: \"process\"\n---\n\n## Body\nok\n",
    )
    .expect("write good");

    // Non-record: a README with no frontmatter delimiters at all. Must be
    // skipped quietly and NOT counted as a parse failure.
    fs::write(dir.join("README.md"), "# Readme\n\nNot a learning.\n").expect("write readme");

    // Corrupt record: opens a frontmatter block but the YAML inside is
    // malformed (a value that can't parse as the typed `confidence: f64`).
    // This is a real record that failed to parse → it MUST be counted.
    fs::write(
        dir.join("lrn-corrupt.md"),
        "---\ntitle: \"Corrupt\"\nconfidence: \"not-a-number\"\n  bad: : indent\n---\n\nbody\n",
    )
    .expect("write corrupt");

    let report: ScanReport = data::scan_learnings_dir_report(dir).expect("scan report");

    let ids: Vec<&str> = report.records.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(ids, vec!["lrn-good"], "only the good record is returned");

    assert_eq!(
        report.failed_count, 1,
        "exactly the corrupt-frontmatter note is counted as a parse failure (README is a non-record, not counted)"
    );

    // `scan_learnings_dir` returns the same records, ignoring the count.
    let recs = data::scan_learnings_dir(dir).expect("scan");
    let rec_ids: Vec<&str> = recs.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(
        rec_ids, ids,
        "scan_learnings_dir agrees with the report records"
    );
}
