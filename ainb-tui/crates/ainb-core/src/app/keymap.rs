//! Data model shared by terminal key dispatch, the keymap CLI, and generated docs.

use std::collections::HashMap;
use std::fmt;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::events::AppEvent;
use super::screens::ids as screen_ids;
use super::state::{AppState, FocusedPane};

/// Key-code variants used by legacy event tests.
///
/// Kept test-only so production host dispatch remains entirely chord based.
#[cfg(test)]
pub(crate) mod test_key_codes {
    pub(crate) use crossterm::event::KeyCode::*;
}

/// A terminal-normalised key chord.
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

impl Chord {
    /// Parse the wire spelling used by `keymap.toml`.
    pub fn parse(input: &str) -> Result<Self, ChordParseError> {
        let input = input.trim();
        if input.is_empty() {
            return Err(ChordParseError("key chord cannot be empty".to_string()));
        }

        let keys = input
            .split_ascii_whitespace()
            .map(Self::parse_key)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self(keys.join(" ")))
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

    /// Convert the only terminal event type at the input boundary.
    #[must_use]
    pub fn from_key_event(event: &KeyEvent) -> Self {
        let mut modifiers = Vec::new();
        if event.modifiers.contains(KeyModifiers::CONTROL) {
            modifiers.push("ctrl");
        }
        if event.modifiers.contains(KeyModifiers::ALT) {
            modifiers.push("alt");
        }
        if event.modifiers.contains(KeyModifiers::SHIFT) {
            modifiers.push("shift");
        }
        let key = match event.code {
            KeyCode::Backspace => "backspace".to_string(),
            KeyCode::Enter => "enter".to_string(),
            KeyCode::Left => "left".to_string(),
            KeyCode::Right => "right".to_string(),
            KeyCode::Up => "up".to_string(),
            KeyCode::Down => "down".to_string(),
            KeyCode::Home => "home".to_string(),
            KeyCode::End => "end".to_string(),
            KeyCode::PageUp => "pageup".to_string(),
            KeyCode::PageDown => "pagedown".to_string(),
            KeyCode::Tab => "tab".to_string(),
            KeyCode::BackTab => "tab".to_string(),
            KeyCode::Delete => "delete".to_string(),
            KeyCode::Insert => "insert".to_string(),
            KeyCode::F(number) => format!("f{number}"),
            KeyCode::Esc => "esc".to_string(),
            KeyCode::Char(' ') => "space".to_string(),
            KeyCode::Char('+') => "plus".to_string(),
            KeyCode::Char(character) => character.to_string(),
            other => format!("{other:?}").to_ascii_lowercase(),
        };
        // Crossterm may report SHIFT for a printable glyph (for example `:`).
        // The glyph already carries that intent, so storing `shift+:` would
        // make terminal layout details leak into the user-facing table.
        if matches!(&event.code, KeyCode::Char(_)) {
            modifiers.retain(|modifier| *modifier != "shift");
        }
        let source = modifiers
            .into_iter()
            .chain(std::iter::once(key.as_str()))
            .collect::<Vec<_>>()
            .join("+");
        // Every crossterm spelling above is a supported wire key.
        Self::parse(&source).expect("crossterm key normalisation is valid")
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
            "setup_menu.menu" => Self::Screen("setup_menu", SubContext::Named("menu")),
            "setup_menu.confirm" => Self::Screen("setup_menu", SubContext::Named("confirm")),
            "daemons.overlay" => Self::Screen("daemons", SubContext::Named("overlay")),
            "daemons.list" => Self::Screen("daemons", SubContext::Named("list")),
            _ => return None,
        })
    }
}

