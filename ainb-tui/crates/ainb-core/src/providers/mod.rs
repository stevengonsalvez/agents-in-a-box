//! CLI provider registry — Phase 2c.
//!
//! Replaces the `enum CliProvider` (formerly `config::CliProvider`) with a
//! `Provider` trait and a string-id-keyed registry. Built-in providers live
//! alongside this module; future plugin-supplied providers register through
//! the same surface in Phase 4 via `ProviderRegistry::register`.
//!
//! ## Why a trait + registry
//!
//! The enum forced us to recompile core for every new provider. The trait
//! lets a plugin (or a downstream crate) ship a `Box<dyn Provider>` and
//! register it at startup. Built-ins keep their current behaviour 1:1 — see
//! `ProviderRegistry::built_ins`.
//!
//! ## Serialisation
//!
//! `AuthenticationConfig::cli_provider` is now `String` (the provider's
//! `id()`), so `~/.agents-in-a-box/config/config.toml` round-trips without
//! migration: the old enum already serialised as snake_case ("claude",
//! "codex", "gemini", "copilot") and that is exactly the string id today.
//!
//! ## Thread-safety
//!
//! Built-in impls are zero-sized unit structs; the registry holds them in
//! `Arc<dyn Provider>` so call sites that need shared access (CLI dispatch,
//! TUI render path) can clone cheaply.

pub mod claude;
pub mod codex;
pub mod copilot;
pub mod gemini;

use std::collections::HashMap;
use std::sync::Arc;

pub use claude::ClaudeProvider;
pub use codex::CodexProvider;
pub use copilot::CopilotProvider;
pub use gemini::GeminiProvider;

/// One CLI agent provider (claude, codex, gemini, copilot, …).
///
/// Method semantics match the legacy `enum CliProvider` 1:1 so call sites
/// can be migrated mechanically.
pub trait Provider: Send + Sync {
    /// Stable string identifier ("claude", "codex", …). Used as map key,
    /// serde value, and CLI flag value.
    fn id(&self) -> &'static str;

    /// Human-readable name shown in UI ("Claude Code", "OpenAI Codex", …).
    fn display_name(&self) -> &'static str;

    /// Executable command name to spawn (typically the same as `id()`).
    fn command(&self) -> &'static str;

    /// Environment variable that holds this provider's API key, if any.
    fn api_key_env_var(&self) -> Option<&'static str>;

    /// Flag the provider's CLI accepts to skip permission prompts.
    fn skip_permissions_flag(&self) -> Option<&'static str>;

    /// URL pointing at the provider's install / setup docs.
    fn install_docs_url(&self) -> &'static str;
}

/// In-process registry of `Provider` impls keyed by `id()`.
#[derive(Clone, Default)]
pub struct ProviderRegistry {
    by_id: HashMap<&'static str, Arc<dyn Provider>>,
    /// Insertion order — stable for help/UI listings.
    ids: Vec<&'static str>,
}

impl ProviderRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registry pre-populated with every built-in provider, in their canonical
    /// listing order.
    #[must_use]
    pub fn built_ins() -> Self {
        let mut r = Self::new();
        r.register(Arc::new(ClaudeProvider));
        r.register(Arc::new(CodexProvider));
        r.register(Arc::new(GeminiProvider));
        r.register(Arc::new(CopilotProvider));
        r
    }

    pub fn register(&mut self, p: Arc<dyn Provider>) {
        let id = p.id();
        self.by_id.insert(id, p);
        if !self.ids.contains(&id) {
            self.ids.push(id);
        }
    }

    /// Look up a provider by id. Returns `None` if no provider with that id
    /// is registered (e.g. a plugin manifest referenced an unloaded plugin).
    pub fn get(&self, id: &str) -> Option<Arc<dyn Provider>> {
        self.by_id.get(id).cloned()
    }

    /// All registered provider ids in registration order.
    #[must_use]
    pub fn ids(&self) -> &[&'static str] {
        &self.ids
    }

    /// All registered providers in registration order.
    pub fn iter(&self) -> impl Iterator<Item = Arc<dyn Provider>> + '_ {
        self.ids
            .iter()
            .filter_map(|id| self.by_id.get(*id).cloned())
    }

    /// `true` if the registry has no providers.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    /// Number of registered providers.
    #[must_use]
    pub fn len(&self) -> usize {
        self.ids.len()
    }

    /// Permissive lookup that mirrors the old `CliProvider::from_str` aliases
    /// ("openai" → codex, "google" → gemini, "github" → copilot). Used by CLI
    /// flag parsing where users may have learned the legacy aliases.
    pub fn get_with_aliases(&self, s: &str) -> Option<Arc<dyn Provider>> {
        let normalised = match s.to_lowercase().as_str() {
            "openai" => "codex".to_string(),
            "google" => "gemini".to_string(),
            "github" => "copilot".to_string(),
            other => other.to_string(),
        };
        self.get(&normalised)
    }
}

/// Default provider id when configuration / CLI flags omit it.
pub const DEFAULT_PROVIDER_ID: &str = "claude";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_registry_resolves_all_built_ins() {
        let r = ProviderRegistry::built_ins();
        assert_eq!(r.len(), 4);
        for id in ["claude", "codex", "gemini", "copilot"] {
            let p = r.get(id).unwrap_or_else(|| panic!("missing {id}"));
            assert_eq!(p.id(), id);
        }
    }

    #[test]
    fn registry_iter_preserves_registration_order() {
        let r = ProviderRegistry::built_ins();
        let ids: Vec<_> = r.iter().map(|p| p.id()).collect();
        assert_eq!(ids, vec!["claude", "codex", "gemini", "copilot"]);
    }

    #[test]
    fn aliases_resolve_to_canonical_ids() {
        let r = ProviderRegistry::built_ins();
        assert_eq!(r.get_with_aliases("OpenAI").unwrap().id(), "codex");
        assert_eq!(r.get_with_aliases("google").unwrap().id(), "gemini");
        assert_eq!(r.get_with_aliases("GITHUB").unwrap().id(), "copilot");
        assert_eq!(r.get_with_aliases("claude").unwrap().id(), "claude");
        assert!(r.get_with_aliases("unknown").is_none());
    }

    #[test]
    fn built_in_methods_match_legacy_enum_table() {
        let r = ProviderRegistry::built_ins();

        let claude = r.get("claude").unwrap();
        assert_eq!(claude.command(), "claude");
        assert_eq!(claude.api_key_env_var(), Some("ANTHROPIC_API_KEY"));
        assert_eq!(claude.skip_permissions_flag(), Some("--dangerously-skip-permissions"));

        let codex = r.get("codex").unwrap();
        assert_eq!(codex.command(), "codex");
        assert_eq!(codex.skip_permissions_flag(), Some("--dangerously-bypass-approvals-and-sandbox"));

        let gemini = r.get("gemini").unwrap();
        assert_eq!(gemini.skip_permissions_flag(), Some("-y"));

        let copilot = r.get("copilot").unwrap();
        assert_eq!(copilot.skip_permissions_flag(), Some("--yolo"));
    }

    #[test]
    fn empty_registry_reports_zero_len() {
        let r = ProviderRegistry::new();
        assert!(r.is_empty());
        assert_eq!(r.len(), 0);
        assert!(r.get("claude").is_none());
    }
}
