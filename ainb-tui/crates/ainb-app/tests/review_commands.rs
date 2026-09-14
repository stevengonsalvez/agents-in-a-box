// ABOUTME: The code review screen's pointer commands name what was hit by path,
// so a click resolved against one frame acts on the same row after the tree
// changed, and the wheel scrolls through the reducer rather than the host.

use ainb_app::app::NoRenderer;
use ainb_app::app::pointer;
use ainb_app::app::screens::ids as screen_ids;
use ainb_app::components::code_review::model::{DiffRow, Hunk, ReviewFile, ReviewModel, RowKind};
use ainb_app::components::code_review::render::ReviewRowId;
use ainb_app::components::git_view::{GitFileStatus, GitTab, GitViewState};
use ainb_app::{AppState, Keymap, SectionId, dispatch};

fn bumped(before: &[u64], after: &[u64]) -> Vec<SectionId> {
    SectionId::ALL
        .into_iter()
        .filter(|id| before[id.index()] != after[id.index()])
        .collect()
}

fn file(path: &str, lines: usize) -> ReviewFile {
    ReviewFile {
        path: path.to_string(),
        status: GitFileStatus::Modified,
        insertions: lines,
        deletions: 0,
        language: None,
        collapsed: false,
        binary: false,
        hunks: vec![Hunk {
            old_start: 1,
            new_start: 1,
            gap_before: 0,
            gap_after: 0,
            expanded_before: 0,
            expanded_after: 0,
            rows: (1..=lines)
                .map(|n| DiffRow {
                    kind: RowKind::Added,
                    old_lineno: None,
                    new_lineno: Some(n),
                    raw: format!("line {n}"),
                    emphasis: Vec::new(),
                })
                .collect(),
        }],
        new_lines: Vec::new(),
    }
}

/// The git view on its Review tab over `paths`.
fn reviewing(paths: &[&str]) -> AppState {
    let home = tempfile::tempdir().expect("scratch home");
    std::env::set_var("HOME", home.path());
    std::mem::forget(home);
    let mut state = AppState::new();
    state.shell.current_screen = screen_ids::GIT_VIEW.to_string();
    let mut git = GitViewState::new("/parity/api".into());
    git.active_tab = GitTab::Review;
    git.review = ReviewModel {
        files: paths.iter().map(|path| file(path, 40)).collect(),
    };
    state.git_view.git_view_state = Some(git);
    state
}

fn selected(state: &AppState) -> usize {
    state
        .git_view
        .git_view_state
        .as_ref()
        .expect("git view")
        .review_ui
        .selected_file
}

#[test]
fn a_review_click_selects_the_file_it_named_after_the_tree_changed() {
    let keymap = Keymap::defaults();
    let mut state = reviewing(&["a.rs", "b.rs"]);
    let click = pointer::select_review_row(&ReviewRowId::File("b.rs".to_string()));

    // A refresh lands before the click: a file sorted ahead of b.rs appears.
    state.git_view.git_view_state.as_mut().expect("git view").review.files =
        vec![file("a.rs", 40), file("aa.rs", 40), file("b.rs", 40)];
    let _ = dispatch(&mut state, &keymap, &mut NoRenderer, click);

    let git = state.git_view.git_view_state.as_ref().expect("git view");
    assert_eq!(git.review.files[selected(&state)].path, "b.rs");
}

#[test]
fn a_review_click_on_a_file_that_is_gone_changes_nothing() {
    let keymap = Keymap::defaults();
    let mut state = reviewing(&["a.rs", "b.rs"]);
    let before = state.versions();

    let _ = dispatch(
        &mut state,
        &keymap,
        &mut NoRenderer,
        pointer::select_review_row(&ReviewRowId::File("gone.rs".to_string())),
    );

    assert!(bumped(&before, &state.versions()).is_empty());
}

#[test]
fn the_wheel_scrolls_the_review_through_the_reducer() {
    let keymap = Keymap::defaults();
    let mut state = reviewing(&["a.rs"]);
    let before = state.versions();

    let _ = dispatch(
        &mut state,
        &keymap,
        &mut NoRenderer,
        pointer::scroll_git_view(3),
    );
    let git = state.git_view.git_view_state.as_ref().expect("git view");
    assert_eq!(git.review_ui.scroll, 3);
    assert_eq!(bumped(&before, &state.versions()), vec![SectionId::GitView]);

    let _ = dispatch(
        &mut state,
        &keymap,
        &mut NoRenderer,
        pointer::scroll_git_view(-2),
    );
    assert_eq!(
        state.git_view.git_view_state.as_ref().expect("git view").review_ui.scroll,
        1
    );
}

#[test]
fn review_commands_do_nothing_off_the_git_view() {
    let keymap = Keymap::defaults();
    let mut state = reviewing(&["a.rs"]);
    state.shell.current_screen = screen_ids::SESSION_LIST.to_string();
    let before = state.versions();

    let _ = dispatch(
        &mut state,
        &keymap,
        &mut NoRenderer,
        pointer::scroll_git_view(3),
    );

    assert!(bumped(&before, &state.versions()).is_empty());
}
