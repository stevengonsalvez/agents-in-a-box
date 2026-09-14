// ABOUTME: The renderer contract's output side. The reducer never touches a
// terminal, an editor, the clipboard or a browser; it describes the work as an
// `Effect`, and whichever host drained the outbox carries it out.

use std::path::PathBuf;

use uuid::Uuid;

/// Work the reducer asks its host to do.
///
/// Effects are queued while an intent or a tick is applied and handed back by
/// [`crate::app::dispatch`] and [`crate::app::App::tick`] once that step has
/// finished writing state, so a host always acts on committed state. Each
/// variant says which host executes it and what that host does when it cannot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// Give the user a live terminal on `target`.
    ///
    /// Terminal host: suspends its own screen, attaches (or runs the target in
    /// tmux and attaches), and resumes when the user detaches. Desktop host:
    /// opens or focuses a terminal tab on the target. On failure (no tmux
    /// session, tool not installed, nested terminal) the host posts an error
    /// notice naming the target and leaves the screen it was on.
    AttachTerminal(TerminalTarget),
    /// Leave the live terminal the user is in.
    ///
    /// Terminal host: releases the in-place interactive pane back to the
    /// read-only preview. Desktop host: returns keyboard focus from the
    /// terminal tab to the app. With no live terminal this is a no-op, not an
    /// error.
    Detach,
    /// Open `path` in the user's preferred editor.
    ///
    /// Terminal host: runs the configured `preferred_editor`, else `code`,
    /// else `$EDITOR`, whichever is on `PATH` first, detached, and posts a
    /// success notice. When none resolves or the editor fails to start, it
    /// posts an error notice saying how to set one. Desktop host: the same
    /// resolution, or the platform's default handler for the path.
    OpenEditor(PathBuf),
}

/// What an [`Effect::AttachTerminal`] attaches to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalTarget {
    /// An ainb session's own tmux session.
    Session(Uuid),
    /// The selected row's tmux session, writable in the session list's own
    /// preview pane instead of full screen. Terminal host: sizes the pane to
    /// its current layout and hands keyboard input to it; when the row has no
    /// tmux session or the attach fails, a notice says which.
    InPlace,
    /// A named tmux session ainb did not create: an "Other tmux" row, an SSH
    /// session's tmux, or a workspace shell that already exists.
    Tmux(String),
    /// A companion tool run in its own tmux session.
    Tool(ToolTerminal),
    /// The shell for a workspace, created on first use, optionally `cd`'d to
    /// `target_dir` before attaching.
    WorkspaceShell {
        workspace_index: usize,
        target_dir: Option<PathBuf>,
    },
    /// The Claude OAuth login, run interactively in `image` with `auth_dir`
    /// mounted as the container user's `~/.claude`.
    ///
    /// Terminal host: leaves its screen for a plain tty, runs the image's
    /// auth script, waits for Enter after it exits, restores its screen and
    /// reports how the child exited to [`crate::app::AppState::finish_oauth_login`],
    /// which decides success from the credentials the login wrote. When the
    /// child cannot start, the host reports it as a failed exit. Desktop host:
    /// the same command in a terminal window it opens, reported the same way.
    ClaudeLogin { auth_dir: PathBuf, image: String },
}

/// Companion tools the host runs in their own tmux session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolTerminal {
    /// `witr -i`, the process-causality browser.
    Witr,
    /// `abtop --exit-on-jump`, the agent monitor.
    Abtop,
    /// `abtop --setup` in a detached pane, then abtop itself.
    AbtopWithSetup,
}

/// Effects queued during one step, waiting for the host to drain them.
///
/// Deliberately not a section: queuing an effect is not a state change a
/// renderer draws, so it bumps no version.
#[derive(Debug, Default)]
pub struct EffectOutbox(Vec<Effect>);

impl EffectOutbox {
    pub fn push(&mut self, effect: Effect) {
        self.0.push(effect);
    }

    /// Everything queued so far, oldest first, leaving the outbox empty.
    #[must_use]
    pub fn take(&mut self) -> Vec<Effect> {
        std::mem::take(&mut self.0)
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}
