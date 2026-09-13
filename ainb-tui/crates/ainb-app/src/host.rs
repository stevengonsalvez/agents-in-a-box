// ABOUTME: Services the host process lends the renderer-agnostic core. Code in
// `ainb-app` that must hand the controlling terminal to a child process (an
// interactive `docker exec`, the OAuth login flow) asks the host through here
// instead of driving a terminal library itself.

use std::io;
use std::sync::OnceLock;

/// Hands the controlling terminal to a child process and takes it back.
///
/// The terminal host implements it over its own input modes (raw mode, the
/// alternate screen, mouse capture, bracketed paste). A host without a
/// terminal installs nothing, and both calls do nothing.
pub trait TerminalHandoff: Send + Sync {
    /// Leave every input mode the host set up, so a child sees a plain tty.
    fn release(&self) -> io::Result<()>;
    /// Restore the input modes [`TerminalHandoff::release`] left.
    fn reclaim(&self) -> io::Result<()>;
}

static TERMINAL: OnceLock<Box<dyn TerminalHandoff>> = OnceLock::new();

/// Install the host's terminal handoff. Only the first call takes effect;
/// returns whether this one did.
pub fn set_terminal_handoff(handoff: Box<dyn TerminalHandoff>) -> bool {
    TERMINAL.set(handoff).is_ok()
}

/// Release the terminal to a child process, if the host has one.
pub fn release_terminal() -> io::Result<()> {
    TERMINAL.get().map_or(Ok(()), |handoff| handoff.release())
}

/// Take the terminal back from a child process, if the host has one.
pub fn reclaim_terminal() -> io::Result<()> {
    TERMINAL.get().map_or(Ok(()), |handoff| handoff.reclaim())
}
