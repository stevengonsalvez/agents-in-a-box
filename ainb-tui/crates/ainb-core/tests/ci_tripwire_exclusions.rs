#![allow(missing_docs)]

// ABOUTME: The `core-tripwires` CI job runs every ainb-core tripwire binary not
// named in `tests/tripwire_ci_exclusions.txt`. This keeps that list honest:
// each exclusion names a real tripwire, once, with an issue and a reason.

use std::collections::BTreeSet;
use std::path::Path;

fn tests_dir() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests")
}

#[test]
fn every_exclusion_names_a_tripwire_once_with_an_issue_and_a_reason() {
    let list = std::fs::read_to_string(tests_dir().join("tripwire_ci_exclusions.txt"))
        .expect("the exclusion list is committed beside the tests");
    let mut seen = BTreeSet::new();
    for (number, line) in list.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut fields = line.split_whitespace();
        let name = fields.next().expect("non-empty line has a name");
        let issue = fields.next().unwrap_or_default();
        let reason: Vec<&str> = fields.collect();
        let at = format!("line {}: `{line}`", number + 1);
        assert!(name.starts_with("tripwire_"), "{at}: not a tripwire binary");
        assert!(
            tests_dir().join(format!("{name}.rs")).is_file(),
            "{at}: no tests/{name}.rs, so the exclusion is stale"
        );
        assert!(
            issue.len() > 1
                && issue.starts_with('#')
                && issue[1..].chars().all(|c| c.is_ascii_digit()),
            "{at}: the second field must be an issue like #1023"
        );
        assert!(!reason.is_empty(), "{at}: an exclusion needs a reason");
        assert!(seen.insert(name.to_string()), "{at}: excluded twice");
    }
}
