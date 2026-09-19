#![allow(missing_docs)]

// ABOUTME: Each parity fixture's framed sections, committed beside it, so the
// DOM half of parity renders from what a window actually receives.
//
// The ratatui half builds the fixture into an `AppState` and draws it
// (`ainb-core/tests/parity_snapshots.rs`). A webview never sees an `AppState`:
// it sees frames. So this writes `<fixture>.frames.json`, one entry per
// section, and the node runner renders the same fixture from that file. One
// fixture, two renderers, one set of facts to diff.
//
// The dump is the contract, so a plain run never rewrites it: set
// `UPDATE_PARITY_FRAMES=1` deliberately, read the diff, and commit it.

#[path = "parity/support.rs"]
mod support;

use std::path::{Path, PathBuf};

use ainb_app::SectionId;
use ainb_app::wire::frame::HostId;
use ainb_app::wire::{section_json, section_name};
use support::ParityFixture;

/// Both tests set `HOME` in one process, so they take turns: without this the
/// fixture one test builds can be built under the other's scratch home, and the
/// dumps differ by whose home won the race.
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/parity")
}

/// Where the dumps live: a directory of their own, not beside the fixtures.
/// `ParityFixture::all_in` takes every `.json` in the fixture directory as a
/// fixture, so a dump parked there would be loaded as one and fail to parse.
fn frames_dir() -> PathBuf {
    fixture_dir().join("frames")
}

/// A fixed instant every timestamp in a dump is rewritten to.
///
/// A fixture's sessions are stamped when they are built, so two runs of one
/// fixture differ by the clock alone. What the parity suite compares is what a
/// renderer draws from a frame, not when the fixture was made.
const FIXED_INSTANT: &str = "1970-01-01T00:00:00Z";

/// What the scratch home is rewritten to: a fixture built under a temporary
/// home carries that path in its text, and it is a new path every run.
const FIXED_HOME: &str = "<home>";

/// `value` with every object's keys in sorted order, and every timestamp
/// rewritten to [`FIXED_INSTANT`].
///
/// A map on the wire is a `HashMap` more often than not, and this workspace
/// builds serde_json with insertion order preserved, so two runs of one fixture
/// can emit the same object with its keys in different order. Arrays are left
/// exactly as they are: their order is the thing a renderer draws.
fn canonical(value: serde_json::Value, home: &str) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let sorted: std::collections::BTreeMap<String, serde_json::Value> =
                map.into_iter().map(|(key, value)| (key, canonical(value, home))).collect();
            serde_json::Value::Object(sorted.into_iter().collect())
        }
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.into_iter().map(|item| canonical(item, home)).collect())
        }
        serde_json::Value::String(text) if chrono::DateTime::parse_from_rfc3339(&text).is_ok() => {
            serde_json::Value::String(FIXED_INSTANT.to_string())
        }
        serde_json::Value::String(text) if text.contains(home) => {
            serde_json::Value::String(text.replace(home, FIXED_HOME))
        }
        other => other,
    }
}

/// Every section of `fixture`, keyed by wire name, as a window receives them.
///
/// The host id is the fixed local one, never this box's, so the dump is the
/// fixture's and not the machine's.
fn frames(fixture: &ParityFixture, home: &str) -> String {
    let state = fixture.build();
    let host = HostId::local();
    let sections: serde_json::Map<String, serde_json::Value> = SectionId::ALL
        .into_iter()
        .map(|id| {
            (
                section_name(id).to_string(),
                canonical(section_json(&state, id, &host), home),
            )
        })
        .collect();
    let mut body = serde_json::to_string_pretty(&serde_json::Value::Object(sections))
        .expect("the frames encode");
    body.push('\n');
    body
}

#[test]
fn every_fixture_frames_its_committed_sections() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let home = tempfile::tempdir().expect("scratch home");
    // The parity tests own this process's environment; the fixtures read no
    // real home.
    std::env::set_var("HOME", home.path());
    let home_path = home.path().display().to_string();

    let dir = fixture_dir();
    std::fs::create_dir_all(frames_dir()).expect("the frames directory");
    let mut stale = Vec::new();
    for (name, path) in ParityFixture::all_in(&dir) {
        let fixture = ParityFixture::load(&path).unwrap_or_else(|error| panic!("{error}"));
        let framed = frames(&fixture, &home_path);
        let committed = frames_dir().join(format!("{name}.json"));

        if std::env::var_os("UPDATE_PARITY_FRAMES").is_some() {
            std::fs::write(&committed, &framed).expect("write the frames");
            continue;
        }

        let held = std::fs::read_to_string(&committed).unwrap_or_else(|error| {
            panic!(
                "{name}: no frames at {}: {error}; write them with UPDATE_PARITY_FRAMES=1",
                committed.display()
            )
        });
        if held != framed {
            stale.push(name);
        }
    }

    assert!(
        stale.is_empty(),
        "the framed sections of {stale:?} changed. Read the diff: a field that moved here is a field the window's renderer reads. Rewrite with UPDATE_PARITY_FRAMES=1 once it is deliberate"
    );
}

/// The dump is what the DOM half reads, so it has to be stable: the same
/// fixture framed twice is the same bytes, or a diff means nothing.
#[test]
fn framing_one_fixture_twice_gives_the_same_bytes() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let home = tempfile::tempdir().expect("scratch home");
    // As above: a scratch home, never this box's.
    std::env::set_var("HOME", home.path());
    let home_path = home.path().display().to_string();

    let dir = fixture_dir();
    let (name, path) = ParityFixture::all_in(&dir).into_iter().next().expect("a fixture");
    let fixture = ParityFixture::load(&path).unwrap_or_else(|error| panic!("{error}"));

    assert_eq!(
        frames(&fixture, &home_path),
        frames(&fixture, &home_path),
        "{name} framed twice"
    );
}
