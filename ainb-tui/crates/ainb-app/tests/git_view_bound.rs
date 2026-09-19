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

/// The same projection on budgets small enough to prove a property without
/// building megabytes of text first.
fn framed_within(state: &AppState, text: usize, lists: usize) -> serde_json::Value {
    let git = state.git_view.get().git_view_state.clone().expect("the git view");
    serde_json::to_value(ainb_app::wire::git_view::project_within(&git, text, lists))
        .expect("the frame encodes")
}

/// What the section encodes to, as the frame writer measures it.
fn encoded(body: &serde_json::Value) -> usize {
    serde_json::to_vec(body).expect("encodes").len()
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
    // The token sits past MAX_LINE_CHARS, so the character cut is what would
    // split it: scrub first and the whole row is already redacted, cut first
    // and the tail of the token frames as ordinary text.
    let straddling = format!("{}export TOKEN={token}", "x".repeat(1_990));
    let state = state_with(1, 4, &straddling);

    let body = framed(&state);
    let text = serde_json::to_string(&body).expect("encodes");

    assert!(
        !text.contains("npm_a1B2"),
        "no part of the token survives the projection"
    );
    assert!(
        !text.contains("a1B2a1B2"),
        "and no tail of it survives the character cut either"
    );
}

/// A private key's body is the lines BELOW its header, so a document scrubbed
/// one line at a time redacts the header and frames the key.
#[test]
fn a_key_block_in_markdown_is_scrubbed_past_its_header() {
    use ainb_app::components::git_view::{MarkdownLine, MarkdownStyle};

    let body_line = "MIIBOgIBAAJBAKj34GkxFhD90vcNLYLInFEX6Ppy1tPf9Cnzj4p4WGeK";
    let document = [
        "# Deploy notes",
        "-----BEGIN RSA PRIVATE KEY-----",
        body_line,
        "-----END RSA PRIVATE KEY-----",
    ];
    let mut state = state_with(1, 1, "a changed line");
    {
        let git = state.git_view.get_mut().git_view_state.as_mut().expect("the git view");
        git.markdown_content = document
            .iter()
            .map(|content| MarkdownLine {
                content: (*content).to_string(),
                style: MarkdownStyle::Paragraph,
            })
            .collect();
    }

    let text = serde_json::to_string(&framed(&state)).expect("encodes");

    assert!(
        !text.contains(body_line),
        "the key body is redacted with its header, not framed under it"
    );
}

