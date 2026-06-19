// ABOUTME: `ainb diff-review [path]` — open the Warp-style Code Review surface for a
// repository's uncommitted changes directly, without a session. Reuses the exact
// render + interaction code from the in-session `G` view.

use std::io;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyModifiers, MouseButton,
    MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use crate::components::code_review;
use crate::components::git_view::GitViewState;

/// Lines scrolled per mouse-wheel tick.
const WHEEL_LINES: usize = 3;

/// Headless emit of the structured diff as JSON (no TUI). main.rs routes any
/// non-`text` `--format` here.
// ponytail: JSON only — the nested files->hunks->rows shape doesn't tabulate,
// so csv/markdown reuse the JSON. Add a `format` param + branch here if csv/
// markdown ever need a distinct rendering.
pub fn run_headless(path: PathBuf) -> Result<()> {
    let model = crate::components::code_review::parse::build_review_model(&path)?;
    let out = DiffJson::from_model(&path, &model);
    println!("{}", serde_json::to_string_pretty(&out)?);
    Ok(())
}

#[derive(serde::Serialize)]
struct DiffJson {
    path: String,
    total_insertions: usize,
    total_deletions: usize,
    files: Vec<DiffFileJson>,
}

#[derive(serde::Serialize)]
struct DiffFileJson {
    path: String,
    status: &'static str,
    insertions: usize,
    deletions: usize,
    binary: bool,
    language: Option<&'static str>,
    hunks: Vec<DiffHunkJson>,
}

#[derive(serde::Serialize)]
struct DiffHunkJson {
    old_start: usize,
    new_start: usize,
    rows: Vec<DiffRowJson>,
}

#[derive(serde::Serialize)]
struct DiffRowJson {
    kind: &'static str,
    old_lineno: Option<usize>,
    new_lineno: Option<usize>,
    text: String,
}

impl DiffJson {
    fn from_model(path: &std::path::Path, m: &crate::components::code_review::model::ReviewModel) -> Self {
        use crate::components::code_review::model::RowKind;
        Self {
            path: path.display().to_string(),
            total_insertions: m.total_insertions(),
            total_deletions: m.total_deletions(),
            files: m
                .files
                .iter()
                .map(|f| DiffFileJson {
                    path: f.path.clone(),
                    status: f.status.symbol(),
                    insertions: f.insertions,
                    deletions: f.deletions,
                    binary: f.binary,
                    language: f.language,
                    hunks: f
                        .hunks
                        .iter()
                        .map(|h| DiffHunkJson {
                            old_start: h.old_start,
                            new_start: h.new_start,
                            rows: h
                                .rows
                                .iter()
                                .map(|r| DiffRowJson {
                                    kind: match r.kind {
                                        RowKind::Context => "context",
                                        RowKind::Added => "added",
                                        RowKind::Removed => "removed",
                                    },
                                    old_lineno: r.old_lineno,
                                    new_lineno: r.new_lineno,
                                    text: r.raw.clone(),
                                })
                                .collect(),
                        })
                        .collect(),
                })
                .collect(),
        }
    }
}

/// Launch the interactive Code Review surface for `path`.
pub fn run(path: PathBuf) -> Result<()> {
    let mut state = GitViewState::new(path);
    state.refresh_review();

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = event_loop(&mut state, &mut terminal);

    // Always restore the terminal, even on error.
    let _ = disable_raw_mode();
    let _ = execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    );
    let _ = terminal.show_cursor();
    result
}

fn event_loop(
    state: &mut GitViewState,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
) -> Result<()> {
    loop {
        terminal.draw(|frame| {
            code_review::render::render(frame, frame.area(), &state.review, &state.review_ui);
        })?;

        if !event::poll(Duration::from_millis(250))? {
            continue;
        }
        match event::read()? {
            Event::Key(key) if key.kind == event::KeyEventKind::Press => {
                if handle_key(state, key.code, key.modifiers) {
                    break;
                }
            }
            Event::Mouse(m) => match m.kind {
                MouseEventKind::ScrollDown => state.review_scroll_down(WHEEL_LINES),
                MouseEventKind::ScrollUp => state.review_scroll_up(WHEEL_LINES),
                MouseEventKind::Down(MouseButton::Left) => {
                    state.review_sidebar_click(m.column, m.row);
                }
                _ => {}
            },
            _ => {}
        }
    }
    Ok(())
}

/// Returns `true` when the loop should exit.
fn handle_key(state: &mut GitViewState, code: KeyCode, mods: KeyModifiers) -> bool {
    match code {
        KeyCode::Char('q') | KeyCode::Esc => return true,
        KeyCode::Char('c') if mods.contains(KeyModifiers::CONTROL) => return true,
        KeyCode::Down => state.review_sidebar_down(),
        KeyCode::Up => state.review_sidebar_up(),
        KeyCode::Char('j') => state.review_scroll_down(1),
        KeyCode::Char('k') => state.review_scroll_up(1),
        KeyCode::Char('n') => state.review_next_hunk(),
        KeyCode::Char('N') => state.review_prev_hunk(),
        KeyCode::Char(']') => state.review_next_file(),
        KeyCode::Char('[') => state.review_prev_file(),
        KeyCode::Char(' ') | KeyCode::Enter => state.review_toggle_collapse(),
        KeyCode::Char('z') => state.review_expand_context(),
        KeyCode::Char('e') => state.review_expand_all_folders(),
        KeyCode::Char('E') => state.review_collapse_all_folders(),
        _ => {}
    }
    false
}
