//! The host and its executor as one lockable unit, for the window's commands
//! and its tick to share.
//!
//! Both run an intent's effects with the executor while holding the host, so
//! they live behind one `Mutex`: there is no second lock to take in a different
//! order, and a dispatch arriving during a tick waits for it rather than
//! deadlocking against it.

use std::sync::{Mutex, MutexGuard, PoisonError};

use ainb_app::Intent;
use ainb_app::wire::frame::{HostId, Subscription};

use crate::executor::DesktopExecutor;
use crate::host::{DesktopHost, Executor, FrameSink};
use crate::intent::Refusal;

struct Core<S: FrameSink> {
    host: DesktopHost<S>,
    executor: DesktopExecutor,
}

/// The shell's host and executor, locked together.
pub struct Shell<S: FrameSink> {
    core: Mutex<Core<S>>,
}

impl<S: FrameSink> Shell<S> {
    #[must_use]
    pub fn new(host: DesktopHost<S>, executor: DesktopExecutor) -> Self {
        Self {
            core: Mutex::new(Core { host, executor }),
        }
    }

    fn core(&self) -> MutexGuard<'_, Core<S>> {
        self.core.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Apply an intent from the renderer and run what it queued.
    pub fn dispatch(&self, intent: Intent) {
        let mut core = self.core();
        let Core { host, executor } = &mut *core;
        host.run(intent, executor);
    }

    /// Apply an intent the webview sent, unless
    /// [`DesktopHost::refused_from_renderer`] refuses what it would run; the
    /// refusal is returned for the webview to show. The check runs under the
    /// same lock that would apply the intent, so the state cannot move between
    /// the check and the dispatch.
    #[must_use = "a refusal the webview never hears about looks like a dead key"]
    pub fn dispatch_renderer(&self, intent: Intent) -> Option<Refusal> {
        let mut core = self.core();
        let Core { host, executor } = &mut *core;
        if let Some(refusal) = host.refused_from_renderer(&intent) {
            tracing::warn!(
                "`{}` refused from the webview: {}",
                refusal.command,
                refusal.reason
            );
            return Some(refusal);
        }
        host.run(intent, executor);
        None
    }

    /// Frame what moved since the last tick, and run the effects and deferred
    /// reports that work produced.
    pub fn tick(&self) {
        let mut core = self.core();
        let Core { host, executor } = &mut *core;
        let mut reports = Vec::new();
        for effect in host.tick() {
            reports.extend(executor.execute(effect));
        }
        reports.extend(executor.take_deferred());
        for report in reports {
            host.run(report, executor);
        }
    }

    /// Drain the executor's queued session-store writes; see
    /// [`DesktopExecutor::flush_session_store_writes`]. Answers with the
    /// number still unwritten when the bound ran out.
    ///
    /// The window calls this on the paths that end the process, which is the
    /// only place it can be called: this shell never unwinds.
    pub fn flush_session_store_writes(&self, within: std::time::Duration) -> usize {
        self.core().executor.flush_session_store_writes(within)
    }

    /// Every command the palette may offer; see [`DesktopHost::palette`].
    #[must_use]
    pub fn palette(&self) -> Vec<crate::host::PaletteEntry> {
        self.core().host.palette()
    }

    /// Put the reducer on the session list; see [`DesktopHost::open_sessions`].
    pub fn open_sessions(&self) {
        let mut core = self.core();
        let Core { host, executor } = &mut *core;
        host.open_sessions(executor);
    }

    /// Frame every section in `subscription`, and only those, for a renderer
    /// that just attached, and answer the host those frames name. One lock, so
    /// the answer is the host of the frames this call sent.
    pub fn subscribe(&self, subscription: Subscription) -> HostId {
        let mut core = self.core();
        core.host.subscribe(subscription);
        core.host.host_id().clone()
    }

    /// The host every frame names.
    pub fn host_id(&self) -> HostId {
        self.core().host.host_id().clone()
    }

    /// Re-pin the host every frame names; see [`DesktopHost::set_host`].
    pub fn set_host(&self, host_id: HostId) -> bool {
        self.core().host.set_host(host_id)
    }
}

/// One column per agent state: the most entries a board telemetry line
/// carries.
pub const BOARD_COLUMNS: usize = 5;

/// The board's columns as the `renderer applied` log line prints them,
/// `state=cards` each, and how many entries past [`BOARD_COLUMNS`] were
/// dropped.
///
/// Five states, so a longer list is a renderer that drew no board it could
/// name. The extra entries are not printed, and the count says they were
/// there: a proof reading the line would otherwise take that renderer for one
/// that drew five columns (#1194).
#[must_use]
pub fn board_columns(
    board: &[(ainb_hangar_proto::agent_status::AgentState, usize)],
) -> (Vec<String>, usize) {
    let columns = board
        .iter()
        .take(BOARD_COLUMNS)
        .map(|(state, cards)| format!("{}={cards}", state.as_str()))
        .collect();
    (columns, board.len().saturating_sub(BOARD_COLUMNS))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ainb_hangar_proto::agent_status::AgentState;

    #[test]
    fn a_board_line_holds_one_entry_per_state_and_counts_the_rest() {
        let five = [
            (AgentState::Working, 1),
            (AgentState::Waiting, 2),
            (AgentState::Idle, 0),
            (AgentState::Exited, 3),
            (AgentState::Unverifiable, 4),
        ];
        let (columns, dropped) = board_columns(&five);
        assert_eq!(
            columns,
            [
                "working=1",
                "waiting=2",
                "idle=0",
                "exited=3",
                "unverifiable=4"
            ]
        );
        assert_eq!(dropped, 0);

        let mut over = five.to_vec();
        over.extend([(AgentState::Waiting, 9), (AgentState::Idle, 9)]);
        let (columns, dropped) = board_columns(&over);
        assert_eq!(columns.len(), BOARD_COLUMNS, "the line stays bounded");
        assert_eq!(dropped, 2, "and the drop is counted, not silent");
    }
}
