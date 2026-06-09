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
            code_review::render::render(frame, frame.size(), &state.review, &state.review_ui);
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
