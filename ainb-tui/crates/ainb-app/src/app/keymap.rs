//! Data model shared by terminal key dispatch, the keymap CLI, and generated docs.

use std::collections::HashMap;
use std::fmt;

use super::events::AppEvent;
use super::screens::ids as screen_ids;
use super::state::{AppState, FocusedPane};

/// Key variants used by the event tests, so a test names `Char('q')` or `Esc`
/// the way a renderer would hand it over.
#[cfg(test)]
pub(crate) mod test_key_codes {
    pub(crate) use super::Key::*;
}

/// A key on its own, before modifiers.
///
/// Renderers convert their native key events into this at their input edge;
/// nothing past that edge sees a renderer's key type. Shift+Tab is `Tab` with
/// [`Mods::SHIFT`], not a key of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Key {
    Char(char),
    Enter,
    Esc,
    Tab,
    Backspace,
    Delete,
    Insert,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    F(u8),
}

/// The modifier keys held with a [`Key`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Mods {
    bits: u8,
}

impl Mods {
    pub const NONE: Self = Self { bits: 0 };
    pub const CTRL: Self = Self { bits: 1 };
    pub const ALT: Self = Self { bits: 1 << 1 };
    pub const SHIFT: Self = Self { bits: 1 << 2 };

    /// Whether every modifier in `other` is held.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.bits & other.bits == other.bits
    }

    /// Whether no modifier is held.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.bits == 0
    }
}

impl std::ops::BitOr for Mods {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self {
        Self {
            bits: self.bits | rhs.bits,
        }
    }
}

/// A key plus its modifiers, in one canonical form.
///
/// Stored as its wire spelling (`"ctrl+k"`, `"G"`, `"shift+tab"`), which is
/// what the keymap table, `keymap.toml`, the docs and the palette all use, so
/// two chords are equal exactly when they would resolve to the same binding.
/// [`Chord::code`] and [`Chord::modifiers`] give the structured view a reducer
/// matches on.
#[derive(
    Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(into = "String", try_from = "String")]
pub struct Chord(String);

impl From<Chord> for String {
    fn from(chord: Chord) -> Self {
        chord.0
    }
}

impl TryFrom<String> for Chord {
    type Error = ChordParseError;

    fn try_from(spelling: String) -> Result<Self, Self::Error> {
        Self::parse(&spelling)
    }
}

/// Invalid user supplied chord.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChordParseError(String);

impl fmt::Display for ChordParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ChordParseError {}

impl From<Key> for Chord {
    fn from(key: Key) -> Self {
        Self::new(key, Mods::NONE)
    }
}

impl Chord {
    /// Parse the wire spelling used by `keymap.toml`.
    pub fn parse(input: &str) -> Result<Self, ChordParseError> {
        let input = input.trim();
        if input.is_empty() {
            return Err(ChordParseError("key chord cannot be empty".to_string()));
        }
        if input.split_whitespace().nth(1).is_some() {
            return Err(ChordParseError(
                "key sequences are not supported by terminal dispatch".to_string(),
            ));
        }

        Self::parse_key(input).map(Self)
    }

    fn parse_key(input: &str) -> Result<String, ChordParseError> {
        let mut modifiers = Vec::new();
        let mut key = None;
        for part in input.split('+') {
            if part.is_empty() {
                return Err(ChordParseError(format!("invalid chord `{input}`")));
            }
            match part.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => modifiers.push("ctrl"),
                "alt" => modifiers.push("alt"),
                "shift" => modifiers.push("shift"),
                "cmd" | "command" | "super" | "meta" => {
                    return Err(ChordParseError(
                        "cmd/super chords belong to the desktop renderer, not the TUI".to_string(),
                    ));
                }
                _ if key.replace(part).is_some() => {
                    return Err(ChordParseError(format!("invalid chord `{input}`")));
                }
                _ => {}
            }
        }

        let key = key.ok_or_else(|| ChordParseError(format!("missing key in `{input}`")))?;
        modifiers.sort_unstable();
        modifiers.dedup();
        let key = Self::normalise_key_name(
            key,
            modifiers.contains(&"shift")
                || (modifiers.is_empty() && key.chars().any(char::is_uppercase)),
        )?;
        if key.chars().count() == 1 && key.chars().all(char::is_alphabetic) {
            modifiers.retain(|modifier| *modifier != "shift");
        }
        let mut parts = modifiers.into_iter().map(str::to_string).collect::<Vec<_>>();
        parts.push(key);
        Ok(parts.join("+"))
    }

    fn normalise_key_name(key: &str, shifted: bool) -> Result<String, ChordParseError> {
        let lower = key.to_ascii_lowercase();
        let named = [
            "backspace",
            "delete",
            "down",
            "end",
            "enter",
            "esc",
            "home",
            "insert",
            "left",
            "pagedown",
            "pageup",
            "plus",
            "right",
            "space",
            "tab",
            "up",
        ];
        if named.contains(&lower.as_str())
            || (lower.starts_with('f') && lower[1..].parse::<u8>().is_ok())
        {
            return Ok(lower);
        }
        let mut chars = key.chars();
        let char_key = chars
            .next()
            .filter(|_| chars.next().is_none())
            .ok_or_else(|| ChordParseError(format!("unsupported key `{key}`")))?;
        if char_key.is_ascii_alphabetic() {
            return Ok(if shifted {
                char_key.to_ascii_uppercase().to_string()
            } else {
                char_key.to_ascii_lowercase().to_string()
            });
        }
        Ok(char_key.to_string())
    }

    /// Build the chord a renderer reports for `key` held with `mods`.
    ///
    /// Shift on a printable glyph is dropped: the glyph (`:`, `G`) already
    /// carries it, and keeping `shift+:` would leak a terminal's layout
    /// details into the user-facing table.
    #[must_use]
    pub fn new(key: Key, mods: Mods) -> Self {
        let mut modifiers = Vec::new();
        if mods.contains(Mods::CTRL) {
            modifiers.push("ctrl");
        }
        if mods.contains(Mods::ALT) {
            modifiers.push("alt");
        }
        if mods.contains(Mods::SHIFT) && !matches!(key, Key::Char(_)) {
            modifiers.push("shift");
        }
        let name = match key {
            Key::Backspace => "backspace".to_string(),
            Key::Enter => "enter".to_string(),
            Key::Left => "left".to_string(),
            Key::Right => "right".to_string(),
            Key::Up => "up".to_string(),
            Key::Down => "down".to_string(),
            Key::Home => "home".to_string(),
            Key::End => "end".to_string(),
            Key::PageUp => "pageup".to_string(),
            Key::PageDown => "pagedown".to_string(),
            Key::Tab => "tab".to_string(),
            Key::Delete => "delete".to_string(),
            Key::Insert => "insert".to_string(),
            Key::F(number) => format!("f{number}"),
            Key::Esc => "esc".to_string(),
            Key::Char(' ') => "space".to_string(),
            Key::Char('+') => "plus".to_string(),
            Key::Char(character) => character.to_string(),
        };
        let source = modifiers
            .into_iter()
            .chain(std::iter::once(name.as_str()))
            .collect::<Vec<_>>()
            .join("+");
        // Every `Key` spells a supported wire key.
        Self::parse(&source).expect("every Key has a wire spelling")
    }

    /// The key, without its modifiers.
    #[must_use]
    pub fn code(&self) -> Key {
        let name = self.0.rsplit('+').next().unwrap_or_default();
        // A lone `+` never survives parsing (it is spelled `plus`), so the last
        // `+`-separated part is always the key name.
        match name {
            "backspace" => Key::Backspace,
            "delete" => Key::Delete,
            "down" => Key::Down,
            "end" => Key::End,
            "enter" => Key::Enter,
            "esc" => Key::Esc,
            "home" => Key::Home,
            "insert" => Key::Insert,
            "left" => Key::Left,
            "pagedown" => Key::PageDown,
            "pageup" => Key::PageUp,
            "plus" => Key::Char('+'),
            "right" => Key::Right,
            "space" => Key::Char(' '),
            "tab" => Key::Tab,
            "up" => Key::Up,
            function if function.len() > 1 && function.starts_with('f') => {
                Key::F(function[1..].parse().expect("parsed chords only carry f<number>"))
            }
            glyph => Key::Char(glyph.chars().next().expect("parsed chords carry a key")),
        }
    }

    /// The modifiers held with [`Chord::code`].
    #[must_use]
    pub fn modifiers(&self) -> Mods {
        let mut parts = self.0.split('+').collect::<Vec<_>>();
        parts.pop();
        parts.into_iter().fold(Mods::NONE, |mods, part| {
            mods | match part {
                "ctrl" => Mods::CTRL,
                "alt" => Mods::ALT,
                "shift" => Mods::SHIFT,
                _ => Mods::NONE,
            }
        })
    }

    /// Wire spelling used in TOML, docs, and palette labels.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Bare printable glyph, when this chord represents text input.
    #[must_use]
    pub fn printable(&self) -> Option<char> {
        match self.0.as_str() {
            "space" => Some(' '),
            value if !value.contains('+') && value.chars().count() == 1 => value.chars().next(),
            _ => None,
        }
    }
}

/// A sub-state of a screen. Strings deliberately preserve unmigrated legacy guards.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SubContext {
    None,
    Named(&'static str),
}

/// Active keymap area. Caller orders active contexts by precedence.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum KeyContext {
    EmbedInteractive,
    PreviewScroll,
    ConfirmDialog,
    McpOverlay,
    SessionRename,
    OtherTmuxRename,
    SshRename,
    SessionContextMenu,
    HelpVisible,
    QuickCommit,
    ConfigPopup,
    AuthProviderPopup,
    TextInput,
    Screen(&'static str, SubContext),
    Global,
}

