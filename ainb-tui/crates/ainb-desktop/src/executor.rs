//! The desktop host's effect executor.
//!
//! Each `Effect` variant documents its desktop half; this runs the halves the
//! shell can reach and answers the rest with the failure report the variant
//! documents, so the reducer tells the operator instead of waiting on
//! work that never happens. Like the terminal host's executor it neither reads
//! nor writes state: the effect carries what it needs and the outcome comes
//! back as report intents.

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::Duration;

use ainb_app::Intent;
use ainb_app::app::reports::{
    self, AttachOutcome, AttachedTo, DaemonActionReport, EditorOutcome, ShellOutcome,
};
use ainb_app::app::{Effect, TerminalTarget, ToolTerminal};

use crate::host::Executor;
use crate::terminal::{TabTarget, Terminals};

/// Why this shell answers an attach with a failure: its tabs attach ainb
/// sessions and named tmux sessions, and no other target yet.
const NO_TERMINAL_TABS: &str =
    "this desktop build opens terminal tabs only on sessions and tmux sessions";
/// Why this shell answers every attach with a failure: no tmux was found.
const NO_TMUX: &str = "no tmux was found, so this desktop build cannot open terminal tabs";
/// Why plugin work is refused: this shell runs no plugin runtime.
const NO_PLUGIN_RUNTIME: &str = "this desktop build runs no plugin runtime";

/// Runs effects for the desktop host.
///
/// Work that outlives the call (a daemon lifecycle verb) reports later through
/// [`Self::take_deferred`], which the shell drains on its tick.
pub struct DesktopExecutor {
    /// The `ainb` binary daemon verbs run through, when one was found.
    ainb: Option<std::path::PathBuf>,
    /// The terminal tabs attaches open, when the shell has them.
    terminals: Option<Terminals>,
    deferred_tx: mpsc::Sender<Intent>,
    deferred_rx: mpsc::Receiver<Intent>,
    /// The one worker that writes the session store. One thread, not one per
    /// write: the writes are a queue, and two at once would race for the same
    /// `sessions.json` lock.
    session_store_writer: Option<SessionStoreWriter>,
}

/// The session-store worker, as the executor holds it.
struct SessionStoreWriter {
    /// Where a queued write goes. Dropping it ends the worker's loop.
    work: mpsc::Sender<ainb_app::app::Persist>,
    /// The worker sends once here when its queue is empty and it is leaving,
    /// which is what a flush waits on: a `JoinHandle` has no bounded wait.
    done: mpsc::Receiver<()>,
    handle: thread::JoinHandle<()>,
    /// Writes queued and not yet written, for a timed-out flush to report.
    queued: Arc<AtomicUsize>,
}

impl DesktopExecutor {
    /// An executor that runs daemon verbs through `ainb`, or reports them as
    /// failed when `None`.
    #[must_use]
    pub fn new(ainb: Option<std::path::PathBuf>) -> Self {
        let (deferred_tx, deferred_rx) = mpsc::channel();
        Self {
            ainb,
            terminals: None,
            deferred_tx,
            deferred_rx,
            session_store_writer: None,
        }
    }

