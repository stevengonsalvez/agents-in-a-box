// ABOUTME: Every committed parity fixture builds into an AppState that opens
// the screen it names. The renderer half of parity lives in
// `ainb-core/tests/parity_snapshots.rs`, which draws these same fixtures.

#[path = "parity/support.rs"]
mod support;

use std::path::Path;

use support::ParityFixture;

#[test]
fn every_parity_fixture_builds_the_screen_it_names() {
    let home = tempfile::tempdir().expect("scratch home");
    std::env::set_var("HOME", home.path());

    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/parity");
    let fixtures = ParityFixture::all_in(&dir);
    assert!(
        fixtures.len() >= 12,
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
        assert!(
            dir.join(format!("{name}.snap")).is_file(),
            "{name} has no committed snapshot"
        );
    }
}
