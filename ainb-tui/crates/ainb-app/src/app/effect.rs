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
///
/// A host never writes state. Whatever the work changed (a session detached,
/// a login wrote credentials, a tool was missing) comes back as a report
/// intent from [`crate::app::reports`], which the host dispatches like any
/// other; the reducer turns it into state and notices.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// Give the user a live terminal on `target`.
    ///
    /// Terminal host: suspends its own screen, attaches (or runs the target in
    /// tmux and attaches), and resumes when the user detaches. Desktop host:
    /// opens or focuses a terminal tab on the target. On failure (no tmux
    /// session, tool not installed, nested terminal) the host posts an error
    /// report naming the target and the outcome, and the reducer posts the notice.
    AttachTerminal(TerminalTarget),
    /// Leave the live terminal the user is in.
    ///
    /// Terminal host: releases the in-place interactive pane back to the
    /// read-only preview by reporting [`crate::app::reports::detached`].
    /// Desktop host: returns keyboard focus from the terminal tab to the app.
    /// With no live terminal this is a no-op, not an error.
    Detach,
    /// Open `path` in the user's preferred editor.
    ///
    /// Terminal host: runs the configured `preferred_editor`, else `code`,
    /// else `$EDITOR`, whichever is on `PATH` first, detached, and reports
    /// which with [`crate::app::reports::editor_finished`], or that none
    /// resolved or the editor failed to start. Desktop host: the same
    /// resolution, or the platform's default handler for the path.
    OpenEditor(PathBuf),
    /// Paste the clipboard's text into the field that has focus, for a paste
    /// key (Ctrl+V) the terminal did not deliver as a bracketed paste.
    ///
    /// Terminal host: reads the system clipboard and dispatches the text as
    /// [`crate::app::Intent::Text`], the route a bracketed paste takes. When
    /// the clipboard cannot be read (a headless host with no display server,
    /// or no text on it) it reports [`crate::app::reports::clipboard_failed`]
    /// instead. Desktop host: reads its platform clipboard, same dispatch.
    PasteClipboard,
    /// Run `ainb daemon <daemon> <action>`, a daemon lifecycle verb.
    ///
    /// Terminal host: runs the command off the UI thread and, once it exits,
    /// reports its exit status and output with
    /// [`crate::app::reports::daemon_action_finished`]. A command that cannot
    /// start is reported as a failure naming why. Desktop host: the same
    /// command against the host it drives, reported the same way.
    RunDaemonAction {
        daemon: crate::fleet::daemons::probe::DaemonKind,
        action: crate::cli::daemon::Action,
        /// Echoed in the report, so a report for an earlier request (one the
        /// row gave up on) is not taken for this one.
        generation: u64,
    },
}

/// What an [`Effect::AttachTerminal`] attaches to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalTarget {
    /// An ainb session's own tmux session.
    Session(Uuid),
    /// The selected row's tmux session, writable in the session list's own
    /// preview pane instead of full screen. Terminal host: measures the pane
    /// for its current layout and reports it with
    /// [`crate::app::reports::in_place_sized`]; the reducer attaches, and when
    /// the row has no tmux session or the attach fails, a notice says which.
    InPlace,
    /// A named tmux session ainb did not create: an "Other tmux" row, an SSH
    /// session's tmux, or a workspace shell that already exists.
    Tmux(String),
    /// A companion tool run in its own tmux session.
    Tool(ToolTerminal),
    /// The shell of the workspace at `workspace_path`: tmux session
    /// `tmux_session`, created on first use (`new_shell` when the reducer just
    /// added its record), optionally `cd`'d to `target_dir` before attaching.
    ///
    /// Terminal host: creates or reuses the tmux session and reports how with
    /// [`crate::app::reports::shell_prepared`], then attaches and reports the
    /// end with [`crate::app::reports::attach_finished`].
    WorkspaceShell {
        workspace_path: PathBuf,
        tmux_session: String,
        new_shell: bool,
        target_dir: Option<PathBuf>,
    },
    /// The Claude OAuth login, run interactively in `image` with `auth_dir`
    /// mounted as the container user's `~/.claude`.
    ///
    /// Terminal host: leaves its screen for a plain tty, runs the image's
    /// auth script, waits for Enter after it exits, restores its screen and
    /// reports how the child exited with [`crate::app::reports::login_finished`];
    /// the reducer decides success from the credentials the login wrote. When the
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
    pub const fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}