    /// Hand one session-store write to the worker, starting it on first use
    /// (P6e). A write can reach the hangar daemon and wait out its deadline,
    /// and the tick must not: this returns as soon as the write is queued,
    /// and a failure comes back through the deferred reports.
    fn queue_session_store_write(&mut self, store: ainb_app::app::Persist) -> Vec<Intent> {
        let store_id = store.store_id();
        if self.session_store_writer.is_none() {
            let (work_tx, work_rx) = mpsc::channel::<ainb_app::app::Persist>();
            let (done_tx, done_rx) = mpsc::channel::<()>();
            let reports_tx = self.deferred_tx.clone();
            let queued = Arc::new(AtomicUsize::new(0));
            let counted = Arc::clone(&queued);
            match thread::Builder::new().name("ainb-desktop-session-store-write".into()).spawn(
                move || {
                    // In order, one at a time, until the sender is dropped.
                    for store in work_rx {
                        if let Err(error) = ainb_app::config::persist::write(&store) {
                            let _ =
                                reports_tx.send(reports::persist_failed(store.store_id(), &error));
                        }
                        counted.fetch_sub(1, Ordering::SeqCst);
                    }
                    // The queue is empty and the worker is leaving: a flush
                    // that is waiting can stop waiting.
                    let _ = done_tx.send(());
                },
            ) {
                Ok(handle) => {
                    self.session_store_writer = Some(SessionStoreWriter {
                        work: work_tx,
                        done: done_rx,
                        handle,
                        queued,
                    });
                }
                Err(error) => {
                    return vec![reports::persist_failed(
                        store_id,
                        &format!("the worker did not start: {error}"),
                    )];
                }
            }
        }
        if let Some(writer) = self.session_store_writer.as_ref() {
            // Counted before the send, so the worker never sees a write it
            // cannot subtract.
            writer.queued.fetch_add(1, Ordering::SeqCst);
            if let Err(error) = writer.work.send(store) {
                writer.queued.fetch_sub(1, Ordering::SeqCst);
                return vec![reports::persist_failed(
                    store_id,
                    &format!("the session store worker is gone: {error}"),
                )];
            }
        }
        Vec::new()
    }

    /// Wait for every queued session-store write to land, up to `within` for
    /// all of them together, and answer with the number still unwritten.
    ///
    /// The desktop calls this on the paths that end the process: this shell
    /// never unwinds, so nothing here can be left to a destructor. A later
    /// write simply starts the worker again.
    pub fn flush_session_store_writes(&mut self, within: Duration) -> usize {
        let Some(writer) = self.session_store_writer.take() else {
            return 0;
        };
        let SessionStoreWriter {
            work,
            done,
            handle,
            queued,
        } = writer;
        // The worker's loop ends when the last sender goes, and only then does
        // it say it is done.
        drop(work);
        match done.recv_timeout(within) {
            Ok(()) => {
                let _ = handle.join();
                0
            }
            // Past the bound the writes that are left are left: the worker is
            // blocked on a lock or a daemon, and the exit does not wait on it.
            Err(_) => queued.load(Ordering::SeqCst),
        }
    }

    /// Open terminal tabs on `terminals` for the attaches they can hold.
    #[must_use]
    pub fn with_terminals(mut self, terminals: Terminals) -> Self {
        self.terminals = Some(terminals);
        self
    }

    /// Where background work (a terminal tab that closed) sends its reports,
    /// for [`Self::take_deferred`] to hand back.
    #[must_use]
    pub fn report_sender(&self) -> mpsc::Sender<Intent> {
        self.deferred_tx.clone()
    }

    /// Reports from finished background work, oldest first.
    pub fn take_deferred(&mut self) -> Vec<Intent> {
        self.deferred_rx.try_iter().collect()
    }
}

impl Drop for DesktopExecutor {
    /// Drain the queue for an executor that is dropped rather than exited
    /// from, which in the shipped app is no one: tao's run loop exits the
    /// process, so [`Self::flush_session_store_writes`] is called by hand on
    /// each path that ends it. This is the same drain for a host that is only
    /// built and dropped, a test one among them.
    fn drop(&mut self) {
        let dropped =
            self.flush_session_store_writes(ainb_app::cli::util::SESSION_STORE_FLUSH_BOUND);
        if dropped > 0 {
            tracing::warn!(dropped, "session-store writes were still queued at drop");
        }
    }
}