/// The keymap row that releases the in-place interactive pane.
pub const EMBED_DETACH_ROW: &str = "detach";

impl KeyContext {
    /// Screen context without a sub-state.
    #[must_use]
    pub const fn screen(screen: &'static str) -> Self {
        Self::Screen(screen, SubContext::None)
    }

    /// Stable TOML and docs group name.
    #[must_use]
    pub fn name(&self) -> String {
        match self {
            Self::EmbedInteractive => "embed_interactive".to_string(),
            Self::PreviewScroll => "preview_scroll".to_string(),
            Self::ConfirmDialog => "confirm_dialog".to_string(),
            Self::McpOverlay => "mcp_overlay".to_string(),
            Self::SessionRename => "session_rename".to_string(),
            Self::OtherTmuxRename => "other_tmux_rename".to_string(),
            Self::SshRename => "ssh_rename".to_string(),
            Self::SessionContextMenu => "session_context_menu".to_string(),
            Self::HelpVisible => "help_visible".to_string(),
            Self::QuickCommit => "quick_commit".to_string(),
            Self::ConfigPopup => "config_popup".to_string(),
            Self::AuthProviderPopup => "auth_provider_popup".to_string(),
            Self::TextInput => "text_input".to_string(),
            Self::Screen(screen, SubContext::None) => (*screen).to_string(),
            Self::Screen(screen, SubContext::Named(sub)) => format!("{screen}.{sub}"),
            Self::Global => "global".to_string(),
        }
    }

    /// Parse a context name used as a TOML table name.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "embed_interactive" => Self::EmbedInteractive,
            "preview_scroll" => Self::PreviewScroll,
            "confirm_dialog" => Self::ConfirmDialog,
            "mcp_overlay" => Self::McpOverlay,
            "session_rename" => Self::SessionRename,
            "other_tmux_rename" => Self::OtherTmuxRename,
            "ssh_rename" => Self::SshRename,
            "session_context_menu" => Self::SessionContextMenu,
            "help_visible" => Self::HelpVisible,
            "quick_commit" => Self::QuickCommit,
            "config_popup" => Self::ConfigPopup,
            "auth_provider_popup" => Self::AuthProviderPopup,
            "text_input" => Self::TextInput,
            "global" => Self::Global,
            "home" => Self::screen("home"),
            "session_list" => Self::screen("session_list"),
            "config" => Self::screen("config"),
            "git_view" => Self::screen("git_view"),
            "log_history" => Self::screen("log_history"),
            "session_recovery" => Self::screen("session_recovery"),
            "skills" => Self::screen("skills"),
            "skill_manager" => Self::screen("skill_manager"),
            "daemons" => Self::screen("daemons"),
            "notifications.visible" => Self::Screen("notifications", SubContext::Named("visible")),
            "help.text" => Self::Screen("help", SubContext::Named("text")),
            "plugin.owned" => Self::Screen("plugin", SubContext::Named("owned")),
            "home.sidebar" => Self::Screen("home", SubContext::Named("sidebar")),
            "home.content" => Self::Screen("home", SubContext::Named("content")),
            "session_list.sessions_pane" => {
                Self::Screen("session_list", SubContext::Named("sessions_pane"))
            }
            "session_list.composer" => Self::Screen("session_list", SubContext::Named("composer")),
            "session_list.ask" => Self::Screen("session_list", SubContext::Named("ask")),
            "session_list.logs_pane" => {
                Self::Screen("session_list", SubContext::Named("logs_pane"))
            }
            "session_list.preview_pane" => {
                Self::Screen("session_list", SubContext::Named("preview_pane"))
            }
            "skill_manager.sync_confirm" => {
                Self::Screen("skill_manager", SubContext::Named("sync_confirm"))
            }
            "skill_manager.preview" => Self::Screen("skill_manager", SubContext::Named("preview")),
            "skill_manager.source_remove" => {
                Self::Screen("skill_manager", SubContext::Named("source_remove"))
            }
            "skill_manager.input" => Self::Screen("skill_manager", SubContext::Named("input")),
            "skill_manager.browse_query" => {
                Self::Screen("skill_manager", SubContext::Named("browse_query"))
            }
            "skill_manager.browse_results" => {
                Self::Screen("skill_manager", SubContext::Named("browse_results"))
            }
            "skill_manager.library" => Self::Screen("skill_manager", SubContext::Named("library")),
            "skill_manager.discovery" => {
                Self::Screen("skill_manager", SubContext::Named("discovery"))
            }
            "skill_manager.sources" => Self::Screen("skill_manager", SubContext::Named("sources")),
            "git_view.review" => Self::Screen("git_view", SubContext::Named("review")),
            "git_view.commit" => Self::Screen("git_view", SubContext::Named("commit")),
            "log_history.sessions" => Self::Screen("log_history", SubContext::Named("sessions")),
            "log_history.logs" => Self::Screen("log_history", SubContext::Named("logs")),
            "skills.search" => Self::Screen("skills", SubContext::Named("search")),
            "session_recovery.search" => {
                Self::Screen("session_recovery", SubContext::Named("search"))
            }
            "session_recovery.filtered" => {
                Self::Screen("session_recovery", SubContext::Named("filtered"))
            }
            "config.editing" => Self::Screen("config", SubContext::Named("editing")),
            "config.api_key" => Self::Screen("config", SubContext::Named("api_key")),
            "config.search" => Self::Screen("config", SubContext::Named("search")),
            "config.categories" => Self::Screen("config", SubContext::Named("categories")),
            "config_popup.input" => Self::Screen("config_popup", SubContext::Named("input")),
            "auth_provider_popup.input" => {
                Self::Screen("auth_provider_popup", SubContext::Named("input"))
            }
            "auth_setup.picker" => Self::Screen("auth_setup", SubContext::Named("picker")),
            "auth_setup.input" => Self::Screen("auth_setup", SubContext::Named("input")),
            "search_workspace" => Self::screen("search_workspace"),
            "onboarding.git_directories" => {
                Self::Screen("onboarding", SubContext::Named("git_directories"))
            }
            "onboarding.questions" => Self::Screen("onboarding", SubContext::Named("questions")),
            "onboarding.dependency" => Self::Screen("onboarding", SubContext::Named("dependency")),
            "onboarding.dependency_agent" => {
                Self::Screen("onboarding", SubContext::Named("dependency_agent"))
            }
            "onboarding.auth_key" => Self::Screen("onboarding", SubContext::Named("auth_key")),
            "onboarding.auth_method" => {
                Self::Screen("onboarding", SubContext::Named("auth_method"))
            }
            "onboarding.auth_agents" => {
                Self::Screen("onboarding", SubContext::Named("auth_agents"))
            }
            "onboarding.otel" => Self::Screen("onboarding", SubContext::Named("otel")),
            "onboarding.dependency_ready" => {
                Self::Screen("onboarding", SubContext::Named("dependency_ready"))
            }
            "onboarding.welcome" => Self::Screen("onboarding", SubContext::Named("welcome")),
            "onboarding.editor" => Self::Screen("onboarding", SubContext::Named("editor")),
            "onboarding.summary" => Self::Screen("onboarding", SubContext::Named("summary")),
            "setup_menu.menu" => Self::Screen("setup_menu", SubContext::Named("menu")),
            "setup_menu.confirm" => Self::Screen("setup_menu", SubContext::Named("confirm")),
            "daemons.overlay" => Self::Screen("daemons", SubContext::Named("overlay")),
            "daemons.list" => Self::Screen("daemons", SubContext::Named("list")),
            _ => return None,
        })
    }
}

/// Which `onboarding.*` sub-context the wizard is in.
///
/// Lifted out of [`active_contexts`] so it can be enumerated: every step of the
/// wizard has to resolve to a sub-context the table actually has rows for, and
/// four of them did not. A free function over the four inputs that decide it is
/// something a test can walk exhaustively; a match buried in a 600-line
/// dispatcher is not.
#[must_use]
pub(crate) fn onboarding_sub_context(
    step: &crate::components::onboarding::OnboardingStep,
    auth_pane: &crate::components::onboarding::AuthPane,
    agent_pick_open: bool,
    dependencies_checked: bool,
) -> &'static str {
    use crate::components::onboarding::{AuthPane, OnboardingStep};

    match step {
        OnboardingStep::GitDirectories => "git_directories",
        OnboardingStep::Source | OnboardingStep::Role | OnboardingStep::UseCase => "questions",
        OnboardingStep::DependencyCheck if agent_pick_open => "dependency_agent",
        OnboardingStep::DependencyCheck if dependencies_checked => "dependency_ready",
        OnboardingStep::DependencyCheck => "dependency",
        OnboardingStep::Authentication => match auth_pane {
            AuthPane::KeyEntry { .. } => "auth_key",
            AuthPane::MethodPicker { .. } => "auth_method",
            AuthPane::AgentList => "auth_agents",
        },
        OnboardingStep::OtelSetup => "otel",
        OnboardingStep::EditorSelection => "editor",
        OnboardingStep::Summary => "summary",
        OnboardingStep::Welcome => "welcome",
    }
}

