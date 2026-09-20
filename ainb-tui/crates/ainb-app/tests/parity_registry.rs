#![allow(missing_docs)]

// ABOUTME: The parity enumeration is a file, `tests/parity/screens.txt`, and
// this walks it against the one place every screen id is declared: a screen
// id with no row, a row naming no screen id, and a ratatui-covered row with
// no fixture, snapshot or facts list each fail. So a screen added without a
// row is a failed test, not a criterion someone has to remember.

#[path = "parity/support.rs"]
mod support;

#[path = "support/home.rs"]
mod home;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use ainb_app::app::screens::ids;
use home::ScopedHome;
use support::ParityFixture;

fn parity_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/parity")
}

/// One row of the enumeration.
struct Row {
    screen: String,
    coverage: String,
    detail: String,
}

fn rows() -> Vec<Row> {
    let text = std::fs::read_to_string(parity_dir().join("screens.txt")).expect("screens.txt");
    text.lines()
        .filter(|line| !line.trim().is_empty() && !line.starts_with('#'))
        .map(|line| {
            let mut cells = line.splitn(3, '\t');
            let screen = cells.next().expect("screen").to_string();
            let coverage =
                cells.next().unwrap_or_else(|| panic!("{screen}: no coverage")).to_string();
            let detail = cells.next().unwrap_or_else(|| panic!("{screen}: no detail")).to_string();
            Row {
                screen,
                coverage,
                detail,
            }
        })
        .collect()
}

/// The ids as the source declares them, so `ids::ALL` cannot fall behind a
/// constant added beside it.
fn declared_ids() -> BTreeSet<String> {
    let source = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("src/app/screens/mod.rs"),
    )
    .expect("screens/mod.rs");
    source
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            let rest = line.strip_prefix("pub const ")?;
            let (_, value) = rest.split_once(": &str = \"")?;
            Some(value.trim_end_matches("\";").to_string())
        })
        .collect()
}

#[test]
fn ids_all_is_every_declared_screen_id_once() {
    let listed: Vec<&str> = ids::ALL.to_vec();
    let set: BTreeSet<String> = listed.iter().map(ToString::to_string).collect();
    assert_eq!(set.len(), listed.len(), "an id is listed twice: {listed:?}");
    assert_eq!(set, declared_ids(), "ids::ALL and the constants differ");
}

#[test]
fn every_screen_id_has_one_row_and_every_row_names_a_screen_id() {
    let rows = rows();
    let named: Vec<&str> = rows.iter().map(|row| row.screen.as_str()).collect();
    let set: BTreeSet<&str> = named.iter().copied().collect();
    assert_eq!(set.len(), named.len(), "a screen has two rows: {named:?}");
    let all: BTreeSet<&str> = ids::ALL.iter().copied().collect();
    let missing: Vec<&&str> = all.iter().filter(|id| !set.contains(**id)).collect();
    assert!(
        missing.is_empty(),
        "screens with no row in screens.txt: {missing:?}"
    );
    let stale: Vec<&&str> = set.iter().filter(|id| !all.contains(**id)).collect();
    assert!(stale.is_empty(), "rows naming no screen id: {stale:?}");
}

/// A `both` row has its fixture, its snapshot and its frames; a `dom` row its
/// fixture, frames and facts and no snapshot; an `excluded` row its reason.
/// Facts for `both` rows are read wherever a list exists (`parity.rs`), and
/// requiring one on every `both` row is the follow-up that writes the ten
/// lists still missing.
#[test]
fn every_covered_row_has_its_fixture_snapshot_and_frames_and_every_excluded_row_its_reason() {
    let dir = parity_dir();
    for row in rows() {
        match row.coverage.as_str() {
            "both" => {
                for fixture in row.detail.split_whitespace() {
                    for file in [format!("{fixture}.json"), format!("{fixture}.snap")] {
                        assert!(
                            dir.join(&file).is_file(),
                            "{}: a both row without its ratatui half: {file}",
                            row.screen
                        );
                    }
                    assert!(
                        dir.join("frames").join(format!("{fixture}.json")).is_file(),
                        "{}: a both row without the frames the DOM half reads: {fixture}",
                        row.screen
                    );
                }
            }
            "dom" => {
                for fixture in row.detail.split_whitespace() {
                    assert!(
                        dir.join(format!("{fixture}.json")).is_file(),
                        "{}: a dom row without its fixture: {fixture}.json",
                        row.screen
                    );
                    assert!(
                        !dir.join(format!("{fixture}.snap")).is_file(),
                        "{}: a dom row with a ratatui snapshot is a both row: {fixture}",
                        row.screen
                    );
                    for file in [
                        format!("frames/{fixture}.json"),
                        format!("facts/{fixture}.txt"),
                    ] {
                        assert!(
                            dir.join(&file).is_file(),
                            "{}: a dom row without what its one half reads: {file}",
                            row.screen
                        );
                    }
                }
            }
            "excluded" => {
                assert!(
                    row.detail.split_whitespace().count() >= 2,
                    "{}: an excluded row carries its reason",
                    row.screen
                );
            }
            other => panic!("{}: unknown coverage {other:?}", row.screen),
        }
    }
}

/// Every fixture on disk is claimed by a row, so a fixture added without a
/// row is as loud as a screen without one.
#[test]
fn every_fixture_on_disk_is_named_by_a_row() {
    let claimed: BTreeSet<String> = rows()
        .iter()
        .filter(|row| row.coverage == "both" || row.coverage == "dom")
        .flat_map(|row| row.detail.split_whitespace().map(ToString::to_string))
        .collect();
    let mut unclaimed = Vec::new();
    for entry in std::fs::read_dir(parity_dir()).expect("parity dir") {
        let path = entry.expect("entry").path();
        if path.extension().is_some_and(|ext| ext == "json") {
            let name = path.file_stem().unwrap().to_string_lossy().to_string();
            if !claimed.contains(&name) {
                unclaimed.push(name);
            }
        }
    }
    assert!(unclaimed.is_empty(), "fixtures no row names: {unclaimed:?}");
}

/// A row's fixture builds the row's screen: the state the fixture makes opens
/// the screen the row names, so a fixture filed under the wrong screen fails
/// here rather than passing on the file's existence.
#[test]
fn every_row_fixture_builds_the_row_s_screen() {
    let _home = ScopedHome::new();
    let dir = parity_dir();
    for row in rows() {
        if row.coverage != "both" && row.coverage != "dom" {
            continue;
        }
        for fixture in row.detail.split_whitespace() {
            let loaded = ParityFixture::load(&dir.join(format!("{fixture}.json")))
                .unwrap_or_else(|error| panic!("{error}"));
            let state = loaded.build();
            assert_eq!(
                state.shell.current_screen, row.screen,
                "{fixture} builds a screen its row does not name"
            );
        }
    }
}