/// Mirror host dispatch precedence without allowing renderer state into `AppState`.
#[must_use]
pub fn active_contexts(state: &AppState, host: &HostFlags) -> Vec<KeyContext> {
    let mut contexts = Vec::new();
    let mut text_context_pushed = false;
    let text_input_active = crate::app::events::EventHandler::is_in_text_input_context(state);
    let auth_setup_api_input = state.current_screen == screen_ids::AUTH_SETUP
        && state
            .auth_setup_state
            .as_ref()
            .is_some_and(|auth| auth.selected_method == crate::app::state::AuthMethod::ApiKey);
    // Attached-terminal text is terminal-owned, while the auth picker only
    // accepts text after the API-key method has been selected. Neither may
    // consume host-owned table rows through the generic text fallback.
    let table_text_input_active = text_input_active
        && state.current_screen != screen_ids::ATTACHED_TERMINAL
        && (state.current_screen != screen_ids::AUTH_SETUP || auth_setup_api_input);
    let plugin_screen_active =
        crate::app::screens::builtin::plugin_id_for_screen(&state.current_screen).is_some();

    if host.embed_interactive {
        contexts.push(KeyContext::EmbedInteractive);
    }
    if host.preview_scroll_mode {
        contexts.push(KeyContext::PreviewScroll);
    }
    if state.confirmation_dialog.is_some() {
        contexts.push(KeyContext::ConfirmDialog);
    }
    if state.mcp_overlay.is_some() {
        contexts.push(KeyContext::McpOverlay);
    }
    if state.other_tmux_rename_mode {
        contexts.push(KeyContext::OtherTmuxRename);
    }
    if state.ssh_session_rename_mode {
        contexts.push(KeyContext::SshRename);
    }
    if state.session_label_rename_mode {
        contexts.push(KeyContext::SessionRename);
    }
    if state.session_context_menu.is_some() {
        contexts.push(KeyContext::SessionContextMenu);
    }
    if state.help_visible {
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
    if state.current_screen == screen_ids::SESSION_LIST {
        use crate::components::session_tabs::{SessionTab, resolve};

        match resolve(state, state.session_tab) {
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
    if state.auth_provider_popup_state.show_popup {
        if state.auth_provider_popup_state.is_entering_key {
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
    if state.config_popup_state.show_popup {
        if state.config_popup_state.is_text_entry() {
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

    let screen = match state.current_screen.as_str() {
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
            screen_ids::SESSION_LIST => match state.focused_pane {
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
                let focus = format!("{:?}", state.home_screen_v2_state.focus);
                let sub = if focus == "Sidebar" {
                    "sidebar"
                } else {
                    "content"
                };
                contexts.push(KeyContext::Screen(screen, SubContext::Named(sub)));
            }
            screen_ids::CONFIG => {
                if state.config_screen_state.api_key_input_mode {
                    contexts.push(KeyContext::Screen(screen, SubContext::Named("api_key")));
                } else if state.config_screen_state.editing {
                    contexts.push(KeyContext::Screen(screen, SubContext::Named("editing")));
                } else if state.config_screen_state.is_searching() {
                    contexts.push(KeyContext::Screen(screen, SubContext::Named("search")));
                }
            }
            screen_ids::GIT_VIEW => {
                if let Some(git) = &state.git_view_state {
                    if git.is_in_commit_mode() {
                        contexts.push(KeyContext::Screen(screen, SubContext::Named("commit")));
                    } else if matches!(git.active_tab, crate::components::git_view::GitTab::Review)
                    {
                        contexts.push(KeyContext::Screen(screen, SubContext::Named("review")));
                    }
                }
            }
            screen_ids::LOG_HISTORY => {
                let sub = match state.log_history_state.focus {
                    crate::components::log_history_viewer::LogViewerFocus::SessionList => {
                        "sessions"
                    }
                    crate::components::log_history_viewer::LogViewerFocus::LogEntries => "logs",
                };
                contexts.push(KeyContext::Screen(screen, SubContext::Named(sub)));
            }
            screen_ids::SESSION_RECOVERY => {
                if state.session_recovery_state.recovery_overlay.is_some() {
                    contexts.push(KeyContext::Screen(screen, SubContext::Named("overlay")));
                } else if state.session_recovery_state.search_active {
                    contexts.push(KeyContext::Screen(screen, SubContext::Named("search")));
                } else if !state.session_recovery_state.search_query.is_empty() {
                    contexts.push(KeyContext::Screen(screen, SubContext::Named("filtered")));
                }
            }
            screen_ids::SKILLS if state.skills_state.search_active => {
                contexts.push(KeyContext::Screen(screen, SubContext::Named("search")));
            }
            screen_ids::SKILL_MANAGER => {
                use crate::components::skill_manager_screen::{BrowseMode, FocusedSkillPane};

                let skills = &state.skill_manager_state;
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
                if state.auth_setup_state.is_some() {
                    if auth_setup_api_input {
                        contexts.push(KeyContext::Screen(screen, SubContext::Named("input")));
                    } else {
                        contexts.push(KeyContext::Screen(screen, SubContext::Named("picker")));
                    }
                }
            }
            screen_ids::ONBOARDING => {
                use crate::components::onboarding::{AuthPane, OnboardingStep};

                if let Some(onboarding) = &state.onboarding_state {
                    let sub = match onboarding.current_step {
                        OnboardingStep::GitDirectories => "git_directories",
                        OnboardingStep::Source | OnboardingStep::Role | OnboardingStep::UseCase => {
                            "questions"
                        }
                        OnboardingStep::DependencyCheck if onboarding.agent_pick_open => {
                            "dependency_agent"
                        }
                        OnboardingStep::DependencyCheck
                            if onboarding.dependency_status.is_some() =>
                        {
                            "dependency_ready"
                        }
                        OnboardingStep::DependencyCheck => "dependency",
                        OnboardingStep::Authentication => match onboarding.auth_pane {
                            AuthPane::KeyEntry { .. } => "auth_key",
                            AuthPane::MethodPicker { .. } => "auth_method",
                            AuthPane::AgentList => "auth_agents",
                        },
                        OnboardingStep::OtelSetup => "otel",
                        OnboardingStep::EditorSelection => "editor",
                        OnboardingStep::Summary => "summary",
                        OnboardingStep::Welcome => "welcome",
                    };
                    contexts.push(KeyContext::Screen(screen, SubContext::Named(sub)));
                }
            }
            screen_ids::SETUP_MENU => {
                let sub = if state.setup_menu_state.showing_confirmation {
                    "confirm"
                } else {
                    "menu"
                };
                contexts.push(KeyContext::Screen(screen, SubContext::Named(sub)));
            }
            screen_ids::DAEMONS => {
                let sub = if state.daemons_state.has_overlay() {
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

    let ask_free_text = state.current_screen == screen_ids::SESSION_LIST
        && matches!(
            crate::components::session_tabs::resolve(state, state.session_tab),
            crate::components::session_tabs::SessionTab::Ask
        )
        && state.ask_state.focus() == crate::fleet::answer::AskFocus::FreeText;
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

/// UI-owned command declared in the table while Phase 3 still owns mutation in main.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiAction {
    PreviewScrollUp,
    PreviewScrollDown,
    PreviewPageUp,
    PreviewPageDown,
    PreviewExitScroll,
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
            let Some(mut index) = self
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
            // A user override owns its requested chord. If that chord was a
            // default for another action in the same context, drop the old
            // row from the effective map rather than rejecting the override.
            // This is what makes `[session_list] attach = "o"` move attach
            // off Enter and replace the former `o` action.
            if let Some(other) = self.bindings.iter().position(|binding| {
                binding.ctx == context && binding.chord == chord && binding.id != override_row.event
            }) {
                self.bindings.remove(other);
                if other < index {
                    index -= 1;
                }
            }
            self.bindings[index].chord = chord;
        }
        Self::new(self.bindings)
    }
}
