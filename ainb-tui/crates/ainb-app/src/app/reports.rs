// ABOUTME: Commands a host dispatches to report something it did or measured
// (its screen width at startup, how a terminal it ran ended), so the reducer,
// not the host, decides what state changes. Unbound keymap rows, like the
// pointer commands.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::app::events::AppEvent;
use crate::app::intent::{Args, Intent};
use crate::app::keymap::CommandId;

/// Command ids of the report rows.
pub mod ids {
    /// `{"columns": u16}`
    pub const MIGRATE_LAYOUT_WIDTHS: &str = "global.migrate_layout_widths";
    /// `{"target": AttachedTo, "outcome": AttachOutcome}`
    pub const ATTACH_FINISHED: &str = "global.attach_finished";
    /// `{"workspace": path, "outcome": ShellOutcome}`
    pub const SHELL_PREPARED: &str = "global.shell_prepared";
    /// `{"ok": bool}`
    pub const ABTOP_SETUP_FINISHED: &str = "global.abtop_setup_finished";
    /// `{"tmux_session": String, "embed": LocalEmbed}`
    pub const IN_PLACE_OPENED: &str = "global.in_place_opened";
    /// `{"tmux_session": String, "error": String}`
    pub const IN_PLACE_FAILED: &str = "global.in_place_failed";
    /// `{"plugin": String, "action_id": String}`
    pub const PLUGIN_ACTION_UNDELIVERED: &str = "global.plugin_action_undelivered";
    /// No arguments.
    pub const DETACHED: &str = "global.detached";
    /// `{"outcome": EditorOutcome}`
    pub const EDITOR_FINISHED: &str = "global.editor_finished";
    /// `{"error": String}`
    pub const CLIPBOARD_FAILED: &str = "global.clipboard_failed";
    /// `{"auth_dir": path, "exited_ok": bool}`
    pub const LOGIN_FINISHED: &str = "global.login_finished";
    /// `{"report": DaemonActionReport}`
    pub const DAEMON_ACTION_FINISHED: &str = "global.daemon_action_finished";

    /// Every report command id.
    pub const ALL: &[&str] = &[
        MIGRATE_LAYOUT_WIDTHS,
        ATTACH_FINISHED,
        SHELL_PREPARED,
        ABTOP_SETUP_FINISHED,
        IN_PLACE_OPENED,
        IN_PLACE_FAILED,
        PLUGIN_ACTION_UNDELIVERED,
        DETACHED,
        EDITOR_FINISHED,
        CLIPBOARD_FAILED,
        LOGIN_FINISHED,
        DAEMON_ACTION_FINISHED,
    ];
}

/// What a full-screen terminal attach was attached to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttachedTo {
    /// An ainb session's tmux session.
    Session(Uuid),
    /// A tmux session ainb did not create, by name.
    Tmux(String),
    /// The witr browser.
    Witr,
    /// The abtop monitor.
    Abtop,
    /// A workspace's shell, by the workspace's path.
    WorkspaceShell(PathBuf),
}

/// How a full-screen terminal attach ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttachOutcome {
    /// Attached, and the user came back.
    Detached,
    /// The attach failed with this error while its target may still be alive.
    Failed(String),
    /// The attach failed and the tmux target is gone.
    TargetMissing(String),
    /// The tool's tmux session would not start; it is most likely not installed.
    NotInstalled,
}

/// How preparing a workspace shell's tmux session went.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShellOutcome {
    /// The tmux session exists; `created` when this attach made it, and
    /// `cd` how moving it to the target directory went.
    Ready { created: bool, cd: ShellCd },
    /// The tmux session could not be created or reached.
    Failed(String),
}

/// How changing a workspace shell's directory went.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShellCd {
    /// No directory was asked for.
    Stayed,
    /// The shell moved to this directory.
    Moved(PathBuf),
    /// tmux took the command but reported a problem.
    MaybeFailed(PathBuf),
    /// The command could not be sent.
    Failed(String),
}

/// How opening an editor went.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EditorOutcome {
    /// This editor started.
    Opened(String),
    /// No editor in the preference chain is installed.
    NoneFound,
    /// The editor would not start.
    Failed(String),
}

