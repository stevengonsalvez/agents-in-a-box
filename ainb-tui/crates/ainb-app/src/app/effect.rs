// ABOUTME: The renderer contract's output side. The reducer never touches a
// terminal, an editor, the clipboard or a browser; it describes the work as an
// `Effect`, and whichever host drained the outbox carries it out.

use std::path::{Path, PathBuf};

use uuid::Uuid;

/// Work the reducer asks its host to do.
///
/// Effects are queued while an intent or a tick is applied and handed back by
/// [`crate::app::dispatch`] and the host's tick (`App::tick` in `ainb-core`)
/// once that step has finished writing state, so a host always acts on
/// committed state. Each
/// variant says which host executes it and what that host does when it cannot.
///
/// A host never writes state, and it never reads it either: everything an
/// effect needs rides on the effect. Whatever the work changed (a session
/// detached, a login wrote credentials, a tool was missing) comes back as a
/// report intent from [`crate::app::reports`], which the host dispatches like
/// any other; the reducer turns it into state and notices.
#[derive(Debug, Clone, PartialEq)]
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
    /// Open `path` in the user's editor.
    ///
    /// Terminal host: runs `preferred_editor`, else `code`, else `$EDITOR`,
    /// whichever is on `PATH` first, detached, and reports which with
    /// [`crate::app::reports::editor_finished`], or that none resolved or the
    /// editor failed to start. Desktop host: the same resolution, or the
    /// platform's default handler for the path.
    OpenEditor {
        path: EditorPath,
        /// The configured `preferred_editor`, read when the effect was queued.
        preferred_editor: Option<String>,
    },
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
    /// Sweep the local human's inbox read over `hangar/inbox_mark_read`, the
    /// D18 mutation it is: the host mints one op id, sends it, and retries a
    /// lost reply with the same id ([`crate::fleet::inbox_write`]). Reported
    /// with [`crate::app::reports::inbox_mark_all_read_finished`].
    ///
    /// Terminal host and desktop host: the same call off the UI thread,
    /// against the daemon each drives.
    InboxMarkAllRead,
    /// Ask `plugin` to run its own action `action_id` with `payload`, over
    /// `plugin/handle_action`.
    ///
    /// Terminal host: sends it through the plugin runtime it owns; when the
    /// runtime has no such plugin running, reports
    /// [`crate::app::reports::plugin_action_undelivered`]. What the action
    /// changed arrives through the plugin's render and its `ui.state` view.
    RunPluginAction {
        plugin: String,
        action_id: String,
        payload: serde_json::Value,
    },
    /// Send `input`, already translated to the plugin protocol, to `plugin`,
    /// which owns `screen`.
    ///
    /// Terminal host: sends it through the plugin runtime it owns. When a key
    /// that leaves the screen (`back`) cannot be serviced (the plugin is gone
    /// or its render is wedged), it reports
    /// [`crate::app::reports::plugin_input_undelivered`] so the reducer leaves
    /// the screen itself. Other input the plugin cannot take is dropped.
    /// Desktop host: the same, through its own runtime.
    ForwardToPlugin {
        plugin: String,
        screen: String,
        input: PluginInput,
        back: bool,
    },
    /// Write what the step just changed in a store.
    ///
    /// The reducer never writes to disk, so `dispatch` never waits on it, and
    /// it updates its own copy before queuing, so the same step reads back what
    /// it chose. One step queues at most one write per store: a later write to
    /// the same store folds into the earlier one ([`Persist::coalesce`]).
    ///
    /// Terminal host: writes with [`crate::config::persist::write`] once the
    /// step that queued it has finished, in queue order, and reports a failure
    /// with [`crate::app::reports::persist_failed`] carrying
    /// [`Persist::store_id`]; a write that lands says nothing. Desktop host: the
    /// same writes to the same files. The files resolve from `HOME` (the user
    /// config, favourites, labels, onboarding) and `AINB_HOME` (the session
    /// store), which is the seam a host points at its own config root.
    ///
    /// A failed write leaves disk behind memory. Whole-store writes
    /// (`Favorites`, `SessionLabels`, `Onboarding`) heal on the next change to
    /// that store, which carries the whole store again. Keyed and field writes
    /// (`AppConfig`, `ConfigExternalKeys`, `OnboardingGitDirectories`,
    /// `SessionHeadroom`) carry only what changed, so a failed one is lost
    /// until that setting changes again; the report says so and the host does
    /// not retry.
    Persist(Persist),
}

/// Input for a plugin, in the protocol's portable shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginInput {
    Key(ainb_plugin_runtime::KeyEvent),
    Mouse(ainb_plugin_runtime::MouseEvent),
}

