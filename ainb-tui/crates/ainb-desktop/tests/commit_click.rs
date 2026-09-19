#![allow(missing_docs)]

// ABOUTME: A click on a commit row in the window reaches the reducer: the
// intent the webview sends is not refused at the seam, it names the commit by
// the hash the frame carries, and the next frame carries the selection it made.

use std::cell::RefCell;
use std::rc::Rc;

use ainb_app::app::screens::ids as screen_ids;
use ainb_app::components::git_view::{GitTab, GitViewState};
use ainb_app::git::operations::CommitInfo;
use ainb_app::wire::frame::{FrameBatch, HostId, Subscription};
use ainb_app::{AppState, CommandId, Intent, Keymap, SectionId};
use ainb_desktop::host::DesktopHost;

mod support;
use support::isolated_home;

type Frames = Rc<RefCell<Vec<serde_json::Value>>>;

/// What the window sends when a person clicks the row for `sha`.
fn click(sha: &str) -> Intent {
    Intent::Command(
        CommandId::new("git_view.select_commit"),
        serde_json::json!({ "sha": sha }),
    )
}

/// A state on the git view's Commits tab, carrying `count` commits.
fn on_commits(count: usize) -> AppState {
    let mut state = AppState::new();
    state.shell.current_screen = screen_ids::GIT_VIEW.to_string();
    let mut git = GitViewState::new(std::path::PathBuf::from("/work/repo"));
    git.active_tab = GitTab::Commits;
    git.commits = (0..count)
        .map(|n| CommitInfo {
            hash_short: format!("c{n:04}"),
            author: "Sample Dev".to_string(),
            date: "2026-09-19".to_string(),
            message: format!("commit {n}"),
        })
        .collect();
    state.git_view.get_mut().git_view_state = Some(git);
    state
}

fn host(state: AppState, frames: &Frames) -> DesktopHost<impl FnMut(FrameBatch)> {
    isolated_home();
    let frames = Rc::clone(frames);
    DesktopHost::hosting(
        state,
        Keymap::defaults(),
        HostId::local(),
        Subscription::only(&[SectionId::GitView]),
        move |batch: FrameBatch| {
            for frame in batch.frames {
                frames.borrow_mut().push(frame.body().clone());
            }
        },
    )
}

/// The seam lets it through, the reducer acts on it, and the window is told.
#[test]
fn a_click_on_a_commit_row_reaches_the_reducer_and_comes_back_in_a_frame() {
    let frames = Frames::default();
    let mut host = host(on_commits(6), &frames);

    assert_eq!(
        host.refused_from_renderer(&click("c0003")),
        None,
        "the window may send it: it writes nothing outside ainb"
    );

    frames.borrow_mut().clear();
    let _ = host.dispatch(click("c0003"));

    let selection = frames
        .borrow()
        .iter()
        .filter_map(|frame| frame["git_view_state"]["selected_commit_index"].as_u64())
        .next_back();
    assert_eq!(
        selection,
        Some(3),
        "the frame the window draws from carries the commit the click named"
    );
}

/// The hash is read against the list as it stands: a click naming a commit the
/// list no longer carries moves nothing, and sends no frame saying it did.
#[test]
fn a_click_on_a_commit_that_is_gone_moves_nothing() {
    let frames = Frames::default();
    let mut host = host(on_commits(6), &frames);
    let _ = host.dispatch(click("c0002"));

    frames.borrow_mut().clear();
    let _ = host.dispatch(click("c9999"));

    let moved = frames
        .borrow()
        .iter()
        .filter_map(|frame| frame["git_view_state"]["selected_commit_index"].as_u64())
        .any(|at| at != 2);
    assert!(!moved, "the selection stayed where the last live click put it");
}
