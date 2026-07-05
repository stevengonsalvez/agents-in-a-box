//! The card provider-agent kind (`claude` / `codex` / `copilot`) and the
//! task-create default cascade (spec F4).
//!
//! A card runs on ONE provider backend, chosen in the create overlay's agent
//! picker (spec F1). That choice is a distinct axis from the assignee *profile*
//! (which selects an agent-row / persona): [`AgentKind`] names the runner
//! backend the daemon dispatches through. Three backends exist in the picker;
//! `claude` + `codex` dispatch today, `copilot` is selectable but its dispatch
//! returns a clear "provider not yet wired" error (spec F8, D7).
//!
//! # The default cascade (F4)
//!
//! When the overlay opens, the agent picker pre-selects a default resolved from
//! four sources, most-specific first:
//!
//! ```text
//! last-used ──▶ board default ──▶ workspace default ──▶ global config
//! ```
//!
//! [`resolve_agent_cascade`] walks that order and returns the first source that
//! carries a value, falling back to [`AgentKind::DEFAULT`] (`claude`) when every
//! source is empty (a fresh install with no history). The resolver is a pure
//! function so the store (which reads the four sources) and the daemon (which
//! applies the result at dispatch) share one authoritative precedence rule.

use serde::{Deserialize, Serialize};

/// Which provider backend a card's task dispatches through (spec F1 / D7).
///
/// The wire + DB token is lowercase (`claude` / `codex` / `copilot`), matching
/// the runner backend names so a stored `agent_kind` round-trips byte-identically
/// through JSON and the `agent_task_queue.agent_kind` column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentKind {
    /// Anthropic Claude (`claude -p` headless / YOLO interactive). Dispatches today.
    Claude,
    /// OpenAI Codex (`codex exec` / codex interactive). Dispatches today.
    Codex,
    /// GitHub Copilot. Selectable in the picker; dispatch returns a clear
    /// "provider not yet wired" error until the third runner backend lands
    /// (spec F8, D7 — claude + codex first).
    Copilot,
}

impl AgentKind {
    /// The default backend when the whole cascade is empty (a fresh install with
    /// no board/workspace/global default and no last-used history).
    pub const DEFAULT: Self = Self::Claude;

    /// The lowercase wire/DB token (`claude` / `codex` / `copilot`).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Copilot => "copilot",
        }
    }

    /// Parse a wire/DB token into an [`AgentKind`], or `None` for an unknown
    /// token. Case-insensitive so a hand-edited config value (`Claude`, `CODEX`)
    /// still resolves rather than silently falling through the cascade.
    #[must_use]
    pub fn parse(token: &str) -> Option<Self> {
        match token.trim().to_ascii_lowercase().as_str() {
            "claude" => Some(Self::Claude),
            "codex" => Some(Self::Codex),
            "copilot" => Some(Self::Copilot),
            _ => None,
        }
    }

    /// Whether this backend can be DISPATCHED today. `copilot` is selectable in
    /// the picker but not yet wired to a runner (spec F8); the daemon rejects a
    /// dispatch on it with a clear error rather than stranding the task.
    #[must_use]
    pub const fn is_dispatchable(self) -> bool {
        matches!(self, Self::Claude | Self::Codex)
    }

    /// Every kind, in picker order — the chips the create overlay renders
    /// (claude, codex, copilot).
    #[must_use]
    pub const fn all() -> [Self; 3] {
        [Self::Claude, Self::Codex, Self::Copilot]
    }
}

impl std::fmt::Display for AgentKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Resolve the task-create default agent from the four cascade sources (F4),
/// most-specific first: `last_used → board_default → workspace_default →
/// global_default`, falling back to [`AgentKind::DEFAULT`] when all are empty.
///
/// This is the authoritative precedence rule the picker pre-select and the
/// dispatch default both key off. A source is "empty" when it carries `None`
/// (unset, or an unparseable stored token the reader dropped).
#[must_use]
pub fn resolve_agent_cascade(
    last_used: Option<AgentKind>,
    board_default: Option<AgentKind>,
    workspace_default: Option<AgentKind>,
    global_default: Option<AgentKind>,
) -> AgentKind {
    last_used
        .or(board_default)
        .or(workspace_default)
        .or(global_default)
        .unwrap_or(AgentKind::DEFAULT)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every token round-trips through `as_str` / `parse`.
    #[test]
    fn token_round_trip() {
        for kind in AgentKind::all() {
            assert_eq!(AgentKind::parse(kind.as_str()), Some(kind));
        }
    }

    /// `parse` is case/whitespace tolerant and rejects the unknown.
    #[test]
    fn parse_is_tolerant_and_rejects_unknown() {
        assert_eq!(AgentKind::parse("  Claude "), Some(AgentKind::Claude));
        assert_eq!(AgentKind::parse("CODEX"), Some(AgentKind::Codex));
        assert_eq!(AgentKind::parse("cursor"), None);
        assert_eq!(AgentKind::parse(""), None);
    }

    /// Only claude + codex dispatch; copilot is picker-visible but not wired.
    #[test]
    fn dispatchability() {
        assert!(AgentKind::Claude.is_dispatchable());
        assert!(AgentKind::Codex.is_dispatchable());
        assert!(!AgentKind::Copilot.is_dispatchable());
    }

    /// The cascade returns the FIRST non-empty source, in precedence order.
    #[test]
    fn cascade_prefers_most_specific() {
        // last-used wins over everything.
        assert_eq!(
            resolve_agent_cascade(
                Some(AgentKind::Copilot),
                Some(AgentKind::Codex),
                Some(AgentKind::Claude),
                Some(AgentKind::Claude),
            ),
            AgentKind::Copilot,
        );
        // no last-used → board default wins.
        assert_eq!(
            resolve_agent_cascade(None, Some(AgentKind::Codex), Some(AgentKind::Claude), None,),
            AgentKind::Codex,
        );
        // only workspace default set.
        assert_eq!(
            resolve_agent_cascade(None, None, Some(AgentKind::Codex), None),
            AgentKind::Codex,
        );
        // only global default set.
        assert_eq!(
            resolve_agent_cascade(None, None, None, Some(AgentKind::Codex)),
            AgentKind::Codex,
        );
    }

    /// An entirely empty cascade falls back to the hard default (claude).
    #[test]
    fn cascade_falls_back_to_default() {
        assert_eq!(
            resolve_agent_cascade(None, None, None, None),
            AgentKind::DEFAULT,
        );
        assert_eq!(AgentKind::DEFAULT, AgentKind::Claude);
    }

    /// The wire form is snake_case lowercase (byte-identical to the DB token).
    #[test]
    fn serde_wire_is_lowercase() {
        assert_eq!(
            serde_json::to_string(&AgentKind::Copilot).unwrap(),
            "\"copilot\""
        );
        assert_eq!(
            serde_json::from_str::<AgentKind>("\"codex\"").unwrap(),
            AgentKind::Codex
        );
    }
}
