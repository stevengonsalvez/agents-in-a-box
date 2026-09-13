// ABOUTME: The TUI's terminal handoff: leaves and restores the input modes the
// binary sets up at startup (raw mode, alternate screen, mouse capture,
// bracketed paste) when `ainb-app` runs a child process that needs the tty.

use std::io;

use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};

/// [`ainb_app::host::TerminalHandoff`] over crossterm and stdout.
pub struct CrosstermHandoff;

impl ainb_app::host::TerminalHandoff for CrosstermHandoff {
    fn release(&self) -> io::Result<()> {
        disable_raw_mode()?;
        execute!(
            io::stdout(),
            LeaveAlternateScreen,
            DisableMouseCapture,
            DisableBracketedPaste
        )
    }

    fn reclaim(&self) -> io::Result<()> {
        enable_raw_mode()?;
        execute!(
            io::stdout(),
            EnterAlternateScreen,
            EnableMouseCapture,
            EnableBracketedPaste
        )
    }
}
