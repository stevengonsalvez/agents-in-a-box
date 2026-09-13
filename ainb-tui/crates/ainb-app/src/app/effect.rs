// ABOUTME: The renderer contract's output side. The reducer never touches a
// terminal, an editor, the clipboard or a browser; it describes the work as an
// `Effect`, and whichever host drained the outbox carries it out.

use std::path::PathBuf;

/// Work the reducer asks its host to do.
///
/// Effects are queued while an intent or a tick is applied and handed back by
/// [`crate::app::dispatch`] and [`crate::app::App::tick`] once that step has
/// finished writing state, so a host always acts on committed state. Each
/// variant says which host executes it and what that host does when it cannot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// Open `path` in the user's preferred editor.
    ///
    /// Terminal host: runs the configured `preferred_editor`, else `code`,
    /// else `$EDITOR`, whichever is on `PATH` first, detached, and posts a
    /// success notice. When none resolves or the editor fails to start, it
    /// posts an error notice saying how to set one. Desktop host: the same
    /// resolution, or the platform's default handler for the path.
    OpenEditor(PathBuf),
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
