#![allow(missing_docs)]

// ABOUTME: The `git_view` section carries a diff twice, as `diff_content` and
// again as the review rows, and a section past `MAX_FRAME_BYTES` is withheld
// WHOLE rather than trimmed, which would take the file tree and the commit box
// with it. So the frame carries a bounded projection, and says what it cut.

use std::path::PathBuf;

use ainb_app::AppState;
use ainb_app::SectionId;
use ainb_app::components::code_review::model::{DiffRow, Hunk, ReviewFile, ReviewModel, RowKind};
use ainb_app::components::git_view::{ChangedFile, GitFileStatus, GitViewState};
use ainb_app::wire::frame::{HostId, MAX_FRAME_BYTES};
use ainb_app::wire::section_json;

fn row(n: usize, text: &str) -> DiffRow {
    DiffRow {
        kind: RowKind::Added,
        old_lineno: None,
        new_lineno: Some(n),
        raw: text.to_string(),
        emphasis: Vec::new(),
    }
}

fn file(path: &str, rows: usize, text: &str) -> ReviewFile {
    ReviewFile {
        path: path.to_string(),
        status: GitFileStatus::Modified,
        insertions: rows,
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
            rows: (1..=rows).map(|n| row(n, text)).collect(),
        }],
        new_lines: Vec::new(),
    }
}

/// A state whose git view holds `files` files of `rows` rows each, every row
/// carrying `text`, plus a file tree and a commit list that must survive.
fn state_with(files: usize, rows: usize, text: &str) -> AppState {
    let mut state = AppState::new();
    let mut git = GitViewState::new(PathBuf::from("/repo"));
    git.changed_files = (0..files)
        .map(|i| ChangedFile {
            path: format!("src/file{i}.rs"),
            status: GitFileStatus::Modified,
            insertions: rows,
            deletions: 0,
        })
        .collect();
    git.diff_content = (0..files * rows).map(|_| text.to_string()).collect();
    git.review = ReviewModel {
        files: (0..files).map(|i| file(&format!("src/file{i}.rs"), rows, text)).collect(),
    };
    state.git_view.get_mut().git_view_state = Some(git);
    state
}

fn framed(state: &AppState) -> serde_json::Value {
    section_json(state, SectionId::GitView, &HostId::local())
}

#[test]
fn a_large_diff_frames_inside_the_cap_and_says_what_it_cut() {
    // Past the frame's own ceiling if nothing bounded it: 60 files of 250
    // rows, each row 300 characters, is about 9 MB of diff carried twice.
    let state = state_with(60, 250, &"x".repeat(300));

    let body = framed(&state);
    let bytes = serde_json::to_vec(&body).expect("encodes").len();

    assert!(
        bytes < MAX_FRAME_BYTES,
        "the section frames in {bytes} bytes, over the {MAX_FRAME_BYTES} ceiling, so it would be withheld whole"
    );
    let view = &body["git_view_state"];
    assert!(
        view["diff_lines_cut"].as_u64().expect("a cut count") > 0,
        "the frame says the diff was cut: {view}"
    );
    assert_eq!(
        view["changed_files"].as_array().expect("the file list").len(),
        60,
        "the file tree survives the cut"
    );
}

#[test]
fn every_file_keeps_its_place_and_says_how_many_rows_it_lost() {
    // Rows are cheap text here: what is under test is the budget, not size.
    let state = state_with(40, 500, "a changed line");

    let body = framed(&state);
    let files = body["git_view_state"]["review"]["files"].as_array().expect("files");

    assert_eq!(files.len(), 40, "every changed file is still listed");
    for file in files {
        let rows: usize = file["hunks"]
            .as_array()
            .expect("hunks")
            .iter()
            .map(|hunk| hunk["rows"].as_array().expect("rows").len())
            .sum();
        assert!(rows <= 400, "no file is over the per-file cap: {rows}");
        assert!(
            file["rows_cut"].as_u64().expect("a per-file cut count") > 0,
            "a file past the bound says how many rows it lost: {file}"
        );
    }
}

#[test]
fn a_credential_straddling_the_cut_is_scrubbed_whole() {
    // Assembled at runtime, so no credential-shaped literal is committed.
    let token = format!("npm_{}", "a1B2".repeat(9));
    let state = state_with(1, 1_200, &format!("export TOKEN={token}"));

    let body = framed(&state);
    let text = serde_json::to_string(&body).expect("encodes");

    assert!(
        !text.contains("npm_a1B2"),
        "no part of the token survives the projection"
    );
}

#[test]
fn a_small_diff_is_framed_whole_and_says_it_cut_nothing() {
    let state = state_with(2, 10, "a changed line");

    let body = framed(&state);
    let view = &body["git_view_state"];

    assert_eq!(view["diff_lines_cut"].as_u64(), Some(0));
    let files = view["review"]["files"].as_array().expect("files");
    for file in files {
        assert_eq!(
            file["rows_cut"].as_u64(),
            Some(0),
            "nothing was cut: {file}"
        );
        let rows = file["hunks"][0]["rows"].as_array().expect("rows").len();
        assert_eq!(rows, 10, "every row is framed");
    }
}