impl Executor for DesktopExecutor {
    fn execute(&mut self, effect: Effect) -> Vec<Intent> {
        match effect {
            Effect::AttachTerminal(target) => match (&self.terminals, tab_target(&target)) {
                (Some(terminals), Some(tab)) => terminals.open(tab).into_iter().collect(),
                (Some(_), None) => vec![attach_unsupported(target, NO_TERMINAL_TABS)],
                (None, _) => vec![attach_unsupported(target, NO_TMUX)],
            },
            // Desktop host: returns keyboard focus from the terminal tab to the
            // app, which the reducer hears as the user having left it.
            Effect::Detach => vec![reports::detached()],
            Effect::OpenEditor {
                path,
                preferred_editor,
            } => vec![open_editor(path.as_path(), preferred_editor.as_deref())],
            Effect::PasteClipboard => vec![paste_clipboard()],
            Effect::RunDaemonAction {
                daemon,
                action,
                generation,
            } => {
                let (daemon, verb) = (daemon.id(), action.id());
                let tx = self.deferred_tx.clone();
                let ainb = self.ainb.clone();
                let spawned = std::thread::Builder::new()
                    .name("ainb-desktop-daemon-action".into())
                    .spawn(move || {
                        let report = run_daemon_action(ainb.as_deref(), daemon, verb);
                        let _ =
                            tx.send(reports::daemon_action_finished(&report.sealed(generation)));
                    });
                match spawned {
                    Ok(_) => Vec::new(),
                    Err(error) => vec![reports::daemon_action_finished(
                        &failed_action(daemon, verb, format!("the worker did not start: {error}"))
                            .sealed(generation),
                    )],
                }
            }
            Effect::InboxMarkAllRead => {
                use ainb_hangar_proto::connections::SurfaceKind;
                let tx = self.deferred_tx.clone();
                let spawned = std::thread::Builder::new()
                    .name("ainb-desktop-inbox-mark-read".into())
                    .spawn(move || {
                        let outcome = ainb_app::fleet::inbox_write::mark_all_read_blocking(|| {
                            ainb_app::fleet::bridge::daemon::surface_client(SurfaceKind::Desktop)
                        });
                        let _ = tx.send(reports::inbox_mark_all_read_finished(&outcome));
                    });
                match spawned {
                    Ok(_) => Vec::new(),
                    Err(error) => {
                        let outcome = ainb_app::fleet::inbox_write::MarkAllReadOutcome {
                            op_id: String::new(),
                            ok: false,
                            marked: 0,
                            unread: 0,
                            error: Some(format!("the worker did not start: {error}")),
                            after: None,
                        };
                        vec![reports::inbox_mark_all_read_finished(&outcome)]
                    }
                }
            }
            // P6e: the session store is the one store a write can reach the
            // hangar daemon for, and a daemon that is not ready holds a write
            // for its bounded wait. That must not be on the tick: it goes to
            // a worker and its failure comes back as a deferred report. Every
            // other store is a local file write and stays here.
            Effect::Persist(store @ ainb_app::app::Persist::SessionHeadroom { .. }) => {
                self.queue_session_store_write(store)
            }
            Effect::Persist(store) => match ainb_app::config::persist::write(&store) {
                Ok(()) => Vec::new(),
                Err(error) => vec![reports::persist_failed(store.store_id(), &error)],
            },
            Effect::ForwardToPlugin {
                plugin,
                screen,
                back,
                ..
            } => {
                if back {
                    vec![reports::plugin_input_undelivered(&plugin, &screen)]
                } else {
                    tracing::debug!(%plugin, %screen, NO_PLUGIN_RUNTIME, "plugin input dropped");
                    Vec::new()
                }
            }
            Effect::RunPluginAction {
                plugin, action_id, ..
            } => vec![reports::plugin_action_undelivered(&plugin, &action_id)],
        }
    }
}

/// The tab an attach opens, for the targets a tab can hold.
fn tab_target(target: &TerminalTarget) -> Option<TabTarget> {
    match target {
        TerminalTarget::Session { id, tmux_session } => Some(TabTarget::Session {
            id: *id,
            tmux: tmux_session.as_str().to_string(),
        }),
        TerminalTarget::Tmux(name) => Some(TabTarget::Tmux {
            tmux: name.as_str().to_string(),
        }),
        _ => None,
    }
}

