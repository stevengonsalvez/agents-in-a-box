// ABOUTME: Parity: each fixture under `ainb-app/tests/parity/` renders through
// the terminal host to the text snapshot committed beside it. The snapshots
// were drawn before the P2 to P5 extraction started; every staged change must
// leave them byte-identical. `UPDATE_PARITY_SNAPSHOTS=1` rewrites them, which
// only a deliberate UI change should ever do.

#[path = "../../ainb-app/tests/parity/support.rs"]
mod support;

use std::path::{Path, PathBuf};

use ainb::app::ui_state::UiState;
use ainb::components::LayoutComponent;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use support::ParityFixture;

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../ainb-app/tests/parity")
}

/// Draw one frame of `fixture` and return it as text, one line per row.
fn render(fixture: &ParityFixture) -> String {
    draw(fixture, |_| {})
}

/// [`render`], with `change` applied to the state the fixture built before it
/// is drawn: what a renderer that lost something would have shown.
fn draw(fixture: &ParityFixture, change: impl FnOnce(&mut ainb::app::AppState)) -> String {
    let mut state = fixture.build();
    change(&mut state);
    // What the host's startup and tick do before a frame: the status bar draws
    // from the sections this fills.
    state.refresh_statusline();
    let layout = LayoutComponent::new();
    layout.tick_before_draw(&mut state);
    let mut layout = layout;
    let mut ui = UiState::default();
    let mut terminal =
        Terminal::new(TestBackend::new(fixture.width, fixture.height)).expect("test terminal");
    terminal.draw(|frame| layout.render(frame, &state, &mut ui)).expect("draw");
    let buffer = terminal.backend().buffer();
    let mut text = String::new();
    for y in 0..buffer.area.height {
        let line: String = (0..buffer.area.width).map(|x| buffer[(x, y)].symbol()).collect();
        text.push_str(line.trim_end());
        text.push('\n');
    }
    // The release version is printed on the home banner; it is not state.
    text.replace(&format!("v{}", env!("CARGO_PKG_VERSION")), "v<version>")
}

#[test]
fn every_fixture_renders_its_committed_snapshot() {
    let home = tempfile::tempdir().expect("scratch home");
    std::env::set_var("HOME", home.path());
    let update = std::env::var_os("UPDATE_PARITY_SNAPSHOTS").is_some();

    let mut mismatched = Vec::new();
    for (name, path) in ParityFixture::all_in(&fixture_dir()) {
        let fixture = ParityFixture::load(&path).unwrap_or_else(|error| panic!("{error}"));
        let frame = render(&fixture);
        let snap = path.with_extension("snap");
        if update {
            std::fs::write(&snap, &frame).expect("write snapshot");
            continue;
        }
        let committed = std::fs::read_to_string(&snap)
            .unwrap_or_else(|error| panic!("{name}: no snapshot at {}: {error}", snap.display()));
        if committed != frame {
            mismatched.push(format!(
                "--- {name} committed\n{committed}+++ {name} rendered\n{frame}"
            ));
        }
    }
    assert!(
        mismatched.is_empty(),
        "parity snapshots changed:\n{}",
        mismatched.join("\n")
    );
}

/// The facts a fixture must show, as its list gives them.
fn facts(list: &str) -> Vec<&str> {
    list.lines()
        .map(str::trim_end)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect()
}

/// The expected facts of `fixture` the drawing `drawn` does not show.
fn missing<'a>(drawn: &str, list: &'a str) -> Vec<&'a str> {
    let text: String = drawn.lines().map(str::trim).collect::<Vec<_>>().join("\n");
    facts(list).into_iter().filter(|fact| !text.contains(fact)).collect()
}

/// The expected-facts check has to be able to fail, or a renderer that drew
/// nothing would pass it. So this takes a file out of what the RENDERER is
/// given, draws again, and asserts the facts that file carried are reported
/// missing: the check is against the drawing, not against a string edited
/// after the fact.
#[test]
fn a_renderer_that_loses_a_file_fails_the_facts() {
    let home = tempfile::tempdir().expect("scratch home");
    std::env::set_var("HOME", home.path());

    let dir = fixture_dir();
    let fixture = ParityFixture::load(&dir.join("git_review.json")).expect("the review fixture");
    let list = std::fs::read_to_string(dir.join("facts/git_review.txt")).expect("the facts list");

    assert_eq!(
        missing(&render(&fixture), &list),
        Vec::<&str>::new(),
        "the fixture as it stands shows every fact"
    );

    let lost = draw(&fixture, |state| {
        let git = state.git_view.git_view_state.as_mut().expect("the git view");
        git.review.files.remove(0);
    });

    assert!(
        !missing(&lost, &list).is_empty(),
        "a renderer missing a whole file still showed every expected fact, so the list proves nothing"
    );
}