/// The contexts a named command may run in: [`active_contexts`] up to the end
/// of the topmost overlay, when one is open, and then [`KeyContext::Global`].
///
/// A command is a click or a palette pick resolved against what a renderer
/// drew. While a dialog, popup, menu or the live terminal pane covers the
/// screen, a command for the screen beneath it (a wheel over the diff, a row
/// click) was aimed at something the user can no longer act on, so it runs
/// only if it belongs to the overlay or is global. Keys are not cut: a key the
/// overlay does not bind still reaches the screen, because the user pressed it
/// looking at the screen.
#[must_use]
pub fn command_contexts(state: &AppState) -> Vec<KeyContext> {
    let mut contexts = classified_contexts(state);
    if let Some(first) = contexts.iter().position(|(_, overlay)| *overlay) {
        let end = first + contexts[first..].iter().take_while(|(_, overlay)| *overlay).count();
        contexts.truncate(end);
        contexts.push((KeyContext::Global, false));
    }
    contexts.into_iter().map(|(context, _)| context).collect()
}

/// Mirror host dispatch precedence without allowing renderer state into `AppState`.
#[must_use]
pub fn active_contexts(state: &AppState) -> Vec<KeyContext> {
    classified_contexts(state).into_iter().map(|(context, _)| context).collect()
}

/// Every context a key resolves through, in precedence order, each marked
/// where it is pushed with whether it is an overlay drawn over the screen. The
/// preview-scroll mode is the terminal host's own and is never pushed here.
fn classified_contexts(state: &AppState) -> Vec<(KeyContext, bool)> {
    let mut contexts = ContextList::default();
    let mut text_context_pushed = false;
    let text_input_active = crate::app::events::EventHandler::is_in_text_input_context(state);
    let auth_setup_api_input = state.shell.current_screen == screen_ids::AUTH_SETUP
        && state
            .onboarding
            .auth_setup_state
            .as_ref()
            .is_some_and(|auth| auth.selected_method == crate::app::state::AuthMethod::ApiKey);
    // Attached-terminal text is terminal-owned, while the auth picker only
    // accepts text after the API-key method has been selected. Neither may
    // consume host-owned table rows through the generic text fallback.
    let table_text_input_active = text_input_active
        && state.shell.current_screen != screen_ids::ATTACHED_TERMINAL
        && (state.shell.current_screen != screen_ids::AUTH_SETUP || auth_setup_api_input);
    let plugin_screen_active =
        crate::app::screens::builtin::plugin_id_for_screen(&state.shell.current_screen).is_some();

    if state.is_interactive_pane() {
        contexts.overlay(KeyContext::EmbedInteractive);
    }
    if state.shell.confirmation_dialog.is_some() {
        contexts.overlay(KeyContext::ConfirmDialog);
    }
    if state.mcp_pool.mcp_overlay.is_some() {
        contexts.overlay(KeyContext::McpOverlay);
    }
    if state.tmux.other_tmux_rename_mode {
        contexts.overlay(KeyContext::OtherTmuxRename);
    }
    if state.ssh.ssh_session_rename_mode {
        contexts.overlay(KeyContext::SshRename);
    }
    if state.session_labels.session_label_rename_mode {
        contexts.overlay(KeyContext::SessionRename);
    }
    if state.session_labels.session_context_menu.is_some() {
        contexts.overlay(KeyContext::SessionContextMenu);
    }
    if state.shell.help_visible {
        if text_input_active {
            contexts.overlay(KeyContext::Screen("help", SubContext::Named("text")));
        } else {
            contexts.overlay(KeyContext::HelpVisible);
        }
    }
    // Toasts are not an overlay: any live notice shows one, and it covers
    // nothing the user clicks.
    if state.has_visible_notifications() {
        contexts.push(KeyContext::Screen(
            "notifications",
            SubContext::Named("visible"),
        ));
    }
    if state.is_in_quick_commit_mode() {
        contexts.overlay(KeyContext::QuickCommit);
    }
    if state.shell.current_screen == screen_ids::SESSION_LIST {
        use crate::components::session_tabs::{SessionTab, resolve};

        match resolve(state, state.shell.session_tab) {
            SessionTab::Ask => contexts.push(KeyContext::Screen(
                screen_ids::SESSION_LIST,
                SubContext::Named("ask"),
            )),
            SessionTab::Thread | SessionTab::Pal if state.session_tab_owns_keys() => {
                contexts.push(KeyContext::Screen(
                    screen_ids::SESSION_LIST,
                    SubContext::Named("composer"),
                ));
                // Typed text is the composer's ahead of the focused pane's
                // rows: the logs pane binds space. Only while it captures text:
                // a focused card leaves `q` and `d` to their rows and answers
                // its own keys through the event handler.
                if state.session_composer_captures_text() {
                    contexts.push(KeyContext::TextInput);
                    text_context_pushed = true;
                }
            }
            _ => {}
        }
    }
    if state.onboarding.auth_provider_popup_state.show_popup {
        if state.onboarding.auth_provider_popup_state.is_entering_key {
            contexts.overlay(KeyContext::Screen(
                "auth_provider_popup",
                SubContext::Named("input"),
            ));
            // Text ownership precedes the popup's `d` delete shortcut. This
            // keeps a typed `d` in an API key from deleting the stored key.
            contexts.overlay(KeyContext::TextInput);
            text_context_pushed = true;
        }
        contexts.overlay(KeyContext::AuthProviderPopup);
    }
    if state.config.config_popup_state.show_popup {
        if state.config.config_popup_state.is_text_entry() {
            contexts.overlay(KeyContext::Screen(
                "config_popup",
                SubContext::Named("input"),
            ));
            // Text ownership precedes the popup's j/k navigation rows.
            contexts.overlay(KeyContext::TextInput);
            text_context_pushed = true;
        }
        contexts.overlay(KeyContext::ConfigPopup);
    }

    let screen = match state.shell.current_screen.as_str() {
        screen_ids::HOME => Some(screen_ids::HOME),
        screen_ids::SESSION_LIST => Some(screen_ids::SESSION_LIST),
        screen_ids::CONFIG => Some(screen_ids::CONFIG),
        screen_ids::GIT_VIEW => Some(screen_ids::GIT_VIEW),
        screen_ids::LOG_HISTORY => Some(screen_ids::LOG_HISTORY),
        screen_ids::SESSION_RECOVERY => Some(screen_ids::SESSION_RECOVERY),
        screen_ids::SKILLS => Some(screen_ids::SKILLS),
        screen_ids::SKILL_MANAGER => Some(screen_ids::SKILL_MANAGER),
        screen_ids::DAEMONS => Some(screen_ids::DAEMONS),
        screen_ids::INBOX => Some(screen_ids::INBOX),
        screen_ids::ONBOARDING => Some(screen_ids::ONBOARDING),
        screen_ids::SETUP_MENU => Some(screen_ids::SETUP_MENU),
        screen_ids::AUTH_SETUP => Some(screen_ids::AUTH_SETUP),
        screen_ids::CLAUDE_CHAT => Some(screen_ids::CLAUDE_CHAT),
        screen_ids::ATTACHED_TERMINAL => Some(screen_ids::ATTACHED_TERMINAL),
        screen_ids::SEARCH_WORKSPACE => Some(screen_ids::SEARCH_WORKSPACE),
        screen_ids::NON_GIT_NOTIFICATION => Some(screen_ids::NON_GIT_NOTIFICATION),
        screen_ids::CHANGELOG => Some(screen_ids::CHANGELOG),
        _ => None,
    };

    let mut base_screen = None;
    if let Some(screen) = screen {
        match screen {
            screen_ids::SESSION_LIST => match state.shell.focused_pane {
                FocusedPane::Sessions => contexts.push(KeyContext::Screen(
                    screen,
                    SubContext::Named("sessions_pane"),
                )),
                FocusedPane::LiveLogs => {
                    contexts.push(KeyContext::Screen(screen, SubContext::Named("logs_pane")))
                }
                FocusedPane::Preview => contexts.push(KeyContext::Screen(
                    screen,
                    SubContext::Named("preview_pane"),
                )),
            },
            screen_ids::HOME => {
                let focus = format!("{:?}", state.shell.home_screen_v2_state.focus);
                let sub = if focus == "Sidebar" {
                    "sidebar"
                } else {
                    "content"
                };
                contexts.push(KeyContext::Screen(screen, SubContext::Named(sub)));
            }
            screen_ids::CONFIG => {
                if state.config.config_screen_state.api_key_input_mode {
                    contexts.push(KeyContext::Screen(screen, SubContext::Named("api_key")));
                } else if state.config.config_screen_state.editing {
                    contexts.push(KeyContext::Screen(screen, SubContext::Named("editing")));
                } else if state.config.config_screen_state.is_searching() {
                    contexts.push(KeyContext::Screen(screen, SubContext::Named("search")));
                } else if state.config.config_screen_state.focused_pane
                    == crate::app::state::ConfigPane::Categories
                {
                    contexts.push(KeyContext::Screen(screen, SubContext::Named("categories")));
                }
            }
            screen_ids::GIT_VIEW => {
                if let Some(git) = &state.git_view.git_view_state {
                    if git.is_in_commit_mode() {
                        contexts.push(KeyContext::Screen(screen, SubContext::Named("commit")));
                    } else if matches!(git.active_tab, crate::components::git_view::GitTab::Review)
                    {
                        contexts.push(KeyContext::Screen(screen, SubContext::Named("review")));
                    }
                }
            }
            screen_ids::LOG_HISTORY => {
                let sub = match state.log_streams.log_history_state.focus {
                    crate::components::log_history_viewer::LogViewerFocus::SessionList => {
                        "sessions"
                    }
                    crate::components::log_history_viewer::LogViewerFocus::LogEntries => "logs",
                };
                contexts.push(KeyContext::Screen(screen, SubContext::Named(sub)));
            }
            screen_ids::SESSION_RECOVERY => {
                if state.recovery.session_recovery_state.recovery_overlay.is_some() {
                    contexts.overlay(KeyContext::Screen(screen, SubContext::Named("overlay")));
                } else if state.recovery.session_recovery_state.search_active {
                    contexts.push(KeyContext::Screen(screen, SubContext::Named("search")));
                } else if !state.recovery.session_recovery_state.search_query.is_empty() {
                    contexts.push(KeyContext::Screen(screen, SubContext::Named("filtered")));
                }
            }
            screen_ids::SKILLS if state.skills.skills_state.search_active => {
                contexts.push(KeyContext::Screen(screen, SubContext::Named("search")));
            }
            screen_ids::SKILL_MANAGER => {
                use crate::components::skill_manager_screen::{BrowseMode, FocusedSkillPane};

                let skills = &state.skills.skill_manager_state;
                if skills.input.is_some() {
                    contexts.push(KeyContext::Screen(screen, SubContext::Named("input")));
                } else if skills.sync_confirm.is_some() {
                    contexts.overlay(KeyContext::Screen(
                        screen,
                        SubContext::Named("sync_confirm"),
                    ));
                } else if skills.source_remove_confirm.is_some() {
                    contexts.overlay(KeyContext::Screen(
                        screen,
                        SubContext::Named("source_remove"),
                    ));
                } else if skills.preview.is_some() {
                    contexts.push(KeyContext::Screen(screen, SubContext::Named("preview")));
                } else if let Some(browse) = &skills.browse {
                    let sub = match browse.mode {
                        BrowseMode::Query => "browse_query",
                        BrowseMode::Results => "browse_results",
                    };
                    contexts.push(KeyContext::Screen(screen, SubContext::Named(sub)));
                } else if skills.library.is_some() {
                    contexts.push(KeyContext::Screen(screen, SubContext::Named("library")));
                } else if skills.banner.is_active() {
                    contexts.push(KeyContext::Screen(screen, SubContext::Named("discovery")));
                } else if matches!(skills.focused_pane, FocusedSkillPane::Sources) {
                    contexts.push(KeyContext::Screen(screen, SubContext::Named("sources")));
                }
            }
            screen_ids::AUTH_SETUP => {
                if state.onboarding.auth_setup_state.is_some() {
                    if auth_setup_api_input {
                        contexts.push(KeyContext::Screen(screen, SubContext::Named("input")));
                    } else {
                        contexts.push(KeyContext::Screen(screen, SubContext::Named("picker")));
                    }
                }
            }
            screen_ids::ONBOARDING => {
                if let Some(onboarding) = &state.onboarding.onboarding_state {
                    let sub = onboarding_sub_context(
                        &onboarding.current_step,
                        &onboarding.auth_pane,
                        onboarding.agent_pick_open,
                        onboarding.dependency_status.is_some(),
                    );
                    contexts.push(KeyContext::Screen(screen, SubContext::Named(sub)));
                }
            }
            screen_ids::SETUP_MENU => {
                if state.onboarding.setup_menu_state.showing_confirmation {
                    contexts.overlay(KeyContext::Screen(screen, SubContext::Named("confirm")));
                } else {
                    contexts.push(KeyContext::Screen(screen, SubContext::Named("menu")));
                }
            }
            screen_ids::DAEMONS => {
                if state.hangar.daemons_state.has_overlay() {
                    contexts.overlay(KeyContext::Screen(screen, SubContext::Named("overlay")));
                } else {
                    contexts.push(KeyContext::Screen(screen, SubContext::Named("list")));
                }
            }
            _ => {}
        }
        base_screen = Some(screen);
    }

    let ask_free_text = state.shell.current_screen == screen_ids::SESSION_LIST
        && matches!(
            crate::components::session_tabs::resolve(state, state.shell.session_tab),
            crate::components::session_tabs::SessionTab::Ask
        )
        && state.fleet.ask_state.focus() == crate::fleet::answer::AskFocus::FreeText;
    if (table_text_input_active || ask_free_text) && !text_context_pushed {
        contexts.push(KeyContext::TextInput);
    }
    if plugin_screen_active {
        contexts.push(KeyContext::Screen("plugin", SubContext::Named("owned")));
    }
    if let Some(screen) = base_screen {
        contexts.push(KeyContext::screen(screen));
    }
    contexts.push(KeyContext::Global);
    contexts.0
}

