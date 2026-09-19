#![allow(missing_docs)]

// ABOUTME: The code review screen's pointer commands name what was hit by path,
// so a click resolved against one frame acts on the same row after the tree
// changed, and the wheel scrolls through the reducer rather than the host.

#[path = "support/home.rs"]
mod home;

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
    home::shared();
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

/// A confirmation dialog covers the review: the wheel command aimed at the
/// diff beneath it changes nothing, and the dialog's own command still runs.
#[test]
fn a_confirmation_dialog_blocks_the_review_scroll_but_not_its_own_commands() {
    use ainb_app::app::state::{ConfirmAction, ConfirmationDialog};
    use ainb_app::{CommandId, Intent};

    let keymap = Keymap::defaults();
    let mut state = reviewing(&["a.rs"]);
    state.shell.confirmation_dialog = Some(ConfirmationDialog {
        title: "Stop session".to_string(),
        message: "Stop it?".to_string(),
        confirm_action: ConfirmAction::DismissNotifyPrompt,
        selected_option: false,
        warning: None,
        options: None,
        selected_index: 0,
    });
    let before = state.versions();

    let _ = dispatch(
        &mut state,
        &keymap,
        &mut NoRenderer,
        pointer::scroll_git_view(3),
    );
    assert!(bumped(&before, &state.versions()).is_empty());

    let _ = dispatch(
        &mut state,
        &keymap,
        &mut NoRenderer,
        Intent::Command(
            CommandId::new("confirm_dialog.cancel"),
            serde_json::Value::Null,
        ),
    );
    assert!(
        state.shell.confirmation_dialog.is_none(),
        "the dialog's command ran"
    );
    let _ = dispatch(
        &mut state,
        &keymap,
        &mut NoRenderer,
        pointer::scroll_git_view(3),
    );
    assert_eq!(
        state.git_view.git_view_state.as_ref().expect("git view").review_ui.scroll,
        3,
        "with the dialog gone the wheel scrolls again"
    );
}

/// The git view on its Commits tab over `count` commits.
fn on_commits(count: usize) -> AppState {
    let mut state = reviewing(&["a.rs"]);
    let git = state.git_view.git_view_state.as_mut().expect("git view");
    git.active_tab = GitTab::Commits;
    git.commits = (0..count)
        .map(|n| ainb_app::git::operations::CommitInfo {
            hash_short: format!("c{n:04}"),
            author: "Sample Dev".to_string(),
            date: "2026-09-19".to_string(),
            message: format!("commit {n}"),
        })
        .collect();
    state
}

fn scroll(state: &mut AppState, lines: i32) {
    let _ = dispatch(
        state,
        &Keymap::defaults(),
        &mut NoRenderer,
        pointer::scroll_git_view(lines),
    );
}

/// #1242: the wheel on the Commits tab moves through the commit list, bounded
/// by it, and the frame a webview draws from moves with it. Both the terminal's
/// wheel and the desktop's `git_view.scroll` arrive through this command; it
/// used to be accepted and do nothing on this tab.
#[test]
fn the_wheel_moves_through_the_commit_list() {
    let mut state = on_commits(10);
    let before = state.versions();

    scroll(&mut state, 3);
    let commit = |state: &AppState| {
        state.git_view.git_view_state.as_ref().expect("git view").selected_commit_index
    };
    assert_eq!(commit(&state), 3);
    assert_eq!(bumped(&before, &state.versions()), vec![SectionId::GitView]);
    let frame = ainb_app::wire::section_json(
        &state,
        SectionId::GitView,
        &ainb_app::wire::frame::HostId::local(),
    );
    assert_eq!(
        frame["git_view_state"]["selected_commit_index"], 3,
        "the frame moves too"
    );

    scroll(&mut state, -2);
    assert_eq!(commit(&state), 1);
    scroll(&mut state, 100);
    assert_eq!(commit(&state), 9, "bounded by the last commit");
    scroll(&mut state, -100);
    assert_eq!(commit(&state), 0, "bounded by the first");
}

/// An empty commit list has nothing to move through: the wheel leaves the
/// position at the start rather than past the end of an empty list.
#[test]
fn the_wheel_on_an_empty_commit_list_stays_at_the_start() {
    let mut state = on_commits(0);
    scroll(&mut state, 3);
    scroll(&mut state, -5);
    assert_eq!(
        state.git_view.git_view_state.as_ref().expect("git view").selected_commit_index,
        0
    );
}

fn select_commit(state: &mut AppState, sha: &str) {
    let _ = dispatch(
        state,
        &Keymap::defaults(),
        &mut NoRenderer,
        pointer::select_commit(sha),
    );
}

/// A click names the commit, and the reducer moves its selection to the commit
/// with that hash wherever it sits in the list.
#[test]
fn a_click_selects_the_commit_its_hash_names() {
    let mut state = on_commits(10);
    let before = state.versions();
    let commit = |state: &AppState| {
        state.git_view.git_view_state.as_ref().expect("git view").selected_commit_index
    };

    select_commit(&mut state, "c0007");
    assert_eq!(commit(&state), 7);
    assert_eq!(bumped(&before, &state.versions()), vec![SectionId::GitView]);
    let frame = ainb_app::wire::section_json(
        &state,
        SectionId::GitView,
        &ainb_app::wire::frame::HostId::local(),
    );
    assert_eq!(
        frame["git_view_state"]["selected_commit_index"], 7,
        "the frame carries the selection a click made"
    );

    select_commit(&mut state, "c0000");
    assert_eq!(commit(&state), 0, "and back up the list");
}

/// The hash is read against the list as it stands: a commit that is no longer
/// in it selects nothing rather than whatever now sits at some index.
#[test]
fn a_click_on_a_commit_that_is_gone_selects_nothing() {
    let mut state = on_commits(10);
    select_commit(&mut state, "c0004");

    select_commit(&mut state, "c9999");
    assert_eq!(
        state.git_view.git_view_state.as_ref().expect("git view").selected_commit_index,
        4,
        "the selection stays where the last live click put it"
    );

    select_commit(&mut state, "");
    assert_eq!(
        state.git_view.git_view_state.as_ref().expect("git view").selected_commit_index,
        4,
        "and an empty hash is refused before the reducer sees it"
    );
}

/// An empty commit list has nothing to select.
#[test]
fn a_click_on_an_empty_commit_list_stays_at_the_start() {
    let mut state = on_commits(0);
    select_commit(&mut state, "c0000");
    assert_eq!(
        state.git_view.git_view_state.as_ref().expect("git view").selected_commit_index,
        0
    );
}
