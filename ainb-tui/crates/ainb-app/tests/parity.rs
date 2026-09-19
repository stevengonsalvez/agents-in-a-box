// ABOUTME: Every committed parity fixture builds into an AppState that opens
// the screen it names, and every fixture with an expected-facts list shows
// each of those facts in the screen the ratatui half drew.
//
// The renderer half lives in `ainb-core/tests/parity_snapshots.rs`, which
// draws these same fixtures, and the webview half in
// `ainb-desktop/ui/src/parity.test.ts`, which renders them from the committed
// frames. The facts list is ONE file read by both, so a fact only one renderer
// shows is a fact the other is missing, rather than two lists that drift.

#[path = "parity/support.rs"]
mod support;

#[path = "support/home.rs"]
mod home;

use home::ScopedHome;

use std::path::Path;

use support::ParityFixture;

#[test]
fn every_parity_fixture_builds_the_screen_it_names() {
    let _home = ScopedHome::new();

    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/parity");
    let fixtures = ParityFixture::all_in(&dir);
    assert!(
        // 19: sixteen with the three plugin placeholder fixtures (D3p-f),
        // seventeen with the Commits tab's, eighteen with the stats fixture
        // (D3p-e) and nineteen with the inbox fixture (D3p-d).
        fixtures.len() >= 19,
        "expected a fixture per screen, found {}",
        fixtures.len()
    );
    for (name, path) in fixtures {
        let fixture = ParityFixture::load(&path).unwrap_or_else(|error| panic!("{error}"));
        let state = fixture.build();
        assert_eq!(state.shell.current_screen, fixture.screen_id(), "{name}");
        let sessions: usize = state.sessions.workspaces.iter().map(|w| w.sessions.len()).sum();
        let expected: usize = fixture.workspaces.iter().map(|w| w.sessions.len()).sum();
        assert_eq!(sessions, expected, "{name}");
        // A DOM-only fixture has no ratatui half, so no snapshot, and says why.
        if let Some(reason) = &fixture.dom_only {
            assert!(
                !reason.trim().is_empty(),
                "{name}: dom_only names no reason"
            );
            assert!(
                !dir.join(format!("{name}.snap")).is_file(),
                "{name} is DOM-only but has a ratatui snapshot"
            );
            continue;
        }
        assert!(
            dir.join(format!("{name}.snap")).is_file(),
            "{name} has no committed snapshot"
        );
    }
}

/// The facts a fixture must show, as its list gives them: comments and blank
/// lines dropped.
fn facts(list: &str) -> Vec<&str> {
    list.lines()
        .map(str::trim_end)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect()
}

/// Every fact of `list` that `drawn` does not show.
///
/// The screen is joined into one text first: a fact is what the screen says,
/// not where it says it, and the two renderers wrap and pad differently.
fn missing<'a>(drawn: &str, list: &'a str) -> Vec<&'a str> {
    let text: String = drawn.lines().map(str::trim).collect::<Vec<_>>().join("\n");
    facts(list).into_iter().filter(|fact| !text.contains(fact)).collect()
}

#[test]
fn every_expected_fact_is_on_the_screen_the_ratatui_half_drew() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/parity");
    let mut checked = 0;
    for (name, path) in ParityFixture::all_in(&dir) {
        let list = dir.join(format!("facts/{name}.txt"));
        let fixture = ParityFixture::load(&path).unwrap_or_else(|error| panic!("{error}"));
        // A DOM-only fixture's facts are the webview half's alone.
        if !list.is_file() || fixture.dom_only.is_some() {
            continue;
        }
        let list = std::fs::read_to_string(&list).expect("the facts list");
        let drawn = std::fs::read_to_string(dir.join(format!("{name}.snap")))
            .unwrap_or_else(|error| panic!("{name}: no committed snapshot: {error}"));

        assert!(
            !facts(&list).is_empty(),
            "{name}: the facts list has no facts in it"
        );
        assert_eq!(
            missing(&drawn, &list),
            Vec::<&str>::new(),
            "{name}: the screen the ratatui half drew is missing these facts"
        );
        checked += 1;
    }
    assert!(checked > 0, "no fixture has an expected-facts list");
}

// The check that the facts list can FAIL lives with the renderers, because
// only they can lose a fact: `ainb-core/tests/parity_snapshots.rs`
// (`a_renderer_that_loses_a_file_fails_the_facts`) takes a file out of what
// the ratatui half is given, and `ainb-desktop/ui/src/parity.test.ts` takes it
// out of the frame the webview half is given. Editing a drawing after it was
// drawn would only prove the comparator works.