/// Contexts in push order, each marked with whether it is an overlay.
#[derive(Default)]
struct ContextList(Vec<(KeyContext, bool)>);

impl ContextList {
    fn push(&mut self, context: KeyContext) {
        self.0.push((context, false));
    }

    /// Push a context drawn over the screen: a named command stops at it.
    fn overlay(&mut self, context: KeyContext) {
        self.0.push((context, true));
    }
}

/// A renderer-local scroll intent.
///
/// Split out of [`UiAction`] so that both halves of the scroll path can be
/// exhaustive matches. They were two hand-written lists of the same twelve
/// variants, one in `events.rs` deciding what to queue and one in
/// `UiState::apply` deciding what to do, each ending in a catch-all. A
/// thirteenth variant added to one list and missed in the other is a key that
/// silently does nothing, which is the failure this nesting makes impossible:
/// the compiler now names the arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrollAction {
    ScrollLogsUp,
    ScrollLogsDown,
    ScrollLogsToTop,
    ScrollLogsToBottom,
    ToggleAutoScroll,
    ScrollPreviewUp,
    ScrollPreviewDown,
    PreviewScrollUp,
    PreviewScrollDown,
    PreviewPageUp,
    PreviewPageDown,
    PreviewExitScroll,
    /// The changelog viewer, a line or a page at a time or to either end.
    /// Where it is scrolled is this renderer's alone (#1052).
    ChangelogUp,
    ChangelogDown,
    ChangelogPageUp,
    ChangelogPageDown,
    ChangelogToTop,
    ChangelogToBottom,
}

/// Work a keymap row hands to the renderer that dispatched it.
///
/// Delivered through [`crate::app::events::RendererHost::queue`]. It changes
/// how that renderer lays things out, never app state, so another renderer on
/// the same state is unaffected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostAction {
    /// Scroll a pane.
    Scroll(ScrollAction),
    /// Collapse or expand the sessions sidebar.
    ToggleSessionsSidebar,
    /// Widen the Skill Manager's Sources panel by one step, within this
    /// renderer's width.
    GrowSkillSources,
    /// Narrow the Skill Manager's Sources panel by one step.
    ShrinkSkillSources,
}

/// Renderer-local command: it is applied to the ratatui host's `UiState` and
/// `LayoutComponent` and never reaches the reducer, because scroll position is
/// not something the product knows.
#[derive(Debug, Clone)]
pub enum UiAction {
    /// Renderer-local scrolling, applied against the host layout. Nested rather
    /// than flattened so the two matches that handle it stay exhaustive.
    Scroll(ScrollAction),
    SessionComposerEnter,
    SessionComposerBackspace,
    SessionComposerEscape,
    SessionComposerUp,
    SessionComposerDown,
    SessionComposerFocusToggle,
    SessionComposerRetry,
    SessionComposerCancel,
    PalCycleEngine,
    PalCycleModel,
    PalCycleMode,
    PalRetry,
    SessionAskPrevious,
    SessionAskNext,
    SessionAskBackspace,
    SessionAskClear,
    SkillManagerShrinkSources,
    SkillManagerGrowSources,
    DaemonsCloseOverlay,
    DaemonsCloseAndBack,
    DaemonsConfirmMenu,
    DaemonsOpenMenu,
    DaemonsMoveOverlay(isize),
    DaemonsMoveSelection(isize),
    SkillManagerSyncOrConflict,
    SkillManagerRemoveOrSource,
    SkillManagerOpenUnitIfFocused,
    SkillManagerCopyToLibraryIfFocused,
    SkillManagerBackOrClearFilter,
    SessionActivateSelected,
    SessionResumeSelected,
    SessionStartRename,
    SessionHeadroomOrHelp,
    AttachSessionByPosition(usize),
    UsageWireStatusline,
    /// Renderer-local, like [`Self::Scroll`]: the sidebar's collapsed flag is
    /// layout the renderer owns.
    ToggleSessionsSidebar,
}

/// Intent emitted by a key binding.
#[derive(Debug, Clone)]
pub enum KeyAction {
    App(AppEvent),
    Ui(UiAction),
    Passthrough,
    OpenSlashPalette,
    Text(char),
}

impl KeyAction {
    /// A pointer or report row's payload parse; see
    /// [`crate::app::pointer::with_args`].
    fn host_command_args(
        event: &AppEvent,
        args: &crate::app::intent::Args,
    ) -> Option<Option<AppEvent>> {
        crate::app::pointer::with_args(event, args)
            .or_else(|| crate::app::reports::with_args(event, args))
            .or_else(|| crate::app::plugin_action::with_args(event, args))
    }

    /// Whether running this action changes something ainb did not create:
    /// a file outside `~/.agents-in-a-box` and the session worktree, a tmux
    /// session ainb did not start, a package install, or a network write.
    /// `tests/key_only_completeness.rs` derives the same set from the reducer
    /// source and fails when the two disagree.
    ///
    /// A confirmation dialog's Confirm is not listed here: what it does is the
    /// dialog's pending [`crate::app::state::ConfirmAction`], which
    /// [`crate::app::state::ConfirmAction::runs_only_from_key`] judges.
    #[must_use]
    pub const fn writes_outside_ainb(&self) -> bool {
        match self {
            Self::App(event) => app_event_writes_outside_ainb(event),
            Self::Ui(action) => ui_action_writes_outside_ainb(action),
            // A pass-through key goes to the session pane ainb started; a
            // palette opens and a character is typed into ainb's own input.
            Self::Passthrough | Self::OpenSlashPalette | Self::Text(_) => false,
        }
    }