/// What `ainb daemon <daemon> <verb>` reported when it exited.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DaemonActionReport {
    /// The daemon's stable id, as `ainb daemon` spells it.
    pub daemon: String,
    /// The verb, as `ainb daemon` spells it.
    pub verb: String,
    /// The generation of the request this answers, as the effect carried it.
    #[serde(default)]
    pub generation: u64,
    /// Whether the command exited zero.
    pub ok: bool,
    /// One line for the daemon's row.
    pub summary: String,
    /// Everything the command said: argv, exit status and output.
    pub detail: String,
    /// The real summary and detail when they carry a credential (a pairing
    /// code): kept in this process and only a handle serialised, while
    /// `summary` and `detail` say so without it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local: Option<LocalOutput>,
}

impl DaemonActionReport {
    /// This report answering the request of `generation`, ready to leave the
    /// executor. A `pair` report's output is the Codex pairing code, a
    /// short-lived credential: it moves into [`LocalOutput`] and the
    /// serialised `summary` and `detail` say only that a code was minted.
    #[must_use]
    pub fn sealed(mut self, generation: u64) -> Self {
        self.generation = generation;
        if self.verb == crate::cli::daemon::Action::Pair.id() {
            let summary = if self.ok {
                "pairing code ready on this machine's Daemons screen".to_string()
            } else {
                "pair failed".to_string()
            };
            let detail = format!(
                "`ainb daemon {} pair` output is kept on the machine that ran it",
                self.daemon
            );
            let kept = LocalOutput::keep(
                std::mem::replace(&mut self.summary, summary),
                std::mem::replace(&mut self.detail, detail),
            );
            self.local = Some(kept);
        }
        self
    }
}

/// Command output that never leaves the process that ran the command.
///
/// Serialised as an opaque random handle. The reducer of the same process
/// redeems it once for the text; any other process, or a second redeem, gets
/// nothing and shows the redacted fields instead.
///
/// ponytail: a report that is never dispatched leaves its entry behind; one
/// per pairing attempt, so no eviction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct LocalOutput(String);

type LocalOutputs = std::sync::Mutex<std::collections::HashMap<String, (String, String)>>;

fn local_outputs() -> &'static LocalOutputs {
    static OUTPUTS: std::sync::OnceLock<LocalOutputs> = std::sync::OnceLock::new();
    OUTPUTS.get_or_init(LocalOutputs::default)
}

impl LocalOutput {
    /// Keep `summary` and `detail` in this process behind a new handle.
    #[must_use]
    pub fn keep(summary: String, detail: String) -> Self {
        let handle = Uuid::new_v4().to_string();
        if let Ok(mut outputs) = local_outputs().lock() {
            outputs.insert(handle.clone(), (summary, detail));
        }
        Self(handle)
    }

    /// The kept summary and detail, once, in the process that kept them.
    #[must_use]
    pub fn redeem(&self) -> Option<(String, String)> {
        local_outputs().lock().ok()?.remove(&self.0)
    }
}

fn command(id: &str, args: Args) -> Intent {
    Intent::Command(CommandId::new(id), args)
}

/// Report the host's screen width so layout widths saved as column counts
/// become fractions of it.
#[must_use]
pub fn migrate_layout_widths(columns: u16) -> Intent {
    command(ids::MIGRATE_LAYOUT_WIDTHS, json!({ "columns": columns }))
}

/// Report how a full-screen attach to `target` ended.
#[must_use]
pub fn attach_finished(target: &AttachedTo, outcome: &AttachOutcome) -> Intent {
    command(
        ids::ATTACH_FINISHED,
        json!({ "target": target, "outcome": outcome }),
    )
}

/// Report how preparing the shell of the workspace at `workspace` went.
#[must_use]
pub fn shell_prepared(workspace: &Path, outcome: &ShellOutcome) -> Intent {
    command(
        ids::SHELL_PREPARED,
        json!({ "workspace": workspace, "outcome": outcome }),
    )
}

/// Report whether `abtop --setup` started.
#[must_use]
pub fn abtop_setup_finished(ok: bool) -> Intent {
    command(ids::ABTOP_SETUP_FINISHED, json!({ "ok": ok }))
}

/// Report that the host opened a tmux client on `tmux_session` for the
/// in-place pane, parked as `embed` for the reducer to adopt.
#[must_use]
pub fn in_place_opened(tmux_session: &str, embed: &LocalEmbed) -> Intent {
    command(
        ids::IN_PLACE_OPENED,
        json!({ "tmux_session": tmux_session, "embed": embed }),
    )
}

