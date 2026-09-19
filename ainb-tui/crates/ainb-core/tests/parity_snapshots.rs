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
    let mut state = fixture.build();
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

/// The tests set `HOME` in one process, so they take turns.
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The expected facts committed beside `fixture`, `<fixture>.facts`, one per
/// line with `#` comments, or `None` when the fixture has no list. The same
/// file is read by the DOM half (`ainb-desktop/ui/src/parity.test.ts`), so one
/// list is diffed against both renderers.
fn facts(fixture: &Path) -> Option<Vec<String>> {
    let text = std::fs::read_to_string(fixture.with_extension("facts")).ok()?;
    Some(
        text.lines()
            .map(str::trim_end)
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .map(str::to_string)
            .collect(),
    )
}

/// The facts no line of `rendered` contains.
fn missing_facts(rendered: &str, facts: &[String]) -> Vec<String> {
    facts
        .iter()
        .filter(|fact| !rendered.lines().any(|line| line.contains(fact.as_str())))
        .cloned()
        .collect()
}

#[test]
fn every_fixture_renders_its_committed_snapshot() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
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

/// The screens the settings page draws (D3d) each carry a facts list, and the
/// terminal renders every fact of every fixture that has one.
#[test]
fn every_fixture_with_a_facts_list_renders_every_fact() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let home = tempfile::tempdir().expect("scratch home");
    std::env::set_var("HOME", home.path());

    let mut checked = Vec::new();
    let mut missing = Vec::new();
    for (name, path) in ParityFixture::all_in(&fixture_dir()) {
        let Some(facts) = facts(&path) else {
            continue;
        };
        assert!(!facts.is_empty(), "{name}.facts lists at least one fact");
        let fixture = ParityFixture::load(&path).unwrap_or_else(|error| panic!("{error}"));
        let absent = missing_facts(&render(&fixture), &facts);
        if !absent.is_empty() {
            missing.push(format!("{name}: {absent:?}"));
        }
        checked.push(name);
    }
    for required in ["config", "daemons"] {
        assert!(
            checked.iter().any(|name| name == required),
            "the settings page draws `{required}`, which needs a facts list"
        );
    }
    assert!(
        missing.is_empty(),
        "facts missing from the render:\n{}",
        missing.join("\n")
    );
}

/// The suite can fail: with one fact deleted from the render, the check
/// names it.
#[test]
fn the_facts_check_fails_when_one_fact_is_deleted_from_the_render() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let home = tempfile::tempdir().expect("scratch home");
    std::env::set_var("HOME", home.path());

    let path = fixture_dir().join("config.json");
    let facts = facts(&path).expect("config.facts");
    let fixture = ParityFixture::load(&path).unwrap_or_else(|error| panic!("{error}"));
    let rendered = render(&fixture);
    assert_eq!(missing_facts(&rendered, &facts), Vec::<String>::new());

    let deleted = facts.last().expect("a fact").clone();
    let mutated = rendered.replace(deleted.as_str(), "");
    assert_eq!(missing_facts(&mutated, &facts), vec![deleted]);
}