    fn carries_payload(&self) -> bool {
        // A pointer row carries a payload exactly when it refuses to run bare.
        if let Self::App(event) = self {
            if matches!(
                Self::host_command_args(event, &serde_json::Value::Null),
                Some(None)
            ) {
                return true;
            }
        }
        matches!(
            self,
            Self::App(
                AppEvent::OnboardingGenerateScript(_)
                    | AppEvent::SkillManagerSyncScroll(_)
                    | AppEvent::SkillManagerPreviewTool(_)
                    | AppEvent::SkillManagerSourceRemoveMove(_)
            ) | Self::Ui(
                UiAction::AttachSessionByPosition(_)
                    | UiAction::DaemonsMoveOverlay(_)
                    | UiAction::DaemonsMoveSelection(_)
            ) | Self::Text(_)
        )
    }

    /// This row's action with its payload replaced by `args`.
    ///
    /// `Args::Null` runs the row as the table wrote it. Any other value must
    /// be the payload of a row that carries one, as a JSON value of that
    /// payload's type: a number for a position or a step, an agent name for
    /// the onboarding script, one character for a text row. Anything else,
    /// a payload for a row that takes none or a value of the wrong type, is
    /// `None`, and the command does not run.
    #[must_use]
    pub fn with_args(&self, args: &crate::app::intent::Args) -> Option<Self> {
        use serde_json::Value;
        // Pointer rows parse their own structured payloads, and those that
        // carry one refuse Null.
        if let Self::App(event) = self {
            if let Some(event) = Self::host_command_args(event, args) {
                return event.map(Self::App);
            }
        }
        if args.is_null() {
            return Some(self.clone());
        }
        let step = || args.as_i64().and_then(|n| isize::try_from(n).ok());
        let index = || args.as_u64().and_then(|n| usize::try_from(n).ok());
        Some(match self {
            Self::App(AppEvent::OnboardingGenerateScript(_)) => Self::App(
                AppEvent::OnboardingGenerateScript(crate::setup::Agent::parse(args.as_str()?)?),
            ),
            Self::App(AppEvent::SkillManagerSyncScroll(_)) => {
                Self::App(AppEvent::SkillManagerSyncScroll(step()?))
            }
            Self::App(AppEvent::SkillManagerPreviewTool(_)) => {
                Self::App(AppEvent::SkillManagerPreviewTool(index()?))
            }
            Self::App(AppEvent::SkillManagerSourceRemoveMove(_)) => {
                Self::App(AppEvent::SkillManagerSourceRemoveMove(step()?))
            }
            // Positions count from 1, as the number keys do.
            Self::Ui(UiAction::AttachSessionByPosition(_)) => Self::Ui(
                UiAction::AttachSessionByPosition(index().filter(|&n| n >= 1)?),
            ),
            Self::Ui(UiAction::DaemonsMoveOverlay(_)) => {
                Self::Ui(UiAction::DaemonsMoveOverlay(step()?))
            }
            Self::Ui(UiAction::DaemonsMoveSelection(_)) => {
                Self::Ui(UiAction::DaemonsMoveSelection(step()?))
            }
            Self::Text(_) => {
                let Value::String(text) = args else {
                    return None;
                };
                let mut chars = text.chars();
                match (chars.next(), chars.next()) {
                    (Some(character), None) => Self::Text(character),
                    _ => return None,
                }
            }
            _ => return None,
        })
    }
}

