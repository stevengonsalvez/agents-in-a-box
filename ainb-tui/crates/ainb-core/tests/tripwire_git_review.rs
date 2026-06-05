//! Tripwire: the Warp-style Code Review surface renders the real git diff.
//!
//! Drives the end-to-end pipeline on a REAL git repository:
//!   build_review_model (git2 + similar)  ->  GitViewState  ->  code_review::render
//!
//! Unlike a tmux capture-pane test (which only sees text), this renders into a
//! ratatui `TestBackend` and inspects actual cell BACKGROUND colors, so it proves
//! the green/red row tints — not just that "something rendered". It also exercises
//! the real interaction methods (collapse, hunk jump) and re-renders to assert the
//! user-visible effect. Deterministic and non-flaky (no subprocess / no session
//! discovery needed). The live-terminal proof is the vhs journeys (see tmux-verify).

use std::path::Path;
use std::process::Command;

use ainb::components::code_review::highlight::{
    ADD_TINT, ADD_TINT_BRIGHT, DEL_TINT, DEL_TINT_BRIGHT,
};
use ainb::components::code_review::model::RowKind;
use ainb::components::code_review::{parse, render};
use ainb::components::git_view::{GitFileStatus, GitViewState};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::style::Color;

fn git(repo: &Path, args: &[&str]) {
    let status = Command::new("git").current_dir(repo).args(args).status().expect("run git");
    assert!(status.success(), "git {args:?} failed");
}

fn git_available() -> bool {
    Command::new("git")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Build a repo with: a modified file (one intra-line word change), an untracked
/// file, and a deleted file.
fn seed_repo(repo: &Path) {
    git(repo, &["init", "-q"]);
    git(repo, &["config", "user.email", "t@t.t"]);
    git(repo, &["config", "user.name", "t"]);

    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(
        repo.join("src/app.py"),
        "def main():\n    style.color = 'red'\n    return render()\n",
    )
    .unwrap();
    std::fs::write(repo.join("old.txt"), "to be deleted\n").unwrap();
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", "init"]);

    // Working-dir changes.
    std::fs::write(
        repo.join("src/app.py"),
        "def main():\n    style.color = 'green'\n    return render()\n",
    )
    .unwrap();
    std::fs::write(repo.join("notes.txt"), "a brand new file\n").unwrap();
    std::fs::remove_file(repo.join("old.txt")).unwrap();
}

fn render_to_text(state: &GitViewState) -> (String, Vec<Color>) {
    let mut term = Terminal::new(TestBackend::new(120, 30)).unwrap();
    term.draw(|f| render::render(f, f.size(), &state.review, &state.review_ui))
        .unwrap();
    let buf = term.backend().buffer();
    let mut text = String::new();
    let mut bgs = Vec::new();
    for y in 0..buf.area.height {
        for x in 0..buf.area.width {
            let cell = buf.get(x, y);
            text.push_str(cell.symbol());
            bgs.push(cell.bg);
        }
        text.push('\n');
    }
    (text, bgs)
}

#[test]
fn code_review_surface_renders_real_diff_and_responds_to_keys() {
    if !git_available() {
        eprintln!("SKIP: git not available");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    seed_repo(repo);

    // 1) The parser produces the right model from real git data.
    let model = parse::build_review_model(repo).expect("build model");
    let by_path = |p: &str| model.files.iter().find(|f| f.path == p);
    assert_eq!(
        model.files.len(),
        3,
        "files: {:?}",
        model.files.iter().map(|f| &f.path).collect::<Vec<_>>()
    );
    assert_eq!(
        by_path("src/app.py").unwrap().status,
        GitFileStatus::Modified
    );
    assert_eq!(
        by_path("notes.txt").unwrap().status,
        GitFileStatus::Untracked
    );
    assert_eq!(by_path("old.txt").unwrap().status, GitFileStatus::Deleted);

    // Word-level emphasis on the changed line ('red' -> 'green'), not the whole line.
    let app = by_path("src/app.py").unwrap();
    let added = app
        .hunks
        .iter()
        .flat_map(|h| &h.rows)
        .find(|r| r.kind == RowKind::Added)
        .expect("an added row");
    assert!(!added.emphasis.is_empty(), "expected intra-line emphasis");
    let emph_len: usize = added.emphasis.iter().map(|(s, e)| e - s).sum();
    assert!(
        emph_len < added.raw.len(),
        "emphasis must be a subset of the line"
    );

    // 2) The surface renders via the real GitViewState refresh + render path.
    let mut state = GitViewState::new(repo.to_path_buf());
    state.refresh_review();
    let (text, bgs) = render_to_text(&state);

    assert!(text.contains("Code Review"), "missing title:\n{text}");
    assert!(
        text.contains("src/app.py"),
        "missing modified path:\n{text}"
    );
    assert!(
        text.contains("notes.txt"),
        "missing untracked path:\n{text}"
    );
    assert!(text.contains('▌'), "missing change bar:\n{text}");
    assert!(text.contains("Hunk 1/"), "missing hunk counter:\n{text}");
    // The diff body shows the REAL changed code — both the removed and added lines.
    assert!(
        text.contains("'red'"),
        "removed line content missing:\n{text}"
    );
    assert!(
        text.contains("'green'"),
        "added line content missing:\n{text}"
    );
    // A code row carries both a gutter line number and the change bar.
    assert!(
        text.lines().any(|l| l.contains('▌') && l.chars().any(|c| c.is_ascii_digit())),
        "missing gutter line numbers next to a change bar:\n{text}"
    );

    // Real green AND red background tints on changed rows.
    let has_green = bgs.iter().any(|c| *c == ADD_TINT || *c == ADD_TINT_BRIGHT);
    let has_red = bgs.iter().any(|c| *c == DEL_TINT || *c == DEL_TINT_BRIGHT);
    assert!(has_green, "no added-row green tint rendered");
    assert!(has_red, "no removed-row red tint rendered");

    // 3) Collapse the selected file's diff block -> its code rows disappear.
    use ainb::components::code_review::render::{SidebarRow, build_sidebar};
    let app_idx = model.files.iter().position(|f| f.path == "src/app.py").unwrap();
    let tree = build_sidebar(&state.review, &state.review_ui.collapsed_dirs);
    state.review_ui.sidebar_selected = tree
        .iter()
        .position(|r| matches!(r, SidebarRow::File { file, .. } if *file == app_idx))
        .unwrap();
    state.review_toggle_collapse();
    let (collapsed_text, _) = render_to_text(&state);
    assert!(
        !collapsed_text.contains("style.color"),
        "collapsed file still shows code:\n{collapsed_text}"
    );
    // Expand again restores it.
    state.review_toggle_collapse();
    let (reopened_text, _) = render_to_text(&state);
    assert!(
        reopened_text.contains("style.color"),
        "re-expand did not restore code"
    );

    // 4) Hunk jump advances the counter.
    state.review_next_hunk();
    let (jumped_text, _) = render_to_text(&state);
    let total = state.review_hunk_counter().1;
    if total > 1 {
        assert!(
            jumped_text.contains("Hunk 2/"),
            "n did not advance the hunk counter:\n{jumped_text}"
        );
    }

    // 5) Tab traversal (in-session): Review is default; Tab cycles Review -> Commits
    //    -> Review (Markdown only appears when a .md file is selected). The legacy
    //    Files/Diff tabs are retired from the cycle.
    use ainb::components::git_view::GitTab;
    assert_eq!(state.active_tab, GitTab::Review);
    state.switch_tab();
    assert_eq!(state.active_tab, GitTab::Commits);
    state.switch_tab();
    assert_eq!(state.active_tab, GitTab::Review);
}
