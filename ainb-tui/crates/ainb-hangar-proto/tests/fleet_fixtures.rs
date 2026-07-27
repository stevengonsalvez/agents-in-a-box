//! Deterministic cross-language Fleet fixture currentness gate.

#[allow(dead_code)]
#[path = "../examples/export_fleet_fixtures.rs"]
mod exporter;

use std::fs;

#[test]
fn fleet_fixtures_are_current() {
    let expected = exporter::fixtures();
    let directory = exporter::fixture_directory();
    let expected_names: Vec<_> = expected.iter().map(|(name, _)| *name).collect();
    let mut actual_names: Vec<_> = fs::read_dir(&directory)
        .expect("fixture directory exists")
        .map(|entry| entry.expect("fixture entry is readable"))
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| name.ends_with(".json"))
        .collect();
    actual_names.sort_unstable();
    assert_eq!(actual_names, expected_names, "fixture file list drifted");

    for (name, value) in expected {
        let actual = fs::read(directory.join(name)).expect("fixture exists");
        assert_eq!(
            actual,
            exporter::fixture_bytes(&value),
            "fixture drifted: {name}"
        );
    }
}
