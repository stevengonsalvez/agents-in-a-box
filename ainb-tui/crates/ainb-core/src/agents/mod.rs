//! Session agent registry — Phase 2c.
//!
//! Replaces the closed `enum SessionAgentType` (`models/session.rs:23`)
//! with a `SessionAgent` trait + `SessionAgentRegistry` keyed on string id.
//! The enum is kept as the serde-stable type for `~/.agents-in-a-box/
//! sessions.json` (its existing tags — `"claude"`, `"shell"`, `"ssh"`,
//! `"codex"`, `"gemini"`, `"copilot"`, `"kiro"` — already match the trait
//! object ids 1:1, so configs round-trip without migration).
//!
//! ## Why a separate registry from `providers`
//!
//! `providers::Provider` covers AI-CLI agents (claude / codex / gemini /
//! copilot). `SessionAgent` is broader — it also covers `shell`, `ssh`, and
//! `kiro`, which aren't AI-CLIs but are session types the TUI lists in the
//! "new session" picker. Plugins in Phase 4 can register either: a Provider
//! to add a new AI CLI, or a SessionAgent to add a new session shape (e.g.
//! "kubectl session", "docker exec session").
//!
//! Where the two registries overlap (the four AI-CLI ids), the `SessionAgent`
//! impls re-export the same id strings so call sites can resolve through
//! either registry.

use std::collections::HashMap;
use std::sync::Arc;

/// One session-agent type — the row in the "new session" picker.
pub trait SessionAgent: Send + Sync {
    /// Stable string id ("claude", "shell", "ssh", "codex", "gemini",
    /// "copilot", "kiro", or a plugin-supplied id).
    fn id(&self) -> &'static str;

    /// Single-glyph icon shown next to the agent name.
    fn icon(&self) -> &'static str;

    /// Human-readable name shown in the picker.
    fn name(&self) -> &'static str;

    /// Longer description shown when the row is highlighted.
    fn description(&self) -> &'static str;

    /// `false` for agents that aren't currently usable (e.g. Kiro).
    fn is_available(&self) -> bool {
        true
    }
}

/// In-process registry of `SessionAgent` impls keyed by `id()`.
#[derive(Clone, Default)]
pub struct SessionAgentRegistry {
    by_id: HashMap<&'static str, Arc<dyn SessionAgent>>,
    ids: Vec<&'static str>,
}

impl SessionAgentRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registry pre-populated with every built-in session agent, in their
    /// canonical listing order (matches the order the legacy enum used in
    /// the picker).
    #[must_use]
    pub fn built_ins() -> Self {
        let mut r = Self::new();
        r.register(Arc::new(ClaudeAgent));
        r.register(Arc::new(ShellAgent));
        r.register(Arc::new(SshAgent));
        r.register(Arc::new(CodexAgent));
        r.register(Arc::new(GeminiAgent));
        r.register(Arc::new(CopilotAgent));
        r.register(Arc::new(KiroAgent));
        r
    }

    pub fn register(&mut self, a: Arc<dyn SessionAgent>) {
        let id = a.id();
        self.by_id.insert(id, a);
        if !self.ids.contains(&id) {
            self.ids.push(id);
        }
    }

    pub fn get(&self, id: &str) -> Option<Arc<dyn SessionAgent>> {
        self.by_id.get(id).cloned()
    }

    #[must_use]
    pub fn ids(&self) -> &[&'static str] {
        &self.ids
    }

    pub fn iter(&self) -> impl Iterator<Item = Arc<dyn SessionAgent>> + '_ {
        self.ids.iter().filter_map(|id| self.by_id.get(*id).cloned())
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.ids.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }
}

// ──────────────────────────────────────────────────────────────────────────
// Built-in agents. Strings match `models::session::SessionAgentType` 1:1.
// ──────────────────────────────────────────────────────────────────────────

pub struct ClaudeAgent;
impl SessionAgent for ClaudeAgent {
    fn id(&self) -> &'static str {
        "claude"
    }
    fn icon(&self) -> &'static str {
        "✻"
    }
    fn name(&self) -> &'static str {
        "Claude Code"
    }
    fn description(&self) -> &'static str {
        "AI coding assistant powered by Anthropic"
    }
}