/// Whether `event` writes outside ainb; see [`KeyAction::writes_outside_ainb`].
///
/// Exhaustive on purpose: a new variant does not compile until someone
/// judges it.
#[allow(clippy::too_many_lines, reason = "one arm per variant, by design")]
const fn app_event_writes_outside_ainb(event: &AppEvent) -> bool {
    match event {
        // Dependency installs and `~/.tmux.conf`.
        AppEvent::OnboardingInstallFocusedDep
        | AppEvent::OnboardingInstallConfig
        // A catalog install runs its `sh -c` recipe.
        | AppEvent::SkillManagerBrowseInstall
        // Skill sync, update and removal write the tools' own skill dirs
        // (`~/.claude`, `~/.codex`, `~/.cursor`), which those tools load.
        | AppEvent::SkillManagerSyncConfirm
        | AppEvent::SkillManagerUpdate
        // `git commit` and `git push` in the user's repository.
        | AppEvent::GitViewCommitConfirm
        | AppEvent::QuickCommitConfirm
        // `tmux rename-session` on a session ainb did not start.
        | AppEvent::OtherTmuxConfirmRename
        => true,
        AppEvent::Quit
        | AppEvent::Plugin { .. }
        | AppEvent::PluginAction { .. }
        | AppEvent::WatchPluginScreen { .. }
        | AppEvent::NavigateTo(..)
        | AppEvent::GoToHomeScreen
        | AppEvent::NextSession
        | AppEvent::PreviousSession
        | AppEvent::NextWorkspace
        | AppEvent::PreviousWorkspace
        | AppEvent::ToggleHelp
        | AppEvent::McpOverlayOpen
        | AppEvent::McpOverlayClose
        | AppEvent::McpOverlayPrev
        | AppEvent::McpOverlayNext
        | AppEvent::McpOverlayRefresh
        | AppEvent::McpOverlayStopServer
        | AppEvent::McpOverlayStopDaemon
        | AppEvent::McpOverlayImport
        | AppEvent::DaemonsRefresh
        | AppEvent::DaemonsRepairHooks
        | AppEvent::DaemonsPinHookBinary
        | AppEvent::RefreshWorkspaces
        | AppEvent::CycleSessionFilter
        | AppEvent::ToggleClaudeChat
        | AppEvent::NewSession
        | AppEvent::SearchWorkspace
        | AppEvent::DetachSession
        | AppEvent::KillContainer
        | AppEvent::ReauthenticateCredentials
        | AppEvent::RestartSession
        | AppEvent::DowngradeHeadroom
        | AppEvent::DeleteSession
        | AppEvent::ResumeSession(..)
        | AppEvent::ResumeSelectedSessions(..)
        | AppEvent::OpenInEditor
        | AppEvent::OpenQuickShell
        | AppEvent::CleanupOrphaned
        | AppEvent::SwitchToLogs
        | AppEvent::SwitchToTerminal
        | AppEvent::GoToTop
        | AppEvent::GoToBottom
        | AppEvent::SwitchPaneFocus
        | AppEvent::Consumed
        | AppEvent::SessionTabNext
        | AppEvent::SessionTabPrev
        | AppEvent::SessionAskSend
        | AppEvent::SessionTabComposerSend
        | AppEvent::SessionStartHangarDaemon
        | AppEvent::SessionListSelectRow { .. }
        | AppEvent::SessionListSelectTab(..)
        | AppEvent::SessionListOpenTranscript(..)
        | AppEvent::SessionListOpenRowMenu { .. }
        | AppEvent::SessionListFocusPane(..)
        | AppEvent::SaveSessionsPaneLayout { .. }
        | AppEvent::SkillManagerFocusPane(..)
        | AppEvent::HomeSidebarSaveWidth { .. }
        | AppEvent::MigrateLayoutWidths { .. }
        | AppEvent::AttachFinished { .. }
        | AppEvent::ShellPrepared { .. }
        | AppEvent::AbtopSetupFinished { .. }
        | AppEvent::InPlaceOpened { .. }
        | AppEvent::ObserverOpened { .. }
        | AppEvent::ObserverFailed { .. }
        | AppEvent::TerminalExited { .. }
        | AppEvent::TerminalInputClosed { .. }
        | AppEvent::InPlaceFailed { .. }
        | AppEvent::PluginActionUndelivered { .. }
        | AppEvent::PluginInputUndelivered { .. }
        | AppEvent::HostDisconnected { .. }
        | AppEvent::Detached
        | AppEvent::EditorFinished { .. }
        | AppEvent::ClipboardFailed { .. }
        | AppEvent::LoginFinished { .. }
        | AppEvent::DaemonActionFinished { .. }
        | AppEvent::PersistFailed { .. }
        | AppEvent::InboxMarkAllRead
        | AppEvent::InboxMarkAllReadFinished { .. }
        | AppEvent::GitReviewSelectRow { .. }
        | AppEvent::GitViewScrollBy(..)
        | AppEvent::HomeSidebarClickItem { .. }
        | AppEvent::NewSessionCancel
        | AppEvent::PickRepoPaste(..)
        | AppEvent::ShowNotification(..)
        | AppEvent::DismissNotifications
        | AppEvent::FileFinderNavigateUp
        | AppEvent::FileFinderNavigateDown
        | AppEvent::FileFinderSelectFile
        | AppEvent::FileFinderCancel
        | AppEvent::ConfirmationToggle
        | AppEvent::ConfirmationPrev
        | AppEvent::ConfirmationConfirm
        | AppEvent::ConfirmationCancel
        | AppEvent::AuthSetupNext
        | AppEvent::AuthSetupPrevious
        | AppEvent::AuthSetupSelect
        | AppEvent::AuthSetupCancel
        | AppEvent::AuthSetupInputChar(..)
        | AppEvent::AuthSetupBackspace
        | AppEvent::AuthSetupCheckStatus
        | AppEvent::AuthSetupRefresh
        | AppEvent::AuthSetupShowCommand
        | AppEvent::ShowGitView
        | AppEvent::GitViewSwitchTab
        | AppEvent::GitViewNextFile
        | AppEvent::GitViewPrevFile
        | AppEvent::GitViewScrollUp
        | AppEvent::GitViewScrollDown
        | AppEvent::GitViewNextCommit
        | AppEvent::GitViewPrevCommit
        | AppEvent::GitViewShowCommitDiff
        | AppEvent::GitViewCommitPush
        | AppEvent::GitViewBack
        | AppEvent::GitCommitAndPush
        | AppEvent::QuickCommitStart
        | AppEvent::QuickCommitInputChar(..)
        | AppEvent::QuickCommitBackspace
        | AppEvent::QuickCommitCursorLeft
        | AppEvent::QuickCommitCursorRight
        | AppEvent::QuickCommitCancel
        | AppEvent::GitViewStartCommit
        | AppEvent::GitViewCommitInputChar(..)
        | AppEvent::GitViewCommitBackspace
        | AppEvent::GitViewCommitCursorLeft
        | AppEvent::GitViewCommitCursorRight
        | AppEvent::GitViewCommitCancel
        | AppEvent::GitCommitSuccess(..)
        | AppEvent::GitViewToggleFolder
        | AppEvent::GitViewExpandAll
        | AppEvent::GitViewCollapseAll
        | AppEvent::GitReviewToggleCollapse
        | AppEvent::GitReviewExpandContext
        | AppEvent::GitReviewNextHunk
        | AppEvent::GitReviewPrevHunk
        | AppEvent::GitReviewNextReviewFile
        | AppEvent::GitReviewPrevReviewFile
        | AppEvent::GitReviewSidebarUp
        | AppEvent::GitReviewSidebarDown
        | AppEvent::GitReviewExpandAllFolders
        | AppEvent::GitReviewCollapseAllFolders
        | AppEvent::AttachTmuxSession
        | AppEvent::EnterInteractivePane
        | AppEvent::DetachTmuxSession
        | AppEvent::ToggleExpandAll
        | AppEvent::ToggleSessionMenuBar
        | AppEvent::OtherTmuxStartRename
        | AppEvent::OtherTmuxRenameChar(..)
        | AppEvent::OtherTmuxRenameBackspace
        | AppEvent::OtherTmuxCancelRename
        | AppEvent::SshSessionStartRename
        | AppEvent::SshSessionRenameChar(..)
        | AppEvent::SshSessionRenameBackspace
        | AppEvent::SshSessionConfirmRename
        | AppEvent::SshSessionCancelRename
        | AppEvent::SessionLabelStartRename
        | AppEvent::SessionLabelRenameChar(..)
        | AppEvent::SessionLabelRenameBackspace
        | AppEvent::SessionLabelConfirmRename
        | AppEvent::SessionLabelCancelRename
        | AppEvent::SessionContextNext
        | AppEvent::SessionContextPrev
        | AppEvent::SessionContextActivate
        | AppEvent::SessionContextCancel
        | AppEvent::HomeScreenSelectTile
        | AppEvent::HomeScreenNavigateUp
        | AppEvent::HomeScreenNavigateDown
        | AppEvent::HomeScreenNavigateLeft
        | AppEvent::HomeScreenNavigateRight
        | AppEvent::HomeScreenSidebarUp
        | AppEvent::HomeScreenSidebarDown
        | AppEvent::HomeScreenSidebarSelect
        | AppEvent::HomeScreenToggleFocus
        | AppEvent::StarSelectedWorkspace
        | AppEvent::WelcomePanelScrollUp
        | AppEvent::WelcomePanelScrollDown
        | AppEvent::WelcomePanelPageUp
        | AppEvent::WelcomePanelPageDown
        | AppEvent::WelcomePanelCopyContent
        | AppEvent::GoToConfig
        | AppEvent::GoToSessionList
        | AppEvent::GoToStats
        | AppEvent::GoToWitr
        | AppEvent::GoToLearnings
        | AppEvent::GoToAbtop
        | AppEvent::GoToSkills
        | AppEvent::GoToSetupMenu
        | AppEvent::GoToLogHistory
        | AppEvent::GoToSkillManager
        | AppEvent::SkillManagerBack
        | AppEvent::SkillManagerDiscoveryImport
        | AppEvent::SkillManagerDiscoveryToggleDetails
        | AppEvent::SkillManagerDiscoverySkip
        | AppEvent::SkillManagerConflictFlip
        | AppEvent::SkillManagerSync
        | AppEvent::SkillManagerSyncCancel
        | AppEvent::SkillManagerSyncScroll(..)
        | AppEvent::SkillManagerSelectPrev
        | AppEvent::SkillManagerSelectNext
        | AppEvent::SkillManagerSelectFirst
        | AppEvent::SkillManagerSelectLast
        | AppEvent::SkillManagerToggleFocus
        | AppEvent::SkillManagerSourceSelectPrev
        | AppEvent::SkillManagerSourceSelectNext
        | AppEvent::SkillManagerApplySourceFilter
        | AppEvent::SkillManagerClearSourceFilter
        | AppEvent::SkillManagerSourceClick { .. }
        | AppEvent::SkillManagerUnitClick { .. }
        | AppEvent::SkillManagerSaveSourcesWidth { .. }
        | AppEvent::SkillManagerRefreshDiscovery
        | AppEvent::SkillManagerCheck
        | AppEvent::SkillManagerRemove
        | AppEvent::SkillManagerOpenAddSource
        | AppEvent::SkillManagerOpenSearch
        | AppEvent::SkillManagerInputChar(..)
        | AppEvent::SkillManagerInputBackspace
        | AppEvent::SkillManagerInputSubmit
        | AppEvent::SkillManagerInputCancel
        | AppEvent::SkillManagerOpenLibrary
        | AppEvent::SkillManagerLibrarySelectPrev
        | AppEvent::SkillManagerLibrarySelectNext
        | AppEvent::SkillManagerLibraryEnter
        | AppEvent::SkillManagerLibraryClose
        | AppEvent::SkillManagerOpenBrowse
        | AppEvent::SkillManagerBrowseInputChar(..)
        | AppEvent::SkillManagerBrowseInputBackspace
        | AppEvent::SkillManagerBrowseSearch
        | AppEvent::SkillManagerBrowseSelectPrev
        | AppEvent::SkillManagerBrowseSelectNext
        | AppEvent::SkillManagerBrowseEditQuery
        | AppEvent::SkillManagerBrowseToggleCatalog
        | AppEvent::SkillManagerBrowseClose
        | AppEvent::SkillManagerPreviewUp
        | AppEvent::SkillManagerPreviewDown
        | AppEvent::SkillManagerPreviewToggle
        | AppEvent::SkillManagerPreviewAll
        | AppEvent::SkillManagerPreviewNone
        | AppEvent::SkillManagerPreviewTool(..)
        | AppEvent::SkillManagerPreviewConfirm
        | AppEvent::SkillManagerPreviewClose
        | AppEvent::SkillManagerPreviewSource
        | AppEvent::SkillManagerApplySourceFilterKey
        | AppEvent::SkillManagerOpenUnitInEditor
        | AppEvent::SkillManagerToggleLibrarySource
        | AppEvent::SkillManagerCopyToLibrary
        | AppEvent::SkillManagerSourceRemoveOpen
        | AppEvent::SkillManagerSourceRemoveMove(..)
        | AppEvent::SkillManagerSourceRemoveConfirm
        | AppEvent::SkillManagerSourceRemoveCancel
        | AppEvent::GoToRecovery
        | AppEvent::GoToDaemons
        | AppEvent::GoToInbox
        | AppEvent::PanelBack
        | AppEvent::GoToHangar
        | AppEvent::ConfigBack
        | AppEvent::ConfigNextCategory
        | AppEvent::ConfigPrevCategory
        | AppEvent::ConfigNextSetting
        | AppEvent::ConfigPrevSetting
        | AppEvent::ConfigSwitchPane
        | AppEvent::ConfigNavigateUp
        | AppEvent::ConfigNavigateDown
        | AppEvent::ConfigFocusCategories
        | AppEvent::ConfigFocusSettings
        | AppEvent::ConfigEditSetting
        | AppEvent::ConfigSaveEdit
        | AppEvent::ConfigCancelEdit
        | AppEvent::ConfigEditChar(..)
        | AppEvent::ConfigEditBackspace
        | AppEvent::ConfigSaveAll
        | AppEvent::ConfigToggleExpand
        | AppEvent::ConfigSearchStart
        | AppEvent::ConfigSearchChar(..)
        | AppEvent::ConfigSearchBackspace
        | AppEvent::ConfigSearchCancel
        | AppEvent::ConfigSecretToKeychain
        | AppEvent::ConfigApiKeyStart
        | AppEvent::ConfigApiKeySave
        | AppEvent::ConfigApiKeyDelete
        | AppEvent::AuthProviderPopupOpen
        | AppEvent::AuthProviderPopupClose
        | AppEvent::AuthProviderPopupNext
        | AppEvent::AuthProviderPopupPrev
        | AppEvent::AuthProviderPopupSelect
        | AppEvent::AuthProviderPopupInputChar(..)
        | AppEvent::AuthProviderPopupBackspace
        | AppEvent::AuthProviderPopupDeleteKey
        | AppEvent::ConfigPopupNavigateUp
        | AppEvent::ConfigPopupNavigateDown
        | AppEvent::ConfigPopupConfirm
        | AppEvent::ConfigPopupCancel
        | AppEvent::ConfigPopupInputChar(..)
        | AppEvent::ConfigPopupBackspace
        | AppEvent::ConfigPopupPaste(..)
        | AppEvent::ConfigPopupPasteClipboard
        | AppEvent::ConfigPopupDelete
        | AppEvent::ConfigPopupCursorLeft
        | AppEvent::ConfigPopupCursorRight
        | AppEvent::ConfigPopupCursorHome
        | AppEvent::ConfigPopupCursorEnd
        | AppEvent::LogHistoryBack
        | AppEvent::LogHistoryNextSession
        | AppEvent::LogHistoryPrevSession
        | AppEvent::LogHistorySelectSession
        | AppEvent::LogHistoryToggleFocus
        | AppEvent::LogHistoryScrollUp
        | AppEvent::LogHistoryScrollDown
        | AppEvent::LogHistoryPageUp
        | AppEvent::LogHistoryPageDown
        | AppEvent::LogHistoryCycleFilter
        | AppEvent::LogHistoryRefresh
        | AppEvent::LogHistoryCopySelection
        | AppEvent::LogHistoryScrollLeft
        | AppEvent::LogHistoryScrollRight
        | AppEvent::LogHistoryScrollHome
        | AppEvent::LogHistoryCleanup
        | AppEvent::OnboardingNext
        | AppEvent::OnboardingBack
        | AppEvent::OnboardingToMenu
        | AppEvent::OnboardingInputChar(..)
        | AppEvent::OnboardingBackspace
        | AppEvent::OnboardingDelete
        | AppEvent::OnboardingCursorLeft
        | AppEvent::OnboardingCursorRight
        | AppEvent::OnboardingCursorHome
        | AppEvent::OnboardingCursorEnd
        | AppEvent::OnboardingCheckDeps
        | AppEvent::OnboardingSkipAuth
        | AppEvent::OnboardingAuthUp
        | AppEvent::OnboardingAuthDown
        | AppEvent::OnboardingAuthSelect
        | AppEvent::OnboardingAuthKeyChar(..)
        | AppEvent::OnboardingAuthKeyBackspace
        | AppEvent::OnboardingAuthCancel
        | AppEvent::OnboardingEditorUp
        | AppEvent::OnboardingEditorDown
        | AppEvent::OnboardingQuestionUp
        | AppEvent::OnboardingQuestionDown
        | AppEvent::OnboardingFinish
        | AppEvent::OnboardingDepCursorUp
        | AppEvent::OnboardingDepCursorDown
        | AppEvent::OnboardingScriptPrompt
        | AppEvent::OnboardingCancelScriptPrompt
        | AppEvent::OnboardingGenerateScript(..)
        | AppEvent::OnboardingOtelChar(..)
        | AppEvent::OnboardingOtelBackspace
        | AppEvent::OnboardingOtelNextField
        | AppEvent::OnboardingOtelPrevField
        | AppEvent::SetupMenuBack
        | AppEvent::SetupMenuSelect
        | AppEvent::SetupMenuUp
        | AppEvent::SetupMenuDown
        | AppEvent::StartOnboarding
        | AppEvent::FactoryReset
        | AppEvent::ShowChangelog
        | AppEvent::ChangelogBack
        | AppEvent::UsageWireStatusline
        | AppEvent::SkillsBack
        | AppEvent::SkillsNextProvider
        | AppEvent::SkillsPrevProvider
        | AppEvent::SkillsNextTab
        | AppEvent::SkillsPrevTab
        | AppEvent::SkillsScrollUp
        | AppEvent::SkillsScrollDown
        | AppEvent::SkillsPageUp
        | AppEvent::SkillsPageDown
        | AppEvent::SkillsToTop
        | AppEvent::SkillsToBottom
        | AppEvent::SkillsRefresh
        | AppEvent::SkillsSearchStart
        | AppEvent::SkillsSearchChar(..)
        | AppEvent::SkillsSearchBackspace
        | AppEvent::SkillsSearchClose
        | AppEvent::SessionRecoveryBack
        | AppEvent::SessionRecoveryNext
        | AppEvent::SessionRecoveryPrev
        | AppEvent::SessionRecoveryResume
        | AppEvent::SessionRecoveryArchive
        | AppEvent::SessionRecoveryRefresh
        | AppEvent::SessionRecoveryToggleView
        | AppEvent::SessionRecoveryRecoverAll
        | AppEvent::SessionRecoveryToggleSelect
        | AppEvent::SessionRecoveryDeleteSelected
        | AppEvent::SessionRecoverySearchStart
        | AppEvent::SessionRecoverySearchChar(..)
        | AppEvent::SessionRecoverySearchBackspace
        | AppEvent::SessionRecoverySearchClose
        | AppEvent::SessionRecoverySearchCancel
        | AppEvent::ToggleSelectSession
        | AppEvent::DeleteSelectedSessions
        | AppEvent::ConfigureLaunch(..)
        | AppEvent::ConfigureBack
        | AppEvent::ConfigureOpenPresetManager
        | AppEvent::ConfigureOpenBranchPicker
        | AppEvent::ConfigureInitRemoteRepo
        => false,
    }
}

