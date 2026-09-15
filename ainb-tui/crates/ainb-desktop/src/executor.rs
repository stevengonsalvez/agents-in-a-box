//! The desktop host's effect executor.
//!
//! Each `Effect` variant documents its desktop half; this runs the halves the
//! shell can reach in D1a and answers the rest with the failure report the
//! variant documents, so the reducer tells the operator instead of waiting on
//! work that never happens. Like the terminal host's executor it neither reads
//! nor writes state: the effect carries what it needs and the outcome comes
//! back as report intents.

use std::path::Path;
use std::sync::mpsc;

use ainb_app::Intent;
use ainb_app::app::reports::{
    self, AttachOutcome, AttachedTo, DaemonActionReport, EditorOutcome, ShellOutcome,
};
use ainb_app::app::{Effect, TerminalTarget, ToolTerminal};

use crate::host::Executor;

/// Why this shell answers an attach with a failure: terminal tabs are D1c.
const NO_TERMINAL_TABS: &str = "this desktop build has no terminal tabs yet";
/// Why plugin work is refused: this shell runs no plugin runtime.
const NO_PLUGIN_RUNTIME: &str = "this desktop build runs no plugin runtime";

/// Runs effects for the desktop host.
///
/// Work that outlives the call (a daemon lifecycle verb) reports later through
/// [`Self::take_deferred`], which the shell drains on its tick.
pub struct DesktopExecutor {
    /// The `ainb` binary daemon verbs run through, when one was found.
    ainb: Option<std::path::PathBuf>,
    deferred_tx: mpsc::Sender<Intent>,
    deferred_rx: mpsc::Receiver<Intent>,
}

impl DesktopExecutor {
    /// An executor that runs daemon verbs through `ainb`, or reports them as
    /// failed when `None`.
    #[must_use]
    pub fn new(ainb: Option<std::path::PathBuf>) -> Self {
        let (deferred_tx, deferred_rx) = mpsc::channel();
        Self {
            ainb,
            deferred_tx,
            deferred_rx,
        }
    }

    /// Reports from finished background work, oldest first.
    pub fn take_deferred(&mut self) -> Vec<Intent> {
        self.deferred_rx.try_iter().collect()
    }
}

impl Executor for DesktopExecutor {
    fn execute(&mut self, effect: Effect) -> Vec<Intent> {
        match effect {
            Effect::AttachTerminal(target) => vec![attach_unsupported(target)],
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

/// The failure report an attach documents, for a shell with no terminal tabs.
fn attach_unsupported(target: TerminalTarget) -> Intent {
    let failed = AttachOutcome::Failed(NO_TERMINAL_TABS.to_string());
    match target {
        TerminalTarget::InPlace { tmux_session, .. } => {
            reports::in_place_failed(tmux_session.as_str(), NO_TERMINAL_TABS, true)
        }
        TerminalTarget::Observe { tmux_session, .. } => {
            reports::observer_failed(tmux_session.as_str(), NO_TERMINAL_TABS, true)
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
        TerminalTarget::WorkspaceShell { workspace_path, .. } => reports::shell_prepared(
            &workspace_path,
            &ShellOutcome::Failed(NO_TERMINAL_TABS.to_string()),
        ),
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