/// Report that the in-place client on `tmux_session` would not open.
#[must_use]
pub fn in_place_failed(tmux_session: &str, error: &str) -> Intent {
    command(
        ids::IN_PLACE_FAILED,
        json!({ "tmux_session": tmux_session, "error": error }),
    )
}

/// Report that the host's plugin runtime has no running `plugin` to take
/// `action_id`.
#[must_use]
pub fn plugin_action_undelivered(plugin: &str, action_id: &str) -> Intent {
    command(
        ids::PLUGIN_ACTION_UNDELIVERED,
        json!({ "plugin": plugin, "action_id": action_id }),
    )
}

/// A tmux client the host opened for the in-place pane, parked in this
/// process until the reducer adopts it.
///
/// Serialised as an opaque random handle, like [`LocalOutput`]: the client is
/// a PTY this process owns, so only this process's reducer can adopt it, and
/// only once. A report that is never dispatched leaves its client parked
/// until the process ends.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct LocalEmbed(String);

type LocalEmbeds = std::sync::Mutex<std::collections::HashMap<String, crate::tmux::EmbedClient>>;

fn local_embeds() -> &'static LocalEmbeds {
    static EMBEDS: std::sync::OnceLock<LocalEmbeds> = std::sync::OnceLock::new();
    EMBEDS.get_or_init(LocalEmbeds::default)
}

impl LocalEmbed {
    /// A handle nothing is parked behind, for the keymap row's placeholder
    /// event.
    #[must_use]
    pub const fn placeholder() -> Self {
        Self(String::new())
    }

    /// Park `client` behind a new handle.
    #[must_use]
    pub fn keep(client: crate::tmux::EmbedClient) -> Self {
        let handle = Uuid::new_v4().to_string();
        local_embeds()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(handle.clone(), client);
        Self(handle)
    }

    /// The parked client, once, in the process that parked it.
    #[must_use]
    pub fn adopt(&self) -> Option<crate::tmux::EmbedClient> {
        local_embeds()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.0)
    }
}

/// Report that the user left the live terminal.
#[must_use]
pub fn detached() -> Intent {
    command(ids::DETACHED, Value::Null)
}

/// Report how opening an editor went.
#[must_use]
pub fn editor_finished(outcome: &EditorOutcome) -> Intent {
    command(ids::EDITOR_FINISHED, json!({ "outcome": outcome }))
}

/// Report that the clipboard could not be read.
#[must_use]
pub fn clipboard_failed(error: &str) -> Intent {
    command(ids::CLIPBOARD_FAILED, json!({ "error": error }))
}

/// Report how the interactive OAuth login ended.
#[must_use]
pub fn login_finished(auth_dir: &Path, exited_ok: bool) -> Intent {
    command(
        ids::LOGIN_FINISHED,
        json!({ "auth_dir": auth_dir, "exited_ok": exited_ok }),
    )
}

/// Report how a daemon lifecycle command ended.
#[must_use]
pub fn daemon_action_finished(report: &DaemonActionReport) -> Intent {
    command(ids::DAEMON_ACTION_FINISHED, json!({ "report": report }))
}

/// Whether an OAuth login that exited `exited_ok` left credentials in
/// `auth_dir`: the one test of success, shared by the reducer and a host that
/// wants to tell the user before it restores its screen.
#[must_use]
pub fn oauth_credentials_written(auth_dir: &Path, exited_ok: bool) -> bool {
    exited_ok
        && std::fs::metadata(auth_dir.join(".credentials.json"))
            .is_ok_and(|metadata| metadata.len() > 0)
}