/// Whether `action` writes outside ainb; see [`KeyAction::writes_outside_ainb`].
///
/// Exhaustive on purpose: a new variant does not compile until someone
/// judges it.
const fn ui_action_writes_outside_ainb(action: &UiAction) -> bool {
    match action {
        // `~/.claude/settings.json`.
        UiAction::UsageWireStatusline
        // Copying and removing skills write the tools' own skill dirs.
        | UiAction::SkillManagerCopyToLibraryIfFocused
        | UiAction::SkillManagerRemoveOrSource
        // `s` previews a sync through the same `ainb_cli` dispatch that
        // applies one; the walk cannot tell the dry run apart, so it is
        // judged with the apply.
        | UiAction::SkillManagerSyncOrConflict
        => true,
        UiAction::Scroll(..)
        | UiAction::SessionComposerEnter
        | UiAction::SessionComposerBackspace
        | UiAction::SessionComposerEscape
        | UiAction::SessionComposerUp
        | UiAction::SessionComposerDown
        | UiAction::SessionComposerFocusToggle
        | UiAction::SessionComposerRetry
        | UiAction::SessionComposerCancel
        | UiAction::PalCycleEngine
        | UiAction::PalCycleModel
        | UiAction::PalCycleMode
        | UiAction::PalRetry
        | UiAction::SessionAskPrevious
        | UiAction::SessionAskNext
        | UiAction::SessionAskBackspace
        | UiAction::SessionAskClear
        | UiAction::SkillManagerShrinkSources
        | UiAction::SkillManagerGrowSources
        | UiAction::DaemonsCloseOverlay
        | UiAction::DaemonsCloseAndBack
        | UiAction::DaemonsConfirmMenu
        | UiAction::DaemonsOpenMenu
        | UiAction::DaemonsMoveOverlay(..)
        | UiAction::DaemonsMoveSelection(..)
        | UiAction::SkillManagerOpenUnitIfFocused
        | UiAction::SkillManagerBackOrClearFilter
        | UiAction::SessionActivateSelected
        | UiAction::SessionResumeSelected
        | UiAction::SessionStartRename
        | UiAction::SessionHeadroomOrHelp
        | UiAction::AttachSessionByPosition(..)
        | UiAction::ToggleSessionsSidebar
        => false,
    }
}

/// One discoverable, overrideable row in the keymap.
#[derive(Debug, Clone)]
pub struct Binding {
    pub id: &'static str,
    pub ctx: KeyContext,
    /// The key that runs this row, or `None` for a command reachable only by
    /// name (from a palette or a click). Unbound rows never match a key press.
    pub chord: Option<Chord>,
    pub action: KeyAction,
    pub doc: &'static str,
}

impl Binding {
    /// Whether this row runs only from its key, never from
    /// `Intent::Command` (#1080): its action writes outside ainb, so no other
    /// surface (a desktop webview, a mirror, a web client) may fire it by
    /// name. Derived from the action, so no row can carry a writer without
    /// the flag.
    #[must_use]
    pub fn key_only(&self) -> bool {
        self.action.writes_outside_ainb()
    }
}

/// Immutable resolved table. No dispatch code stores a mutable binding map.
#[derive(Debug, Clone)]
pub struct Keymap {
    bindings: Vec<Binding>,
    by_chord: HashMap<(KeyContext, Chord), usize>,
}

/// Override application failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverrideError(pub String);

impl fmt::Display for OverrideError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for OverrideError {}

impl Keymap {
    /// Construct the built-in table.
    #[must_use]
    pub fn defaults() -> Self {
        Self::new(crate::app::keymap_defaults::defaults()).expect("default keymap is valid")
    }

    /// Check a list of rows once, before key events begin.
    pub fn new(bindings: Vec<Binding>) -> Result<Self, OverrideError> {
        let mut by_chord = HashMap::new();
        for (index, binding) in bindings.iter().enumerate() {
            if binding.doc.trim().is_empty() {
                return Err(OverrideError(format!(
                    "binding `{}` has no documentation",
                    binding.id
                )));
            }
            let Some(chord) = &binding.chord else {
                continue;
            };
            if by_chord.insert((binding.ctx.clone(), chord.clone()), index).is_some() {
                return Err(OverrideError(format!(
                    "duplicate key `{}` in [{}]",
                    chord.as_str(),
                    binding.ctx.name()
                )));
            }
        }
        Ok(Self { bindings, by_chord })
    }

