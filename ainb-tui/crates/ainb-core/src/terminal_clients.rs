// ABOUTME: The terminal host's live tmux client for the session list's preview
// pane. The reducer names the session the pane should show; this owns the PTY
// client for it, closes it when the reducer stops naming it, and reports what
// only the client can know (it opened, it ended, input stopped reaching it).

use std::sync::{Arc, RwLock};

use crate::app::Intent;
use crate::app::TmuxSessionName;
use crate::app::reports;
use crate::app::state::AppState;
use crate::tmux::EmbedClient;

/// At most one tmux client, read-only or writable, on the session
/// `AppState::embed_session_name` names.
#[derive(Default)]
pub struct TerminalClients {
    held: Option<Held>,
}

struct Held {
    session: String,
    client: EmbedClient,
    /// Opened in place, so input is the user's. A read-only observer mirrors
    /// the session and never types into it.
    writable: bool,
}

impl TerminalClients {
    /// Close the client when `state` no longer names its session: the reducer
    /// released the pane, declined a report, or moved to another row. Returns
    /// true when a client closed.
    pub fn reconcile(&mut self, state: &AppState) -> bool {
        let named = state.embed_session_name();
        if self.held.as_ref().is_some_and(|held| Some(held.session.as_str()) != named) {
            self.close();
            return true;
        }
        false
    }

    /// Open a writable client on `tmux_session` at `rows` by `cols`, replacing
    /// any other, and report how it went.
    pub fn open_in_place(
        &mut self,
        tmux_session: &TmuxSessionName,
        rows: u16,
        cols: u16,
    ) -> Intent {
        self.close();
        let name = tmux_session.as_str();
        match EmbedClient::attach(name, rows, cols) {
            Ok(client) => {
                self.held = Some(Held {
                    session: name.to_string(),
                    client,
                    writable: true,
                });
                reports::in_place_opened(name)
            }
            Err(error) => reports::in_place_failed(name, &error.to_string(), false),
        }
    }

    /// Open a read-only client on `tmux_session` at `rows` by `cols`, replacing
    /// any other, and report how it went.
    pub fn open_observer(
        &mut self,
        tmux_session: &TmuxSessionName,
        rows: u16,
        cols: u16,
    ) -> Intent {
        let name = tmux_session.as_str();
        if !EmbedClient::read_only_observer_supported() {
            return reports::observer_failed(
                name,
                "this tmux cannot keep a read-only client out of the window size",
                true,
            );
        }
        self.close();
        match EmbedClient::observe(name, rows, cols) {
            Ok(client) => {
                self.held = Some(Held {
                    session: name.to_string(),
                    client,
                    writable: false,
                });
                reports::observer_opened(name)
            }
            Err(error) => reports::observer_failed(name, &error.to_string(), false),
        }
    }

    /// The report for a client that ended on its own, closing it.
    pub fn take_exited(&mut self) -> Option<Intent> {
        if !self.held.as_ref().is_some_and(|held| held.client.has_exited()) {
            return None;
        }
        let held = self.held.take()?;
        Some(reports::terminal_exited(&held.session))
    }

    /// Send `bytes` to a writable client. A read-only observer takes none and
    /// reports nothing. When they cannot be written the client is closed and
    /// the report for that comes back.
    pub fn write_input(&mut self, bytes: &[u8]) -> Option<Intent> {
        let held = self.held.as_ref().filter(|held| held.writable)?;
        if held.client.write_input(bytes).is_ok() {
            return None;
        }
        let held = self.held.take()?;
        Some(reports::terminal_input_closed(&held.session))
    }

    /// Whether new output arrived since the last call. Clears the flag.
    pub fn take_dirty(&self) -> bool {
        self.held.as_ref().is_some_and(|held| held.client.take_dirty())
    }

    /// Size the client to `rows` by `cols`. Returns true once it has that size.
    pub fn resize(&mut self, rows: u16, cols: u16) -> bool {
        self.held.as_mut().is_some_and(|held| held.client.resize(rows, cols).is_ok())
    }

    /// The client's screen, for the preview pane to draw.
    pub fn screen(&self) -> Option<Arc<RwLock<vt100::Parser>>> {
        self.held.as_ref().map(|held| held.client.parser())
    }

    /// The session and cell size of the live client, if any.
    pub fn held(&self) -> Option<(&str, (u16, u16))> {
        self.held.as_ref().map(|held| (held.session.as_str(), held.client.size()))
    }

    fn close(&mut self) {
        if let Some(mut held) = self.held.take() {
            held.client.shutdown();
        }
    }
}