/// A tmux attach failure, naming the target and the two causes that produce it.
///
/// tmux prints its real reason to the terminal the TUI is about to repaint
/// over, so an exit code is all that survives the round trip. What the caller
/// knows is the target and the two things that actually produce a bare exit 1
/// here.
#[must_use]
pub fn attach_failure_notice(session_name: &str, error: &str) -> String {
    format!(
        "Failed to attach to '{session_name}': {error}. Either the session ended after \
         the list was drawn (press f to refresh), or it is the tmux session ainb is \
         itself running in, which tmux refuses to nest."
    )
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ColumnsArgs {
    columns: u16,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AttachArgs {
    target: AttachedTo,
    outcome: AttachOutcome,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ShellArgs {
    workspace: PathBuf,
    outcome: ShellOutcome,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OkArgs {
    ok: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OpenedArgs {
    tmux_session: String,
    embed: LocalEmbed,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InPlaceFailedArgs {
    tmux_session: String,
    error: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UndeliveredArgs {
    plugin: String,
    action_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EditorArgs {
    outcome: EditorOutcome,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ErrorArgs {
    error: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LoginArgs {
    auth_dir: PathBuf,
    exited_ok: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DaemonArgs {
    report: DaemonActionReport,
}

fn parse<T: for<'de> Deserialize<'de>>(args: &Args) -> Option<T> {
    serde_json::from_value(args.clone()).ok()
}

/// The event a report row runs with `args` as its payload, with the same
/// contract as [`crate::app::pointer::with_args`].
pub(crate) fn with_args(event: &AppEvent, args: &Args) -> Option<Option<AppEvent>> {
    Some(match event {
        AppEvent::MigrateLayoutWidths { .. } => {
            parse::<ColumnsArgs>(args).map(|args| AppEvent::MigrateLayoutWidths {
                columns: args.columns,
            })
        }
        AppEvent::AttachFinished { .. } => {
            parse::<AttachArgs>(args).map(|args| AppEvent::AttachFinished {
                target: args.target,
                outcome: args.outcome,
            })
        }
        AppEvent::ShellPrepared { .. } => {
            parse::<ShellArgs>(args).map(|args| AppEvent::ShellPrepared {
                workspace: args.workspace,
                outcome: args.outcome,
            })
        }
        AppEvent::AbtopSetupFinished { .. } => {
            parse::<OkArgs>(args).map(|args| AppEvent::AbtopSetupFinished { ok: args.ok })
        }
        AppEvent::InPlaceOpened { .. } => {
            parse::<OpenedArgs>(args).map(|args| AppEvent::InPlaceOpened {
                tmux_session: args.tmux_session,
                embed: args.embed,
            })
        }
        AppEvent::InPlaceFailed { .. } => {
            parse::<InPlaceFailedArgs>(args).map(|args| AppEvent::InPlaceFailed {
                tmux_session: args.tmux_session,
                error: args.error,
            })
        }
        AppEvent::PluginActionUndelivered { .. } => {
            parse::<UndeliveredArgs>(args).map(|args| AppEvent::PluginActionUndelivered {
                plugin: args.plugin,
                action_id: args.action_id,
            })
        }
        AppEvent::Detached => args.is_null().then_some(AppEvent::Detached),
        AppEvent::EditorFinished { .. } => {
            parse::<EditorArgs>(args).map(|args| AppEvent::EditorFinished {
                outcome: args.outcome,
            })
        }
        AppEvent::ClipboardFailed { .. } => {
            parse::<ErrorArgs>(args).map(|args| AppEvent::ClipboardFailed { error: args.error })
        }
        AppEvent::LoginFinished { .. } => {
            parse::<LoginArgs>(args).map(|args| AppEvent::LoginFinished {
                auth_dir: args.auth_dir,
                exited_ok: args.exited_ok,
            })
        }
        AppEvent::DaemonActionFinished { .. } => {
            parse::<DaemonArgs>(args).map(|args| AppEvent::DaemonActionFinished {
                report: args.report,
            })
        }
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The three things a bare exit code never said.
    #[test]
    fn the_attach_notice_names_the_target_the_error_and_what_to_do() {
        let notice = attach_failure_notice(
            "tmux_myrepo_main",
            "tmux attach-session failed with exit code: Some(1)",
        );
        assert!(notice.contains("tmux_myrepo_main"), "the target: {notice}");
        assert!(notice.contains("exit code: Some(1)"), "the error: {notice}");
        assert!(
            notice.contains("press f to refresh"),
            "the remedy: {notice}"
        );
        assert!(notice.contains("nest"), "the other cause: {notice}");
    }

    #[test]
    fn credentials_count_only_after_a_clean_exit_that_wrote_them() {
        let dir = tempfile::tempdir().expect("auth dir");
        assert!(!oauth_credentials_written(dir.path(), true));
        std::fs::write(dir.path().join(".credentials.json"), "{}").expect("credentials");
        assert!(!oauth_credentials_written(dir.path(), false));
        assert!(oauth_credentials_written(dir.path(), true));
    }
}
