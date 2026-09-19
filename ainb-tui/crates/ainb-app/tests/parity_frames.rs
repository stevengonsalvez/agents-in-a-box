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

/// `value`, canonical: object keys sorted, timestamps rewritten to
/// [`FIXED_INSTANT`], the scratch home to [`FIXED_HOME`].
///
/// A map on the wire is a `HashMap` more often than not, and this workspace
/// builds `serde_json` with insertion order preserved, so two runs of one fixture
/// can emit the same object with its keys in different order. Arrays are left
/// exactly as they are: their order is the thing a renderer draws.
///
/// `key` is the field the string sits under, because only a field that HOLDS a
/// time is flattened: a commit message or a log line that happens to parse as
/// RFC 3339 is content, and rewriting it would hide a real change from the
/// diff.
fn canonical(value: serde_json::Value, homes: &[String], key: Option<&str>) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let sorted: std::collections::BTreeMap<String, serde_json::Value> = map
                .into_iter()
                .map(|(name, value)| {
                    let value = canonical(value, homes, Some(name.as_str()));
                    (name, value)
                })
                .collect();
            serde_json::Value::Object(sorted.into_iter().collect())
        }
        serde_json::Value::Array(items) => serde_json::Value::Array(
            items.into_iter().map(|item| canonical(item, homes, key)).collect(),
        ),
        serde_json::Value::String(text) => {
            if key.is_some_and(holds_a_time) && chrono::DateTime::parse_from_rfc3339(&text).is_ok()
            {
                return serde_json::Value::String(FIXED_INSTANT.to_string());
            }
            let mut text = text;
            for home in homes {
                if text.contains(home.as_str()) {
                    text = text.replace(home.as_str(), FIXED_HOME);
                }
            }
            serde_json::Value::String(text)
        }
        other => other,
    }
}

/// Whether a field named `key` holds a time rather than text that might read
/// like one.
///
/// Named fields rather than every string that parses: a commit message, a log
/// line or a path can parse as RFC 3339, and flattening one would hide a real
/// change from the diff. A time field this misses shows up as an unstable dump
/// in `framing_every_fixture_twice_gives_the_same_bytes`, which is the loud
/// failure rather than the silent one.
fn holds_a_time(key: &str) -> bool {
    const MARKS: &[&str] = &[
        "_at",
        "date",
        "time",
        "accessed",
        "created",
        "updated",
        "modified",
        "seen",
        "since",
        "expires",
        "started",
        "finished",
        "heartbeat",
        "poll",
        "stamp",
        "deadline",
    ];
    MARKS.iter().any(|mark| key.contains(mark))
}

/// Every section of `fixture`, keyed by wire name, as a window receives them.
///
/// The host id is the fixed local one, never this box's, so the dump is the
/// fixture's and not the machine's.
fn frames(fixture: &ParityFixture, homes: &[String]) -> String {
    let state = fixture.build();
    let host = HostId::local();
    let sections: serde_json::Map<String, serde_json::Value> = SectionId::ALL
        .into_iter()
        .map(|id| {
            (
                section_name(id).to_string(),
                canonical(section_json(&state, id, &host), homes, None),
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
    let homes = homes_of(home.path());

    let dir = fixture_dir();
    std::fs::create_dir_all(frames_dir()).expect("the frames directory");
    let mut stale = Vec::new();
    let mut expected = std::collections::BTreeSet::new();
    for (name, path) in ParityFixture::all_in(&dir) {
        let fixture = ParityFixture::load(&path).unwrap_or_else(|error| panic!("{error}"));
        let framed = frames(&fixture, &homes);
        let committed = frames_dir().join(format!("{name}.json"));
        expected.insert(format!("{name}.json"));

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

    // A fixture that was renamed or deleted leaves its dump behind, and an
    // orphan dump is a renderer's input that no fixture builds any more.
    let mut orphans: Vec<String> = std::fs::read_dir(frames_dir())
        .expect("the frames directory")
        .filter_map(|entry| {
            let path = entry.ok()?.path();
            if path.extension()? != "json" {
                return None;
            }
            Some(path.file_name()?.to_string_lossy().into_owned())
        })
        .filter(|name| !expected.contains(name))
        .collect();
    orphans.sort();
    if std::env::var_os("UPDATE_PARITY_FRAMES").is_some() {
        for orphan in &orphans {
            std::fs::remove_file(frames_dir().join(orphan)).expect("remove the orphan dump");
        }
        orphans.clear();
    }

    assert!(
        stale.is_empty(),
        "the framed sections of {stale:?} changed. Read the diff: a field that moved here is a field the window's renderer reads. Rewrite with UPDATE_PARITY_FRAMES=1 once it is deliberate"
    );
    assert!(
        orphans.is_empty(),
        "{orphans:?} in the frames directory belong to no fixture. Delete them, or rewrite with UPDATE_PARITY_FRAMES=1"
    );
}

/// Every spelling of the scratch home a fixture can carry: the path as it was
/// handed out, and the one the filesystem resolves it to. On macOS a temporary
/// directory lives under a symlink (`/var` to `/private/var`), so a path a
/// fixture canonicalised would slip past a literal match.
fn homes_of(home: &Path) -> Vec<String> {
    let mut homes = vec![home.display().to_string()];
    if let Ok(resolved) = home.canonicalize() {
        let resolved = resolved.display().to_string();
        if !homes.contains(&resolved) {
            homes.push(resolved);
        }
    }
    // The longest first, so a path that contains the other is rewritten whole.
    homes.sort_by_key(|home| std::cmp::Reverse(home.len()));
    homes
}

/// The dump is what the DOM half reads, so it has to be stable: the same
/// fixture framed twice is the same bytes, or a diff means nothing.
///
/// Every fixture, not one: a clock or a map that only one fixture carries is
/// exactly the instability that would land as a mystery diff in CI later.
#[test]
fn framing_every_fixture_twice_gives_the_same_bytes() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let home = tempfile::tempdir().expect("scratch home");
    // As above: a scratch home, never this box's.
    std::env::set_var("HOME", home.path());
    let homes = homes_of(home.path());

    for (name, path) in ParityFixture::all_in(&fixture_dir()) {
        let fixture = ParityFixture::load(&path).unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(
            frames(&fixture, &homes),
            frames(&fixture, &homes),
            "{name} framed twice"
        );
    }
}
