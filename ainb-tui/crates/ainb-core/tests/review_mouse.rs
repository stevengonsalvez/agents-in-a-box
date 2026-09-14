#![allow(missing_docs)]

// ABOUTME: A press on the code review sidebar names the row by path, from the
// geometry the renderer recorded in its own `UiState`, not from shared state.

use ainb::app::mouse::press;
use ainb::app::pointer;
use ainb::app::screens::ids as screen_ids;
use ainb::app::state::AppState;
use ainb::app::ui_state::UiState;
use ainb::app::{Btn, Pos};
use ainb::components::code_review::model::{ReviewFile, ReviewModel};
use ainb::components::code_review::render::{ReviewRowId, ReviewSidebarLayout};
use ainb::components::git_view::{GitFileStatus, GitTab, GitViewState};
use ratatui::layout::Rect;

fn file(path: &str) -> ReviewFile {
    ReviewFile {
        path: path.to_string(),
        status: GitFileStatus::Modified,
        insertions: 1,
        deletions: 0,
        language: None,
        collapsed: false,
        binary: false,
        hunks: Vec::new(),
        new_lines: Vec::new(),
    }
}

#[test]
fn a_press_on_a_review_sidebar_row_names_its_file() {
    let home = tempfile::tempdir().expect("scratch home");
    std::env::set_var("HOME", home.path());
    let mut state = AppState::new();
    state.shell.current_screen = screen_ids::GIT_VIEW.to_string();
    let mut git = GitViewState::new("/parity/api".into());
    git.active_tab = GitTab::Review;
    git.review = ReviewModel {
        files: vec![file("a.rs"), file("b.rs")],
    };
    state.git_view.git_view_state = Some(git);
    let mut ui = UiState::default();
    ui.review_sidebar = ReviewSidebarLayout {
        rect: Rect::new(2, 5, 30, 10),
        window: 0,
    };
    let before = state.versions();

    let intent = press(&state, &mut ui, Pos { x: 4, y: 6 }, Btn::Left);

    assert_eq!(
        intent,
        Some(pointer::select_review_row(&ReviewRowId::File(
            "b.rs".to_string()
        )))
    );
    assert_eq!(
        press(&state, &mut ui, Pos { x: 40, y: 6 }, Btn::Left),
        None,
        "outside the list"
    );
    assert_eq!(state.versions(), before, "a press reads state only");
}
