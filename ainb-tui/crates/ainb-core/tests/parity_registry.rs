// ABOUTME: The terminal's half of the parity enumeration: every screen the
// terminal registers has a row in `ainb-app/tests/parity/screens.txt`, and
// every screen a row says the ratatui half draws is one the terminal can
// open (registered, or drawn by the layout's split-pane path).

use std::collections::BTreeSet;
use std::path::Path;

use ainb::components::LayoutComponent;

fn rows() -> Vec<(String, String)> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../ainb-app/tests/parity/screens.txt");
    std::fs::read_to_string(&path)
        .expect("screens.txt")
        .lines()
        .filter(|line| !line.trim().is_empty() && !line.starts_with('#'))
        .map(|line| {
            let mut cells = line.split('\t');
            (
                cells.next().expect("screen").to_string(),
                cells.next().expect("coverage").to_string(),
            )
        })
        .collect()
}

#[test]
fn every_registered_screen_has_a_row() {
    let rows = rows();
    let named: BTreeSet<&str> = rows.iter().map(|(screen, _)| screen.as_str()).collect();
    let layout = LayoutComponent::new();
    let unlisted: Vec<&str> = layout
        .screen_ids()
        .iter()
        .map(String::as_str)
        .filter(|id| !named.contains(id))
        .collect();
    assert!(
        unlisted.is_empty(),
        "screens the terminal registers with no row in screens.txt: {unlisted:?}"
    );
}

/// The screens the layout draws on its split-pane path, outside the
/// registry: `LayoutComponent`'s own list.
const SPLIT_PANE: [&str; 6] = [
    "session_list",
    "logs",
    "new_session",
    "claude_chat",
    "search_workspace",
    "non_git_notification",
];

/// Every screen a `both` row says the ratatui half draws is one the terminal
/// opens: registered, or drawn by the layout's split-pane path.
#[test]
fn every_row_the_ratatui_half_draws_is_a_screen_the_terminal_opens() {
    let layout = LayoutComponent::new();
    let registered: BTreeSet<&str> = layout.screen_ids().iter().map(String::as_str).collect();
    let unopenable: Vec<String> = rows()
        .into_iter()
        .filter(|(_, coverage)| coverage == "both")
        .map(|(screen, _)| screen)
        .filter(|screen| {
            !registered.contains(screen.as_str()) && !SPLIT_PANE.contains(&screen.as_str())
        })
        .collect();
    assert!(
        unopenable.is_empty(),
        "both rows naming screens the terminal neither registers nor draws: {unopenable:?}"
    );
}