    /// Resolve first matching active context. Context order is the priority rule.
    #[must_use]
    pub fn resolve(&self, active: &[KeyContext], chord: &Chord) -> Option<KeyAction> {
        self.resolve_with_context(active, chord).map(|(_, action)| action)
    }

    /// Resolve and retain the matching context for text-input routing.
    #[must_use]
    pub fn resolve_with_context(
        &self,
        active: &[KeyContext],
        chord: &Chord,
    ) -> Option<(KeyContext, KeyAction)> {
        active.iter().find_map(|context| {
            let binding = self
                .by_chord
                .get(&(context.clone(), chord.clone()))
                .map(|index| &self.bindings[*index]);
            binding
                .map(|binding| (binding.ctx.clone(), binding.action.clone()))
                .or_else(|| {
                    (context == &KeyContext::TextInput)
                        .then(|| {
                            chord
                                .printable()
                                .map(|character| (context.clone(), KeyAction::Text(character)))
                        })
                        .flatten()
                })
        })
    }

    /// All rows, stable default order, for docs and the CLI.
    pub fn bindings(&self) -> impl Iterator<Item = &Binding> {
        self.bindings.iter()
    }

    /// Whether `chord` releases the in-place pane.
    ///
    /// While the pane is live a host sends every key to its client except this
    /// one, which it turns into the `embed_interactive.detach` command, so the
    /// reducer still decides the release.
    #[must_use]
    pub fn releases_in_place_pane(&self, chord: &Chord) -> bool {
        self.binding_for(&KeyContext::EmbedInteractive, EMBED_DETACH_ROW)
            .is_some_and(|row| row.chord.as_ref() == Some(chord))
    }

    /// Locate a named row for tests and TOML override validation.
    #[must_use]
    pub fn binding_for(&self, ctx: &KeyContext, id: &str) -> Option<&Binding> {
        self.bindings.iter().find(|binding| binding.ctx == *ctx && binding.id == id)
    }

    /// The command registry: every row, addressable by its [`CommandId`].
    ///
    /// Palettes list it and clicks invoke from it, so it is the same table key
    /// presses resolve through, overrides included.
    pub fn commands(&self) -> impl Iterator<Item = (CommandId, &Binding)> {
        self.bindings.iter().map(|binding| (CommandId::of(binding), binding))
    }

    /// Whether the row `id` runs only from its key; see [`Binding::key_only`].
    #[must_use]
    pub fn is_key_only(&self, id: &CommandId) -> bool {
        self.command(id).is_some_and(Binding::key_only)
    }

    /// Look a command up by id. `None` for ids no row carries.
    #[must_use]
    pub fn command(&self, id: &CommandId) -> Option<&Binding> {
        // ponytail: linear scan of a few hundred rows per palette or click
        // invocation; index by id if commands ever run per frame.
        self.commands()
            .find_map(|(candidate, binding)| (candidate == *id).then_some(binding))
    }
    /// Load the conventional override file, preserving defaults on every error.
    #[must_use]
    pub fn load_user() -> (Self, Option<String>) {
        let defaults = Self::defaults();
        let Some(path) = crate::app::keymap_toml::KeymapOverrides::default_path() else {
            return (defaults, None);
        };
        match crate::app::keymap_toml::KeymapOverrides::from_path(&path).and_then(|overrides| {
            defaults.clone().with_overrides(&overrides).map_err(|error| error.to_string())
        }) {
            Ok(keymap) => (keymap, None),
            Err(error) => (
                defaults,
                Some(format!("Ignoring {}: {error}", path.display())),
            ),
        }
    }

    /// Overlay valid user selections without changing actions or contexts.
    pub fn with_overrides(
        mut self,
        overrides: &crate::app::keymap_toml::KeymapOverrides,
    ) -> Result<Self, OverrideError> {
        let mut replacements = Vec::new();
        for override_row in overrides.rows() {
            let Some(context) = KeyContext::from_name(&override_row.context) else {
                tracing::warn!(context = %override_row.context, "ignoring unknown keymap context");
                continue;
            };
            if context == KeyContext::EmbedInteractive {
                return Err(OverrideError(
                    "interactive embed bindings are terminal-owned and cannot be overridden"
                        .to_string(),
                ));
            }
            let Some(index) = self
                .bindings
                .iter()
                .position(|binding| binding.ctx == context && binding.id == override_row.event)
            else {
                tracing::warn!(
                    context = %override_row.context,
                    event = %override_row.event,
                    "ignoring unknown keymap event"
                );
                continue;
            };
            if self.bindings[index].action.carries_payload() {
                return Err(OverrideError(format!(
                    "payload-bearing binding `{}` in [{}] cannot be overridden",
                    override_row.event,
                    context.name(),
                )));
            }
            let chord = Chord::parse(&override_row.chord)
                .map_err(|error| OverrideError(error.to_string()))?;
            replacements.push((index, context, override_row.event.clone(), chord));
        }

        // Resolve all target rows before removing displaced defaults. This lets
        // two overrides exchange chords without erasing either target row.
        let mut bindings =
            std::mem::take(&mut self.bindings).into_iter().enumerate().collect::<Vec<_>>();
        bindings.retain(|(index, binding)| {
            let is_override_target = replacements.iter().any(|(target, _, _, _)| index == target);
            is_override_target
                || !replacements.iter().any(|(_, context, event, chord)| {
                    binding.ctx == *context
                        && binding.chord.as_ref() == Some(chord)
                        && binding.id != event.as_str()
                })
        });
        for (index, _, _, chord) in replacements {
            let binding = bindings
                .iter_mut()
                .find(|(binding_index, _)| *binding_index == index)
                .expect("validated override target remains in keymap");
            binding.1.chord = Some(chord);
        }
        self.bindings = bindings.into_iter().map(|(_, binding)| binding).collect();

        Self::new(self.bindings)
    }
}

/// Stable name of a keymap row: `<context>.<row id>`.
///
/// For example `session_list.attach_session` or `global.toggle_help`. The
/// context half is the `[section]` a `keymap.toml` override names, so a
/// command id and an override address the same row.
#[derive(
    Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(transparent)]
pub struct CommandId(String);

impl CommandId {
    /// Wrap a command name. Unknown names are allowed and resolve to nothing.
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }

    fn of(binding: &Binding) -> Self {
        Self(format!("{}.{}", binding.ctx.name(), binding.id))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for CommandId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod onboarding_context_coverage {
    use super::*;
    use crate::components::onboarding::{AuthAgent, AuthPane, OnboardingStep};

    /// Every sub-context the wizard can put the resolver in must have rows.
    ///
    /// Four did not. `active_contexts` pushed `welcome`, `summary`, `editor` and
    /// `dependency_ready`, and the default table had no bindings under any of
    /// them, so on those screens every key fell through to Global: `Enter` on
    /// the first screen of a first run did nothing at all.
    ///
    /// The parity fixture could not catch it, because it was generated from the
    /// table rather than from the screens, so a context with no rows produced no
    /// rows to compare. This walks the steps instead, which is the direction
    /// that fails when a screen is dropped.
    #[test]
    fn every_onboarding_step_resolves_to_a_context_with_bindings() {
        let keymap = Keymap::defaults();
        let with_rows: std::collections::HashSet<KeyContext> =
            keymap.bindings().map(|binding| binding.ctx.clone()).collect();

        let panes = [
            AuthPane::AgentList,
            AuthPane::MethodPicker {
                agent: AuthAgent::Claude,
                cursor: 0,
            },
            AuthPane::KeyEntry {
                agent: AuthAgent::Claude,
                buf: String::new(),
            },
        ];
        // The shipped list, not a copy of it. A hand-written ten would go stale
        // the moment an eleventh step lands, which is the exact failure this
        // test exists to catch.
        let mut missing = Vec::new();
        for step in OnboardingStep::all() {
            for pane in &panes {
                for agent_pick_open in [false, true] {
                    for checked in [false, true] {
                        let sub = onboarding_sub_context(step, pane, agent_pick_open, checked);
                        let ctx = KeyContext::Screen(
                            crate::app::screens::ids::ONBOARDING,
                            SubContext::Named(sub),
                        );
                        if !with_rows.contains(&ctx) && !missing.contains(&sub) {
                            missing.push(sub);
                        }
                    }
                }
            }
        }

        assert!(
            missing.is_empty(),
            "onboarding sub-contexts the wizard can reach with no bindings at all: {missing:?}. \
             Every key on those screens falls through to Global."
        );
    }

    /// The two screens whose `Enter` is not `OnboardingNext`, pinned by name so a
    /// future table edit cannot quietly make Summary advance instead of finish.
    #[test]
    fn enter_advances_the_wizard_and_finishes_it_on_summary() {
        let keymap = Keymap::defaults();
        let enter = Chord::parse("enter").expect("enter parses");
        let resolve = |sub: &'static str| {
            let ctx =
                KeyContext::Screen(crate::app::screens::ids::ONBOARDING, SubContext::Named(sub));
            match keymap.resolve(&[ctx], &enter) {
                Some(KeyAction::App(event)) => format!("{event:?}"),
                other => panic!("enter on onboarding.{sub} resolved to {other:?}"),
            }
        };

        assert_eq!(resolve("welcome"), "OnboardingNext");
        assert_eq!(resolve("editor"), "OnboardingNext");
        assert_eq!(resolve("dependency_ready"), "OnboardingNext");
        assert_eq!(resolve("dependency"), "OnboardingCheckDeps");
        assert_eq!(resolve("summary"), "OnboardingFinish");
    }
}