/// A store an [`Effect::Persist`] writes.
#[derive(Debug, Clone, PartialEq)]
pub enum Persist {
    /// The named dotted keys of the user config, `config.toml`, each with its
    /// value in `config`. Every other key on disk is kept, so a copy loaded at
    /// startup cannot put back what another process wrote since.
    AppConfig {
        config: Snapshot<crate::config::AppConfig>,
        keys: Vec<String>,
    },
    /// Registry keys `config.toml` holds outside `AppConfig`'s shape, as the
    /// raw values the settings screen took; the host's write validates them.
    ConfigExternalKeys(Vec<(String, String)>),
    /// The repository favourites, whole.
    Favorites(Snapshot<crate::config::FavoritesStore>),
    /// The durable session labels, whole.
    SessionLabels(Snapshot<crate::config::SessionLabelStore>),
    /// The onboarding record, whole.
    Onboarding(Snapshot<crate::config::OnboardingConfig>),
    /// The onboarding record's git directories, set on the record on disk so
    /// the rest of it is kept.
    OnboardingGitDirectories(Vec<PathBuf>),
    /// One session's Headroom switch in the interactive session store, set to
    /// `enabled` under the store's lock only while it still reads `expected`,
    /// the value this step decided from.
    SessionHeadroom {
        tmux_session: String,
        expected: bool,
        enabled: bool,
    },
}

impl Persist {
    /// The store this writes, as a stable id a report carries.
    #[must_use]
    pub const fn store_id(&self) -> &'static str {
        match self {
            Self::AppConfig { .. } | Self::ConfigExternalKeys(_) => "config",
            Self::Favorites(_) => "favorites",
            Self::SessionLabels(_) => "session_labels",
            Self::Onboarding(_) | Self::OnboardingGitDirectories(_) => "onboarding",
            Self::SessionHeadroom { .. } => "session_store",
        }
    }

    /// What a notice calls the store `store_id` names.
    #[must_use]
    pub fn store_label(store_id: &str) -> &'static str {
        match store_id {
            "config" => "settings",
            "favorites" => "favorites",
            "session_labels" => "session labels",
            "onboarding" => "onboarding",
            "session_store" => "the session store",
            _ => "a store",
        }
    }

    /// Fold `later`, queued after `self` in the same step, into `self` when
    /// both are the same write: keyed writes take the union of their keys and
    /// the later values, whole-store writes take the later store, and a
    /// Headroom write keeps the first expected value and the last setting.
    /// Returns false, changing nothing, when they are different writes.
    pub fn coalesce(&mut self, later: &Self) -> bool {
        match (self, later) {
            (
                Self::AppConfig { config, keys },
                Self::AppConfig {
                    config: later_config,
                    keys: later_keys,
                },
            ) => {
                config.clone_from(later_config);
                for key in later_keys {
                    if !keys.contains(key) {
                        keys.push(key.clone());
                    }
                }
                true
            }
            (Self::ConfigExternalKeys(edits), Self::ConfigExternalKeys(later_edits)) => {
                for (key, value) in later_edits {
                    match edits.iter_mut().find(|(queued, _)| queued == key) {
                        Some(edit) => edit.1.clone_from(value),
                        None => edits.push((key.clone(), value.clone())),
                    }
                }
                true
            }
            (Self::Favorites(store), Self::Favorites(later)) => {
                store.clone_from(later);
                true
            }
            (Self::SessionLabels(store), Self::SessionLabels(later)) => {
                store.clone_from(later);
                true
            }
            (Self::Onboarding(record), Self::Onboarding(later)) => {
                record.clone_from(later);
                true
            }
            (
                Self::OnboardingGitDirectories(directories),
                Self::OnboardingGitDirectories(later),
            ) => {
                directories.clone_from(later);
                true
            }
            (
                Self::SessionHeadroom {
                    tmux_session,
                    enabled,
                    ..
                },
                Self::SessionHeadroom {
                    tmux_session: later_session,
                    enabled: later_enabled,
                    ..
                },
            ) if tmux_session == later_session => {
                *enabled = *later_enabled;
                true
            }
            _ => false,
        }
    }
}

/// A store's contents as they stood when an effect was queued. The stores do
/// not implement equality, so two snapshots compare by their serialised form.
#[derive(Debug, Clone)]
pub struct Snapshot<T>(pub T);

impl<T: serde::Serialize> PartialEq for Snapshot<T> {
    fn eq(&self, other: &Self) -> bool {
        serde_json::to_value(&self.0).ok() == serde_json::to_value(&other.0).ok()
    }
}

impl<T: serde::Serialize> Eq for Snapshot<T> {}

/// A tmux session name an effect can target.
///
/// Not empty, free of the `:` and `.` tmux reads as window and pane separators
/// and of control characters, with no whitespace at either end, and not
/// starting with `$`, `%`, `@` or `=`, which tmux reads as a session, pane or
/// window id or an exact-match marker, so such a name could reach another
/// session. At most [`TmuxSessionName::MAX_BYTES`] long: a name is mirrored to
/// every renderer, and a local process could otherwise rename a session to a
/// 100 KB string (#1096).
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "typescript-bindings", derive(specta::Type))]
pub struct TmuxSessionName(String);

impl TmuxSessionName {
    /// The longest name accepted, in bytes. Far past any name ainb mints
    /// (`tmux_<repo>_<branch>`), and small enough to put in every frame.
    pub const MAX_BYTES: usize = 128;

    /// Whether `name` is within the length cap. The one rule the constructor,
    /// tmux session discovery and the name minter share (#1122).
    #[must_use]
    pub const fn within_cap(name: &str) -> bool {
        name.len() <= Self::MAX_BYTES
    }