/// A row keeps its word-level emphasis only while its text is the text those
/// byte offsets were measured against. The cut changes the text as surely as
/// the scrub does.
#[test]
fn a_row_the_cut_shortened_loses_its_emphasis_ranges() {
    let mut state = state_with(1, 1, "unused");
    {
        let git = state.git_view.get_mut().git_view_state.as_mut().expect("the git view");
        let rows = &mut git.review.files[0].hunks[0].rows;
        rows[0] = DiffRow {
            emphasis: vec![(0, 4), (2_400, 2_404)],
            ..row(1, &"y".repeat(4_000))
        };
    }

    let body = framed(&state);
    let framed_row = &body["git_view_state"]["review"]["files"][0]["hunks"][0]["rows"][0];

    assert!(
        framed_row["raw"].as_str().expect("the row text").chars().count() <= 2_001,
        "the row is cut to the character bound: {framed_row}"
    );
    assert_eq!(
        framed_row["emphasis"].as_array().expect("the ranges").len(),
        0,
        "a cut row carries no ranges into text that no longer has them: {framed_row}"
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

/// Every bound at once, on the content each is worst at: the caps multiply, so
/// a section that obeys each of them can still pass the ceiling and be withheld
/// whole, which is the thing this projection exists to prevent.
#[test]
fn the_worst_case_of_every_bound_together_still_frames() {
    use ainb_app::components::git_view::{MarkdownLine, MarkdownStyle};

    // A control character is six bytes once JSON escapes it, so this is the
    // cheapest text to write and the most expensive text to frame.
    let expensive = "\u{1}".repeat(2_000);
    let mut state = state_with(40, 400, &expensive);
    {
        let git = state.git_view.get_mut().git_view_state.as_mut().expect("the git view");
        git.diff_content = (0..2_000).map(|_| expensive.clone()).collect();
        git.markdown_content = (0..2_000)
            .map(|_| MarkdownLine {
                content: expensive.clone(),
                style: MarkdownStyle::Paragraph,
            })
            .collect();
        // A repository mid-rebase can have thousands of changed paths, and the
        // tree is one entry each.
        git.changed_files = (0..5_000)
            .map(|i| ChangedFile {
                path: format!("src/deep/path/to/file{i}.rs"),
                status: GitFileStatus::Modified,
                insertions: 1,
                deletions: 0,
            })
            .collect();
    }

    let body = framed(&state);
    let bytes = serde_json::to_vec(&body).expect("encodes").len();

    assert!(
        bytes < MAX_FRAME_BYTES,
        "the section frames in {bytes} bytes, over the {MAX_FRAME_BYTES} ceiling, so it would be withheld whole and take the file tree with it"
    );
    let view = &body["git_view_state"];
    assert!(view["diff_lines_cut"].as_u64().expect("a diff count") > 0);
    assert!(view["markdown_lines_cut"].as_u64().expect("a markdown count") > 0);
    assert!(
        !view["changed_files"].as_array().expect("the file list").is_empty(),
        "the tree is shortened, never emptied"
    );
}

/// The review's byte budget is spent on the file the person has open first, so
/// a huge diff in a file they are not looking at cannot leave their own file
/// with nothing in it.
///
/// Remove either the budget or the selected-file-first pass and this fails: the
/// files are framed in order, and the first of them spends everything.
#[test]
fn the_open_file_keeps_its_rows_when_the_others_spend_the_budget() {
    // A control character costs six bytes encoded, so 200 rows of them is
    // already more than the budget this asks for.
    let mut state = state_with(12, 200, &"\u{1}".repeat(400));
    {
        let git = state.git_view.get_mut().git_view_state.as_mut().expect("the git view");
        git.review_ui.selected_file = 11;
        git.diff_content = vec!["a changed line".to_string()];
    }

    let body = framed_within(&state, 64 * 1024, 16 * 1024);
    let files = body["review"]["files"].as_array().expect("files");
    let rows = |file: &serde_json::Value| -> usize {
        file["hunks"]
            .as_array()
            .map(|hunks| {
                hunks.iter().map(|hunk| hunk["rows"].as_array().expect("rows").len()).sum()
            })
            .unwrap_or_default()
    };

    let open = files
        .iter()
        .find(|file| file["path"] == "src/file11.rs")
        .unwrap_or_else(|| panic!("the open file is framed at all: {files:?}"));
    assert!(rows(open) > 0, "the open file frames rows: {open}");
    // What the budget cost the others shows one of two ways: a file framed
    // with no rows under it, or a file not framed at all and counted in
    // review.files_cut. Either is the frame saying so; neither is silence.
    let empty = files.iter().any(|file| file["path"] != "src/file11.rs" && rows(file) == 0);
    let dropped = body["review"]["files_cut"].as_u64().expect("a cut count") > 0;
    assert!(
        empty || dropped,
        "the budget the open file spent is accounted for: {} framed, files_cut {}",
        files.len(),
        body["review"]["files_cut"]
    );
    assert_eq!(
        body["review_ui"]["selected_file"].as_u64().expect("the selection"),
        files
            .iter()
            .position(|file| file["path"] == "src/file11.rs")
            .expect("the open file") as u64,
        "the selection points at the open file where it ended up"
    );
}

/// A file costs bytes before any of its rows do, and a repository mid-rebase
/// has thousands of them. An empty `ReviewFileFrame` is about 170 bytes, so
/// twenty thousand of them is over three MiB with not one row in the section.
#[test]
fn twenty_thousand_changed_files_do_not_pass_the_budget_on_their_headers() {
    let state = state_with(20_000, 1, "a changed line");

    let body = framed_within(&state, 64 * 1024, 16 * 1024);

    assert!(
        encoded(&body) < 256 * 1024,
        "the section frames in {} bytes on a 80 KiB budget",
        encoded(&body)
    );
    let files = body["review"]["files"].as_array().expect("files").len();
    assert!(files > 0, "the review is shortened, never emptied");
    assert_eq!(
        files as u64 + body["review"]["files_cut"].as_u64().expect("a cut count"),
        20_000,
        "and every file it dropped is counted"
    );
}

/// One file rewritten line by line has one hunk per line, and a hunk costs
/// bytes with no rows in it at all.
#[test]
fn fifty_thousand_hunks_in_one_file_do_not_pass_the_budget_on_their_headers() {
    let mut state = state_with(1, 1, "a changed line");
    {
        let git = state.git_view.get_mut().git_view_state.as_mut().expect("the git view");
        let hunk = git.review.files[0].hunks[0].clone();
        git.review.files[0].hunks = (0..50_000).map(|_| hunk.clone()).collect();
    }

    let body = framed_within(&state, 64 * 1024, 16 * 1024);

    assert!(
        encoded(&body) < 256 * 1024,
        "the section frames in {} bytes on a 80 KiB budget",
        encoded(&body)
    );
    let file = &body["review"]["files"][0];
    let hunks = file["hunks"].as_array().expect("hunks");
    assert!(!hunks.is_empty(), "the file keeps hunks");
    assert_eq!(
        hunks.len() as u64 + file["hunks_cut"].as_u64().expect("a cut count"),
        50_000,
        "and the hunks it dropped are counted"
    );

    // Every hunk here holds one row, so the rows kept are the hunks kept, to
    // the row: a hunk whose header could not be afforded keeps none of its
    // rows, and a row counted as framed for a hunk that was never pushed is a
    // row the counter would have lied about.
    let rows: usize = hunks.iter().map(|hunk| hunk["rows"].as_array().expect("rows").len()).sum();
    assert_eq!(
        rows,
        hunks.len(),
        "one row a hunk, as the fixture built them"
    );
    assert_eq!(
        file["rows_cut"].as_u64().expect("a row cut count"),
        50_000 - rows as u64,
        "the rows dropped with their hunks are counted as dropped"
    );
}

/// The file a person has open is framed wherever it sits, even past the list
/// cap: a frame that sent a thousand other files and named the last of them as
/// the open one would be pointing at a file nobody chose.
#[test]
fn the_open_file_is_framed_even_past_the_file_cap() {
    let mut state = state_with(1_200, 1, "a changed line");
    {
        let git = state.git_view.get_mut().git_view_state.as_mut().expect("the git view");
        git.review_ui.selected_file = 1_150;
    }

    let body = framed(&state);
    let review = &body["git_view_state"]["review"];
    let files = review["files"].as_array().expect("files");
    let selected = usize::try_from(
        body["git_view_state"]["review_ui"]["selected_file"]
            .as_u64()
            .expect("the selection"),
    )
    .expect("a selection that fits an index");

    assert_eq!(
        files[selected]["path"], "src/file1150.rs",
        "the frame names the file the person has open"
    );
    assert!(
        review["files_cut"].as_u64().expect("a cut count") > 0,
        "and it still says how many it left out"
    );
}

/// The sidebar's row count is `build_sidebar`'s, not the file count, so the
/// frame does not guess at it: a selection past the files crosses as it is.
#[test]
fn a_sidebar_selection_past_the_file_count_crosses_untouched() {
    let mut state = state_with(2, 4, "a changed line");
    {
        let git = state.git_view.get_mut().git_view_state.as_mut().expect("the git view");
        git.review_ui.sidebar_selected = 5;
    }

    let body = framed(&state);

    assert_eq!(
        body["git_view_state"]["review_ui"]["sidebar_selected"].as_u64(),
        Some(5),
        "a tree row is not a file row: clamping to the files moved a valid selection"
    );
}

/// The projection runs on the UI's thread every time the section's version
/// moves, so its cost is a bound like any other. Generous, because this is a
/// debug build on whatever CI gave us: what it catches is a scrub that went
/// back to costing milliseconds a line, which took this same state to 34.7
/// seconds in RELEASE before the shapes were fixed.
#[test]
fn the_worst_case_projects_in_a_bounded_time() {
    use ainb_app::components::git_view::{MarkdownLine, MarkdownStyle};

    let wide = "z".repeat(4_000);
    let mut state = state_with(3, 2_000, &wide);
    {
        let git = state.git_view.get_mut().git_view_state.as_mut().expect("the git view");
        git.diff_content = (0..2_000).map(|_| wide.clone()).collect();
        git.markdown_content = (0..2_000)
            .map(|_| MarkdownLine {
                content: wide.clone(),
                style: MarkdownStyle::Paragraph,
            })
            .collect();
    }
    let git = state.git_view.get().git_view_state.clone().expect("the git view");

    let start = std::time::Instant::now();
    let frame = ainb_app::wire::git_view::project(&git);
    let took = start.elapsed();

    assert!(
        !frame.review.files.is_empty(),
        "the worst case still frames something"
    );
    assert!(
        took < std::time::Duration::from_secs(25),
        "the worst case projected in {took:?}, which is a scrub charging by the character again"
    );
}

/// The same worst case in characters that are not one byte each: the cut counts
/// characters, the budget counts encoded bytes, and a cut inside a multi-byte
/// character is a panic.
#[test]
fn a_multi_byte_diff_frames_inside_the_cap_without_splitting_a_character() {
    let state = state_with(1, 600, &"🔐é".repeat(2_000));

    let body = framed(&state);
    let bytes = serde_json::to_vec(&body).expect("encodes").len();

    assert!(
        bytes < MAX_FRAME_BYTES,
        "the section frames in {bytes} bytes, over the {MAX_FRAME_BYTES} ceiling"
    );
    let view = &body["git_view_state"];
    let first = &view["review"]["files"][0]["hunks"][0]["rows"][0]["raw"];
    if let Some(text) = first.as_str() {
        assert!(
            text.chars().count() <= 2_001,
            "a framed row is cut on characters, not bytes"
        );
    }
    assert!(view["diff_lines_cut"].as_u64().expect("a diff count") > 0);
}

/// The lists the section carries are one entry per changed path, per tree row,
/// per commit, and none of them is bounded in the state.
#[test]
fn a_repository_of_thousands_of_paths_frames_a_shortened_tree_that_says_so() {
    let mut state = state_with(1, 1, "a changed line");
    {
        let git = state.git_view.get_mut().git_view_state.as_mut().expect("the git view");
        git.changed_files = (0..20_000)
            .map(|i| ChangedFile {
                path: format!("src/deep/path/to/file{i}.rs"),
                status: GitFileStatus::Modified,
                insertions: 1,
                deletions: 0,
            })
            .collect();
    }

    let body = framed(&state);
    let bytes = serde_json::to_vec(&body).expect("encodes").len();
    assert!(
        bytes < MAX_FRAME_BYTES,
        "the section frames in {bytes} bytes, over the {MAX_FRAME_BYTES} ceiling"
    );

    let view = &body["git_view_state"];
    let listed = view["changed_files"].as_array().expect("the file list").len();
    assert!(listed > 0, "the tree is shortened, never emptied");
    assert_eq!(
        listed as u64 + view["files_cut"].as_u64().expect("a cut count"),
        20_000,
        "and what it dropped is counted, not silently missing"
    );
}

/// What the projection costs in the worst case, in a build like the one that
/// ships. Not a gate, a number: the frame is built on the UI's thread whenever
/// the section's version moves, so the scrub behind it is a budget of time as
/// well as of bytes.
///
/// `cargo test -p ainb-app --features test-support --release --test
/// git_view_bound -- --ignored --nocapture`
#[test]
#[ignore = "a measurement, and only meaningful in release"]
fn what_the_worst_case_projection_costs() {
    use ainb_app::components::git_view::{MarkdownLine, MarkdownStyle};

    let wide = "z".repeat(4_000);
    let mut state = state_with(3, 2_000, &wide);
    {
        let git = state.git_view.get_mut().git_view_state.as_mut().expect("the git view");
        git.diff_content = (0..2_000).map(|_| wide.clone()).collect();
        git.markdown_content = (0..2_000)
            .map(|_| MarkdownLine {
                content: wide.clone(),
                style: MarkdownStyle::Paragraph,
            })
            .collect();
        git.changed_files = (0..20_000)
            .map(|i| ChangedFile {
                path: format!("src/deep/path/to/file{i}.rs"),
                status: GitFileStatus::Modified,
                insertions: 1,
                deletions: 0,
            })
            .collect();
    }
    let git = state.git_view.get().git_view_state.clone().expect("the git view");

    // Once to warm the caches the regexes build, then the measurement.
    let _ = ainb_app::wire::git_view::project(&git);
    let start = std::time::Instant::now();
    let frame = ainb_app::wire::git_view::project(&git);
    let projected = start.elapsed();
    let start = std::time::Instant::now();
    let bytes = serde_json::to_vec(&frame).expect("encodes").len();
    let encoded = start.elapsed();

    println!("worst case: project {projected:?}, encode {encoded:?}, {bytes} bytes");
}

/// The reducer counts rows over the whole model; the frame carries a cut of
/// it, so the frame translates the offset into its own rows rather than
/// sending a number that names different content on each side of the wire.
///
/// `flatten` (`components/code_review/render.rs:112`) counts a row per file
/// heading and a row per code line, so in a two-file state of ten rows each
/// the model's row 12 is the second file's first line.
#[test]
fn the_frames_scroll_is_its_own_row_and_says_when_the_reducers_row_was_cut() {
    let mut state = state_with(2, 10, "a changed line");
    {
        let git = state.git_view.get_mut().git_view_state.as_mut().expect("the git view");
        git.review_ui.scroll = 12;
    }

    let view = &framed(&state)["git_view_state"]["review_ui"];

    assert_eq!(
        view["scroll"].as_u64(),
        Some(12),
        "nothing was cut, so nothing moved"
    );
    assert_eq!(view["scroll_cut"].as_bool(), Some(false));
}

#[test]
fn a_row_past_the_per_file_cap_frames_as_the_last_row_that_survived_it() {
    // 500 rows a file, cut to MAX_ROWS_PER_FILE 400: the model's row 450 is
    // inside the first file and past what the frame carries of it.
    let mut state = state_with(2, 500, "a changed line");
    {
        let git = state.git_view.get_mut().git_view_state.as_mut().expect("the git view");
        git.review_ui.scroll = 450;
    }

    let view = &framed(&state)["git_view_state"]["review_ui"];

    // The first file frames its heading and 400 rows: rows 0 to 400.
    assert_eq!(
        view["scroll"].as_u64(),
        Some(400),
        "the last row of that file that was sent"
    );
    assert_eq!(
        view["scroll_cut"].as_bool(),
        Some(true),
        "and the frame says the row the terminal is on is not in it"
    );
}

#[test]
fn a_row_in_a_file_the_budget_dropped_frames_at_what_follows_it() {
    // Twelve files of 400 rows: the total row cap (4,000) stops the frame part
    // way, so a model row inside a file that was never framed has to land
    // somewhere honest.
    let mut state = state_with(12, 400, "a changed line");
    {
        let git = state.git_view.get_mut().git_view_state.as_mut().expect("the git view");
        // Inside the last file, which the row budget never reached.
        git.review_ui.scroll = 11 * 401 + 5;
    }

    let body = framed(&state);
    let view = &body["git_view_state"]["review_ui"];
    let framed_rows: u64 = body["git_view_state"]["review"]["files"]
        .as_array()
        .expect("files")
        .iter()
        .map(|file| {
            1 + file["hunks"]
                .as_array()
                .expect("hunks")
                .iter()
                .map(|hunk| hunk["rows"].as_array().expect("rows").len() as u64)
                .sum::<u64>()
        })
        .sum();

    assert_eq!(view["scroll_cut"].as_bool(), Some(true));
    assert!(
        view["scroll"].as_u64().expect("a scroll") <= framed_rows,
        "the offset is inside the rows the frame carries"
    );
}

#[test]
fn the_current_hunk_is_the_frames_hunk_too() {
    let mut state = state_with(3, 10, "a changed line");
    {
        let git = state.git_view.get_mut().git_view_state.as_mut().expect("the git view");
        git.review_ui.current_hunk = 2;
    }

    let body = framed(&state);
    let hunks: usize = body["git_view_state"]["review"]["files"]
        .as_array()
        .expect("files")
        .iter()
        .map(|file| file["hunks"].as_array().expect("hunks").len())
        .sum();

    let current = usize::try_from(
        body["git_view_state"]["review_ui"]["current_hunk"]
            .as_u64()
            .expect("a hunk cursor"),
    )
    .expect("a cursor that fits an index");
    assert!(
        current < hunks,
        "the cursor names a hunk the frame carries: {current} of {hunks}"
    );
}

/// The projection counts the model's rows the way `flatten` does, or the
/// offset it sends names the wrong line. The probe is an uncut state: put the
/// reducer on the last row the model has, and the frame must name that same
/// row and say nothing was cut.
#[test]
fn the_projection_counts_the_model_rows_flatten_counts() {
    use ainb_app::components::code_review::model::Hunk;
    use ainb_app::components::code_review::render::flatten;

    let mut state = state_with(2, 3, "a changed line");
    {
        let git = state.git_view.get_mut().git_view_state.as_mut().expect("the git view");
        // A hunk with hidden context above and below it, and one with no rows
        // at all, which is the shape the counts disagree over.
        git.review.files[0].hunks[0].gap_before = 6;
        git.review.files[0].hunks.push(Hunk {
            old_start: 40,
            new_start: 40,
            gap_before: 5,
            gap_after: 5,
            expanded_before: 0,
            expanded_after: 0,
            rows: Vec::new(),
        });
        git.review.files[1].hunks[0].gap_after = 4;

        let rows = flatten(&git.review).len();
        git.review_ui.scroll = rows - 1;
    }

    let view = &framed(&state)["git_view_state"]["review_ui"];

    assert_eq!(
        view["scroll_cut"].as_bool(),
        Some(false),
        "nothing was cut, so the last model row is a row the frame has"
    );
}

/// A collapsed file draws its heading and nothing else, so its hunks are not
/// on the screen to be counted: counting them would shift every later file's
/// hunk cursor by that many.
#[test]
fn a_collapsed_file_ahead_of_an_open_one_does_not_shift_the_hunk_cursor() {
    let mut state = state_with(2, 4, "a changed line");
    {
        let git = state.git_view.get_mut().git_view_state.as_mut().expect("the git view");
        git.review.files[0].collapsed = true;
        // The model's hunk 0 is the open file's only hunk: the collapsed file
        // ahead of it contributes none.
        git.review_ui.current_hunk = 0;
        git.review_ui.selected_file = 1;
    }

    let body = framed(&state);
    let view = &body["git_view_state"]["review_ui"];
    let files = body["git_view_state"]["review"]["files"].as_array().expect("files");

    assert_eq!(files[0]["collapsed"].as_bool(), Some(true));
    assert_eq!(
        view["current_hunk"].as_u64(),
        Some(0),
        "the cursor is the open file's hunk, not one counted inside the collapsed file"
    );
}
