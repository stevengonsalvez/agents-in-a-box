#![allow(missing_docs)]

// ABOUTME: The terminal's git view scrolls its Commits tab by the wheel. The
// wheel is the `git_view.scroll` command the reducer applies (#1242); this
// paints the tab afterwards and reads what the person would see.

use ainb::app::NoRenderer;
use ainb::app::pointer;
use ainb::app::screens::ids as screen_ids;
use ainb::components::code_review::render::ReviewSidebarLayout;
use ainb::components::git_view::{GitTab, GitViewComponent, GitViewState};
use ainb::git::operations::CommitInfo;
use ainb::{AppState, Keymap, dispatch};
use ratatui::Terminal;
use ratatui::backend::TestBackend;

fn screen_text(terminal: &Terminal<TestBackend>) -> String {
    let buffer = terminal.backend().buffer();
    let mut text = String::new();
    for y in 0..buffer.area.height {
        for x in 0..buffer.area.width {
            text.push_str(buffer[(x, y)].symbol());
        }
        text.push('\n');
    }
    text
}

#[test]
fn the_wheel_brings_later_commits_into_the_terminal_list() {
    let mut state = AppState::new();
    state.shell.current_screen = screen_ids::GIT_VIEW.to_string();
    let mut git = GitViewState::new("/parity/api".into());
    git.active_tab = GitTab::Commits;
    git.commits = (0..60)
        .map(|n| CommitInfo {
            hash_short: format!("c{n:04}"),
            author: "Sample Dev".to_string(),
            date: "2026-09-19".to_string(),
            message: format!("commit {n}"),
        })
        .collect();
    state.git_view.git_view_state = Some(git);

    for _ in 0..10 {
        let _ = dispatch(
            &mut state,
            &Keymap::defaults(),
            &mut NoRenderer,
            pointer::scroll_git_view(3),
        );
    }

    let mut terminal = Terminal::new(TestBackend::new(100, 24)).expect("terminal");
    let git = state.git_view.git_view_state.as_ref().expect("git view");
    terminal
        .draw(|frame| {
            GitViewComponent::render(
                frame,
                frame.area(),
                git,
                &mut ReviewSidebarLayout::default(),
            );
        })
        .expect("draws");
    let text = screen_text(&terminal);
    assert!(
        text.contains("c0030"),
        "thirty lines down shows commit 30:\n{text}"
    );
    assert!(
        !text.contains("c0000"),
        "the first commit has scrolled away:\n{text}"
    );
}