    /// The name, or `None` when tmux could not address a session by it.
    #[must_use]
    pub fn new(name: impl Into<String>) -> Option<Self> {
        let name = name.into();
        let addressable = !name.is_empty()
            && Self::within_cap(&name)
            && name.trim() == name
            && !name.contains([':', '.'])
            && !name.starts_with(['$', '%', '@', '='])
            && !name.chars().any(char::is_control);
        addressable.then_some(Self(name))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Written as the bare name, for a frame. There is deliberately no
/// `Deserialize`: a name only comes into being through [`TmuxSessionName::new`].
impl serde::Serialize for TmuxSessionName {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

/// A path an editor is asked to open: absolute, so it means the same thing to
/// every host whatever its working directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditorPath(PathBuf);

impl EditorPath {
    /// The path, or `None` when it is empty or relative.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Option<Self> {
        let path = path.into();
        path.is_absolute().then_some(Self(path))
    }

    #[must_use]
    pub fn as_path(&self) -> &Path {
        &self.0
    }
}

/// What an [`Effect::AttachTerminal`] attaches to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalTarget {
    /// The ainb session `id`, through its tmux session `tmux_session`.
    Session {
        id: Uuid,
        tmux_session: TmuxSessionName,
    },
    /// The selected row's tmux session `tmux_session`, writable in the session
    /// list's own preview pane instead of full screen, laid out with or
    /// without the session menu bar.
    ///
    /// Terminal host: sizes the pane for its layout, opens a writable tmux
    /// client on it, keeps the client, and reports
    /// [`crate::app::reports::in_place_opened`], or
    /// [`crate::app::reports::in_place_failed`] with the error. A host must
    /// not report `in_place_opened` before it holds the client: the reducer
    /// focuses the pane on that report. While the pane is live the host sends
    /// every key to the client except the chord
    /// [`crate::app::keymap::Keymap::releases_in_place_pane`] names, which it
    /// runs as `embed_interactive.detach`. A host that can never hold a
    /// writable client answers `in_place_failed` with `unsupported`. It closes the
    /// client once `TmuxSection::embed_session` no longer names the session,
    /// which is how the reducer declines or releases it. Output, input and
    /// exit stay between the host and its client; an exit or a closed input
    /// channel comes back as [`crate::app::reports::terminal_exited`] or
    /// [`crate::app::reports::terminal_input_closed`]. Desktop host: the same
    /// contract with its own terminal widget.
    InPlace {
        tmux_session: TmuxSessionName,
        show_menu_bar: bool,
    },
    /// A read-only mirror of the selected row's tmux session `tmux_session`
    /// in the session list's preview pane.
    ///
    /// Terminal host: opens a read-only tmux client that never sizes the
    /// session's window, keeps it, and reports
    /// [`crate::app::reports::observer_opened`], or
    /// [`crate::app::reports::observer_failed`] (marked unsupported when its
    /// tmux cannot keep a client out of the window size). Release, exit and
    /// ownership follow [`TerminalTarget::InPlace`]. A host with no terminal
    /// widget reports it unsupported.
    Observe {
        tmux_session: TmuxSessionName,
        show_menu_bar: bool,
    },
    /// A named tmux session ainb did not create: an "Other tmux" row, an SSH
    /// session's tmux, or a workspace shell that already exists.
    Tmux(TmuxSessionName),
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
        tmux_session: TmuxSessionName,
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

    /// Queue `store`, folding it into a write to the same store already queued
    /// ([`Persist::coalesce`]). The folded write moves after anything queued
    /// since, so the host writes each store once, last value winning.
    pub fn push_persist(&mut self, store: Persist) {
        let queued = self.0.iter_mut().position(|effect| match effect {
            Effect::Persist(earlier) => earlier.coalesce(&store),
            _ => false,
        });
        match queued {
            Some(index) => {
                let merged = self.0.remove(index);
                self.0.push(merged);
            }
            None => self.0.push(Effect::Persist(store)),
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tmux_session_name_tmux_cannot_address_is_refused() {
        for bad in [
            "",
            "work:1",
            "work.0",
            "bell\u{7}",
            "$0",
            "%1",
            "@2",
            "=work",
            " work",
            "work ",
            "work\t",
        ] {
            assert_eq!(TmuxSessionName::new(bad), None, "{bad:?}");
        }
        assert_eq!(
            TmuxSessionName::new("tmux_api_feat-login").map(|name| name.as_str().to_string()),
            Some("tmux_api_feat-login".to_string())
        );
        // #1096: the length is capped, at the boundary exactly.
        let longest = "a".repeat(TmuxSessionName::MAX_BYTES);
        assert!(TmuxSessionName::new(longest.clone()).is_some());
        assert_eq!(TmuxSessionName::new(format!("{longest}a")), None);
    }

    #[test]
    fn an_editor_path_must_be_absolute() {
        for bad in ["", "relative/file.rs", "./file.rs"] {
            assert_eq!(EditorPath::new(bad), None, "{bad:?}");
        }
        assert!(EditorPath::new("/parity/api/worktrees/feat-login").is_some());
    }
}