pub struct ShellAgent;
impl SessionAgent for ShellAgent {
    fn id(&self) -> &'static str {
        "shell"
    }
    fn icon(&self) -> &'static str {
        "🐚"
    }
    fn name(&self) -> &'static str {
        "Shell Only"
    }
    fn description(&self) -> &'static str {
        "Plain terminal shell without AI agent"
    }
}

pub struct SshAgent;
impl SessionAgent for SshAgent {
    fn id(&self) -> &'static str {
        "ssh"
    }
    fn icon(&self) -> &'static str {
        "🔐"
    }
    fn name(&self) -> &'static str {
        "SSH"
    }
    fn description(&self) -> &'static str {
        "SSH connection to remote server"
    }
}

pub struct CodexAgent;
impl SessionAgent for CodexAgent {
    fn id(&self) -> &'static str {
        "codex"
    }
    fn icon(&self) -> &'static str {
        "✦"
    }
    fn name(&self) -> &'static str {
        "Codex CLI"
    }
    fn description(&self) -> &'static str {
        "OpenAI's coding assistant"
    }
}

pub struct GeminiAgent;
impl SessionAgent for GeminiAgent {
    fn id(&self) -> &'static str {
        "gemini"
    }
    fn icon(&self) -> &'static str {
        "✨"
    }
    fn name(&self) -> &'static str {
        "Gemini CLI"
    }
    fn description(&self) -> &'static str {
        "Google's AI assistant"
    }
}

pub struct CopilotAgent;
impl SessionAgent for CopilotAgent {
    fn id(&self) -> &'static str {
        "copilot"
    }
    fn icon(&self) -> &'static str {
        "🐙"
    }
    fn name(&self) -> &'static str {
        "GitHub Copilot"
    }
    fn description(&self) -> &'static str {
        "GitHub Copilot CLI — AI coding agent by GitHub"
    }
}

pub struct KiroAgent;
impl SessionAgent for KiroAgent {
    fn id(&self) -> &'static str {
        "kiro"
    }
    fn icon(&self) -> &'static str {
        "🔮"
    }
    fn name(&self) -> &'static str {
        "Kiro"
    }
    fn description(&self) -> &'static str {
        "AWS AI coding assistant"
    }
    fn is_available(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_resolves_all_seven_built_ins() {
        let r = SessionAgentRegistry::built_ins();
        assert_eq!(r.len(), 7);
        for id in [
            "claude", "shell", "ssh", "codex", "gemini", "copilot", "kiro",
        ] {
            assert!(r.get(id).is_some(), "missing {id}");
        }
    }

    #[test]
    fn order_matches_legacy_enum() {
        let r = SessionAgentRegistry::built_ins();
        let ids: Vec<_> = r.iter().map(|a| a.id()).collect();
        assert_eq!(
            ids,
            vec![
                "claude", "shell", "ssh", "codex", "gemini", "copilot", "kiro"
            ]
        );
    }

    #[test]
    fn kiro_is_unavailable_others_are_available() {
        let r = SessionAgentRegistry::built_ins();
        for id in ["claude", "shell", "ssh", "codex", "gemini", "copilot"] {
            assert!(
                r.get(id).unwrap().is_available(),
                "{id} should be available"
            );
        }
        assert!(
            !r.get("kiro").unwrap().is_available(),
            "kiro should be unavailable"
        );
    }

    #[test]
    fn icons_and_names_match_legacy_table() {
        let r = SessionAgentRegistry::built_ins();
        assert_eq!(r.get("claude").unwrap().icon(), "✻");
        assert_eq!(r.get("claude").unwrap().name(), "Claude Code");
        assert_eq!(r.get("ssh").unwrap().icon(), "🔐");
        assert_eq!(r.get("ssh").unwrap().name(), "SSH");
        assert_eq!(r.get("kiro").unwrap().icon(), "🔮");
    }
}