/// The failure report an attach documents, naming `why` no tab opened.
fn attach_unsupported(target: TerminalTarget, why: &str) -> Intent {
    let failed = AttachOutcome::Failed(why.to_string());
    match target {
        TerminalTarget::InPlace { tmux_session, .. } => {
            reports::in_place_failed(tmux_session.as_str(), why, true)
        }
        TerminalTarget::Observe { tmux_session, .. } => {
            reports::observer_failed(tmux_session.as_str(), why, true)
        }
        TerminalTarget::Session { id, .. } => {
            reports::attach_finished(&AttachedTo::Session(id), &failed)
        }
        TerminalTarget::Tmux(name) => {
            reports::attach_finished(&AttachedTo::Tmux(name.as_str().to_string()), &failed)
        }
        TerminalTarget::Tool(ToolTerminal::Witr) => {
            reports::attach_finished(&AttachedTo::Witr, &failed)
        }
        TerminalTarget::Tool(ToolTerminal::Abtop | ToolTerminal::AbtopWithSetup) => {
            reports::attach_finished(&AttachedTo::Abtop, &failed)
        }
        TerminalTarget::WorkspaceShell { workspace_path, .. } => {
            reports::shell_prepared(&workspace_path, &ShellOutcome::Failed(why.to_string()))
        }
        TerminalTarget::ClaudeLogin { auth_dir, .. } => reports::login_finished(&auth_dir, false),
    }
}

/// Open `path` in the configured editor, else `code`, else `$EDITOR`, the
/// resolution the terminal host uses, detached.
fn open_editor(path: &Path, preferred_editor: Option<&str>) -> Intent {
    let editor = preferred_editor
        .filter(|editor| ainb_app::editors::command_exists(editor))
        .map(str::to_string)
        .or_else(|| ainb_app::editors::command_exists("code").then(|| "code".to_string()))
        .or_else(|| {
            std::env::var("EDITOR")
                .ok()
                .filter(|editor| ainb_app::editors::command_exists(editor))
        });
    let Some(editor) = editor else {
        return reports::editor_finished(&EditorOutcome::NoneFound);
    };
    // `--` ends option parsing, so a path that starts with `-` is opened, not
    // read as an editor flag.
    let outcome = match std::process::Command::new(&editor).arg("--").arg(path).spawn() {
        Ok(_) => EditorOutcome::Opened(editor),
        Err(error) => EditorOutcome::Failed(error.to_string()),
    };
    reports::editor_finished(&outcome)
}

/// Read the platform clipboard's text and hand it to the focused field, the
/// route a bracketed paste takes.
fn paste_clipboard() -> Intent {
    match arboard::Clipboard::new().and_then(|mut clipboard| clipboard.get_text()) {
        Ok(text) => Intent::Text(text),
        Err(error) => reports::clipboard_failed(&error.to_string()),
    }
}

/// Run `ainb daemon <daemon> <verb>` and report how it went.
fn run_daemon_action(ainb: Option<&Path>, daemon: &str, verb: &str) -> DaemonActionReport {
    let Some(ainb) = ainb else {
        return failed_action(
            daemon,
            verb,
            format!("cmd: ainb daemon {daemon} {verb}\nno `ainb` binary was found for this shell"),
        );
    };
    match std::process::Command::new(ainb).args(["daemon", daemon, verb]).output() {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
            let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
            let ok = out.status.success();
            let last_line = |text: &str| {
                text.lines().rev().find(|line| !line.trim().is_empty()).map(str::to_string)
            };
            let summary = if ok {
                last_line(&stdout).unwrap_or_else(|| format!("{verb} ok"))
            } else {
                last_line(&stderr)
                    .or_else(|| last_line(&stdout))
                    .unwrap_or_else(|| format!("{verb} failed"))
            };
            DaemonActionReport {
                daemon: daemon.to_string(),
                verb: verb.to_string(),
                generation: 0,
                ok,
                summary,
                detail: format!("cmd: ainb daemon {daemon} {verb}\n{stdout}\n{stderr}")
                    .trim()
                    .to_string(),
                local: None,
            }
        }
        Err(error) => failed_action(
            daemon,
            verb,
            format!(
                "cmd: ainb daemon {daemon} {verb}\ncould not run {}: {error}",
                ainb.display()
            ),
        ),
    }
}

fn failed_action(daemon: &str, verb: &str, detail: String) -> DaemonActionReport {
    DaemonActionReport {
        daemon: daemon.to_string(),
        verb: verb.to_string(),
        generation: 0,
        ok: false,
        summary: format!("{verb} failed"),
        detail,
        local: None,
    }
}
