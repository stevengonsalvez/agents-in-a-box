//! Embedded curated `agent_template` registry (P6.3).
//!
//! A curated, repo-only set of [`AgentTemplate`]s baked into the binary at
//! compile time via `include_str!`.
//!
//! This is the Multica pattern: templates are reviewed in PRs and shipped in the
//! binary, never mutable in the database. A template names an agent
//! (`agent_md_path`), carries that agent's `instructions`, and bundles a list of
//! skill *names* that the `ainb hangar templates use` flow resolves against the
//! workspace's imported [`crate::skill`] rows.
//!
//! # IO-free
//!
//! This module only parses the embedded JSON — it performs no IO and has no
//! database dependency, preserving the crate's no-IO contract. The transactional
//! `templates_use` (create agent → attach skills in one transaction) lives in
//! `ainb-hangar-daemon`, which owns the sqlx layer.
//!
//! # Skill-reference invariant
//!
//! Every skill name a template references **must** correspond to a real
//! `toolkit/packages/skills/<name>/SKILL.md`. This is enforced two ways:
//! - at compile time by this crate's `build.rs`, which fails the build if any
//!   referenced skill directory is missing (so drift is caught before a binary
//!   is ever produced), and
//! - at run time by the `test_template_skill_refs_resolve_to_toolkit_dir`
//!   integration test (the mirror, so a stale build cache cannot hide a
//!   regression).

use std::sync::OnceLock;

/// One curated agent template.
///
/// Mirrors the embedded JSON shape one-to-one. `skills` is a list of skill
/// *names* (kebab-case, matching `toolkit/packages/skills/<name>/`), resolved to
/// concrete `skill` rows at `templates use` time — the template itself carries
/// no skill bodies, only references.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AgentTemplate {
    /// Stable template name (the `templates use <name>` key). Kebab-case.
    pub name: String,
    /// Human-readable one-line description (shown by `templates list`).
    pub description: String,
    /// The agent `.md` this template maps to (e.g. `engineering/code-reviewer.md`).
    pub agent_md_path: String,
    /// The agent's system prompt / instructions, embedded verbatim.
    pub instructions: String,
    /// Skill names this template bundles. Each must resolve to a real
    /// `toolkit/packages/skills/<name>/SKILL.md` (enforced by `build.rs`).
    pub skills: Vec<String>,
}

/// The 10 curated templates, embedded by name + raw JSON at compile time.
///
/// `include_str!` bakes each file's bytes into the binary, so the registry is
/// available with zero IO and the JSON files travel with the source (and are
/// validated by `build.rs`). The order here is the canonical list order
/// (`TemplateRegistry::list`).
const TEMPLATES: &[(&str, &str)] = &[
    (
        "code-reviewer",
        include_str!("../../templates/code-reviewer.json"),
    ),
    (
        "brainstormer",
        include_str!("../../templates/brainstormer.json"),
    ),
    ("bug-fixer", include_str!("../../templates/bug-fixer.json")),
    (
        "test-writer-fixer",
        include_str!("../../templates/test-writer-fixer.json"),
    ),
    (
        "security-agent",
        include_str!("../../templates/security-agent.json"),
    ),
    (
        "distinguished-engineer",
        include_str!("../../templates/distinguished-engineer.json"),
    ),
    ("planner", include_str!("../../templates/planner.json")),
    (
        "release-manager",
        include_str!("../../templates/release-manager.json"),
    ),
    (
        "backend-developer",
        include_str!("../../templates/backend-developer.json"),
    ),
    (
        "frontend-developer",
        include_str!("../../templates/frontend-developer.json"),
    ),
];

/// Parse + cache the embedded templates once for the process lifetime.
fn parsed() -> &'static Vec<AgentTemplate> {
    static CACHE: OnceLock<Vec<AgentTemplate>> = OnceLock::new();
    CACHE.get_or_init(|| {
        TEMPLATES
            .iter()
            .map(|(name, raw)| {
                let t: AgentTemplate = serde_json::from_str(raw).unwrap_or_else(|e| {
                    panic!("embedded template `{name}` is not valid JSON: {e}")
                });
                assert_eq!(
                    t.name, *name,
                    "embedded template file `{name}.json` declares a mismatched `name` field `{}`",
                    t.name
                );
                t
            })
            .collect()
    })
}

/// Read-only accessor over the embedded curated templates.
///
/// Stateless: every method reads the process-wide parsed cache. Reused by the
/// `ainb hangar templates` CLI verbs and (P4) the agent picker.
pub struct TemplateRegistry;

impl TemplateRegistry {
    /// Return every embedded template, in canonical list order.
    ///
    /// The result is a fresh `Vec` clone of the parsed cache, so callers may own
    /// and mutate it without affecting the registry.
    #[must_use]
    pub fn load_embedded() -> Vec<AgentTemplate> {
        parsed().clone()
    }

    /// Alias for [`load_embedded`](Self::load_embedded) — every template, in
    /// canonical order. Provided so call sites read `TemplateRegistry::list()`.
    #[must_use]
    pub fn list() -> Vec<AgentTemplate> {
        Self::load_embedded()
    }

    /// Resolve one template by its exact (kebab-case) name, or `None` if no
    /// curated template carries that name.
    #[must_use]
    pub fn get(name: &str) -> Option<AgentTemplate> {
        parsed().iter().find(|t| t.name == name).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embeds_ten_templates() {
        assert_eq!(TemplateRegistry::load_embedded().len(), 10);
    }

    #[test]
    fn list_matches_load_embedded() {
        assert_eq!(TemplateRegistry::list(), TemplateRegistry::load_embedded());
    }

    #[test]
    fn get_resolves_known_and_rejects_unknown() {
        assert!(TemplateRegistry::get("planner").is_some());
        assert!(TemplateRegistry::get("nope").is_none());
    }

    #[test]
    fn every_template_name_matches_its_key() {
        // `parsed()` already asserts file `name` == key; this exercises it.
        for t in TemplateRegistry::load_embedded() {
            assert_eq!(TemplateRegistry::get(&t.name).unwrap().name, t.name);
        }
    }
}
