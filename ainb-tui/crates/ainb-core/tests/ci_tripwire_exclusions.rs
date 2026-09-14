#![allow(missing_docs)]

// ABOUTME: The `core-tripwires` CI job runs every ainb-core tripwire test not
// named in `tests/tripwire_ci_exclusions.txt`, and expects exactly the tests in
// `tests/tripwire_ci_skips.txt` to SKIP. This keeps both lists honest: each
// line names a real test, once, with its reason (and an issue, to exclude).

use std::collections::BTreeSet;
use std::path::Path;

fn tests_dir() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests")
}

/// Check every `binary::test` line of `list`, returning the fields after the
/// name for the caller to check.
fn check_list(list: &str, mut fields_ok: impl FnMut(&str, &[&str])) {
    let text = std::fs::read_to_string(tests_dir().join(list))
        .unwrap_or_else(|_| panic!("{list} is committed beside the tests"));
    let mut seen = BTreeSet::new();
    for (number, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let fields: Vec<&str> = line.split_whitespace().collect();
        let at = format!("{list} line {}: `{line}`", number + 1);
        let (binary, test) = fields[0]
            .split_once("::")
            .unwrap_or_else(|| panic!("{at}: the name must be binary::test"));
        assert!(
            binary.starts_with("tripwire_"),
            "{at}: not a tripwire binary"
        );
        let source = std::fs::read_to_string(tests_dir().join(format!("{binary}.rs")))
            .unwrap_or_else(|_| panic!("{at}: no tests/{binary}.rs, so the line is stale"));
        assert!(
            source.contains(&format!("fn {test}(")),
            "{at}: tests/{binary}.rs has no `fn {test}`, so the line is stale"
        );
        assert!(seen.insert(fields[0].to_string()), "{at}: listed twice");
        fields_ok(&at, &fields[1..]);
    }
}

#[test]
fn every_exclusion_names_a_tripwire_test_once_with_an_issue_and_a_reason() {
    check_list("tripwire_ci_exclusions.txt", |at, rest| {
        let issue = rest.first().copied().unwrap_or_default();
        assert!(
            issue.len() > 1
                && issue.starts_with('#')
                && issue[1..].chars().all(|c| c.is_ascii_digit()),
            "{at}: the second field must be an issue like #1023"
        );
        assert!(rest.len() > 1, "{at}: an exclusion needs a reason");
    });
}

#[test]
fn every_expected_skip_names_a_tripwire_test_once_with_a_reason() {
    check_list("tripwire_ci_skips.txt", |at, rest| {
        assert!(!rest.is_empty(), "{at}: an expected SKIP needs a reason");
    });
}

#[test]
fn no_test_is_both_excluded_and_expected_to_skip() {
    let names = |list: &str| -> BTreeSet<String> {
        std::fs::read_to_string(tests_dir().join(list))
            .expect("list")
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .filter_map(|line| line.split_whitespace().next().map(str::to_string))
            .collect()
    };
    let both: Vec<String> = names("tripwire_ci_exclusions.txt")
        .intersection(&names("tripwire_ci_skips.txt"))
        .cloned()
        .collect();
    assert!(
        both.is_empty(),
        "an excluded test never runs, so it cannot SKIP: {both:?}"
    );
}
