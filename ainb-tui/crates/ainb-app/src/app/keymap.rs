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
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Chord(String);

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
    SkillManagerOverlay,
    ConfigPopup,
    AuthProviderPopup,
    TextInput,
    Screen(&'static str, SubContext),
    Global,
}

/// Terminal-host state that is intentionally outside `AppState` until Phase 3.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HostFlags {
    pub embed_interactive: bool,
    pub preview_scroll_mode: bool,
}

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
            Self::SkillManagerOverlay => "skill_manager_overlay".to_string(),
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
            "skill_manager_overlay" => Self::SkillManagerOverlay,
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

/// Mirror host dispatch precedence without allowing renderer state into `AppState`.
#[must_use]
pub fn active_contexts(state: &AppState, host: &HostFlags) -> Vec<KeyContext> {
    let mut contexts = Vec::new();
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

    if host.embed_interactive {
        contexts.push(KeyContext::EmbedInteractive);
    }
    if host.preview_scroll_mode {
        contexts.push(KeyContext::PreviewScroll);
    }
    if state.shell.confirmation_dialog.is_some() {
        contexts.push(KeyContext::ConfirmDialog);
    }
    if state.mcp_pool.mcp_overlay.is_some() {
        contexts.push(KeyContext::McpOverlay);
    }
    if state.tmux.other_tmux_rename_mode {
        contexts.push(KeyContext::OtherTmuxRename);
    }
    if state.ssh.ssh_session_rename_mode {
        contexts.push(KeyContext::SshRename);
    }
    if state.session_labels.session_label_rename_mode {
        contexts.push(KeyContext::SessionRename);
    }
    if state.session_labels.session_context_menu.is_some() {
        contexts.push(KeyContext::SessionContextMenu);
    }
    if state.shell.help_visible {
        if text_input_active {
            contexts.push(KeyContext::Screen("help", SubContext::Named("text")));
        } else {
            contexts.push(KeyContext::HelpVisible);
        }
    }
    if state.has_visible_notifications() {
        contexts.push(KeyContext::Screen(
            "notifications",
            SubContext::Named("visible"),
        ));
    }
    if state.is_in_quick_commit_mode() {
        contexts.push(KeyContext::QuickCommit);
    }
    if state.shell.current_screen == screen_ids::SESSION_LIST {
        use crate::components::session_tabs::{SessionTab, resolve};

        match resolve(state, state.shell.session_tab) {
            SessionTab::Ask => contexts.push(KeyContext::Screen(
                screen_ids::SESSION_LIST,
                SubContext::Named("ask"),
            )),
            SessionTab::Thread | SessionTab::Pal if state.session_tab_owns_keys() => contexts.push(
                KeyContext::Screen(screen_ids::SESSION_LIST, SubContext::Named("composer")),
            ),
            _ => {}
        }
    }
    if state.onboarding.auth_provider_popup_state.show_popup {
        if state.onboarding.auth_provider_popup_state.is_entering_key {
            contexts.push(KeyContext::Screen(
                "auth_provider_popup",
                SubContext::Named("input"),
            ));
            // Text ownership precedes the popup's `d` delete shortcut. This
            // keeps a typed `d` in an API key from deleting the stored key.
            contexts.push(KeyContext::TextInput);
            text_context_pushed = true;
        }
        contexts.push(KeyContext::AuthProviderPopup);
    }
    if state.config.config_popup_state.show_popup {
        if state.config.config_popup_state.is_text_entry() {
            contexts.push(KeyContext::Screen(
                "config_popup",
                SubContext::Named("input"),
            ));
            // Text ownership precedes the popup's j/k navigation rows.
            contexts.push(KeyContext::TextInput);
            text_context_pushed = true;
        }
        contexts.push(KeyContext::ConfigPopup);
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
                    contexts.push(KeyContext::Screen(screen, SubContext::Named("overlay")));
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
                    contexts.push(KeyContext::Screen(
                        screen,
                        SubContext::Named("sync_confirm"),
                    ));
                } else if skills.source_remove_confirm.is_some() {
                    contexts.push(KeyContext::Screen(
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
                let sub = if state.onboarding.setup_menu_state.showing_confirmation {
                    "confirm"
                } else {
                    "menu"
                };
                contexts.push(KeyContext::Screen(screen, SubContext::Named(sub)));
            }
            screen_ids::DAEMONS => {
                let sub = if state.hangar.daemons_state.has_overlay() {
                    "overlay"
                } else {
                    "list"
                };
                contexts.push(KeyContext::Screen(screen, SubContext::Named(sub)));
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
    contexts
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
    fn carries_payload(&self) -> bool {
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
}

/// One discoverable, overrideable row in the keymap.
#[derive(Debug, Clone)]
pub struct Binding {
    pub id: &'static str,
    pub ctx: KeyContext,
    pub chord: Chord,
    pub action: KeyAction,
    pub doc: &'static str,
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
            let key = (binding.ctx.clone(), binding.chord.clone());
            if by_chord.insert(key, index).is_some() {
                return Err(OverrideError(format!(
                    "duplicate key `{}` in [{}]",
                    binding.chord.as_str(),
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

    /// Locate a named row for tests and TOML override validation.
    #[must_use]
    pub fn binding_for(&self, ctx: &KeyContext, id: &str) -> Option<&Binding> {
        self.bindings.iter().find(|binding| binding.ctx == *ctx && binding.id == id)
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
                        && binding.chord == *chord
                        && binding.id != event.as_str()
                })
        });
        for (index, _, _, chord) in replacements {
            let binding = bindings
                .iter_mut()
                .find(|(binding_index, _)| *binding_index == index)
                .expect("validated override target remains in keymap");
            binding.1.chord = chord;
        }
        self.bindings = bindings.into_iter().map(|(_, binding)| binding).collect();

        Self::new(self.bindings)
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
